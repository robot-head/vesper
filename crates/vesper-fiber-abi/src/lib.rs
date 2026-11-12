//! Native Pulse continuation ABI, version 2 (prototype).
//!
//! * [`abi`]: versioned `repr(C)` evidence/continuation layouts and offsets.
//! * [`evidence`]: never-reused handler identities and owned evidence vectors.
//! * [`clif`]: Cranelift emission helpers that encode the ABI.
//! * [`prototype`]: hand-built CLIF entries exercising park/resume, JIT for the
//!   host and object emission for x86_64/AArch64.
//! * `fiber`, `stack`, `switch` (Linux x86_64/AArch64): pinned stacks,
//!   context switching and one-shot continuations.
//! * `fpenv` (Linux): the floating-point control state each switch preserves.
//! * `pulse` (Linux): `vesper_pulse::scheduler::Resume` adapter and the
//!   owned cancellation/shutdown/drain runtime.
//!
//! See `docs/rfcs/native-pulse-abi.md` for the ownership, lifetime and
//! relocation contracts and for what this prototype does not establish.
pub mod abi;
pub mod clif;
pub mod data;
pub mod evidence;
pub mod prototype;

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod fiber;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod fpenv;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod pulse;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod stack;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod switch;

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use fiber::{
    ContinuationTable, Exit, Fiber, FiberConfig, Outcome, Parked, RawContinuation, Rejected,
    ResumeError, Resumption, TableCancel,
};
