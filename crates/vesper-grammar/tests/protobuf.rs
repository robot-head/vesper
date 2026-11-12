use vesper_grammar::protobuf::{
    ErrorKind, FieldSchema as F, FieldValue as V, Kind as K, Limits, Message, MessageSchema as M,
    PROFILE, Schema, Value,
};
fn f(number: u32, kind: K, repeated: bool) -> F {
    F {
        number,
        kind,
        repeated,
    }
}
fn v(number: u32, value: Value<'_>) -> V<'_> {
    V { number, value }
}
fn spec() -> [M; 2] {
    [
        M {
            fields: vec![
                f(1, K::U64, false),
                f(2, K::String, false),
                f(3, K::Message(1), false),
                f(4, K::U32, true),
            ],
        },
        M {
            fields: vec![f(1, K::U64, false)],
        },
    ]
}
fn hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}
fn encoded(schema: &Schema<'_>, message: &Message<'_>) -> Vec<u8> {
    let mut out = vec![0; schema.encoded_size(message).unwrap()];
    schema.encode_into(message, &mut out).unwrap();
    out
}
#[test]
fn standard_protobuf_encoding_guide_fixed_wire_examples() {
    // https://protobuf.dev/programming-guides/encoding/ : varint150,
    // string "testing", submessage {a:150}; packed [3,270,86942] below is
    // independently calculated using the same wire convention for u32 field4.
    let defs = spec();
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    let cases = [
        (
            hex("08 96 01"),
            Message {
                fields: vec![v(1, Value::U64(150))],
            },
        ),
        (
            hex("12 07 74 65 73 74 69 6e 67"),
            Message {
                fields: vec![v(2, Value::String("testing"))],
            },
        ),
        (
            hex("1a 03 08 96 01"),
            Message {
                fields: vec![v(
                    3,
                    Value::Message(Message {
                        fields: vec![v(1, Value::U64(150))],
                    }),
                )],
            },
        ),
        (
            hex("22 06 03 8e 02 9e a7 05"),
            Message {
                fields: vec![
                    v(4, Value::U32(3)),
                    v(4, Value::U32(270)),
                    v(4, Value::U32(86942)),
                ],
            },
        ),
    ];
    for (bytes, value) in cases {
        assert_eq!(schema.parse_exact(&bytes).unwrap(), value);
        assert_eq!(encoded(&schema, &value), bytes);
    }
    let defs = [M {
        fields: vec![f(5, K::U32, true)],
    }];
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    let current_guide = Message {
        fields: vec![
            v(5, Value::U32(1)),
            v(5, Value::U32(2)),
            v(5, Value::U32(3)),
        ],
    };
    assert_eq!(encoded(&schema, &current_guide), hex("2a 03 01 02 03"));
    assert_eq!(
        schema.parse_exact(&hex("2a 03 01 02 03")).unwrap(),
        current_guide
    );
    assert_eq!(PROFILE, "vesper.protobuf.explicit-presence/1");
}
#[test]
fn singular_presence_and_defaults_are_actual_distinct_logical_values() {
    let defs = [
        M {
            fields: vec![
                f(1, K::U64, false),
                f(2, K::Bool, false),
                f(3, K::Bytes, false),
                f(4, K::Message(1), false),
            ],
        },
        M { fields: vec![] },
    ];
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    let absent = schema.parse_exact(&[]).unwrap();
    for bytes in [
        &b"\x08\x00"[..],
        &b"\x10\x00"[..],
        &b"\x1a\x00"[..],
        &b"\x22\x00"[..],
    ] {
        let present = schema.parse_exact(bytes).unwrap();
        assert_ne!(present, absent);
        assert_eq!(encoded(&schema, &present), bytes);
    }
    assert_eq!(encoded(&schema, &absent), []);
}
#[test]
fn all_supported_numeric_scalar_boundaries_roundtrip_and_zigzag_vectors() {
    let defs = [M {
        fields: vec![
            f(1, K::U32, false),
            f(2, K::U64, false),
            f(3, K::Sint32, false),
            f(4, K::Sint64, false),
            f(5, K::Bool, false),
            f(6, K::Fixed32, false),
            f(7, K::Fixed64, false),
        ],
    }];
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    let value = Message {
        fields: vec![
            v(1, Value::U32(u32::MAX)),
            v(2, Value::U64(u64::MAX)),
            v(3, Value::Sint32(i32::MIN)),
            v(4, Value::Sint64(i64::MIN)),
            v(5, Value::Bool(true)),
            v(6, Value::Fixed32(0x01020304)),
            v(7, Value::Fixed64(0x0102030405060708)),
        ],
    };
    let bytes = encoded(&schema, &value);
    assert_eq!(schema.parse_exact(&bytes).unwrap(), value);
    assert!(bytes.ends_with(&hex("35 04 03 02 01 39 08 07 06 05 04 03 02 01")));
    for (value, wire) in [(-1, 1), (0, 0), (1, 2), (-2, 3), (2, 4)] {
        let value = Message {
            fields: vec![v(3, Value::Sint32(value))],
        };
        assert_eq!(encoded(&schema, &value), [0x18, wire]);
    }
}
#[test]
fn repeated_scalars_packed_policy_and_non_numeric_occurrence_order() {
    let defs = [
        M {
            fields: vec![
                f(1, K::Bool, true),
                f(2, K::Fixed32, true),
                f(3, K::String, true),
                f(4, K::Message(1), true),
            ],
        },
        M { fields: vec![] },
    ];
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    let value = Message {
        fields: vec![
            v(1, Value::Bool(false)),
            v(1, Value::Bool(true)),
            v(1, Value::Bool(false)),
            v(2, Value::Fixed32(1)),
            v(2, Value::Fixed32(2)),
            v(3, Value::String("")),
            v(3, Value::String("é🦀")),
            v(4, Value::Message(Message { fields: vec![] })),
            v(4, Value::Message(Message { fields: vec![] })),
        ],
    };
    let bytes = encoded(&schema, &value);
    assert_eq!(&bytes[..5], [0x0a, 3, 0, 1, 0]);
    assert_eq!(schema.parse_exact(&bytes).unwrap(), value);
    for (bytes, kind) in [
        (hex("08 01"), ErrorKind::WireType),
        (hex("0a 00"), ErrorKind::NonCanonical),
        (hex("0a 01 01 0a 01 00"), ErrorKind::Duplicate),
        (hex("12 03 00 00 00"), ErrorKind::Truncated),
    ] {
        assert_eq!(schema.parse_exact(&bytes).unwrap_err().kind, kind);
    }
}
#[test]
fn legal_field_numbers_wire_types_unknowns_order_and_duplicates() {
    let defs = spec();
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    for (bytes, kind) in [
        (hex("00"), ErrorKind::FieldNumber),
        (hex("08 01 08 02"), ErrorKind::Duplicate),
        (hex("12 00 08 01"), ErrorKind::Order),
        (hex("28 01"), ErrorKind::UnknownField),
        (hex("0b"), ErrorKind::WireType),
        (hex("0c"), ErrorKind::WireType),
        (hex("0e"), ErrorKind::WireType),
        (hex("0f"), ErrorKind::WireType),
        (hex("0a 00"), ErrorKind::WireType),
        (hex("88 00 01"), ErrorKind::NonCanonical),
        (hex("08 81 00"), ErrorKind::NonCanonical),
        (hex("12 80 00"), ErrorKind::NonCanonical),
        (hex("12 01 ff"), ErrorKind::Utf8),
        (hex("1a 02 00 00"), ErrorKind::FieldNumber),
        (hex("08 01 00"), ErrorKind::FieldNumber),
    ] {
        assert_eq!(
            schema.parse_exact(&bytes).unwrap_err().kind,
            kind,
            "{bytes:?}"
        );
    }
    let maximum = [M {
        fields: vec![f((1 << 29) - 1, K::U32, false)],
    }];
    let max = Schema::validate(&maximum, 0, Limits::default()).unwrap();
    assert_eq!(
        encoded(
            &max,
            &Message {
                fields: vec![v((1 << 29) - 1, Value::U32(1))]
            }
        ),
        hex("f8 ff ff ff 0f 01")
    );
}
#[test]
fn numeric_and_length_overflow_boolean_alternates_and_exact_bounds() {
    let defs = [M {
        fields: vec![
            f(1, K::U32, false),
            f(2, K::Sint32, false),
            f(3, K::Bool, false),
            f(4, K::Bytes, false),
        ],
    }];
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    for bytes in [
        hex("08 80 80 80 80 10"),
        hex("10 80 80 80 80 10"),
        hex("08 ff ff ff ff ff ff ff ff ff 02"),
        hex("22 ff ff ff ff ff ff ff ff ff 01"),
    ] {
        assert!(schema.parse_exact(&bytes).is_err());
    }
    assert_eq!(
        schema.parse_exact(&hex("18 02")).unwrap_err().kind,
        ErrorKind::NonCanonical
    );
    assert_eq!(
        schema.parse_exact(&hex("22 01")).unwrap_err().kind,
        ErrorKind::Truncated
    );
}
#[test]
fn every_truncation_is_either_rejected_or_a_distinct_complete_optional_message() {
    let defs = spec();
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    let bytes = hex("08 96 01 12 07 74 65 73 74 69 6e 67 1a 03 08 96 01 22 06 03 8e 02 9e a7 05");
    let valid_boundaries = [0, 3, 12, 17, 25];
    assert_eq!(bytes.len(), 25);
    for end in 0..=bytes.len() {
        match schema.parse_exact(&bytes[..end]) {
            Ok(value) => {
                assert!(valid_boundaries.contains(&end), "{end}");
                assert_eq!(encoded(&schema, &value), bytes[..end]);
            }
            Err(_) => assert!(!valid_boundaries.contains(&end), "{end}"),
        }
    }
    for split in 0..=bytes.len() {
        let joined = [&bytes[..split], &bytes[split..]].concat();
        assert_eq!(
            encoded(&schema, &schema.parse_exact(&joined).unwrap()),
            bytes
        );
    }
}
#[test]
fn borrowed_utf8_bytes_and_nested_values_keep_original_payload_slices() {
    let defs = [M {
        fields: vec![f(1, K::String, false), f(2, K::Bytes, false)],
    }];
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    let bytes = hex("0a 02 c3 a9 12 03 00 ff 01");
    let value = schema.parse_exact(&bytes).unwrap();
    let Value::String(text) = value.fields[0].value else {
        panic!()
    };
    let Value::Bytes(raw) = value.fields[1].value else {
        panic!()
    };
    assert_eq!(text.as_ptr(), bytes[2..].as_ptr());
    assert_eq!(raw.as_ptr(), bytes[6..].as_ptr());
    assert_eq!(encoded(&schema, &value), bytes);
}
#[test]
fn schema_rejects_cycles_reused_dag_depth_bad_ids_and_bad_fields_without_cloning() {
    for (defs, kind) in [
        (
            vec![M {
                fields: vec![f(0, K::U64, false)],
            }],
            ErrorKind::FieldNumber,
        ),
        (
            vec![M {
                fields: vec![f(19_000, K::U64, false)],
            }],
            ErrorKind::FieldNumber,
        ),
        (
            vec![M {
                fields: vec![f(1 << 29, K::U64, false)],
            }],
            ErrorKind::FieldNumber,
        ),
        (
            vec![M {
                fields: vec![f(2, K::U64, false), f(1, K::U64, false)],
            }],
            ErrorKind::Order,
        ),
        (
            vec![M {
                fields: vec![f(1, K::U64, false), f(1, K::U64, false)],
            }],
            ErrorKind::Duplicate,
        ),
        (
            vec![M {
                fields: vec![f(1, K::Message(99), false)],
            }],
            ErrorKind::Schema,
        ),
        (
            vec![M {
                fields: vec![f(1, K::Message(0), false)],
            }],
            ErrorKind::RecursiveSchema,
        ),
        (
            vec![
                M { fields: vec![] },
                M {
                    fields: vec![f(1, K::Message(1), false)],
                },
            ],
            ErrorKind::RecursiveSchema,
        ),
    ] {
        assert_eq!(
            Schema::validate(&defs, 0, Limits::default())
                .unwrap_err()
                .kind,
            kind
        );
    }
    assert_eq!(
        Schema::validate(&[], 0, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Schema
    );
    let defs = [
        M { fields: vec![] },
        M {
            fields: vec![f(1, K::Message(0), false)],
        },
        M {
            fields: vec![f(1, K::Message(1), false)],
        },
    ];
    assert_eq!(
        Schema::validate(
            &defs,
            2,
            Limits {
                max_depth: 2,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::DepthLimit
    );
}
#[test]
fn all_schema_budgets_apply_before_any_recursive_schema_copy_or_data_access() {
    let defs = spec();
    let base = Limits::default();
    for (limits, kind) in [
        (
            Limits {
                max_schema_messages: 1,
                ..base
            },
            ErrorKind::Schema,
        ),
        (
            Limits {
                max_schema_fields: 4,
                ..base
            },
            ErrorKind::Schema,
        ),
        (
            Limits {
                max_fields_per_message: 3,
                ..base
            },
            ErrorKind::Schema,
        ),
        (
            Limits {
                max_metadata_bytes: 0,
                ..base
            },
            ErrorKind::MetadataLimit,
        ),
        (
            Limits {
                max_work: 0,
                ..base
            },
            ErrorKind::WorkLimit,
        ),
        (
            Limits {
                max_depth: 0,
                ..base
            },
            ErrorKind::DepthLimit,
        ),
        (
            Limits {
                max_depth: 65,
                ..base
            },
            ErrorKind::DepthLimit,
        ),
    ] {
        assert_eq!(Schema::validate(&defs, 0, limits).unwrap_err().kind, kind);
    }
}
#[test]
fn typed_direct_api_order_type_unknown_and_node_failures_preserve_output_and_values() {
    let defs = spec();
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    let invalid = [
        (
            Message {
                fields: vec![v(1, Value::Bool(true))],
            },
            ErrorKind::TypeMismatch,
        ),
        (
            Message {
                fields: vec![v(9, Value::U64(0))],
            },
            ErrorKind::UnknownField,
        ),
        (
            Message {
                fields: vec![v(1, Value::U64(0)), v(1, Value::U64(1))],
            },
            ErrorKind::Duplicate,
        ),
        (
            Message {
                fields: vec![v(2, Value::String("")), v(1, Value::U64(1))],
            },
            ErrorKind::Order,
        ),
        (
            Message {
                fields: vec![v(0, Value::U64(0))],
            },
            ErrorKind::FieldNumber,
        ),
        (
            Message {
                fields: vec![v(4, Value::U64(1))],
            },
            ErrorKind::TypeMismatch,
        ),
    ];
    for (message, kind) in invalid {
        let mut out = [0xab; 100];
        assert_eq!(
            schema.encode_into(&message, &mut out).unwrap_err().kind,
            kind
        );
        assert_eq!(out, [0xab; 100]);
        assert!(!message.fields.is_empty());
    }
}
#[test]
fn every_insufficient_output_and_data_budget_is_transactional_and_retryable() {
    let defs = spec();
    let base = Limits::default();
    let message = Message {
        fields: vec![v(1, Value::U64(150)), v(2, Value::String("testing"))],
    };
    let schema = Schema::validate(&defs, 0, base).unwrap();
    let bytes = encoded(&schema, &message);
    for len in 0..bytes.len() {
        let mut out = vec![0xab; len];
        assert_eq!(
            schema.encode_into(&message, &mut out).unwrap_err().kind,
            ErrorKind::OutputTooSmall
        );
        assert!(out.iter().all(|b| *b == 0xab));
    }
    let mut out = vec![0xab; bytes.len() + 9];
    let len = schema.encode_into(&message, &mut out).unwrap();
    assert_eq!(out[..len], bytes);
    assert_eq!(out[len..], [0xab; 9]);
    for (limits, encode_error, parse_error) in [
        (
            Limits {
                max_nodes: 2,
                ..base
            },
            Some(ErrorKind::NodeLimit),
            Some(ErrorKind::NodeLimit),
        ),
        (
            Limits {
                max_payload_bytes: 6,
                ..base
            },
            Some(ErrorKind::PayloadLimit),
            Some(ErrorKind::PayloadLimit),
        ),
        (
            Limits {
                max_output_bytes: 1,
                ..base
            },
            Some(ErrorKind::OutputLimit),
            None,
        ),
        (
            Limits {
                max_input_bytes: 1,
                ..base
            },
            None,
            Some(ErrorKind::InputLimit),
        ),
        (
            Limits {
                max_work: 20,
                ..base
            },
            Some(ErrorKind::WorkLimit),
            Some(ErrorKind::WorkLimit),
        ),
        (
            Limits {
                max_metadata_bytes: 64,
                ..base
            },
            None,
            Some(ErrorKind::MetadataLimit),
        ),
    ] {
        let schema = Schema::validate(&defs, 0, limits).unwrap();
        let mut out = [0xab; 100];
        if let Some(kind) = encode_error {
            assert_eq!(
                schema.encode_into(&message, &mut out).unwrap_err().kind,
                kind
            );
            assert_eq!(out, [0xab; 100]);
        }
        if let Some(kind) = parse_error {
            assert_eq!(schema.parse_exact(&bytes).unwrap_err().kind, kind);
        }
    }
}
// Independent division/remainder oracle, preserving explicit presence.
fn leb(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n % 128) as u8;
        n /= 128;
        out.push(byte + if n != 0 { 128 } else { 0 });
        if n == 0 {
            return out;
        }
    }
}
#[test]
fn independent_scalar_reference_and_accepted_byte_identity_corpus() {
    let defs = [M {
        fields: vec![f(1, K::U64, false)],
    }];
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    let mut seed = 0x123abc_u64;
    for _ in 0..10_000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut bytes = vec![8];
        bytes.extend(leb(seed));
        let value = schema.parse_exact(&bytes).unwrap();
        assert_eq!(value.fields[0].value, Value::U64(seed));
        assert_eq!(encoded(&schema, &value), bytes);
    }
    for first in 0..=255 {
        for second in 0..=255 {
            let bytes = [8, first, second];
            if let Ok(value) = schema.parse_exact(&bytes) {
                assert_eq!(encoded(&schema, &value), bytes);
            }
        }
    }
}
#[test]
fn metadata_growth_and_repeated_value_aliases_cannot_bypass_work_node_limits() {
    let defs = [M {
        fields: vec![f(1, K::U32, true)],
    }];
    let base = Limits::default();
    let schema = Schema::validate(&defs, 0, base).unwrap();
    let fields = (0..100).map(|n| v(1, Value::U32(n))).collect();
    let message = Message { fields };
    let bytes = encoded(&schema, &message);
    assert_eq!(schema.parse_exact(&bytes).unwrap(), message);
    let bounded = Schema::validate(
        &defs,
        0,
        Limits {
            max_nodes: 50,
            ..base
        },
    )
    .unwrap();
    assert_eq!(
        bounded.parse_exact(&bytes).unwrap_err().kind,
        ErrorKind::NodeLimit
    );
    let mut out = [0xab; 256];
    assert_eq!(
        bounded.encode_into(&message, &mut out).unwrap_err().kind,
        ErrorKind::NodeLimit
    );
    assert_eq!(out, [0xab; 256]);
    let tight = Schema::validate(
        &defs,
        0,
        Limits {
            max_metadata_bytes: 512,
            ..base
        },
    )
    .unwrap();
    assert_eq!(
        tight.parse_exact(&bytes).unwrap_err().kind,
        ErrorKind::MetadataLimit
    );
}
#[test]
fn many_nested_values_follow_schema_depth_and_exact_inner_bytes() {
    let defs: Vec<_> = (0..16)
        .map(|i| M {
            fields: if i == 15 {
                vec![f(1, K::Bytes, false)]
            } else {
                vec![f(1, K::Message(i + 1), false)]
            },
        })
        .collect();
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    let mut message = Message {
        fields: vec![v(1, Value::Bytes(b"payload"))],
    };
    for _ in 0..15 {
        message = Message {
            fields: vec![v(1, Value::Message(message))],
        };
    }
    let bytes = encoded(&schema, &message);
    assert_eq!(schema.parse_exact(&bytes).unwrap(), message);
    assert_eq!(
        Schema::validate(
            &defs,
            0,
            Limits {
                max_depth: 15,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::DepthLimit
    );
}

#[test]
fn every_numeric_kind_has_one_canonical_packed_encoding() {
    let cases = [
        (
            K::U32,
            Message {
                fields: vec![v(1, Value::U32(0)), v(1, Value::U32(u32::MAX))],
            },
        ),
        (
            K::U64,
            Message {
                fields: vec![v(1, Value::U64(0)), v(1, Value::U64(u64::MAX))],
            },
        ),
        (
            K::Sint32,
            Message {
                fields: vec![v(1, Value::Sint32(i32::MIN)), v(1, Value::Sint32(i32::MAX))],
            },
        ),
        (
            K::Sint64,
            Message {
                fields: vec![v(1, Value::Sint64(i64::MIN)), v(1, Value::Sint64(i64::MAX))],
            },
        ),
        (
            K::Bool,
            Message {
                fields: vec![v(1, Value::Bool(false)), v(1, Value::Bool(true))],
            },
        ),
        (
            K::Fixed32,
            Message {
                fields: vec![v(1, Value::Fixed32(0)), v(1, Value::Fixed32(u32::MAX))],
            },
        ),
        (
            K::Fixed64,
            Message {
                fields: vec![v(1, Value::Fixed64(0)), v(1, Value::Fixed64(u64::MAX))],
            },
        ),
    ];
    for (kind, message) in cases {
        let defs = [M {
            fields: vec![f(1, kind, true)],
        }];
        let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
        let bytes = encoded(&schema, &message);
        assert_eq!(bytes[0], 0x0a);
        assert_eq!(schema.parse_exact(&bytes).unwrap(), message);
    }
}
#[test]
fn accepted_mutation_corpus_reserializes_to_identical_actual_bytes() {
    let defs = spec();
    let schema = Schema::validate(&defs, 0, Limits::default()).unwrap();
    let bytes = hex("08 96 01 12 07 74 65 73 74 69 6e 67 1a 03 08 96 01 22 06 03 8e 02 9e a7 05");
    for index in 0..bytes.len() {
        for byte in 0..=255 {
            let mut candidate = bytes.clone();
            candidate[index] = byte;
            if let Ok(value) = schema.parse_exact(&candidate) {
                assert_eq!(encoded(&schema, &value), candidate);
            }
        }
    }
}
#[test]
fn maximum_depth_is_supported_but_beyond_hard_ceiling_is_refused() {
    let defs: Vec<_> = (0..64)
        .map(|i| M {
            fields: if i == 63 {
                vec![f(1, K::U32, false)]
            } else {
                vec![f(1, K::Message(i + 1), false)]
            },
        })
        .collect();
    let limits = Limits {
        max_depth: 64,
        ..Limits::default()
    };
    let schema = Schema::validate(&defs, 0, limits).unwrap();
    let mut message = Message {
        fields: vec![v(1, Value::U32(1))],
    };
    for _ in 0..63 {
        message = Message {
            fields: vec![v(1, Value::Message(message))],
        };
    }
    let bytes = encoded(&schema, &message);
    assert_eq!(schema.parse_exact(&bytes).unwrap(), message);
    assert_eq!(
        Schema::validate(
            &defs,
            0,
            Limits {
                max_depth: 65,
                ..limits
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::DepthLimit
    );
}

#[test]
fn protobuf_length_delimited_ceiling_is_independent_of_raised_caller_budgets() {
    let defs = [M {
        fields: vec![f(1, K::Bytes, false)],
    }];
    let schema = Schema::validate(
        &defs,
        0,
        Limits {
            max_payload_bytes: usize::MAX,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        schema
            .parse_exact(&hex("0a 80 80 80 80 08"))
            .unwrap_err()
            .kind,
        ErrorKind::Overflow
    );
}
