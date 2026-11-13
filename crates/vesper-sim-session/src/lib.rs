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
//! Scenario version 2 adds a bounded [`model`]: process crash/restart epochs,
//! virtual write-ahead logs with an explicit volatile/durable boundary and
//! torn-tail recovery, and a virtual network with delay, partitions and
//! injected failures. Its transitions form a separate, versioned model trace;
//! replay reproduces outputs, lost tasks and exact durable bytes or rejects
//! the first mismatch.
//!
//! Scenario version 3 adds the Vesper Warp model: source `XdpNic` frames over
//! a conserved UMEM (`umem-v1`) and adversarial datagram links (`datagram-v1`:
//! Gilbert–Elliott burst loss, independent loss, reordering, duplication, a
//! finite bottleneck queue and NAT rebinding). See [`model::warp`].
//!
//! This executes the reference evaluator under deterministic simulated
//! effects. It is not native execution, kernel I/O, exhaustive scheduling or
//! DPOR, not a crash-consistency check of any real filesystem or kernel I/O
//! path, and not a production storage or network implementation.
pub mod identity;
pub mod model;
pub mod program;
pub mod scenario;
pub mod session;
pub mod value;

pub use identity::{ExecutionIdentity, IdentityMismatch, SEMANTICS, executable_sha256};
pub use model::{
    CrashChoice, DurableLog, DurableState, FailOp, FailureSpec, Fault, LinkSpec, LogSpec,
    LostState, MODEL_SEMANTICS, ModelEvent, ModelLimits, ModelSnapshot, ModelSpec, NetworkSpec,
    ProcessSpec, RETIRED_WARP_MODEL_SEMANTICS, StaleSource, TimedFault, WARP_MODEL_SEMANTICS,
};
pub use program::{CheckedProgram, SourceRejected};
pub use scenario::{
    Action, ArgumentWrite, Binding, CancelSpec, Limits, Outcome, RuntimeOptions, Scenario,
    ScenarioError, ScriptEntry, TaskSpec,
};
pub use session::{
    LostOwner, LostTask, PendingOperation, Recording, RecoveredOwner, RejectedCompletion, Report,
    RunResult, Session, SessionError, SourceEvent, StalledOwner, TaskSlot, TerminalFailure,
};
