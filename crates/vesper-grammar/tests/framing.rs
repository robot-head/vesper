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
