use vesper_ffi::*;
fn unsigned(bits: u16, endian: Endian) -> WireType {
    WireType::Integer(Integer::unsigned(bits, endian))
}
fn signed(bits: u16, endian: Endian) -> WireType {
    WireType::Integer(Integer::signed(bits, endian))
}
fn layout(fields: Vec<Field>) -> Result<WireLayout, LayoutError> {
    Registry::new([Declaration::new("Record", fields)])
        .unwrap()
        .compile("Record")
}
fn kafka() -> WireLayout {
    layout(vec![
        Field::new("base_offset", signed(64, Endian::Big)),
        Field::new("batch_length", signed(32, Endian::Big)),
        Field::new("partition_leader_epoch", signed(32, Endian::Big)),
        Field::new("magic", unsigned(8, Endian::Native)),
        Field::new("crc", unsigned(32, Endian::Big)),
        Field::new("attributes", unsigned(16, Endian::Big)),
        Field::new("last_offset_delta", signed(32, Endian::Big)),
        Field::new("base_timestamp", signed(64, Endian::Big)),
        Field::new("max_timestamp", signed(64, Endian::Big)),
        Field::new("producer_id", signed(64, Endian::Big)),
        Field::new("producer_epoch", signed(16, Endian::Big)),
        Field::new("base_sequence", signed(32, Endian::Big)),
        Field::new("records_count", signed(32, Endian::Big)),
    ])
    .unwrap()
}
#[test]
fn kafka_is_exactly_61_bytes_and_projects_unaligned_bytes_without_copying() {
    let layout = kafka();
    assert_eq!(layout.size(), 61);
    assert_eq!(layout.alignment(), 1);
    assert_eq!(
        layout
            .fields()
            .iter()
            .map(FieldLayout::offset)
            .collect::<Vec<_>>(),
        vec![0, 8, 12, 16, 17, 21, 23, 27, 35, 43, 51, 53, 57]
    );
    let mut packet = [0xaa; 68];
    let base = packet.as_ptr();
    let (mut header, payload) = initialize_wire(&layout, &mut packet[1..]).unwrap();
    assert_eq!(header.as_bytes().as_ptr() as usize, base as usize + 1);
    assert_eq!(payload, &[0xaa; 6]);
    header.write_i64(&["base_offset"], -123_456).unwrap();
    header.write_u64(&["crc"], 0x12345678).unwrap();
    header
        .write_i64(&["producer_epoch"], i16::MIN as i64)
        .unwrap();
    assert_eq!(&header.as_bytes()[17..21], &[0x12, 0x34, 0x56, 0x78]);
    assert_eq!(header.read_i64(&["base_offset"]), Ok(-123_456));
    assert_eq!(header.read_i64(&["producer_epoch"]), Ok(i16::MIN as i64));
    payload[0] = 0x42;
    assert_eq!(packet[0], 0xaa);
    assert_eq!(packet[62], 0x42);
    assert_eq!(
        view_wire(&layout, &packet[1..]).unwrap().read_u64(&["crc"]),
        Ok(0x12345678)
    );
}
#[test]
fn endian_and_signed_width_boundaries_round_trip_and_reject_truncation() {
    for endian in [Endian::Little, Endian::Big, Endian::Native] {
        for bits in [8, 16, 32, 64] {
            let layout = layout(vec![
                Field::new("u", unsigned(bits, endian)),
                Field::new("i", signed(bits, endian)),
            ])
            .unwrap();
            #[repr(align(8))]
            struct Aligned([u8; 16]);
            let mut buffer = Aligned([0; 16]);
            let (mut view, _) = initialize_wire(&layout, &mut buffer.0).unwrap();
            let max_u = u64::MAX >> (64 - bits);
            let max_i = i64::MAX >> (64 - bits);
            let min_i = i64::MIN >> (64 - bits);
            for value in [0, 1, max_u] {
                view.write_u64(&["u"], value).unwrap();
                assert_eq!(view.read_u64(&["u"]), Ok(value));
            }
            for value in [min_i, -1, 0, 1, max_i] {
                view.write_i64(&["i"], value).unwrap();
                assert_eq!(view.read_i64(&["i"]), Ok(value));
            }
            let before = view.as_bytes().to_vec();
            if bits < 64 {
                assert_eq!(
                    view.write_u64(&["u"], max_u + 1),
                    Err(ViewError::OutOfRange)
                );
                assert_eq!(
                    view.write_i64(&["i"], max_i + 1),
                    Err(ViewError::OutOfRange)
                );
                assert_eq!(
                    view.write_i64(&["i"], min_i - 1),
                    Err(ViewError::OutOfRange)
                );
            }
            assert_eq!(
                view.write_i64(&["u"], 0),
                Err(ViewError::SignednessMismatch)
            );
            assert_eq!(
                view.write_u64(&["i"], 0),
                Err(ViewError::SignednessMismatch)
            );
            assert_eq!(view.as_bytes(), before);
            assert_eq!(view.read_i64(&["u"]), Err(ViewError::SignednessMismatch));
        }
    }
}
#[test]
fn natural_alignment_requires_explicit_internal_and_tail_padding() {
    let err = layout(vec![
        Field::new("a", unsigned(8, Endian::Native)),
        Field::new("b", unsigned(32, Endian::Native)),
    ])
    .unwrap_err();
    assert_eq!(err.field.as_deref(), Some("b"));
    assert_eq!(
        err.kind,
        LayoutErrorKind::ImplicitPadding {
            offset: 1,
            alignment: 4,
            suggested_bytes: 3
        }
    );
    assert!(err.to_string().contains("ZeroPad[3]"));
    let err = layout(vec![
        Field::new("a", unsigned(32, Endian::Native)),
        Field::new("b", unsigned(8, Endian::Native)),
    ])
    .unwrap_err();
    assert_eq!(err.field, None);
    assert_eq!(
        err.kind,
        LayoutErrorKind::ImplicitPadding {
            offset: 5,
            alignment: 4,
            suggested_bytes: 3
        }
    );
    let padded = layout(vec![
        Field::new("a", unsigned(8, Endian::Native)),
        Field::new("pad", WireType::ZeroPad(3)),
        Field::new("b", unsigned(32, Endian::Native)),
    ])
    .unwrap();
    assert_eq!(padded.size(), 8);
    assert_eq!(padded.alignment(), 4);
}
#[test]
fn views_check_length_and_natural_base_alignment() {
    let layout = layout(vec![Field::new("word", unsigned(64, Endian::Native))]).unwrap();
    #[repr(align(8))]
    struct Aligned([u8; 24]);
    let mut buffer = Aligned([0; 24]);
    assert!(matches!(
        view_wire(&layout, &buffer.0[..7]),
        Err(ViewError::ShortBuffer {
            needed: 8,
            actual: 7
        })
    ));
    assert!(matches!(
        view_wire(&layout, &buffer.0[1..]),
        Err(ViewError::Misaligned { required: 8 })
    ));
    assert!(matches!(
        split_wire_mut(&layout, &mut buffer.0[1..]),
        Err(ViewError::Misaligned { required: 8 })
    ));
    let (mut header, payload) = split_wire_mut(&layout, &mut buffer.0).unwrap();
    header.write_u64(&["word"], 42).unwrap();
    payload.fill(7);
    assert_eq!(header.read_u64(&["word"]), Ok(42));
    assert_eq!(payload, &[7; 16]);
    let (read, tail) = split_wire(&layout, &buffer.0).unwrap();
    assert_eq!(read.read_u64(&["word"]), Ok(42));
    assert_eq!(tail, &[7; 16]);
}
#[test]
fn nested_views_mutate_the_original_and_padding_has_explicit_canonical_validation() {
    let inner = Declaration::new(
        "Inner",
        vec![
            Field::new("v", unsigned(16, Endian::Big)),
            Field::new("pad", WireType::ZeroPad(2)),
            Field::new("bytes", WireType::ByteArray(2)),
        ],
    );
    let outer = Declaration::new(
        "Outer",
        vec![
            Field::new("tag", unsigned(8, Endian::Native)),
            Field::new("inner", WireType::Struct("Inner".into())),
            Field::new("pad", WireType::ZeroPad(1)),
        ],
    );
    let layout = Registry::new([inner, outer])
        .unwrap()
        .compile("Outer")
        .unwrap();
    let mut buffer = [0xaa; 8];
    let (mut view, _) = initialize_wire(&layout, &mut buffer).unwrap();
    assert_eq!(view.validate_padding(), Ok(()));
    view.write_u64(&["inner", "v"], 0x1234).unwrap();
    view.bytes_mut(&["inner", "bytes"])
        .unwrap()
        .copy_from_slice(&[5, 6]);
    view.project_mut(&["inner"])
        .unwrap()
        .write_u64(&["v"], 0xabcd)
        .unwrap();
    assert_eq!(
        view.as_view().project(&["inner"]).unwrap().read_u64(&["v"]),
        Ok(0xabcd)
    );
    assert_eq!(view.as_bytes(), &[0, 0xab, 0xcd, 0, 0, 5, 6, 0]);
    assert_eq!(view.bytes_mut(&["pad"]), Err(ViewError::ExpectedBytes));
    let mut other = buffer;
    other[3] = 1;
    other[7] = 2;
    let foreign = view_wire(&layout, &other).unwrap();
    assert_eq!(
        foreign.validate_padding(),
        Err(ViewError::NonZeroPadding { offset: 3 })
    );
    assert_eq!(layout.semantic_eq(&buffer, &other), Ok(true));
    other[2] ^= 1;
    assert_eq!(layout.semantic_eq(&buffer, &other), Ok(false));
}
#[test]
fn invalid_types_names_nesting_and_size_overflow_fail_closed() {
    for ty in [
        WireType::Bool,
        WireType::SparseEnum,
        WireType::Unsupported("pointer".into()),
    ] {
        assert!(matches!(
            layout(vec![Field::new("invalid", ty)]).unwrap_err().kind,
            LayoutErrorKind::UnsupportedType(_)
        ));
    }
    assert_eq!(
        layout(vec![Field::new("a", unsigned(7, Endian::Big))])
            .unwrap_err()
            .kind,
        LayoutErrorKind::InvalidIntegerWidth(7)
    );
    assert_eq!(
        layout(vec![
            Field::new("a", WireType::ByteArray(usize::MAX)),
            Field::new("b", WireType::ByteArray(1))
        ])
        .unwrap_err()
        .kind,
        LayoutErrorKind::SizeOverflow
    );
    assert_eq!(
        layout(vec![
            Field::new("a", unsigned(8, Endian::Native)),
            Field::new("a", unsigned(8, Endian::Native))
        ])
        .unwrap_err()
        .kind,
        LayoutErrorKind::DuplicateField
    );
    assert_eq!(
        layout(vec![Field::new("", WireType::ZeroPad(0))])
            .unwrap_err()
            .kind,
        LayoutErrorKind::EmptyName
    );
    assert_eq!(
        layout(vec![Field::new("x", WireType::Struct("Missing".into()))])
            .unwrap_err()
            .kind,
        LayoutErrorKind::UnknownStruct("Missing".into())
    );
    assert_eq!(
        Registry::new([Declaration::new("A", vec![]), Declaration::new("A", vec![])])
            .err()
            .unwrap()
            .kind,
        LayoutErrorKind::DuplicateDeclaration
    );
    let cycle = Registry::new([
        Declaration::new("A", vec![Field::new("b", WireType::Struct("B".into()))]),
        Declaration::new("B", vec![Field::new("a", WireType::Struct("A".into()))]),
    ])
    .unwrap()
    .compile("A")
    .unwrap_err();
    assert_eq!(
        cycle.kind,
        LayoutErrorKind::RecursiveStruct(vec!["A".into(), "B".into(), "A".into()])
    );
}
#[test]
fn projections_reject_invalid_paths_and_field_kinds() {
    let layout = layout(vec![
        Field::new("n", unsigned(8, Endian::Big)),
        Field::new("bytes", WireType::ByteArray(2)),
    ])
    .unwrap();
    let mut bytes = [0; 3];
    let (mut view, _) = initialize_wire(&layout, &mut bytes).unwrap();
    assert_eq!(view.read_u64(&[]), Err(ViewError::EmptyPath));
    assert_eq!(
        view.read_u64(&["missing"]),
        Err(ViewError::UnknownField("missing".into()))
    );
    assert_eq!(view.read_u64(&["n", "x"]), Err(ViewError::ExpectedStruct));
    assert_eq!(view.read_u64(&["bytes"]), Err(ViewError::ExpectedInteger));
    assert_eq!(view.bytes_mut(&["n"]), Err(ViewError::ExpectedBytes));
    assert!(matches!(
        view.project_mut(&["n"]),
        Err(ViewError::ExpectedStruct)
    ));
}

#[test]
fn adversarial_nesting_and_zero_sized_expansion_are_bounded() {
    let deep = (0..130)
        .map(|i| {
            Declaration::new(
                format!("D{i}"),
                if i == 129 {
                    vec![]
                } else {
                    vec![Field::new("child", WireType::Struct(format!("D{}", i + 1)))]
                },
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        Registry::new(deep).unwrap().compile("D0").unwrap_err().kind,
        LayoutErrorKind::DepthLimit
    );
    let repeated = (0..18)
        .map(|i| {
            Declaration::new(
                format!("D{i}"),
                if i == 0 {
                    vec![Field::new("empty", WireType::ByteArray(0))]
                } else {
                    vec![
                        Field::new("left", WireType::Struct(format!("D{}", i - 1))),
                        Field::new("right", WireType::Struct(format!("D{}", i - 1))),
                    ]
                },
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        Registry::new(repeated)
            .unwrap()
            .compile("D17")
            .unwrap_err()
            .kind,
        LayoutErrorKind::ComplexityLimit
    );
}
#[test]
fn failed_construction_and_writes_leave_bytes_unchanged() {
    let layout = layout(vec![Field::new("word", unsigned(32, Endian::Native))]).unwrap();
    #[repr(align(4))]
    struct Aligned([u8; 8]);
    let mut buffer = Aligned([0x7b; 8]);
    assert!(initialize_wire(&layout, &mut buffer.0[..3]).is_err());
    assert_eq!(buffer.0, [0x7b; 8]);
    assert!(initialize_wire(&layout, &mut buffer.0[1..]).is_err());
    assert_eq!(buffer.0, [0x7b; 8]);
    let (mut view, _) = split_wire_mut(&layout, &mut buffer.0).unwrap();
    assert_eq!(
        view.write_u64(&["word"], u64::MAX),
        Err(ViewError::OutOfRange)
    );
    assert_eq!(view.as_bytes(), &[0x7b; 4]);
}
