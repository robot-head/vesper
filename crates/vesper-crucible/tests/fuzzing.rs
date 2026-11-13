//! Acceptance: planted magic-number, length-prefix and checksum bugs that the
//! comparison-guided fuzzer finds and pure random search misses within the
//! same execution budget, then shrinks and replays deterministically.

use vesper_crucible::{
    ChoiceTape, FuzzConfig, FuzzReport, PropError, Source, Strategy, Verdict, fuzz, replay,
};

const BUDGET: u64 = 20_000;
const MAGIC: u64 = 0x2152_4550_5345_5621; // "!VESPER!" little-endian

fn magic_target(src: &mut Source) -> Result<(), PropError> {
    let data = src.draw_bytes(64)?;
    src.probe().edge(1);
    if data.len() >= 8 {
        src.probe().edge(2);
        let word = u64::from_le_bytes(data[..8].try_into().expect("8 bytes"));
        if src.probe().eq(10, word, MAGIC) {
            src.probe().edge(3);
            return Err(PropError::fail("magic", format!("{} bytes", data.len())));
        }
    }
    Ok(())
}

/// A 4-byte little-endian length prefix that must equal the body length, and
/// a crash on bodies of at least four bytes.
fn length_target(src: &mut Source) -> Result<(), PropError> {
    let data = src.draw_bytes(200)?;
    if data.len() >= 4 {
        src.probe().edge(1);
        let declared = u32::from_le_bytes(data[..4].try_into().expect("4 bytes"));
        let body = (data.len() - 4) as u64;
        if src.probe().eq(20, u64::from(declared), body) {
            src.probe().edge(2);
            if body >= 4 {
                return Err(PropError::fail("length", format!("body {body}")));
            }
        }
    }
    Ok(())
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// `"VC" | crc32(payload) LE | payload`: a frame whose command byte is `'!'`
/// and whose checksum verifies crashes. Reaching it takes three solved
/// comparisons in sequence (tag bytes, command byte, checksum).
fn checksum_target(src: &mut Source) -> Result<(), PropError> {
    let data = src.draw_bytes(96)?;
    if data.len() < 7 {
        return Ok(());
    }
    if !src.probe().eq_bytes(30, &data[..2], b"VC") {
        return Ok(());
    }
    src.probe().edge(1);
    let stored = u32::from_le_bytes(data[2..6].try_into().expect("4 bytes"));
    let payload = &data[6..];
    if !src.probe().eq(31, u64::from(payload[0]), u64::from(b'!')) {
        return Ok(());
    }
    src.probe().edge(2);
    if src
        .probe()
        .eq(32, u64::from(stored), u64::from(crc32(payload)))
    {
        src.probe().edge(3);
        return Err(PropError::fail("checksum", "valid '!' frame"));
    }
    Ok(())
}

fn config(strategy: Strategy) -> FuzzConfig {
    FuzzConfig {
        seed: 0x5eed_c0de,
        max_execs: BUDGET,
        strategy,
        ..FuzzConfig::default()
    }
}

fn campaign(
    name: &str,
    target: fn(&mut Source) -> Result<(), PropError>,
) -> (FuzzReport, FuzzReport) {
    let guided = fuzz(&config(Strategy::Guided), target);
    let random = fuzz(&config(Strategy::Random), target);
    eprintln!(
        "{name}: guided found={} after {:?} execs (solver runs {}, hits {}, corpus {}), \
         random found={} in {} execs",
        guided.counterexample.is_some(),
        guided.counterexample.as_ref().map(|c| c.found_after_execs),
        guided.solver_runs,
        guided.solver_hits,
        guided.corpus.len(),
        random.counterexample.is_some(),
        random.execs,
    );
    (guided, random)
}

fn assert_minimal_replay(
    target: fn(&mut Source) -> Result<(), PropError>,
    report: &FuzzReport,
    kind: &str,
) -> ChoiceTape {
    let cx = report
        .counterexample
        .as_ref()
        .expect("guided search finds the bug");
    assert_eq!(cx.failure.kind, kind);
    assert!(
        cx.shrink.shrunk.smaller_than(cx.shrink.initial) || cx.shrink.shrunk == cx.shrink.initial
    );
    // Deterministic replay of the shrunk tape, also through the text format.
    let mut t = target;
    let text = cx.tape.to_text();
    let reparsed = ChoiceTape::from_text(&text).expect("round trip");
    assert_eq!(reparsed, cx.tape);
    for _ in 0..3 {
        match replay(&mut t, &reparsed) {
            Verdict::Fail(f) => assert_eq!(f.kind, kind),
            other => panic!("shrunk tape does not replay: {other:?}"),
        }
    }
    cx.tape.clone()
}

#[test]
fn magic_number_found_by_solver_missed_by_random() {
    let (guided, random) = campaign("magic", magic_target);
    assert!(random.counterexample.is_none(), "random search should miss");
    assert_eq!(random.execs, BUDGET);
    let tape = assert_minimal_replay(magic_target, &guided, "magic");
    // Minimal: eight (flag=1, byte) pairs and the terminating flag.
    assert_eq!(tape.len(), 17, "shrunk tape {:?}", tape.values);
    let bytes: Vec<u8> = tape
        .values
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| c[1] as u8)
        .collect();
    assert_eq!(bytes, MAGIC.to_le_bytes());
}

#[test]
fn length_prefix_found_by_solver_missed_by_random() {
    let (guided, random) = campaign("length-prefix", length_target);
    assert!(random.counterexample.is_none(), "random search should miss");
    let tape = assert_minimal_replay(length_target, &guided, "length");
    // Minimal: a 4-byte prefix declaring 4, four zero body bytes.
    let bytes: Vec<u64> = tape
        .values
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| c[1])
        .collect();
    assert_eq!(bytes, vec![4, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn checksum_equality_found_by_solver_missed_by_random() {
    let (guided, random) = campaign("checksum", checksum_target);
    assert!(random.counterexample.is_none(), "random search should miss");
    let tape = assert_minimal_replay(checksum_target, &guided, "checksum");
    let bytes: Vec<u8> = tape
        .values
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| c[1] as u8)
        .collect();
    assert_eq!(&bytes[..2], b"VC");
    assert_eq!(bytes[6], b'!');
    assert_eq!(
        u32::from_le_bytes(bytes[2..6].try_into().unwrap()),
        crc32(&bytes[6..])
    );
}

#[test]
fn campaigns_are_deterministic() {
    let a = fuzz(&config(Strategy::Guided), magic_target);
    let b = fuzz(&config(Strategy::Guided), magic_target);
    assert_eq!(a, b);
}

#[test]
fn panics_are_failures_and_shrink() {
    let report = fuzz(&config(Strategy::Guided), |src: &mut Source| {
        let v = src.draw_u64_bounded(1000)?;
        assert!(v < 500, "too big");
        Ok(())
    });
    let cx = report.counterexample.expect("found");
    assert_eq!(cx.failure.kind, vesper_crucible::PANIC_KIND);
    assert_eq!(cx.tape.values, vec![500]);
}

#[test]
fn throughput_is_measured_not_claimed() {
    // Records the measured rate for the report; asserts nothing about speed.
    let start = std::time::Instant::now();
    let report = fuzz(
        &FuzzConfig {
            max_execs: 50_000,
            ..config(Strategy::Guided)
        },
        |src: &mut Source| {
            let data = src.draw_bytes(16)?;
            if data.first() == Some(&7) {
                src.probe().edge(1);
            }
            Ok(())
        },
    );
    let secs = start.elapsed().as_secs_f64();
    eprintln!(
        "throughput: {} execs in {secs:.3}s = {:.0} execs/s (debug/test profile, one thread)",
        report.execs,
        report.execs as f64 / secs
    );
    assert_eq!(report.execs, 50_000);
}
