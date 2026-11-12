//! Infrastructure failures of the execution layer. Hygiene and authority
//! outcomes are not errors here: they are [`crate::Rejection`]s.

use std::path::PathBuf;

use vesper_loom_cas::{CasError, DerivationKey, ObjectId};
use vesper_loom_sandbox::{Diagnostic, OutputFile, SandboxError};

use crate::request::RequestError;

#[derive(Debug)]
pub enum RunnerError {
    Request(RequestError),
    /// An input could not be snapshotted, or changed while it was read.
    Snapshot {
        guest_path: String,
        reason: String,
    },
    /// A materialized input changed during execution.
    InputMutatedDuringRun {
        guest_path: String,
    },
    /// Trap, fuel/memory/wall-clock budget, link/protocol failure or a
    /// malformed/oversized packed result rejected by the sandbox.
    Sandbox(SandboxError),
    /// The guest returned `error`, or a success carrying an error
    /// diagnostic. Nothing is recorded.
    GuestFailed {
        diagnostics: Vec<Diagnostic>,
    },
    /// Macros return tokens only; files left in `/out` are rejected rather
    /// than silently dropped.
    OutputFilesWritten {
        files: Vec<OutputFile>,
    },
    /// The encoded raw output exceeds the runner's record bound.
    RecordTooLarge {
        bytes: usize,
        limit: usize,
    },
    /// A cache entry is corrupt, dangling, stale or inconsistent
    /// (`CorruptionPolicy::Fail`).
    CorruptCache {
        key: DerivationKey,
        reason: String,
    },
    /// A recorded execution differs from a fresh one. Records are never
    /// replaced.
    DivergentExecution {
        key: DerivationKey,
        existing: ObjectId,
        proposed: ObjectId,
    },
    /// The host identity is incomplete; only `CachePolicy::Uncached` runs.
    IncompleteIdentity {
        missing: Vec<&'static str>,
    },
    /// A prepared execution from a runner with another identity.
    IdentityMismatch,
    Cas(CasError),
    Io {
        op: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunnerError::Cas(e) => write!(f, "store: {e}"),
            RunnerError::Io { op, path, source } => {
                write!(f, "{op} {}: {source}", path.display())
            }
            other => write!(f, "{other:?}"),
        }
    }
}

impl std::error::Error for RunnerError {}

impl From<CasError> for RunnerError {
    fn from(e: CasError) -> Self {
        RunnerError::Cas(e)
    }
}

impl From<RequestError> for RunnerError {
    fn from(e: RequestError) -> Self {
        RunnerError::Request(e)
    }
}

impl From<SandboxError> for RunnerError {
    fn from(e: SandboxError) -> Self {
        RunnerError::Sandbox(e)
    }
}

pub(crate) fn io(
    op: &'static str,
    path: impl Into<PathBuf>,
) -> impl FnOnce(std::io::Error) -> RunnerError {
    let path = path.into();
    move |source| RunnerError::Io { op, path, source }
}
