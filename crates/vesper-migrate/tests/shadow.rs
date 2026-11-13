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
use vesper_ffi::{Declaration, Endian, Field, Integer, Registry, WireLayout, WireType, split_wire};
use vesper_migrate::shadow::{self, Isolation, Outcome, ShadowConfig, ShadowReport, SplitMix64};

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
