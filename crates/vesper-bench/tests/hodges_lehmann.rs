use vesper_bench::{HodgesLehmannError, MAX_HODGES_LEHMANN_PAIRS, hodges_lehmann_paired};

#[test]
fn point_estimate_retains_quarter_units_and_integer_extremes() {
    let one = hodges_lehmann_paired(&[3]).unwrap();
    assert_eq!(one.estimate_times_four, 12);
    assert_eq!(one.interval, None);
    let two = hodges_lehmann_paired(&[1, 2]).unwrap();
    assert_eq!(two.estimate_times_four, 6);
    let four = hodges_lehmann_paired(&[1, 2, 5, 7]).unwrap();
    // Walsh averages' central pair is 3.5 and 4, median 3.75.
    assert_eq!(four.estimate_times_four, 15);
    let extremes = hodges_lehmann_paired(&[i64::MIN, i64::MAX]).unwrap();
    assert_eq!(extremes.estimate_times_four, -2);
}

#[test]
fn signed_rank_intervals_match_independent_sign_enumeration() {
    for n in 6..=12usize {
        let differences: Vec<i64> = (0..n).map(|i| 1i64 << i).collect();
        let result = hodges_lehmann_paired(&differences).unwrap();
        let interval = result.interval.unwrap();
        let mut distribution = vec![0u64; n * (n + 1) / 2 + 1];
        for mask in 0u32..(1u32 << n) {
            let rank_sum: usize = (0..n).filter(|i| mask & (1 << i) != 0).map(|i| i + 1).sum();
            distribution[rank_sum] += 1;
        }
        let total = 1u64 << n;
        let mut tail = 0;
        let mut selected = None;
        for (rank, outcomes) in distribution.into_iter().enumerate() {
            tail += outcomes;
            if tail * 40 <= total {
                selected = Some((rank, tail));
            } else {
                break;
            }
        }
        let (rank, tail) = selected.unwrap();
        let mut walsh = Vec::new();
        for i in 0..n {
            for j in i..n {
                walsh.push(i128::from(differences[i]) + i128::from(differences[j]));
            }
        }
        walsh.sort_unstable();
        assert_eq!(interval.critical_rank, rank);
        assert_eq!(
            (interval.covered_outcomes, interval.total_outcomes),
            (total - 2 * tail, total)
        );
        assert_eq!(
            (interval.lower_times_two, interval.upper_times_two),
            (walsh[rank], walsh[walsh.len() - 1 - rank])
        );
        assert!(interval.covered_outcomes * 100 >= interval.total_outcomes * 95);
        let shifted: Vec<i64> = differences.iter().map(|x| x + 1000).collect();
        let shifted = hodges_lehmann_paired(&shifted).unwrap();
        assert_eq!(
            shifted.estimate_times_four,
            result.estimate_times_four + 4000
        );
        assert_eq!(
            shifted.interval.unwrap().lower_times_two,
            interval.lower_times_two + 2000
        );
        assert_eq!(
            shifted.interval.unwrap().upper_times_two,
            interval.upper_times_two + 2000
        );
    }
}

#[test]
fn bounds_and_ambiguous_ranks_fail_closed() {
    assert_eq!(
        hodges_lehmann_paired(&[]),
        Err(HodgesLehmannError::EmptyEvidence)
    );
    assert_eq!(
        hodges_lehmann_paired(&[1; MAX_HODGES_LEHMANN_PAIRS + 1]),
        Err(HodgesLehmannError::TooManyPairs)
    );
    assert_eq!(
        hodges_lehmann_paired(&[0, 1]),
        Err(HodgesLehmannError::ZeroDifference)
    );
    assert_eq!(
        hodges_lehmann_paired(&[-1, 1]),
        Err(HodgesLehmannError::TiedAbsoluteDifferences)
    );
    assert_eq!(
        hodges_lehmann_paired(&[1, 3, 5]),
        Err(HodgesLehmannError::TiedWalshAverages)
    );
    let maximum: Vec<i64> = (0..MAX_HODGES_LEHMANN_PAIRS).map(|i| 1i64 << i).collect();
    let interval = hodges_lehmann_paired(&maximum).unwrap().interval.unwrap();
    assert_eq!(interval.total_outcomes, 1u64 << MAX_HODGES_LEHMANN_PAIRS);
}

#[test]
fn intervals_cover_zero_for_exact_signed_rank_fraction() {
    // Superincreasing magnitudes keep every signed Walsh average distinct.
    for n in 6..=10usize {
        let magnitudes: Vec<i64> = (0..n).map(|i| 10i64.pow(i as u32)).collect();
        let mut covered = 0u64;
        let mut expected = None;
        for mask in 0u32..(1u32 << n) {
            let sample: Vec<i64> = magnitudes
                .iter()
                .enumerate()
                .map(|(i, &v)| if mask & (1 << i) == 0 { -v } else { v })
                .collect();
            let interval = hodges_lehmann_paired(&sample).unwrap().interval.unwrap();
            expected = Some(interval.covered_outcomes);
            if interval.lower_times_two <= 0 && interval.upper_times_two >= 0 {
                covered += 1;
            }
        }
        assert_eq!(covered, expected.unwrap(), "sample size {n}");
    }
}

#[test]
fn paired_difference_gate_uses_exact_interval_bounds_and_preserves_evidence() {
    use vesper_bench::{Decision, PairedDifferenceInconclusive, gate_paired_difference};
    let positive = [1, 2, 4, 8, 16, 32];
    let pass = gate_paired_difference(&positive, 32).unwrap();
    assert_eq!(pass.decision, Decision::Pass);
    assert_eq!(pass.reason, None);
    assert_eq!(pass.estimate, hodges_lehmann_paired(&positive).unwrap());
    let overlap = gate_paired_difference(&positive, 1).unwrap();
    assert_eq!(overlap.decision, Decision::Inconclusive);
    assert_eq!(
        overlap.reason,
        Some(PairedDifferenceInconclusive::IntervalCrossesThreshold)
    );
    assert_eq!(
        gate_paired_difference(&positive, 0).unwrap().decision,
        Decision::Regression
    );
    assert_eq!(
        gate_paired_difference(&positive, i64::MIN)
            .unwrap()
            .decision,
        Decision::Regression
    );
    assert_eq!(
        gate_paired_difference(&positive, i64::MAX)
            .unwrap()
            .decision,
        Decision::Pass
    );
    let negative = [-1, -2, -4, -8, -16, -32];
    assert_eq!(
        gate_paired_difference(&negative, -1).unwrap().decision,
        Decision::Pass
    );
    assert_eq!(
        gate_paired_difference(&negative, -33).unwrap().decision,
        Decision::Regression
    );
    let too_few = gate_paired_difference(&positive[..5], 0).unwrap();
    assert_eq!(too_few.decision, Decision::Inconclusive);
    assert_eq!(
        too_few.reason,
        Some(PairedDifferenceInconclusive::InsufficientPairs)
    );
    assert!(too_few.estimate.interval.is_none());
    assert_eq!(
        gate_paired_difference(&[0, 1], 0),
        Err(HodgesLehmannError::ZeroDifference)
    );
    assert_eq!(
        gate_paired_difference(&[-1, 1], 0),
        Err(HodgesLehmannError::TiedAbsoluteDifferences)
    );
}

#[test]
fn paired_difference_gate_compares_fractional_bounds_without_rounding() {
    use vesper_bench::{Decision, gate_paired_difference};
    let sample = [
        26226, 107474, 116740, 146317, 234054, 256788, 288390, 670488, 772247, 777573,
    ];
    let estimate = hodges_lehmann_paired(&sample).unwrap();
    let interval = estimate.interval.unwrap();
    assert!(interval.lower_times_two % 2 != 0 || interval.upper_times_two % 2 != 0);
    assert_eq!(interval.upper_times_two, 1_034_361);
    assert_eq!(
        gate_paired_difference(&sample, 517180).unwrap().decision,
        Decision::Inconclusive
    );
    assert_eq!(
        gate_paired_difference(&sample, 517181).unwrap().decision,
        Decision::Pass
    );
    for threshold in [0, 130139, 130140, 130141, 517180, 517181, 1_000_000] {
        let gate = gate_paired_difference(&sample, threshold).unwrap();
        let exact_threshold = i128::from(threshold) * 2;
        let expected = if interval.upper_times_two <= exact_threshold {
            Decision::Pass
        } else if interval.lower_times_two > exact_threshold {
            Decision::Regression
        } else {
            Decision::Inconclusive
        };
        assert_eq!(gate.decision, expected, "threshold {threshold}");
    }
}
