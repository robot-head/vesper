use vesper_grammar::{
    ErrorKind,
    framing::{self, Limits},
};

fn vectors() -> Vec<(u64, Vec<u8>)> {
    vec![
        (0, vec![0]),
        (1, vec![1]),
        (127, vec![0x7f]),
        (128, vec![0x80, 1]),
        (255, vec![0xff, 1]),
        (624485, vec![0xe5, 0x8e, 0x26]),
        (u32::MAX as u64, vec![0xff, 0xff, 0xff, 0xff, 0x0f]),
        (
            u64::MAX,
            vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1],
        ),
    ]
}
#[test]
fn known_uleb_vectors_exact_prefix_and_transactional_output() {
    for (value, bytes) in vectors() {
        assert_eq!(
            framing::decode_canonical_u64_leb128(&bytes).unwrap(),
            (value, bytes.len())
        );
        assert_eq!(
            framing::parse_u64_exact(&bytes, Limits::default()).unwrap(),
            value
        );
        let mut output = [0xab; 16];
        let n = framing::encode_u64_into(value, &mut output, Limits::default()).unwrap();
        assert_eq!(&output[..n], bytes);
        assert!(output[n..].iter().all(|b| *b == 0xab));
        for len in 0..n {
            let mut output = vec![0xab; len];
            assert_eq!(
                framing::encode_u64_into(value, &mut output, Limits::default())
                    .unwrap_err()
                    .kind,
                ErrorKind::OutputTooSmall
            );
            assert!(output.iter().all(|b| *b == 0xab));
            assert!(framing::parse_u64_exact(&bytes[..len], Limits::default()).is_err());
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(
            framing::parse_u64_exact(&extra, Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::TrailingBytes
        );
        let (same, tail) = framing::parse_u64(&extra, Limits::default()).unwrap();
        assert_eq!(same, value);
        let error = tail.assert_exact_eof().unwrap_err();
        assert_eq!(error.tail.remaining_bytes(), [0]);
        assert_eq!(error.tail.offset(), bytes.len());
    }
}
#[test]
fn all_overlong_and_tenth_byte_overflow_cases() {
    for byte in 0..=127 {
        assert_eq!(
            framing::decode_canonical_u64_leb128(&[byte | 128, 0])
                .unwrap_err()
                .kind,
            ErrorKind::NonCanonical
        );
    }
    for length in 2..=10 {
        let mut bytes = vec![0x80; length];
        bytes[length - 1] = 0;
        assert_eq!(
            framing::decode_canonical_u64_leb128(&bytes)
                .unwrap_err()
                .kind,
            ErrorKind::NonCanonical
        );
    }
    for tenth in 2..=255 {
        let mut bytes = vec![0xff; 10];
        bytes[9] = tenth;
        assert_eq!(
            framing::decode_canonical_u64_leb128(&bytes)
                .unwrap_err()
                .kind,
            ErrorKind::Overflow
        );
    }
    assert_eq!(
        framing::decode_canonical_u64_leb128(&[0x80; 11])
            .unwrap_err()
            .kind,
        ErrorKind::Overflow
    );
    assert_eq!(
        framing::decode_canonical_u64_leb128(&[0x80; 9])
            .unwrap_err()
            .kind,
        ErrorKind::UnexpectedEof
    );
}
// Independent division/remainder oracle; does not use implementation helpers.
fn reference(mut value: u64) -> Vec<u8> {
    let mut b = Vec::new();
    loop {
        let low = (value % 128) as u8;
        value /= 128;
        b.push(low + if value != 0 { 128 } else { 0 });
        if value == 0 {
            return b;
        }
    }
}
#[test]
fn differential_varints_and_whole_encoding_uniqueness_on_all_short_inputs() {
    let mut state = 0xabcd12345_u64;
    for _ in 0..20_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let expected = reference(state);
        let mut out = [0; 10];
        let len = framing::encode_u64_into(state, &mut out, Limits::default()).unwrap();
        assert_eq!(&out[..len], expected);
        assert_eq!(
            framing::parse_u64_exact(&expected, Limits::default()).unwrap(),
            state
        );
    }
    // Every accepted exact one/two-byte input reserializes to identical bytes.
    for first in 0..=255 {
        for second in 0..=255 {
            let bytes = [first, second];
            if let Ok(value) = framing::parse_u64_exact(&bytes, Limits::default()) {
                assert_eq!(reference(value), bytes);
            }
        }
    }
}
#[test]
fn dependent_frames_remainders_failure_recovery_and_exact_eof() {
    let bytes = b"\x03abc\x02de";
    let first = framing::parse_frame(bytes, Limits::default()).unwrap();
    assert_eq!(first.payload, b"abc");
    assert_eq!(first.payload.as_ptr(), bytes[1..].as_ptr());
    let failure = first.tail.assert_exact_eof().unwrap_err();
    assert_eq!(failure.tail.offset(), 4);
    let failure = failure
        .tail
        .next_frame(Limits {
            max_payload_bytes: 1,
            ..Limits::default()
        })
        .unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::PayloadLimit);
    assert_eq!(failure.tail.offset(), 4);
    assert_eq!(failure.tail.remaining_bytes(), b"\x02de");
    let second = failure.tail.next_frame(Limits::default()).unwrap();
    assert_eq!(second.payload, b"de");
    second.tail.assert_exact_eof().unwrap();
    assert_eq!(
        framing::parse_frame_exact(bytes, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::TrailingBytes
    );
    assert_eq!(
        framing::parse_frame_exact(b"\x00", Limits::default()).unwrap(),
        b""
    );
    for malformed in [&b"\x80\x00"[..], &b"\x04abc"[..], &b""[..]] {
        assert!(framing::parse_frame_exact(malformed, Limits::default()).is_err());
    }
    let oversized = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1];
    assert!(
        framing::parse_frame(
            &oversized,
            Limits {
                max_payload_bytes: usize::MAX,
                ..Limits::default()
            }
        )
        .is_err()
    );
}
#[test]
fn every_frame_truncation_split_and_output_limit_preserves_buffers() {
    let payload: Vec<_> = (0..=255).collect();
    let size = 258;
    let mut bytes = vec![0xab; size + 4];
    assert_eq!(
        framing::encode_frame_into(&payload, &mut bytes, Limits::default()).unwrap(),
        size
    );
    assert_eq!(&bytes[..2], [0x80, 2]);
    assert_eq!(&bytes[2..size], payload);
    assert_eq!(&bytes[size..], [0xab; 4]);
    for end in 0..size {
        assert!(framing::parse_frame_exact(&bytes[..end], Limits::default()).is_err());
    }
    for split in 0..=size {
        let joined = [&bytes[..split], &bytes[split..size]].concat();
        assert_eq!(
            framing::parse_frame_exact(&joined, Limits::default()).unwrap(),
            payload
        );
    }
    for len in 0..size {
        let mut out = vec![0xab; len];
        assert!(framing::encode_frame_into(&payload, &mut out, Limits::default()).is_err());
        assert!(out.iter().all(|b| *b == 0xab));
    }
    for (limits, expected) in [
        (
            Limits {
                max_output_bytes: size - 1,
                ..Limits::default()
            },
            ErrorKind::OutputLimit,
        ),
        (
            Limits {
                max_payload_bytes: 255,
                ..Limits::default()
            },
            ErrorKind::PayloadLimit,
        ),
        (
            Limits {
                max_work: size - 1,
                ..Limits::default()
            },
            ErrorKind::WorkLimit,
        ),
    ] {
        let mut out = [0xab; 300];
        assert_eq!(
            framing::encode_frame_into(&payload, &mut out, limits)
                .unwrap_err()
                .kind,
            expected
        );
        assert_eq!(out, [0xab; 300]);
    }
    assert_eq!(
        framing::parse_frame_exact(
            &bytes[..size],
            Limits {
                max_input_bytes: size - 1,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::InputLimit
    );
    assert_eq!(
        framing::parse_frame_exact(
            &bytes[..size],
            Limits {
                max_work: size - 1,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::WorkLimit
    );
    assert_eq!(
        framing::parse_frame_exact(
            &bytes[..size],
            Limits {
                max_work: size,
                ..Limits::default()
            }
        )
        .unwrap(),
        payload
    );
}

#[test]
fn u24_big_endian_frames_roundtrip_and_preserve_remainder() {
    let bytes = b"\x00\x00\x03abc\x00\x00\x02de";
    let first = framing::parse_u24_frame(bytes, Limits::default()).unwrap();
    assert_eq!(first.payload, b"abc");
    assert_eq!(first.tail.offset(), 6);
    let fail = first
        .tail
        .next_u24_frame(Limits {
            max_work: 2,
            ..Limits::default()
        })
        .unwrap_err();
    assert_eq!(fail.error.kind, ErrorKind::WorkLimit);
    assert_eq!(fail.tail.offset(), 6);
    let second = fail.tail.next_u24_frame(Limits::default()).unwrap();
    assert_eq!(second.payload, b"de");
    second.tail.assert_exact_eof().unwrap();
    assert_eq!(
        framing::parse_u24_frame_exact(bytes, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::TrailingBytes
    );
    assert_eq!(
        framing::parse_u24_frame_exact(b"\0\0\0", Limits::default()).unwrap(),
        b""
    );
    let mut out = [0xaa; 9];
    assert_eq!(
        framing::encode_u24_frame_into(b"abc", &mut out, Limits::default()).unwrap(),
        6
    );
    assert_eq!(&out, b"\0\0\x03abc\xaa\xaa\xaa");
}

#[test]
fn u24_boundaries_fail_transactionally() {
    for input in [&b""[..], &b"\0"[..], &b"\0\0"[..], &b"\0\0\x02x"[..]] {
        assert_eq!(
            framing::parse_u24_frame_exact(input, Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::UnexpectedEof
        );
    }
    let payload = vec![7; 0x10000];
    let mut output = vec![0xaa; payload.len() + 3];
    let limits = Limits {
        max_payload_bytes: payload.len(),
        max_output_bytes: output.len(),
        max_work: output.len(),
        ..Limits::default()
    };
    assert_eq!(
        framing::encode_u24_frame_into(&payload, &mut output, limits).unwrap(),
        output.len()
    );
    assert_eq!(&output[..3], &[1, 0, 0]);
    assert_eq!(
        framing::parse_u24_frame_exact(&output, limits).unwrap(),
        payload
    );
    for limits in [
        Limits {
            max_payload_bytes: 2,
            ..Limits::default()
        },
        Limits {
            max_output_bytes: 5,
            ..Limits::default()
        },
        Limits {
            max_work: 5,
            ..Limits::default()
        },
    ] {
        let mut out = [0x5a; 10];
        assert!(framing::encode_u24_frame_into(b"abc", &mut out, limits).is_err());
        assert_eq!(out, [0x5a; 10]);
    }
    let mut short = [0x5a; 5];
    assert_eq!(
        framing::encode_u24_frame_into(b"abc", &mut short, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::OutputTooSmall
    );
    assert_eq!(short, [0x5a; 5]);
    let too_large = vec![0; framing::MAX_U24_PAYLOAD + 1];
    let mut sentinel = [0x5a; 1];
    assert_eq!(
        framing::encode_u24_frame_into(&too_large, &mut sentinel, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Overflow
    );
    assert_eq!(sentinel, [0x5a; 1]);
}

#[test]
fn capnp_stream_segments_are_borrowed_and_framing_is_exact() {
    use vesper_grammar::capnp;
    // Two segments: one word and two words. Padding has no prescribed value.
    let mut input = vec![1, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0xab, 0xcd, 0xef, 0x12];
    input.extend(0u8..24);
    input.extend_from_slice(b"tail");
    let (message, tail) = capnp::parse_prefix(&input, capnp::Limits::default()).unwrap();
    assert_eq!(message.segment_count(), 2);
    let mut segments = message.segments();
    assert_eq!(segments.len(), 2);
    let first = segments.next().unwrap();
    assert_eq!(first, &input[16..24]);
    assert_eq!(first.as_ptr(), input[16..].as_ptr());
    assert_eq!(segments.len(), 1);
    assert_eq!(segments.next().unwrap(), &input[24..40]);
    assert_eq!(segments.next(), None);
    assert_eq!(tail, b"tail");
    assert_eq!(
        capnp::parse_exact(&input, capnp::Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::TrailingBytes
    );
    assert_eq!(
        capnp::parse_exact(&input[..40], capnp::Limits::default())
            .unwrap()
            .segments()
            .count(),
        2
    );
    let empty = [0u8; 8];
    let empty_message = capnp::parse_exact(&empty, capnp::Limits::default()).unwrap();
    assert_eq!(empty_message.segments().collect::<Vec<_>>(), vec![&b""[..]]);

    // Three segments need no table padding; the middle one may be empty.
    let mut three = vec![2, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0];
    three.extend(0u8..16);
    let segments: Vec<_> = capnp::parse_exact(&three, capnp::Limits::default())
        .unwrap()
        .segments()
        .collect();
    assert_eq!(
        segments,
        vec![&three[16..24], &three[24..24], &three[24..32]]
    );
}

#[test]
fn capnp_framing_rejects_truncation_and_prepays_limits() {
    use vesper_grammar::capnp;
    let input = [
        1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8,
    ];
    for end in 0..input.len() {
        let error = capnp::parse_exact(&input[..end], capnp::Limits::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnexpectedEof, "prefix {end}");
        assert_eq!(error.offset, end);
    }
    let limits = capnp::Limits::default();
    assert_eq!(
        capnp::parse_exact(
            &input,
            capnp::Limits {
                max_segments: 1,
                ..limits
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::CountLimit
    );
    assert_eq!(
        capnp::parse_exact(
            &input,
            capnp::Limits {
                max_work: 2,
                ..limits
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::WorkLimit
    );
    assert_eq!(
        capnp::parse_exact(
            &input,
            capnp::Limits {
                max_words: 0,
                ..limits
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::PayloadLimit
    );
    assert_eq!(
        capnp::parse_exact(
            &input,
            capnp::Limits {
                max_input_bytes: 23,
                ..limits
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::InputLimit
    );
    let huge_count = [255, 255, 255, 255];
    assert!(matches!(
        capnp::parse_exact(&huge_count, limits).unwrap_err().kind,
        ErrorKind::CountLimit | ErrorKind::Overflow
    ));
    let huge_segment = [0, 0, 0, 0, 255, 255, 255, 255];
    assert_eq!(
        capnp::parse_exact(&huge_segment, limits).unwrap_err().kind,
        ErrorKind::PayloadLimit
    );
}

#[test]
fn capnp_local_struct_pointer_views_signed_offsets_and_sections() {
    use vesper_grammar::capnp::{self, PointerError};
    assert_eq!(capnp::local_struct_pointer(&[0; 8], 0), Ok(None));
    assert_eq!(
        capnp::local_struct_pointer(&[0; 7], 0),
        Err(PointerError::MisalignedSegment)
    );
    assert_eq!(
        capnp::local_struct_pointer(&[0; 8], 1),
        Err(PointerError::PointerOutOfBounds)
    );
    for kind in 1..=3 {
        assert_eq!(
            capnp::local_struct_pointer(&u64::from(kind).to_le_bytes(), 0),
            Err(PointerError::UnsupportedKind(kind))
        );
    }

    // One-word forward displacement, one data word, one pointer word.
    let raw = (1u64 << 2) | (1u64 << 32) | (1u64 << 48);
    let mut segment = [0u8; 32];
    segment[..8].copy_from_slice(&raw.to_le_bytes());
    segment[16..24].copy_from_slice(b"datadata");
    segment[24..32].copy_from_slice(b"pointers");
    let view = capnp::local_struct_pointer(&segment, 0).unwrap().unwrap();
    assert_eq!(view.data, b"datadata");
    assert_eq!(view.pointers, b"pointers");
    assert_eq!(view.data.as_ptr(), segment[16..].as_ptr());
    assert_eq!(view.pointers.as_ptr(), segment[24..].as_ptr());

    // The pointer at word 2 addresses word 0 via a signed -3 displacement.
    let negative = ((0x3fff_fffdu64) << 2) | (1u64 << 32);
    let mut backward = [0u8; 24];
    backward[..8].copy_from_slice(b"backward");
    backward[16..24].copy_from_slice(&negative.to_le_bytes());
    let view = capnp::local_struct_pointer(&backward, 2).unwrap().unwrap();
    assert_eq!(view.data, b"backward");
    assert!(view.pointers.is_empty());

    // This root is framed as one segment; root pointer remains local.
    let mut framed = vec![0, 0, 0, 0, 4, 0, 0, 0];
    framed.extend_from_slice(&segment);
    let message = capnp::parse_exact(&framed, capnp::Limits::default()).unwrap();
    assert_eq!(message.root_struct().unwrap().unwrap().data, b"datadata");
}

#[test]
fn capnp_local_struct_pointer_rejects_invalid_extents() {
    use vesper_grammar::capnp::{self, PointerError};
    let too_far_back = (0x3fff_fffdu64 << 2) | (1u64 << 32);
    assert_eq!(
        capnp::local_struct_pointer(&too_far_back.to_le_bytes(), 0),
        Err(PointerError::TargetOutOfBounds)
    );
    let too_far_forward = (1u64 << 2) | (1u64 << 32);
    assert_eq!(
        capnp::local_struct_pointer(&too_far_forward.to_le_bytes(), 0),
        Err(PointerError::TargetOutOfBounds)
    );
    let missing_pointer_section = (1u64 << 48).to_le_bytes();
    assert_eq!(
        capnp::local_struct_pointer(&missing_pointer_section, 0),
        Err(PointerError::TargetOutOfBounds)
    );
    assert_eq!(
        capnp::local_struct_pointer(&[0; 8], usize::MAX),
        Err(PointerError::Overflow)
    );
    let empty_segment = [0u8; 8];
    let message = capnp::parse_exact(&empty_segment, capnp::Limits::default()).unwrap();
    assert_eq!(message.root_struct(), Err(PointerError::PointerOutOfBounds));
}

#[test]
fn capnp_struct_pointer_matches_small_word_index_oracle() {
    use vesper_grammar::capnp::{self, PointerError};
    for at in 0..8usize {
        for offset in -10i32..=10 {
            for data_words in 0..=3u16 {
                for pointer_words in 0..=3u16 {
                    let raw = (u64::from((offset as u32) & 0x3fff_ffff) << 2)
                        | (u64::from(data_words) << 32)
                        | (u64::from(pointer_words) << 48);
                    let mut segment = [0x5a; 64];
                    segment[at * 8..at * 8 + 8].copy_from_slice(&raw.to_le_bytes());
                    let actual = capnp::local_struct_pointer(&segment, at);
                    if raw == 0 {
                        assert_eq!(actual, Ok(None));
                        continue;
                    }
                    let target = at as i64 + 1 + i64::from(offset);
                    let end = target + i64::from(data_words) + i64::from(pointer_words);
                    if target < 0 || end > 8 {
                        assert_eq!(actual, Err(PointerError::TargetOutOfBounds));
                    } else {
                        let view = actual.unwrap().unwrap();
                        let start = target as usize * 8;
                        let middle = start + usize::from(data_words) * 8;
                        let end = middle + usize::from(pointer_words) * 8;
                        assert_eq!(view.data, &segment[start..middle]);
                        assert_eq!(view.pointers, &segment[middle..end]);
                    }
                }
            }
        }
    }
}

#[test]
fn capnp_local_primitive_lists_borrow_logical_bytes_and_check_padding() {
    use vesper_grammar::capnp::{self, ListKind, PointerError};
    fn raw(offset: i32, size: u64, count: u32) -> u64 {
        1 | (u64::from((offset as u32) & 0x3fff_ffff) << 2)
            | (size << 32)
            | (u64::from(count) << 35)
    }
    let kinds = [
        ListKind::Void,
        ListKind::Bit,
        ListKind::Byte,
        ListKind::TwoBytes,
        ListKind::FourBytes,
        ListKind::EightBytes,
        ListKind::Pointer,
    ];
    for (code, (kind, bits)) in kinds
        .into_iter()
        .zip([0u64, 1, 8, 16, 32, 64, 64])
        .enumerate()
    {
        let mut segment = [0xa5; 80];
        segment[..8].copy_from_slice(&raw(0, code as u64, 9).to_le_bytes());
        let view = capnp::local_list_pointer(&segment, 0).unwrap().unwrap();
        assert_eq!(view.kind, kind);
        assert_eq!(view.element_count, 9);
        let logical = (9 * bits).div_ceil(8) as usize;
        assert_eq!(view.data, &segment[8..8 + logical]);
        assert_eq!(view.data.as_ptr(), segment[8..].as_ptr());
    }
    let mut segment = [0u8; 16];
    segment[..8].copy_from_slice(&raw(0, 2, 9).to_le_bytes());
    assert_eq!(
        capnp::local_list_pointer(&segment, 0),
        Err(PointerError::TargetOutOfBounds)
    );
    // The payload starts before the pointer at word two.
    let mut backward = [0u8; 24];
    backward[..8].copy_from_slice(b"backward");
    backward[16..].copy_from_slice(&raw(-3, 2, 8).to_le_bytes());
    assert_eq!(
        capnp::local_list_pointer(&backward, 2)
            .unwrap()
            .unwrap()
            .data,
        b"backward"
    );
    let mut framed = vec![0, 0, 0, 0, 3, 0, 0, 0];
    framed.extend_from_slice(&backward);
    // Root word is the first segment word, not the later list pointer.
    assert_eq!(
        capnp::parse_exact(&framed, capnp::Limits::default())
            .unwrap()
            .root_list(),
        Err(PointerError::UnsupportedKind(2))
    );
    let mut root = vec![0, 0, 0, 0, 2, 0, 0, 0];
    root.extend_from_slice(&raw(0, 2, 3).to_le_bytes());
    root.extend_from_slice(b"abc\0\0\0\0\0");
    let view = capnp::parse_exact(&root, capnp::Limits::default())
        .unwrap()
        .root_list()
        .unwrap()
        .unwrap();
    assert_eq!(
        (view.kind, view.element_count, view.data),
        (ListKind::Byte, 3, &b"abc"[..])
    );
}

#[test]
fn capnp_local_list_rejects_unhandled_kinds_and_invalid_ranges() {
    use vesper_grammar::capnp::{self, ListKind, PointerError};
    fn raw(offset: i32, size: u64, count: u32) -> u64 {
        1 | (u64::from((offset as u32) & 0x3fff_ffff) << 2)
            | (size << 32)
            | (u64::from(count) << 35)
    }
    assert_eq!(capnp::local_list_pointer(&[0; 8], 0), Ok(None));
    let empty = raw(0, 2, 0).to_le_bytes();
    let view = capnp::local_list_pointer(&empty, 0).unwrap().unwrap();
    assert_eq!(
        (view.kind, view.element_count, view.data),
        (ListKind::Byte, 0, &b""[..])
    );
    assert_eq!(
        capnp::local_list_pointer(&raw(0, 7, 0).to_le_bytes(), 0),
        Err(PointerError::UnsupportedCompositeList)
    );
    for kind in [2u8, 3] {
        assert_eq!(
            capnp::local_list_pointer(&u64::from(kind).to_le_bytes(), 0),
            Err(PointerError::UnsupportedKind(kind))
        );
    }
    assert_eq!(
        capnp::local_list_pointer(&raw(-3, 2, 1).to_le_bytes(), 0),
        Err(PointerError::TargetOutOfBounds)
    );
    assert_eq!(
        capnp::local_list_pointer(&[0; 7], 0),
        Err(PointerError::MisalignedSegment)
    );
    let huge = capnp::local_list_pointer(&raw(0, 5, 0x1fff_ffff).to_le_bytes(), 0).unwrap_err();
    assert!(matches!(
        huge,
        PointerError::TargetOutOfBounds | PointerError::Overflow
    ));
    let huge_void = raw(0, 0, 0x1fff_ffff).to_le_bytes();
    let view = capnp::local_list_pointer(&huge_void, 0).unwrap().unwrap();
    assert_eq!(
        (view.kind, view.element_count, view.data),
        (ListKind::Void, 0x1fff_ffff, &b""[..])
    );
}

#[test]
fn capnp_primitive_list_lengths_match_small_word_oracle() {
    use vesper_grammar::capnp::{self, PointerError};
    for at in 0..8usize {
        for offset in -10i32..=10 {
            for size in 0..=6u64 {
                for count in 0..=17u32 {
                    let raw = 1
                        | (u64::from((offset as u32) & 0x3fff_ffff) << 2)
                        | (size << 32)
                        | (u64::from(count) << 35);
                    let mut segment = [0x5a; 64];
                    segment[at * 8..at * 8 + 8].copy_from_slice(&raw.to_le_bytes());
                    let bits = [0u64, 1, 8, 16, 32, 64, 64][size as usize] * u64::from(count);
                    let logical = bits.div_ceil(8) as usize;
                    let words = bits.div_ceil(64) as i64;
                    let target = at as i64 + 1 + i64::from(offset);
                    let actual = capnp::local_list_pointer(&segment, at);
                    if target < 0 || target + words > 8 {
                        assert_eq!(actual, Err(PointerError::TargetOutOfBounds));
                    } else {
                        let view = actual.unwrap().unwrap();
                        let start = target as usize * 8;
                        assert_eq!(view.element_count, count);
                        assert_eq!(view.data, &segment[start..start + logical]);
                    }
                }
            }
        }
    }
}
