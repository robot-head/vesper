//! Native Pulse continuation ABI, version 2 (prototype).
//!
//! * [`abi`]: versioned `repr(C)` evidence/continuation layouts and offsets.
//! * [`evidence`]: never-reused handler identities and owned evidence vectors.
//! * [`clif`]: Cranelift emission helpers that encode the ABI.
//! * [`prototype`]: hand-built CLIF entries exercising park/resume, JIT for the
//!   host and object emission for x86_64/AArch64.
//! * `fiber`, `stack`, `switch` (Linux x86_64/AArch64, macOS AArch64 and
//!   Windows x86_64): pinned stacks, context switching and one-shot
//!   continuations. Windows uses `stack_windows.rs` and `switch_windows.rs`.
//! * `fault` (Windows only): reports structured exceptions raised on a fiber
//!   stack.
//! * `fpenv` (same targets): the floating-point control state each switch
//!   preserves.
//! * `pulse` (Linux only): `vesper_pulse::scheduler::Resume` adapter and the
//!   owned cancellation/shutdown/drain runtime. Direct fibers on macOS do not
//!   make the io_uring driver or that scheduler portable.
//!
//! See `docs/rfcs/native-pulse-abi.md` for the ownership, lifetime and
//! relocation contracts and for what this prototype does not establish.
pub mod abi;
pub mod clif;
pub mod data;
pub mod evidence;
pub mod prototype;

#[cfg(all(vesper_direct_fibers, windows))]
pub mod fault;
#[cfg(vesper_direct_fibers)]
pub mod fiber;
#[cfg(vesper_direct_fibers)]
pub mod fpenv;
#[cfg(all(vesper_direct_fibers, target_os = "linux"))]
pub mod pulse;
#[cfg(vesper_direct_fibers)]
#[cfg_attr(windows, path = "stack_windows.rs")]
pub mod stack;
#[cfg(vesper_direct_fibers)]
#[cfg_attr(windows, path = "switch_windows.rs")]
pub mod switch;

#[cfg(vesper_direct_fibers)]
pub use fiber::{
    ContinuationTable, Exit, Fiber, FiberConfig, Outcome, Parked, RawContinuation, Rejected,
    ResumeError, Resumption, TableCancel,
};
