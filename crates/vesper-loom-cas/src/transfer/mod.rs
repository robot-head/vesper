//! Bounded portable Merkle-closure packs (design §8.2.6).
//!
//! Object identities describe content, not provenance or permission to execute.
//! [`export_pack`] verifies the complete reachable closure and emits canonical
//! children-first records. [`import_pack`] requires the caller's expected root
//! and validates the entire pack and closure before opening the destination.
//! Logical admission limits precede publication; subsequent store/I/O failures
//! may leave verified immutable objects. No derivations or trust are transferred.

use std::fmt;

use crate::{CasError, ObjectId};

mod export;
mod import;

pub use export::export_pack;
pub use import::import_pack;

pub(crate) const MAGIC: &[u8; 8] = b"VSPACK01";
pub(crate) const PACK_VERSION: u32 = 1;

/// Hard transfer ceilings. Callers may lower every positive limit.
pub const HARD_MAX_PACK_BYTES: usize = 256 * 1024 * 1024;
pub const HARD_MAX_OBJECTS: usize = 16_384;
pub const HARD_MAX_DEPTH: usize = 128;
pub const HARD_MAX_TREE_ENTRIES: usize = 65_536;
pub const HARD_MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
pub const HARD_MAX_WORK: usize = 2 * 1024 * 1024 * 1024;

/// Logical resource limits; these exclude allocator bookkeeping and caller data.
/// All fields must be positive and no greater than the corresponding hard cap.
/// Canonical destination-root prefixes and their legacy Store path copies are
/// excluded with filesystem path resolution; these are not process RSS caps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferLimits {
    pub max_pack_bytes: usize,
    pub max_objects: usize,
    /// Maximum path length in objects, including the root (depth one).
    pub max_depth: usize,
    pub max_tree_entries: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
}

impl Default for TransferLimits {
    fn default() -> Self {
        Self {
            max_pack_bytes: 64 * 1024 * 1024,
            max_objects: 4096,
            max_depth: 64,
            max_tree_entries: 16_384,
            max_metadata_bytes: 16 * 1024 * 1024,
            max_work: 512 * 1024 * 1024,
        }
    }
}

impl TransferLimits {
    /// Validate limits before allocating or accessing a store.
    pub fn validate(self) -> Result<(), TransferError> {
        for (name, value, hard) in [
            ("pack bytes", self.max_pack_bytes, HARD_MAX_PACK_BYTES),
            ("objects", self.max_objects, HARD_MAX_OBJECTS),
            ("depth", self.max_depth, HARD_MAX_DEPTH),
            ("tree entries", self.max_tree_entries, HARD_MAX_TREE_ENTRIES),
            (
                "metadata bytes",
                self.max_metadata_bytes,
                HARD_MAX_METADATA_BYTES,
            ),
            ("work", self.max_work, HARD_MAX_WORK),
        ] {
            if value == 0 || value > hard {
                return Err(TransferError::InvalidLimits(name));
            }
        }
        Ok(())
    }
}

/// Actual import results. This is a diagnostic, never a trust/approval token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferReport {
    pub root: ObjectId,
    pub objects: usize,
    pub pack_bytes: usize,
    pub payload_bytes: usize,
    pub inserted: usize,
    pub reused: usize,
}

#[derive(Debug)]
pub enum TransferError {
    InvalidLimits(&'static str),
    Limit(&'static str),
    InvalidPack(&'static str),
    UnexpectedRoot {
        expected: ObjectId,
        found: ObjectId,
    },
    DigestMismatch {
        expected: ObjectId,
        actual: ObjectId,
    },
    AllocationFailed,
    Cas(CasError),
}

impl fmt::Display for TransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits(name) => write!(f, "invalid transfer limit: {name}"),
            Self::Limit(name) => write!(f, "transfer limit exceeded: {name}"),
            Self::InvalidPack(reason) => write!(f, "invalid closure pack: {reason}"),
            Self::UnexpectedRoot { expected, found } => {
                write!(f, "expected pack root {expected}, found {found}")
            }
            Self::DigestMismatch { expected, actual } => {
                write!(f, "expected object {expected}, contents hash to {actual}")
            }
            Self::AllocationFailed => f.write_str("transfer allocation failed"),
            Self::Cas(error) => write!(f, "transfer CAS operation: {error}"),
        }
    }
}

impl std::error::Error for TransferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cas(error) => Some(error),
            _ => None,
        }
    }
}

impl From<CasError> for TransferError {
    fn from(error: CasError) -> Self {
        Self::Cas(error)
    }
}
