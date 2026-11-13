//! Vesper Resonance (design §32): host-library slice of hard-realtime DSP and
//! control loops.
//!
//! What this crate implements:
//!
//! * [`confine`]: the §32.5 realtime confinement verifier. A `@realtime` root
//!   may not *transitively* reach a forbidden effect (allocation, blocking
//!   locks, blocking syscalls, async, I/O, unwinding, ...), an unknown or
//!   unresolved indirect callee, a loop the policy does not bound, recursion,
//!   or a worst-case stack depth over its budget. It is a reachability
//!   analysis over declared effects, not a callee-name allowlist
//!   (`docs/rfcs/expansion-37-review.md`).
//! * [`clif`]: builds the same [`confine::CallGraph`] from real Cranelift
//!   `ir::Function`s (calls, indirect calls, loops, stack slots) plus a
//!   symbol-effect table, so integration can run the verifier on emitted code.
//! * [`pdc`]: the delay-compensated DAG compiler (topological schedule,
//!   automatic compensation delay lines, explicit delayed feedback edges,
//!   buffer liveness reuse) and an allocation-free block executor.
//! * [`spsc`]: wait-free SPSC handoff built on the `rtrb` ring:
//!   [`spsc::RtRetirementSink`] / [`spsc::RtJanitor`] retire values off the
//!   realtime thread, [`spsc::RtMailbox`] swaps in new state.
//! * [`lease`]: the linear [`lease::RtBufferLease`] output lease.
//! * [`events`]: the 16-byte `RtParamEventWire` codec and sample-accurate
//!   block splitting with parameter ramps.
//! * [`kernels`]: the §32.3 state-variable-filter voice bank, the realtime
//!   quantum and a 1 kHz PID torque controller.
//! * [`denorm`]: the `ConstDenorm` scope (x86_64 MXCSR FTZ|DAZ, AArch64 FPCR
//!   FZ), the only module allowed to use `unsafe`.
//!
//! Linear owners (`RtBufferLease`) are non-Clone, non-Copy `#[must_use]`
//! values whose only exits are consuming calls; a dropped one is counted as a
//! leak by its auditor. That is host-library discipline enforced by Rust
//! ownership; it is **not** an implementation of Vesper's source typing rules
//! for `linear` or `eff [Realtime]`.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod clif;
pub mod confine;
pub mod denorm;
pub mod events;
pub mod kernels;
pub mod lease;
pub mod pdc;
pub mod spsc;

/// One verifier diagnostic (wave-five shared vocabulary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifierFinding {
    /// Function the finding is reported against (the realtime root's callee
    /// where the problem is, or the root itself).
    pub function: String,
    /// Cranelift instruction index (layout order) when there is one.
    pub inst: Option<u32>,
    /// Stable rule identifier, e.g. `rt-forbidden-effect`.
    pub rule: &'static str,
    /// Human-readable explanation, including the call path from the root.
    pub message: String,
}
