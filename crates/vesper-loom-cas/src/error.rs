//! Structured store errors.

use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::derivation::DerivationKey;
use crate::id::{Domain, ObjectId, ObjectKind};

/// Why an entry name was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    DotOrDotDot,
    Separator,
    ForbiddenCharacter,
    NulOrControl,
    WindowsReserved,
    TrailingDotOrSpace,
    NotUtf8,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NameError::Empty => "empty name",
            NameError::TooLong => "name longer than 255 bytes",
            NameError::DotOrDotDot => "`.` and `..` are not entry names",
            NameError::Separator => "name contains a path separator",
            NameError::ForbiddenCharacter => {
                "name contains a character that is not portable (`:<>\"|?*`)"
            }
            NameError::NulOrControl => "name contains NUL or a control character",
            NameError::WindowsReserved => "name is a reserved Windows device name",
            NameError::TrailingDotOrSpace => "name ends with `.` or a space",
            NameError::NotUtf8 => "name is not valid UTF-8",
        })
    }
}

/// Why a tree encoding or tree value was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeError {
    BadMagic,
    UnsupportedVersion(u32),
    Truncated,
    TrailingBytes,
    TooManyEntries {
        count: u64,
        limit: u64,
    },
    UnknownEntryKind(u8),
    InvalidName {
        name: String,
        reason: NameError,
    },
    NotSorted {
        previous: String,
        next: String,
    },
    CaseCollision {
        first: String,
        second: String,
    },
    DirectorySize {
        name: String,
    },
    TooDeep {
        limit: usize,
    },
    /// A child id from another domain, or a blob used as a directory (or the
    /// reverse).
    ChildMismatch {
        name: String,
        child: ObjectId,
    },
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TreeError::BadMagic => f.write_str("not a tree encoding (bad magic)"),
            TreeError::UnsupportedVersion(v) => write!(f, "unsupported tree schema version {v}"),
            TreeError::Truncated => f.write_str("truncated tree encoding"),
            TreeError::TrailingBytes => f.write_str("trailing bytes after tree encoding"),
            TreeError::TooManyEntries { count, limit } => {
                write!(f, "tree has {count} entries; limit is {limit}")
            }
            TreeError::UnknownEntryKind(k) => write!(f, "unknown tree entry kind tag {k}"),
            TreeError::InvalidName { name, reason } => {
                write!(f, "invalid entry name {name:?}: {reason}")
            }
            TreeError::NotSorted { previous, next } => write!(
                f,
                "entries are not in strictly ascending byte order ({previous:?} then {next:?})"
            ),
            TreeError::CaseCollision { first, second } => write!(
                f,
                "entries {first:?} and {second:?} collide on case-insensitive file systems"
            ),
            TreeError::DirectorySize { name } => {
                write!(f, "directory entry {name:?} carries a nonzero size")
            }
            TreeError::TooDeep { limit } => write!(f, "tree nesting exceeds {limit} levels"),
            TreeError::ChildMismatch { name, child } => write!(
                f,
                "entry {name:?} cannot point to {child}: wrong domain or object kind"
            ),
        }
    }
}

/// Why derivation inputs were rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DerivationError {
    BadMagic,
    UnsupportedSchema(u32),
    Truncated,
    TrailingBytes,
    SourceOutputDomain,
    UnknownDomain(u8),
    InvalidLabel { field: &'static str, value: String },
    NoInputs,
    TooManyEntries { field: &'static str, limit: usize },
    NotSorted { field: &'static str },
    InvalidObjectId,
}

impl fmt::Display for DerivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DerivationError::BadMagic => f.write_str("not a derivation encoding (bad magic)"),
            DerivationError::UnsupportedSchema(v) => {
                write!(f, "unsupported derivation input schema version {v}")
            }
            DerivationError::Truncated => f.write_str("truncated derivation encoding"),
            DerivationError::TrailingBytes => {
                f.write_str("trailing bytes after derivation encoding")
            }
            DerivationError::SourceOutputDomain => f.write_str(
                "derivations produce interface or artifact objects; source is only ingested",
            ),
            DerivationError::UnknownDomain(t) => write!(f, "unknown domain tag {t}"),
            DerivationError::InvalidLabel { field, value } => {
                write!(f, "invalid {field} label {value:?}")
            }
            DerivationError::NoInputs => f.write_str("a derivation needs at least one input"),
            DerivationError::TooManyEntries { field, limit } => {
                write!(f, "{field} has more than {limit} entries")
            }
            DerivationError::NotSorted { field } => {
                write!(f, "{field} keys are not in strictly ascending order")
            }
            DerivationError::InvalidObjectId => f.write_str("invalid object id encoding"),
        }
    }
}

/// Every failure the store reports. Nothing here is a trust verdict: a digest
/// match proves only content identity, never provenance.
#[derive(Debug)]
pub enum CasError {
    Io {
        op: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// A stored object's contents no longer hash to its identity.
    Corrupt {
        id: ObjectId,
        actual: ObjectId,
        path: PathBuf,
    },
    /// Ingested contents did not match the caller's expected identity.
    DigestMismatch {
        expected: ObjectId,
        actual: ObjectId,
    },
    /// Stored size metadata disagrees with the object on disk.
    SizeMismatch {
        id: ObjectId,
        expected: u64,
        actual: u64,
    },
    NotFound(ObjectId),
    WrongKind {
        id: ObjectId,
        expected: ObjectKind,
    },
    DomainMismatch {
        expected: Domain,
        found: Domain,
    },
    InvalidId(String),
    InvalidName {
        path: PathBuf,
        reason: NameError,
    },
    InvalidTree(TreeError),
    InvalidDerivation(DerivationError),
    /// A derivation record exists and names a different output: the builder
    /// is nondeterministic or one of the outputs is wrong. Never overwritten.
    DerivationConflict {
        key: DerivationKey,
        existing: ObjectId,
        proposed: ObjectId,
    },
    /// A derivation record on disk does not decode or does not hash to its key.
    CorruptDerivationRecord {
        key: DerivationKey,
        path: PathBuf,
    },
    /// Symlinks, devices, sockets and FIFOs cannot be ingested or materialized.
    UnsupportedFileType {
        path: PathBuf,
        kind: &'static str,
    },
    DestinationExists(PathBuf),
    /// The store root or one of its internal directories is not a plain
    /// directory, or carries an unknown layout marker.
    UnsafeStoreLayout {
        path: PathBuf,
        reason: String,
    },
    /// `CopyStrategy::ReflinkOnly` was requested and cloning failed.
    ReflinkUnavailable {
        path: PathBuf,
        source: io::Error,
    },
    /// Publishing kept racing with corrupt objects being replaced.
    PublishContention(ObjectId),
}

impl CasError {
    pub(crate) fn io(op: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        CasError::Io {
            op,
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for CasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CasError::Io { op, path, source } => {
                write!(f, "{op} {}: {source}", path.display())
            }
            CasError::Corrupt { id, actual, path } => write!(
                f,
                "corrupt object {id} at {}: contents hash to {actual}",
                path.display()
            ),
            CasError::DigestMismatch { expected, actual } => {
                write!(f, "expected {expected}, ingested contents hash to {actual}")
            }
            CasError::SizeMismatch {
                id,
                expected,
                actual,
            } => write!(f, "object {id} has size {actual}, expected {expected}"),
            CasError::NotFound(id) => write!(f, "object {id} is not in the store"),
            CasError::WrongKind { id, expected } => write!(f, "object {id} is not a {expected}"),
            CasError::DomainMismatch { expected, found } => {
                write!(f, "expected a {expected} object, found {found}")
            }
            CasError::InvalidId(text) => write!(f, "invalid object id {text:?}"),
            CasError::InvalidName { path, reason } => {
                write!(f, "cannot store {}: {reason}", path.display())
            }
            CasError::InvalidTree(e) => write!(f, "invalid tree: {e}"),
            CasError::InvalidDerivation(e) => write!(f, "invalid derivation inputs: {e}"),
            CasError::DerivationConflict {
                key,
                existing,
                proposed,
            } => write!(
                f,
                "derivation {key} already maps to {existing}; refusing to replace it with {proposed}"
            ),
            CasError::CorruptDerivationRecord { key, path } => {
                write!(f, "corrupt derivation record {key} at {}", path.display())
            }
            CasError::UnsupportedFileType { path, kind } => {
                write!(
                    f,
                    "{} is a {kind}; only regular files and directories are supported",
                    path.display()
                )
            }
            CasError::DestinationExists(path) => {
                write!(
                    f,
                    "materialization destination {} already exists",
                    path.display()
                )
            }
            CasError::UnsafeStoreLayout { path, reason } => {
                write!(f, "unsafe store layout at {}: {reason}", path.display())
            }
            CasError::ReflinkUnavailable { path, source } => {
                write!(f, "reflink to {} failed: {source}", path.display())
            }
            CasError::PublishContention(id) => {
                write!(f, "could not publish {id}: repeated concurrent corruption")
            }
        }
    }
}

impl std::error::Error for CasError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CasError::Io { source, .. } | CasError::ReflinkUnavailable { source, .. } => {
                Some(source)
            }
            _ => None,
        }
    }
}

impl From<TreeError> for CasError {
    fn from(e: TreeError) -> Self {
        CasError::InvalidTree(e)
    }
}

impl From<DerivationError> for CasError {
    fn from(e: DerivationError) -> Self {
        CasError::InvalidDerivation(e)
    }
}

pub type Result<T, E = CasError> = std::result::Result<T, E>;
