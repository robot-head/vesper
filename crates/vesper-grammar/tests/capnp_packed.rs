use vesper_grammar::capnp_packed::{self as packed, ErrorKind, Limits};

fn unpack(input: &[u8]) -> Vec<u8> {
    let size = packed::unpacked_len(input, Limits::default()).unwrap();
    let mut output = vec![0xa5; size + 3];
    assert_eq!(
        packed::unpack_into(input, &mut output, Limits::default()).unwrap(),
        size
    );
    assert_eq!(&output[size..], &[0xa5; 3]);
    output.truncate(size);
    output
}
fn pack(input: &[u8]) -> Vec<u8> {
    let size = packed::packed_len(input, Limits::default()).unwrap();
    let mut output = vec![0xa5; size + 3];
    assert_eq!(
        packed::pack_into(input, &mut output, Limits::default()).unwrap(),
        size
    );
    assert_eq!(&output[size..], &[0xa5; 3]);
    output.truncate(size);
    output
}

#[test]
fn official_encoding_vectors_and_raw_runs_with_zeroes() {
    // https://capnproto.org/encoding.html#packing
    let unpacked = [
        0x08, 0, 0, 0, 0x03, 0, 0x02, 0, 0x19, 0, 0, 0, 0xaa, 0x01, 0, 0,
    ];
    let encoded = [0x51, 0x08, 0x03, 0x02, 0x31, 0x19, 0xaa, 0x01];
    assert_eq!(unpack(&encoded), unpacked);
    assert_eq!(pack(&unpacked), encoded);
    assert_eq!(unpack(&[0, 3]), [0; 32]);
    assert_eq!(pack(&[0; 32]), [0, 3]);
    let mut full = vec![0xff];
    full.extend([0x8a; 8]);
    full.push(3);
    full.extend([0x8a; 24]);
    assert_eq!(unpack(&full), [0x8a; 32]);
    assert_eq!(pack(&[0x8a; 32]), full);

    // The raw words following an FF tag may contain zero bytes.
    let mut raw = vec![0xff];
    raw.extend([1; 8]);
    raw.push(1);
    raw.extend([0, 2, 0, 3, 0, 4, 0, 5]);
    let mut expected = vec![1; 8];
    expected.extend([0, 2, 0, 3, 0, 4, 0, 5]);
    assert_eq!(unpack(&raw), expected);
    assert_ne!(pack(&expected), raw); // Legal packing is not unique.
    assert_eq!(unpack(&[]), []);
    assert_eq!(pack(&[]), []);
}

#[test]
fn all_tag_masks_and_noncanonical_alternatives_decode() {
    for tag in 0..=u8::MAX {
        let mut wire = vec![tag];
        let mut word = [0u8; 8];
        for (i, slot) in word.iter_mut().enumerate() {
            if tag & (1 << i) != 0 {
                *slot = (i + 1) as u8;
                wire.push(*slot);
            }
        }
        if tag == 0 || tag == 0xff {
            wire.push(0);
        }
        assert_eq!(unpack(&wire), word, "tag {tag:02x}");
        assert_eq!(pack(&word), wire, "tag {tag:02x}");
    }
    // Decoding accepts alternate encodings; it does not establish canonicality.
    assert_eq!(unpack(&[1, 0]), [0; 8]);
    assert_eq!(pack(&[0; 8]), [0, 0]);
}

#[test]
fn run_lengths_stop_at_256_words_and_roundtrip_mixed_input() {
    for word in [[0u8; 8], [0x8a; 8]] {
        for words in [1, 2, 255, 256, 257, 512, 513] {
            let input = word.repeat(words);
            let encoded = pack(&input);
            assert_eq!(unpack(&encoded), input);
            assert_eq!(encoded[0], if word[0] == 0 { 0 } else { 0xff });
            let count_offset = if word[0] == 0 { 1 } else { 9 };
            assert_eq!(usize::from(encoded[count_offset]), (words - 1).min(255));
        }
    }
    let input: Vec<u8> = (0..8192).map(|n| ((n * 73 + n / 11) % 256) as u8).collect();
    assert_eq!(unpack(&pack(&input)), input);
}

#[test]
fn every_short_output_preserves_the_whole_buffer() {
    let mut source = vec![0; 16];
    source.extend([1; 16]);
    source.extend([0, 1, 0, 2, 0, 3, 0, 4]);
    let encoded = pack(&source);
    for capacity in 0..encoded.len() {
        let mut output = vec![0xa5; encoded.len() + 5];
        assert_eq!(
            packed::pack_into(&source, &mut output[..capacity], Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::OutputTooSmall
        );
        assert!(output.iter().all(|byte| *byte == 0xa5));
    }
    for capacity in 0..source.len() {
        let mut output = vec![0xa5; source.len() + 5];
        assert_eq!(
            packed::unpack_into(&encoded, &mut output[..capacity], Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::OutputTooSmall
        );
        assert!(output.iter().all(|byte| *byte == 0xa5));
    }
}

#[test]
fn truncation_limits_and_capacity_preserve_every_output_byte() {
    let mut sample = vec![0xff];
    sample.extend([1; 8]);
    sample.push(2);
    sample.extend([0; 16]);
    for n in 1..sample.len() {
        let mut output = [0xa5; 64];
        assert_eq!(
            packed::unpack_into(&sample[..n], &mut output, Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::Truncated,
            "prefix {n}"
        );
        assert_eq!(output, [0xa5; 64]);
    }
    for input in [&[0u8][..], &[0x01][..], &[0xff, 1, 2][..]] {
        let mut output = [0xa5; 64];
        assert_eq!(
            packed::unpack_into(input, &mut output, Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::Truncated
        );
        assert_eq!(output, [0xa5; 64]);
    }
    let cases = [
        (
            Limits {
                max_input_bytes: 1,
                ..Limits::default()
            },
            ErrorKind::InputLimit,
        ),
        (
            Limits {
                max_output_bytes: 7,
                ..Limits::default()
            },
            ErrorKind::OutputLimit,
        ),
        (
            Limits {
                max_words: 0,
                ..Limits::default()
            },
            ErrorKind::WordLimit,
        ),
        (
            Limits {
                max_work: 11,
                ..Limits::default()
            },
            ErrorKind::WorkLimit,
        ),
        (
            Limits {
                max_input_bytes: usize::MAX,
                ..Limits::default()
            },
            ErrorKind::InvalidLimits,
        ),
    ];
    for (limits, expected) in cases {
        let mut output = [0xa5; 64];
        assert_eq!(
            packed::unpack_into(&[0, 0], &mut output, limits)
                .unwrap_err()
                .kind,
            expected
        );
        assert_eq!(output, [0xa5; 64]);
    }
    let mut short = [0xa5; 7];
    assert_eq!(
        packed::unpack_into(&[0, 0], &mut short, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::OutputTooSmall
    );
    assert_eq!(short, [0xa5; 7]);
    let mut output = [0xa5; 64];
    assert_eq!(
        packed::pack_into(&[1; 7], &mut output, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::UnalignedInput
    );
    assert_eq!(output, [0xa5; 64]);
    assert_eq!(
        packed::pack_into(&[0; 8], &mut output[..1], Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::OutputTooSmall
    );
    assert_eq!(output, [0xa5; 64]);
    let limits = Limits {
        max_work: 33,
        ..Limits::default()
    };
    assert_eq!(
        packed::pack_into(&[0; 8], &mut output, limits)
            .unwrap_err()
            .kind,
        ErrorKind::WorkLimit
    );
    assert_eq!(output, [0xa5; 64]);
}

#[test]
fn exact_budget_boundaries_apply_to_both_directions() {
    let empty = Limits {
        max_input_bytes: 0,
        max_output_bytes: 0,
        max_words: 0,
        max_work: 0,
    };
    let mut output = [0xa5; 2050];
    assert_eq!(packed::pack_into(&[], &mut output, empty).unwrap(), 0);
    assert_eq!(packed::unpack_into(&[], &mut output, empty).unwrap(), 0);
    assert_eq!(output, [0xa5; 2050]);

    // One zero word encodes to two bytes. Packing charges 4*8+2 work.
    let exact = Limits {
        max_input_bytes: 8,
        max_output_bytes: 2,
        max_words: 1,
        max_work: 34,
    };
    assert_eq!(packed::pack_into(&[0; 8], &mut output, exact).unwrap(), 2);
    assert_eq!(&output[..2], &[0, 0]);
    assert!(output[2..].iter().all(|b| *b == 0xa5));
    for (limits, expected) in [
        (
            Limits {
                max_input_bytes: 7,
                ..exact
            },
            ErrorKind::InputLimit,
        ),
        (
            Limits {
                max_output_bytes: 1,
                ..exact
            },
            ErrorKind::OutputLimit,
        ),
        (
            Limits {
                max_words: 0,
                ..exact
            },
            ErrorKind::WordLimit,
        ),
        (
            Limits {
                max_work: 33,
                ..exact
            },
            ErrorKind::WorkLimit,
        ),
        (
            Limits {
                max_words: usize::MAX,
                ..exact
            },
            ErrorKind::InvalidLimits,
        ),
    ] {
        let mut untouched = [0xa5; 16];
        assert_eq!(
            packed::pack_into(&[0; 8], &mut untouched, limits)
                .unwrap_err()
                .kind,
            expected
        );
        assert_eq!(untouched, [0xa5; 16]);
    }

    // A maximum zero run contains 256 words and charges 2*2+2048 work.
    let exact = Limits {
        max_input_bytes: 2,
        max_output_bytes: 2048,
        max_words: 256,
        max_work: 2052,
    };
    let mut decoded = [0xa5; 2050];
    assert_eq!(
        packed::unpack_into(&[0, 255], &mut decoded, exact).unwrap(),
        2048
    );
    assert!(decoded[..2048].iter().all(|b| *b == 0));
    assert_eq!(&decoded[2048..], &[0xa5; 2]);
    for (limits, expected) in [
        (
            Limits {
                max_words: 255,
                ..exact
            },
            ErrorKind::WordLimit,
        ),
        (
            Limits {
                max_output_bytes: 2047,
                ..exact
            },
            ErrorKind::OutputLimit,
        ),
        (
            Limits {
                max_work: 2051,
                ..exact
            },
            ErrorKind::WorkLimit,
        ),
    ] {
        let mut untouched = [0xa5; 2050];
        assert_eq!(
            packed::unpack_into(&[0, 255], &mut untouched, limits)
                .unwrap_err()
                .kind,
            expected
        );
        assert_eq!(untouched, [0xa5; 2050]);
    }
    assert_eq!(
        packed::unpacked_len(&[0xff, 1], Limits::default())
            .unwrap_err()
            .offset,
        2
    );
    assert_eq!(
        packed::packed_len(&[1; 7], Limits::default())
            .unwrap_err()
            .offset,
        7
    );
}
