//! Secret and `@pii` redaction applied before `.vchrono` serialization
//! (§31.1 item 5).
//!
//! Three kinds of rule:
//! * payload fields (`@pii` attributes): a byte range of every payload of one
//!   effect id and op tag;
//! * snapshot image regions (Vault `SecretSlab` pages inside a state image);
//! * secret values: every occurrence of the registered bytes anywhere in a
//!   payload, a snapshot image or the bundle metadata.
//!
//! Redacted bytes are overwritten with zeros. After serialization the whole
//! bundle is scanned once more for every registered secret value; a match that
//! survived (for example one spanning two records or sitting in a header
//! field) fails serialization instead of producing a bundle. The scan covers
//! the registered values only, not other copies an encoder may have made.
//! Digest records whose field rule matches are zeroed entirely because a
//! digest of part of an outcome cannot be recomputed.

use std::fmt;

/// Secrets shorter than this would match by accident too often.
pub const MIN_SECRET_BYTES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldRule {
    pub effect_id: u16,
    pub op_tag: u16,
    pub offset: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRegion {
    pub offset: usize,
    pub len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedactionError {
    SecretTooShort {
        len: usize,
    },
    EmptyRule,
    /// A registered secret still occurs in the serialized bundle.
    SecretSurvived {
        offset: usize,
    },
}

impl fmt::Display for RedactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chrono redaction: {self:?}")
    }
}
impl std::error::Error for RedactionError {}

/// What was scrubbed, written into the bundle so replay knows which bytes
/// are not the original outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedactionEntry {
    pub target: RedactionTarget,
    pub offset: u32,
    pub len: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactionTarget {
    Record { lamport_seq: u64 },
    Snapshot { epoch_id: u32 },
    Metadata,
}

impl RedactionTarget {
    pub(crate) fn tag(self) -> (u8, u64) {
        match self {
            Self::Record { lamport_seq } => (0, lamport_seq),
            Self::Snapshot { epoch_id } => (1, epoch_id.into()),
            Self::Metadata => (2, 0),
        }
    }
    pub(crate) fn from_tag(tag: u8, value: u64) -> Option<Self> {
        match (tag, value) {
            (0, lamport_seq) => Some(Self::Record { lamport_seq }),
            (1, epoch) => u32::try_from(epoch)
                .ok()
                .map(|epoch_id| Self::Snapshot { epoch_id }),
            (2, 0) => Some(Self::Metadata),
            _ => None,
        }
    }
}

#[derive(Default, Clone)]
pub struct RedactionPolicy {
    fields: Vec<FieldRule>,
    regions: Vec<ImageRegion>,
    secrets: Vec<Vec<u8>>,
}

impl fmt::Debug for RedactionPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print secret values.
        f.debug_struct("RedactionPolicy")
            .field("fields", &self.fields)
            .field("regions", &self.regions)
            .field("secrets", &self.secrets.len())
            .finish()
    }
}

impl RedactionPolicy {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn pii_field(&mut self, rule: FieldRule) -> Result<&mut Self, RedactionError> {
        if rule.len == 0 {
            return Err(RedactionError::EmptyRule);
        }
        self.fields.push(rule);
        Ok(self)
    }
    pub fn secret_region(&mut self, region: ImageRegion) -> Result<&mut Self, RedactionError> {
        if region.len == 0 {
            return Err(RedactionError::EmptyRule);
        }
        self.regions.push(region);
        Ok(self)
    }
    pub fn secret_value(&mut self, bytes: &[u8]) -> Result<&mut Self, RedactionError> {
        if bytes.len() < MIN_SECRET_BYTES {
            return Err(RedactionError::SecretTooShort { len: bytes.len() });
        }
        self.secrets.push(bytes.to_vec());
        Ok(self)
    }
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.regions.is_empty() && self.secrets.is_empty()
    }

    pub(crate) fn scrub_record(
        &self,
        lamport_seq: u64,
        effect_id: u16,
        op_tag: u16,
        digest: bool,
        payload: &mut [u8],
        log: &mut Vec<RedactionEntry>,
    ) {
        let target = RedactionTarget::Record { lamport_seq };
        for rule in &self.fields {
            if rule.effect_id == effect_id && rule.op_tag == op_tag {
                if digest {
                    zero(payload, 0, payload.len(), target, log);
                } else {
                    zero(payload, rule.offset, rule.len, target, log);
                }
            }
        }
        self.scrub_secrets(payload, target, log);
    }

    pub(crate) fn scrub_image(
        &self,
        epoch_id: u32,
        image: &mut [u8],
        log: &mut Vec<RedactionEntry>,
    ) {
        let target = RedactionTarget::Snapshot { epoch_id };
        for region in &self.regions {
            zero(image, region.offset, region.len, target, log);
        }
        self.scrub_secrets(image, target, log);
    }

    pub(crate) fn scrub_metadata(&self, metadata: &mut [u8], log: &mut Vec<RedactionEntry>) {
        self.scrub_secrets(metadata, RedactionTarget::Metadata, log);
    }

    fn scrub_secrets(
        &self,
        bytes: &mut [u8],
        target: RedactionTarget,
        log: &mut Vec<RedactionEntry>,
    ) {
        for secret in &self.secrets {
            let mut start = 0;
            while let Some(found) = find(&bytes[start..], secret) {
                let at = start + found;
                zero(bytes, at, secret.len(), target, log);
                start = at + secret.len();
            }
        }
    }

    /// Final check over the serialized bundle.
    pub(crate) fn verify_absent(&self, bundle: &[u8]) -> Result<(), RedactionError> {
        for secret in &self.secrets {
            if let Some(offset) = find(bundle, secret) {
                return Err(RedactionError::SecretSurvived { offset });
            }
        }
        Ok(())
    }
}

/// Zero `[offset, offset+len)` clipped to `bytes`, logging the clipped range.
fn zero(
    bytes: &mut [u8],
    offset: usize,
    len: usize,
    target: RedactionTarget,
    log: &mut Vec<RedactionEntry>,
) {
    if offset >= bytes.len() {
        return;
    }
    let end = offset.saturating_add(len).min(bytes.len());
    bytes[offset..end].fill(0);
    log.push(RedactionEntry {
        target,
        offset: offset as u32,
        len: (end - offset) as u32,
    });
}

pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}
