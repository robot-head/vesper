use vesper_serde::{
    EncodeLimits, Error, ErrorKind, FloatLimits, IndexedJson, Kind, Limits, Value, encode_into,
};

fn parsed(input: &[u8]) -> IndexedJson<'_> {
    IndexedJson::parse(input, Limits::default()).unwrap()
}

#[test]
fn finite_decimal_edges_have_expected_binary64_bits() {
    for (input, bits) in [
        ("0", 0x0000_0000_0000_0000),
        ("-0", 0x8000_0000_0000_0000),
        ("0.0", 0x0000_0000_0000_0000),
        ("-0.000e+99", 0x8000_0000_0000_0000),
        ("1", 0x3ff0_0000_0000_0000),
        ("-1", 0xbff0_0000_0000_0000),
        ("1.5", 0x3ff8_0000_0000_0000),
        ("0.1", 0x3fb9_9999_9999_999a),
        ("1e3", 0x408f_4000_0000_0000),
        ("5e-324", 0x0000_0000_0000_0001),
        ("-5e-324", 0x8000_0000_0000_0001),
        ("4.9406564584124654e-324", 0x0000_0000_0000_0001),
        ("2.225073858507201e-308", 0x000f_ffff_ffff_ffff),
        ("2.2250738585072014e-308", 0x0010_0000_0000_0000),
        ("1.7976931348623157e308", 0x7fef_ffff_ffff_ffff),
        ("-1.7976931348623157e308", 0xffef_ffff_ffff_ffff),
    ] {
        let document = parsed(input.as_bytes());
        assert_eq!(document.root().kind(), Kind::Number);
        assert_eq!(
            document
                .root()
                .as_f64(FloatLimits::default())
                .unwrap()
                .to_bits(),
            bits,
            "{input}"
        );
        assert_eq!(document.root().raw(), input);
    }
}

#[test]
fn decimal_conversion_rounds_ties_and_underflow_without_losing_zero_sign() {
    for (input, expected) in [
        ("9007199254740993", 0x4340_0000_0000_0000),
        ("18446744073709551615", 0x43f0_0000_0000_0000),
        (
            "1.00000000000000011102230246251565404236316680908203125",
            0x3ff0_0000_0000_0000,
        ),
        (
            "1.00000000000000033306690738754696212708950042724609375",
            0x3ff0_0000_0000_0002,
        ),
        ("1e-324", 0x0000_0000_0000_0000),
        ("-1e-324", 0x8000_0000_0000_0000),
        ("1e-9999999999999999999999999999", 0x0000_0000_0000_0000),
        ("-1e-9999999999999999999999999999", 0x8000_0000_0000_0000),
    ] {
        let document = parsed(input.as_bytes());
        assert_eq!(
            document
                .root()
                .as_f64(FloatLimits::default())
                .unwrap()
                .to_bits(),
            expected,
            "{input}"
        );
    }
}

#[test]
fn overflow_is_a_number_conversion_error_at_the_original_utf8_byte_offset() {
    for number in [
        "1e309",
        "-1e309",
        "1.7976931348623159e308",
        "1e99999999999999999999999",
    ] {
        let input = format!("{{\"😀\": [null,  {number}]}}");
        let document = parsed(input.as_bytes());
        let mut cursor = document
            .root()
            .field("😀", 1000)
            .unwrap()
            .unwrap()
            .elements()
            .unwrap();
        cursor.skip().unwrap();
        let position = cursor.position();
        let original = cursor.peek().unwrap().raw();
        let expected = Error {
            kind: ErrorKind::Number,
            offset: input.find(number).unwrap(),
        };
        assert_eq!(
            cursor.peek().unwrap().as_f64(FloatLimits::default()),
            Err(expected)
        );
        assert_eq!(cursor.read_f64(FloatLimits::default()), Err(expected));
        assert_eq!(cursor.position(), position);
        assert_eq!(cursor.read_raw().unwrap().raw(), original);
        assert!(cursor.peek().is_none());
    }
}

#[test]
fn non_number_types_fail_without_advancing_and_can_still_be_read_raw() {
    for input in ["  null", "  true", "  \"1.25\"", "  []", "  {}"] {
        let document = parsed(input.as_bytes());
        let mut cursor = document.cursor();
        let initial = cursor.position();
        let expected = Error {
            kind: ErrorKind::Type,
            offset: 2,
        };
        assert_eq!(
            document.root().as_f64(FloatLimits::default()),
            Err(expected)
        );
        assert_eq!(cursor.read_f64(FloatLimits::default()), Err(expected));
        assert_eq!(cursor.position(), initial);
        assert_eq!(cursor.read_raw().unwrap().raw(), &input[2..]);
        assert_eq!(document.root().raw(), &input[2..]);
    }
}

#[test]
fn cursor_success_and_failure_keep_array_scope_and_exact_next_value() {
    let input = br#"{"values":[0,-0,1.25,{"skip":[1,2]},3.5],"outside":99}"#;
    let document = parsed(input);
    let mut cursor = document
        .root()
        .field("values", 1000)
        .unwrap()
        .unwrap()
        .elements()
        .unwrap();
    for bits in [0, 0x8000_0000_0000_0000, 0x3ff4_0000_0000_0000] {
        let before = cursor.position();
        assert_eq!(
            cursor.read_f64(FloatLimits::default()).unwrap().to_bits(),
            bits
        );
        assert!(cursor.position() > before);
    }
    let position = cursor.position();
    assert_eq!(
        cursor.read_f64(FloatLimits::default()).unwrap_err().kind,
        ErrorKind::Type
    );
    assert_eq!(cursor.position(), position);
    assert_eq!(cursor.read_raw().unwrap().raw(), r#"{"skip":[1,2]}"#);
    assert_eq!(
        cursor.read_f64(FloatLimits::default()).unwrap().to_bits(),
        0x400c_0000_0000_0000
    );
    let position = cursor.position();
    assert_eq!(
        cursor.read_f64(FloatLimits::default()),
        Err(Error {
            kind: ErrorKind::End,
            offset: input.len()
        })
    );
    assert_eq!(cursor.position(), position);
    assert!(cursor.peek().is_none());
    assert_eq!(
        document
            .root()
            .field("outside", 1000)
            .unwrap()
            .unwrap()
            .as_u64()
            .unwrap(),
        99
    );
}

#[test]
fn admission_boundaries_and_bad_settings_preserve_cursor_and_relocate_errors() {
    let input = b" [123.5] ";
    let document = parsed(input);
    let mut cursor = document.root().elements().unwrap();
    let before = cursor.position();
    let node = cursor.peek().unwrap();
    let work = node.raw().len() * node.raw().len() + 512;
    let exact = FloatLimits {
        max_number_bytes: 5,
        max_work: work,
    };
    for (limits, kind) in [
        (
            FloatLimits {
                max_number_bytes: 4,
                ..exact
            },
            ErrorKind::InputLimit,
        ),
        (
            FloatLimits {
                max_work: work - 1,
                ..exact
            },
            ErrorKind::WorkLimit,
        ),
        (
            FloatLimits {
                max_number_bytes: 0,
                ..exact
            },
            ErrorKind::InputLimit,
        ),
        (
            FloatLimits {
                max_number_bytes: FloatLimits::HARD.max_number_bytes + 1,
                ..exact
            },
            ErrorKind::InputLimit,
        ),
        (
            FloatLimits {
                max_work: 0,
                ..exact
            },
            ErrorKind::WorkLimit,
        ),
        (
            FloatLimits {
                max_work: FloatLimits::HARD.max_work + 1,
                ..exact
            },
            ErrorKind::WorkLimit,
        ),
    ] {
        let expected = Error { kind, offset: 2 };
        assert_eq!(node.as_f64(limits), Err(expected));
        assert_eq!(cursor.read_f64(limits), Err(expected));
        assert_eq!(cursor.position(), before);
        assert_eq!(cursor.peek().unwrap().raw(), "123.5");
    }
    assert_eq!(cursor.read_f64(exact).unwrap(), 123.5);
    assert!(cursor.peek().is_none());
}

#[test]
fn larger_valid_decimal_is_refused_before_conversion_and_retried_with_admission() {
    let number = format!("-0.{}1", "0".repeat(8188));
    assert_eq!(number.len(), 8192);
    let input = format!("[{number}]");
    let document = parsed(input.as_bytes());
    let mut cursor = document.root().elements().unwrap();
    let before = cursor.position();
    assert_eq!(
        cursor.read_f64(FloatLimits::default()),
        Err(Error {
            kind: ErrorKind::InputLimit,
            offset: 1
        })
    );
    let bytes_allowed = FloatLimits {
        max_number_bytes: number.len(),
        ..Default::default()
    };
    assert_eq!(
        cursor.read_f64(bytes_allowed),
        Err(Error {
            kind: ErrorKind::WorkLimit,
            offset: 1
        })
    );
    assert_eq!(cursor.position(), before);
    let enough = FloatLimits {
        max_number_bytes: number.len(),
        max_work: number.len() * number.len() + 512,
    };
    assert_eq!(
        cursor.read_f64(enough).unwrap().to_bits(),
        0x8000_0000_0000_0000
    );
    assert!(cursor.peek().is_none());
    assert_eq!(document.root().raw(), input);
}

#[test]
fn huge_numbers_remain_valid_raw_json_when_binary64_conversion_is_refused() {
    let number = format!("1{}", "0".repeat(5000));
    let input = format!("{{\"number\":{number},\"tail\":true}}");
    let document = parsed(input.as_bytes());
    let node = document.root().field("number", 1000).unwrap().unwrap();
    assert_eq!(node.kind(), Kind::Number);
    assert_eq!(
        node.as_f64(FloatLimits::default()).unwrap_err().kind,
        ErrorKind::InputLimit
    );
    assert_eq!(
        node.as_f64(FloatLimits {
            max_number_bytes: 8192,
            max_work: FloatLimits::HARD.max_work
        })
        .unwrap_err()
        .kind,
        ErrorKind::Number
    );
    let mut output = vec![0xa5; number.len() + 12];
    let count = encode_into(&Value::Raw(node), &mut output, EncodeLimits::default()).unwrap();
    assert_eq!(&output[..count], number.as_bytes());
    assert_eq!(output[count..], [0xa5; 12]);
    assert_eq!(
        node.raw().as_ptr(),
        input.as_ptr().wrapping_add(input.find(&number).unwrap())
    );
    assert_eq!(
        document
            .root()
            .field("tail", 1000)
            .unwrap()
            .unwrap()
            .as_bool(),
        Ok(true)
    );
}

#[test]
fn integer_queries_and_following_borrowed_strings_keep_existing_contracts() {
    let input = r#"[18446744073709551615,-9223372036854775808,1e0,"héllo 😀"]"#;
    let document = parsed(input.as_bytes());
    let mut cursor = document.root().elements().unwrap();
    let first = cursor.peek().unwrap();
    assert_eq!(first.as_u64(), Ok(u64::MAX));
    assert_eq!(first.as_i64().unwrap_err().kind, ErrorKind::Number);
    assert_eq!(
        cursor.read_f64(FloatLimits::default()).unwrap().to_bits(),
        0x43f0_0000_0000_0000
    );
    let second = cursor.peek().unwrap();
    assert_eq!(second.as_i64(), Ok(i64::MIN));
    assert_eq!(second.as_u64().unwrap_err().kind, ErrorKind::Number);
    cursor.read_f64(FloatLimits::default()).unwrap();
    let exponent = cursor.peek().unwrap();
    assert_eq!(exponent.as_u64().unwrap_err().kind, ErrorKind::Number);
    assert_eq!(exponent.as_i64().unwrap_err().kind, ErrorKind::Number);
    assert_eq!(cursor.read_f64(FloatLimits::default()).unwrap(), 1.0);
    let mut scratch = [0xab; 4];
    let string = cursor.read_str(&mut scratch).unwrap();
    assert_eq!(string.as_str(), "héllo 😀");
    assert_eq!(
        string.as_str().as_ptr(),
        input.as_ptr().wrapping_add(input.find("héllo").unwrap())
    );
    assert_eq!(scratch, [0xab; 4]);
    assert!(cursor.peek().is_none());
}

#[test]
fn bad_limits_do_not_replace_existing_type_or_end_errors() {
    let invalid = FloatLimits {
        max_number_bytes: 0,
        max_work: 0,
    };
    let document = parsed(b"  true");
    assert_eq!(
        document.root().as_f64(invalid),
        Err(Error {
            kind: ErrorKind::Type,
            offset: 2
        })
    );
    let empty = parsed(b" [ ] ");
    let mut cursor = empty.root().elements().unwrap();
    let before = cursor.position();
    assert_eq!(
        cursor.read_f64(invalid),
        Err(Error {
            kind: ErrorKind::End,
            offset: 5
        })
    );
    assert_eq!(cursor.position(), before);
}

#[test]
fn complete_json_validation_still_precedes_all_numeric_views() {
    for input in [
        br#"{"known":1.25,"unknown":[1e+]}"#.as_slice(),
        br#"{"known":1.25,"unknown":{"x":1,"\u0078":2}}"#,
        br#"{"known":1.25,"unknown":"\uD800"}"#,
        b"NaN",
        b"Infinity",
        b"-Infinity",
        b"+1",
        b"01",
        b"1.",
        b"1e+",
        b"0x1",
    ] {
        assert!(
            IndexedJson::parse(input, Limits::default()).is_err(),
            "{input:?}"
        );
    }
}
