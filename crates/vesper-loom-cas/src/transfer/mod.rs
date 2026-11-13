//! Bounded portable Merkle-closure packs (design §8.2.6).
//!
//! Object identities describe content, not provenance or permission to execute.
//! [`export_pack`] verifies the complete reachable closure and emits canonical
//! children-first records. [`import_pack`] requires the caller's expected root
//! and validates the entire pack and closure before opening the destination.
//! Logical admission limits precede publication; subsequent store/I/O failures
//! may leave verified immutable objects. No derivations or trust are transferred.

use std::{fmt, mem::size_of};

use crate::{CasError, Domain, ObjectId, Tree};

mod export;
mod import;

pub use export::export_pack;
pub use import::import_pack;

pub(crate) const MAGIC: &[u8; 8] = b"VSPACK01";
pub(crate) const PACK_VERSION: u32 = 1;
pub(crate) const HEADER_BYTES: usize = 54;
pub(crate) const RECORD_BYTES: usize = 42;

/// Hard transfer ceilings. Callers may lower every positive limit.
pub const HARD_MAX_PACK_BYTES: usize = 256 * 1024 * 1024;
pub const HARD_MAX_OBJECTS: usize = 16_384;
pub const HARD_MAX_DEPTH: usize = 128;
pub const HARD_MAX_TREE_ENTRIES: usize = 65_536;
pub const HARD_MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
pub const HARD_MAX_WORK: usize = 2 * 1024 * 1024 * 1024;

/// Logical resource limits; these exclude allocator bookkeeping and caller data.
/// All fields must be positive and no greater than the corresponding hard cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferLimits {
    pub max_pack_bytes: usize,
    pub max_objects: usize,
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

/// Cumulative logical requests, rather than live allocation or RSS accounting.
/// Every growing reserve charges its FULL requested new capacity and copy work;
/// a reserve which fits existing capacity costs nothing. Payload capacities of
/// all verified objects share one cap. The final pack has a separate cap.
#[derive(Debug)]
pub(crate) struct Budget {
    limits: TransferLimits,
    work: usize,
    metadata: usize,
    payload_reserved: usize,
    pack_reserved: usize,
    entries: usize,
}

impl Budget {
    pub(crate) fn new(limits: TransferLimits) -> Result<Self, TransferError> {
        limits.validate()?;
        Ok(Self {
            limits,
            work: 0,
            metadata: 0,
            payload_reserved: 0,
            pack_reserved: 0,
            entries: 0,
        })
    }

    pub(crate) fn limits(&self) -> TransferLimits {
        self.limits
    }

    pub(crate) fn work(&mut self, amount: usize) -> Result<(), TransferError> {
        charge(&mut self.work, amount, self.limits.max_work, "work")
    }

    pub(crate) fn metadata(&mut self, amount: usize) -> Result<(), TransferError> {
        charge(
            &mut self.metadata,
            amount,
            self.limits.max_metadata_bytes,
            "metadata bytes",
        )
    }

    pub(crate) fn entries(&mut self, amount: usize) -> Result<(), TransferError> {
        charge(
            &mut self.entries,
            amount,
            self.limits.max_tree_entries,
            "tree entries",
        )
    }

    /// `additional` is relative to len, exactly as for Vec::try_reserve_exact.
    pub(crate) fn reserve_metadata<T>(
        &mut self,
        out: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), TransferError> {
        let target = add(out.len(), additional, "metadata bytes")?;
        if target <= out.capacity() {
            return Ok(());
        }
        let bytes = mul(target, size_of::<T>(), "metadata bytes")?;
        self.metadata(bytes)?;
        self.work(bytes)?;
        reserve(out, additional)
    }

    pub(crate) fn reserve_payload(
        &mut self,
        out: &mut Vec<u8>,
        additional: usize,
    ) -> Result<(), TransferError> {
        let target = add(out.len(), additional, "payload bytes")?;
        if target > self.limits.max_pack_bytes {
            return Err(TransferError::Limit("payload bytes"));
        }
        if target <= out.capacity() {
            return Ok(());
        }
        charge(
            &mut self.payload_reserved,
            target,
            self.limits.max_pack_bytes,
            "payload capacity requests",
        )?;
        self.work(target)?;
        reserve(out, additional)
    }

    pub(crate) fn reserve_pack(
        &mut self,
        out: &mut Vec<u8>,
        additional: usize,
    ) -> Result<(), TransferError> {
        let target = add(out.len(), additional, "pack bytes")?;
        if target > self.limits.max_pack_bytes {
            return Err(TransferError::Limit("pack bytes"));
        }
        if target <= out.capacity() {
            return Ok(());
        }
        charge(
            &mut self.pack_reserved,
            target,
            self.limits.max_pack_bytes,
            "pack capacity requests",
        )?;
        self.work(target)?;
        reserve(out, additional)
    }
}

fn charge(
    used: &mut usize,
    amount: usize,
    limit: usize,
    name: &'static str,
) -> Result<(), TransferError> {
    let next = add(*used, amount, name)?;
    if next > limit {
        return Err(TransferError::Limit(name));
    }
    *used = next;
    Ok(())
}

pub(crate) fn add(a: usize, b: usize, name: &'static str) -> Result<usize, TransferError> {
    a.checked_add(b).ok_or(TransferError::Limit(name))
}

pub(crate) fn mul(a: usize, b: usize, name: &'static str) -> Result<usize, TransferError> {
    a.checked_mul(b).ok_or(TransferError::Limit(name))
}

fn reserve<T>(out: &mut Vec<T>, additional: usize) -> Result<(), TransferError> {
    out.try_reserve_exact(additional)
        .map_err(|_| TransferError::AllocationFailed)
}

/// Decode only after prepaying all Tree allocations, error clones, reencoding
/// and later publication tree buffers. This does not pay publication's generic
/// Store I/O scratch/path costs; the importer must prepay those for every record.
pub(crate) fn decode_tree(
    domain: Domain,
    bytes: &[u8],
    budget: &mut Budget,
) -> Result<Tree, TransferError> {
    budget.work(bytes.len())?;
    if bytes.len() < 16 || &bytes[..4] != b"VLTR" {
        return Err(TransferError::InvalidPack("tree header"));
    }
    if u32::from_le_bytes(bytes[4..8].try_into().unwrap()) != crate::TREE_SCHEMA_VERSION {
        return Err(TransferError::InvalidPack("tree version"));
    }
    let count = usize::try_from(u64::from_le_bytes(bytes[8..16].try_into().unwrap()))
        .map_err(|_| TransferError::Limit("tree entries"))?;
    budget.entries(count)?;
    // Every entry needs u16 name length, one name byte, tag, digest and size.
    if count > (bytes.len() - 16) / 44 {
        return Err(TransferError::InvalidPack("truncated tree entries"));
    }
    let mut pos = 16usize;
    let mut names = 0usize;
    for _ in 0..count {
        let length_end = add(pos, 2, "tree bytes")?;
        let raw = bytes
            .get(pos..length_end)
            .ok_or(TransferError::InvalidPack("tree name length"))?;
        let length = u16::from_le_bytes(raw.try_into().unwrap()) as usize;
        if length == 0 || length > 255 {
            return Err(TransferError::InvalidPack("tree name length"));
        }
        names = add(names, length, "tree name bytes")?;
        pos = add(length_end, add(length, 41, "tree bytes")?, "tree bytes")?;
        if pos > bytes.len() {
            return Err(TransferError::InvalidPack("truncated tree entry"));
        }
    }
    if pos != bytes.len() {
        return Err(TransferError::InvalidPack("tree trailing bytes"));
    }
    // Two BTree maps: reserve 2 KiB per entry for logical node storage,
    // including sparsely populated nodes. 32x name bytes covers originals,
    // previous/error strings, uppercase names, Unicode lowercase expansion
    // (at most 3x bytes) and temporary String growth/copies. 16x encode capacity
    // covers decode canonical checks and eventual Tree::encode publication,
    // including geometric Vec growth from its initial 16 + 64*count capacity.
    let encode = add(16, mul(count, 64, "tree metadata")?, "tree metadata")?;
    let encode = add(encode, bytes.len(), "tree metadata")?;
    let metadata = add(
        add(
            mul(count, 2048, "tree metadata")?,
            mul(names, 32, "tree metadata")?,
            "tree metadata",
        )?,
        mul(encode, 16, "tree metadata")?,
        "tree metadata",
    )?;
    budget.metadata(metadata)?;
    // BTree comparisons, name transformations and encoding/copies. A tree's
    // height is bounded by binary log(count)+1; allow 64 name comparisons per
    // level across map operations, each inspecting at most 255 bytes.
    let levels = (usize::BITS - count.leading_zeros()) as usize + 1;
    let comparison_work = mul(mul(count, levels, "tree work")?, 64 * 255, "tree work")?;
    budget.work(add(comparison_work, metadata, "tree work")?)?;
    Tree::decode(domain, bytes).map_err(|error| TransferError::Cas(error.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_reject_zero_and_hard_overflow() {
        let defaults = TransferLimits::default();
        assert!(defaults.validate().is_ok());
        assert!(
            TransferLimits {
                max_work: 0,
                ..defaults
            }
            .validate()
            .is_err()
        );
        assert!(
            TransferLimits {
                max_pack_bytes: HARD_MAX_PACK_BYTES + 1,
                ..defaults
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn capacity_requests_are_cumulative_but_no_growth_is_free() {
        let mut budget = Budget::new(TransferLimits {
            max_pack_bytes: 8,
            ..TransferLimits::default()
        })
        .unwrap();
        let mut payload = Vec::new();
        budget.reserve_payload(&mut payload, 4).unwrap();
        budget.reserve_payload(&mut payload, 4).unwrap();
        payload.extend_from_slice(b"1234");
        assert!(budget.reserve_payload(&mut payload, 4).is_err());
        let mut pack = Vec::new();
        budget.reserve_pack(&mut pack, 8).unwrap();
        assert_eq!(budget.payload_reserved, 4);
        assert_eq!(budget.pack_reserved, 8);
    }

    #[test]
    fn tree_counts_and_metadata_are_checked_before_decode() {
        let tree = Tree::new(Domain::Source);
        let bytes = tree.encode();
        let mut budget = Budget::new(TransferLimits::default()).unwrap();
        assert_eq!(
            decode_tree(Domain::Source, &bytes, &mut budget).unwrap(),
            tree
        );
        let mut budget = Budget::new(TransferLimits {
            max_metadata_bytes: 1,
            ..TransferLimits::default()
        })
        .unwrap();
        assert!(matches!(
            decode_tree(Domain::Source, &bytes, &mut budget),
            Err(TransferError::Limit("metadata bytes"))
        ));
        let mut bad = bytes;
        bad[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(
            decode_tree(
                Domain::Source,
                &bad,
                &mut Budget::new(TransferLimits::default()).unwrap()
            )
            .is_err()
        );
    }
}
