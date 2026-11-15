use vesper_bench::{
    MAX_SIGNED_RANK_CONFIDENCE_PAIRS, PairedSample, PairedSampleConfidenceError,
    SignedRankConfidenceSegment, paired_samples_confidence_set, signed_rank_confidence_set,
};

fn pair(baseline: u64, candidate: u64, operations: u64) -> PairedSample {
    PairedSample::new(baseline, candidate, operations).unwrap()
}

#[test]
fn common_operations_preserve_fractional_endpoints_without_rounding() {
    let samples = [pair(100, 101, 2); 6];
    let before = samples;
    let result = paired_samples_confidence_set(&samples).unwrap();
    assert_eq!(samples, before);
    assert_eq!(result.operations_per_arm(), 2);
    assert_eq!(result.endpoint_denominator(), 4);
    // The exact singleton is 2/4 = 1/2 per operation.
    assert_eq!(
        result.segments(),
        &[SignedRankConfidenceSegment {
            lower_times_two: Some(2),
            lower_inclusive: true,
            upper_times_two: Some(2),
            upper_inclusive: true,
        }]
    );
    let raw = [1i64, 2, 4, 8, 16, 32];
    let samples = raw.map(|delta| pair(100, 100 + delta as u64, 3));
    let fractional = paired_samples_confidence_set(&samples).unwrap();
    assert_eq!(fractional.endpoint_denominator(), 6);
    assert_eq!(
        fractional.segments(),
        signed_rank_confidence_set(&raw).unwrap()
    );
    assert_eq!(fractional.segments()[0].lower_times_two, Some(2));
    assert_eq!(fractional.segments()[0].upper_times_two, Some(64));
}

#[test]
fn mismatched_counts_and_unrepresentable_totals_fail_at_their_index() {
    assert_eq!(
        paired_samples_confidence_set(&[]),
        Err(PairedSampleConfidenceError::EmptyEvidence)
    );
    let mismatch = [pair(100, 101, 2), pair(100, 101, 3)];
    assert_eq!(
        paired_samples_confidence_set(&mismatch),
        Err(PairedSampleConfidenceError::OperationsMismatch { index: 1 })
    );
    let large_positive = [pair(1, (1u64 << 63) + 1, 1)];
    assert_eq!(
        paired_samples_confidence_set(&large_positive),
        Err(PairedSampleConfidenceError::DifferenceOutOfRange { index: 0 })
    );
    let large_negative = [pair((1u64 << 63) + 2, 1, 1)];
    assert_eq!(
        paired_samples_confidence_set(&large_negative),
        Err(PairedSampleConfidenceError::DifferenceOutOfRange { index: 0 })
    );
    let min = [pair((1u64 << 63) + 1, 1, 1)];
    assert!(paired_samples_confidence_set(&min).is_ok());
}

#[test]
fn maximal_denominator_and_pair_count_are_bounded() {
    let samples = [pair(100, 101, u64::MAX); MAX_SIGNED_RANK_CONFIDENCE_PAIRS];
    let result = paired_samples_confidence_set(&samples).unwrap();
    assert_eq!(result.operations_per_arm(), u64::MAX);
    assert_eq!(result.endpoint_denominator(), 2 * u128::from(u64::MAX));
    assert_eq!(result.segments()[0].lower_times_two, Some(2));
    assert_eq!(result.segments()[0].upper_times_two, Some(2));
    assert_eq!(
        paired_samples_confidence_set(&[pair(1, 2, 1); MAX_SIGNED_RANK_CONFIDENCE_PAIRS + 1]),
        Err(PairedSampleConfidenceError::TooManySamples)
    );
}

#[test]
fn paired_sample_gate_compares_fractional_endpoints_without_rounding() {
    use vesper_bench::{Decision, PairedSampleGateInconclusive, gate_paired_sample_confidence};
    let half = [pair(100, 101, 2); 6];
    let passing = gate_paired_sample_confidence(&half, 1).unwrap();
    assert_eq!(passing.decision, Decision::Pass);
    assert_eq!(passing.reason, None);
    assert_eq!(
        passing.confidence,
        paired_samples_confidence_set(&half).unwrap()
    );
    assert_eq!(
        gate_paired_sample_confidence(&half, 0).unwrap().decision,
        Decision::Regression
    );
    let integer = [pair(100, 102, 2); 6];
    assert_eq!(
        gate_paired_sample_confidence(&integer, 1).unwrap().decision,
        Decision::Pass
    );
    let samples = [1u64, 2, 4, 8, 16, 32].map(|delta| pair(100, 100 + delta, 3));
    assert_eq!(
        gate_paired_sample_confidence(&samples, 0).unwrap().decision,
        Decision::Regression
    );
    let overlap = gate_paired_sample_confidence(&samples, 10).unwrap();
    assert_eq!(overlap.decision, Decision::Inconclusive);
    assert_eq!(
        overlap.reason,
        Some(PairedSampleGateInconclusive::ThresholdNotSeparated)
    );
    assert_eq!(
        gate_paired_sample_confidence(&samples, 11)
            .unwrap()
            .decision,
        Decision::Pass
    );
}

#[test]
fn paired_sample_gate_handles_extreme_products_and_propagates_errors() {
    use vesper_bench::{Decision, PairedSampleGateInconclusive, gate_paired_sample_confidence};
    let tiny = [pair(100, 101, u64::MAX); 6];
    assert_eq!(
        gate_paired_sample_confidence(&tiny, i64::MAX)
            .unwrap()
            .decision,
        Decision::Pass
    );
    assert_eq!(
        gate_paired_sample_confidence(&tiny, i64::MIN)
            .unwrap()
            .decision,
        Decision::Regression
    );
    assert_eq!(
        gate_paired_sample_confidence(&tiny, 0).unwrap().decision,
        Decision::Regression
    );
    let unbounded = gate_paired_sample_confidence(&tiny[..1], i64::MAX).unwrap();
    assert_eq!(unbounded.decision, Decision::Inconclusive);
    assert_eq!(
        unbounded.reason,
        Some(PairedSampleGateInconclusive::ThresholdNotSeparated)
    );
    let mismatch = [pair(100, 101, 2), pair(100, 101, 3)];
    assert_eq!(
        gate_paired_sample_confidence(&mismatch, 0),
        Err(PairedSampleConfidenceError::OperationsMismatch { index: 1 })
    );
    assert_eq!(
        gate_paired_sample_confidence(&[], 0),
        Err(PairedSampleConfidenceError::EmptyEvidence)
    );
}
