//! Failures of request validation and of the build runner.

use std::fmt;
use std::path::PathBuf;

use vesper_loom_cas::{CasError, DerivationKey, ObjectId};
use vesper_loom_sandbox::{ArtifactKind, Diagnostic, SandboxError, StepKind};

/// A request rejected before any snapshot or execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestError {
    UnsupportedVersion {
        found: u32,
        supported: u32,
    },
    UnsupportedProtocol {
        found: u32,
        supported: u32,
    },
    /// Only `StepKind::BuildScript` is accepted in this milestone.
    UnsupportedStep(StepKind),
    IncompatiblePolicy(String),
    NotAComponent(ArtifactKind),
    Malformed {
        field: &'static str,
        value: String,
    },
    ZeroLimit(&'static str),
    InvalidGuestPath(String),
    MountOverlapsOutput(String),
    OverlappingMounts {
        first: String,
        second: String,
    },
    OverlappingHostInputs {
        first: PathBuf,
        second: PathBuf,
    },
    InputUnavailable {
        host_path: PathBuf,
        reason: String,
    },
    InputNotDirectory(PathBuf),
    /// The store, scratch area or destination lies inside a declared input
    /// (or the reverse), so snapshots or outputs would alias each other.
    LayoutOverlap {
        what: &'static str,
        path: PathBuf,
        input: PathBuf,
    },
    Json(String),
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid build request: {self:?}")
    }
}

impl std::error::Error for RequestError {}

/// Why a build produced no accepted result.
#[derive(Debug)]
pub enum BuildError {
    Request(RequestError),
    /// The runner's execution fingerprint has unknown fields, so a cache
    /// read or write would not be sound. Use `CachePolicy::Uncached`.
    UnknownFingerprint {
        missing: Vec<&'static str>,
    },
    /// An input could not be snapshotted (symlink, special file,
    /// non-portable name, I/O failure, ...).
    Snapshot {
        guest_path: String,
        source: Box<CasError>,
    },
    /// Two consecutive snapshots of one input disagreed: it was being
    /// modified during ingestion. Nothing was executed.
    InputChangedDuringSnapshot {
        guest_path: String,
        first: ObjectId,
        second: ObjectId,
    },
    /// A materialized input differed from its snapshot after the run.
    InputMutatedDuringRun {
        guest_path: String,
    },
    /// The sandbox failed: compile/link, trap, fuel, memory, timeout,
    /// transport or output-limit violations.
    Sandbox(Box<SandboxError>),
    /// The guest returned its error result, or a success carrying
    /// error-severity diagnostics.
    GuestFailed {
        diagnostics: Vec<Diagnostic>,
    },
    /// The output tree was not acceptable (non-portable names, mismatch
    /// with the sandbox's own output scan, ...).
    OutputRejected(String),
    /// A cached result is unreadable, corrupt, dangling or inconsistent.
    CorruptCache {
        key: DerivationKey,
        reason: String,
    },
    /// One derivation key produced two different accepted results.
    DivergentOutput {
        key: DerivationKey,
        existing: ObjectId,
        proposed: ObjectId,
    },
    /// The destination exists and the policy forbids replacing it, or it
    /// is not a plain directory.
    Destination {
        path: PathBuf,
        reason: String,
    },
    Store(Box<CasError>),
    Io {
        op: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Request(e) => write!(f, "{e}"),
            BuildError::Store(e) => write!(f, "store: {e}"),
            BuildError::Sandbox(e) => write!(f, "sandbox: {e}"),
            other => write!(f, "{other:?}"),
        }
    }
}

impl std::error::Error for BuildError {}

impl From<RequestError> for BuildError {
    fn from(e: RequestError) -> Self {
        BuildError::Request(e)
    }
}

impl From<CasError> for BuildError {
    fn from(e: CasError) -> Self {
        BuildError::Store(Box::new(e))
    }
}

impl From<SandboxError> for BuildError {
    fn from(e: SandboxError) -> Self {
        BuildError::Sandbox(Box::new(e))
    }
}

pub(crate) fn io(
    op: &'static str,
    path: impl Into<PathBuf>,
) -> impl FnOnce(std::io::Error) -> BuildError {
    let path = path.into();
    move |source| BuildError::Io { op, path, source }
}
