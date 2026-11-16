//! Bounded confidence-set inversion of an exact paired signed-rank test.
//!
//! At the true location, the coverage interpretation requires independent,
//! equiprobable signs conditional on the centered nonzero magnitudes and zero
//! pattern. This API does not establish those assumptions or measurement
//! provenance, and is separate from the benchmark regression gate.
use crate::{Decision, PairedSample};
use std::{cmp::Ordering, fmt};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairedSampleGateInconclusive {
    EmptyConfidenceSet,
    ThresholdNotSeparated,
}

/// A decision and its complete exact per-operation confidence evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedSampleConfidenceGate {
    pub decision: Decision,
    pub reason: Option<PairedSampleGateInconclusive>,
    pub confidence: PairedSampleConfidenceSet,
}

/// Compare an exact paired-sample confidence set against a maximum allowed
/// candidate-minus-baseline difference per operation, in the samples' common
/// unit. The entire set must be at or below the threshold to pass, or strictly
/// above it to report regression. Empty or nonseparating sets are inconclusive.
/// This inherits [`paired_samples_confidence_set`]'s restrictions and
/// assumptions, and does not change the existing ratio gate.
pub fn gate_paired_sample_confidence(
    samples: &[PairedSample],
    max_difference: i64,
) -> Result<PairedSampleConfidenceGate, PairedSampleConfidenceError> {
    let confidence = paired_samples_confidence_set(samples)?;
    let (decision, reason) = classify_paired_sample_set(
        confidence.segments(),
        confidence.operations_per_arm(),
        max_difference,
    );
    Ok(PairedSampleConfidenceGate {
        decision,
        reason,
        confidence,
    })
}

// The confidence builder returns ordered, nonempty segments and a positive
// common operation count. The full threshold product can exceed i128, so it
// is compared as a sign and u128 magnitude rather than rounded or saturated.
fn compare_endpoint(endpoint: i128, operations: u64, max_difference: i64) -> Ordering {
    // |i64::MIN| * u64::MAX * 2 is below 2^128, so this product fits u128.
    let threshold = u128::from(max_difference.unsigned_abs()) * u128::from(operations) * 2;
    match (endpoint < 0, max_difference < 0) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => endpoint.unsigned_abs().cmp(&threshold),
        (true, true) => threshold.cmp(&endpoint.unsigned_abs()),
    }
}

fn classify_paired_sample_set(
    segments: &[SignedRankConfidenceSegment],
    operations: u64,
    max_difference: i64,
) -> (Decision, Option<PairedSampleGateInconclusive>) {
    let (Some(first), Some(last)) = (segments.first(), segments.last()) else {
        return (
            Decision::Inconclusive,
            Some(PairedSampleGateInconclusive::EmptyConfidenceSet),
        );
    };
    if last.upper_times_two.is_some_and(|upper| {
        compare_endpoint(upper, operations, max_difference) != Ordering::Greater
    }) {
        (Decision::Pass, None)
    } else if first.lower_times_two.is_some_and(|lower| {
        let order = compare_endpoint(lower, operations, max_difference);
        order == Ordering::Greater || (order == Ordering::Equal && !first.lower_inclusive)
    }) {
        (Decision::Regression, None)
    } else {
        (
            Decision::Inconclusive,
            Some(PairedSampleGateInconclusive::ThresholdNotSeparated),
        )
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignedRankGateInconclusive {
    EmptyConfidenceSet,
    ThresholdNotSeparated,
}

/// The complete confidence set is retained as evidence for every decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedRankConfidenceGate {
    pub decision: Decision,
    pub reason: Option<SignedRankGateInconclusive>,
    pub segments: Vec<SignedRankConfidenceSegment>,
}

/// Classify a candidate-minus-baseline difference against a maximum allowed
/// difference in the same units. Pass requires every retained location to be
/// at or below the threshold; regression requires every retained location to
/// be strictly above it. Empty and nonseparating sets are inconclusive.
/// This inherits the confidence set's assumptions and does not establish
/// measurement provenance or change the existing ratio gate.
pub fn gate_signed_rank_confidence(
    differences: &[i64],
    max_difference: i64,
) -> Result<SignedRankConfidenceGate, SignedRankConfidenceError> {
    let segments = signed_rank_confidence_set(differences)?;
    let (decision, reason) = classify(&segments, max_difference);
    Ok(SignedRankConfidenceGate {
        decision,
        reason,
        segments,
    })
}

// `signed_rank_confidence_set` returns ordered disjoint nonempty segments.
fn classify(
    segments: &[SignedRankConfidenceSegment],
    max_difference: i64,
) -> (Decision, Option<SignedRankGateInconclusive>) {
    let (Some(first), Some(last)) = (segments.first(), segments.last()) else {
        return (
            Decision::Inconclusive,
            Some(SignedRankGateInconclusive::EmptyConfidenceSet),
        );
    };
    let threshold = 2 * i128::from(max_difference);
    if last.upper_times_two.is_some_and(|upper| upper <= threshold) {
        (Decision::Pass, None)
    } else if first
        .lower_times_two
        .is_some_and(|lower| lower > threshold || (lower == threshold && !first.lower_inclusive))
    {
        (Decision::Regression, None)
    } else {
        (
            Decision::Inconclusive,
            Some(SignedRankGateInconclusive::ThresholdNotSeparated),
        )
    }
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

#[cfg(test)]
mod paired_gate_tests {
    use super::*;

    fn segment(
        lower: Option<i128>,
        lower_inclusive: bool,
        upper: Option<i128>,
        upper_inclusive: bool,
    ) -> SignedRankConfidenceSegment {
        SignedRankConfidenceSegment {
            lower_times_two: lower,
            lower_inclusive,
            upper_times_two: upper,
            upper_inclusive,
        }
    }

    #[test]
    fn compare_thresholds_respects_fractional_open_and_disconnected_sets() {
        assert_eq!(
            classify_paired_sample_set(&[], 2, 0),
            (
                Decision::Inconclusive,
                Some(PairedSampleGateInconclusive::EmptyConfidenceSet)
            )
        );
        let right_open = [segment(Some(0), false, Some(2), true)];
        assert_eq!(
            classify_paired_sample_set(&right_open, 2, 0),
            (Decision::Regression, None)
        );
        let right_closed = [segment(Some(0), true, Some(2), true)];
        assert_eq!(
            classify_paired_sample_set(&right_closed, 2, 0),
            (
                Decision::Inconclusive,
                Some(PairedSampleGateInconclusive::ThresholdNotSeparated)
            )
        );
        let left_open = [segment(None, false, Some(0), false)];
        assert_eq!(
            classify_paired_sample_set(&left_open, 2, 0),
            (Decision::Pass, None)
        );
        let disconnected = [
            segment(Some(-4), true, Some(-2), true),
            segment(Some(2), true, Some(4), true),
        ];
        assert_eq!(
            classify_paired_sample_set(&disconnected, 2, 0),
            (
                Decision::Inconclusive,
                Some(PairedSampleGateInconclusive::ThresholdNotSeparated)
            )
        );
        let all = [segment(None, false, None, false)];
        assert_eq!(
            classify_paired_sample_set(&all, u64::MAX, i64::MIN).0,
            Decision::Inconclusive
        );
        assert_eq!(
            classify_paired_sample_set(&all, u64::MAX, i64::MAX).0,
            Decision::Inconclusive
        );
        assert_eq!(
            compare_endpoint(i128::MIN, u64::MAX, i64::MIN),
            Ordering::Greater
        );
        assert_eq!(
            compare_endpoint(i128::MAX, u64::MAX, i64::MAX),
            Ordering::Less
        );
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    fn segment(
        lower: Option<i128>,
        lower_inclusive: bool,
        upper: Option<i128>,
        upper_inclusive: bool,
    ) -> SignedRankConfidenceSegment {
        SignedRankConfidenceSegment {
            lower_times_two: lower,
            lower_inclusive,
            upper_times_two: upper,
            upper_inclusive,
        }
    }

    #[test]
    fn threshold_classification_respects_open_closed_disconnected_and_empty_sets() {
        assert_eq!(
            classify(&[], 0),
            (
                Decision::Inconclusive,
                Some(SignedRankGateInconclusive::EmptyConfidenceSet)
            )
        );
        let right_open = [segment(Some(0), false, Some(2), true)];
        assert_eq!(classify(&right_open, 0), (Decision::Regression, None));
        let right_closed = [segment(Some(0), true, Some(2), true)];
        assert_eq!(
            classify(&right_closed, 0),
            (
                Decision::Inconclusive,
                Some(SignedRankGateInconclusive::ThresholdNotSeparated)
            )
        );
        let left_open = [segment(None, false, Some(0), false)];
        assert_eq!(classify(&left_open, 0), (Decision::Pass, None));
        let disconnected = [
            segment(Some(-4), true, Some(-2), true),
            segment(Some(2), true, Some(4), true),
        ];
        assert_eq!(
            classify(&disconnected, 0),
            (
                Decision::Inconclusive,
                Some(SignedRankGateInconclusive::ThresholdNotSeparated)
            )
        );
        let unbounded_right = [segment(Some(2), true, None, false)];
        assert_eq!(classify(&unbounded_right, 0), (Decision::Regression, None));
        let all = [segment(None, false, None, false)];
        assert_eq!(classify(&all, i64::MIN).0, Decision::Inconclusive);
        assert_eq!(classify(&all, i64::MAX).0, Decision::Inconclusive);
    }
}
