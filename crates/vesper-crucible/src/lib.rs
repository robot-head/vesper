#![forbid(unsafe_code)]
//! Vesper Crucible (design §29, excluding §29.3 `vesper migrate`).
//!
//! * [`ChoiceTape`] / [`Source`]: one monotone 64-bit choice tape drives both
//!   structured input generation and SimPulse scheduling decisions; saved
//!   tapes replay deterministically.
//! * [`shrink`]: the shortlex tape reducer. It re-runs the unmodified
//!   property and keeps only valid, still-failing, strictly `(len, sum)`-
//!   smaller tapes, so generators that only build legal protocol sequences
//!   stay legal while shrinking, with no type-specific shrinkers.
//!   `lean/CrucibleShrink.lean` proves the order well-founded and the
//!   modelled loop terminating.
//! * [`fuzz`]: coverage-guided fuzzing fed by the [`Probe`] API, with a
//!   bounded comparison-operand (input-to-state) solver step.
//! * [`run_fibers`]: concolic SimPulse schedule exploration on the real
//!   `vesper-simpulse` simulator.
//! * [`refute`]: counterexample search for a predicate with a minimal
//!   deterministic reproduction.
//!
//! Scope and honesty notes: Cranelift edge/comparison instrumentation of
//! Vesper source, MPK differential shadowing, the `vesper fuzz` / `vesper
//! verify --refute` CLI, and the source-level `FuzzSource` linear effect are
//! not implemented here. The solver is bounded operand replacement, not SMT.
//! Rust ownership in this crate is not evidence that Vesper's source typing
//! rules (Theorem 2's obligation validity) are implemented.

mod exec;
mod fuzz;
mod refute;
mod rng;
mod shrink;
mod sim;
mod source;
mod tape;

pub use exec::{PANIC_KIND, Verdict};
pub use fuzz::{Counterexample, FuzzConfig, FuzzReport, Strategy, fuzz};
pub use refute::{REFUTED_KIND, Refutation, refute};
pub use rng::SplitMix64;
pub use shrink::{DoesNotFail, ShrinkConfig, ShrinkStats, Shrunk, replay, shrink};
pub use sim::{Fiber, FiberStep, SimConfig, SimRun, replay_fibers, run_fibers};
pub use source::{
    Arbitrary, CmpKind, CmpOperands, CmpRecord, DEFAULT_MAX_DRAWS, DrawKind, Failure, Invalid,
    MAX_CMP_BYTES, MAX_CMP_RECORDS, Probe, PropError, Source, check,
};
pub use tape::{ChoiceTape, MAX_TAPE_LEN, ShrinkKey, TAPE_HEADER, TapeError};
pub use vesper_simpulse::Journal;
