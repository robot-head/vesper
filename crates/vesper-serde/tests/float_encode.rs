use vesper_serde::{
    EncodeLimits, ErrorKind, FloatLimits, IndexedJson, JsonSizeBound, Kind, Limits, Value,
    encode_into, encoded_size,
};

fn finite_roundtrip(value: f64) -> usize {
    let mut output = [0xa5; 520];
    let limits = EncodeLimits::default();
    let measured = encoded_size(&Value::F64(value), limits).unwrap();
    assert!(measured <= 512);
    let written = encode_into(&Value::F64(value), &mut output, limits).unwrap();
    assert_eq!(written, measured);
    assert!(output[written..].iter().all(|&b| b == 0xa5));
    let parsed = IndexedJson::parse(&output[..written], Limits::default()).unwrap();
    assert_eq!(parsed.root().kind(), Kind::Number);
    assert_eq!(
        parsed
            .root()
            .as_f64(FloatLimits::default())
            .unwrap()
            .to_bits(),
        value.to_bits()
    );
    // A separate scalar conversion also checks the formatter's actual text.
    assert_eq!(
        core::str::from_utf8(&output[..written])
            .unwrap()
            .parse::<f64>()
            .unwrap()
            .to_bits(),
        value.to_bits()
    );
    let mut again = [0x5a; 520];
    assert_eq!(
        encode_into(&Value::F64(value), &mut again, limits).unwrap(),
        written
    );
    assert_eq!(&output[..written], &again[..written]);
    assert!(again[written..].iter().all(|&b| b == 0x5a));
    written
}

#[test]
fn finite_boundary_values_roundtrip_as_actual_numbers_and_preserve_signed_zero() {
    for bits in [
        0,
        1 << 63,
        1,
        (1 << 63) | 1,
        2,
        (1 << 52) - 1,
        1 << 52,
        (1 << 52) + 1,
        0x7fef_ffff_ffff_ffff,
        0xffef_ffff_ffff_ffff,
        0x3fef_ffff_ffff_ffff,
        0x3ff0_0000_0000_0000,
        0x3ff0_0000_0000_0001,
        0x4340_0000_0000_0000,
        0x4340_0000_0000_0001,
    ] {
        finite_roundtrip(f64::from_bits(bits));
    }
    for value in [
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        f64::EPSILON,
        -f64::EPSILON,
        std::f64::consts::PI,
        -std::f64::consts::PI,
        1e-100,
        1e100,
        1e-308,
        -1e-308,
        1e308,
        -1e308,
    ] {
        finite_roundtrip(value);
    }
    let mut output = [0; 512];
    let n = encode_into(&Value::F64(-0.0), &mut output, EncodeLimits::default()).unwrap();
    assert_eq!(output[0], b'-');
    assert_eq!(
        IndexedJson::parse(&output[..n], Limits::default())
            .unwrap()
            .root()
            .as_f64(FloatLimits::default())
            .unwrap()
            .to_bits(),
        1 << 63
    );
}

#[test]
fn every_finite_exponent_class_and_arbitrary_finite_bits_roundtrip_exactly() {
    for exponent in 0..0x7ff_u64 {
        for (sign, fraction) in [(0, 0), (1 << 63, 1), (0, (1 << 52) - 1)] {
            finite_roundtrip(f64::from_bits(sign | exponent << 52 | fraction));
        }
    }
    let mut state = 0x64a9_e6c7_53d2_f081_u64;
    let mut checked = 0;
    for _ in 0..4096 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let value = f64::from_bits(state);
        if value.is_finite() {
            finite_roundtrip(value);
            checked += 1;
        }
    }
    assert!(checked > 4000);
}

#[test]
fn all_nonfinite_sign_and_payload_classes_refuse_before_any_nested_output_write() {
    for sign in [0, 1 << 63] {
        for fraction in [0, 1, 2, 1 << 17, 1 << 50, 1 << 51, (1 << 52) - 1] {
            let value = f64::from_bits(sign | 0x7ff0_0000_0000_0000 | fraction);
            assert!(!value.is_finite());
            let bad = Value::F64(value);
            let inner = [Value::Bool(true), bad];
            let object = [("first", Value::F64(0.25)), ("last", Value::Array(&inner))];
            let outer = [
                Value::String("already checked prefix"),
                Value::Object(&object),
            ];
            for input in [bad, Value::Array(&outer)] {
                assert_eq!(
                    encoded_size(&input, EncodeLimits::default())
                        .unwrap_err()
                        .kind,
                    ErrorKind::Number
                );
                for capacity in [0, 1, 8, 2048] {
                    let mut output = [0xa5; 2048];
                    let error =
                        encode_into(&input, &mut output[..capacity], EncodeLimits::default())
                            .unwrap_err();
                    assert_eq!((error.kind, error.offset), (ErrorKind::Number, 0));
                    assert_eq!(output, [0xa5; 2048]);
                }
            }
        }
    }
}

#[test]
fn both_stack_formatting_passes_and_output_bytes_are_admitted_before_writing() {
    // Frozen declared logical allowance, not a claim about CPU instructions.
    const PASS: usize = 512 * 512 + 512;
    let input = Value::F64(0.0);
    let exact = PASS * 2 + 2; // One value token, two passes, one output byte.
    assert_eq!(
        encoded_size(
            &input,
            EncodeLimits {
                max_work: exact,
                ..Default::default()
            }
        )
        .unwrap(),
        1
    );
    for max_work in [0, PASS, PASS * 2, exact - 1] {
        let limits = EncodeLimits {
            max_work,
            ..Default::default()
        };
        let mut output = [0xa5; 16];
        assert_eq!(
            encode_into(&input, &mut output, limits).unwrap_err().kind,
            ErrorKind::WorkLimit
        );
        assert_eq!(output, [0xa5; 16]);
    }
    let values = [Value::F64(0.0); 32];
    assert!(encoded_size(&Value::Array(&values[..31]), EncodeLimits::default()).is_ok());
    let mut output = [0xa5; 128];
    assert_eq!(
        encode_into(&Value::Array(&values), &mut output, EncodeLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::WorkLimit
    );
    assert_eq!(output, [0xa5; 128]);
    let values = [Value::F64(-f64::from_bits(1)); 31];
    assert!(encoded_size(&Value::Array(&values), EncodeLimits::default()).is_ok());
}

#[test]
fn size_capacity_and_suffix_checks_remain_exact_for_long_and_short_float_text() {
    for value in [0.0, -0.0, 1.5, f64::MAX, -f64::from_bits(1)] {
        let input = Value::F64(value);
        let size = encoded_size(&input, EncodeLimits::default()).unwrap();
        for capacity in [0, size.saturating_sub(1)] {
            let mut output = [0x6b; 520];
            assert_eq!(
                encode_into(&input, &mut output[..capacity], EncodeLimits::default())
                    .unwrap_err()
                    .kind,
                ErrorKind::OutputTooSmall
            );
            assert_eq!(output, [0x6b; 520]);
        }
        let mut output = [0x6b; 520];
        assert_eq!(
            encode_into(
                &input,
                &mut output,
                EncodeLimits {
                    max_output_bytes: size - 1,
                    ..Default::default()
                }
            )
            .unwrap_err()
            .kind,
            ErrorKind::OutputLimit
        );
        assert_eq!(output, [0x6b; 520]);
        assert_eq!(
            encode_into(&input, &mut output[..size], EncodeLimits::default()).unwrap(),
            size
        );
        assert!(output[size..].iter().all(|&b| b == 0x6b));
    }
}

#[test]
fn mixed_raw_unicode_floats_and_integer_extremes_keep_existing_semantics() {
    let raw_text = r#"{"nested":["é🦀",-0,1e999999999999999999999]}"#;
    let raw = IndexedJson::parse(raw_text.as_bytes(), Limits::default()).unwrap();
    let fields = [
        ("float", Value::F64(-0.0)),
        ("é🦀", Value::String("\"\\\n")),
        ("raw", Value::Raw(raw.root())),
        ("u", Value::U64(u64::MAX)),
        ("i", Value::I64(i64::MIN)),
    ];
    let mut output = [0xa5; 2048];
    let written = encode_into(
        &Value::Object(&fields),
        &mut output,
        EncodeLimits::default(),
    )
    .unwrap();
    assert!(output[written..].iter().all(|&b| b == 0xa5));
    let parsed = IndexedJson::parse(&output[..written], Limits::default()).unwrap();
    let field = |name| parsed.root().field(name, 4096).unwrap().unwrap();
    assert_eq!(
        field("float")
            .as_f64(FloatLimits::default())
            .unwrap()
            .to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(field("raw").raw(), raw_text);
    assert_eq!(field("u").as_u64().unwrap(), u64::MAX);
    assert_eq!(field("i").as_i64().unwrap(), i64::MIN);
    assert_eq!(field("é🦀").string().unwrap().decoded_len(), 3);
}

#[test]
fn old_duplicate_string_raw_depth_and_token_failures_remain_atomic_with_floats() {
    let duplicate = [("x", Value::F64(1.0)), ("x", Value::F64(2.0))];
    let nested = [Value::F64(1.0)];
    let raw = IndexedJson::parse(br#"[[0]]"#, Limits::default()).unwrap();
    let cases = [
        (
            Value::Object(&duplicate),
            EncodeLimits::default(),
            ErrorKind::DuplicateKey,
        ),
        (
            Value::Array(&nested),
            EncodeLimits {
                max_depth: 0,
                ..Default::default()
            },
            ErrorKind::DepthLimit,
        ),
        (
            Value::Array(&nested),
            EncodeLimits {
                max_tokens: 1,
                ..Default::default()
            },
            ErrorKind::TokenLimit,
        ),
        (
            Value::Raw(raw.root()),
            EncodeLimits {
                max_depth: 1,
                ..Default::default()
            },
            ErrorKind::DepthLimit,
        ),
        (
            Value::String("é"),
            EncodeLimits {
                max_string_bytes: 1,
                ..Default::default()
            },
            ErrorKind::StringLimit,
        ),
    ];
    for (value, limits, expected) in cases {
        let mut output = [0xa5; 1024];
        assert_eq!(
            encode_into(&value, &mut output, limits).unwrap_err().kind,
            expected
        );
        assert_eq!(output, [0xa5; 1024]);
    }
}

#[test]
fn finite_size_ceiling_composes_without_authorizing_nonfinite_values() {
    assert_eq!(JsonSizeBound::F64, JsonSizeBound::Bounded(512));
    assert_eq!(
        JsonSizeBound::Bounded(2)
            .combine_field(1, JsonSizeBound::F64, true)
            .unwrap(),
        JsonSizeBound::Bounded(518)
    );
    let mut output = [0xa5; 512];
    assert_eq!(
        encode_into(&Value::F64(f64::NAN), &mut output, EncodeLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Number
    );
    assert_eq!(output, [0xa5; 512]);
}
