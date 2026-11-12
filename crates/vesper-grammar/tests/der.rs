use vesper_grammar::der::*;
fn decode(bytes: &[u8]) -> Document<'_> {
    parse_exact(bytes, &Schema::Any, Limits::default()).unwrap()
}
fn encode(value: &Value<'_>, schema: &Schema<'_>) -> Vec<u8> {
    let size = encoded_len(value, schema, Limits::default()).unwrap();
    let mut out = vec![0xab; size + 3];
    assert_eq!(
        encode_into(value, schema, &mut out, Limits::default()).unwrap(),
        size
    );
    assert_eq!(&out[size..], &[0xab; 3]);
    out.truncate(size);
    out
}
fn identity(bytes: &[u8], schema: &Schema<'_>) {
    let doc = parse_exact(bytes, schema, Limits::default()).unwrap();
    let mut out = vec![0xab; bytes.len() + 3];
    let len = reencode_into(&doc, &mut out, Limits::default()).unwrap();
    assert_eq!(&out[..len], bytes);
    assert_eq!(&out[len..], &[0xab; 3]);
}
fn bad(bytes: &[u8]) {
    assert!(
        parse_exact(bytes, &Schema::Any, Limits::default()).is_err(),
        "accepted {bytes:02x?}"
    );
}
#[test]
fn independent_x690_primitives_and_rfc3279_algorithm_identifier() {
    // X.690 DER primitive canonical forms, and RFC3279§2.3.1 rsaEncryption
    // AlgorithmIdentifier (OID1.2.840.113549.1.1.1 plus required NULL).
    let fixtures: [(&[u8], Value<'_>); 12] = [
        (&[1, 1, 0], Value::Boolean(false)),
        (&[1, 1, 0xff], Value::Boolean(true)),
        (&[2, 1, 0], Value::Integer(&[0])),
        (&[2, 1, 0x7f], Value::Integer(&[0x7f])),
        (&[2, 2, 0, 0x80], Value::Integer(&[0, 0x80])),
        (&[2, 1, 0x80], Value::Integer(&[0x80])),
        (&[2, 2, 0xff, 0x7f], Value::Integer(&[0xff, 0x7f])),
        (&[2, 1, 0xff], Value::Integer(&[0xff])),
        (
            &[3, 1, 0],
            Value::BitString {
                unused_bits: 0,
                bytes: &[],
            },
        ),
        (
            &[3, 2, 5, 0xa0],
            Value::BitString {
                unused_bits: 5,
                bytes: &[0xa0],
            },
        ),
        (&[4, 2, 0, 0xff], Value::OctetString(&[0, 0xff])),
        (&[5, 0], Value::Null),
    ];
    for (bytes, value) in fixtures {
        assert_eq!(encode(&value, &Schema::Any), bytes);
        identity(bytes, &Schema::Any);
    }
    let oid = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1];
    let bytes = &[
        0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1, 5, 0,
    ];
    let fields = [Value::ObjectIdentifier(oid), Value::Null];
    let schema = [Schema::ObjectIdentifier, Schema::Null];
    assert_eq!(
        encode(&Value::Sequence(&fields), &Schema::Sequence(&schema)),
        bytes
    );
    let doc = parse_exact(bytes, &Schema::Sequence(&schema), Limits::default()).unwrap();
    assert_eq!(doc.node_count(), 3);
    assert_eq!(doc.nodes().count(), 3);
    assert_eq!(
        doc.root().children().next().unwrap().oid_bytes(),
        Some(oid.as_slice())
    );
    identity(bytes, &Schema::Any);
}
#[test]
fn integer_full_signed_boundaries_and_nonminimal_forms() {
    for value in [
        i64::MIN,
        i64::MIN + 1,
        -65536,
        -32769,
        -32768,
        -257,
        -256,
        -129,
        -128,
        -1,
        0,
        1,
        127,
        128,
        255,
        256,
        32767,
        32768,
        i64::MAX,
    ] {
        let raw = value.to_be_bytes();
        let mut start = 0;
        while start < 7
            && ((raw[start] == 0 && raw[start + 1] & 0x80 == 0)
                || (raw[start] == 0xff && raw[start + 1] & 0x80 != 0))
        {
            start += 1;
        }
        let bytes = encode(&Value::Integer(&raw[start..]), &Schema::Integer);
        assert_eq!(decode(&bytes).root().as_i64().unwrap(), value);
        identity(&bytes, &Schema::Any);
    }
    for bytes in [
        &[2, 0][..],
        &[2, 2, 0, 0],
        &[2, 2, 0, 0x7f],
        &[2, 2, 0xff, 0x80],
        &[2, 2, 0xff, 0xff],
    ] {
        bad(bytes);
    }
    let large = [0x01; 64];
    let bytes = encode(&Value::Integer(&large), &Schema::Integer);
    let doc = decode(&bytes);
    assert_eq!(doc.root().integer_bytes(), Some(large.as_slice()));
    assert_eq!(doc.root().as_i64().unwrap_err().kind, ErrorKind::Overflow);
}
#[test]
fn bits_padding_boolean_null_and_constructed_bits_are_strict() {
    for value in 0..=255u8 {
        let bytes = [1, 1, value];
        assert_eq!(
            parse_exact(&bytes, &Schema::Any, Limits::default()).is_ok(),
            matches!(value, 0 | 0xff)
        );
    }
    for unused in 0..=255u8 {
        for last in 0..=255u8 {
            let bytes = [3, 2, unused, last];
            let valid = unused <= 7 && last & ((1u16 << u16::from(unused.min(7))) - 1) as u8 == 0;
            assert_eq!(
                parse_exact(&bytes, &Schema::Any, Limits::default()).is_ok(),
                valid
            );
        }
    }
    for bytes in [
        &[1, 0][..],
        &[1, 2, 0, 0],
        &[3, 0],
        &[3, 1, 1],
        &[5, 1, 0],
        &[0x21, 1, 0xff],
        &[0x22, 1, 1],
        &[0x23, 1, 0],
        &[0x24, 0],
        &[0x25, 0],
        &[0x26, 1, 0],
        &[0x10, 0],
        &[0x11, 0],
        &[0, 0],
    ] {
        bad(bytes);
    }
}
#[test]
fn definite_length_minimal_tags_unknowns_and_boundary_lengths() {
    for length in [0, 1, 126, 127, 128, 129, 255, 256, 257, 65535, 65536] {
        let payload = vec![0xa5; length];
        let bytes = encode(&Value::OctetString(&payload), &Schema::Any);
        let doc = decode(&bytes);
        assert_eq!(doc.root().octets(), Some(payload.as_slice()));
        identity(&bytes, &Schema::Any);
    }
    for bytes in [
        &[4, 0x80, 0, 0][..],
        &[4, 0x81, 0],
        &[4, 0x81, 127],
        &[4, 0x82, 0, 128],
        &[4, 0xff],
        &[0x1f, 0x1e, 0],
        &[0x1f, 0x80, 0x1f, 0],
        &[0x1f, 0x81],
        &[0x9f, 0x81],
        &[0x30, 0x80, 5, 0, 0, 0],
    ] {
        bad(bytes);
    }
    for bytes in [
        &[0x1f, 0x1f, 0][..],
        &[0x80, 0],
        &[0xa0, 0],
        &[0x0c, 1, b'a'],
        &[0x13, 1, b'a'],
        &[0x17, 0],
        &[0x18, 0],
    ] {
        assert!(matches!(
            parse_exact(bytes, &Schema::Any, Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::UnsupportedTag { .. }
        ));
    }
    let mut overflow = vec![0x1f];
    overflow.extend([0xff; 10]);
    overflow.push(0);
    assert_eq!(
        parse_exact(&overflow, &Schema::Any, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Overflow
    );
    let mut length = vec![4, 0x88];
    length.extend(u64::MAX.to_be_bytes());
    assert!(parse_exact(&length, &Schema::Any, Limits::default()).is_err());
}
fn base128(mut value: u64) -> Vec<u8> {
    let mut bytes = vec![(value % 128) as u8];
    value /= 128;
    while value != 0 {
        bytes.push((value % 128) as u8 | 128);
        value /= 128;
    }
    bytes.reverse();
    bytes
}
#[test]
fn oid_subidentifiers_minimal_termination_and_u64_overflow() {
    for number in [
        0,
        1,
        39,
        40,
        79,
        80,
        127,
        128,
        16383,
        16384,
        u32::MAX as u64,
        u64::MAX,
    ] {
        let bytes = base128(number);
        let encoded = encode(&Value::ObjectIdentifier(&bytes), &Schema::ObjectIdentifier);
        assert_eq!(decode(&encoded).root().oid_bytes(), Some(bytes.as_slice()));
        identity(&encoded, &Schema::Any);
    }
    for content in [
        &[][..],
        &[0x80, 0],
        &[42, 0x80, 0],
        &[0x81],
        &[42, 0xff],
        &[0x82, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0],
    ] {
        let mut bytes = vec![6, content.len() as u8];
        bytes.extend(content);
        bad(&bytes);
    }
}
#[test]
fn typed_set_of_sorts_complete_encodings_retains_duplicates_and_rejects_untyped_set() {
    let children = [
        Value::Integer(&[0xff]),
        Value::Integer(&[0, 0x80]),
        Value::Integer(&[0x7f]),
        Value::Integer(&[0x7f]),
    ];
    let schema = Schema::SetOf(&Schema::Integer);
    let bytes = encode(&Value::SetOf(&children), &schema);
    assert_eq!(
        bytes,
        [0x31, 13, 2, 1, 0x7f, 2, 1, 0x7f, 2, 1, 0xff, 2, 2, 0, 0x80]
    );
    let doc = parse_exact(&bytes, &schema, Limits::default()).unwrap();
    assert_eq!(doc.root().kind(), Kind::SetOf);
    assert_eq!(doc.root().children().count(), 4);
    identity(&bytes, &schema);
    bad(&bytes);
    let unsorted = [0x31, 6, 2, 1, 2, 2, 1, 1];
    assert_eq!(
        parse_exact(&unsorted, &schema, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::SetOrder
    );
    assert!(parse_exact(&[0x31, 2, 5, 0], &schema, Limits::default()).is_err());
    let mixed = [Value::Boolean(true), Value::OctetString(b"x"), Value::Null];
    let mixed_schema = Schema::SetOf(&Schema::Any);
    let mixed = encode(&Value::SetOf(&mixed), &mixed_schema);
    identity(&mixed, &mixed_schema);
}
#[test]
fn schema_arity_nested_frames_prefix_and_borrowing() {
    let children = [Value::Null, Value::OctetString(b"borrowed")];
    let bytes = encode(&Value::Sequence(&children), &Schema::Any);
    let schema = [Schema::Null, Schema::OctetString];
    let doc = parse_exact(&bytes, &Schema::Sequence(&schema), Limits::default()).unwrap();
    let node = doc.root().children().nth(1).unwrap();
    let offset = bytes.iter().position(|b| *b == b'b').unwrap();
    assert_eq!(node.octets().unwrap().as_ptr(), bytes[offset..].as_ptr());
    assert!(
        parse_exact(
            &bytes,
            &Schema::Sequence(&[Schema::Null]),
            Limits::default()
        )
        .is_err()
    );
    assert!(
        parse_exact(
            &bytes,
            &Schema::SequenceOf(&Schema::Null),
            Limits::default()
        )
        .is_err()
    );
    let mut pipelined = bytes.clone();
    pipelined.extend([5, 0]);
    let (first, consumed) = parse_prefix(&pipelined, &Schema::Any, Limits::default()).unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(first.raw(), bytes);
    assert_eq!(decode(&pipelined[consumed..]).root().kind(), Kind::Null);
    assert_eq!(
        parse_exact(&pipelined, &Schema::Any, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::TrailingBytes
    );
    for end in 0..bytes.len() {
        bad(&bytes[..end]);
    }
    bad(&[0x30, 3, 2, 2, 1]);
    bad(&[0x30, 4, 2, 1, 0, 0]);
}
#[test]
fn encoder_all_error_classes_preserve_every_output_byte_and_source() {
    let children = [Value::Integer(&[1]), Value::OctetString(b"data")];
    let value = Value::Sequence(&children);
    let base = Limits::default();
    let plan = encoding_plan(&value, &Schema::Any, base).unwrap();
    for (limits, expected) in [
        (
            Limits {
                max_input_bytes: 0,
                ..base
            },
            ErrorKind::InputLimit,
        ),
        (
            Limits {
                max_value_bytes: 0,
                ..base
            },
            ErrorKind::ValueLimit,
        ),
        (
            Limits {
                max_output_bytes: 0,
                ..base
            },
            ErrorKind::OutputLimit,
        ),
        (
            Limits {
                max_intermediate_bytes: plan.intermediate_bytes - 1,
                ..base
            },
            ErrorKind::IntermediateLimit,
        ),
        (
            Limits {
                max_metadata_bytes: plan.metadata_bytes - 1,
                ..base
            },
            ErrorKind::MetadataLimit,
        ),
        (
            Limits {
                max_nodes: plan.nodes - 1,
                ..base
            },
            ErrorKind::NodeLimit,
        ),
        (
            Limits {
                max_schema_nodes: 0,
                ..base
            },
            ErrorKind::SchemaLimit,
        ),
        (
            Limits {
                max_depth: 1,
                ..base
            },
            ErrorKind::DepthLimit,
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
                max_depth: 65,
                ..base
            },
            ErrorKind::InvalidLimits,
        ),
    ] {
        let mut output = [0xab; 100];
        assert_eq!(
            encode_into(&value, &Schema::Any, &mut output, limits)
                .unwrap_err()
                .kind,
            expected
        );
        assert_eq!(output, [0xab; 100]);
    }
    for size in 0..plan.encoded_bytes {
        let mut output = vec![0xab; size];
        assert_eq!(
            encode_into(&value, &Schema::Any, &mut output, base)
                .unwrap_err()
                .kind,
            ErrorKind::OutputTooSmall
        );
        assert_eq!(output, vec![0xab; size]);
    }
    let mut out = [0xab; 100];
    assert!(encode_into(&Value::Integer(&[0, 0]), &Schema::Integer, &mut out, base).is_err());
    assert_eq!(out, [0xab; 100]);
    assert!(encode_into(&Value::SetOf(&children), &Schema::Any, &mut out, base).is_err());
    assert_eq!(out, [0xab; 100]);
    let exact = Limits {
        max_output_bytes: plan.encoded_bytes,
        max_input_bytes: plan.encoded_bytes,
        max_metadata_bytes: plan.metadata_bytes,
        max_intermediate_bytes: plan.intermediate_bytes,
        max_nodes: plan.nodes,
        ..base
    };
    assert_eq!(
        encoded_len(&value, &Schema::Any, exact).unwrap(),
        plan.encoded_bytes
    );
    assert!(encode_into(&value, &Schema::Any, &mut out, exact).is_ok());
}
#[test]
fn schema_and_value_alias_dags_are_visited_before_allocating_and_bounded() {
    let s0 = Schema::Integer;
    let s1 = [Schema::SequenceOf(&s0), Schema::SequenceOf(&s0)];
    let s2 = Schema::Sequence(&s1);
    assert_eq!(
        parse_exact(
            &[0x30, 0],
            &s2,
            Limits {
                max_schema_nodes: 3,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::SchemaLimit
    );
    let mut schemas = vec![Schema::Integer];
    for _ in 0..65 {
        let previous = schemas.pop().unwrap();
        let leaked = Box::leak(Box::new(previous));
        schemas.push(Schema::SequenceOf(leaked));
    }
    assert_eq!(
        parse_exact(&[0x30, 0], &schemas[0], Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::DepthLimit
    );
    let values0 = [Value::Null];
    let values1 = [Value::Sequence(&values0), Value::Sequence(&values0)];
    let values2 = [Value::Sequence(&values1), Value::Sequence(&values1)];
    let mut out = [0xab; 100];
    assert_eq!(
        encode_into(
            &Value::Sequence(&values2),
            &Schema::Any,
            &mut out,
            Limits {
                max_nodes: 4,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::NodeLimit
    );
    assert_eq!(out, [0xab; 100]);
}
#[test]
fn borrowed_document_reencoder_applies_new_depth_value_intermediate_and_output_limits() {
    let doc = decode(&[0x30, 3, 2, 1, 1]);
    let base = Limits::default();
    for limits in [
        Limits {
            max_depth: 1,
            ..base
        },
        Limits {
            max_value_bytes: 0,
            ..base
        },
        Limits {
            max_intermediate_bytes: 0,
            ..base
        },
        Limits {
            max_metadata_bytes: 0,
            ..base
        },
        Limits {
            max_output_bytes: 0,
            ..base
        },
        Limits {
            max_work: 0,
            ..base
        },
        Limits {
            max_nodes: 0,
            ..base
        },
    ] {
        let mut out = [0xab; 10];
        assert!(reencode_into(&doc, &mut out, limits).is_err());
        assert_eq!(out, [0xab; 10]);
    }
}

#[test]
fn independent_openssl_generated_profile_fixture() {
    // OpenSSL3.5.7, `openssl asn1parse -genconf profile.cnf -out profile.der`:
    // asn1=SEQUENCE:profile
    // [profile]
    // bool=BOOLEAN:TRUE
    // int=INTEGER:-129
    // bits=FORMAT:HEX,BITSTRING:A0
    // octets=FORMAT:HEX,OCTETSTRING:00FF
    // null=NULL
    // oid=OID:1.2.840.113549.1.1.1
    let expected = [
        0x30, 0x1c, 1, 1, 0xff, 2, 2, 0xff, 0x7f, 3, 2, 0, 0xa0, 4, 2, 0, 0xff, 5, 0, 6, 9, 0x2a,
        0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1,
    ];
    let children = [
        Value::Boolean(true),
        Value::Integer(&[0xff, 0x7f]),
        Value::BitString {
            unused_bits: 0,
            bytes: &[0xa0],
        },
        Value::OctetString(&[0, 0xff]),
        Value::Null,
        Value::ObjectIdentifier(&[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1]),
    ];
    assert_eq!(encode(&Value::Sequence(&children), &Schema::Any), expected);
    let doc = decode(&expected);
    assert_eq!(doc.node_count(), 7);
    let nodes: Vec<_> = doc.root().children().collect();
    assert_eq!(nodes[0].boolean(), Some(true));
    assert_eq!(nodes[1].as_i64(), Ok(-129));
    assert_eq!(nodes[2].bit_string(), Some((0, &[0xa0][..])));
    assert_eq!(nodes[3].octets(), Some(&[0, 0xff][..]));
    assert_eq!(nodes[4].kind(), Kind::Null);
    identity(&expected, &Schema::Any);
    for end in 0..expected.len() {
        bad(&expected[..end]);
    }
}
#[test]
fn parse_budgets_work_metadata_and_typed_value_boundaries() {
    let bytes = [0x30, 3, 2, 1, 1];
    let base = Limits::default();
    let doc = decode(&bytes);
    for (limits, expected) in [
        (
            Limits {
                max_input_bytes: 4,
                ..base
            },
            ErrorKind::InputLimit,
        ),
        (
            Limits {
                max_value_bytes: 2,
                ..base
            },
            ErrorKind::ValueLimit,
        ),
        (
            Limits {
                max_nodes: 1,
                ..base
            },
            ErrorKind::NodeLimit,
        ),
        (
            Limits {
                max_schema_nodes: 0,
                ..base
            },
            ErrorKind::SchemaLimit,
        ),
        (
            Limits {
                max_depth: 1,
                ..base
            },
            ErrorKind::DepthLimit,
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
                max_metadata_bytes: doc.metadata_bytes() - 1,
                ..base
            },
            ErrorKind::MetadataLimit,
        ),
        (
            Limits {
                max_nodes: 16385,
                ..base
            },
            ErrorKind::InvalidLimits,
        ),
    ] {
        assert_eq!(
            parse_exact(&bytes, &Schema::Any, limits).unwrap_err().kind,
            expected
        );
    }
    let exact = Limits {
        max_input_bytes: bytes.len(),
        max_value_bytes: 3,
        max_nodes: 2,
        max_metadata_bytes: doc.metadata_bytes(),
        ..base
    };
    assert_eq!(
        parse_exact(&bytes, &Schema::Any, exact)
            .unwrap()
            .node_count(),
        2
    );
    // Schema visit1 + each null parse pass(node1+tag1+length1)=7 total.
    assert_eq!(
        parse_exact(
            &[5, 0],
            &Schema::Any,
            Limits {
                max_work: 6,
                ..base
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::WorkLimit
    );
    assert!(
        parse_exact(
            &[5, 0],
            &Schema::Any,
            Limits {
                max_work: 7,
                ..base
            }
        )
        .is_ok()
    );
    let mut output = [0xab; 4];
    assert_eq!(
        encode_into(
            &Value::Null,
            &Schema::Any,
            &mut output,
            Limits {
                max_work: 5,
                ..base
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::WorkLimit
    );
    assert_eq!(output, [0xab; 4]);
    assert_eq!(
        encode_into(
            &Value::Null,
            &Schema::Any,
            &mut output,
            Limits {
                max_work: 6,
                ..base
            }
        )
        .unwrap(),
        2
    );
}
#[test]
fn exhaustive_small_length_forms_and_all_tag_octets() {
    for length in 0..=260usize {
        let payload = vec![0x5a; length];
        let canonical = encode(&Value::OctetString(&payload), &Schema::Any);
        identity(&canonical, &Schema::Any);
        for end in 0..canonical.len() {
            bad(&canonical[..end]);
        }
        if length < 128 {
            let mut overlong = vec![4, 0x81, length as u8];
            overlong.extend(&payload);
            bad(&overlong);
        }
        let mut overlong = vec![4, 0x82, 0, length as u8];
        if length < 256 {
            overlong.extend(&payload);
            bad(&overlong);
        } else {
            overlong.clear();
        }
    }
    for tag in 0..=255u8 {
        let bytes = [tag, 0];
        let accepted = parse_exact(&bytes, &Schema::Any, Limits::default()).is_ok();
        assert_eq!(accepted, matches!(tag, 4 | 5 | 0x30), "{tag:02x}");
    }
}
#[test]
fn all_set_of_input_permutations_encode_same_canonical_multiset() {
    let values = [
        Value::Integer(&[1]),
        Value::Integer(&[2]),
        Value::Integer(&[0xff]),
        Value::Integer(&[0, 0x80]),
    ];
    let schema = Schema::SetOf(&Schema::Integer);
    let expected = encode(&Value::SetOf(&values), &schema);
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let order = [a, b, c, d];
                    if (0..4).all(|i| (i + 1..4).all(|j| order[i] != order[j])) {
                        let selected = order.map(|i| match &values[i] {
                            Value::Integer(bytes) => Value::Integer(bytes),
                            _ => unreachable!(),
                        });
                        assert_eq!(encode(&Value::SetOf(&selected), &schema), expected);
                    }
                }
            }
        }
    }
    identity(&expected, &schema);
    let nested_schema = Schema::SetOf(&Schema::SetOf(&Schema::Integer));
    let inner_a = [Value::Integer(&[2]), Value::Integer(&[1])];
    let inner_b = [Value::Integer(&[0])];
    let values = [Value::SetOf(&inner_a), Value::SetOf(&inner_b)];
    let bytes = encode(&Value::SetOf(&values), &nested_schema);
    identity(&bytes, &nested_schema);
}
#[test]
fn deterministic_byte_mutations_never_publish_noncanonical_roundtrips() {
    let bytes = [
        0x30, 0x1c, 1, 1, 0xff, 2, 2, 0xff, 0x7f, 3, 2, 0, 0xa0, 4, 2, 0, 0xff, 5, 0, 6, 9, 0x2a,
        0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1,
    ];
    for pos in 0..bytes.len() {
        for byte in 0..=255u8 {
            let mut candidate = bytes;
            candidate[pos] = byte;
            if let Ok(doc) = parse_exact(&candidate, &Schema::Any, Limits::default()) {
                let mut output = [0xab; 64];
                let len = reencode_into(&doc, &mut output, Limits::default()).unwrap();
                assert_eq!(&output[..len], candidate);
                assert!(output[len..].iter().all(|b| *b == 0xab));
            }
        }
    }
}
