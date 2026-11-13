//! Independent wire oracles and semantic values, not a replay of captured input.
use vesper_grammar::{
    ErrorKind,
    grammar::{
        Branch, Grammar, Limits, Rule,
        codec::{CodecLimits, Value, decode_exact, encode_into, encoded_len},
    },
};

#[derive(Clone, Copy)]
enum Record<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}

// Division/remainder oracle deliberately does not call production framing or
// codec helpers. The explicit u128 arithmetic also rejects every >u64 input.
fn wire_number(mut number: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let quotient = number / 128;
        let digit = (number - quotient * 128) as u8;
        bytes.push(digit + if quotient == 0 { 0 } else { 128 });
        if quotient == 0 {
            return bytes;
        }
        number = quotient;
    }
}

fn read_number(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let start = *at;
    let mut result = 0_u128;
    let mut place = 1_u128;
    for _ in 0..10 {
        let byte = *bytes.get(*at)?;
        *at += 1;
        result = result.checked_add(u128::from(byte % 128) * place)?;
        if byte < 128 {
            if *at - start > 1 && byte == 0 {
                return None;
            }
            return u64::try_from(result).ok();
        }
        place *= 128;
    }
    None
}

fn exact_number(bytes: &[u8]) -> Option<u64> {
    let mut at = 0;
    let number = read_number(bytes, &mut at)?;
    (at == bytes.len()).then_some(number)
}

fn exact_numbers(bytes: &[u8], maximum: usize) -> Option<Vec<u64>> {
    let mut at = 0;
    let count = usize::try_from(read_number(bytes, &mut at)?).ok()?;
    if count > maximum {
        return None;
    }
    let mut numbers = Vec::new();
    for _ in 0..count {
        numbers.push(read_number(bytes, &mut at)?);
    }
    (at == bytes.len()).then_some(numbers)
}

fn protocol_rules() -> Vec<Rule> {
    vec![
        Rule::Sequence(vec![1, 2]),
        Rule::Repeat { rule: 3, count: 2 },
        Rule::Frame(4),
        Rule::Byte(b'V'),
        Rule::Counted(5),
        Rule::Choice(vec![Branch { id: 11, rule: 6 }, Branch { id: 22, rule: 9 }]),
        Rule::Sequence(vec![7, 8]),
        Rule::Byte(0x10),
        Rule::Uleb128,
        Rule::Sequence(vec![10, 11]),
        Rule::Byte(0x20),
        Rule::Blob,
    ]
}

fn packet<'a>(records: &[Record<'a>]) -> Value<'a> {
    let entries = records
        .iter()
        .map(|record| match record {
            Record::Number(n) => Value::Choice {
                branch: 11,
                value: Box::new(Value::Sequence(vec![Value::Unit, Value::Number(*n)])),
            },
            Record::Bytes(bytes) => Value::Choice {
                branch: 22,
                value: Box::new(Value::Sequence(vec![Value::Unit, Value::Bytes(bytes)])),
            },
        })
        .collect();
    Value::Sequence(vec![
        Value::Repeat(vec![Value::Unit, Value::Unit]),
        Value::Frame(Box::new(Value::Repeat(entries))),
    ])
}

fn wire_packet(records: &[Record<'_>]) -> Vec<u8> {
    let mut body = wire_number(records.len() as u64);
    for record in records {
        match record {
            Record::Number(n) => {
                body.push(0x10);
                body.extend(wire_number(*n));
            }
            Record::Bytes(bytes) => {
                body.push(0x20);
                body.extend(wire_number(bytes.len() as u64));
                body.extend_from_slice(bytes);
            }
        }
    }
    let mut packet = b"VV".to_vec();
    packet.extend(wire_number(body.len() as u64));
    packet.extend(body);
    packet
}

fn reencode(grammar: &Grammar<'_>, value: &Value<'_>) -> Vec<u8> {
    let length = encoded_len(grammar, value, CodecLimits::default()).unwrap();
    let mut output = vec![0xa5; length + 19];
    let written = encode_into(grammar, value, &mut output[3..], CodecLimits::default()).unwrap();
    assert_eq!(written, length);
    assert_eq!(&output[..3], &[0xa5; 3]);
    assert!(output[3 + written..].iter().all(|&b| b == 0xa5));
    output[3..3 + written].to_vec()
}

fn refused(grammar: &Grammar<'_>, value: &Value<'_>, limits: CodecLimits) -> ErrorKind {
    let mut output = [0x6d; 512];
    let error = encode_into(grammar, value, &mut output, limits).unwrap_err();
    assert_eq!(output, [0x6d; 512], "error changed caller storage: {error}");
    error.kind
}

fn same_limits(limits: &CodecLimits) -> CodecLimits {
    CodecLimits {
        max_values: limits.max_values,
        max_metadata_bytes: limits.max_metadata_bytes,
        max_output_bytes: limits.max_output_bytes,
        max_depth: limits.max_depth,
        max_work: limits.max_work,
    }
}

#[test]
fn constructed_tagged_batches_match_independent_wire_bytes_and_capture_parser() {
    let rules = protocol_rules();
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let all_bytes: Vec<_> = (0..=255).collect();
    let cases = [
        vec![],
        vec![Record::Number(42)],
        vec![Record::Bytes(b"")],
        vec![Record::Number(u64::MAX), Record::Bytes(&all_bytes)],
        vec![
            Record::Bytes(b"\0\xffVV\x10\x20"),
            Record::Number(0),
            Record::Number(127),
            Record::Number(128),
            Record::Bytes("UTF8 λ 😀".as_bytes()),
        ],
    ];
    for records in cases {
        let value = packet(&records);
        let expected = wire_packet(&records);
        assert_eq!(reencode(&grammar, &value), expected);
        let decoded = decode_exact(&grammar, &expected, CodecLimits::default()).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(reencode(&grammar, &decoded), expected);
        let captures = grammar.parse_exact(&expected).unwrap();
        assert_eq!(captures[0].range, 0..expected.len());
        assert_eq!(
            captures
                .iter()
                .find(|event| event.rule == 4)
                .unwrap()
                .number,
            Some(records.len() as u64)
        );
        for end in 0..expected.len() {
            assert!(decode_exact(&grammar, &expected[..end], CodecLimits::default()).is_err());
        }
        let mut trailing = expected;
        trailing.push(0);
        assert_eq!(
            decode_exact(&grammar, &trailing, CodecLimits::default())
                .unwrap_err()
                .kind,
            ErrorKind::TrailingBytes
        );
    }
}

#[test]
fn generated_lengths_and_numbers_cross_canonical_prefix_boundaries() {
    let rules = protocol_rules();
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let bytes: Vec<_> = (0..300).map(|i| (i * 71) as u8).collect();
    for length in [0, 1, 2, 125, 126, 127, 128, 129, 255, 256, 300] {
        let records = [
            Record::Bytes(&bytes[..length]),
            Record::Number(length as u64),
        ];
        let expected = wire_packet(&records);
        let value = packet(&records);
        assert_eq!(reencode(&grammar, &value), expected);
        assert_eq!(
            decode_exact(&grammar, &expected, CodecLimits::default()).unwrap(),
            value
        );
    }
    let mut state = 0x4b13_fde9_0896_623d_u64;
    for _ in 0..2048 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let records = [Record::Number(state)];
        let expected = wire_packet(&records);
        assert_eq!(reencode(&grammar, &packet(&records)), expected);
        assert_eq!(
            decode_exact(&grammar, &expected, CodecLimits::default()).unwrap(),
            packet(&records)
        );
    }
}

#[test]
fn blob_views_borrow_only_the_actual_input_payload_and_compare_by_contents() {
    let rules = [Rule::Blob];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let original = b"\x05hello";
    let Value::Bytes(payload) = decode_exact(&grammar, original, CodecLimits::default()).unwrap()
    else {
        panic!("blob did not produce semantic bytes")
    };
    assert_eq!(payload, b"hello");
    assert_eq!(payload.as_ptr(), original[1..].as_ptr());
    let separately_owned = b"hello".to_vec();
    assert_eq!(Value::Bytes(payload), Value::Bytes(&separately_owned));
    assert_eq!(
        reencode(&grammar, &Value::Bytes(&separately_owned)),
        original
    );
    // A transformed logical value must generate new bytes, not echo original.
    assert_eq!(
        reencode(&grammar, &Value::Bytes(b"goodbye")),
        b"\x07goodbye"
    );
}

#[test]
fn exhaustive_zero_one_and_two_byte_inputs_match_independent_numeric_oracles() {
    let number_rules = [Rule::Uleb128];
    let number = Grammar::validate(&number_rules, 0, Limits::default()).unwrap();
    let count_rules = [Rule::Counted(1), Rule::Uleb128];
    let counted = Grammar::validate(
        &count_rules,
        0,
        Limits {
            max_repetitions: 16,
            ..Limits::default()
        },
    )
    .unwrap();
    let mut accepted_number = 0;
    let mut accepted_counted = 0;
    let mut verify = |bytes: &[u8]| {
        let expected = exact_number(bytes);
        let decoded = decode_exact(&number, bytes, CodecLimits::default());
        match expected {
            Some(value) => {
                let decoded = decoded.unwrap();
                assert_eq!(decoded, Value::Number(value), "{bytes:02x?}");
                assert_eq!(reencode(&number, &decoded), bytes);
                accepted_number += 1;
            }
            None => assert!(decoded.is_err(), "accepted numeric alias {bytes:02x?}"),
        }
        let expected = exact_numbers(bytes, 16);
        let decoded = decode_exact(&counted, bytes, CodecLimits::default());
        match expected {
            Some(values) => {
                let value = Value::Repeat(values.into_iter().map(Value::Number).collect());
                let decoded = decoded.unwrap();
                assert_eq!(decoded, value, "{bytes:02x?}");
                assert_eq!(reencode(&counted, &decoded), bytes);
                accepted_counted += 1;
            }
            None => assert!(decoded.is_err(), "accepted counted alias {bytes:02x?}"),
        }
    };
    verify(&[]);
    for byte in 0..=255_u8 {
        verify(&[byte]);
    }
    for first in 0..=255_u8 {
        for second in 0..=255_u8 {
            verify(&[first, second]);
        }
    }
    assert_eq!(accepted_number, 16384);
    assert_eq!(accepted_counted, 129);
}

#[test]
fn alternate_number_count_blob_and_frame_encodings_are_not_accepted() {
    for (rules, bytes) in [
        (vec![Rule::Uleb128], vec![0x80, 0]),
        (vec![Rule::Uleb128], vec![0x81, 0]),
        (vec![Rule::Counted(1), Rule::Empty], vec![0x80, 0]),
        (vec![Rule::Blob], vec![0x81, 0, b'x']),
        (vec![Rule::Frame(1), Rule::Byte(b'x')], vec![0x81, 0, b'x']),
    ] {
        let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
        assert_eq!(
            decode_exact(&grammar, &bytes, CodecLimits::default())
                .unwrap_err()
                .kind,
            ErrorKind::NonCanonical,
            "{bytes:02x?}"
        );
    }
    let rules = [Rule::Counted(1), Rule::Empty];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    assert_eq!(
        decode_exact(&grammar, &[0xff; 10], CodecLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Overflow
    );
    assert!(matches!(
        decode_exact(&grammar, &wire_number(u64::MAX), CodecLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::CountLimit | ErrorKind::Overflow
    ));
}

#[test]
fn nested_frames_enforce_inner_eof_and_cannot_escape_the_enclosing_range() {
    let rules = [Rule::Frame(1), Rule::Frame(2), Rule::Uleb128];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let value = Value::Frame(Box::new(Value::Frame(Box::new(Value::Number(42)))));
    assert_eq!(reencode(&grammar, &value), [2, 1, 42]);
    assert_eq!(
        decode_exact(&grammar, &[2, 1, 42], CodecLimits::default()).unwrap(),
        value
    );
    for bytes in [
        &[3, 2, 42, 0][..],
        &[1, 2, 42][..],
        &[2, 1, 0x80][..],
        &[2, 1, 42, 0][..],
        &[3, 0x81, 0, 42][..],
        &[0][..],
    ] {
        assert!(decode_exact(&grammar, bytes, CodecLimits::default()).is_err());
    }
}

#[test]
fn zero_byte_fixed_and_counted_children_retain_distinct_semantic_counts() {
    let rules = [
        Rule::Sequence(vec![1, 2, 3]),
        Rule::Repeat { rule: 4, count: 0 },
        Rule::Repeat { rule: 4, count: 3 },
        Rule::Counted(4),
        Rule::Empty,
    ];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    for count in [0, 1, 2, 127, 128] {
        let value = Value::Sequence(vec![
            Value::Repeat(vec![]),
            Value::Repeat(vec![Value::Unit, Value::Unit, Value::Unit]),
            Value::Repeat((0..count).map(|_| Value::Unit).collect()),
        ]);
        let expected = wire_number(count as u64);
        assert_eq!(reencode(&grammar, &value), expected);
        assert_eq!(
            decode_exact(&grammar, &expected, CodecLimits::default()).unwrap(),
            value
        );
    }
}

#[test]
fn wrong_shapes_branch_ids_and_all_sequence_repeat_arities_preserve_output() {
    let rules = protocol_rules();
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let good = packet(&[Record::Number(42)]);
    assert_eq!(
        reencode(&grammar, &good),
        wire_packet(&[Record::Number(42)])
    );
    for value in [
        Value::Unit,
        Value::Number(42),
        Value::Sequence(vec![]),
        Value::Sequence(vec![Value::Repeat(vec![Value::Unit]), Value::Unit]),
        Value::Sequence(vec![
            Value::Repeat(vec![Value::Unit, Value::Unit, Value::Unit]),
            Value::Frame(Box::new(Value::Repeat(vec![]))),
        ]),
        Value::Sequence(vec![
            Value::Repeat(vec![Value::Unit, Value::Unit]),
            Value::Frame(Box::new(Value::Repeat(vec![Value::Choice {
                branch: 99,
                value: Box::new(Value::Unit),
            }]))),
        ]),
        Value::Sequence(vec![
            Value::Repeat(vec![Value::Unit, Value::Unit]),
            Value::Frame(Box::new(Value::Repeat(vec![Value::Choice {
                branch: 22,
                value: Box::new(Value::Sequence(vec![Value::Unit, Value::Number(42)])),
            }]))),
        ]),
    ] {
        assert!(encoded_len(&grammar, &value, CodecLimits::default()).is_err());
        refused(&grammar, &value, CodecLimits::default());
    }
}

#[test]
fn every_short_output_is_untouched_and_success_preserves_both_outside_regions() {
    let rules = protocol_rules();
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let value = packet(&[Record::Number(u64::MAX), Record::Bytes(b"payload")]);
    let expected = wire_packet(&[Record::Number(u64::MAX), Record::Bytes(b"payload")]);
    for capacity in 0..expected.len() {
        let mut output = vec![0x93; expected.len() + 13];
        assert_eq!(
            encode_into(
                &grammar,
                &value,
                &mut output[4..4 + capacity],
                CodecLimits::default(),
            )
            .unwrap_err()
            .kind,
            ErrorKind::OutputTooSmall
        );
        assert!(output.iter().all(|&byte| byte == 0x93));
    }
    assert_eq!(reencode(&grammar, &value), expected);
}

#[test]
fn every_codec_resource_field_has_an_actual_refusal_on_an_otherwise_valid_packet() {
    let rules = protocol_rules();
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let records = [Record::Number(42), Record::Bytes(b"actual payload")];
    let value = packet(&records);
    let bytes = wire_packet(&records);
    assert_eq!(reencode(&grammar, &value), bytes);
    let cases = [
        CodecLimits {
            max_values: 1,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_metadata_bytes: 1,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_output_bytes: bytes.len() - 1,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_depth: 1,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_work: 1,
            ..CodecLimits::default()
        },
    ];
    for (index, limits) in cases.iter().enumerate() {
        let kind = refused(&grammar, &value, same_limits(limits));
        assert!(matches!(
            kind,
            ErrorKind::CountLimit
                | ErrorKind::AllocationLimit
                | ErrorKind::OutputLimit
                | ErrorKind::DepthLimit
                | ErrorKind::WorkLimit
        ));
        assert!(encoded_len(&grammar, &value, same_limits(limits)).is_err());
        // Output byte limits constrain serialization, not an input-only decode.
        if index != 2 {
            assert!(decode_exact(&grammar, &bytes, same_limits(limits)).is_err());
        }
    }
}

#[test]
fn invalid_codec_settings_fail_before_any_output_change() {
    let rules = [Rule::Uleb128];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let value = Value::Number(42);
    let cases = [
        CodecLimits {
            max_values: 0,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_metadata_bytes: 0,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_output_bytes: 0,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_depth: 0,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_work: 0,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_values: 16385,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_metadata_bytes: 4 * 1024 * 1024 + 1,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_output_bytes: 4 * 1024 * 1024 + 1,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_depth: 65,
            ..CodecLimits::default()
        },
        CodecLimits {
            max_work: 64 * 1024 * 1024 + 1,
            ..CodecLimits::default()
        },
    ];
    for limits in &cases {
        assert_eq!(
            refused(&grammar, &value, same_limits(limits)),
            ErrorKind::Rule
        );
        assert_eq!(
            encoded_len(&grammar, &value, same_limits(limits))
                .unwrap_err()
                .kind,
            ErrorKind::Rule
        );
        assert_eq!(
            decode_exact(&grammar, &[42], same_limits(limits))
                .unwrap_err()
                .kind,
            ErrorKind::Rule
        );
    }
}

#[test]
fn actual_grammar_count_input_frame_and_visit_limits_also_constrain_the_codec() {
    let count_rules = [Rule::Counted(1), Rule::Empty];
    let grammar = Grammar::validate(
        &count_rules,
        0,
        Limits {
            max_repetitions: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    let too_many = Value::Repeat(vec![Value::Unit, Value::Unit, Value::Unit]);
    assert_eq!(
        refused(&grammar, &too_many, CodecLimits::default()),
        ErrorKind::CountLimit
    );
    assert_eq!(
        decode_exact(&grammar, &[3], CodecLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::CountLimit
    );
    let rules = protocol_rules();
    let records = [Record::Number(42)];
    let value = packet(&records);
    let bytes = wire_packet(&records);
    for limits in [
        Limits {
            max_input_bytes: bytes.len() - 1,
            ..Limits::default()
        },
        Limits {
            max_frame_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_events: 2,
            ..Limits::default()
        },
    ] {
        let grammar = Grammar::validate(&rules, 0, limits).unwrap();
        assert!(decode_exact(&grammar, &bytes, CodecLimits::default()).is_err());
        refused(&grammar, &value, CodecLimits::default());
        assert!(encoded_len(&grammar, &value, CodecLimits::default()).is_err());
    }
}

#[test]
fn shared_nullable_dag_visits_are_counted_and_depth_limits_precede_value_recursion() {
    let rules = [
        Rule::Sequence(vec![1, 1]),
        Rule::Frame(2),
        Rule::Sequence(vec![3, 3]),
        Rule::Counted(4),
        Rule::Empty,
    ];
    let value = Value::Sequence(
        [[1, 2], [3, 4]]
            .into_iter()
            .map(|counts| {
                Value::Frame(Box::new(Value::Sequence(
                    counts
                        .into_iter()
                        .map(|count| Value::Repeat((0..count).map(|_| Value::Unit).collect()))
                        .collect(),
                )))
            })
            .collect(),
    );
    let expected = [2, 1, 2, 2, 3, 4];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    assert_eq!(reencode(&grammar, &value), expected);
    assert_eq!(
        decode_exact(&grammar, &expected, CodecLimits::default()).unwrap(),
        value
    );
    let bounded = Grammar::validate(
        &rules,
        0,
        Limits {
            max_events: 10,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(decode_exact(&bounded, &expected, CodecLimits::default()).is_err());
    refused(&bounded, &value, CodecLimits::default());

    let mut deep_rules: Vec<_> = (1..=40).map(Rule::Frame).collect();
    deep_rules.push(Rule::Uleb128);
    let deep = Grammar::validate(
        &deep_rules,
        0,
        Limits {
            max_depth: 64,
            ..Limits::default()
        },
    )
    .unwrap();
    let mut value = Value::Number(42);
    let mut bytes = vec![42];
    for _ in 0..40 {
        value = Value::Frame(Box::new(value));
        let mut framed = wire_number(bytes.len() as u64);
        framed.extend(bytes);
        bytes = framed;
    }
    assert_eq!(
        refused(&deep, &value, CodecLimits::default()),
        ErrorKind::DepthLimit
    );
    assert_eq!(
        decode_exact(&deep, &bytes, CodecLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::DepthLimit
    );
    let limits = CodecLimits {
        max_depth: 64,
        ..CodecLimits::default()
    };
    let mut output = vec![0x54; bytes.len() + 9];
    let length = encode_into(&deep, &value, &mut output, same_limits(&limits)).unwrap();
    assert_eq!(&output[..length], bytes);
    assert!(output[length..].iter().all(|&b| b == 0x54));
    assert_eq!(decode_exact(&deep, &bytes, limits).unwrap(), value);
}
