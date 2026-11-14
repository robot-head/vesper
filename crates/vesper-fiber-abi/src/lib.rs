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
//! * `image` (same targets): byte images of parked fibers (stack bytes,
//!   decoded register frame) and restoring them at the same fixed address.
//! * `fpenv` (same targets): the floating-point control state each switch
//!   preserves.
//! * `pulse` (same targets): `vesper_pulse::scheduler::Resume` adapter and the
//!   owned cancellation/shutdown/drain runtime. It runs on any
//!   `vesper_pulse` driver: io_uring or epoll on Linux, kqueue on macOS,
//!   IOCP on Windows (see `vesper_pulse::select::open`).
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
#[cfg(vesper_direct_fibers)]
pub mod image;
#[cfg(vesper_direct_fibers)]
pub mod pulse;
#[cfg(vesper_direct_fibers)]
#[cfg_attr(windows, path = "stack_windows.rs")]
pub mod stack;
#[cfg(vesper_direct_fibers)]
#[cfg_attr(windows, path = "switch_windows.rs")]
pub mod switch;

#[cfg(vesper_direct_fibers)]
pub use fiber::{
    ContinuationTable, Exit, Fiber, FiberConfig, ImageRejected, Outcome, Parked, RawContinuation,
    Rejected, ResumeError, Resumption, TableCancel,
};
#[cfg(vesper_direct_fibers)]
pub use image::{FiberImage, FrameAbi, ImageError, RegisterFrame, StackImage};
