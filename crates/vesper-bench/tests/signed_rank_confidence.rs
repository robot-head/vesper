use vesper_bench::{
    MAX_SIGNED_RANK_CONFIDENCE_PAIRS, SignedRankConfidenceError, SignedRankConfidenceSegment,
    signed_rank_confidence_set,
};

fn contains(segments: &[SignedRankConfidenceSegment], location_times_four: i128) -> bool {
    segments.iter().any(|segment| {
        let lower = segment.lower_times_two.is_none_or(|bound| {
            location_times_four > 2 * bound
                || (location_times_four == 2 * bound && segment.lower_inclusive)
        });
        let upper = segment.upper_times_two.is_none_or(|bound| {
            location_times_four < 2 * bound
                || (location_times_four == 2 * bound && segment.upper_inclusive)
        });
        lower && upper
    })
}

// Independent oracle: calculate each midrank from pairwise comparisons and
// enumerate every sign assignment rather than using the implementation's DP.
fn oracle_accepts(sample: &[i64], location_times_four: i128) -> bool {
    let centered: Vec<i128> = sample
        .iter()
        .map(|x| 4 * i128::from(*x) - location_times_four)
        .filter(|x| *x != 0)
        .collect();
    let n = centered.len();
    if n == 0 {
        return true;
    }
    let ranks: Vec<usize> = centered
        .iter()
        .map(|x| {
            let less = centered
                .iter()
                .filter(|y| y.unsigned_abs() < x.unsigned_abs())
                .count();
            let equal = centered
                .iter()
                .filter(|y| y.unsigned_abs() == x.unsigned_abs())
                .count();
            2 * less + equal + 1
        })
        .collect();
    let total_rank: usize = ranks.iter().sum();
    let positive: usize = centered
        .iter()
        .zip(&ranks)
        .filter(|(x, _)| **x > 0)
        .map(|(_, rank)| rank)
        .sum();
    let limit = positive.min(total_rank - positive);
    let mut tail = 0u64;
    for mask in 0u32..(1u32 << n) {
        let sum: usize = ranks
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, rank)| rank)
            .sum();
        if sum <= limit {
            tail += 1;
        }
    }
    40 * tail > (1u64 << n)
}

#[test]
fn tied_confidence_sets_match_independent_test_inversion() {
    for sample in [
        &[1i64, 1, 2, 2, 3, 3][..],
        &[-3, -3, 0, 1, 2, 4],
        &[1, 2, 4, 8, 16, 32],
    ] {
        let set = signed_rank_confidence_set(sample).unwrap();
        for location_times_four in -20..=140 {
            assert_eq!(
                contains(&set, location_times_four),
                oracle_accepts(sample, location_times_four),
                "{sample:?}, location/4={location_times_four}, set={set:?}"
            );
        }
    }
}

#[test]
fn tied_extremes_and_open_ends_are_preserved() {
    let singleton = signed_rank_confidence_set(&[7; 6]).unwrap();
    assert_eq!(
        singleton,
        vec![SignedRankConfidenceSegment {
            lower_times_two: Some(14),
            lower_inclusive: true,
            upper_times_two: Some(14),
            upper_inclusive: true
        }]
    );
    let all = signed_rank_confidence_set(&[i64::MIN, i64::MAX]).unwrap();
    assert_eq!(
        all,
        vec![SignedRankConfidenceSegment {
            lower_times_two: None,
            lower_inclusive: false,
            upper_times_two: None,
            upper_inclusive: false
        }]
    );
    let ordinary = signed_rank_confidence_set(&[1, 2, 4, 8, 16, 32]).unwrap();
    assert_eq!(
        ordinary,
        vec![SignedRankConfidenceSegment {
            lower_times_two: Some(2),
            lower_inclusive: true,
            upper_times_two: Some(64),
            upper_inclusive: true
        }]
    );
    assert_eq!(
        signed_rank_confidence_set(&[]),
        Err(SignedRankConfidenceError::EmptyEvidence)
    );
    assert_eq!(
        signed_rank_confidence_set(&[0; MAX_SIGNED_RANK_CONFIDENCE_PAIRS + 1]),
        Err(SignedRankConfidenceError::TooManyPairs)
    );
    assert!(signed_rank_confidence_set(&[0; MAX_SIGNED_RANK_CONFIDENCE_PAIRS]).is_ok());
    let minimum = signed_rank_confidence_set(&[i64::MIN; 6]).unwrap();
    assert_eq!(minimum.len(), 1);
    assert_eq!(minimum[0].lower_times_two, Some(2 * i128::from(i64::MIN)));
    assert_eq!(minimum[0].upper_times_two, Some(2 * i128::from(i64::MIN)));
    assert!(minimum[0].lower_inclusive && minimum[0].upper_inclusive);
    let maximum: Vec<i64> = (0..MAX_SIGNED_RANK_CONFIDENCE_PAIRS)
        .map(|i| 1i64 << i)
        .collect();
    let unique_breakpoints: std::collections::BTreeSet<i128> = maximum
        .iter()
        .enumerate()
        .flat_map(|(i, &a)| {
            maximum[i..]
                .iter()
                .map(move |&b| i128::from(a) + i128::from(b))
        })
        .collect();
    assert_eq!(unique_breakpoints.len(), 528);
    let maximum_set = signed_rank_confidence_set(&maximum).unwrap();
    assert!(!maximum_set.is_empty());
    assert!(!contains(&maximum_set, 0));
    assert!(!contains(&maximum_set, 4 * (i128::from(1i64 << 31) + 1)));
}

#[test]
fn tied_sign_assignments_have_at_least_nominal_coverage() {
    let magnitudes = [1i64, 1, 2, 2, 4, 8];
    let mut covered = 0;
    for mask in 0u32..64 {
        let sample: Vec<i64> = magnitudes
            .iter()
            .enumerate()
            .map(|(i, &v)| if mask & (1 << i) == 0 { -v } else { v })
            .collect();
        let set = signed_rank_confidence_set(&sample).unwrap();
        let accepts_zero = contains(&set, 0);
        assert_eq!(accepts_zero, oracle_accepts(&sample, 0));
        covered += usize::from(accepts_zero);
    }
    assert!(covered * 100 >= 64 * 95);
}

#[test]
fn confidence_gate_classifies_real_tied_and_unbounded_sets() {
    use vesper_bench::{Decision, SignedRankGateInconclusive, gate_signed_rank_confidence};
    let singleton = gate_signed_rank_confidence(&[7; 6], 7).unwrap();
    assert_eq!(singleton.decision, Decision::Pass);
    assert_eq!(singleton.reason, None);
    assert_eq!(
        singleton.segments,
        signed_rank_confidence_set(&[7; 6]).unwrap()
    );
    assert_eq!(
        gate_signed_rank_confidence(&[7; 6], 6).unwrap().decision,
        Decision::Regression
    );
    let finite = [1, 2, 4, 8, 16, 32];
    assert_eq!(
        gate_signed_rank_confidence(&finite, 32).unwrap().decision,
        Decision::Pass
    );
    assert_eq!(
        gate_signed_rank_confidence(&finite, 0).unwrap().decision,
        Decision::Regression
    );
    let touching = gate_signed_rank_confidence(&finite, 1).unwrap();
    assert_eq!(touching.decision, Decision::Inconclusive);
    assert_eq!(
        touching.reason,
        Some(SignedRankGateInconclusive::ThresholdNotSeparated)
    );
    let unbounded = gate_signed_rank_confidence(&[1, 2], i64::MAX).unwrap();
    assert_eq!(unbounded.decision, Decision::Inconclusive);
    assert_eq!(
        unbounded.reason,
        Some(SignedRankGateInconclusive::ThresholdNotSeparated)
    );
    assert_eq!(
        gate_signed_rank_confidence(&[], 0),
        Err(SignedRankConfidenceError::EmptyEvidence)
    );
    assert_eq!(
        gate_signed_rank_confidence(&[0; MAX_SIGNED_RANK_CONFIDENCE_PAIRS + 1], 0),
        Err(SignedRankConfidenceError::TooManyPairs)
    );
}
