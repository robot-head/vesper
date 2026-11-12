use vesper_grammar::{
    ErrorKind,
    arrow::{
        AlignmentProfile as Profile, BufferDesc as Desc, FieldNode as Node, I64Column, Limits,
        checked_buffer_range,
    },
};
fn descriptor(offset: usize, length: usize) -> Desc {
    Desc {
        offset_bytes: offset as i64,
        byte_length: length as i64,
    }
}
fn fixture() -> Vec<u8> {
    let mut body = vec![0; 88];
    body[0] = 0b101;
    for (i, value) in [10_i64, i64::MAX, -4].iter().enumerate() {
        body[64 + i * 8..72 + i * 8].copy_from_slice(&value.to_le_bytes());
    }
    body
}
fn column(body: &[u8]) -> I64Column<'_> {
    I64Column::validate(
        body,
        Node {
            row_length: 3,
            null_count: 1,
        },
        descriptor(0, 1),
        descriptor(64, 24),
        Profile::Relative64,
        Limits::default(),
    )
    .unwrap()
}
#[test]
fn actual_nullable_i64_bytes_null_count_and_checked_sum() {
    let body = fixture();
    let col = column(&body);
    assert_eq!(col.len(), 3);
    assert_eq!(col.null_count(), 1);
    assert_eq!(col.get(0), Some(Some(10)));
    assert_eq!(col.get(1), Some(None));
    assert_eq!(col.get(2), Some(Some(-4)));
    assert_eq!(col.get(3), None);
    assert_eq!(col.checked_sum(3).unwrap(), Some(6));
    assert_eq!(col.checked_sum(2).unwrap_err().kind, ErrorKind::WorkLimit);
    assert_eq!(col.values_bytes().as_ptr(), body[64..].as_ptr());
    assert_eq!(col.validity_bytes().unwrap().as_ptr(), body.as_ptr());
    assert_eq!(
        Desc::from_wire(&[0; 15]).unwrap_err().kind,
        ErrorKind::Descriptor
    );
    let node = Node::from_wire(&[3, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]).unwrap();
    assert_eq!(node.row_length, 3);
    assert_eq!(node.null_count, 1);
    let desc = Desc::from_wire(&[64, 0, 0, 0, 0, 0, 0, 0, 24, 0, 0, 0, 0, 0, 0, 0]).unwrap();
    assert_eq!(desc, descriptor(64, 24));
}
#[test]
fn empty_all_null_absent_bitmap_and_padding_bits_are_explicit() {
    let body = [0; 16];
    let empty = I64Column::validate(
        &[],
        Node {
            row_length: 0,
            null_count: 0,
        },
        descriptor(0, 0),
        descriptor(0, 0),
        Profile::ByteCopy,
        Limits::default(),
    )
    .unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty.checked_sum(0).unwrap(), None);
    let allnull = I64Column::validate(
        &body,
        Node {
            row_length: 1,
            null_count: 1,
        },
        descriptor(0, 1),
        descriptor(8, 8),
        Profile::Relative8,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(allnull.checked_sum(1).unwrap(), None);
    let mut body = fixture();
    body[0] = 0xff;
    let col = I64Column::validate(
        &body,
        Node {
            row_length: 3,
            null_count: 0,
        },
        descriptor(0, 2),
        descriptor(64, 24),
        Profile::Relative64,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(col.null_count(), 0); // bitmap padding bits/bytes ignored
    let col = I64Column::validate(
        &body,
        Node {
            row_length: 3,
            null_count: 0,
        },
        descriptor(0, 0),
        descriptor(64, 24),
        Profile::Relative64,
        Limits::default(),
    )
    .unwrap();
    assert!(col.validity_bytes().is_none());
}
#[test]
fn negative_descriptors_unknown_nulls_wrong_counts_and_bitmap_truncation_fail() {
    let body = fixture();
    let good = Node {
        row_length: 3,
        null_count: 1,
    };
    let validity = descriptor(0, 1);
    let values = descriptor(64, 24);
    for node in [
        Node {
            row_length: -1,
            null_count: 0,
        },
        Node {
            row_length: 3,
            null_count: -1,
        },
        Node {
            row_length: 3,
            null_count: 4,
        },
        Node {
            row_length: 3,
            null_count: 2,
        },
    ] {
        assert!(
            I64Column::validate(
                &body,
                node,
                validity,
                values,
                Profile::ByteCopy,
                Limits::default()
            )
            .is_err()
        );
    }
    for desc in [
        Desc {
            offset_bytes: -1,
            byte_length: 1,
        },
        Desc {
            offset_bytes: 0,
            byte_length: -1,
        },
        Desc {
            offset_bytes: i64::MAX,
            byte_length: i64::MAX,
        },
    ] {
        assert!(
            I64Column::validate(
                &body,
                good,
                desc,
                values,
                Profile::ByteCopy,
                Limits::default()
            )
            .is_err()
        );
    }
    assert_eq!(
        I64Column::validate(
            &body,
            good,
            descriptor(0, 0),
            values,
            Profile::ByteCopy,
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::Bitmap
    );
    assert_eq!(
        I64Column::validate(
            &body,
            Node {
                row_length: 9,
                null_count: 9
            },
            descriptor(0, 1),
            descriptor(8, 72),
            Profile::ByteCopy,
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::Bitmap
    );
    assert_eq!(
        I64Column::validate(
            &body[..87],
            good,
            validity,
            values,
            Profile::Relative64,
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::Range
    );
    assert_eq!(
        I64Column::validate(
            &body,
            good,
            validity,
            descriptor(64, 23),
            Profile::Relative64,
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::Range
    );
}
#[test]
fn alignment_checks_distinguish_relative_offsets_from_actual_host_addresses() {
    #[repr(align(64))]
    struct Aligned([u8; 192]);
    let mut aligned = Aligned([0; 192]);
    aligned.0[..88].copy_from_slice(&fixture());
    I64Column::validate(
        &aligned.0[..88],
        Node {
            row_length: 3,
            null_count: 1,
        },
        descriptor(0, 1),
        descriptor(64, 24),
        Profile::HostAddress64,
        Limits::default(),
    )
    .unwrap();
    let mut shifted = Aligned([0; 192]);
    shifted.0[1..89].copy_from_slice(&fixture());
    column(&shifted.0[1..89]); // Same relative offsets, unaligned absolute base.
    assert_eq!(
        I64Column::validate(
            &shifted.0[1..89],
            Node {
                row_length: 3,
                null_count: 1
            },
            descriptor(0, 1),
            descriptor(64, 24),
            Profile::HostAddress64,
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::Alignment
    );
    let body = [0; 24];
    assert!(
        I64Column::validate(
            &body,
            Node {
                row_length: 1,
                null_count: 0
            },
            descriptor(0, 0),
            descriptor(1, 8),
            Profile::ByteCopy,
            Limits::default()
        )
        .is_ok()
    );
    assert_eq!(
        I64Column::validate(
            &body,
            Node {
                row_length: 1,
                null_count: 0
            },
            descriptor(0, 0),
            descriptor(1, 8),
            Profile::Relative8,
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::Alignment
    );
}
#[test]
fn overlapping_buffers_limits_checked_sizes_and_arithmetic_overflow_fail() {
    let body = fixture();
    assert_eq!(
        I64Column::validate(
            &body,
            Node {
                row_length: 3,
                null_count: 0
            },
            descriptor(64, 1),
            descriptor(64, 24),
            Profile::ByteCopy,
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::Aliasing
    );
    for (limits, expected) in [
        (
            Limits {
                max_body_bytes: 87,
                ..Limits::default()
            },
            ErrorKind::InputLimit,
        ),
        (
            Limits {
                max_rows: 2,
                ..Limits::default()
            },
            ErrorKind::CountLimit,
        ),
        (
            Limits {
                max_work: 2,
                ..Limits::default()
            },
            ErrorKind::WorkLimit,
        ),
    ] {
        assert_eq!(
            I64Column::validate(
                &body,
                Node {
                    row_length: 3,
                    null_count: 1
                },
                descriptor(0, 1),
                descriptor(64, 24),
                Profile::Relative64,
                limits
            )
            .unwrap_err()
            .kind,
            expected
        );
    }
    assert_eq!(
        I64Column::validate(
            &[],
            Node {
                row_length: i64::MAX,
                null_count: 0
            },
            descriptor(0, 0),
            descriptor(0, 0),
            Profile::ByteCopy,
            Limits {
                max_rows: usize::MAX,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Overflow
    );
    let body = [i64::MAX.to_le_bytes(), 1_i64.to_le_bytes()].concat();
    let col = I64Column::validate(
        &body,
        Node {
            row_length: 2,
            null_count: 0,
        },
        descriptor(0, 0),
        descriptor(0, 16),
        Profile::Relative8,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(col.checked_sum(2).unwrap_err().kind, ErrorKind::Overflow);
    assert!(
        checked_buffer_range(
            u64::MAX,
            Desc {
                offset_bytes: -1,
                byte_length: 0
            },
            1
        )
        .is_err()
    );
    assert!(checked_buffer_range(u64::MAX, descriptor(0, 0), 0).is_err());
    #[cfg(target_pointer_width = "32")]
    assert_eq!(
        checked_buffer_range(
            u64::MAX,
            Desc {
                offset_bytes: i64::MAX,
                byte_length: 0
            },
            1
        )
        .unwrap_err()
        .kind,
        ErrorKind::Overflow
    );
}
#[test]
fn all_nullable_bitmap_combinations_match_independent_sum_oracle() {
    for rows in 0..=9 {
        for pattern in 0..(1usize << rows) {
            let mut body = vec![0; 64 + rows * 8];
            let mut expected = 0;
            let mut valid = 0;
            for row in 0..rows {
                let value = row as i64 - 4;
                body[64 + row * 8..72 + row * 8].copy_from_slice(&value.to_le_bytes());
                if pattern & (1 << row) != 0 {
                    body[row / 8] |= 1 << (row % 8);
                    valid += 1;
                    expected += value;
                }
            }
            let col = I64Column::validate(
                &body,
                Node {
                    row_length: rows as i64,
                    null_count: (rows - valid) as i64,
                },
                descriptor(0, rows.div_ceil(8)),
                descriptor(64, rows * 8),
                Profile::Relative64,
                Limits::default(),
            )
            .unwrap();
            assert_eq!(
                col.checked_sum(rows).unwrap(),
                if valid == 0 { None } else { Some(expected) }
            );
        }
    }
}

#[test]
fn checked_nullable_aggregation_matches_i128_prefix_oracle() {
    let cases = [
        vec![i64::MAX, 1, -1],
        vec![i64::MIN, -1, 1],
        vec![i64::MAX, -1, -i64::MAX],
        vec![i64::MIN, i64::MAX, 1],
    ];
    for values in cases {
        for mask in 0..8 {
            let mut body = vec![0; 88];
            body[0] = mask;
            let mut wide = 0_i128;
            let mut overflow = false;
            let mut valid = 0;
            for (row, value) in values.iter().enumerate() {
                body[64 + row * 8..72 + row * 8].copy_from_slice(&value.to_le_bytes());
                if mask & (1 << row) != 0 {
                    valid += 1;
                    wide += i128::from(*value);
                    overflow |= wide < i128::from(i64::MIN) || wide > i128::from(i64::MAX);
                }
            }
            let column = I64Column::validate(
                &body,
                Node {
                    row_length: 3,
                    null_count: 3 - valid,
                },
                descriptor(0, 1),
                descriptor(64, 24),
                Profile::Relative64,
                Limits::default(),
            )
            .unwrap();
            if overflow {
                assert_eq!(column.checked_sum(3).unwrap_err().kind, ErrorKind::Overflow);
            } else {
                assert_eq!(
                    column.checked_sum(3).unwrap(),
                    if valid == 0 { None } else { Some(wide as i64) }
                );
            }
        }
    }
    let body = fixture();
    for end in 0..body.len() {
        assert!(
            I64Column::validate(
                &body[..end],
                Node {
                    row_length: 3,
                    null_count: 1
                },
                descriptor(0, 1),
                descriptor(64, 24),
                Profile::Relative64,
                Limits::default()
            )
            .is_err()
        );
    }
}
