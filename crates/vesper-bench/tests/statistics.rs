use vesper_bench::*;
fn pair(a: u64, b: u64) -> PairedSample {
    PairedSample::new(a, b, 1).unwrap()
}
#[test]
fn independent_binomial_rank_and_coverage_vectors() {
    for (n, rank, covered, total) in [
        (6, 1, 62, 64),
        (10, 2, 1002, 1024),
        (20, 6, 1_005_176, 1_048_576),
        (
            64,
            24,
            17_842_322_501_232_739_958u128,
            18_446_744_073_709_551_616u128,
        ),
    ] {
        let samples: Vec<_> = (100..100 + n as u64).map(|b| pair(100, b)).collect();
        let a = analyze_pairs(&samples, 0).unwrap();
        let interval = a.interval.unwrap();
        assert_eq!(
            (interval.lower_rank, interval.upper_rank),
            (rank, n - rank + 1)
        );
        assert_eq!(
            (interval.covered_outcomes, interval.total_outcomes),
            (covered, total)
        );
        assert_eq!(interval.lower.candidate_total(), 100 + rank as u64 - 1);
        assert_eq!(interval.upper.candidate_total(), 100 + (n - rank) as u64);
    }
}
#[test]
fn known_regression_improvement_ties_and_mixed_results() {
    assert_eq!(
        analyze_pairs(&[pair(100, 110); 6], 500).unwrap().decision,
        Decision::Regression
    );
    assert_eq!(
        analyze_pairs(&[pair(100, 90); 6], 0).unwrap().decision,
        Decision::Pass
    );
    assert_eq!(
        analyze_pairs(&[pair(100, 100); 6], 0).unwrap().decision,
        Decision::Pass
    );
    let samples: Vec<_> = [90, 100, 110, 120, 130, 140].map(|b| pair(100, b)).into();
    let a = analyze_pairs(&samples, 0).unwrap();
    assert_eq!(a.decision, Decision::Inconclusive);
    assert_eq!(a.reason, Some(InconclusiveReason::IntervalCrossesThreshold));
    assert_eq!(a.median_lower.candidate_total(), 110);
    assert_eq!(a.median_upper.candidate_total(), 120);
    assert_eq!(
        analyze_pairs(&samples, 4000).unwrap().decision,
        Decision::Pass
    );
    assert_eq!(
        analyze_pairs(&samples, -1001).unwrap().decision,
        Decision::Regression
    );
    assert_eq!(
        analyze_pairs(&samples, -1000).unwrap().decision,
        Decision::Inconclusive
    );
}
#[test]
fn empty_invalid_and_insufficient_evidence_never_pass_as_zero() {
    assert_eq!(analyze_pairs(&[], 0), Err(Error::EmptyEvidence));
    for (a, b, n) in [(0, 1, 1), (1, 0, 1), (1, 1, 0)] {
        assert_eq!(PairedSample::new(a, b, n), Err(Error::NonPositiveEvidence));
    }
    for n in 1..6 {
        let a = analyze_pairs(&vec![pair(100, 1); n], 10000).unwrap();
        assert_eq!(a.decision, Decision::Inconclusive);
        assert!(a.interval.is_none());
    }
    assert_eq!(
        analyze_pairs(&[pair(1, 1)], -10000),
        Err(Error::InvalidThreshold)
    );
    assert_eq!(
        analyze_pairs(&[pair(1, 1); 65], 0),
        Err(Error::Limit("epochs"))
    );
}
#[test]
fn gates_use_exact_ratios_instead_of_truncated_basis_points() {
    let third = pair(3, 4);
    assert_eq!(
        third.regression_bp_bounds(),
        BasisPointBounds {
            floor: 3333,
            ceil: 3334
        }
    );
    assert_eq!(
        analyze_pairs(&[third; 6], 3333).unwrap().decision,
        Decision::Regression
    );
    assert_eq!(
        analyze_pairs(&[third; 6], 3334).unwrap().decision,
        Decision::Pass
    );
    let negative = pair(3, 2);
    assert_eq!(
        negative.regression_bp_bounds(),
        BasisPointBounds {
            floor: -3334,
            ceil: -3333
        }
    );
    assert_eq!(
        analyze_pairs(&[negative; 6], -3334).unwrap().decision,
        Decision::Regression
    );
    assert_eq!(
        analyze_pairs(&[negative; 6], -3333).unwrap().decision,
        Decision::Pass
    );
}
#[test]
fn maximal_evidence_ordering_scaling_and_thresholds_are_overflow_safe() {
    let same = pair(u64::MAX, u64::MAX);
    assert_eq!(
        same.regression_bp_bounds(),
        BasisPointBounds { floor: 0, ceil: 0 }
    );
    assert_eq!(
        analyze_pairs(&[same; 64], 0).unwrap().decision,
        Decision::Pass
    );
    let large = pair(1, u64::MAX);
    assert_eq!(
        large.regression_bp_bounds().floor,
        (i128::from(u64::MAX) - 1) * 10000
    );
    assert_eq!(
        analyze_pairs(&[large; 6], i64::MAX).unwrap().decision,
        Decision::Regression
    );
    let tiny = pair(u64::MAX, 1);
    assert_eq!(
        tiny.regression_bp_bounds(),
        BasisPointBounds {
            floor: -10000,
            ceil: -9999
        }
    );
    assert_eq!(
        analyze_pairs(&[tiny; 6], -9999).unwrap().decision,
        Decision::Pass
    );
    assert_eq!(
        analyze_pairs(&[pair(u64::MAX - 1, u64::MAX); 6], 0)
            .unwrap()
            .decision,
        Decision::Regression
    );
    let mut reverse = vec![
        pair(u64::MAX - 1, u64::MAX),
        pair(u64::MAX, u64::MAX - 1),
        same,
        large,
        tiny,
        same,
    ];
    let a = analyze_pairs(&reverse, 0).unwrap();
    reverse.reverse();
    assert_eq!(a, analyze_pairs(&reverse, 0).unwrap());
}
#[test]
fn width_specific_counter_model_has_explicit_wrap_and_range_limits() {
    for width in 1..=64 {
        let max = if width == 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        assert_eq!(modular_counter_delta(max, 0, width).unwrap(), 1);
        assert_eq!(modular_counter_delta(0, max, width).unwrap(), max);
    }
    assert_eq!(modular_counter_delta(65530, 4, 16).unwrap(), 10);
    assert_eq!(modular_counter_delta(u64::MAX, 3, 64).unwrap(), 4);
    assert_eq!(
        modular_counter_delta(0, 0, 0),
        Err(Error::InvalidCounterWidth)
    );
    assert_eq!(
        modular_counter_delta(0, 0, 65),
        Err(Error::InvalidCounterWidth)
    );
    assert_eq!(
        modular_counter_delta(256, 0, 8),
        Err(Error::CounterValueOutOfRange)
    );
    assert_eq!(
        modular_counter_delta(0, 256, 8),
        Err(Error::CounterValueOutOfRange)
    );
    // Endpoints cannot distinguish no progress from a full modulus of events.
    assert_eq!(modular_counter_delta(7, 7, 8).unwrap(), 0);
}
#[test]
fn deterministic_offsets_only_and_unavailable_metrics_never_pass() {
    assert_eq!(layout_offset_bytes(0, 0), 0);
    assert_eq!(layout_offset_bytes(0, 1), 3008);
    assert_eq!(layout_offset_bytes(19, 3), 1856);
    let first: Vec<_> = (0..1000).map(|i| layout_offset_bytes(19, i)).collect();
    assert_eq!(
        first,
        (0..1000)
            .map(|i| layout_offset_bytes(19, i))
            .collect::<Vec<_>>()
    );
    assert!(first.iter().all(|n| *n < 4096 && *n % 64 == 0));
    for requirement in [
        EvidenceRequirement::HardwareCycles,
        EvidenceRequirement::RetiredInstructions,
        EvidenceRequirement::L1DataMisses,
        EvidenceRequirement::LlcMisses,
        EvidenceRequirement::BranchMispredicts,
        EvidenceRequirement::SimdVectorOperations,
        EvidenceRequirement::TmaBackendSlots,
        EvidenceRequirement::ZeroAllocation,
        EvidenceRequirement::Vectorization,
    ] {
        let gate = evaluate_evidence_requirement(requirement);
        assert_eq!(gate.decision, Decision::Inconclusive);
        assert_eq!(gate.unavailable.requirement, requirement);
    }
}
