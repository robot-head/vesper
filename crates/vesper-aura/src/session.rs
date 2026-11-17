use crate::{CloseDecision, PreparedUpdate, Session, Transaction, UiThread, ViewError, ViewTree};

/// Transactional retained-backend boundary.
///
/// Implementers MUST apply the entire transaction atomically, including
/// subscription ownership and change notifications. On `Err`, widget state,
/// registrations, root, revision and externally visible notifications must be
/// unchanged. Staging or a real rollback protocol is required. Calling arbitrary
/// native APIs sequentially and returning an error after partial effects does
/// not satisfy this trait's contract; such adapters are not supported.
///
/// Disconnections precede destruction in the plan. Slot notifications occur
/// once per SetSlot and only after a successful commit; subscribers must not
/// observe a partially applied tree. Backends must reject foreign/stale plans.
/// The recording backend enforces this contract with staged retained state.
/// The trait is a semantic obligation on trusted host code, not an unsafe trait
/// or a proof that all third-party implementations are transactional.
pub trait Backend {
    type Error;
    fn apply(&mut self, thread: &UiThread, transaction: &Transaction) -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub enum InitError {
    WrongThread,
    View(ViewError),
    IdentityExhausted,
}
#[derive(Debug)]
pub struct InitFailure<B> {
    pub error: InitError,
    pub backend: B,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrepareError {
    WrongThread,
    View(ViewError),
}
#[derive(Debug)]
pub struct PrepareFailure {
    pub error: PrepareError,
    /// The caller retains the complete proposed declarative input on failure.
    pub tree: Option<ViewTree>,
}

#[derive(Debug)]
pub enum CommitError<E> {
    WrongThread,
    ForeignSession,
    StaleRevision,
    Closed,
    Backend(E),
}
#[derive(Debug)]
pub struct CommitFailure<E> {
    pub error: CommitError<E>,
    pub update: PreparedUpdate,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitReport {
    pub revision: u64,
    pub patches: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseAction {
    Veto,
    Allow,
}
#[derive(Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    Vetoed,
    Closed(CommitReport),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseRequestError {
    WrongThread,
    AlreadyPending,
    Closed,
    Exhausted,
}

#[derive(Debug)]
pub enum CloseError<E> {
    WrongThread,
    ForeignSession,
    StaleDecision,
    Prepare(PrepareFailure),
    Commit(Box<CommitFailure<E>>),
}
#[derive(Debug)]
pub struct CloseFailure<E> {
    pub decision: CloseDecision,
    pub error: CloseError<E>,
}
#[derive(Debug)]
pub enum ShutdownError<E> {
    WrongThread,
    PendingClose,
    Prepare(PrepareError),
    Commit(CommitError<E>),
}
#[derive(Debug)]
pub struct ShutdownFailure<B: Backend> {
    pub session: Session<B>,
    pub error: ShutdownError<B::Error>,
    pub update: Option<PreparedUpdate>,
}
