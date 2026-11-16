use vesper_bench::contract::{BenchContract, Decimal, parse_attribute};
use vesper_bench::contract_gate::{
    Counts, GateError, GateStatus as S, Overall, Unavailable as U, evaluate,
};
use vesper_bench::wire::{PmuField as F, PmuSample};

fn sample(instructions: u64, misses: u64) -> PmuSample {
    let mut sample = PmuSample::new();
    sample.set(F::InstructionsRetired, instructions).unwrap();
    sample.set(F::L1dReadMisses, misses).unwrap();
    sample
}

#[test]
fn documented_numerical_gates_compare_exact_ratios() {
    let contract =
        parse_attribute("@bench(max_instructions_per_byte=2.2, max_l1d_misses_per_op=4)").unwrap();
    let counts = Counts {
        input_bytes: Some(10),
        operations: Some(3),
    };
    let equal = evaluate(&contract, &sample(22, 12), counts).unwrap();
    assert_eq!(equal.instructions_per_byte, S::Pass);
    assert_eq!(equal.l1d_misses_per_op, S::Pass);
    assert_eq!(equal.overall, Overall::Pass);
    let above = evaluate(&contract, &sample(23, 13), counts).unwrap();
    assert_eq!(above.instructions_per_byte, S::Fail);
    assert_eq!(above.l1d_misses_per_op, S::Fail);
    assert_eq!(above.overall, Overall::Fail);
    let below = evaluate(&contract, &sample(21, 11), counts).unwrap();
    assert_eq!(below.overall, Overall::Pass);
}

#[test]
fn unknown_zero_and_unsupported_evidence_are_never_passes() {
    let contract = parse_attribute("@bench(zero_alloc=true, expect_vectorized=true, stabilizer_pages=16, max_instructions_per_byte=2.2, max_l1d_misses_per_op=4)").unwrap();
    let empty = evaluate(&contract, &PmuSample::new(), Counts::default()).unwrap();
    assert_eq!(
        empty.instructions_per_byte,
        S::Inconclusive(U::Counter(F::InstructionsRetired))
    );
    assert_eq!(
        empty.l1d_misses_per_op,
        S::Inconclusive(U::Counter(F::L1dReadMisses))
    );
    assert_eq!(empty.zero_alloc, S::Inconclusive(U::AllocationEvidence));
    assert_eq!(
        empty.expect_vectorized,
        S::Inconclusive(U::VectorizationEvidence)
    );
    assert_eq!(empty.stabilizer_pages, S::Inconclusive(U::LayoutEvidence));
    assert_eq!(empty.overall, Overall::Inconclusive);
    let missing = evaluate(&contract, &sample(1, 1), Counts::default()).unwrap();
    assert_eq!(
        missing.instructions_per_byte,
        S::Inconclusive(U::InputBytes)
    );
    assert_eq!(missing.l1d_misses_per_op, S::Inconclusive(U::Operations));
    let zero = evaluate(
        &contract,
        &sample(0, 0),
        Counts {
            input_bytes: Some(0),
            operations: Some(0),
        },
    )
    .unwrap();
    assert_eq!(
        zero.instructions_per_byte,
        S::Inconclusive(U::ZeroInputBytes)
    );
    assert_eq!(zero.l1d_misses_per_op, S::Inconclusive(U::ZeroOperations));
    assert_eq!(zero.overall, Overall::Inconclusive);
    let mixed = evaluate(
        &contract,
        &sample(23, 0),
        Counts {
            input_bytes: Some(10),
            operations: Some(1),
        },
    )
    .unwrap();
    assert_eq!(mixed.overall, Overall::Fail);
    let no_gates = parse_attribute("@bench(zero_alloc=false, expect_vectorized=false)").unwrap();
    assert_eq!(
        evaluate(&no_gates, &sample(0, 0), Counts::default())
            .unwrap()
            .overall,
        Overall::NotRequested
    );
}

#[test]
fn extreme_values_are_exact_and_invalid_programmatic_scale_fails() {
    let contract = BenchContract {
        max_instructions_per_byte: Some(Decimal {
            coefficient: 1,
            scale: 0,
        }),
        max_l1d_misses_per_op: Some(u64::MAX),
        ..BenchContract::default()
    };
    let counts = Counts {
        input_bytes: Some(u64::MAX),
        operations: Some(u64::MAX),
    };
    assert_eq!(
        evaluate(&contract, &sample(u64::MAX, u64::MAX), counts)
            .unwrap()
            .overall,
        Overall::Pass
    );
    let tiny = BenchContract {
        max_instructions_per_byte: Some(Decimal {
            coefficient: 1,
            scale: 18,
        }),
        ..BenchContract::default()
    };
    assert_eq!(
        evaluate(
            &tiny,
            &sample(1, 0),
            Counts {
                input_bytes: Some(1_000_000_000_000_000_000),
                operations: None
            }
        )
        .unwrap()
        .overall,
        Overall::Pass
    );
    assert_eq!(
        evaluate(
            &tiny,
            &sample(1, 0),
            Counts {
                input_bytes: Some(999_999_999_999_999_999),
                operations: None
            }
        )
        .unwrap()
        .overall,
        Overall::Fail
    );
    let invalid = BenchContract {
        max_instructions_per_byte: Some(Decimal {
            coefficient: 0,
            scale: 19,
        }),
        ..BenchContract::default()
    };
    assert_eq!(
        evaluate(&invalid, &PmuSample::new(), Counts::default()).unwrap_err(),
        GateError::InvalidDecimalScale
    );
}
