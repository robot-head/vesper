//! Fixed-seed parity of the two builds (port P1 lane 18, #915): the Vesper
//! port and the Rust seed copy must produce the same generator outputs, the
//! same tapes, the same counterexample and the same shrink sequence. The
//! expected values were recorded from the Rust build; both packages run
//! this file, so a port that drifts from Rust fails it.

mod common;

use common::{build, cells_key, random_program, random_program_ext, rpc};
use vesper_crucible::{
    ChoiceTape, FuzzConfig, PropError, ShrinkConfig, ShrinkKey, Source, SplitMix64, fuzz, shrink,
};
use vesper_crucible::{ExploreConfig, Search, explore_cached, explore_dpor, explore_exhaustive};

/// FNV-1a over the words, as a compact fingerprint of a long sequence.
fn fingerprint(words: impl IntoIterator<Item = u64>) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for w in words {
        for b in w.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

#[test]
fn splitmix_outputs_match_the_rust_build() {
    let mut rng = SplitMix64(42);
    let raw: Vec<u64> = (0..4).map(|_| rng.next_u64()).collect();
    let below: Vec<u64> = (0..4).map(|i| rng.below_inclusive(1000 + i)).collect();
    let chance: Vec<bool> = (0..8).map(|_| rng.chance(256)).collect();
    let full = rng.below_inclusive(u64::MAX);
    let index = rng.index(7);
    assert_eq!(
        (raw, below, chance, full, index, rng.0),
        golden_rng(),
        "SplitMix64 outputs"
    );
}

#[test]
fn random_and_replayed_sources_match_the_rust_build() {
    let mut src = Source::random(7);
    let mut seen = Vec::new();
    for i in 0..64u64 {
        seen.push(src.draw_u64_bounded(i * 37).unwrap());
    }
    let bytes = src.draw_bytes(40).unwrap();
    let run = src.draw_run_now().unwrap();
    let v = src.draw_vec(16, |s| s.draw_range_i64(-5, 5)).unwrap();
    assert_eq!(
        (
            fingerprint(seen),
            bytes.len(),
            run,
            v.len(),
            fingerprint(src.realized_values().iter().copied())
        ),
        GOLDEN_SOURCE,
        "random source"
    );

    // Replay clamps to the bound and overruns at the end of the tape.
    let mut replay = Source::replay(&ChoiceTape::new(vec![500, 3, u64::MAX]));
    assert_eq!(replay.draw_u64_bounded(10), Ok(10));
    assert_eq!(replay.draw_u8(), Ok(3));
    assert_eq!(replay.draw_bool(), Ok(true));
    assert!(replay.draw_u64().is_err());
    assert_eq!(replay.realized_values(), &[10, 3, 1]);
}

#[test]
fn replayed_byte_strings_stop_clamp_and_overrun_like_the_rust_build() {
    // Flags clamp to 1 and bytes to 255; a 0 flag ends the string.
    let mut src = Source::replay(&ChoiceTape::new(vec![1, 7, 5, 300, 0, 4]));
    assert_eq!(src.draw_bytes(10), Ok(vec![7, 255]));
    assert_eq!(src.realized_values(), &[1, 7, 1, 255, 0]);
    assert_eq!(src.draw_u8(), Ok(4));

    // At `max_len` bytes no further flag is drawn.
    let mut src = Source::replay(&ChoiceTape::new(vec![1, 2, 1, 3, 1, 4]));
    assert_eq!(src.draw_bytes(2), Ok(vec![2, 3]));
    assert_eq!(src.realized_values(), &[1, 2, 1, 3]);
    assert_eq!(src.draw_bytes(0), Ok(vec![]));
    assert_eq!(src.draws(), 4);

    // A tape that ends inside the string overruns; the draws before the
    // overrun stay realized.
    let mut src = Source::replay(&ChoiceTape::new(vec![1, 9, 1]));
    assert!(src.draw_bytes(10).is_err());
    assert_eq!(src.realized_values(), &[1, 9, 1]);
}

#[test]
fn tape_text_and_keys_match_the_rust_build() {
    let tape = ChoiceTape::new(vec![0, 1, 0xabc_def0, u64::MAX, 16]);
    assert_eq!(
        tape.to_text(),
        "vesper-crucible-tape/1\n0\n1\nabcdef0\nffffffffffffffff\n10\n"
    );
    // Unicode White_Space around values and the header is trimmed like
    // `str::trim`; a no-break space inside a value is not.
    let spaced = "\u{3000}vesper-crucible-tape/1\u{a0}\r\n\u{2009}ff\u{85}\n\n";
    assert_eq!(ChoiceTape::from_text(spaced).unwrap().values, vec![0xff]);
    assert!(ChoiceTape::from_text("vesper-crucible-tape/1\nf\u{a0}f\n").is_err());
    // Every byte outside 0-9 and a-f is an invalid digit, the ones between
    // `9` and `a` (`W` to the backtick) and `A-F` included.
    for c in (0u8..=127).filter(|c| !matches!(c, b'0'..=b'9' | b'a'..=b'f' | b'\n' | b'\r')) {
        let text = format!("vesper-crucible-tape/1\n1{}\n", c as char);
        if text.lines().nth(1).is_some_and(|l| l.trim() != "1") {
            assert!(ChoiceTape::from_text(&text).is_err(), "{text:?}");
        }
    }
    assert_eq!(
        ChoiceTape::from_text("vesper-crucible-tape/1\n09af\n")
            .unwrap()
            .values,
        vec![0x9af]
    );
    let long = format!("  \u{2028}{}\n", "é".repeat(80));
    match ChoiceTape::from_text(&long) {
        Err(vesper_crucible::TapeError::UnknownHeader(shown)) => {
            assert_eq!(shown, "é".repeat(64))
        }
        other => panic!("{other:?}"),
    }
    let big = ShrinkKey::of(&[u64::MAX, u64::MAX, 2]);
    assert_eq!(big.sum, 2 * u128::from(u64::MAX) + 2);
    assert!(ShrinkKey::of(&[5]).smaller_than(ShrinkKey::of(&[0, 0])));
    assert!(ShrinkKey::of(&[1, 2]).smaller_than(big));
    assert!(!big.smaller_than(big));
}

/// The README property: a 32-bit magic guard that the comparison solver
/// pierces, so the counterexample and its shrink are fixed by the seed.
fn magic(src: &mut Source) -> Result<(), PropError> {
    let data = src.draw_bytes(64)?;
    if data.len() >= 4 {
        let word = u32::from_le_bytes(data[..4].try_into().unwrap());
        if src.probe().eq(1, u64::from(word), 0xfeed_f00d) {
            return Err(PropError::fail("magic", "reached"));
        }
    }
    Ok(())
}

#[test]
fn fuzz_counterexample_matches_the_rust_build() {
    let report = fuzz(&FuzzConfig::default(), magic);
    let cx = report.counterexample.expect("solver pierces the guard");
    assert_eq!(
        (
            report.execs,
            cx.tape.values.clone(),
            cx.original.values.len(),
            cx.shrink.runs,
            cx.shrink.accepted
        ),
        golden_fuzz(),
        "fuzz report"
    );
}

#[test]
fn shrink_sequence_matches_the_rust_build() {
    // Every tape the shrinker runs, in order, is part of the contract.
    let mut runs: Vec<Vec<u64>> = Vec::new();
    let mut prop = |src: &mut Source| {
        let v = src.draw_vec(64, |s| s.draw_u64_bounded(100))?;
        let flag = src.draw_bool()?;
        runs.push(src.realized_values().to_vec());
        src.assume(v.len() != 3)?;
        if v.iter().sum::<u64>() > 300 && (flag || v.len() > 5) {
            Err(PropError::fail("sum", "too big"))
        } else if v.contains(&77) {
            Err(PropError::fail("other", "77"))
        } else {
            Ok(())
        }
    };
    let mut rng = SplitMix64(11);
    let mut tape = Vec::new();
    for _ in 0..24 {
        tape.push(1);
        tape.push(rng.below_inclusive(100));
    }
    tape.extend([0, 1]);
    let config = ShrinkConfig {
        max_runs: 400,
        ..ShrinkConfig::default()
    };
    let shrunk = shrink(&mut prop, &ChoiceTape::new(tape), &config).unwrap();
    let sequence = fingerprint(
        runs.iter()
            .flat_map(|r| r.iter().copied().chain([u64::MAX])),
    );
    let s = &shrunk.stats;
    assert_eq!(
        (
            runs.len(),
            sequence,
            shrunk.tape.values.clone(),
            (s.runs, s.accepted, s.invalid, s.not_reproducing),
            s.budget_exhausted
        ),
        golden_shrink(),
        "shrink sequence"
    );

    // A budget small enough to run out mid-pass stops at the same place.
    let mut count = 0u64;
    let mut prop = |src: &mut Source| {
        count += 1;
        let v = src.draw_vec(64, |s| s.draw_u64_bounded(1000))?;
        if v.iter().sum::<u64>() > 10 {
            Err(PropError::fail("sum", "big"))
        } else {
            Ok(())
        }
    };
    let tape: Vec<u64> = (0..30).flat_map(|i| [1, 900 - i]).chain([0]).collect();
    let config = ShrinkConfig {
        max_runs: 37,
        ..ShrinkConfig::default()
    };
    let shrunk = shrink(&mut prop, &ChoiceTape::new(tape), &config).unwrap();
    assert!(shrunk.stats.budget_exhausted);
    assert_eq!(shrunk.stats.runs, 37);
    assert_eq!(count, 37);
    assert_eq!(
        fingerprint(shrunk.tape.values.iter().copied()),
        GOLDEN_BUDGET,
        "budgeted shrink"
    );
}

type RngOutputs = (Vec<u64>, Vec<u64>, Vec<bool>, u64, usize, u64);
type ShrinkOutputs = (usize, u64, Vec<u64>, (u64, u64, u64, u64), bool);

fn golden_rng() -> RngOutputs {
    (
        vec![
            13679457532755275413,
            2949826092126892291,
            5139283748462763858,
            6349198060258255764,
        ],
        vec![475, 942, 762, 568],
        vec![false, false, false, true, false, false, false, false],
        1910607418205583989,
        4,
        2298681937012504996,
    )
}
const GOLDEN_SOURCE: (u64, usize, bool, usize, u64) =
    (5480631544679405514, 38, true, 2, 1695556231656988067);
fn golden_fuzz() -> (u64, Vec<u64>, usize, u64, u64) {
    (65, vec![1, 13, 1, 240, 1, 237, 1, 254, 0], 25, 304, 2)
}
fn golden_shrink() -> ShrinkOutputs {
    (
        328,
        1351420804930808777,
        vec![1, 27, 1, 60, 1, 71, 1, 89, 1, 54, 1, 0, 0, 0],
        (400, 8, 107, 284),
        true,
    )
}
const GOLDEN_BUDGET: u64 = 1411699998813427827;

/// Everything a campaign reports, with the corpus as a fingerprint.
fn campaign(config: &FuzzConfig, prop: impl FnMut(&mut Source) -> Result<(), PropError>) -> String {
    let r = fuzz(config, prop);
    let corpus = fingerprint(
        r.corpus
            .iter()
            .flat_map(|t| t.values.iter().copied().chain([u64::MAX])),
    );
    let cx = r.counterexample.map(|c| {
        (
            c.found_after_execs,
            fingerprint(c.original.values),
            c.tape.values,
            c.failure.kind,
        )
    });
    format!(
        "execs={} invalid={} corpus={}/{corpus:x} features={} solver={}/{} cx={cx:?}",
        r.execs,
        r.invalid_runs,
        r.corpus.len(),
        r.features,
        r.solver_runs,
        r.solver_hits
    )
}

/// A target with byte-string, ordering and nested integer guards, rejected
/// inputs, and no failure reachable within the budget unless the solver and
/// mutators find all three guards.
fn layered(src: &mut Source) -> Result<(), PropError> {
    let data = src.draw_bytes(48)?;
    let n = src.draw_u64_bounded(5000)?;
    src.assume(n != 13)?;
    let p = src.probe();
    p.edge(u32::try_from(data.len().min(9)).unwrap());
    if p.eq_bytes(1, &data[..data.len().min(4)], b"CRU1") {
        p.edge(100);
        if p.cmp(2, vesper_crucible::CmpKind::Gt, n, 4321) {
            p.edge(101);
            let tail = data
                .get(4..6)
                .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
            if p.eq(3, u64::from(tail), 0xbeef) {
                return Err(PropError::fail("layered", "all guards"));
            }
        }
    }
    Ok(())
}

/// No guard the solver can copy: coverage comes from mutation alone, a
/// quarter of the runs are rejected, and the budget runs out.
fn hashed(src: &mut Source) -> Result<(), PropError> {
    let data = src.draw_bytes(32)?;
    let n = src.draw_u64_bounded(1000)?;
    src.assume(n % 4 != 0)?;
    let h = fingerprint(data.iter().map(|&b| u64::from(b)).chain([n]));
    let p = src.probe();
    p.edge((h % 61) as u32);
    p.cmp(7, vesper_crucible::CmpKind::Lt, h % 1000, n);
    if h % 100_003 == 17 {
        return Err(PropError::fail("hash", "hit"));
    }
    Ok(())
}

#[test]
fn fuzz_campaigns_match_the_rust_build() {
    let guided = FuzzConfig {
        max_execs: 20_000,
        ..FuzzConfig::default()
    };
    let random = FuzzConfig {
        strategy: vesper_crucible::Strategy::Random,
        max_execs: 3_000,
        seed: 5,
        ..FuzzConfig::default()
    };
    let plain = FuzzConfig {
        value_profile: false,
        max_execs: 4_000,
        max_solver_candidates: 7,
        seed: 99,
        ..FuzzConfig::default()
    };
    let got = [
        campaign(&guided, layered),
        campaign(&random, layered),
        campaign(&plain, layered),
        campaign(&plain, magic),
        campaign(&guided_short(), hashed),
    ];
    assert_eq!(got, golden_campaigns(), "fuzz campaigns");
}

fn guided_short() -> FuzzConfig {
    FuzzConfig {
        max_execs: 3_000,
        seed: 21,
        ..FuzzConfig::default()
    }
}

fn golden_campaigns() -> [String; 5] {
    [
        "execs=80 invalid=0 corpus=12/e9770354c81ec860 features=19 solver=16/4 cx=Some((80, 9725649468902132467, [1, 67, 1, 82, 1, 85, 1, 49, 1, 190, 1, 239, 0, 4322], \"layered\"))",
        "execs=3000 invalid=0 corpus=0/cbf29ce484222325 features=0 solver=0/0 cx=None",
        "execs=79 invalid=0 corpus=11/7fae6aaabbadf1b9 features=16 solver=15/3 cx=Some((79, 18409858763161512801, [1, 67, 1, 82, 1, 85, 1, 49, 1, 190, 1, 239, 0, 4322], \"layered\"))",
        "execs=65 invalid=0 corpus=1/f4d72741092f0448 features=1 solver=1/1 cx=Some((65, 7814649910919464463, [1, 13, 1, 240, 1, 237, 1, 254, 0], \"magic\"))",
        "execs=3000 invalid=573 corpus=63/a7b140b0e63b5c3a features=64 solver=188/26 cx=None",
    ]
    .map(String::from)
}

/// Every systematic explorer on one program, as counters and a fingerprint
/// of every outcome with its schedule (or the error).
fn explorations(program: &[Vec<common::Op>], bound: Option<u32>) -> Vec<String> {
    let cfg = ExploreConfig {
        max_executions: 3_000,
        preemption_bound: bound,
        ..ExploreConfig::default()
    };
    let observe = |w: &common::Cells| Ok(w.clone());
    let runs = [
        explore_exhaustive(&cfg, || build(program), observe),
        explore_dpor(&cfg, || build(program), observe),
        explore_cached(
            Search::Exhaustive,
            &cfg,
            || build(program),
            observe,
            cells_key,
        ),
        explore_cached(Search::Dpor, &cfg, || build(program), observe, cells_key),
    ];
    runs.iter()
        .map(|run| match run {
            Ok(e) => format!(
                "{}/{}/{}/{}/{} outcomes={}/{:x}",
                e.executions,
                e.sleep_blocked,
                e.transitions,
                e.cache_hits,
                e.bound_pruned,
                e.outcomes.len(),
                fingerprint(format!("{:?}", e.outcomes).bytes().map(u64::from)),
            ),
            Err(err) => format!("{err:?}"),
        })
        .collect()
}

/// The explorers over a sweep of random programs (plain, and with spawns,
/// cancellation and chosen I/O results), unbounded and with a preemption
/// bound of one, and the RPC timeout race.
fn exploration_sweep() -> Vec<String> {
    let mut out = Vec::new();
    for seed in 0..12u64 {
        let fibers = 2 + (seed % 2) as usize;
        for program in [
            random_program(seed, fibers, 3),
            random_program_ext(seed, fibers, 3),
        ] {
            for bound in [None, Some(1)] {
                out.push(explorations(&program, bound).join(" | "));
            }
        }
    }
    let cfg = ExploreConfig::default();
    for checks in [false, true] {
        let observe = |w: &common::Rpc| common::applied_once(w).map(|()| w.clone());
        let ex = explore_exhaustive(&cfg, || rpc(checks), observe).unwrap();
        let dp = explore_dpor(&cfg, || rpc(checks), observe).unwrap();
        out.push(format!(
            "rpc {} {} {:x}",
            ex.executions,
            dp.executions,
            fingerprint(
                format!("{:?}{:?}", ex.outcomes, dp.outcomes)
                    .bytes()
                    .map(u64::from)
            ),
        ));
    }
    out
}

#[test]
fn explorations_match_the_rust_build() {
    let sweep = exploration_sweep();
    for line in &sweep {
        println!("{line}");
    }
    let total = fingerprint(sweep.iter().flat_map(|l| l.bytes().map(u64::from)));
    assert_eq!(sweep.len(), 50);
    assert_eq!(
        total, 4_769_677_699_205_162_100,
        "exploration sweep fingerprint"
    );
}
