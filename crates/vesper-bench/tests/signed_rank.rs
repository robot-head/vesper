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
