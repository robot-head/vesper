//! Exact arithmetic for numerical `@bench` limits over one supplied sample.
//!
//! These are caller-provided values, not authenticated measurement evidence.
//! This module does not instrument allocations, prove vectorization or verify
//! layout configuration. Requested requirements without evidence remain
//! inconclusive; it does not execute benchmarks or attach source attributes.

use crate::contract::{BenchContract, Decimal};
use crate::wire::{PmuField, PmuSample};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    Counter(PmuField),
    InputBytes,
    Operations,
    ZeroInputBytes,
    ZeroOperations,
    AllocationEvidence,
    VectorizationEvidence,
    LayoutEvidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateStatus {
    NotRequested,
    Pass,
    Fail,
    Inconclusive(Unavailable),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overall {
    NotRequested,
    Pass,
    Fail,
    Inconclusive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateReport {
    pub instructions_per_byte: GateStatus,
    pub l1d_misses_per_op: GateStatus,
    pub zero_alloc: GateStatus,
    pub expect_vectorized: GateStatus,
    pub stabilizer_pages: GateStatus,
    pub overall: Overall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateError {
    /// Public programmatic contracts may contain a scale outside the parser's
    /// supported range. Refuse it even if measurement evidence is unavailable.
    InvalidDecimalScale,
}
impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDecimalScale => f.write_str("decimal scale exceeds 18"),
        }
    }
}
impl std::error::Error for GateError {}

/// Counts refer to the same caller-defined sample window. `None` is unknown,
/// not zero. The caller is responsible for obtaining and associating values.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counts {
    pub input_bytes: Option<u64>,
    pub operations: Option<u64>,
}

fn ratio_gate(
    counter: Option<u64>,
    denominator: Option<u64>,
    limit: Decimal,
    field: PmuField,
    missing_denominator: Unavailable,
    zero_denominator: Unavailable,
) -> GateStatus {
    let Some(counter) = counter else {
        return GateStatus::Inconclusive(Unavailable::Counter(field));
    };
    let Some(denominator) = denominator else {
        return GateStatus::Inconclusive(missing_denominator);
    };
    if denominator == 0 {
        return GateStatus::Inconclusive(zero_denominator);
    }
    // scale <= 18 is checked before this call. Both products fit u128:
    // u64::MAX * 10^18 < 2^128, and u64::MAX * u64::MAX < 2^128.
    let left = u128::from(counter) * 10u128.pow(limit.scale);
    let right = u128::from(limit.coefficient) * u128::from(denominator);
    if left <= right {
        GateStatus::Pass
    } else {
        GateStatus::Fail
    }
}

/// Evaluate the documented fields against one caller-supplied sample.
/// A known numerical failure wins over inconclusive requirements. Without any
/// requested requirement, the overall status is `NotRequested`, not a pass.
/// `false` boolean flags impose no requirement. `true` requires evidence that
/// this arithmetic-only API cannot supply and therefore remains inconclusive.
pub fn evaluate(
    contract: &BenchContract,
    sample: &PmuSample,
    counts: Counts,
) -> Result<GateReport, GateError> {
    if contract
        .max_instructions_per_byte
        .is_some_and(|v| v.scale > 18)
    {
        return Err(GateError::InvalidDecimalScale);
    }
    let instructions_per_byte = match contract.max_instructions_per_byte {
        Some(limit) => ratio_gate(
            sample.get(PmuField::InstructionsRetired),
            counts.input_bytes,
            limit,
            PmuField::InstructionsRetired,
            Unavailable::InputBytes,
            Unavailable::ZeroInputBytes,
        ),
        None => GateStatus::NotRequested,
    };
    let l1d_misses_per_op = match contract.max_l1d_misses_per_op {
        Some(limit) => ratio_gate(
            sample.get(PmuField::L1dReadMisses),
            counts.operations,
            Decimal {
                coefficient: limit,
                scale: 0,
            },
            PmuField::L1dReadMisses,
            Unavailable::Operations,
            Unavailable::ZeroOperations,
        ),
        None => GateStatus::NotRequested,
    };
    let zero_alloc = if contract.zero_alloc == Some(true) {
        GateStatus::Inconclusive(Unavailable::AllocationEvidence)
    } else {
        GateStatus::NotRequested
    };
    let expect_vectorized = if contract.expect_vectorized == Some(true) {
        GateStatus::Inconclusive(Unavailable::VectorizationEvidence)
    } else {
        GateStatus::NotRequested
    };
    let stabilizer_pages = if contract.stabilizer_pages.is_some() {
        GateStatus::Inconclusive(Unavailable::LayoutEvidence)
    } else {
        GateStatus::NotRequested
    };
    let statuses = [
        instructions_per_byte,
        l1d_misses_per_op,
        zero_alloc,
        expect_vectorized,
        stabilizer_pages,
    ];
    let overall = if statuses.contains(&GateStatus::Fail) {
        Overall::Fail
    } else if statuses
        .iter()
        .any(|s| matches!(s, GateStatus::Inconclusive(_)))
    {
        Overall::Inconclusive
    } else if statuses.contains(&GateStatus::Pass) {
        Overall::Pass
    } else {
        Overall::NotRequested
    };
    Ok(GateReport {
        instructions_per_byte,
        l1d_misses_per_op,
        zero_alloc,
        expect_vectorized,
        stabilizer_pages,
        overall,
    })
}
