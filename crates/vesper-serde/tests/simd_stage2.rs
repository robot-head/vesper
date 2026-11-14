//! Stage 2 differential tests: the SIMD-indexed tape builder must accept and
//! reject exactly what the scalar recursive-descent parser does, and build an
//! identical tape (tokens, subtree ends, decoded lengths, work) on success.
mod common;

use common::{Rng, document, mutate};
use vesper_serde::simd::Isa;
use vesper_serde::{ErrorKind, IndexedJson, JsonStr, Kind, Limits, SimdParser};

/// Compare the two engines on one input; returns whether error kinds agreed.
fn compare(input: &[u8], limits: Limits) -> bool {
    let scalar = IndexedJson::parse(input, limits);
    let mut kinds_agree = true;
    for isa in Isa::available() {
        let simd = IndexedJson::parse_with_isa(input, limits, isa);
        match (&scalar, &simd) {
            (Ok(a), Ok(b)) => {
                assert!(
                    a.same_tape(b),
                    "{isa:?} tape differs on {:?}",
                    String::from_utf8_lossy(input)
                );
                assert_eq!(a.index_capacity_bytes(), b.index_capacity_bytes());
            }
            (Err(a), Err(b)) => {
                if a != b && std::env::var_os("SHOW_KINDS").is_some() {
                    eprintln!("{a:?} vs {b:?} on {:?}", String::from_utf8_lossy(input));
                }
                kinds_agree &= a == b
            }
            _ => panic!(
                "{isa:?} disagrees on {:?}: scalar {:?}, simd {:?}",
                String::from_utf8_lossy(input),
                scalar.as_ref().map(|_| ()),
                simd.as_ref().map(|_| ())
            ),
        }
        let mut parser = SimdParser::with_isa(isa).unwrap();
        for _ in 0..2 {
            let reused = parser.parse(input, limits, |doc| {
                if let Ok(a) = &scalar {
                    assert!(a.same_tape(doc));
                }
            });
            assert_eq!(reused.is_ok(), scalar.is_ok());
        }
    }
    kinds_agree
}

#[test]
fn random_valid_documents_build_identical_tapes() {
    let mut rng = Rng(0xfeed_beef);
    for _ in 0..3000 {
        let doc = document(&mut rng);
        let parsed = IndexedJson::parse(&doc, Limits::default());
        assert!(
            parsed.is_ok(),
            "{:?}: {parsed:?}",
            String::from_utf8_lossy(&doc)
        );
        compare(&doc, Limits::default());
    }
}

#[test]
fn mutated_documents_are_accepted_and_rejected_identically() {
    let mut rng = Rng(0x5eed);
    let (mut total, mut agree) = (0, 0);
    for _ in 0..12_000 {
        let mut doc = document(&mut rng);
        mutate(&mut rng, &mut doc);
        total += 1;
        agree += usize::from(compare(&doc, Limits::default()));
    }
    // With default limits, every mutated input fails for the same reason.
    assert_eq!(
        agree, total,
        "error kinds agreed on {agree}/{total} mutated inputs"
    );
}

#[test]
fn tight_limits_are_enforced_identically() {
    let mut rng = Rng(77);
    let (mut total, mut agree) = (0, 0);
    for _ in 0..1500 {
        let doc = document(&mut rng);
        let limits = Limits {
            max_input_bytes: rng.below(doc.len() + 8),
            max_depth: rng.below(6),
            max_tokens: rng.below(40),
            max_string_bytes: rng.below(60),
            max_total_string_bytes: rng.below(200),
            max_work: rng.below(doc.len() * 3 + 400),
            max_index_bytes: rng.below(2000),
        };
        total += 2;
        agree += usize::from(compare(&doc, limits));
        agree += usize::from(compare(
            &doc,
            Limits {
                max_input_bytes: 1 << 20,
                ..limits
            },
        ));
    }
    // Several limits can fail at once; every engine reports the first one
    // the scalar parser meets (see also tests/limits_differential.rs).
    assert_eq!(
        agree, total,
        "tight limits: errors identical on {agree}/{total}"
    );
}

#[test]
fn deep_and_wide_inputs() {
    for depth in [1usize, 63, 64, 65, 127, 128, 129, 300] {
        let doc = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let limits = Limits {
            max_depth: 200,
            ..Limits::default()
        };
        compare(doc.as_bytes(), limits);
        compare(doc.as_bytes(), Limits::default());
        compare(&doc.as_bytes()[..depth], limits);
    }
    let mut wide = String::from("{");
    for i in 0..300 {
        if i > 0 {
            wide.push(',');
        }
        wide.push_str(&format!("\"k{i}\":{i}"));
    }
    wide.push('}');
    compare(wide.as_bytes(), Limits::default());
    let dup = wide.replace("\"k299\"", "\"k\\u0031\"");
    assert_eq!(
        IndexedJson::parse_simd(dup.as_bytes(), Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::DuplicateKey
    );
    compare(dup.as_bytes(), Limits::default());
}

#[test]
fn on_demand_reads_borrow_unescaped_strings_and_skip_subtrees() {
    let input =
        br#"{"skip":{"deep":[1,2,{"x":"\u0041"}]},"id":42,"topic":"plain","esc":"a\nb","f":2.5}"#;
    for isa in Isa::available() {
        let doc = IndexedJson::parse_with_isa(input, Limits::default(), isa).unwrap();
        let root = doc.root();
        assert_eq!(
            root.field("id", 1 << 20)
                .unwrap()
                .unwrap()
                .as_u64()
                .unwrap(),
            42
        );
        let topic = root
            .field("topic", 1 << 20)
            .unwrap()
            .unwrap()
            .string()
            .unwrap();
        assert!(!topic.is_escaped());
        let mut scratch = [0u8; 8];
        assert_eq!(
            topic.decode(&mut scratch).unwrap(),
            JsonStr::Borrowed("plain")
        );
        let esc = root
            .field("esc", 1 << 20)
            .unwrap()
            .unwrap()
            .string()
            .unwrap();
        assert_eq!(esc.decode(&mut scratch).unwrap(), JsonStr::Scratch("a\nb"));
        assert_eq!(
            root.field("skip", 1 << 20).unwrap().unwrap().raw(),
            r#"{"deep":[1,2,{"x":"\u0041"}]}"#
        );
        let kinds: Vec<Kind> = root.fields().unwrap().map(|(_, v)| v.kind()).collect();
        assert_eq!(
            kinds,
            [
                Kind::Object,
                Kind::Number,
                Kind::String,
                Kind::String,
                Kind::Number
            ]
        );
    }
}

#[test]
fn reusable_parser_reports_errors_and_recovers() {
    let mut parser = SimdParser::new();
    let err = parser
        .parse(b"[1,]", Limits::default(), |_| ())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Syntax);
    let n = parser
        .parse(b"[1,2,3]", Limits::default(), |doc| doc.token_count())
        .unwrap();
    assert_eq!(n, 4);
    let topic = parser
        .parse(br#"{"t":"borrowed"}"#, Limits::default(), |doc| {
            doc.root().field("t", 100).unwrap().unwrap().raw()
        })
        .unwrap();
    assert_eq!(topic, "\"borrowed\"");
    for isa in Isa::ALL {
        assert_eq!(SimdParser::with_isa(isa).is_ok(), isa.is_available());
    }
}

#[test]
fn malformed_classics_are_rejected_by_both() {
    let cases: &[&[u8]] = &[
        b"",
        b" ",
        b"[",
        b"]",
        b"{",
        b"\"",
        b"\"abc",
        b"[1 2]",
        b"[1,]",
        b"{\"a\"}",
        b"{\"a\":}",
        b"{\"a\" 1}",
        b"{1:1}",
        b"tru",
        b"truex",
        b"nul",
        b"01",
        b"1.",
        b"-",
        b"1e",
        b"1e+",
        b"\"\\x\"",
        b"\"\\u12\"",
        b"\"\\ud800\"",
        b"\"\\udc00\"",
        b"\"\x01\"",
        b"\"a\"b",
        b"1\"a\"",
        b"[\"a\" \"b\"]",
        b"{\"a\":1 \"b\":2}",
        b"\xef\xbb\xbf[]",
        b"[\x0c]",
        b"[1]\x1a",
        b"\"\\\"",
        b"{\"a\":1}}",
        b"[[]",
        b"[]]",
        b"\"\xed\xa0\x80\"",
    ];
    for case in cases {
        assert!(
            IndexedJson::parse(case, Limits::default()).is_err(),
            "{case:?}"
        );
        compare(case, Limits::default());
    }
}
