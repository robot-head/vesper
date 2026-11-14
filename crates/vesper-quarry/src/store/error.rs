//! Typed errors of the table store.
use crate::device::DeviceError;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreErrorKind {
    /// Host I/O failed. The store is poisoned; reopen it to recover.
    Io,
    /// The simulated device crashed. The store is poisoned.
    Crashed,
    /// A page, log frame or catalog failed validation where no repair is
    /// possible. The store is poisoned.
    Corrupt,
    /// Write-write conflict: the row is locked by another active
    /// transaction, or a version newer than this transaction's snapshot was
    /// committed (first committer wins). The transaction stays usable.
    Conflict,
    /// The table or row does not exist in this transaction's view.
    NotFound,
    /// A configured or hard limit was exceeded.
    Limit,
    /// The log region cannot hold this commit group even after a checkpoint.
    LogFull,
    /// The call is not valid in the current state (for example DDL with
    /// active transactions, or use after poisoning).
    State,
    /// The handle belongs to a different store.
    WrongStore,
    Overflow,
    /// A row does not match its table schema.
    Data,
    /// The SQL layer rejected a statement or query (see the message).
    Sql(crate::ErrorKind),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreError {
    pub kind: StoreErrorKind,
    pub message: String,
}
impl StoreError {
    pub(crate) fn new(kind: StoreErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    /// Errors after which in-memory state may be ahead of the device.
    pub(crate) fn poisons(&self) -> bool {
        matches!(
            self.kind,
            StoreErrorKind::Io | StoreErrorKind::Crashed | StoreErrorKind::Corrupt
        )
    }
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}
impl std::error::Error for StoreError {}
impl From<DeviceError> for StoreError {
    fn from(e: DeviceError) -> Self {
        match e {
            DeviceError::Crashed => Self::new(StoreErrorKind::Crashed, "device crashed"),
            other => Self::new(StoreErrorKind::Io, other.to_string()),
        }
    }
}
impl From<crate::Error> for StoreError {
    fn from(e: crate::Error) -> Self {
        Self::new(StoreErrorKind::Sql(e.kind), e.message)
    }
}
pub(crate) fn corrupt(message: impl Into<String>) -> StoreError {
    StoreError::new(StoreErrorKind::Corrupt, message)
}
pub(crate) fn limit(message: impl Into<String>) -> StoreError {
    StoreError::new(StoreErrorKind::Limit, message)
}
pub(crate) fn overflow(message: impl Into<String>) -> StoreError {
    StoreError::new(StoreErrorKind::Overflow, message)
}
pub type StoreResult<T> = Result<T, StoreError>;
