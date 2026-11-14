//! Executing host benchmark harness, not a compiler/PMU integration. Actual
//! `std::time::Instant` elapsed nanoseconds (units, not a clock resolution or
//! accuracy guarantee) include loop/callback/black_box
//! overhead. Inputs and output capacity are prepared before timing; teardown
//! and disposal run afterward. No zero-allocation/spill, cycle, precision or
//! scheduler-isolation claim is made.
//!
//! [`layout`] randomizes heap offsets, stack padding and (through a process
//! driver) environment size per paired run and tests whether a difference
//! survives; it does not relocate machine code. [`causal`] is a Coz-style
//! virtual-speedup profiler for instrumented host threads.
#![forbid(unsafe_code)]
mod arithmetic;
pub mod causal;
mod execution;
pub mod layout;
mod statistics;
pub use arithmetic::*;
pub use execution::*;
pub use statistics::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfig(&'static str),
    Limit(&'static str),
    ArithmeticOverflow,
    AllocationFailed,
    ClockOverflow,
    ZeroDuration,
    EmptyEvidence,
    NonPositiveEvidence,
    InvalidThreshold,
    InvalidCounterWidth,
    CounterValueOutOfRange,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceRequirement {
    HardwareCycles,
    RetiredInstructions,
    L1DataMisses,
    LlcMisses,
    BranchMispredicts,
    SimdVectorOperations,
    TmaBackendSlots,
    ZeroAllocation,
    Vectorization,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    NoPmuProvider,
    NoAllocationInstrumentation,
    NoCompilerVectorizationEvidence,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnavailableEvidence {
    pub requirement: EvidenceRequirement,
    pub reason: UnavailableReason,
}
/// This backend has no hardware/static evidence provider. A requirement remains
/// inconclusive; caller assertions and wall time never authorize it to pass.
pub fn unavailable_evidence(requirement: EvidenceRequirement) -> UnavailableEvidence {
    let reason = match requirement {
        EvidenceRequirement::ZeroAllocation => UnavailableReason::NoAllocationInstrumentation,
        EvidenceRequirement::Vectorization => UnavailableReason::NoCompilerVectorizationEvidence,
        _ => UnavailableReason::NoPmuProvider,
    };
    UnavailableEvidence {
        requirement,
        reason,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvidenceGate {
    pub decision: Decision,
    pub unavailable: UnavailableEvidence,
}
pub fn evaluate_evidence_requirement(requirement: EvidenceRequirement) -> EvidenceGate {
    EvidenceGate {
        decision: Decision::Inconclusive,
        unavailable: unavailable_evidence(requirement),
    }
}
