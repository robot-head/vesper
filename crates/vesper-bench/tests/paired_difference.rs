use vesper_bench::{
    MAX_EXACT_DIFFERENCE_SAMPLES, PairedDifferenceError, PairedSample, exact_paired_differences,
};

fn pair(baseline: u64, candidate: u64, operations: u64) -> PairedSample {
    PairedSample::new(baseline, candidate, operations).unwrap()
}

#[test]
fn exact_per_operation_differences_preserve_sign_and_order() {
    let samples = [
        pair(100, 110, 5),
        pair(100, 85, 5),
        pair(19, 19, 7),
        pair(1, u64::MAX, 2),
    ];
    let before = samples;
    assert_eq!(
        exact_paired_differences(&samples).unwrap(),
        [2, -3, 0, i64::MAX]
    );
    assert_eq!(samples, before);
}

#[test]
fn fractional_and_out_of_range_differences_fail_at_their_index() {
    let fractional = [pair(1, 3, 2), pair(100, 95, 2)];
    assert_eq!(
        exact_paired_differences(&fractional),
        Err(PairedDifferenceError::NonIntegral { index: 1 })
    );
    let too_large = [pair(1, 1u64 << 63, 1), pair(1, (1u64 << 63) + 1, 1)];
    assert_eq!(
        exact_paired_differences(&too_large),
        Err(PairedDifferenceError::OutOfRange { index: 1 })
    );
    let negative_min = [pair((1u64 << 63) + 1, 1, 1)];
    assert_eq!(exact_paired_differences(&negative_min).unwrap(), [i64::MIN]);
    let negative_overflow = [pair(u64::MAX, 1, 1)];
    assert_eq!(
        exact_paired_differences(&negative_overflow),
        Err(PairedDifferenceError::OutOfRange { index: 0 })
    );
}

#[test]
fn sample_count_limits_fail_without_partial_output() {
    assert_eq!(
        exact_paired_differences(&[]),
        Err(PairedDifferenceError::EmptyEvidence)
    );
    assert_eq!(
        exact_paired_differences(&[pair(1, 2, 1); MAX_EXACT_DIFFERENCE_SAMPLES]).unwrap(),
        vec![1; MAX_EXACT_DIFFERENCE_SAMPLES]
    );
    assert_eq!(
        exact_paired_differences(&[pair(1, 2, 1); MAX_EXACT_DIFFERENCE_SAMPLES + 1]),
        Err(PairedDifferenceError::TooManySamples)
    );
}
