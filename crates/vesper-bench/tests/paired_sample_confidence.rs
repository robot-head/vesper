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
