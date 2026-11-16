//! Exact integer conversion of paired measurement totals.
//!
//! This is arithmetic over caller-provided `PairedSample`s. It does not
//! establish measurement provenance, common units, or statistical assumptions.
use crate::PairedSample;
use std::fmt;

/// Maximum sample count for the paired-difference analysis bridge.
pub const MAX_EXACT_DIFFERENCE_SAMPLES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairedDifferenceError {
    EmptyEvidence,
    TooManySamples,
    NonIntegral { index: usize },
    OutOfRange { index: usize },
    AllocationFailed,
}
impl fmt::Display for PairedDifferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "paired difference: {self:?}")
    }
}
impl std::error::Error for PairedDifferenceError {}

fn difference(sample: PairedSample, index: usize) -> Result<i64, PairedDifferenceError> {
    // Positive u64 totals and operation counts are guaranteed by PairedSample.
    // Their difference and division fit i128, including all u64 endpoints.
    let delta = i128::from(sample.candidate_total()) - i128::from(sample.baseline_total());
    let operations = i128::from(sample.operations_per_arm());
    if delta % operations != 0 {
        return Err(PairedDifferenceError::NonIntegral { index });
    }
    i64::try_from(delta / operations).map_err(|_| PairedDifferenceError::OutOfRange { index })
}

/// Convert each sample into the exact integer candidate-minus-baseline
/// difference per operation, in the totals' common unit. Fractional or
/// out-of-range values are rejected rather than rounded. Returns no partial
/// vector on error; samples are unchanged. The caller must establish that all
/// totals share a unit and that any downstream inference's assumptions hold.
/// This does not run or change the existing ratio-based regression gate.
pub fn exact_paired_differences(
    samples: &[PairedSample],
) -> Result<Vec<i64>, PairedDifferenceError> {
    if samples.is_empty() {
        return Err(PairedDifferenceError::EmptyEvidence);
    }
    if samples.len() > MAX_EXACT_DIFFERENCE_SAMPLES {
        return Err(PairedDifferenceError::TooManySamples);
    }
    for (index, &sample) in samples.iter().enumerate() {
        difference(sample, index)?;
    }
    let mut result = Vec::new();
    result
        .try_reserve_exact(samples.len())
        .map_err(|_| PairedDifferenceError::AllocationFailed)?;
    for (index, &sample) in samples.iter().enumerate() {
        result.push(difference(sample, index)?);
    }
    Ok(result)
}
