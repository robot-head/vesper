//! Worst-case stack over a static call graph (wave-five `CallGraph` shape).
//!
//! eBPF has no indirect calls apart from tail calls, which this crate does
//! not support, so every edge is direct. Integration B merges this with the
//! Cranelift stack analyzer of lane 3.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FnId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnNode {
    pub id: FnId,
    pub name: String,
    /// Frame size in bytes, already rounded the way the target requires.
    pub frame_bytes: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Direct,
    /// Rejected unless the policy lists its targets (never, for eBPF).
    Indirect,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallGraph {
    pub functions: Vec<FnNode>,
    pub calls: Vec<(FnId, FnId)>,
    /// Call sites whose callee is not statically known.
    pub indirect: Vec<FnId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StackError {
    Exceeded { bytes: u32, path: Vec<FnId> },
    Recursion { path: Vec<FnId> },
    TooDeep { path: Vec<FnId> },
    UnknownFunction(FnId),
    IndirectCall(FnId),
}

impl fmt::Display for StackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StackError::Exceeded { bytes, path } => {
                write!(f, "stack of {bytes} bytes along {path:?}")
            }
            StackError::Recursion { path } => write!(f, "recursion along {path:?}"),
            StackError::TooDeep { path } => write!(f, "call depth along {path:?}"),
            StackError::UnknownFunction(id) => write!(f, "unknown function {id:?}"),
            StackError::IndirectCall(id) => write!(f, "indirect call in {id:?}"),
        }
    }
}

impl std::error::Error for StackError {}

/// Maximum, over every call path from `root`, of the sum of frame sizes.
/// Rejects recursion, paths longer than `max_frames` and totals above
/// `limit`.
pub fn check_stack(
    g: &CallGraph,
    root: FnId,
    limit: u32,
    max_frames: usize,
) -> Result<u32, StackError> {
    crate::port::check_stack(g, root, limit, max_frames)
}
