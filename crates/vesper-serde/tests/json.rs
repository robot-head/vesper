use vesper_serde::{
    EncodeLimits, ErrorKind, IndexedJson, JsonSizeBound, JsonStr, Kind, Limits, SwarFieldMatcher,
    Value, compute_escaped_mask_64, encode_into, encoded_size,
};

fn parse(bytes: &[u8]) -> IndexedJson<'_> {
    IndexedJson::parse(bytes, Limits::default()).unwrap()
}
fn bad(bytes: &[u8]) {
    assert!(
        IndexedJson::parse(bytes, Limits::default()).is_err(),
        "accepted {:?}",
        bytes
    );
}
fn kind_error(bytes: &[u8], limits: Limits, expected: ErrorKind) {
    assert_eq!(
        IndexedJson::parse(bytes, limits).unwrap_err().kind,
        expected
    );
}

#[test]
fn validates_entire_unknown_subtrees_before_exposing_raw() {
    let doc = parse(br#"{"known":42,"unknown":{"nested":[null,true,false,-0,1.2e+3,"x"]}}"#);
    assert_eq!(doc.root().kind(), Kind::Object);
    assert_eq!(
        doc.root()
            .field("known", 1000)
            .unwrap()
            .unwrap()
            .as_u64()
            .unwrap(),
        42
    );
    let raw = doc.root().field("unknown", 1000).unwrap().unwrap();
    assert_eq!(raw.raw(), r#"{"nested":[null,true,false,-0,1.2e+3,"x"]}"#);
    for input in [
        br#"{"known":42,"unknown":[tru]}"#.as_slice(),
        br#"{"known":42,"unknown":{"bad":01}}"#,
        br#"{"known":42,"unknown":{"bad":"\uD800"}}"#,
        br#"{"known":42,"unknown":[1,]}"#,
        br#"{"known":42,"unknown":{"x":1,"x":2}}"#,
    ] {
        bad(input);
    }
}

#[test]
fn unescaped_utf8_is_zero_copy_without_scratch() {
    let input = "\"héllo 🦀\"";
    let doc = parse(input.as_bytes());
    let mut cursor = doc.cursor();
    let s = cursor.read_str(&mut []).unwrap();
    assert!(matches!(s, JsonStr::Borrowed(_)));
    assert_eq!(s.as_str(), "héllo 🦀");
    assert_eq!(s.as_str().as_ptr(), input[1..].as_ptr());
    assert!(cursor.peek().is_none());
}

#[test]
fn escapes_and_surrogate_pairs_decode_into_explicit_scratch() {
    let doc = parse(br#""\"\\\/\b\f\n\r\t\u0000\u00e9\uD83E\uDD80""#);
    let s = doc.root().string().unwrap();
    let expected = "\"\\/\x08\x0c\n\r\t\0é🦀";
    assert_eq!(s.decoded_len(), expected.len());
    assert!(s.equals(expected));
    let mut scratch = [0xcc; 80];
    let decoded = s.decode(&mut scratch).unwrap();
    assert!(matches!(decoded, JsonStr::Scratch(_)));
    assert_eq!(decoded.as_str(), expected);
    assert!(scratch[expected.len()..].iter().all(|b| *b == 0xcc));
}

#[test]
fn scratch_failure_preserves_cursor_bytes_and_owner_for_retry() {
    let doc = parse(br#""a\uD83E\uDD80b""#);
    let mut cursor = doc.cursor();
    let position = cursor.position();
    let mut scratch = [0xab; 5];
    assert_eq!(
        cursor.read_str(&mut scratch).unwrap_err().kind,
        ErrorKind::ScratchTooSmall
    );
    assert_eq!(cursor.position(), position);
    assert_eq!(scratch, [0xab; 5]);
    let mut scratch = [0; 6];
    assert_eq!(cursor.read_str(&mut scratch).unwrap().as_str(), "a🦀b");
    assert_eq!(doc.root().raw(), r#""a\uD83E\uDD80b""#);
    let doc = parse(b"123");
    let mut cursor = doc.cursor();
    assert_eq!(cursor.read_str(&mut []).unwrap_err().kind, ErrorKind::Type);
    assert_eq!(cursor.position(), 0);
    assert_eq!(cursor.read_raw().unwrap().as_u64().unwrap(), 123);
}

#[test]
fn duplicate_policy_compares_decoded_unicode_in_every_object() {
    for input in [
        r#"{"a":1,"a":2}"#,
        r#"{"a":1,"\u0061":2}"#,
        r#"{"\u0061":1,"a":2}"#,
        r#"{"🦀":1,"\uD83E\uDD80":2}"#,
        r#"{"a":{"x":1},"b":{"x":1,"\u0078":2}}"#,
        r#"{"":0,"":1}"#,
        r#"{"/":0,"\/":1}"#,
    ] {
        assert_eq!(
            IndexedJson::parse(input.as_bytes(), Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::DuplicateKey
        );
    }
    // Unicode normalization is intentionally not applied.
    parse("{\"é\":1,\"é\":2,\"A\":3,\"a\":4}".as_bytes());
}

#[test]
fn decoded_lookup_skips_siblings_and_unknown_nested_values() {
    let doc = parse(br#"{"unknown":[{"a":"x"}],"\u0069d":18446744073709551615,"ok":true}"#);
    assert_eq!(
        doc.root()
            .field("id", 4096)
            .unwrap()
            .unwrap()
            .as_u64()
            .unwrap(),
        u64::MAX
    );
    assert!(
        doc.root()
            .field("ok", 4096)
            .unwrap()
            .unwrap()
            .as_bool()
            .unwrap()
    );
    assert!(doc.root().field("missing", 4096).unwrap().is_none());
    assert_eq!(
        doc.root().field("id", 0).unwrap_err().kind,
        ErrorKind::WorkLimit
    );
    assert_eq!(doc.root().fields().unwrap().count(), 3);
}

#[test]
fn array_cursor_is_scoped_and_subtree_skip_is_atomic() {
    let doc = parse(br#"{"a":[[1,2],"x",3],"outside":7}"#);
    let a = doc.root().field("a", 1000).unwrap().unwrap();
    let mut cursor = a.elements().unwrap();
    assert_eq!(cursor.peek().unwrap().kind(), Kind::Array);
    cursor.skip().unwrap();
    assert_eq!(cursor.read_str(&mut []).unwrap().as_str(), "x");
    assert_eq!(cursor.read_raw().unwrap().as_u64().unwrap(), 3);
    assert!(cursor.peek().is_none());
    let end = cursor.position();
    assert_eq!(cursor.skip().unwrap_err().kind, ErrorKind::End);
    assert_eq!(cursor.position(), end);
    assert!(parse(b"[]").root().elements().unwrap().peek().is_none());
}

#[test]
fn strict_number_grammar_and_integer_ranges() {
    for input in [
        "0",
        "-0",
        "1",
        "-10",
        "0.01",
        "1e0",
        "1E+02",
        "-1.5e-9",
        "1e99999999999999999999999",
    ] {
        assert_eq!(parse(input.as_bytes()).root().kind(), Kind::Number);
    }
    for input in [
        "+1", "01", "-01", "-", ".1", "1.", "1e", "1e+", "1e-", "--1", "1_0", "NaN", "Infinity",
        "0x1", "1 2",
    ] {
        bad(input.as_bytes());
    }
    assert_eq!(
        parse(b"18446744073709551615").root().as_u64().unwrap(),
        u64::MAX
    );
    assert!(parse(b"18446744073709551616").root().as_u64().is_err());
    assert_eq!(
        parse(b"-9223372036854775808").root().as_i64().unwrap(),
        i64::MIN
    );
    assert!(parse(b"9223372036854775808").root().as_i64().is_err());
    assert!(parse(b"1e0").root().as_u64().is_err());
    assert!(parse(b"-1").root().as_u64().is_err());
    assert!(parse(b"true").root().as_u64().is_err());
}

#[test]
fn rejects_invalid_unicode_utf8_controls_and_trailing_data() {
    for input in [
        br#""\uD800""#.as_slice(),
        br#""\uDC00""#,
        br#""\uD800\u0000""#,
        br#""\uD800x""#,
        br#""\uD800\uD800""#,
        br#""\uZZZZ""#,
        br#""\x00""#,
        &[b'"', 0xff, b'"'],
        &[b'"', 0xc0, 0x80, b'"'],
        &[b'"', 0xed, 0xa0, 0x80, b'"'],
        &[b'"', 0xf4, 0x90, 0x80, 0x80, b'"'],
        b"\xef\xbb\xbfnull",
        b"true false",
        b"{\"a\":1,}",
        b"[1,,2]",
        b"{a:1}",
        b"\x0bnull",
        b"\"\n\"",
    ] {
        bad(input);
    }
    for control in 0..=31 {
        bad(&[b'"', control, b'"']);
    }
    for input in [r#""\u0000""#, r#""\uD800\uDC00""#, r#""\uDBFF\uDFFF""#] {
        parse(input.as_bytes());
    }
}

#[test]
fn every_truncation_and_split_of_nested_unicode_document() {
    let source = "{\"a\":[null,true,false,-0.12e+3,\"é🦀\\uD83E\\uDD80\"],\"z\":{\"x\":42}}";
    let bytes = source.as_bytes();
    for end in 0..bytes.len() {
        bad(&bytes[..end]);
    }
    let doc = parse(bytes);
    for split in 0..=bytes.len() {
        // There is no streaming API. Exercise byte-split reconstruction, including
        // splits inside UTF-8, escapes and surrogate pairs, as immutable input.
        let joined = [&bytes[..split], &bytes[split..]].concat();
        assert_eq!(parse(&joined).root().raw(), doc.root().raw());
    }
}

#[test]
fn backslashes_across_every_64_byte_boundary() {
    for start in 0..140 {
        for run in 0..140 {
            let mut input = String::from("\"");
            input.extend(std::iter::repeat_n('a', start));
            input.extend(std::iter::repeat_n('\\', run));
            // An odd run escapes this quote; a further quote closes the string.
            input.push('"');
            if run % 2 != 0 {
                input.push('"');
            }
            let doc = parse(input.as_bytes());
            let string = doc.root().string().unwrap();
            let mut scratch = vec![0; string.decoded_len()];
            let result = string.decode(&mut scratch).unwrap();
            assert_eq!(result.as_str().len(), start + run / 2 + run % 2);
        }
    }
}

#[test]
fn escape_carry_matches_independent_run_reference() {
    for carry in [false, true] {
        for bits in 0..=u16::MAX {
            let mask = u64::from(bits) << 48;
            let (actual, out) = compute_escaped_mask_64(mask, carry);
            let mut expected = u64::from(carry);
            let mut run = usize::from(carry);
            for i in 0..64 {
                if mask & (1_u64 << i) == 0 {
                    run = 0;
                } else {
                    run += 1;
                }
                if run % 2 == 1 && i != 63 {
                    expected |= 1_u64 << (i + 1);
                }
            }
            assert_eq!(actual, expected);
            assert_eq!(out, run % 2 == 1);
        }
    }
    let (first, carry) = compute_escaped_mask_64(1_u64 << 63, false);
    assert_eq!(first, 0);
    assert!(carry);
    let (second, carry) = compute_escaped_mask_64(1, carry);
    assert_eq!(second, 1);
    assert!(!carry);
}

#[test]
fn parse_resource_limits_have_real_boundaries() {
    let bytes = r#"{"a":["é","\uD83E\uDD80"]}"#.as_bytes();
    let doc = parse(bytes);
    let base = Limits::default();
    kind_error(
        bytes,
        Limits {
            max_input_bytes: bytes.len() - 1,
            ..base
        },
        ErrorKind::InputLimit,
    );
    IndexedJson::parse(
        bytes,
        Limits {
            max_input_bytes: bytes.len(),
            ..base
        },
    )
    .unwrap();
    kind_error(
        bytes,
        Limits {
            max_depth: 1,
            ..base
        },
        ErrorKind::DepthLimit,
    );
    IndexedJson::parse(
        bytes,
        Limits {
            max_depth: 2,
            ..base
        },
    )
    .unwrap();
    kind_error(
        bytes,
        Limits {
            max_tokens: doc.token_count() - 1,
            ..base
        },
        ErrorKind::TokenLimit,
    );
    IndexedJson::parse(
        bytes,
        Limits {
            max_tokens: doc.token_count(),
            ..base
        },
    )
    .unwrap();
    kind_error(
        bytes,
        Limits {
            max_string_bytes: 3,
            ..base
        },
        ErrorKind::StringLimit,
    );
    IndexedJson::parse(
        bytes,
        Limits {
            max_string_bytes: 4,
            ..base
        },
    )
    .unwrap();
    kind_error(
        bytes,
        Limits {
            max_total_string_bytes: 6,
            ..base
        },
        ErrorKind::StringLimit,
    );
    IndexedJson::parse(
        bytes,
        Limits {
            max_total_string_bytes: 7,
            ..base
        },
    )
    .unwrap();
    kind_error(
        bytes,
        Limits {
            max_work: doc.work_used() - 1,
            ..base
        },
        ErrorKind::WorkLimit,
    );
    IndexedJson::parse(
        bytes,
        Limits {
            max_work: doc.work_used(),
            ..base
        },
    )
    .unwrap();
    kind_error(
        bytes,
        Limits {
            max_index_bytes: 0,
            ..base
        },
        ErrorKind::AllocationLimit,
    );
    // `max_index_bytes` pays for the tape and then 4 bytes per stage 1
    // entry (structural or escape position), in every engine.
    let mut stage1 = vesper_serde::simd::StructuralIndex::new();
    stage1
        .build(bytes, vesper_serde::simd::Isa::Scalar, Default::default())
        .unwrap();
    let entries = stage1.structurals().len() + stage1.escapes().len();
    let index_bytes = doc.index_capacity_bytes() + 4 * entries;
    let limited = IndexedJson::parse(
        bytes,
        Limits {
            max_index_bytes: index_bytes,
            ..base
        },
    )
    .unwrap();
    assert!(limited.index_capacity_bytes() <= index_bytes);
    kind_error(
        bytes,
        Limits {
            max_index_bytes: index_bytes - 1,
            ..base
        },
        ErrorKind::AllocationLimit,
    );
    kind_error(
        b"0",
        Limits {
            max_tokens: 0,
            ..base
        },
        ErrorKind::TokenLimit,
    );
    kind_error(
        b"0",
        Limits {
            max_work: 0,
            ..base
        },
        ErrorKind::WorkLimit,
    );
    kind_error(
        b"[]",
        Limits {
            max_depth: 0,
            ..base
        },
        ErrorKind::DepthLimit,
    );
    // Hard recursion ceiling protects even callers requesting an unbounded depth.
    let deep = format!("{}0{}", "[".repeat(129), "]".repeat(129));
    kind_error(
        deep.as_bytes(),
        Limits {
            max_depth: usize::MAX,
            ..base
        },
        ErrorKind::DepthLimit,
    );
}

#[test]
fn duplicate_comparison_work_is_bounded_before_completion() {
    let prefix = "a".repeat(1000);
    let source = format!("{{\"{prefix}x\":0,\"{prefix}y\":1}}");
    let ordinary = parse(source.as_bytes());
    assert!(ordinary.work_used() > source.len() * 2);
    kind_error(
        source.as_bytes(),
        Limits {
            max_work: source.len() * 3,
            ..Limits::default()
        },
        ErrorKind::WorkLimit,
    );
}

#[test]
fn swar_matching_rejects_unsafe_keys_and_checks_short_windows() {
    for key in ["abcdefgh", "a\"", "a\\", "\n", "é", "\x7f"] {
        assert!(SwarFieldMatcher::compile_key(0, key).is_none());
    }
    let doc = parse(br#"{"\u0069d":1}"#);
    let (key, _) = doc.root().fields().unwrap().next().unwrap();
    assert!(
        SwarFieldMatcher::compile_key(0, "id")
            .unwrap()
            .matches_key(key)
    );
    for key in ["", "id", "1234567", "two key"] {
        let matcher = SwarFieldMatcher::compile_key(9, key).unwrap();
        assert_eq!(matcher.field_idx(), 9);
        assert_eq!(matcher.key_len(), key.len());
        let window = format!("{key}\":trailing");
        assert!(matcher.matches_window(window.as_bytes()));
        for n in 0..=key.len() {
            assert!(!matcher.matches_window(&window.as_bytes()[..n]));
        }
        assert!(!matcher.matches_window(format!("{key}x\"").as_bytes()));
    }
}

#[test]
fn schema_bounds_use_checked_arithmetic() {
    assert_eq!(
        JsonSizeBound::string(20).unwrap(),
        JsonSizeBound::Bounded(122)
    );
    assert_eq!(
        JsonSizeBound::string(usize::MAX).unwrap_err().kind,
        ErrorKind::Overflow
    );
    assert_eq!(
        JsonSizeBound::Bounded(2)
            .combine_field(2, JsonSizeBound::U64, true)
            .unwrap(),
        JsonSizeBound::Bounded(27)
    );
    assert_eq!(
        JsonSizeBound::Bounded(2)
            .combine_field(2, JsonSizeBound::Dynamic { static_ceiling: 2 }, false)
            .unwrap(),
        JsonSizeBound::Dynamic { static_ceiling: 10 }
    );
    for (a, key, b) in [(usize::MAX, 0, 0), (0, usize::MAX, 0), (0, 0, usize::MAX)] {
        assert_eq!(
            JsonSizeBound::Bounded(a)
                .combine_field(key, JsonSizeBound::Bounded(b), false)
                .unwrap_err()
                .kind,
            ErrorKind::Overflow
        );
    }
}

#[test]
fn real_encoding_roundtrip_including_raw_validated_subtree() {
    let raw = parse(br#"[1e999,{"\u0061":true}]"#);
    let array = [
        Value::U64(u64::MAX),
        Value::I64(i64::MIN),
        Value::Bool(false),
        Value::Null,
    ];
    let fields = [
        ("id", Value::U64(42)),
        ("topic", Value::String("a\n\0é🦀\"\\")),
        ("array", Value::Array(&array)),
        ("raw", Value::Raw(raw.root())),
    ];
    let value = Value::Object(&fields);
    let size = encoded_size(&value, EncodeLimits::default()).unwrap();
    let mut output = vec![0xab; size + 8];
    assert_eq!(
        encode_into(&value, &mut output, EncodeLimits::default()).unwrap(),
        size
    );
    assert_eq!(&output[size..], &[0xab; 8]);
    let doc = parse(&output[..size]);
    assert_eq!(
        doc.root()
            .field("id", 5000)
            .unwrap()
            .unwrap()
            .as_u64()
            .unwrap(),
        42
    );
    let s = doc
        .root()
        .field("topic", 5000)
        .unwrap()
        .unwrap()
        .string()
        .unwrap();
    assert!(s.equals("a\n\0é🦀\"\\"));
    assert_eq!(
        doc.root().field("raw", 5000).unwrap().unwrap().raw(),
        raw.root().raw()
    );
    let mut items = doc
        .root()
        .field("array", 5000)
        .unwrap()
        .unwrap()
        .elements()
        .unwrap();
    assert_eq!(items.read_raw().unwrap().as_u64().unwrap(), u64::MAX);
    assert_eq!(items.read_raw().unwrap().as_i64().unwrap(), i64::MIN);
}

#[test]
fn all_ascii_controls_and_unicode_encode_correctly() {
    let controls: String = (0..=127).map(char::from).chain(['é', '🦀']).collect();
    let value = Value::String(&controls);
    let mut out = vec![0; encoded_size(&value, EncodeLimits::default()).unwrap()];
    encode_into(&value, &mut out, EncodeLimits::default()).unwrap();
    assert!(parse(&out).root().string().unwrap().equals(&controls));
}

#[test]
fn every_undersized_output_is_unchanged_and_input_reusable() {
    let fields = [("a", Value::String("é\n🦀")), ("b", Value::I64(i64::MIN))];
    let value = Value::Object(&fields);
    let size = encoded_size(&value, EncodeLimits::default()).unwrap();
    for n in 0..size {
        let mut out = vec![0xa5; n];
        assert_eq!(
            encode_into(&value, &mut out, EncodeLimits::default())
                .unwrap_err()
                .kind,
            ErrorKind::OutputTooSmall
        );
        assert_eq!(out, vec![0xa5; n]);
    }
    let mut out = vec![0; size];
    encode_into(&value, &mut out, EncodeLimits::default()).unwrap();
    parse(&out);
}

#[test]
fn all_encoder_failure_categories_are_transactional() {
    let fields = [("a", Value::String("hello")), ("b", Value::Bool(true))];
    let value = Value::Object(&fields);
    let base = EncodeLimits::default();
    let cases = [
        (
            EncodeLimits {
                max_output_bytes: 0,
                ..base
            },
            ErrorKind::OutputLimit,
        ),
        (
            EncodeLimits {
                max_tokens: 0,
                ..base
            },
            ErrorKind::TokenLimit,
        ),
        (
            EncodeLimits {
                max_depth: 0,
                ..base
            },
            ErrorKind::DepthLimit,
        ),
        (
            EncodeLimits {
                max_string_bytes: 4,
                ..base
            },
            ErrorKind::StringLimit,
        ),
        (
            EncodeLimits {
                max_total_string_bytes: 6,
                ..base
            },
            ErrorKind::StringLimit,
        ),
        (
            EncodeLimits {
                max_work: 0,
                ..base
            },
            ErrorKind::WorkLimit,
        ),
    ];
    for (limits, expected) in cases {
        let mut out = [0xab; 100];
        assert_eq!(
            encode_into(&value, &mut out, limits).unwrap_err().kind,
            expected
        );
        assert_eq!(out, [0xab; 100]);
    }
    let duplicates = [("é", Value::U64(0)), ("é", Value::U64(1))];
    let mut out = [0xab; 100];
    assert_eq!(
        encode_into(&Value::Object(&duplicates), &mut out, base)
            .unwrap_err()
            .kind,
        ErrorKind::DuplicateKey
    );
    assert_eq!(out, [0xab; 100]);
}

#[test]
fn raw_forwarding_cannot_bypass_encoder_limits() {
    let raw = parse(br#"[[{"a":"hello"}]]"#);
    let value = Value::Raw(raw.root());
    let base = EncodeLimits::default();
    for limits in [
        EncodeLimits {
            max_depth: 2,
            ..base
        },
        EncodeLimits {
            max_tokens: 3,
            ..base
        },
        EncodeLimits {
            max_string_bytes: 4,
            ..base
        },
        EncodeLimits {
            max_total_string_bytes: 5,
            ..base
        },
        EncodeLimits {
            max_work: 0,
            ..base
        },
    ] {
        let mut out = [0xef; 100];
        assert!(encode_into(&value, &mut out, limits).is_err());
        assert_eq!(out, [0xef; 100]);
    }
    // Relative depth of a non-root raw subtree is rebased correctly.
    let subtree = raw.root().elements().unwrap().read_raw().unwrap();
    assert!(
        encoded_size(
            &Value::Raw(subtree),
            EncodeLimits {
                max_depth: 2,
                ..base
            }
        )
        .is_ok()
    );
    let nested = [Value::Raw(raw.root())];
    assert_eq!(
        encoded_size(
            &Value::Array(&nested),
            EncodeLimits {
                max_depth: 3,
                ..base
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::DepthLimit
    );
}

#[test]
fn integer_encoding_matches_standard_decimal_for_many_values() {
    let mut seed = 0xabc123456789_u64;
    for _ in 0..10_000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut out = [0; 20];
        let n = encode_into(&Value::U64(seed), &mut out, EncodeLimits::default()).unwrap();
        assert_eq!(&out[..n], seed.to_string().as_bytes());
        let signed = seed as i64;
        let n = encode_into(&Value::I64(signed), &mut out, EncodeLimits::default()).unwrap();
        assert_eq!(&out[..n], signed.to_string().as_bytes());
    }
}

#[test]
fn deterministic_byte_mutation_corpus_never_panics_or_exposes_invalid_views() {
    let source = br#"{"a":[1,true,null,"x\uD83E\uDD80"],"b":{"k":-2.5e+3}}"#;
    for position in 0..source.len() {
        for byte in 0..=u8::MAX {
            let mut candidate = source.to_vec();
            candidate[position] = byte;
            if let Ok(doc) = IndexedJson::parse(&candidate, Limits::default()) {
                let value = Value::Raw(doc.root());
                let mut out = vec![0; candidate.len()];
                encode_into(&value, &mut out, EncodeLimits::default()).unwrap();
                assert_eq!(candidate, out);
                let reparsed = parse(&out);
                assert_eq!(doc.token_count(), reparsed.token_count());
            }
        }
    }
}

#[test]
fn random_valid_unicode_strings_roundtrip_without_global_decode_storage() {
    let mut state = 0x2134789191_u64;
    let mut source = String::new();
    for _ in 0..20_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        if let Some(ch) = char::from_u32((state % 0x110000) as u32) {
            source.push(ch);
        }
    }
    let value = Value::String(&source);
    let size = encoded_size(&value, EncodeLimits::default()).unwrap();
    let mut output = vec![0; size];
    encode_into(&value, &mut output, EncodeLimits::default()).unwrap();
    let doc = parse(&output);
    let string = doc.root().string().unwrap();
    let mut scratch = vec![0; string.decoded_len()];
    assert_eq!(string.decode(&mut scratch).unwrap().as_str(), source);
}

#[test]
fn schema_dynamic_static_ceiling_is_never_claimed_as_a_total_lower_bound() {
    let bound = JsonSizeBound::Bounded(2)
        .combine_field(1, JsonSizeBound::U64, true)
        .unwrap()
        .combine_field(1, JsonSizeBound::Dynamic { static_ceiling: 0 }, false)
        .unwrap();
    assert_eq!(bound, JsonSizeBound::Dynamic { static_ceiling: 31 });
    let fields = [("a", Value::U64(0)), ("b", Value::String(""))];
    let actual = encoded_size(&Value::Object(&fields), EncodeLimits::default()).unwrap();
    assert_eq!(actual, 14); // The static ceiling 31 is intentionally larger.
    for first in [false, true] {
        let prefix = if first { 4 } else { 5 };
        for (left, right, dynamic) in [
            (JsonSizeBound::Bounded(2), JsonSizeBound::Bounded(3), false),
            (
                JsonSizeBound::Bounded(2),
                JsonSizeBound::Dynamic { static_ceiling: 3 },
                true,
            ),
            (
                JsonSizeBound::Dynamic { static_ceiling: 2 },
                JsonSizeBound::Bounded(3),
                true,
            ),
            (
                JsonSizeBound::Dynamic { static_ceiling: 2 },
                JsonSizeBound::Dynamic { static_ceiling: 3 },
                true,
            ),
        ] {
            let expected = if dynamic {
                JsonSizeBound::Dynamic {
                    static_ceiling: 5 + prefix,
                }
            } else {
                JsonSizeBound::Bounded(5 + prefix)
            };
            assert_eq!(left.combine_field(1, right, first).unwrap(), expected);
        }
    }
    assert_eq!(
        JsonSizeBound::Dynamic {
            static_ceiling: usize::MAX
        }
        .combine_field(0, JsonSizeBound::Bounded(0), true)
        .unwrap_err()
        .kind,
        ErrorKind::Overflow
    );
}
