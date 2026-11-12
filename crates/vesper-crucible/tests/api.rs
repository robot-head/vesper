//! Tape format, draw semantics and `refute`.

use vesper_crucible::{
    Arbitrary, ChoiceTape, FuzzConfig, Invalid, MAX_TAPE_LEN, REFUTED_KIND, Refutation, ShrinkKey,
    Source, TAPE_HEADER, TapeError, refute,
};

#[test]
fn tape_text_round_trips_and_rejects_malformed_input() {
    let tape = ChoiceTape::new(vec![0, 1, u64::MAX, 0xdead_beef]);
    let text = tape.to_text();
    assert!(text.starts_with(TAPE_HEADER));
    assert_eq!(ChoiceTape::from_text(&text).unwrap(), tape);
    assert_eq!(
        ChoiceTape::from_text(&format!("\n{TAPE_HEADER}\n\n  ff  \n")).unwrap(),
        ChoiceTape::new(vec![255])
    );
    assert_eq!(ChoiceTape::from_text(""), Err(TapeError::MissingHeader));
    assert!(matches!(
        ChoiceTape::from_text("vesper-crucible-tape/2\n1\n"),
        Err(TapeError::UnknownHeader(_))
    ));
    for bad in ["+1", "-1", "0x1", "FF", "1 2", "10000000000000000", "g"] {
        assert_eq!(
            ChoiceTape::from_text(&format!("{TAPE_HEADER}\n0\n{bad}\n")),
            Err(TapeError::BadValue { index: 1 }),
            "{bad:?}"
        );
    }
    let long = format!("{TAPE_HEADER}\n{}", "0\n".repeat(MAX_TAPE_LEN + 1));
    assert_eq!(
        ChoiceTape::from_text(&long),
        Err(TapeError::TooLong {
            limit: MAX_TAPE_LEN
        })
    );
}

#[test]
fn shrink_key_is_length_then_sum() {
    let k = |v: &[u64]| ShrinkKey::of(v);
    assert!(k(&[u64::MAX]).smaller_than(k(&[0, 0])));
    assert!(k(&[0, 1]).smaller_than(k(&[1, 1])));
    assert!(!k(&[1, 0]).smaller_than(k(&[0, 1])));
    assert!(!k(&[2]).smaller_than(k(&[2])));
    // No overflow at the maximum sum per entry.
    assert!(k(&[u64::MAX, u64::MAX - 1]).smaller_than(k(&[u64::MAX, u64::MAX])));
}

#[test]
fn draws_clamp_replayed_values_and_record_the_realized_tape() {
    let mut src = Source::replay(&ChoiceTape::new(vec![99, 7, 300, 5, 2]));
    assert_eq!(src.draw_u64_bounded(10), Ok(10));
    assert_eq!(src.draw_range_i64(-3, 3), Ok(3));
    assert_eq!(src.draw_u8(), Ok(255));
    assert_eq!(src.draw_index(3), Ok(2));
    assert_eq!(src.draw_enum(&['a', 'b', 'c']), Ok('c'));
    assert_eq!(src.draw_u8(), Err(Invalid::Overrun));
    assert_eq!(src.realized().values, vec![10, 6, 255, 2, 2]);
    let mut src = Source::replay(&ChoiceTape::default());
    assert_eq!(src.draw_index(0), Err(Invalid::EmptyChoice));
    assert_eq!(src.draw_range_u64(5, 4), Err(Invalid::EmptyRange));
    assert_eq!(src.assume(false), Err(Invalid::Rejected));
}

#[test]
fn full_ranges_do_not_overflow() {
    let mut src = Source::replay(&ChoiceTape::new(vec![u64::MAX, u64::MAX, 0]));
    assert_eq!(src.draw_range_i64(i64::MIN, i64::MAX), Ok(i64::MAX));
    assert_eq!(src.draw_range_u64(0, u64::MAX), Ok(u64::MAX));
    assert_eq!(src.draw_range_i64(i64::MIN, i64::MAX), Ok(i64::MIN));
}

#[test]
fn random_generation_replays_from_its_realized_tape() {
    for seed in 0..50 {
        let mut src = Source::random(seed);
        let value = <(Vec<u16>, Option<i64>, bool)>::arbitrary(&mut src).unwrap();
        let bytes = src.draw_bytes(40).unwrap();
        let tape = src.realized();
        let mut again = Source::replay(&tape);
        assert_eq!(
            <(Vec<u16>, Option<i64>, bool)>::arbitrary(&mut again).unwrap(),
            value
        );
        assert_eq!(again.draw_bytes(40).unwrap(), bytes);
        assert_eq!(again.realized(), tape);
    }
}

#[test]
fn zero_tape_decodes_to_the_simplest_values() {
    let mut src = Source::replay(&ChoiceTape::new(vec![0; 8]));
    assert_eq!(i64::arbitrary(&mut src), Ok(0));
    assert_eq!(<Vec<u8>>::arbitrary(&mut src), Ok(vec![]));
    assert_eq!(<Option<u32>>::arbitrary(&mut src), Ok(None));
    let mut src = Source::replay(&ChoiceTape::new(vec![1, 2, 3, 4]));
    let zigzag: Vec<i64> = (0..4).map(|_| i64::arbitrary(&mut src).unwrap()).collect();
    assert_eq!(zigzag, vec![-1, 1, -2, 2]);
}

#[test]
fn refute_returns_minimal_counterexample_to_overflowing_midpoint() {
    // Claimed invariant: the wrapping midpoint of two bytes is at least the
    // smaller one. False when the sum overflows.
    let outcome = refute(
        &FuzzConfig::default(),
        |src| Ok((src.draw_u8()?, src.draw_u8()?)),
        |&(a, b), _| a.wrapping_add(b) / 2 >= a.min(b),
    );
    let Refutation::Refuted {
        input: (a, b),
        counterexample,
    } = outcome
    else {
        panic!("expected a refutation, got {outcome:?}");
    };
    assert_eq!(counterexample.failure.kind, REFUTED_KIND);
    // Locally minimal in (len, sum): lowering either byte makes it pass.
    assert_eq!(u16::from(a) + u16::from(b), 256, "({a}, {b})");
    assert_eq!(counterexample.tape.values, vec![u64::from(a), u64::from(b)]);
}

#[test]
fn refute_uses_probe_comparisons_to_pierce_magic_values() {
    const SECRET: u64 = 0x0bad_cafe_f00d_1234;
    let outcome = refute(
        &FuzzConfig {
            max_execs: 5_000,
            ..FuzzConfig::default()
        },
        |src| src.draw_u64(),
        |&x, probe| !probe.eq(1, x, SECRET),
    );
    match outcome {
        Refutation::Refuted { input, .. } => assert_eq!(input, SECRET),
        other => panic!("{other:?}"),
    }
}

#[test]
fn refute_survives_true_predicates_with_witnesses() {
    let outcome = refute(
        &FuzzConfig {
            max_execs: 2_000,
            ..FuzzConfig::default()
        },
        |src| src.draw_u8(),
        |&x, probe| {
            if probe.cmp(1, vesper_crucible::CmpKind::Lt, u64::from(x), 128) {
                probe.edge(1);
            }
            u16::from(x) * 2 / 2 == u16::from(x)
        },
    );
    match outcome {
        Refutation::Survived { execs, witnesses } => {
            assert_eq!(execs, 2_000);
            assert!(!witnesses.is_empty());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn refute_reports_a_panicking_predicate_as_crashed_not_refuted() {
    let outcome = refute(
        &FuzzConfig {
            max_execs: 100,
            ..FuzzConfig::default()
        },
        |_src| Ok(42u8),
        |_x, _probe| panic!("predicate crashed"),
    );
    match outcome {
        Refutation::Crashed { counterexample } => {
            assert_eq!(counterexample.failure.kind, vesper_crucible::PANIC_KIND)
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn refute_rechecks_the_predicate_on_the_regenerated_input() {
    // A predicate that is false only on its first call: the search sees a
    // refutation, but the independent re-check does not reproduce it.
    let calls = std::cell::Cell::new(0u32);
    let outcome = refute(
        &FuzzConfig {
            max_execs: 100,
            ..FuzzConfig::default()
        },
        |src| src.draw_u8(),
        |_x, _probe| {
            calls.set(calls.get() + 1);
            calls.get() > 1
        },
    );
    assert!(
        matches!(outcome, Refutation::Unreproducible { .. }),
        "{outcome:?}"
    );
}

#[test]
fn strict_replay_is_not_limited_by_the_default_draw_budget() {
    let n = vesper_crucible::DEFAULT_MAX_DRAWS + 1;
    let mut prop = |src: &mut Source| {
        for _ in 0..n {
            src.draw_bool()?;
        }
        Err(vesper_crucible::PropError::fail("bug", "after many draws"))
    };
    let verdict = vesper_crucible::replay(&mut prop, &ChoiceTape::new(vec![0; n]));
    assert!(
        matches!(verdict, vesper_crucible::Verdict::Fail(ref f) if f.kind == "bug"),
        "{verdict:?}"
    );
}

#[test]
fn solver_respects_its_candidate_limit_on_long_tapes() {
    let report = vesper_crucible::fuzz(
        &FuzzConfig {
            max_execs: 200,
            initial_random: 1,
            max_solver_candidates: 4,
            ..FuzzConfig::default()
        },
        |src: &mut Source| {
            let mut ones = 0u64;
            for _ in 0..16_384 {
                ones += u64::from(src.draw_bool()?);
            }
            src.probe().eq(1, 1, ones);
            Ok(())
        },
    );
    assert!(report.solver_runs <= 4 * report.corpus.len() as u64);
}
