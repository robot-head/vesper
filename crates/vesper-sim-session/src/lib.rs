//! Deterministic source-level simulation for Vesper.
//!
//! [`CheckedProgram`] parses and checks real source. A versioned [`Scenario`]
//! binds each checked external operation signature to a simulated action
//! (virtual clock, yield, virtual sleep, scripted I/O, or host-completed I/O).
//! [`Session`] runs every task on the reference CEK evaluator and routes each
//! external linear operation through a `vesper_simpulse::Simulator` task token,
//! producing a [`Recording`] that replays only under the same executable,
//! source, scenario, bindings, options and semantics.
//!
//! This executes the reference evaluator under deterministic simulated
//! effects. It is not native execution, kernel I/O, exhaustive scheduling or
//! DPOR, and not a production storage or network implementation.
pub mod identity;
pub mod program;
pub mod scenario;
pub mod session;
pub mod value;

pub use identity::{ExecutionIdentity, IdentityMismatch, SEMANTICS, executable_sha256};
pub use program::{CheckedProgram, SourceRejected};
pub use scenario::{
    Action, ArgumentWrite, Binding, CancelSpec, Limits, Outcome, RuntimeOptions, Scenario,
    ScenarioError, ScriptEntry, TaskSpec,
};
pub use session::{
    PendingOperation, Recording, RecoveredOwner, Report, RunResult, Session, SessionError,
    SourceEvent, StalledOwner, TaskSlot, TerminalFailure,
};
