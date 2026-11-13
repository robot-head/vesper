//! Independent finite bit-pattern, mixed-value and failure-atomic acceptance.
use vesper_serde::{
    EncodeLimits, ErrorKind, FloatLimits, IndexedJson, JsonSizeBound, Limits, Value, encode_into,
    encoded_size,
};

fn parse(bytes: &[u8]) -> IndexedJson<'_> {
    IndexedJson::parse(bytes, Limits::default()).unwrap()
}

fn roundtrip(bits: u64) {
    let value = f64::from_bits(bits);
    assert!(value.is_finite());
    let mut output = [0x9b; 1024];
    let size = encoded_size(&Value::F64(value), EncodeLimits::default()).unwrap();
    assert!(size <= 512);
    let length = encode_into(&Value::F64(value), &mut output, EncodeLimits::default()).unwrap();
    assert_eq!(length, size);
    assert!(output[length..].iter().all(|byte| *byte == 0x9b));
    let decoded = parse(&output[..length]);
    assert_eq!(
        decoded
            .root()
            .as_f64(FloatLimits::default())
            .unwrap()
            .to_bits(),
        bits
    );
    let mut cursor = decoded.cursor();
    assert_eq!(
        cursor.read_f64(FloatLimits::default()).unwrap().to_bits(),
        bits
    );
    let end = cursor.position();
    assert_eq!(
        cursor.read_f64(FloatLimits::default()).unwrap_err().kind,
        ErrorKind::End
    );
    assert_eq!(cursor.position(), end);
}

#[test]
fn finite_ieee_boundaries_and_deterministic_bit_patterns_roundtrip_exactly() {
    let edges = [
        0,
        0x8000_0000_0000_0000,
        1,
        0x8000_0000_0000_0001,
        0x000f_ffff_ffff_ffff,
        0x800f_ffff_ffff_ffff,
        0x0010_0000_0000_0000,
        0x8010_0000_0000_0000,
        0x0010_0000_0000_0001,
        0x8010_0000_0000_0001,
        0x7fef_ffff_ffff_ffff,
        0xffef_ffff_ffff_ffff,
        0x7fef_ffff_ffff_fffe,
        0xffef_ffff_ffff_fffe,
        0x3fef_ffff_ffff_ffff,
        0x3ff0_0000_0000_0000,
        0x3ff0_0000_0000_0001,
        0x3cb0_0000_0000_0000,
        0x3fb9_9999_9999_999a,
        0x4340_0000_0000_0000,
        0x4340_0000_0000_0001,
    ];
    for bits in edges {
        roundtrip(bits);
    }
    let mut state = 0xbb67_ae85_84ca_a73b_u64;
    for _ in 0..8192 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        if state & 0x7ff0_0000_0000_0000 != 0x7ff0_0000_0000_0000 {
            roundtrip(state);
        }
    }
    assert_eq!(JsonSizeBound::F64, JsonSizeBound::Bounded(512));
}

#[test]
fn decimal_ieee_rounding_underflow_and_integer_contracts_remain_distinct() {
    for (source, bits) in [
        ("0", 0),
        ("-0", 0x8000_0000_0000_0000),
        ("-0.0", 0x8000_0000_0000_0000),
        ("-1e-9999", 0x8000_0000_0000_0000),
        ("1e-9999", 0),
        ("5e-324", 1),
        ("2.4703282292062327e-324", 0),
        ("2.4703282292062328e-324", 1),
        ("9007199254740993", 0x4340_0000_0000_0000),
        (
            "1.00000000000000011102230246251565404236316680908203125",
            0x3ff0_0000_0000_0000,
        ),
        (
            "1.00000000000000033306690738754696212708950042724609375",
            0x3ff0_0000_0000_0002,
        ),
        ("1.7976931348623157e308", 0x7fef_ffff_ffff_ffff),
    ] {
        let doc = parse(source.as_bytes());
        assert_eq!(
            doc.root().as_f64(FloatLimits::default()).unwrap().to_bits(),
            bits,
            "{source}"
        );
    }
    let unsigned = parse(b"18446744073709551615");
    assert_eq!(unsigned.root().as_u64().unwrap(), u64::MAX);
    assert_eq!(
        unsigned
            .root()
            .as_f64(FloatLimits::default())
            .unwrap()
            .to_bits(),
        18_446_744_073_709_551_616_f64.to_bits()
    );
    let signed = parse(b"-9223372036854775808");
    assert_eq!(signed.root().as_i64().unwrap(), i64::MIN);
    assert_eq!(
        parse(b"9007199254740993").root().as_u64().unwrap(),
        9_007_199_254_740_993
    );
    assert!(parse(b"1.5").root().as_u64().is_err());
    assert!(parse(b"1e0").root().as_i64().is_err());
}

#[test]
fn every_conversion_failure_keeps_original_offset_and_cursor_for_retry() {
    let source = b" [1.25, \"no\", 1e309, -0, 2.5] ";
    let doc = parse(source);
    let mut cursor = doc.root().elements().unwrap();
    assert_eq!(cursor.read_f64(FloatLimits::default()).unwrap(), 1.25);
    let position = cursor.position();
    let failure = cursor.read_f64(FloatLimits::default()).unwrap_err();
    assert_eq!((failure.kind, failure.offset), (ErrorKind::Type, 8));
    assert_eq!(cursor.position(), position);
    cursor.skip().unwrap();
    let position = cursor.position();
    let failure = cursor.read_f64(FloatLimits::default()).unwrap_err();
    assert_eq!((failure.kind, failure.offset), (ErrorKind::Number, 14));
    assert_eq!(cursor.position(), position);
    assert_eq!(cursor.read_raw().unwrap().raw(), "1e309");
    assert_eq!(
        cursor.read_f64(FloatLimits::default()).unwrap().to_bits(),
        (-0.0_f64).to_bits()
    );
    for (limits, kind) in [
        (
            FloatLimits {
                max_number_bytes: 2,
                ..FloatLimits::default()
            },
            ErrorKind::InputLimit,
        ),
        (
            FloatLimits {
                max_work: 8,
                ..FloatLimits::default()
            },
            ErrorKind::WorkLimit,
        ),
        (
            FloatLimits {
                max_number_bytes: 0,
                ..FloatLimits::default()
            },
            ErrorKind::InputLimit,
        ),
        (
            FloatLimits {
                max_work: 0,
                ..FloatLimits::default()
            },
            ErrorKind::WorkLimit,
        ),
        (
            FloatLimits {
                max_number_bytes: FloatLimits::HARD.max_number_bytes + 1,
                ..FloatLimits::default()
            },
            ErrorKind::InputLimit,
        ),
        (
            FloatLimits {
                max_work: FloatLimits::HARD.max_work + 1,
                ..FloatLimits::default()
            },
            ErrorKind::WorkLimit,
        ),
    ] {
        let position = cursor.position();
        let failure = cursor.read_f64(limits).unwrap_err();
        assert_eq!((failure.kind, failure.offset), (kind, 25));
        assert_eq!(cursor.position(), position);
    }
    assert_eq!(cursor.read_f64(FloatLimits::default()).unwrap(), 2.5);
    let end = cursor.position();
    assert_eq!(
        cursor.read_f64(FloatLimits::default()).unwrap_err().kind,
        ErrorKind::End
    );
    assert_eq!(cursor.position(), end);
}

#[test]
fn enormous_valid_raw_numbers_forward_without_claiming_float_representability() {
    let source = format!(" {{\"raw\":1{},\"tail\":42}} ", "0".repeat(5000));
    let doc = parse(source.as_bytes());
    let raw = doc.root().field("raw", 4096).unwrap().unwrap();
    assert_eq!(
        raw.as_f64(FloatLimits::default()).unwrap_err().kind,
        ErrorKind::InputLimit
    );
    assert_eq!(
        raw.as_f64(FloatLimits::HARD).unwrap_err().kind,
        ErrorKind::Number
    );
    let mut output = vec![0xd4; source.len() + 16];
    let length = encode_into(&Value::Raw(raw), &mut output, EncodeLimits::default()).unwrap();
    assert_eq!(&output[..length], raw.raw().as_bytes());
    assert!(output[length..].iter().all(|byte| *byte == 0xd4));
    assert_eq!(
        doc.root()
            .field("tail", 4096)
            .unwrap()
            .unwrap()
            .as_u64()
            .unwrap(),
        42
    );
    let expensive = "1".to_owned() + &"0".repeat(6000);
    let huge = parse(expensive.as_bytes());
    assert_eq!(
        huge.root()
            .as_f64(FloatLimits {
                max_number_bytes: FloatLimits::HARD.max_number_bytes,
                ..FloatLimits::default()
            })
            .unwrap_err()
            .kind,
        ErrorKind::WorkLimit
    );
}

fn assert_preserved(value: &Value<'_>, limits: EncodeLimits, expected: ErrorKind) {
    let mut output = [0xc7; 2048];
    assert_eq!(
        encode_into(value, &mut output, limits).unwrap_err().kind,
        expected
    );
    assert_eq!(output, [0xc7; 2048]);
}

#[test]
fn every_nonfinite_class_and_nested_late_failure_preserves_all_output() {
    for bits in [
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff0_0000_0000_0001,
        0xfff0_0000_0000_0001,
        0x7ff8_0000_0000_0000,
        0xfff8_0000_0000_0000,
        0x7fff_ffff_ffff_ffff,
        0xffff_ffff_ffff_ffff,
    ] {
        let invalid = Value::F64(f64::from_bits(bits));
        assert_preserved(&invalid, EncodeLimits::default(), ErrorKind::Number);
        let inner = [("late", invalid)];
        let outer = [
            Value::String("already visited é🦀"),
            Value::F64(42.0),
            Value::Object(&inner),
        ];
        assert_preserved(
            &Value::Array(&outer),
            EncodeLimits::default(),
            ErrorKind::Number,
        );
    }
    let value = Value::F64(f64::MAX);
    assert_preserved(
        &value,
        EncodeLimits {
            max_work: 1,
            ..EncodeLimits::default()
        },
        ErrorKind::WorkLimit,
    );
    assert_preserved(
        &value,
        EncodeLimits {
            max_output_bytes: 1,
            ..EncodeLimits::default()
        },
        ErrorKind::OutputLimit,
    );
    let mut too_small = [0xc7; 1];
    assert_eq!(
        encode_into(&value, &mut too_small, EncodeLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::OutputTooSmall
    );
    assert_eq!(too_small, [0xc7; 1]);
    let pair = [("same", Value::F64(1.0)), ("same", Value::F64(-0.0))];
    assert_preserved(
        &Value::Object(&pair),
        EncodeLimits::default(),
        ErrorKind::DuplicateKey,
    );
}

#[test]
fn mixed_nested_raw_unicode_and_integer_values_keep_exact_contracts_and_suffix() {
    let raw = parse("{\"text\":\"é🦀\",\"negative\":-0}".as_bytes());
    let array = [
        Value::F64(-0.0),
        Value::F64(f64::from_bits(1)),
        Value::Raw(raw.root()),
        Value::U64(u64::MAX),
        Value::I64(i64::MIN),
    ];
    let fields = [
        ("float", Value::F64(f64::MAX)),
        ("é🦀", Value::Array(&array)),
    ];
    let value = Value::Object(&fields);
    let mut output = [0xe2; 2048];
    let length = encode_into(&value, &mut output, EncodeLimits::default()).unwrap();
    assert_eq!(
        length,
        encoded_size(&value, EncodeLimits::default()).unwrap()
    );
    assert!(output[length..].iter().all(|byte| *byte == 0xe2));
    let doc = parse(&output[..length]);
    assert_eq!(
        doc.root()
            .field("float", 4096)
            .unwrap()
            .unwrap()
            .as_f64(FloatLimits::default())
            .unwrap(),
        f64::MAX
    );
    let mut cursor = doc
        .root()
        .field("é🦀", 4096)
        .unwrap()
        .unwrap()
        .elements()
        .unwrap();
    assert_eq!(
        cursor.read_f64(FloatLimits::default()).unwrap().to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(
        cursor.read_f64(FloatLimits::default()).unwrap().to_bits(),
        1
    );
    assert_eq!(cursor.read_raw().unwrap().raw(), raw.root().raw());
    assert_eq!(cursor.read_raw().unwrap().as_u64().unwrap(), u64::MAX);
    assert_eq!(cursor.read_raw().unwrap().as_i64().unwrap(), i64::MIN);
    assert!(cursor.peek().is_none());
    assert_preserved(
        &value,
        EncodeLimits {
            max_tokens: 3,
            ..EncodeLimits::default()
        },
        ErrorKind::TokenLimit,
    );
    assert_preserved(
        &value,
        EncodeLimits {
            max_depth: 1,
            ..EncodeLimits::default()
        },
        ErrorKind::DepthLimit,
    );
    assert_preserved(
        &value,
        EncodeLimits {
            max_string_bytes: 1,
            ..EncodeLimits::default()
        },
        ErrorKind::StringLimit,
    );
    assert_preserved(
        &value,
        EncodeLimits {
            max_total_string_bytes: 1,
            ..EncodeLimits::default()
        },
        ErrorKind::StringLimit,
    );
}
