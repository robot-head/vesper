//! Explicit pure Tier1 expansion of the complete current source input.
//!
//! This query is independent of signature cutoffs and ordinary semantic editor
//! analysis. Its immutable artifact describes expanded source only. Package and
//! policies are fixed in this process; no caller approvals or guest results enter
//! the memoized value.
use super::{SourceFile, SourceIdentity};
use crate::forge_frontend::{self, CompileConfig, CompileFailure, CompileLimits, ExpandedChecked};
use std::sync::Arc;

const COMPILE_LIMITS: CompileLimits = CompileLimits {
    max_source_bytes: 128 * 1024,
    max_expanded_bytes: 128 * 1024,
    max_map_entries: 1024,
    max_metadata_bytes: 2 * 1024 * 1024,
    max_identifier_bytes: 256,
    max_work: 128 * 1024 * 1024,
};

/// Fixed editor profile, separate from caller-configurable batch compilation.
pub fn compile_limits() -> CompileLimits {
    COMPILE_LIMITS
}

// Prepay a separate finite envelope before the snapshot's hash/copy. Six byte
// visits include hashing, retention allocation/copy and identity association;
// fixed metadata is charged as well. The compiler has its own unchanged work
// allowance. This invariant covers every bounded legacy SourceFile value, so no
// synthetic compiler failure or unchecked fallback is needed.
const SNAPSHOT_WORK: usize = 1024 * 1024;
const SNAPSHOT_BYTE_WORK: usize = 6;
const SNAPSHOT_FIXED_WORK: usize = 256;
const _: () = assert!(
    COMPILE_LIMITS.max_source_bytes * SNAPSHOT_BYTE_WORK + SNAPSHOT_FIXED_WORK <= SNAPSHOT_WORK
);

/// Sealed current-input snapshot. It retains only bounded original source and
/// the real compiler outcome; successful Worlds are never deeply cloned.
#[derive(Debug)]
pub struct MacroSnapshot {
    identity: SourceIdentity,
    source: Option<Arc<str>>,
    result: Result<Arc<ExpandedChecked>, Arc<CompileFailure>>,
}

// Salsa requires PartialEq even with no_eq. Pointer identity compares actual
// immutable outcomes, including refusals, without traversing checked Worlds.
impl PartialEq for MacroSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
            && match (&self.result, &other.result) {
                (Ok(a), Ok(b)) => Arc::ptr_eq(a, b),
                (Err(a), Err(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}
impl Eq for MacroSnapshot {}

impl MacroSnapshot {
    pub fn source_identity(&self) -> &SourceIdentity {
        &self.identity
    }
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
    pub fn expanded(&self) -> Result<&ExpandedChecked, &CompileFailure> {
        match &self.result {
            Ok(checked) => Ok(checked),
            Err(failure) => Err(failure),
        }
    }
    /// Retain the immutable real outcome beyond a database borrow.
    pub fn outcome(&self) -> Result<Arc<ExpandedChecked>, Arc<CompileFailure>> {
        match &self.result {
            Ok(checked) => Ok(Arc::clone(checked)),
            Err(failure) => Err(Arc::clone(failure)),
        }
    }
}

/// Memoize full-source expansion with the fixed pure editor configuration.
/// Every source edit, including helper bodies and comments, observes this input
/// directly and reruns actual expansion, checking, authority gating and IR.
#[salsa::tracked(returns(ref), no_eq)]
pub fn preview(db: &dyn salsa::Database, file: SourceFile) -> MacroSnapshot {
    let source = file.text(db);
    // In particular, legacy oversized inputs reach the actual compiler's early
    // byte refusal before any snapshot hashing or copying of their contents.
    let result = forge_frontend::expand_source(source, &CompileConfig::default(), COMPILE_LIMITS)
        .map(Arc::new)
        .map_err(Arc::new);
    let (retained, hash) = if source.len() <= COMPILE_LIMITS.max_source_bytes {
        let _remaining_snapshot_work =
            SNAPSHOT_WORK - (source.len() * SNAPSHOT_BYTE_WORK + SNAPSHOT_FIXED_WORK);
        let hash = *blake3::hash(source.as_bytes()).as_bytes();
        (Some(Arc::from(source.as_str())), Some(hash))
    } else {
        (None, None)
    };
    MacroSnapshot {
        identity: SourceIdentity {
            byte_len: source.len(),
            blake3: hash,
            opaque: hash.is_none().then(|| Arc::new(())),
        },
        source: retained,
        result,
    }
}
