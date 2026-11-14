//! Differential shadowing of the worked example (design §29.3): the legacy
//! Rust parser from the fixture crate (executed, not just analysed) against a
//! migrated replacement written over a checked `vesper_ffi` big-endian wire
//! view, the shape of `view_wire_prefix[KafkaBatchWireHeader]` in the design.
//! Planted bugs must be caught and reported with their input.

#[allow(dead_code)]
#[path = "fixtures/legacy_codec/src/parse.rs"]
mod parse;
#[allow(dead_code)]
#[path = "fixtures/legacy_codec/src/stats.rs"]
mod stats;

use parse::{HEADER_LEN, ParseError};
use stats::DecoderStats;
use vesper_ffi::isolation::{IsolationDomain, RegionId, Unavailable, UnavailableReason};
use vesper_ffi::{Declaration, Endian, Field, Integer, Registry, WireLayout, WireType, split_wire};
use vesper_migrate::shadow::{self, Isolation, Outcome, ShadowConfig, ShadowReport, SplitMix64};

/// Isolated runs fork; serialize the tests so no other test thread is
/// printing a panic (holding std's panic locks) at the instant of a fork.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

type Observed = (Result<i64, ParseError>, (u32, i32));

fn layout() -> WireLayout {
    let be = |bits, signed| {
        WireType::Integer(if signed {
            Integer::signed(bits, Endian::Big)
        } else {
            Integer::unsigned(bits, Endian::Big)
        })
    };
    Registry::new([Declaration::new(
        "BatchWireHeader",
        vec![
            Field::new("base_offset", be(64, true)),
            Field::new("batch_length", be(32, true)),
            Field::new("leader_epoch", be(32, true)),
            Field::new(
                "magic",
                WireType::Integer(Integer::unsigned(8, Endian::Big)),
            ),
            Field::new("crc", be(32, false)),
            Field::new("records_count", be(32, true)),
        ],
    )])
    .unwrap()
    .compile("BatchWireHeader")
    .unwrap()
}

fn legacy(raw: &[u8]) -> Observed {
    let mut st = DecoderStats::default();
    let r = parse::parse_batch_header(raw, &mut st);
    (r, (st.last_crc, st.record_count))
}

/// The migrated replacement: `inout state.{last_crc, record_count}` only.
fn migrated(l: &WireLayout, raw: &[u8], bug: Option<&str>) -> Observed {
    let mut last_crc = 0u32;
    let mut record_count = 0i32;
    let result = (|| {
        let min = if bug == Some("off-by-one") {
            HEADER_LEN + 1
        } else {
            HEADER_LEN
        };
        if raw.len() < min {
            return Err(ParseError::Short);
        }
        let (hdr, _) = split_wire(l, raw).map_err(|_| ParseError::Short)?;
        let magic_ok = match bug {
            Some("magic") => hdr.read_u64(&["magic"]).unwrap() >= 2,
            _ => hdr.read_u64(&["magic"]).unwrap() == 2,
        };
        if !magic_ok {
            return Err(ParseError::BadMagic);
        }
        last_crc = u32::try_from(hdr.read_u64(&["crc"]).unwrap()).unwrap();
        let count = hdr.read_i64(&["records_count"]).unwrap();
        record_count = if bug == Some("endian") {
            i32::from_le_bytes((count as i32).to_be_bytes())
        } else {
            count as i32
        };
        Ok(hdr.read_i64(&["base_offset"]).unwrap())
    })();
    (result, (last_crc, record_count))
}

fn boundary() -> Vec<Vec<u8>> {
    let mut valid = vec![0u8; HEADER_LEN];
    valid[16] = 2;
    vec![
        vec![],
        vec![0; HEADER_LEN - 1],
        vec![0; HEADER_LEN],
        valid.clone(),
        [valid, vec![0xff; 7]].concat(),
    ]
}

fn generator(rng: &mut SplitMix64) -> Vec<u8> {
    let mut prefix = vec![0u8; HEADER_LEN];
    prefix[16] = 2;
    rng.bytes(HEADER_LEN + 8, &prefix)
}

fn run(bug: Option<&'static str>, with_boundary: bool) -> ShadowReport<Vec<u8>, Observed> {
    let l = layout();
    let b = if with_boundary { boundary() } else { vec![] };
    shadow::run(
        ShadowConfig {
            seed: 0xC0DEC,
            cases: 20_000,
        },
        b,
        generator,
        |raw: &Vec<u8>| legacy(raw),
        |raw: &Vec<u8>| migrated(&l, raw, bug),
    )
}

#[test]
fn migrated_parser_agrees_with_legacy_on_generated_inputs() {
    let _serial = serial();
    assert_eq!(
        layout().size(),
        HEADER_LEN,
        "wire layout matches the legacy header length"
    );
    let report = run(None, true);
    assert!(report.agreed(), "{report}");
    assert_eq!(report.cases_run, 20_005);
    assert!(matches!(report.isolation, Isolation::Unavailable { .. }));
    let text = report.to_string();
    assert!(
        text.contains("not an equivalence proof") && text.contains("isolation unavailable"),
        "{text}"
    );
}

#[test]
fn planted_divergences_are_caught_with_their_input() {
    let _serial = serial();
    for bug in ["off-by-one", "magic", "endian"] {
        let report = run(Some(bug), false);
        let d = report
            .divergence
            .as_ref()
            .unwrap_or_else(|| panic!("planted `{bug}` bug not caught"));
        assert_ne!(d.legacy, d.replacement);
        // The reported input reproduces the divergence.
        let l = layout();
        assert_ne!(
            legacy(&d.input),
            migrated(&l, &d.input, Some(bug)),
            "{bug}: {report}"
        );
        // Shrinking keeps it diverging and makes it no larger.
        let small = shadow::shrink_bytes(&d.input, |x| legacy(x) != migrated(&l, x, Some(bug)));
        assert!(small.len() <= d.input.len());
        assert_ne!(legacy(&small), migrated(&l, &small, Some(bug)));
        if bug == "off-by-one" {
            assert_eq!(
                small.len(),
                HEADER_LEN,
                "minimal witness is exactly one header"
            );
        }
    }
    // Boundary cases find the off-by-one immediately.
    let report = run(Some("off-by-one"), true);
    assert_eq!(report.divergence.unwrap().case, 2);
}

#[test]
fn panics_are_outcomes_not_harness_failures() {
    let _serial = serial();
    let report = shadow::run(
        ShadowConfig {
            seed: 1,
            cases: 100,
        },
        [3u8],
        |r| (r.next_u64() % 8) as u8,
        |x: &u8| u32::from(*x) * 2,
        |x: &u8| {
            if *x == 3 {
                panic!("replacement bug at {x}")
            } else {
                u32::from(*x) * 2
            }
        },
    );
    let d = report.divergence.unwrap();
    assert_eq!(d.input, 3);
    assert_eq!(d.legacy, Outcome::Returned(6));
    assert!(matches!(&d.replacement, Outcome::Panicked(m) if m.contains("replacement bug")));
}

#[test]
fn runs_are_reproducible_from_the_seed() {
    let _serial = serial();
    let a = run(Some("endian"), false);
    let b = run(Some("endian"), false);
    assert_eq!(a.divergence, b.divergence);
    let mut r1 = SplitMix64::new(7);
    let mut r2 = SplitMix64::new(7);
    assert_eq!(
        (0..64).map(|_| r1.next_u64()).collect::<Vec<_>>(),
        (0..64).map(|_| r2.next_u64()).collect::<Vec<_>>()
    );
}

/// The isolated harness: the migrated side stores its outputs in a
/// Vesper-owned buffer inside the isolation domain; the legacy side runs with
/// that buffer write-protected.
fn isolated(
    domain: Result<&IsolationDomain, &Unavailable>,
    out: Option<RegionId>,
    bug: Option<&'static str>,
    stray_write: bool,
) -> ShadowReport<Vec<u8>, Observed> {
    let l = layout();
    shadow::run_isolated(
        ShadowConfig {
            seed: 0xC0DEC,
            cases: 4_000,
        },
        boundary(),
        generator,
        |raw: &Vec<u8>| {
            if stray_write && raw.len() == HEADER_LEN + 3 {
                // A legacy bug writing past its own buffer into Vesper memory.
                if let (Ok(d), Some(r)) = (domain, out) {
                    d.write(r, 0, &[0xEE]).unwrap();
                }
            }
            legacy(raw)
        },
        |raw: &Vec<u8>| {
            let o = migrated(&l, raw, bug);
            if let (Ok(d), Some(r)) = (domain, out) {
                let (_, (crc, count)) = o;
                d.write(r, 0, &crc.to_le_bytes()).unwrap();
                d.write(r, 4, &count.to_le_bytes()).unwrap();
            }
            o
        },
        domain,
        std::time::Duration::from_secs(120),
    )
}

fn domain() -> Option<(IsolationDomain, RegionId)> {
    match IsolationDomain::new() {
        Ok(mut d) => {
            let r = d.add_region(64).unwrap();
            note(&format!("shadow isolation exercised: {:?}", d.mechanism()));
            Some((d, r))
        }
        Err(u) => {
            note(&format!("shadow isolation unavailable on this host: {u}"));
            None
        }
    }
}

#[test]
fn isolated_shadow_agrees_and_reports_the_hardware_mechanism() {
    let _serial = serial();
    let Some((d, r)) = domain() else { return };
    let report = isolated(Ok(&d), Some(r), None, false);
    assert!(report.agreed(), "{report}");
    assert_eq!(report.cases_run, 4_005);
    assert_eq!(
        report.isolation,
        Isolation::Hardware {
            mechanism: d.mechanism()
        }
    );
    assert!(report.to_string().contains("write-protected"), "{report}");
}

#[test]
fn a_legacy_write_into_vesper_memory_is_a_typed_shadow_failure() {
    let _serial = serial();
    let Some((d, r)) = domain() else { return };
    let report = isolated(Ok(&d), Some(r), None, true);
    assert!(!report.agreed());
    assert!(report.divergence.is_none());
    let v = report
        .isolation_failure
        .as_ref()
        .expect("violation reported");
    assert!(v.is_protection_violation(), "{report}");
    let input = v.input.as_ref().unwrap();
    assert_eq!(
        input.len(),
        HEADER_LEN + 3,
        "the regenerated input is the trigger"
    );
    assert_eq!(report.cases_run, v.case.unwrap() + 1);
    // The fault happened in the child; this process keeps its rights.
    d.write(r, 0, &[1]).unwrap();
}

#[test]
fn isolated_divergences_are_recovered_with_their_outcomes() {
    let _serial = serial();
    let Some((d, r)) = domain() else { return };
    let report = isolated(Ok(&d), Some(r), Some("endian"), false);
    let div = report.divergence.as_ref().expect("planted bug caught");
    assert_ne!(div.legacy, div.replacement);
    assert!(report.isolation_failure.is_none());
    // Same first divergence as the in-process harness over the same stream.
    let plain = shadow::run(
        ShadowConfig {
            seed: 0xC0DEC,
            cases: 4_000,
        },
        boundary(),
        generator,
        |raw: &Vec<u8>| legacy(raw),
        |raw: &Vec<u8>| migrated(&layout(), raw, Some("endian")),
    );
    assert_eq!(plain.divergence.unwrap().case, div.case);
}

#[test]
fn without_pku_or_poe_the_isolated_harness_says_so() {
    let _serial = serial();
    let u = Unavailable {
        reason: UnavailableReason::CpuLacksFeature,
        detail: "test host".into(),
    };
    let report = isolated(Err(&u), None, None, false);
    assert!(report.agreed(), "{report}");
    assert!(
        matches!(report.isolation, Isolation::Unavailable { reason } if reason.contains("lacks PKU/POE"))
    );
    assert!(report.to_string().contains("isolation unavailable"));
}

/// Written straight to the stderr handle, which the test harness does not
/// capture, so CI logs record whether hardware isolation was exercised.
fn note(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{line}");
}
