use vesper_bench::{MAX_SIGNED_RANK_PAIRS, SignedRankError, signed_rank_exact};

#[test]
fn exact_tied_signed_ranks_match_enumerated_sign_assignments() {
    for magnitudes in [&[1i64, 1, 2, 4][..], &[1, 2, 2, 4, 4], &[1, 2, 3, 4, 5, 6]] {
        let n = magnitudes.len();
        // Independently compute doubled midranks by counting smaller/equal values.
        let ranks: Vec<usize> = magnitudes
            .iter()
            .map(|value| {
                let less = magnitudes.iter().filter(|other| *other < value).count();
                let equal = magnitudes.iter().filter(|other| *other == value).count();
                2 * less + equal + 1
            })
            .collect();
        let max_sum: usize = ranks.iter().sum();
        let mut histogram = vec![0u64; max_sum + 1];
        for mask in 0u32..(1u32 << n) {
            let sum: usize = ranks
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, rank)| rank)
                .sum();
            histogram[sum] += 1;
        }
        let total = 1u64 << n;
        for mask in 0u32..(1u32 << n) {
            let mut sample: Vec<i64> = magnitudes
                .iter()
                .enumerate()
                .map(|(i, &x)| if mask & (1 << i) != 0 { x } else { -x })
                .collect();
            sample.push(0);
            let observed: usize = ranks
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, rank)| rank)
                .sum();
            let tail: u64 = histogram[..=observed.min(max_sum - observed)].iter().sum();
            let actual = signed_rank_exact(&sample).unwrap();
            assert_eq!(actual.nonzero_pairs, n);
            assert_eq!(actual.discarded_zero_pairs, 1);
            assert_eq!(actual.twice_w_positive, observed as u64);
            assert_eq!(
                (actual.numerator, actual.denominator),
                ((2 * tail).min(total), total)
            );
            sample.reverse();
            assert_eq!(signed_rank_exact(&sample).unwrap(), actual);
        }
    }
}

#[test]
fn zeros_ties_limits_and_extreme_magnitudes_are_explicit() {
    let result = signed_rank_exact(&[0, -1, 1, 2]).unwrap();
    assert_eq!(
        (
            result.nonzero_pairs,
            result.discarded_zero_pairs,
            result.tied_magnitude_groups
        ),
        (3, 1, 1)
    );
    assert_eq!(
        (
            result.twice_w_positive,
            result.numerator,
            result.denominator
        ),
        (9, 6, 8)
    );
    assert_eq!(signed_rank_exact(&[]), Err(SignedRankError::EmptyEvidence));
    assert_eq!(
        signed_rank_exact(&[0, 0]),
        Err(SignedRankError::NoNonzeroDifferences)
    );
    assert_eq!(
        signed_rank_exact(&[1; MAX_SIGNED_RANK_PAIRS + 1]),
        Err(SignedRankError::TooManyPairs)
    );
    let max = signed_rank_exact(&[1; MAX_SIGNED_RANK_PAIRS]).unwrap();
    assert_eq!(
        (max.twice_w_positive, max.numerator, max.denominator),
        (1056, 2, 1u64 << 32)
    );
    let extreme = signed_rank_exact(&[i64::MIN, 1]).unwrap();
    assert_eq!(
        (
            extreme.twice_w_positive,
            extreme.numerator,
            extreme.denominator
        ),
        (2, 4, 4)
    );
}

#[test]
fn fractional_paired_samples_match_independent_sign_enumeration() {
    use vesper_bench::{PairedSample, signed_rank_exact_paired_samples};
    // The first two values are the same magnitude despite different fractions.
    let magnitudes = [(1u64, 2u64), (2, 4), (1, 3), (3, 4), (2, 1)];
    let n = magnitudes.len();
    let ranks: Vec<usize> = magnitudes
        .iter()
        .map(|&(numerator, denominator)| {
            let less = magnitudes
                .iter()
                .filter(|&&(other, divisor)| {
                    u128::from(other) * u128::from(denominator)
                        < u128::from(numerator) * u128::from(divisor)
                })
                .count();
            let equal = magnitudes
                .iter()
                .filter(|&&(other, divisor)| {
                    u128::from(other) * u128::from(denominator)
                        == u128::from(numerator) * u128::from(divisor)
                })
                .count();
            2 * less + equal + 1
        })
        .collect();
    let total_rank: usize = ranks.iter().sum();
    let mut histogram = vec![0u64; total_rank + 1];
    for mask in 0u32..(1u32 << n) {
        let sum: usize = ranks
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, rank)| rank)
            .sum();
        histogram[sum] += 1;
    }
    for mask in 0u32..(1u32 << n) {
        let mut samples: Vec<PairedSample> = magnitudes
            .iter()
            .enumerate()
            .map(|(i, &(numerator, denominator))| {
                let candidate = if mask & (1 << i) != 0 {
                    100 + numerator
                } else {
                    100 - numerator
                };
                PairedSample::new(100, candidate, denominator).unwrap()
            })
            .collect();
        samples.push(PairedSample::new(100, 100, 7).unwrap());
        let observed: usize = ranks
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, rank)| rank)
            .sum();
        let tail: u64 = histogram[..=observed.min(total_rank - observed)]
            .iter()
            .sum();
        let actual = signed_rank_exact_paired_samples(&samples).unwrap();
        assert_eq!(
            (
                actual.nonzero_pairs,
                actual.discarded_zero_pairs,
                actual.tied_magnitude_groups
            ),
            (n, 1, 1)
        );
        assert_eq!(actual.twice_w_positive, observed as u64);
        assert_eq!(
            (actual.numerator, actual.denominator),
            ((2 * tail).min(1 << n), 1 << n)
        );
        samples.reverse();
        assert_eq!(signed_rank_exact_paired_samples(&samples).unwrap(), actual);
    }
}

#[test]
fn paired_sample_ranks_handle_u64_extremes_and_existing_integer_semantics() {
    use vesper_bench::{PairedSample, signed_rank_exact_paired_samples};
    let sample = |a, b, n| PairedSample::new(a, b, n).unwrap();
    let extreme = [
        sample(1, u64::MAX, u64::MAX),
        sample(u64::MAX, 1, u64::MAX),
        sample(1, 2, 1),
    ];
    let result = signed_rank_exact_paired_samples(&extreme).unwrap();
    assert_eq!(
        (
            result.tied_magnitude_groups,
            result.twice_w_positive,
            result.numerator,
            result.denominator
        ),
        (1, 9, 6, 8)
    );
    let integer = [
        sample((1u64 << 63) + 1, 1, 1),
        sample(100, 103, 1),
        sample(100, 100, 1),
    ];
    assert_eq!(
        signed_rank_exact_paired_samples(&integer),
        signed_rank_exact(&[i64::MIN, 3, 0])
    );
    assert_eq!(
        signed_rank_exact_paired_samples(&[]),
        Err(SignedRankError::EmptyEvidence)
    );
    assert_eq!(
        signed_rank_exact_paired_samples(&[sample(1, 1, 1)]),
        Err(SignedRankError::NoNonzeroDifferences)
    );
    assert_eq!(
        signed_rank_exact_paired_samples(&[sample(1, 2, 1); MAX_SIGNED_RANK_PAIRS + 1]),
        Err(SignedRankError::TooManyPairs)
    );
}
