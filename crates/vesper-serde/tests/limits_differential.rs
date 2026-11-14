//! Differential fuzzing of every engine under tight, simultaneous limits.
//!
//! The scalar recursive-descent parser defines the expected result. Every
//! stage 1 path (and the reusable `SimdParser`) must accept exactly the same
//! documents with identical tapes, and reject every other one with the same
//! error kind **and** offset, whichever of the work, index-memory, token,
//! depth, string and syntax limits is reached first.
mod common;

use common::{Rng, document, mutate};
use std::collections::BTreeMap;
use vesper_serde::simd::{Isa, StructuralIndex};
use vesper_serde::{Error, IndexedJson, Limits, SimdParser};

fn entries(input: &[u8]) -> usize {
    let mut index = StructuralIndex::new();
    match index.build(input, Isa::Scalar, Default::default()) {
        Ok(()) => index.structurals().len() + index.escapes().len(),
        Err(_) => 0,
    }
}

/// Some duplicate keys, so duplicate-key work and errors are exercised:
/// the generator's keys are `"key{i}_{n}"`; respell a `key1_` key as the
/// object's `key0_` key (sometimes as a near miss differing in its last byte).
fn with_duplicates(rng: &mut Rng, doc: &mut Vec<u8>) {
    let text = String::from_utf8_lossy(doc).into_owned();
    let key_at = |from: usize, prefix: &str| {
        let start = from + text[from..].find(prefix)?;
        let end = start + 1 + text[start + 1..].find('"')?;
        Some((start, end + 1))
    };
    let Some((a, a_end)) = key_at(0, "\"key0_") else {
        return;
    };
    let Some((b, b_end)) = key_at(a_end, "\"key1_") else {
        return;
    };
    let mut key = text[a..a_end].to_owned();
    if rng.below(4) == 0 {
        key.insert(key.len() - 1, 'z');
    }
    *doc = format!("{}{}{}", &text[..b], key, &text[b_end..]).into_bytes();
}

fn limits_for(rng: &mut Rng, doc: &[u8]) -> Limits {
    let len = doc.len();
    let base = IndexedJson::parse(doc, Limits::default()).ok();
    let tape = base
        .as_ref()
        .map_or(48 * len.min(64), |d| d.index_capacity_bytes());
    let work = base.as_ref().map_or(3 * len, |d| d.work_used());
    let pick = |rng: &mut Rng, exact: usize, spread: usize| {
        // Around the exact requirement, so limits bind at varied positions.
        exact.saturating_sub(spread) + rng.below(2 * spread + 1)
    };
    let mut limits = Limits::default();
    for _ in 0..1 + rng.below(4) {
        match rng.below(7) {
            0 => limits.max_work = pick(rng, work, len.max(4)),
            1 => limits.max_index_bytes = pick(rng, tape + 4 * entries(doc), 64),
            2 => limits.max_tokens = rng.below(40),
            3 => limits.max_depth = rng.below(6),
            4 => limits.max_string_bytes = rng.below(80),
            5 => limits.max_total_string_bytes = rng.below(300),
            _ => limits.max_input_bytes = pick(rng, len, 4),
        }
    }
    limits
}

fn same_outcome(input: &[u8], limits: Limits) -> Result<(), Error> {
    let scalar = IndexedJson::parse(input, limits);
    for isa in Isa::available() {
        let simd = IndexedJson::parse_with_isa(input, limits, isa);
        match (&scalar, &simd) {
            (Ok(a), Ok(b)) => {
                assert!(a.same_tape(b), "{isa:?} tape on {:?}", show(input));
                assert_eq!(a.index_bytes(), b.index_bytes());
            }
            (Err(a), Err(b)) => {
                assert_eq!(a, b, "{isa:?} error on {:?} with {limits:?}", show(input))
            }
            _ => panic!(
                "{isa:?} acceptance differs on {:?} with {limits:?}: {:?} vs {:?}",
                show(input),
                scalar.as_ref().map(|_| ()),
                simd.as_ref().map(|_| ())
            ),
        }
        let mut parser = SimdParser::with_isa(isa).unwrap();
        for _ in 0..2 {
            let reused = parser.parse(input, limits, |doc| doc.work_used());
            assert_eq!(
                reused.as_ref().err(),
                scalar.as_ref().err(),
                "{isa:?} reused parser on {:?}",
                show(input)
            );
        }
    }
    scalar.map(|_| ())
}

fn show(input: &[u8]) -> String {
    String::from_utf8_lossy(input).into_owned()
}

#[test]
fn every_engine_reports_the_first_limit_or_syntax_error_identically() {
    let mut rng = Rng(0x00c0_ffee);
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let cases = if cfg!(debug_assertions) {
        60_000
    } else {
        1_000_000
    };
    for n in 0..cases {
        let mut doc = document(&mut rng);
        if n % 2 == 0 {
            with_duplicates(&mut rng, &mut doc);
        }
        if n % 3 == 0 {
            mutate(&mut rng, &mut doc);
        }
        let limits = limits_for(&mut rng, &doc);
        let outcome = same_outcome(&doc, limits);
        let key = match outcome {
            Ok(()) => "accepted".to_owned(),
            Err(e) => format!("{:?}", e.kind),
        };
        *kinds.entry(key).or_default() += 1;
    }
    eprintln!("outcomes over {cases} cases: {kinds:?}");
    // The corpus must reach every limit, not just syntax errors.
    for kind in [
        "accepted",
        "WorkLimit",
        "AllocationLimit",
        "TokenLimit",
        "DepthLimit",
        "StringLimit",
        "DuplicateKey",
        "Syntax",
        "InputLimit",
    ] {
        assert!(
            kinds.get(kind).copied().unwrap_or(0) > 0,
            "{kind} never hit"
        );
    }
}

/// Hand-written cases at the tricky ties: work running out inside an escape
/// body, index entries running out at a surrogate pair's second backslash,
/// at a closing quote and at an operator.
#[test]
fn limit_ties_inside_strings_and_escapes() {
    let docs: [&[u8]; 6] = [
        br#"["ab\u00e9cd", "\ud83d\ude00x", {"k":"\n\t"}]"#,
        br#"{"a":1,"a":2}"#,
        br#"{"aaaa":1,"aaab":2,"aaaa":3}"#,
        br#"["\ud83d\n", "x\u12"]"#,
        b"[\"a\x01b\", 1]",
        br#"  [ 1 , "\\\"" , true ,null ]  "#,
    ];
    for doc in docs {
        let len = doc.len();
        for max_work in len..3 * len + 600 {
            let _ = same_outcome(
                doc,
                Limits {
                    max_work,
                    ..Limits::default()
                },
            );
        }
        for max_index_bytes in 0..48 * len + 4 * len + 8 {
            let _ = same_outcome(
                doc,
                Limits {
                    max_index_bytes,
                    ..Limits::default()
                },
            );
        }
        // Token and index-entry limits running out at the same value.
        for max_tokens in 0..12 {
            for max_index_bytes in (0..48 * 12 + 4 * len + 8).step_by(4) {
                let _ = same_outcome(
                    doc,
                    Limits {
                        max_tokens,
                        max_index_bytes,
                        ..Limits::default()
                    },
                );
            }
        }
        for max_string_bytes in 0..12 {
            for max_work in [len, len + 7, 2 * len, 2 * len + 13] {
                let _ = same_outcome(
                    doc,
                    Limits {
                        max_string_bytes,
                        max_work,
                        ..Limits::default()
                    },
                );
            }
        }
    }
}
