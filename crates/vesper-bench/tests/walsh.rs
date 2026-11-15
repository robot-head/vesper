use vesper_bench::{MAX_WALSH_PAIRS, WalshEstimateError, walsh_pseudomedian};

#[test]
fn ties_zeros_and_even_medians_keep_full_multiplicity() {
    let zeros = walsh_pseudomedian(&[0, 0, 0]).unwrap();
    assert_eq!(
        (zeros.pairs, zeros.walsh_averages, zeros.estimate_times_four),
        (3, 6, 0)
    );
    // Walsh averages are [1, 1, 1, 1.5, 1.5, 2], with median 1.25.
    let tied = walsh_pseudomedian(&[1, 1, 2]).unwrap();
    assert_eq!((tied.walsh_averages, tied.estimate_times_four), (6, 5));
    let reordered = walsh_pseudomedian(&[2, 1, 1]).unwrap();
    assert_eq!(reordered, tied);
    let shifted = walsh_pseudomedian(&[11, 11, 12]).unwrap();
    assert_eq!(shifted.estimate_times_four, tied.estimate_times_four + 40);
}

#[test]
fn limits_and_signed_extremes_are_exact() {
    assert_eq!(
        walsh_pseudomedian(&[]),
        Err(WalshEstimateError::EmptyEvidence)
    );
    assert_eq!(
        walsh_pseudomedian(&[0; MAX_WALSH_PAIRS + 1]),
        Err(WalshEstimateError::TooManyPairs)
    );
    let extreme = walsh_pseudomedian(&[i64::MIN, i64::MAX]).unwrap();
    assert_eq!(extreme.estimate_times_four, -2);
    let maximum = walsh_pseudomedian(&[i64::MAX; MAX_WALSH_PAIRS]).unwrap();
    assert_eq!(maximum.walsh_averages, 528);
    assert_eq!(maximum.estimate_times_four, 4 * i128::from(i64::MAX));
}
