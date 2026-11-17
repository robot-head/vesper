//! Span attributes and the §22.4 PII redaction boundary.
//!
//! [`SpanAttributes::insert`] accepts only [`Telemetrable`] values (scalars,
//! `&'static str` vocabulary, `Redacted`). Sensitive values ([`Secret`],
//! [`Pii`]) go through [`SpanAttributes::insert_pii`], which applies a
//! [`PiiPolicy`] **before** anything is stored: the attribute set keeps only
//! the constant marker or a keyed fingerprint, and the sensitive owner is
//! dropped. Nothing downstream (encoder, IPC writer, transport) ever sees the
//! raw bytes.
//!
//! [`Secret`]: vesper_beacon::Secret
//! [`Pii`]: vesper_beacon::Pii

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::Arc;
use vesper_beacon::{Sensitive, Telemetrable, TelemetryValue};

/// Exported string of a redacted attribute.
pub const REDACTED_MARKER: &str = "[REDACTED]";
/// Prefix of a fingerprinted attribute, followed by 16 lowercase hex digits
/// (the first 8 bytes of HMAC-SHA256 under the exporter's key) and `]`.
pub const FINGERPRINT_PREFIX: &str = "[REDACTED:hmac-sha256:";
/// Keys starting with this prefix are reserved for Beacon's own attributes.
pub const RESERVED_KEY_PREFIX: &str = "vesper.";
pub const MAX_KEY_BYTES: usize = 128;
pub const MAX_ATTRIBUTES_PER_SPAN: usize = 64;
/// Any attribute set: resources, scopes, events, links, data points and log
/// records use the same bounded, PII-checked set as spans.
pub type Attributes = SpanAttributes;

/// HMAC key for fingerprinting. No `Debug`, `Clone` or accessor; it is not
/// zeroized on drop.
pub struct FingerprintKey([u8; 32]);
impl FingerprintKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// What an exporter does with a sensitive attribute.
pub enum PiiPolicy {
    /// Replace with [`REDACTED_MARKER`]. The output is independent of the
    /// value (the property `lean/BeaconRedaction.lean` proves for its model).
    Redact,
    /// Replace with a keyed 64-bit HMAC-SHA256 prefix so equal values can be
    /// correlated without exporting them. This is a deliberate, bounded
    /// declassification: the output depends on the value through the HMAC.
    /// Low-entropy values stay guessable by anyone holding the key.
    Fingerprint(FingerprintKey),
}
impl PiiPolicy {
    /// Applies the policy and drops the sensitive owner. Only the marker or
    /// the keyed fingerprint survives; this is the one place a sensitive
    /// value becomes an exportable [`AttributeValue`].
    pub fn apply(&self, value: impl Sensitive) -> AttributeValue {
        let stored = match self {
            PiiPolicy::Redact => AttributeValue::Redacted,
            PiiPolicy::Fingerprint(key) => AttributeValue::Fingerprint(fingerprint(key, &value)),
        };
        drop(value);
        stored
    }
}
impl core::fmt::Debug for PiiPolicy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Redact => "Redact",
            Self::Fingerprint(_) => "Fingerprint(<key>)",
        })
    }
}

/// A stored attribute value. Its only string forms are static vocabulary,
/// the redaction marker and fingerprints, and its only byte strings are
/// static, so no runtime input is held. Slices and maps nest stored values,
/// so every leaf passed the same boundary ([`AttributeValue::public`],
/// [`PiiPolicy::apply`]) as a top-level value.
#[derive(Clone, Debug, PartialEq)]
pub enum AttributeValue {
    Bool(bool),
    Int(i64),
    Double(f64),
    Static(&'static str),
    Redacted,
    Fingerprint([u8; 8]),
    /// OTLP `bytes_value`: static bytes (sensitive bytes go through a
    /// [`PiiPolicy`] like any other sensitive value).
    Bytes(&'static [u8]),
    /// OTLP `array_value`. Build with [`AttributeValue::slice`].
    Slice(Arc<[AttributeValue]>),
    /// OTLP `kvlist_value`, keys unique. Build with [`AttributeValue::map`].
    Map(Arc<[(&'static str, AttributeValue)]>),
}
/// Nesting depth of a structured value (a scalar is depth 1).
pub const MAX_VALUE_DEPTH: usize = 8;
/// Values (scalars, slices and maps) inside one structured value.
pub const MAX_VALUE_NODES: usize = 256;

impl AttributeValue {
    /// The stored form of a public value.
    pub fn public(value: impl Telemetrable) -> Self {
        Self::from_telemetry(value.telemetry_value())
    }
    pub(crate) fn from_telemetry(value: TelemetryValue) -> Self {
        match value {
            TelemetryValue::Bool(b) => Self::Bool(b),
            TelemetryValue::Int(i) => Self::Int(i),
            TelemetryValue::Double(d) => Self::Double(d),
            TelemetryValue::Static(s) => Self::Static(s),
            TelemetryValue::Redacted(_) => Self::Redacted,
        }
    }
    /// A static byte string.
    pub fn bytes(value: &'static [u8]) -> Self {
        Self::Bytes(value)
    }
    /// A slice of stored values, at most [`MAX_VALUE_DEPTH`] deep and
    /// [`MAX_VALUE_NODES`] values in all.
    pub fn slice(items: impl IntoIterator<Item = AttributeValue>) -> Result<Self, AttributeError> {
        let value = Self::Slice(items.into_iter().collect());
        value.check_shape()?;
        Ok(value)
    }
    /// A map of stored values with unique, nonempty keys of at most
    /// [`MAX_KEY_BYTES`], bounded like [`AttributeValue::slice`].
    pub fn map(
        entries: impl IntoIterator<Item = (&'static str, AttributeValue)>,
    ) -> Result<Self, AttributeError> {
        let entries: Vec<_> = entries.into_iter().collect();
        for (i, (key, _)) in entries.iter().enumerate() {
            if key.is_empty() {
                return Err(AttributeError::EmptyKey);
            }
            if key.len() > MAX_KEY_BYTES {
                return Err(AttributeError::KeyTooLong);
            }
            if entries[..i].iter().any(|(k, _)| k == key) {
                return Err(AttributeError::DuplicateKey);
            }
        }
        let value = Self::Map(entries.into());
        value.check_shape()?;
        Ok(value)
    }
    /// Nesting depth (1 for a scalar).
    pub fn depth(&self) -> usize {
        match self {
            Self::Slice(items) => 1 + items.iter().map(Self::depth).max().unwrap_or(0),
            Self::Map(entries) => 1 + entries.iter().map(|(_, v)| v.depth()).max().unwrap_or(0),
            _ => 1,
        }
    }
    /// Number of values, this one included.
    pub fn nodes(&self) -> usize {
        match self {
            Self::Slice(items) => 1 + items.iter().map(Self::nodes).sum::<usize>(),
            Self::Map(entries) => 1 + entries.iter().map(|(_, v)| v.nodes()).sum::<usize>(),
            _ => 1,
        }
    }
    fn check_shape(&self) -> Result<(), AttributeError> {
        if self.depth() > MAX_VALUE_DEPTH {
            return Err(AttributeError::TooDeep);
        }
        if self.nodes() > MAX_VALUE_NODES {
            return Err(AttributeError::TooManyValues);
        }
        Ok(())
    }
    /// The exported string, if this value is exported as an OTLP string.
    pub fn export_str(&self) -> Option<String> {
        match self {
            Self::Static(s) => Some((*s).to_owned()),
            Self::Redacted => Some(REDACTED_MARKER.to_owned()),
            Self::Fingerprint(bytes) => {
                let mut out = String::with_capacity(FINGERPRINT_PREFIX.len() + 17);
                out.push_str(FINGERPRINT_PREFIX);
                for b in bytes {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    out.push(HEX[usize::from(b >> 4)] as char);
                    out.push(HEX[usize::from(b & 15)] as char);
                }
                out.push(']');
                Some(out)
            }
            Self::Bool(_)
            | Self::Int(_)
            | Self::Double(_)
            | Self::Bytes(_)
            | Self::Slice(_)
            | Self::Map(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeError {
    EmptyKey,
    KeyTooLong,
    /// Keys under `vesper.` are produced by Beacon itself.
    ReservedKey,
    DuplicateKey,
    TooManyAttributes,
    /// A structured value nested deeper than [`MAX_VALUE_DEPTH`].
    TooDeep,
    /// A structured value with more than [`MAX_VALUE_NODES`] values.
    TooManyValues,
}

/// Bounded, ordered attribute set of one span, resource, scope, event, link,
/// data point or log record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpanAttributes {
    entries: Vec<(&'static str, AttributeValue)>,
}
impl SpanAttributes {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&'static str, AttributeValue)> + '_ {
        self.entries.iter().map(|(k, v)| (*k, v.clone()))
    }
    fn check(&self, key: &'static str) -> Result<(), AttributeError> {
        if key.is_empty() {
            return Err(AttributeError::EmptyKey);
        }
        if key.len() > MAX_KEY_BYTES {
            return Err(AttributeError::KeyTooLong);
        }
        if key.starts_with(RESERVED_KEY_PREFIX) {
            return Err(AttributeError::ReservedKey);
        }
        if self.entries.iter().any(|(k, _)| *k == key) {
            return Err(AttributeError::DuplicateKey);
        }
        if self.entries.len() >= MAX_ATTRIBUTES_PER_SPAN {
            return Err(AttributeError::TooManyAttributes);
        }
        Ok(())
    }
    /// Adds a public value. All checks precede mutation.
    pub fn insert(
        &mut self,
        key: &'static str,
        value: impl Telemetrable,
    ) -> Result<&mut Self, AttributeError> {
        self.check(key)?;
        self.entries
            .push((key, AttributeValue::from_telemetry(value.telemetry_value())));
        Ok(self)
    }
    /// Adds a sensitive value after applying `policy`. The sensitive owner is
    /// consumed and dropped here; only the policy's output is stored. On a
    /// key error the value is still dropped, never stored.
    pub fn insert_pii(
        &mut self,
        key: &'static str,
        value: impl Sensitive,
        policy: &PiiPolicy,
    ) -> Result<&mut Self, AttributeError> {
        self.check(key)?;
        let stored = policy.apply(value);
        self.entries.push((key, stored));
        Ok(self)
    }
    /// Adds a value that is already a stored form (public, redacted or a
    /// fingerprint), such as the output of [`PiiPolicy::apply`].
    pub fn insert_value(
        &mut self,
        key: &'static str,
        value: AttributeValue,
    ) -> Result<&mut Self, AttributeError> {
        self.check(key)?;
        self.entries.push((key, value));
        Ok(self)
    }
    pub(crate) fn push_reserved(&mut self, key: &'static str, value: AttributeValue) {
        self.entries.push((key, value));
    }
}

fn fingerprint(key: &FingerprintKey, value: &impl Sensitive) -> [u8; 8] {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(&key.0).expect("HMAC accepts any key length");
    mac.update(value.fingerprint_input());
    let tag = mac.finalize().into_bytes();
    let mut out = [0; 8];
    out.copy_from_slice(&tag[..8]);
    out
}
