//! Bounded confidence-set inversion of an exact paired signed-rank test.
//!
//! At the true location, the coverage interpretation requires independent,
//! equiprobable signs conditional on the centered nonzero magnitudes and zero
//! pattern. This API does not establish those assumptions or measurement
//! provenance, and is separate from the benchmark regression gate.
use crate::PairedSample;
use std::fmt;

/// The inversion visits at most 1057 open intervals and breakpoint atoms.
pub const MAX_SIGNED_RANK_CONFIDENCE_PAIRS: usize = 32;
const MAX_BREAKPOINTS: usize =
    MAX_SIGNED_RANK_CONFIDENCE_PAIRS * (MAX_SIGNED_RANK_CONFIDENCE_PAIRS + 1) / 2;
const MAX_RANK_SUM: usize =
    MAX_SIGNED_RANK_CONFIDENCE_PAIRS * (MAX_SIGNED_RANK_CONFIDENCE_PAIRS + 1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignedRankConfidenceError {
    EmptyEvidence,
    TooManyPairs,
    AllocationFailed,
}
impl fmt::Display for SignedRankConfidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "signed-rank confidence: {self:?}")
    }
}
impl std::error::Error for SignedRankConfidenceError {}

/// One connected portion of the confidence set. Finite endpoints are twice
/// the location, so integer and half-integer bounds are exact. A `None` lower
/// or upper endpoint is unbounded; unbounded endpoints are never inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedRankConfidenceSegment {
    pub lower_times_two: Option<i128>,
    pub lower_inclusive: bool,
    pub upper_times_two: Option<i128>,
    pub upper_inclusive: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairedSampleConfidenceError {
    EmptyEvidence,
    TooManySamples,
    OperationsMismatch { index: usize },
    DifferenceOutOfRange { index: usize },
    Inversion(SignedRankConfidenceError),
}
impl fmt::Display for PairedSampleConfidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "paired sample confidence: {self:?}")
    }
}
impl std::error::Error for PairedSampleConfidenceError {}

/// Confidence-set endpoints are stored in twice-total-difference units.
/// Divide each finite endpoint by `2 * operations_per_arm` to obtain the exact
/// per-operation location; neither component is rounded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedSampleConfidenceSet {
    operations_per_arm: u64,
    segments: Vec<SignedRankConfidenceSegment>,
}
impl PairedSampleConfidenceSet {
    pub fn operations_per_arm(&self) -> u64 {
        self.operations_per_arm
    }
    pub fn segments(&self) -> &[SignedRankConfidenceSegment] {
        &self.segments
    }
    pub fn endpoint_denominator(&self) -> u128 {
        2 * u128::from(self.operations_per_arm)
    }
}

/// Build an exact per-operation confidence set from paired totals with the same
/// operation count. The common positive denominator preserves all signed-rank
/// orderings, ties and fractional endpoints. A difference outside i64 or a
/// differing operation count is rejected; no samples are silently discarded.
/// Callers must establish common units, measurement provenance, and the
/// conditional-sign assumptions of [`signed_rank_confidence_set`]. This does
/// not change the existing ratio gate.
pub fn paired_samples_confidence_set(
    samples: &[PairedSample],
) -> Result<PairedSampleConfidenceSet, PairedSampleConfidenceError> {
    if samples.is_empty() {
        return Err(PairedSampleConfidenceError::EmptyEvidence);
    }
    if samples.len() > MAX_SIGNED_RANK_CONFIDENCE_PAIRS {
        return Err(PairedSampleConfidenceError::TooManySamples);
    }
    let operations = samples[0].operations_per_arm();
    let mut differences = [0i64; MAX_SIGNED_RANK_CONFIDENCE_PAIRS];
    for (index, sample) in samples.iter().enumerate() {
        if sample.operations_per_arm() != operations {
            return Err(PairedSampleConfidenceError::OperationsMismatch { index });
        }
        let delta = i128::from(sample.candidate_total()) - i128::from(sample.baseline_total());
        differences[index] = i64::try_from(delta)
            .map_err(|_| PairedSampleConfidenceError::DifferenceOutOfRange { index })?;
    }
    let segments = signed_rank_confidence_set(&differences[..samples.len()])
        .map_err(PairedSampleConfidenceError::Inversion)?;
    Ok(PairedSampleConfidenceSet {
        operations_per_arm: operations,
        segments,
    })
}

/// Exact non-rejection set for a two-sided 5% Wilcoxon signed-rank test,
/// returned as ordered, disjoint connected segments. Zeros are discarded and
/// tied absolute differences receive midranks. At a candidate location where
/// every centered difference is zero, the test does not reject.
///
/// The result covers the true location with probability at least 95% only
/// under the conditional independent-equiprobable-sign assumption described
/// at module level. This is not a claim about ABBA measurement provenance, a
/// rounded estimate, or the existing benchmark regression gate. Each input
/// is a caller-provided signed integer paired difference in a common unit.
pub fn signed_rank_confidence_set(
    differences: &[i64],
) -> Result<Vec<SignedRankConfidenceSegment>, SignedRankConfidenceError> {
    let n = differences.len();
    if n == 0 {
        return Err(SignedRankConfidenceError::EmptyEvidence);
    }
    if n > MAX_SIGNED_RANK_CONFIDENCE_PAIRS {
        return Err(SignedRankConfidenceError::TooManyPairs);
    }
    let mut breakpoints = [0i128; MAX_BREAKPOINTS];
    let mut count = 0;
    for i in 0..n {
        for j in i..n {
            breakpoints[count] = i128::from(differences[i]) + i128::from(differences[j]);
            count += 1;
        }
    }
    breakpoints[..count].sort_unstable();
    let mut unique = 0;
    for i in 0..count {
        if unique == 0 || breakpoints[i] != breakpoints[unique - 1] {
            breakpoints[unique] = breakpoints[i];
            unique += 1;
        }
    }
    let mut result = Vec::new();
    result
        .try_reserve_exact(2 * unique + 1)
        .map_err(|_| SignedRankConfidenceError::AllocationFailed)?;
    let mut left = None;
    for &right in &breakpoints[..unique] {
        // Locations are represented by four times their value for probes.
        // Consecutive breakpoints are in twice-location units; their sum is
        // the exact midpoint in four-times-location units.
        let probe = left.map_or(2 * right - 1, |l| l + right);
        if accepts(differences, probe) {
            append(
                &mut result,
                SignedRankConfidenceSegment {
                    lower_times_two: left,
                    lower_inclusive: false,
                    upper_times_two: Some(right),
                    upper_inclusive: false,
                },
            );
        }
        if accepts(differences, 2 * right) {
            append(
                &mut result,
                SignedRankConfidenceSegment {
                    lower_times_two: Some(right),
                    lower_inclusive: true,
                    upper_times_two: Some(right),
                    upper_inclusive: true,
                },
            );
        }
        left = Some(right);
    }
    // Nonempty inputs always produce at least one breakpoint.
    if accepts(differences, 2 * breakpoints[unique - 1] + 1) {
        append(
            &mut result,
            SignedRankConfidenceSegment {
                lower_times_two: left,
                lower_inclusive: false,
                upper_times_two: None,
                upper_inclusive: false,
            },
        );
    }
    Ok(result)
}

fn append(result: &mut Vec<SignedRankConfidenceSegment>, next: SignedRankConfidenceSegment) {
    if let Some(last) = result.last_mut()
        && last.upper_times_two == next.lower_times_two
        && (last.upper_inclusive || next.lower_inclusive)
    {
        last.upper_times_two = next.upper_times_two;
        last.upper_inclusive = next.upper_inclusive;
    } else {
        result.push(next);
    }
}

/// Whether the exact two-sided p-value is strictly greater than 0.05.
fn accepts(differences: &[i64], location_times_four: i128) -> bool {
    let mut observations = [(0u128, false); MAX_SIGNED_RANK_CONFIDENCE_PAIRS];
    let mut n = 0;
    for &difference in differences {
        let centered = 4 * i128::from(difference) - location_times_four;
        if centered != 0 {
            observations[n] = (centered.unsigned_abs(), centered > 0);
            n += 1;
        }
    }
    if n == 0 {
        return true;
    }
    observations[..n].sort_unstable_by_key(|value| value.0);
    let mut distribution = [0u64; MAX_RANK_SUM + 1];
    distribution[0] = 1;
    let mut total_rank = 0usize;
    let mut positive_rank = 0usize;
    let mut start = 0;
    while start < n {
        let mut end = start + 1;
        while end < n && observations[end].0 == observations[start].0 {
            end += 1;
        }
        let rank = start + 1 + end;
        for &(_, positive) in &observations[start..end] {
            if positive {
                positive_rank += rank;
            }
            for sum in (0..=total_rank).rev() {
                distribution[sum + rank] += distribution[sum];
            }
            total_rank += rank;
        }
        start = end;
    }
    let tail_limit = positive_rank.min(total_rank - positive_rank);
    let tail: u64 = distribution[..=tail_limit].iter().sum();
    let total = 1u64 << n;
    // 2*tail / total > 1/20, equivalently 40*tail > total. With at most
    // 32 nonzero observations, both the subset counts and 40*tail fit u64.
    40 * tail > total
}
