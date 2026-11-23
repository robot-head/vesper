//! Versioned derivation inputs and canonical derivation keys.
//!
//! A derivation key is `BLAKE3-derive_key("... derivation key", encoding)`
//! over the canonical encoding of [`DerivationInputs`]. Every observable
//! input is named explicitly: the builder and its version, the target, the
//! option map, and every input object (source trees, interface files, other
//! artifacts). Following the contract audit, implementation inputs are part
//! of the key alongside the interface identity, so a body-only change that
//! preserves the interface MIF still selects a new object, and a different
//! target never matches.
//!
//! Encoding, schema v1 (all integers little-endian):
//!
//! ```text
//! "VLDI" | u32 schema=1 | u8 output_domain |
//! str builder | str builder_version | u8 has_target [str target] |
//! u32 n_options | n × (str key | str value) |
//! u32 n_inputs  | n × (str name | 34-byte object id)
//! str := u32 len | UTF-8 bytes
//! ```
//!
//! Map keys are strictly ascending by bytes. Decoding rejects any other
//! form, so each input set has exactly one key.

use std::collections::BTreeMap;
use std::fmt;

use crate::error::DerivationError;
use crate::id::{Digest, Domain, ObjectId};

pub(crate) const MAX_LABEL_BYTES: usize = 4096;
pub(crate) const MAX_MAP_ENTRIES: usize = 4096;

/// Versions of the derivation input schema understood by this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputSchema {
    V1,
}

impl InputSchema {
    pub fn number(self) -> u32 {
        match self {
            InputSchema::V1 => 1,
        }
    }
}

/// Everything that determines a derived output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivationInputs {
    pub schema: InputSchema,
    /// Domain of the produced object: `Interface` or `Artifact`.
    pub output_domain: Domain,
    /// Tool identity, e.g. `vesper-codegen-clif`.
    pub builder: String,
    /// Exact tool version or build identity.
    pub builder_version: String,
    /// Target triple or ABI identity; `None` for target-independent outputs.
    pub target: Option<String>,
    /// Compiler options, enabled features, ABI/layout versions, etc.
    pub options: BTreeMap<String, String>,
    /// Named input objects. Must be nonempty.
    pub inputs: BTreeMap<String, ObjectId>,
}

impl DerivationInputs {
    pub fn new(
        output_domain: Domain,
        builder: impl Into<String>,
        builder_version: impl Into<String>,
    ) -> Self {
        DerivationInputs {
            schema: InputSchema::V1,
            output_domain,
            builder: builder.into(),
            builder_version: builder_version.into(),
            target: None,
            options: BTreeMap::new(),
            inputs: BTreeMap::new(),
        }
    }

    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    pub fn with_option(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.insert(key.into(), value.into());
        self
    }

    pub fn with_input(mut self, name: impl Into<String>, id: ObjectId) -> Self {
        self.inputs.insert(name.into(), id);
        self
    }

    pub fn validate(&self) -> Result<(), DerivationError> {
        if self.output_domain == Domain::Source {
            return Err(DerivationError::SourceOutputDomain);
        }
        check_label("builder", &self.builder)?;
        check_label("builder_version", &self.builder_version)?;
        if let Some(target) = &self.target {
            check_label("target", target)?;
        }
        if self.options.len() > MAX_MAP_ENTRIES {
            return Err(DerivationError::TooManyEntries {
                field: "options",
                limit: MAX_MAP_ENTRIES,
            });
        }
        for key in self.options.keys() {
            check_label("option key", key)?;
        }
        for value in self.options.values() {
            check_value("option value", value)?;
        }
        if self.inputs.is_empty() {
            return Err(DerivationError::NoInputs);
        }
        if self.inputs.len() > MAX_MAP_ENTRIES {
            return Err(DerivationError::TooManyEntries {
                field: "inputs",
                limit: MAX_MAP_ENTRIES,
            });
        }
        for name in self.inputs.keys() {
            check_label("input name", name)?;
        }
        Ok(())
    }

    /// Canonical encoding (validates first).
    pub fn encode(&self) -> Result<Vec<u8>, DerivationError> {
        #[cfg(vesper_port)]
        return crate::port::drv_encode(self);
        #[cfg(not(vesper_port))]
        self.encode_rust()
    }

    /// Strict decoder: unknown schemas, unsorted maps, trailing bytes and
    /// invalid labels are rejected.
    pub fn decode(bytes: &[u8]) -> Result<Self, DerivationError> {
        #[cfg(vesper_port)]
        return crate::port::drv_decode(bytes);
        #[cfg(not(vesper_port))]
        Self::decode_rust(bytes)
    }

    /// The canonical derivation key.
    pub fn key(&self) -> Result<DerivationKey, DerivationError> {
        #[cfg(vesper_port)]
        return crate::port::drv_key(self).map(DerivationKey);
        #[cfg(not(vesper_port))]
        Ok(DerivationKey::of_encoding(&self.encode()?))
    }
}

/// BLAKE3 key of a canonical derivation-input encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DerivationKey(pub Digest);

impl DerivationKey {
    pub(crate) fn of_encoding(encoding: &[u8]) -> Self {
        #[cfg(vesper_port)]
        return DerivationKey(crate::port::derivation_hash(encoding));
        #[cfg(not(vesper_port))]
        Self::of_encoding_rust(encoding)
    }
}

/// Text form: `drv:b3v1:<64 lowercase hex>`.
impl fmt::Display for DerivationKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "drv:b3v1:{}", self.0)
    }
}

fn check_label(field: &'static str, value: &str) -> Result<(), DerivationError> {
    if value.is_empty() {
        return Err(DerivationError::InvalidLabel {
            field,
            value: value.to_owned(),
        });
    }
    check_value(field, value)
}

fn check_value(field: &'static str, value: &str) -> Result<(), DerivationError> {
    if value.len() > MAX_LABEL_BYTES || value.chars().any(char::is_control) {
        return Err(DerivationError::InvalidLabel {
            field,
            value: value.chars().take(64).collect(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::blob_id;

    fn sample() -> DerivationInputs {
        DerivationInputs::new(Domain::Artifact, "vesper-codegen-clif", "0.1.0+abc")
            .with_target("x86_64-unknown-linux-gnu")
            .with_option("opt-level", "2")
            .with_option("abi", "AbiLayoutV1")
            .with_input("source", blob_id(Domain::Source, b"fn main() {}"))
            .with_input("interface", blob_id(Domain::Interface, b"iface"))
    }

    #[test]
    fn encoding_round_trips_and_is_order_independent() {
        let a = sample();
        let bytes = a.encode().unwrap();
        assert_eq!(DerivationInputs::decode(&bytes).unwrap(), a);
        // Construction order of map entries does not matter.
        let b = DerivationInputs::new(Domain::Artifact, "vesper-codegen-clif", "0.1.0+abc")
            .with_input("interface", blob_id(Domain::Interface, b"iface"))
            .with_input("source", blob_id(Domain::Source, b"fn main() {}"))
            .with_option("abi", "AbiLayoutV1")
            .with_option("opt-level", "2")
            .with_target("x86_64-unknown-linux-gnu");
        assert_eq!(a.key().unwrap(), b.key().unwrap());
    }

    #[test]
    fn every_field_changes_the_key() {
        let base = sample().key().unwrap();
        let mut variants = vec![
            sample().with_target("aarch64-unknown-linux-gnu"),
            sample().with_option("opt-level", "3"),
            sample().with_option("feature.io_uring", "on"),
            sample().with_input("source", blob_id(Domain::Source, b"fn main() { 1 }")),
            sample().with_input("interface", blob_id(Domain::Artifact, b"iface")),
        ];
        let mut v = sample();
        v.builder_version = "0.1.1".into();
        variants.push(v);
        let mut v = sample();
        v.target = None;
        variants.push(v);
        let mut v = sample();
        v.output_domain = Domain::Interface;
        variants.push(v);
        for variant in variants {
            assert_ne!(variant.key().unwrap(), base, "{variant:?}");
        }
    }

    #[test]
    fn rejects_invalid_inputs() {
        let mut v = sample();
        v.output_domain = Domain::Source;
        assert_eq!(v.key(), Err(DerivationError::SourceOutputDomain));
        let mut v = sample();
        v.inputs.clear();
        assert_eq!(v.key(), Err(DerivationError::NoInputs));
        let mut v = sample();
        v.builder.clear();
        assert!(matches!(v.key(), Err(DerivationError::InvalidLabel { .. })));
        let v = sample().with_option("bad\nkey", "x");
        assert!(matches!(v.key(), Err(DerivationError::InvalidLabel { .. })));
    }

    #[test]
    fn decoder_rejects_noncanonical_forms() {
        let bytes = sample().encode().unwrap();
        let mut v2 = bytes.clone();
        v2[4] = 2;
        assert_eq!(
            DerivationInputs::decode(&v2),
            Err(DerivationError::UnsupportedSchema(2))
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            DerivationInputs::decode(&trailing),
            Err(DerivationError::TrailingBytes)
        );
        assert_eq!(
            DerivationInputs::decode(&bytes[..bytes.len() - 3]),
            Err(DerivationError::Truncated)
        );
        // Hand-build an encoding with options out of order.
        const MAGIC: &[u8; 4] = b"VLDI";
        fn put_str(out: &mut Vec<u8>, s: &str) {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.push(Domain::Artifact.tag());
        put_str(&mut out, "b");
        put_str(&mut out, "1");
        out.push(0);
        out.extend_from_slice(&2u32.to_le_bytes());
        put_str(&mut out, "z");
        put_str(&mut out, "1");
        put_str(&mut out, "a");
        put_str(&mut out, "1");
        out.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            DerivationInputs::decode(&out),
            Err(DerivationError::NotSorted { field: "options" })
        );
    }
}
