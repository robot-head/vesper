use vesper_serde::archive::{self, Archive, RelPtr32, RelSlice32, TreeNode};
use vesper_serde::binary::ErrorKind;
use vesper_serde::streamvbyte::{self, Limits as CompactLimits};

fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .map(|x| u8::from_str_radix(x, 16).unwrap())
        .collect()
}

// Independently hand-calculated from the StreamVByte tag convention and checked
// against fast-pack/streamvbyte scalar C at commit
// 60286c56cc61f16dfbe003d6e1ac84ad48ad4e8b (src/streamvbyte_encode.c).
// The first
// vector exercises every length; the second crosses a control-group boundary.
fn compact_vectors() -> Vec<(Vec<u32>, Vec<u8>)> {
    vec![
        (vec![], vec![]),
        (vec![0], hex("00 00")),
        (vec![255, 256], hex("04 ff 00 01")),
        (
            vec![1, 256, 65_536, 16_777_216],
            hex("e4 01 00 01 00 00 01 00 00 00 01"),
        ),
        (
            vec![1, 256, 65_536, 16_777_216, 42],
            hex("e4 00 01 00 01 00 00 01 00 00 00 01 2a"),
        ),
        (
            vec![0x01020304, 0xff, 0xabcdef, 0x100],
            hex("63 04 03 02 01 ff ef cd ab 00 01"),
        ),
        (
            vec![u32::MAX; 4],
            hex("ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff"),
        ),
    ]
}

#[test]
fn streamvbyte_fixed_reference_bytes_and_output_suffixes() {
    for (values, expected) in compact_vectors() {
        let mut encoded = vec![0xab; expected.len() + 5];
        let n = streamvbyte::encode_into(&values, &mut encoded, CompactLimits::default()).unwrap();
        assert_eq!(n, expected.len());
        assert_eq!(&encoded[..n], expected);
        assert_eq!(&encoded[n..], &[0xab; 5]);
        let mut decoded = vec![0xdeadbeef; values.len() + 5];
        assert_eq!(
            streamvbyte::decode_into(
                &expected,
                values.len(),
                &mut decoded,
                CompactLimits::default()
            )
            .unwrap(),
            values.len()
        );
        assert_eq!(&decoded[..values.len()], values);
        assert_eq!(&decoded[values.len()..], &[0xdeadbeef; 5]);
    }
}

#[test]
fn streamvbyte_every_truncation_and_small_output_preserves_buffers() {
    for (values, encoded) in compact_vectors().into_iter().skip(1) {
        for end in 0..encoded.len() {
            let mut out = vec![0xabababab; values.len()];
            assert!(
                streamvbyte::decode_into(
                    &encoded[..end],
                    values.len(),
                    &mut out,
                    CompactLimits::default()
                )
                .is_err()
            );
            assert!(out.iter().all(|x| *x == 0xabababab));
        }
        for len in 0..values.len() {
            let mut out = vec![0xabababab; len];
            assert_eq!(
                streamvbyte::decode_into(
                    &encoded,
                    values.len(),
                    &mut out,
                    CompactLimits::default()
                )
                .unwrap_err()
                .kind,
                ErrorKind::OutputTooSmall
            );
            assert!(out.iter().all(|x| *x == 0xabababab));
        }
        for len in 0..encoded.len() {
            let mut out = vec![0xab; len];
            assert_eq!(
                streamvbyte::encode_into(&values, &mut out, CompactLimits::default())
                    .unwrap_err()
                    .kind,
                ErrorKind::OutputTooSmall
            );
            assert!(out.iter().all(|x| *x == 0xab));
        }
        let mut with_tail = encoded;
        with_tail.push(0);
        let mut out = vec![0xabababab; values.len()];
        assert_eq!(
            streamvbyte::decode_into(&with_tail, values.len(), &mut out, CompactLimits::default())
                .unwrap_err()
                .kind,
            ErrorKind::TrailingBytes
        );
        assert!(out.iter().all(|x| *x == 0xabababab));
    }
}

#[test]
fn streamvbyte_tails_padding_nonminimal_and_zero_count_are_explicit() {
    for count in 1..=3 {
        let values = vec![0; count];
        let mut canonical = vec![0; count + 1];
        streamvbyte::encode_into(&values, &mut canonical, CompactLimits::default()).unwrap();
        assert_eq!(canonical[0], 0);
        canonical[0] |= 0xff << (count * 2);
        let mut out = vec![99; count];
        streamvbyte::decode_into(&canonical, count, &mut out, CompactLimits::default()).unwrap();
        assert_eq!(out, values); // Unused two-bit tail tags do not describe values.
    }
    let mut out = [99];
    streamvbyte::decode_into(
        &hex("03 01 00 00 00"),
        1,
        &mut out,
        CompactLimits::default(),
    )
    .unwrap();
    assert_eq!(out, [1]); // Non-minimal four-byte representation is interoperable.
    let mut out = [99];
    assert_eq!(
        streamvbyte::decode_into(&[0], 0, &mut out, CompactLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::TrailingBytes
    );
    assert_eq!(out, [99]);
}

// Structurally independent scalar oracle: quotient/remainder decimal-style
// decomposition, separate byte vectors per control group, no leading_zeros or
// LE word conversion and no calls into the implementation under test.
fn reference_streamvbyte(values: &[u32]) -> Vec<u8> {
    let mut tags = Vec::new();
    let mut data = Vec::new();
    for group in values.chunks(4) {
        let mut tag = 0;
        for (i, &value) in group.iter().enumerate() {
            let mut n = u64::from(value);
            let mut bytes = Vec::new();
            loop {
                bytes.push((n % 256) as u8);
                n /= 256;
                if n == 0 {
                    break;
                }
            }
            tag += ((bytes.len() - 1) as u8) * [1, 4, 16, 64][i];
            data.extend(bytes);
        }
        tags.push(tag);
    }
    tags.extend(data);
    tags
}

#[test]
fn streamvbyte_differential_counts_groups_boundaries_and_random_values() {
    let mut state = 0x43211abc123_u64;
    for count in 0..513 {
        let values: Vec<_> = (0..count)
            .map(|i| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                (state as u32) >> ((i % 4) * 8)
            })
            .collect();
        let reference = reference_streamvbyte(&values);
        let mut encoded = vec![0; streamvbyte::max_encoded_size(count).unwrap()];
        let n = streamvbyte::encode_into(&values, &mut encoded, CompactLimits::default()).unwrap();
        assert_eq!(&encoded[..n], reference);
        let mut decoded = vec![0; count];
        streamvbyte::decode_into(&reference, count, &mut decoded, CompactLimits::default())
            .unwrap();
        assert_eq!(decoded, values);
    }
}

#[test]
fn streamvbyte_size_overflow_and_budget_failures_are_transactional() {
    assert_eq!(
        streamvbyte::max_encoded_size(usize::MAX).unwrap_err().kind,
        ErrorKind::Overflow
    );
    let values = [1, 256, 65_536, u32::MAX, 42];
    let reference = reference_streamvbyte(&values);
    let base = CompactLimits::default();
    for (limits, expected) in [
        (
            CompactLimits {
                max_values: 4,
                ..base
            },
            ErrorKind::CountLimit,
        ),
        (
            CompactLimits {
                max_bytes: reference.len() - 1,
                ..base
            },
            ErrorKind::ByteLimit,
        ),
        (
            CompactLimits {
                max_work: 0,
                ..base
            },
            ErrorKind::WorkLimit,
        ),
    ] {
        let mut out = [0xab; 100];
        assert_eq!(
            streamvbyte::encode_into(&values, &mut out, limits)
                .unwrap_err()
                .kind,
            expected
        );
        assert_eq!(out, [0xab; 100]);
        let mut decoded = [0xabababab; 5];
        assert_eq!(
            streamvbyte::decode_into(&reference, 5, &mut decoded, limits)
                .unwrap_err()
                .kind,
            expected
        );
        assert_eq!(decoded, [0xabababab; 5]);
    }
    let exact_work = reference.len() * 2 + values.len() * 2 - 2; // control bytes charged once, payload twice
    let exact = CompactLimits {
        max_work: exact_work,
        max_values: values.len(),
        max_bytes: reference.len(),
    };
    let mut out = [0; 100];
    streamvbyte::encode_into(&values, &mut out, exact).unwrap();
    let mut decoded = [0; 5];
    streamvbyte::decode_into(&reference, 5, &mut decoded, exact).unwrap();
    assert_eq!(decoded, values);
    assert_eq!(
        streamvbyte::encoded_size(
            &values,
            CompactLimits {
                max_work: exact_work - 1,
                ..base
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::WorkLimit
    );
}

// Complete archive vectors computed separately from field-relative addresses.
// The second vector has forward label offsets and negative child-pointer offsets.
fn leaf_vector() -> Vec<u8> {
    hex("56 53 50 41 01 00 01 00 40 00 00 00 01 00 00 00
         10 00 00 00 0c 00 00 00 01 00 00 00 00 00 00 00
         04 03 02 01 00 00 00 00 00 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00")
}
fn tree_vector() -> Vec<u8> {
    hex("56 53 50 41 01 00 01 00 90 00 00 00 03 00 00 00
         10 00 00 00 0c 00 00 00 03 00 00 00 00 00 00 00
         07 00 00 00 5c 00 00 00 01 00 00 00 58 00 00 00
         02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
         08 00 00 00 48 00 00 00 02 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
         09 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
         72 00 00 00 bc ff ff ff d8 ff ff ff c3 a9 00 00")
}
fn tree_nodes() -> [TreeNode<'static>; 3] {
    [
        TreeNode {
            value: 7,
            label: "r",
            children: &[1, 2],
        },
        TreeNode {
            value: 8,
            label: "é",
            children: &[],
        },
        TreeNode {
            value: 9,
            label: "",
            children: &[],
        },
    ]
}
fn encode_tree(nodes: &[TreeNode<'_>]) -> Vec<u8> {
    let limits = archive::Limits::default();
    let mut output = vec![0; archive::encoded_size(nodes, limits).unwrap()];
    archive::encode_into(nodes, &mut output, limits).unwrap();
    output
}
fn word(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn bad_archive(bytes: &[u8]) {
    assert!(Archive::validate(bytes, archive::Limits::default()).is_err());
}

#[test]
fn archive_fixed_reference_bytes_signed_pointers_and_borrowed_views() {
    let leaf = [TreeNode {
        value: 0x01020304,
        label: "",
        children: &[],
    }];
    assert_eq!(encode_tree(&leaf), leaf_vector());
    let nodes = tree_nodes();
    let bytes = encode_tree(&nodes);
    assert_eq!(bytes, tree_vector());
    let archive = Archive::validate(&bytes, archive::Limits::default()).unwrap();
    assert_eq!(archive.len(), 3);
    assert!(!archive.is_empty());
    assert_eq!(archive.as_bytes().as_ptr(), bytes.as_ptr());
    assert_eq!(archive.root().value(), 7);
    assert_eq!(archive.root().label(), "r");
    assert_eq!(archive.root().label_bytes().as_ptr(), bytes[128..].as_ptr());
    let children = archive.root().children();
    assert_eq!(children.len(), 2);
    let children: Vec<_> = children.collect();
    assert_eq!(children[0].index(), 1);
    assert_eq!(children[0].label(), "é");
    assert_eq!(children[1].value(), 9);
    assert_eq!(children[1].label(), "");
    assert_eq!(children[1].children().len(), 0);
    assert!(archive.node(3).is_none());
    let ptr = RelPtr32 { offset: -68 };
    assert_eq!(ptr.resolve(132, 32, 4, 144).unwrap(), 64..96);
}

#[test]
fn archive_logical_alignment_does_not_require_aligned_host_buffers() {
    let mut bytes = vec![0xaa];
    bytes.extend(tree_vector());
    let archive = Archive::validate(&bytes[1..], archive::Limits::default()).unwrap();
    assert_eq!(archive.root().children().next().unwrap().label(), "é");
}

#[test]
fn relative_pointer_boundaries_alignment_and_checked_overflow() {
    assert_eq!(
        RelPtr32::between(0, i32::MAX as usize).unwrap().offset,
        i32::MAX
    );
    assert_eq!(
        RelPtr32::between(i32::MAX as usize + 1, 0).unwrap().offset,
        i32::MIN
    );
    assert_eq!(
        RelPtr32::between(0, i32::MAX as usize + 1)
            .unwrap_err()
            .kind,
        ErrorKind::Overflow
    );
    assert_eq!(
        RelPtr32::between(usize::MAX, 0).unwrap_err().kind,
        ErrorKind::Overflow
    );
    assert_eq!(
        RelPtr32 { offset: i32::MIN }
            .resolve(0, 4, 4, 64)
            .unwrap_err()
            .kind,
        ErrorKind::Offset
    );
    assert_eq!(
        RelPtr32 { offset: 1 }
            .resolve(0, 4, 4, 64)
            .unwrap_err()
            .kind,
        ErrorKind::Alignment
    );
    assert_eq!(
        RelPtr32 { offset: 0 }
            .resolve(1, 4, 4, 64)
            .unwrap_err()
            .kind,
        ErrorKind::Alignment
    );
    assert_eq!(
        RelPtr32 { offset: 0 }
            .resolve(0, 4, 0, 64)
            .unwrap_err()
            .kind,
        ErrorKind::Alignment
    );
    assert_eq!(
        RelPtr32 { offset: 0 }
            .resolve(0, 4, 3, 64)
            .unwrap_err()
            .kind,
        ErrorKind::Alignment
    );
    assert!(
        RelPtr32 { offset: i32::MAX }
            .resolve(usize::MAX - 3, 4, 1, usize::MAX)
            .is_err()
    );
    assert_eq!(
        RelPtr32 { offset: 4 }
            .resolve(0, usize::MAX, 1, usize::MAX)
            .unwrap_err()
            .kind,
        ErrorKind::Overflow
    );
    assert_eq!(
        RelSlice32 {
            offset: 0,
            count: u32::MAX
        }
        .resolve(0, usize::MAX, 1, usize::MAX)
        .unwrap_err()
        .kind,
        ErrorKind::Overflow
    );
    assert_eq!(
        RelSlice32 {
            offset: 1,
            count: 0
        }
        .resolve(0, 1, 1, 64)
        .unwrap_err()
        .kind,
        ErrorKind::Layout
    );
    assert_eq!(
        RelSlice32 {
            offset: 0,
            count: 0
        }
        .resolve(0, 1, 1, 64)
        .unwrap(),
        0..0
    );
    assert_eq!(
        RelSlice32 {
            offset: 0,
            count: 0
        }
        .resolve(0, 0, 1, 64)
        .unwrap_err()
        .kind,
        ErrorKind::Layout
    );
}

#[test]
fn archive_every_truncation_split_and_small_output_is_transactional() {
    let bytes = tree_vector();
    for end in 0..bytes.len() {
        bad_archive(&bytes[..end]);
    }
    for split in 0..=bytes.len() {
        let joined = [&bytes[..split], &bytes[split..]].concat();
        assert_eq!(
            Archive::validate(&joined, archive::Limits::default())
                .unwrap()
                .len(),
            3
        );
    }
    for n in 0..bytes.len() {
        let nodes = tree_nodes();
        let mut out = vec![0xab; n];
        assert_eq!(
            archive::encode_into(&nodes, &mut out, archive::Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::OutputTooSmall
        );
        assert!(out.iter().all(|b| *b == 0xab));
    }
    let nodes = tree_nodes();
    let mut out = vec![0xab; bytes.len() + 9];
    let n = archive::encode_into(&nodes, &mut out, archive::Limits::default()).unwrap();
    assert_eq!(out[..n], bytes);
    assert_eq!(out[n..], [0xab; 9]);
}

#[test]
fn archive_rejects_header_schema_reserved_padding_utf8_and_ranges() {
    let original = tree_vector();
    for (offset, replacement) in [
        (0, 0),
        (4, 2),
        (6, 2),
        (28, 1),
        (52, 1),
        (56, 1),
        (60, 1),
        (129, 1),
        (142, 1),
        (140, 0xff),
        (141, 0xff),
    ] {
        let mut bytes = original.clone();
        bytes[offset] = replacement;
        bad_archive(&bytes);
    }
    for (offset, value) in [
        (8, 143),
        (8, 145),
        (12, 0),
        (12, u32::MAX),
        (24, 2),
        (16, i32::MIN as u32),
        (16, 17),
        (16, 48),
        (20, 0),
        (20, i32::MAX as u32),
        (36, 0),
        (36, i32::MIN as u32),
        (40, u32::MAX),
        (44, 89),
        (44, 0),
        (48, u32::MAX),
        (100, 1), // noncanonical empty label offset
    ] {
        let mut bytes = original.clone();
        word(&mut bytes, offset, value);
        bad_archive(&bytes);
    }
    let mut trailing = original.clone();
    trailing.extend([0; 4]);
    bad_archive(&trailing);
    word(&mut trailing, 8, (original.len() + 4) as u32);
    bad_archive(&trailing); // Even a matching length cannot hide unreachable bytes.
}

#[test]
fn archive_rejects_cycles_shared_children_orphans_and_non_node_targets() {
    let original = tree_vector();
    for (entry, target) in [(132, 32), (132, 68), (136, 64), (136, 0)] {
        let mut bytes = original.clone();
        word(
            &mut bytes,
            entry,
            RelPtr32::between(entry, target).unwrap().offset as u32,
        );
        bad_archive(&bytes);
    }
    // The encoder checks the same schema before touching a caller byte.
    for children in [&[0_u32][..], &[1, 1], &[2], &[3]] {
        let mut nodes = tree_nodes();
        nodes[0].children = children;
        let mut out = [0xab; 256];
        assert_eq!(
            archive::encode_into(&nodes, &mut out, archive::Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::Tree
        );
        assert_eq!(out, [0xab; 256]);
    }
    assert_eq!(
        archive::encoded_size(&[], archive::Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Tree
    );
}

#[test]
fn archive_all_budgets_are_checked_on_encode_and_actual_byte_validation() {
    let nodes = tree_nodes();
    let bytes = tree_vector();
    let base = archive::Limits::default();
    for (limits, expected) in [
        (
            archive::Limits {
                max_archive_bytes: 143,
                ..base
            },
            ErrorKind::ByteLimit,
        ),
        (
            archive::Limits {
                max_nodes: 2,
                ..base
            },
            ErrorKind::CountLimit,
        ),
        (
            archive::Limits {
                max_children_per_node: 1,
                ..base
            },
            ErrorKind::CountLimit,
        ),
        (
            archive::Limits {
                max_edges: 1,
                ..base
            },
            ErrorKind::CountLimit,
        ),
        (
            archive::Limits {
                max_depth: 1,
                ..base
            },
            ErrorKind::DepthLimit,
        ),
        (
            archive::Limits {
                max_label_bytes: 1,
                ..base
            },
            ErrorKind::StringLimit,
        ),
        (
            archive::Limits {
                max_total_label_bytes: 2,
                ..base
            },
            ErrorKind::StringLimit,
        ),
        (
            archive::Limits {
                max_validation_bytes: 0,
                ..base
            },
            ErrorKind::AllocationLimit,
        ),
        (
            archive::Limits {
                max_work: 0,
                ..base
            },
            ErrorKind::WorkLimit,
        ),
    ] {
        let mut out = [0xab; 256];
        assert_eq!(
            archive::encode_into(&nodes, &mut out, limits)
                .unwrap_err()
                .kind,
            expected
        );
        assert_eq!(out, [0xab; 256]);
        assert_eq!(
            Archive::validate(&bytes, limits).unwrap_err().kind,
            expected
        );
    }
    let exact = archive::Limits {
        max_archive_bytes: 144,
        max_nodes: 3,
        max_children_per_node: 2,
        max_edges: 2,
        max_depth: 2,
        max_label_bytes: 2,
        max_total_label_bytes: 3,
        ..base
    };
    assert_eq!(archive::encoded_size(&nodes, exact).unwrap(), 144);
    Archive::validate(&bytes, exact).unwrap();
}

#[test]
fn archive_roundtrip_many_real_trees_and_deep_iterative_depth_bounds() {
    for count in 1..100 {
        let labels: Vec<_> = (0..count).map(|i| format!("node {i} é🦀")).collect();
        let children: Vec<Vec<u32>> = (0..count)
            .map(|i| {
                [(i * 2 + 1) as u32, (i * 2 + 2) as u32]
                    .into_iter()
                    .filter(|x| (*x as usize) < count)
                    .collect()
            })
            .collect();
        let nodes: Vec<_> = (0..count)
            .map(|i| TreeNode {
                value: i as u32,
                label: &labels[i],
                children: &children[i],
            })
            .collect();
        let bytes = encode_tree(&nodes);
        let archive = Archive::validate(&bytes, archive::Limits::default()).unwrap();
        for i in 0..count {
            let node = archive.node(i).unwrap();
            assert_eq!(node.value(), i as u32);
            assert_eq!(node.label(), labels[i]);
            assert_eq!(
                node.children()
                    .map(|c| c.index() as u32)
                    .collect::<Vec<_>>(),
                children[i]
            );
        }
    }
    let children: Vec<Vec<u32>> = (0..200)
        .map(|i| if i < 199 { vec![i + 1] } else { vec![] })
        .collect();
    let nodes: Vec<_> = children
        .iter()
        .map(|children| TreeNode {
            value: 1,
            label: "",
            children,
        })
        .collect();
    assert_eq!(
        archive::encoded_size(&nodes, archive::Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::DepthLimit
    );
    let limits = archive::Limits {
        max_depth: 200,
        ..archive::Limits::default()
    };
    let mut bytes = vec![0; archive::encoded_size(&nodes, limits).unwrap()];
    archive::encode_into(&nodes, &mut bytes, limits).unwrap();
    assert_eq!(
        Archive::validate(&bytes, archive::Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::DepthLimit
    );
    assert_eq!(Archive::validate(&bytes, limits).unwrap().len(), 200);
}

#[test]
fn archive_mutated_bytes_never_publish_an_unchecked_borrowed_view() {
    let original = tree_vector();
    for position in 0..original.len() {
        for byte in [0, 1, 3, 0x7f, 0x80, 0xff] {
            let mut candidate = original.clone();
            candidate[position] = byte;
            if let Ok(archive) = Archive::validate(&candidate, archive::Limits::default()) {
                for i in 0..archive.len() {
                    let node = archive.node(i).unwrap();
                    let _ = node.value();
                    let _ = node.label();
                    for child in node.children() {
                        assert!(child.index() < archive.len());
                    }
                }
            }
        }
    }
}

#[test]
fn archive_metadata_and_work_boundaries_are_explicit() {
    let nodes = tree_nodes();
    let bytes = tree_vector();
    let base = archive::Limits::default();
    let allocation = archive::validation_bytes(3).unwrap();
    assert_eq!(
        archive::validation_bytes(usize::MAX).unwrap_err().kind,
        ErrorKind::Overflow
    );
    let exact_alloc = archive::Limits {
        max_validation_bytes: allocation,
        ..base
    };
    archive::encoded_size(&nodes, exact_alloc).unwrap();
    Archive::validate(&bytes, exact_alloc).unwrap();
    let short_alloc = archive::Limits {
        max_validation_bytes: allocation - 1,
        ..base
    };
    assert_eq!(
        archive::encoded_size(&nodes, short_alloc).unwrap_err().kind,
        ErrorKind::AllocationLimit
    );
    assert_eq!(
        Archive::validate(&bytes, short_alloc).unwrap_err().kind,
        ErrorKind::AllocationLimit
    );
    let encode_work = 3 + 3 * 32 + 3 + 2 * 8 + 2 * 144;
    let validate_work = 3 + 3 * 32 + 3 + 2 * 8 + 144;
    archive::encoded_size(
        &nodes,
        archive::Limits {
            max_work: encode_work,
            ..base
        },
    )
    .unwrap();
    assert_eq!(
        archive::encoded_size(
            &nodes,
            archive::Limits {
                max_work: encode_work - 1,
                ..base
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::WorkLimit
    );
    Archive::validate(
        &bytes,
        archive::Limits {
            max_work: validate_work,
            ..base
        },
    )
    .unwrap();
    assert_eq!(
        Archive::validate(
            &bytes,
            archive::Limits {
                max_work: validate_work - 1,
                ..base
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::WorkLimit
    );
}

#[test]
fn archive_hostile_count_is_rejected_against_actual_bytes_before_allocation() {
    let mut bytes = leaf_vector();
    word(&mut bytes, 12, u32::MAX);
    word(&mut bytes, 24, u32::MAX);
    // Even absurd caller allocation/count budgets cannot make a short header
    // trigger huge metadata initialization: the node-byte range is checked first.
    let limits = archive::Limits {
        max_nodes: usize::MAX,
        max_validation_bytes: usize::MAX,
        max_work: usize::MAX,
        ..archive::Limits::default()
    };
    assert!(Archive::validate(&bytes, limits).is_err());
}

#[test]
fn streamvbyte_every_width_boundary_and_all_control_bytes() {
    let boundaries = [
        0,
        1,
        255,
        256,
        65_535,
        65_536,
        16_777_215,
        16_777_216,
        u32::MAX,
    ];
    for count in 0..=boundaries.len() {
        let values = &boundaries[..count];
        let reference = reference_streamvbyte(values);
        let mut output = vec![0; reference.len()];
        streamvbyte::encode_into(values, &mut output, CompactLimits::default()).unwrap();
        assert_eq!(output, reference);
    }
    for control in 0..=u8::MAX {
        let mut bytes = vec![control];
        let mut expected = Vec::new();
        for lane in 0..4 {
            let width = ((control >> (lane * 2)) & 3) + 1;
            expected.push(if width == 4 {
                u32::MAX
            } else {
                (1_u32 << (width * 8)) - 1
            });
            bytes.extend(std::iter::repeat_n(0xff, width as usize));
        }
        let mut output = [0; 4];
        streamvbyte::decode_into(&bytes, 4, &mut output, CompactLimits::default()).unwrap();
        assert_eq!(output.as_slice(), expected);
        let mut encoded = vec![0; bytes.len()];
        streamvbyte::encode_into(&output, &mut encoded, CompactLimits::default()).unwrap();
        assert_eq!(encoded, bytes);
    }
}
