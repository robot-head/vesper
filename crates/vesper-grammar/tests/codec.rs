use vesper_grammar::{
    ErrorKind,
    grammar::{
        Branch, Grammar, Limits, Rule,
        codec::{CodecLimits, Value, decode_exact, encode_into, encoded_len},
    },
};

fn grammar(rules: &[Rule]) -> Grammar<'_> {
    Grammar::validate(rules, 0, Limits::default()).unwrap()
}
fn bytes(grammar: &Grammar<'_>, value: &Value<'_>) -> Vec<u8> {
    let len = encoded_len(grammar, value, CodecLimits::default()).unwrap();
    let mut out = vec![0xa5; len + 5];
    assert_eq!(
        encode_into(grammar, value, &mut out, CodecLimits::default()).unwrap(),
        len
    );
    assert_eq!(&out[len..], &[0xa5; 5]);
    out.truncate(len);
    out
}
fn preserved(grammar: &Grammar<'_>, value: &Value<'_>, limits: CodecLimits, kind: ErrorKind) {
    let mut out = [0xa5; 128];
    assert_eq!(
        encode_into(grammar, value, &mut out, limits)
            .unwrap_err()
            .kind,
        kind
    );
    assert_eq!(out, [0xa5; 128]);
}

#[test]
fn independently_constructed_numbers_have_known_canonical_encodings() {
    let rules = [Rule::Uleb128];
    let grammar = grammar(&rules);
    for (number, expected) in [
        (0, vec![0]),
        (127, vec![127]),
        (128, vec![128, 1]),
        (624485, vec![0xe5, 0x8e, 0x26]),
        (
            u64::MAX,
            vec![255, 255, 255, 255, 255, 255, 255, 255, 255, 1],
        ),
    ] {
        let value = Value::Number(number);
        assert_eq!(bytes(&grammar, &value), expected);
        assert_eq!(
            decode_exact(&grammar, &expected, CodecLimits::default()).unwrap(),
            value
        );
    }
    for (input, kind) in [
        (vec![], ErrorKind::UnexpectedEof),
        (vec![128, 0], ErrorKind::NonCanonical),
        (vec![0, 1], ErrorKind::TrailingBytes),
        (vec![255; 10], ErrorKind::Overflow),
    ] {
        assert_eq!(
            decode_exact(&grammar, &input, CodecLimits::default())
                .unwrap_err()
                .kind,
            kind
        );
    }
}

#[test]
fn blobs_borrow_actual_payload_and_compare_contents_not_input_positions() {
    let rules = [Rule::Blob];
    let grammar = grammar(&rules);
    let input = [3, b'a', 0, 255];
    let value = decode_exact(&grammar, &input, CodecLimits::default()).unwrap();
    assert_eq!(value, Value::Bytes(&[b'a', 0, 255]));
    let Value::Bytes(payload) = value else {
        panic!("blob did not produce a borrowed payload")
    };
    assert_eq!(payload.as_ptr(), input[1..].as_ptr());
    assert_eq!(bytes(&grammar, &Value::Bytes(b"fresh")), b"\x05fresh");
    for input in [&b"\x80\x00"[..], &b"\x04abc"[..], &b"\x00x"[..]] {
        assert!(decode_exact(&grammar, input, CodecLimits::default()).is_err());
    }
}

#[test]
fn nested_frames_sequences_and_counts_are_measured_from_values() {
    let rules = [
        Rule::Frame(1),
        Rule::Sequence(vec![2, 3, 5]),
        Rule::Byte(b'A'),
        Rule::Counted(4),
        Rule::Uleb128,
        Rule::Blob,
    ];
    let grammar = grammar(&rules);
    let value = Value::Frame(Box::new(Value::Sequence(vec![
        Value::Unit,
        Value::Repeat(vec![Value::Number(1), Value::Number(128)]),
        Value::Bytes(b"xyz"),
    ])));
    let expected = b"\x09A\x02\x01\x80\x01\x03xyz";
    assert_eq!(bytes(&grammar, &value), expected);
    assert_eq!(
        decode_exact(&grammar, expected, CodecLimits::default()).unwrap(),
        value
    );
    for end in 0..expected.len() {
        assert!(decode_exact(&grammar, &expected[..end], CodecLimits::default()).is_err());
    }
    for size in 0..expected.len() {
        let mut out = vec![0xa5; size];
        assert_eq!(
            encode_into(&grammar, &value, &mut out, CodecLimits::default())
                .unwrap_err()
                .kind,
            ErrorKind::OutputTooSmall
        );
        assert!(out.iter().all(|b| *b == 0xa5));
    }
}

#[test]
fn frame_eof_never_borrows_or_consumes_outer_bytes() {
    let rules = [Rule::Frame(1), Rule::Uleb128];
    let grammar = grammar(&rules);
    for (input, kind, offset) in [
        (vec![1, 128, 1], ErrorKind::UnexpectedEof, 2),
        (vec![2, 1, 0], ErrorKind::TrailingBytes, 2),
        (vec![1, 7, 0], ErrorKind::TrailingBytes, 2),
    ] {
        let error = decode_exact(&grammar, &input, CodecLimits::default()).unwrap_err();
        assert_eq!((error.kind, error.offset), (kind, offset));
    }
    let nested = [Rule::Frame(1), Rule::Frame(2), Rule::Byte(9)];
    let grammar = self::grammar(&nested);
    let value = Value::Frame(Box::new(Value::Frame(Box::new(Value::Unit))));
    assert_eq!(bytes(&grammar, &value), [2, 1, 9]);
}

#[test]
fn nested_frame_prefix_width_changes_use_measured_payload_lengths() {
    fn reference_varint(mut n: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let digit = (n % 128) as u8;
            n /= 128;
            bytes.push(digit + if n == 0 { 0 } else { 128 });
            if n == 0 {
                return bytes;
            }
        }
    }
    let rules = [Rule::Frame(1), Rule::Frame(2), Rule::Blob];
    let grammar = grammar(&rules);
    for len in [
        0, 1, 125, 126, 127, 128, 129, 16380, 16381, 16382, 16383, 16384,
    ] {
        let payload: Vec<_> = (0..len).map(|i| (i % 251) as u8).collect();
        let value = Value::Frame(Box::new(Value::Frame(Box::new(Value::Bytes(&payload)))));
        // This oracle builds complete envelopes bottom-up using division and
        // remainder, independently of the codec's deferred prefix write plan.
        let mut expected = reference_varint(payload.len());
        expected.extend_from_slice(&payload);
        for _ in 0..2 {
            let mut framed = reference_varint(expected.len());
            framed.extend_from_slice(&expected);
            expected = framed;
        }
        assert_eq!(bytes(&grammar, &value), expected, "payload length {len}");
        assert_eq!(
            decode_exact(&grammar, &expected, CodecLimits::default()).unwrap(),
            value
        );
    }
}

#[test]
fn choice_keeps_branch_identity_and_uses_real_first_dispatch() {
    let rules = [
        Rule::Choice(vec![Branch { id: 17, rule: 1 }, Branch { id: 99, rule: 2 }]),
        Rule::Literal(b"foo".to_vec()),
        Rule::Literal(b"bar".to_vec()),
    ];
    let grammar = grammar(&rules);
    for (branch, expected) in [(17, b"foo"), (99, b"bar")] {
        let value = Value::Choice {
            branch,
            value: Box::new(Value::Unit),
        };
        assert_eq!(bytes(&grammar, &value), expected);
        assert_eq!(
            decode_exact(&grammar, expected, CodecLimits::default()).unwrap(),
            value
        );
    }
    for value in [
        Value::Choice {
            branch: 18,
            value: Box::new(Value::Unit),
        },
        Value::Choice {
            branch: 17,
            value: Box::new(Value::Number(0)),
        },
    ] {
        preserved(
            &grammar,
            &value,
            CodecLimits::default(),
            ErrorKind::Mismatch,
        );
    }
}

#[test]
fn fixed_and_counted_nullable_repetitions_visit_each_logical_value() {
    for (root, expected) in [
        (Rule::Repeat { rule: 1, count: 2 }, vec![]),
        (Rule::Counted(1), vec![2]),
    ] {
        let rules = [root, Rule::Empty];
        let grammar = grammar(&rules);
        let value = Value::Repeat(vec![Value::Unit, Value::Unit]);
        assert_eq!(bytes(&grammar, &value), expected);
        assert_eq!(
            decode_exact(&grammar, &expected, CodecLimits::default()).unwrap(),
            value
        );
        let limits = CodecLimits {
            max_values: 2,
            ..Default::default()
        };
        assert_eq!(
            decode_exact(&grammar, &expected, limits).unwrap_err().kind,
            ErrorKind::CountLimit
        );
        preserved(&grammar, &value, limits, ErrorKind::CountLimit);
    }
    let rules = [Rule::Counted(1), Rule::Empty];
    let grammar = grammar(&rules);
    assert_eq!(bytes(&grammar, &Value::Repeat(vec![])), [0]);
    assert_eq!(
        decode_exact(&grammar, &[0], CodecLimits::default()).unwrap(),
        Value::Repeat(vec![])
    );
    assert_eq!(
        decode_exact(&grammar, &[0x81, 0x08], CodecLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::CountLimit
    );
}

#[test]
fn wrong_variants_arity_and_late_child_failure_preserve_complete_output() {
    for (rules, value) in [
        (vec![Rule::Byte(1)], Value::Number(1)),
        (vec![Rule::Uleb128], Value::Unit),
        (vec![Rule::Blob], Value::Repeat(vec![])),
        (vec![Rule::Frame(1), Rule::Empty], Value::Unit),
        (
            vec![Rule::Sequence(vec![1]), Rule::Empty],
            Value::Sequence(vec![]),
        ),
        (
            vec![Rule::Repeat { rule: 1, count: 1 }, Rule::Empty],
            Value::Repeat(vec![]),
        ),
        (vec![Rule::Counted(1), Rule::Empty], Value::Sequence(vec![])),
        (
            vec![Rule::Sequence(vec![1, 2]), Rule::Byte(1), Rule::Uleb128],
            Value::Sequence(vec![Value::Unit, Value::Unit]),
        ),
    ] {
        let grammar = grammar(&rules);
        assert_eq!(
            encoded_len(&grammar, &value, CodecLimits::default())
                .unwrap_err()
                .kind,
            ErrorKind::Mismatch
        );
        preserved(
            &grammar,
            &value,
            CodecLimits::default(),
            ErrorKind::Mismatch,
        );
    }
}

#[test]
fn codec_limit_configuration_is_checked_before_all_operations() {
    let rules = [Rule::Empty];
    let grammar = grammar(&rules);
    let default = CodecLimits::default();
    for limits in [
        CodecLimits {
            max_values: 0,
            ..default
        },
        CodecLimits {
            max_metadata_bytes: 0,
            ..default
        },
        CodecLimits {
            max_output_bytes: 0,
            ..default
        },
        CodecLimits {
            max_depth: 0,
            ..default
        },
        CodecLimits {
            max_work: 0,
            ..default
        },
        CodecLimits {
            max_values: CodecLimits::HARD.max_values + 1,
            ..default
        },
        CodecLimits {
            max_metadata_bytes: CodecLimits::HARD.max_metadata_bytes + 1,
            ..default
        },
        CodecLimits {
            max_output_bytes: CodecLimits::HARD.max_output_bytes + 1,
            ..default
        },
        CodecLimits {
            max_depth: CodecLimits::HARD.max_depth + 1,
            ..default
        },
        CodecLimits {
            max_work: CodecLimits::HARD.max_work + 1,
            ..default
        },
    ] {
        assert_eq!(
            decode_exact(&grammar, &[], limits).unwrap_err().kind,
            ErrorKind::Rule
        );
        assert_eq!(
            encoded_len(&grammar, &Value::Unit, limits)
                .unwrap_err()
                .kind,
            ErrorKind::Rule
        );
        preserved(&grammar, &Value::Unit, limits, ErrorKind::Rule);
    }
    let tiny = CodecLimits {
        max_values: 1,
        max_metadata_bytes: 1,
        max_output_bytes: 1,
        max_depth: 1,
        max_work: 1,
    };
    assert_eq!(decode_exact(&grammar, &[], tiny).unwrap(), Value::Unit);
    assert_eq!(
        encode_into(&grammar, &Value::Unit, &mut [], tiny).unwrap(),
        0
    );
}

#[test]
fn value_storage_depth_and_work_are_prepaid_for_decoder_and_encoder() {
    let rules = [Rule::Frame(1), Rule::Uleb128];
    let grammar = grammar(&rules);
    let value = Value::Frame(Box::new(Value::Number(42)));
    for (limits, kind) in [
        (
            CodecLimits {
                max_values: 1,
                ..Default::default()
            },
            ErrorKind::CountLimit,
        ),
        (
            CodecLimits {
                max_metadata_bytes: 1,
                ..Default::default()
            },
            ErrorKind::AllocationLimit,
        ),
        (
            CodecLimits {
                max_depth: 1,
                ..Default::default()
            },
            ErrorKind::DepthLimit,
        ),
        (
            CodecLimits {
                max_work: 1,
                ..Default::default()
            },
            ErrorKind::WorkLimit,
        ),
    ] {
        assert_eq!(
            decode_exact(&grammar, &[1, 42], limits).unwrap_err().kind,
            kind
        );
        preserved(&grammar, &value, limits, kind);
    }
    let rules = [Rule::Sequence(vec![1; 10]), Rule::Empty];
    let grammar = self::grammar(&rules);
    assert_eq!(
        decode_exact(
            &grammar,
            &[],
            CodecLimits {
                max_metadata_bytes: 1,
                ..Default::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::AllocationLimit
    );
}

#[test]
fn applicable_grammar_limits_remain_in_force_without_capture_allocation() {
    let rules = [Rule::Uleb128];
    let grammar = Grammar::validate(
        &rules,
        0,
        Limits {
            max_input_bytes: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        decode_exact(&grammar, &[128, 1], CodecLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::InputLimit
    );
    preserved(
        &grammar,
        &Value::Number(128),
        CodecLimits::default(),
        ErrorKind::InputLimit,
    );

    let rules = [Rule::Frame(1), Rule::Uleb128];
    let grammar = Grammar::validate(
        &rules,
        0,
        Limits {
            max_frame_bytes: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        decode_exact(&grammar, &[2, 128, 1], CodecLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::PayloadLimit
    );
    preserved(
        &grammar,
        &Value::Frame(Box::new(Value::Number(128))),
        CodecLimits::default(),
        ErrorKind::PayloadLimit,
    );
    let grammar = Grammar::validate(
        &rules,
        0,
        Limits {
            max_events: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        decode_exact(&grammar, &[1, 0], CodecLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::CountLimit
    );

    let rules = [Rule::Byte(42)];
    let grammar = Grammar::validate(
        &rules,
        0,
        Limits {
            max_capture_bytes: 0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        grammar.parse_exact(&[42]).unwrap_err().kind,
        ErrorKind::AllocationLimit
    );
    assert_eq!(
        decode_exact(&grammar, &[42], CodecLimits::default()).unwrap(),
        Value::Unit
    );
    let grammar = Grammar::validate(
        &rules,
        0,
        Limits {
            max_work: 2,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        decode_exact(&grammar, &[42], CodecLimits::default()).unwrap(),
        Value::Unit
    );
    preserved(
        &grammar,
        &Value::Unit,
        CodecLimits::default(),
        ErrorKind::WorkLimit,
    );
}

#[test]
fn late_render_budget_refusal_and_output_cap_do_not_write_prefixes() {
    let rules = [Rule::Blob];
    let grammar = grammar(&rules);
    let value = Value::Bytes(&[1; 100]);
    preserved(
        &grammar,
        &value,
        CodecLimits {
            max_output_bytes: 100,
            ..Default::default()
        },
        ErrorKind::OutputLimit,
    );
    // The plan is small, but scratch storage is additional retained metadata.
    preserved(
        &grammar,
        &value,
        CodecLimits {
            max_metadata_bytes: 100,
            ..Default::default()
        },
        ErrorKind::AllocationLimit,
    );
    let measured = encoded_len(&grammar, &value, CodecLimits::default()).unwrap();
    assert_eq!(measured, 101);
    preserved(
        &grammar,
        &value,
        CodecLimits {
            max_work: 200,
            ..Default::default()
        },
        ErrorKind::WorkLimit,
    );
}

#[test]
fn directional_budgets_do_not_promote_decode_or_measurement_to_encode_approval() {
    let rules = [Rule::Uleb128];
    let grammar = grammar(&rules);
    let limits = CodecLimits {
        max_output_bytes: 1,
        ..Default::default()
    };
    let value = decode_exact(&grammar, &[128, 1], limits).unwrap();
    assert_eq!(value, Value::Number(128));
    assert_eq!(
        encoded_len(&grammar, &value, limits).unwrap_err().kind,
        ErrorKind::OutputLimit
    );
    preserved(&grammar, &value, limits, ErrorKind::OutputLimit);

    let rules = [Rule::Blob];
    let grammar = self::grammar(&rules);
    let value = Value::Bytes(&[1; 100]);
    let limits = CodecLimits {
        max_metadata_bytes: 100,
        ..Default::default()
    };
    assert_eq!(encoded_len(&grammar, &value, limits).unwrap(), 101);
    // Measuring a finite plan does not reserve the later scratch output.
    preserved(&grammar, &value, limits, ErrorKind::AllocationLimit);
    let limits = CodecLimits {
        max_work: 200,
        ..Default::default()
    };
    assert_eq!(encoded_len(&grammar, &value, limits).unwrap(), 101);
    preserved(&grammar, &value, limits, ErrorKind::WorkLimit);
}
