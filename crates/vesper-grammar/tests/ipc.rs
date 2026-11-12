use vesper_grammar::{
    arrow::AlignmentProfile,
    ipc::{self, ErrorKind as E, Limits},
};
const MODERN: &[u8] = include_bytes!("fixtures/ipc/modern-v5.arrow");
const LEGACY: &[u8] = include_bytes!("fixtures/ipc/legacy-v4.arrow");
fn u16at(b: &[u8], p: usize) -> usize {
    u16::from_le_bytes(b[p..p + 2].try_into().unwrap()) as usize
}
fn u32at(b: &[u8], p: usize) -> usize {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap()) as usize
}
fn put32(b: &mut [u8], p: usize, n: u32) {
    b[p..p + 4].copy_from_slice(&n.to_le_bytes());
}
fn put64(b: &mut [u8], p: usize, n: i64) {
    b[p..p + 8].copy_from_slice(&n.to_le_bytes());
}
fn target(b: &[u8], p: usize) -> usize {
    p + u32at(b, p)
}
fn slot(b: &[u8], table: usize, index: usize) -> Option<usize> {
    let displacement = i32::from_le_bytes(b[table..table + 4].try_into().unwrap());
    let vt = (table as i64 - i64::from(displacement)) as usize;
    if 4 + index * 2 >= u16at(b, vt) {
        return None;
    }
    let offset = u16at(b, vt + 4 + index * 2);
    (offset != 0).then_some(table + offset)
}
fn message(b: &[u8], frame: usize) -> usize {
    let meta = frame + 8;
    meta + u32at(b, meta)
}
fn header(b: &[u8], frame: usize) -> usize {
    target(b, slot(b, message(b, frame), 2).unwrap())
}
fn vector(b: &[u8], table: usize, index: usize) -> usize {
    target(b, slot(b, table, index).unwrap()) + 4
}
fn first_field(b: &[u8]) -> usize {
    target(b, vector(b, header(b, 0), 1))
}
fn mutated(f: impl FnOnce(&mut [u8])) -> Vec<u8> {
    let mut b = MODERN.to_vec();
    f(&mut b);
    b
}
fn reject(b: &[u8]) -> E {
    ipc::parse_exact(b, Limits::default()).unwrap_err().kind
}
#[test]
fn independent_arrow_values_and_versions() {
    for (bytes, version) in [(MODERN, 4), (LEGACY, 3)] {
        let s = ipc::parse_exact(bytes, Limits::default()).unwrap();
        assert_eq!(s.schema().metadata_version(), version);
        let fields = s.schema().fields();
        assert_eq!(fields.len(), 2);
        assert_eq!((fields[0].name(), fields[0].nullable()), ("nullable", true));
        assert_eq!(
            (fields[1].name(), fields[1].nullable()),
            ("required", false)
        );
        assert_eq!(
            s.batches().iter().map(|b| b.rows()).collect::<Vec<_>>(),
            [9, 0, 4]
        );
        let expected = [
            Some(1),
            None,
            Some(-2),
            Some(0),
            Some(1 << 40),
            Some(-(1 << 40)),
            None,
            Some(7),
            Some(5),
        ];
        let c = s.batches()[0].columns()[0];
        for (i, x) in expected.into_iter().enumerate() {
            assert_eq!(c.get(i), Some(x));
        }
        assert_eq!(c.get(9), None);
        assert_eq!(c.null_count(), 2);
        assert_eq!(c.checked_sum(9).unwrap(), Some(11));
        assert_eq!(
            s.batches()[0].columns()[1].checked_sum(9).unwrap(),
            Some(450)
        );
        assert_eq!(s.batches()[1].columns()[0].checked_sum(0).unwrap(), None);
        assert_eq!(s.batches()[2].columns()[0].null_count(), 4);
        assert_eq!(
            s.batches()[2].columns()[1].checked_sum(4).unwrap(),
            Some(12)
        );
        assert!(c.values_bytes().as_ptr() >= bytes.as_ptr());
        assert!(c.values_bytes().as_ptr() < bytes[bytes.len() - 1..].as_ptr());
    }
}
#[test]
fn schema_only_empty_schema_and_sum_overflow() {
    let s = ipc::parse_exact(
        include_bytes!("fixtures/ipc/schema-only.arrow"),
        Limits::default(),
    )
    .unwrap();
    assert!(s.batches().is_empty());
    let s = ipc::parse_exact(
        include_bytes!("fixtures/ipc/empty-schema.arrow"),
        Limits::default(),
    )
    .unwrap();
    assert!(s.schema().fields().is_empty());
    assert_eq!(s.batches()[0].rows(), 0);
    let s = ipc::parse_exact(
        include_bytes!("fixtures/ipc/sum-overflow.arrow"),
        Limits::default(),
    )
    .unwrap();
    let c = s.batches()[0].columns()[0];
    assert_eq!(c.get(0), Some(Some(i64::MAX)));
    assert_eq!(
        c.checked_sum(2).unwrap_err().kind,
        vesper_grammar::ErrorKind::Overflow
    );
}
#[test]
fn real_unsupported_producer_profiles() {
    for (bytes, kind) in [
        (
            include_bytes!("fixtures/ipc/compressed.arrow").as_slice(),
            E::UnsupportedCompression,
        ),
        (
            include_bytes!("fixtures/ipc/nested.arrow").as_slice(),
            E::UnsupportedNested,
        ),
        (
            include_bytes!("fixtures/ipc/dictionary.arrow").as_slice(),
            E::UnsupportedDictionary,
        ),
        (
            include_bytes!("fixtures/ipc/int32.arrow").as_slice(),
            E::UnsupportedType,
        ),
        (
            include_bytes!("fixtures/ipc/unsigned.arrow").as_slice(),
            E::UnsupportedType,
        ),
        (
            include_bytes!("fixtures/ipc/custom-metadata.arrow").as_slice(),
            E::UnsupportedMetadata,
        ),
    ] {
        assert_eq!(reject(bytes), kind);
    }
}
#[test]
fn exact_prefix_eos_and_framing() {
    let mut b = MODERN.to_vec();
    b.extend_from_slice(b"tail");
    assert_eq!(reject(&b), E::TrailingBytes);
    let p = ipc::parse_prefix(&b, Limits::default()).unwrap();
    assert_eq!(p.consumed(), MODERN.len());
    assert_eq!(p.tail(), b"tail");
    for n in 0..MODERN.len() {
        assert!(
            ipc::parse_exact(&MODERN[..n], Limits::default()).is_err(),
            "{n}"
        );
    }
    assert_eq!(reject(&[0; 8]), E::MissingSchema);
    let b = mutated(|b| put32(b, 0, 200));
    assert!(ipc::parse_exact(&b, Limits::default()).is_err());
    let b = mutated(|b| put32(b, 984, 0));
    assert_eq!(reject(&b), E::Framing);
    let b = mutated(|b| put32(b, 4, 199));
    assert_eq!(reject(&b), E::Alignment);
    let b = mutated(|b| put32(b, 4, 0x80000000));
    assert_eq!(reject(&b), E::Framing);
    let mut b = MODERN[..208].to_vec();
    b.extend_from_slice(&MODERN[..208]);
    b.extend_from_slice(&MODERN[984..]);
    assert_eq!(reject(&b), E::Schema);
    assert_eq!(reject(&MODERN[208..]), E::MissingSchema);
}
#[test]
fn mandatory_eight_is_distinct_from_simd_and_host_pointer() {
    let mut b = vec![0];
    b.extend_from_slice(MODERN);
    ipc::parse_exact(&b[1..], Limits::default()).unwrap();
    ipc::parse_exact_with_alignment(MODERN, Limits::default(), AlignmentProfile::ByteCopy).unwrap();
    assert_eq!(
        ipc::parse_exact_with_alignment(MODERN, Limits::default(), AlignmentProfile::Relative64)
            .unwrap_err()
            .kind,
        E::Alignment
    );
    assert_eq!(
        ipc::parse_exact_with_alignment(MODERN, Limits::default(), AlignmentProfile::HostAddress64)
            .unwrap_err()
            .kind,
        E::Alignment
    );
    let b = mutated(|b| {
        let p = vector(b, header(b, 208), 2);
        put64(b, p + 16, 9);
    });
    assert!(matches!(
        reject(&b),
        E::Column(vesper_grammar::ErrorKind::Alignment)
    ));
}
#[test]
fn hostile_flatbuffer_offsets_vtables_and_vectors() {
    let b = mutated(|b| put32(b, 8, u32::MAX));
    assert!(ipc::parse_exact(&b, Limits::default()).is_err());
    let b = mutated(|b| {
        let p = message(b, 0);
        put32(b, p, i32::MIN as u32);
    });
    assert!(ipc::parse_exact(&b, Limits::default()).is_err());
    let b = mutated(|b| {
        let p = message(b, 0);
        put32(b, p, 0);
    });
    assert!(ipc::parse_exact(&b, Limits::default()).is_err());
    let b = mutated(|b| {
        let p = slot(b, header(b, 0), 1).unwrap();
        put32(b, p, 1);
    });
    assert!(ipc::parse_exact(&b, Limits::default()).is_err());
    let b = mutated(|b| {
        let p = vector(b, header(b, 0), 1) - 4;
        put32(b, p, u32::MAX);
    });
    assert_eq!(reject(&b), E::CountLimit);
    let b = mutated(|b| {
        let p = vector(b, header(b, 208), 2) - 4;
        put32(b, p, u32::MAX);
    });
    assert_eq!(reject(&b), E::CountLimit);
    let b = mutated(|b| {
        let f = first_field(b);
        let p = target(b, slot(b, f, 0).unwrap());
        put32(b, p, u32::MAX);
    });
    assert_eq!(reject(&b), E::NameLimit);
    let b = mutated(|b| {
        let f = first_field(b);
        let p = target(b, slot(b, f, 0).unwrap()) + 4;
        b[p] = 255;
    });
    assert_eq!(reject(&b), E::Utf8);
    let b = mutated(|b| {
        let f = first_field(b);
        let p = target(b, slot(b, f, 0).unwrap());
        let n = u32at(b, p);
        b[p + 4 + n] = 1;
    });
    assert_eq!(reject(&b), E::Flatbuffer);
}
#[test]
fn hostile_schema_and_union_discriminants() {
    let b = mutated(|b| {
        let p = slot(b, message(b, 0), 0).unwrap();
        b[p..p + 2].copy_from_slice(&2u16.to_le_bytes());
    });
    assert_eq!(reject(&b), E::UnsupportedVersion);
    let b = mutated(|b| {
        let p = slot(b, message(b, 0), 1).unwrap();
        b[p] = 0;
    });
    assert_eq!(reject(&b), E::UnsupportedHeader);
    let b = mutated(|b| {
        let f = first_field(b);
        let p = slot(b, f, 2).unwrap();
        b[p] = 3;
    });
    assert_eq!(reject(&b), E::UnsupportedType);
    let b = mutated(|b| {
        let f = first_field(b);
        let p = slot(b, f, 1).unwrap();
        b[p] = 2;
    });
    assert_eq!(reject(&b), E::Flatbuffer);
    // Clear nullable on a field whose actual body includes nulls.
    let b = mutated(|b| {
        let f = first_field(b);
        let p = slot(b, f, 1).unwrap();
        b[p] = 0;
    });
    assert_eq!(reject(&b), E::NonNullable);
}
#[test]
fn body_nodes_buffers_bitmap_and_cross_column_alias() {
    let b = mutated(|b| {
        let p = vector(b, header(b, 208), 1);
        put64(b, p, 8);
    });
    assert_eq!(reject(&b), E::Descriptor);
    let b = mutated(|b| {
        let p = vector(b, header(b, 208), 1);
        put64(b, p + 8, -1);
    });
    assert!(ipc::parse_exact(&b, Limits::default()).is_err());
    let b = mutated(|b| {
        let p = vector(b, header(b, 208), 2);
        put64(b, p, -8);
    });
    assert!(ipc::parse_exact(&b, Limits::default()).is_err());
    let b = mutated(|b| {
        let p = vector(b, header(b, 208), 2);
        put64(b, p + 16, i64::MAX);
    });
    assert!(ipc::parse_exact(&b, Limits::default()).is_err());
    let b = mutated(|b| {
        let p = vector(b, header(b, 208), 2);
        put64(b, p + 24, 64);
    });
    assert!(ipc::parse_exact(&b, Limits::default()).is_err());
    let b = mutated(|b| {
        let p = vector(b, header(b, 208), 2);
        put64(b, p + 48, 8);
    });
    assert_eq!(reject(&b), E::Aliasing);
    let b = mutated(|b| b[400] ^= 1);
    assert!(ipc::parse_exact(&b, Limits::default()).is_err());
    let b = mutated(|b| {
        let p = slot(b, header(b, 208), 0).unwrap();
        put64(b, p, -1);
    });
    assert_eq!(reject(&b), E::Descriptor);
    let b = mutated(|b| {
        let p = slot(b, message(b, 208), 3).unwrap();
        put64(b, p, i64::MAX);
    });
    assert_eq!(reject(&b), E::Alignment);
}
#[test]
fn every_budget_and_exact_resource_boundaries() {
    let cases = [
        (
            Limits {
                max_input_bytes: MODERN.len() - 1,
                ..Limits::default()
            },
            E::InputLimit,
        ),
        (
            Limits {
                max_metadata_bytes: 199,
                ..Limits::default()
            },
            E::MetadataLimit,
        ),
        (
            Limits {
                max_total_metadata_bytes: 200,
                ..Limits::default()
            },
            E::MetadataLimit,
        ),
        (
            Limits {
                max_fields: 1,
                ..Limits::default()
            },
            E::CountLimit,
        ),
        (
            Limits {
                max_batches: 2,
                ..Limits::default()
            },
            E::CountLimit,
        ),
        (
            Limits {
                max_rows_per_batch: 8,
                ..Limits::default()
            },
            E::CountLimit,
        ),
        (
            Limits {
                max_total_rows: 12,
                ..Limits::default()
            },
            E::CountLimit,
        ),
        (
            Limits {
                max_name_bytes: 7,
                ..Limits::default()
            },
            E::NameLimit,
        ),
        (
            Limits {
                max_metadata_objects: 1,
                ..Limits::default()
            },
            E::CountLimit,
        ),
        (
            Limits {
                max_depth: 3,
                ..Limits::default()
            },
            E::DepthLimit,
        ),
        (
            Limits {
                max_work: 0,
                ..Limits::default()
            },
            E::WorkLimit,
        ),
        (
            Limits {
                max_allocation_bytes: 0,
                ..Limits::default()
            },
            E::AllocationLimit,
        ),
        (
            Limits {
                max_depth: 0,
                ..Limits::default()
            },
            E::Configuration,
        ),
        (
            Limits {
                max_fields: usize::MAX,
                ..Limits::default()
            },
            E::Configuration,
        ),
    ];
    for (limits, kind) in cases {
        assert_eq!(ipc::parse_exact(MODERN, limits).unwrap_err().kind, kind);
    }
    let s = ipc::parse_exact(MODERN, Limits::default()).unwrap();
    let work = s.work_used();
    let allocation = s.metadata_allocation_bytes();
    ipc::parse_exact(
        MODERN,
        Limits {
            max_work: work,
            max_allocation_bytes: allocation,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        ipc::parse_exact(
            MODERN,
            Limits {
                max_work: work - 1,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        E::WorkLimit
    );
    assert_eq!(
        ipc::parse_exact(
            MODERN,
            Limits {
                max_allocation_bytes: allocation - 1,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        E::AllocationLimit
    );
}
#[test]
fn bounded_byte_mutation_never_panics() {
    for i in 0..MODERN.len() {
        for value in [0, 255] {
            let mut b = MODERN.to_vec();
            b[i] = value;
            let _ = ipc::parse_exact(&b, Limits::default());
        }
    }
}
#[test]
fn metadata_aliases_scalar_overlap_endian_and_opaque_padding() {
    // Name points into the already-validated children vector: string/vector role confusion.
    let b = mutated(|b| {
        let f = first_field(b);
        let int = target(b, slot(b, f, 5).unwrap());
        let name = slot(b, f, 0).unwrap();
        put32(b, name, (int - name) as u32);
    });
    assert_eq!(reject(&b), E::Aliasing);
    let b = mutated(|b| {
        let f = first_field(b);
        let vt =
            (f as i64 - i64::from(i32::from_le_bytes(b[f..f + 4].try_into().unwrap()))) as usize;
        b[vt + 6..vt + 8].copy_from_slice(&7u16.to_le_bytes());
    });
    assert_eq!(reject(&b), E::Aliasing);
    // Reuse the schema's four-byte scalar space for the optional endian scalar,
    // removing its fields vector. This is structurally valid FlatBuffer metadata.
    let b = mutated(|b| {
        let s = header(b, 0);
        let vt =
            (s as i64 - i64::from(i32::from_le_bytes(b[s..s + 4].try_into().unwrap()))) as usize;
        b[vt + 4..vt + 6].copy_from_slice(&4u16.to_le_bytes());
        b[vt + 6..vt + 8].fill(0);
        b[s + 4..s + 6].copy_from_slice(&1u16.to_le_bytes());
    });
    assert_eq!(reject(&b), E::UnsupportedEndian);
    // Arrow permits opaque metadata/body alignment padding. No canonicality claim.
    let b = mutated(|b| {
        b[12] = 255;
        b[402..408].fill(255);
    });
    ipc::parse_exact(&b, Limits::default()).unwrap();
}
