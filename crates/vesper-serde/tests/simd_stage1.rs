//! Stage 1 differential tests: every available SIMD path must produce masks
//! bit-identical to the scalar reference, block by block.
use vesper_serde::compute_escaped_mask_64;
use vesper_serde::simd::{
    IndexLimits, Isa, StructuralIndex, block_masks, escaped_mask_branchless, prefix_xor_shifts,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn simd_paths() -> Vec<Isa> {
    Isa::available().filter(|isa| *isa != Isa::Scalar).collect()
}

fn assert_same(input: &[u8]) {
    let (want, want_utf8) = block_masks(input, Isa::Scalar).unwrap();
    assert_eq!(want.len(), input.len().div_ceil(64));
    for isa in simd_paths() {
        let (got, got_utf8) = block_masks(input, isa).unwrap();
        assert_eq!(got_utf8, want_utf8, "{isa:?} UTF-8 verdict on {input:?}");
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g, w, "{isa:?} block {i} of {input:?}");
        }
        assert_eq!(got.len(), want.len());
    }
}

/// A JSON-flavoured alphabet, weighted toward the bytes stage 1 classifies.
const ALPHABET: &[u8] =
    b"\\\\\\\"\"\"{}[]:,  \t\n\r0123456789-+.eEtrufalsn\x00\x01\x0c\x1a\x1f\x7f";

fn random_input(rng: &mut Rng, len: usize) -> Vec<u8> {
    (0..len)
        .map(|_| match rng.below(10) {
            0 => rng.next() as u8,
            1 => [
                0xc3, 0xa9, 0xe2, 0x82, 0xac, 0xf0, 0x9f, 0x98, 0x80, 0xed, 0xa0,
            ][rng.below(11)],
            _ => ALPHABET[rng.below(ALPHABET.len())],
        })
        .collect()
}

#[test]
fn simd_paths_are_detected_on_simd_architectures() {
    let paths = simd_paths();
    eprintln!(
        "stage 1 paths: {:?}, detected {:?}, SVE reported by the CPU (no SVE path): {}",
        paths,
        Isa::detect(),
        Isa::sve_reported()
    );
    if std::env::var_os("VESPER_ALLOW_SCALAR_ONLY").is_none() {
        if cfg!(target_arch = "x86_64") {
            assert!(paths.contains(&Isa::Sse42), "x86_64 CI must run SSE4.2");
        }
        if cfg!(target_arch = "aarch64") {
            assert!(paths.contains(&Isa::Neon), "AArch64 CI must run NEON");
        }
    }
    assert!(Isa::Scalar.is_available());
    for isa in Isa::ALL {
        if !isa.is_available() {
            let mut index = StructuralIndex::new();
            let err = index.build(b"[]", isa, IndexLimits::default()).unwrap_err();
            assert_eq!(err.kind, vesper_serde::ErrorKind::IsaUnavailable);
        }
    }
}

#[test]
fn branchless_escape_mask_matches_scalar_reference() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut cases: Vec<u64> = vec![
        0,
        u64::MAX,
        1,
        1 << 63,
        0x5555_5555_5555_5555,
        !0x5555_5555_5555_5555,
    ];
    // Every run of backslashes starting at every position.
    for start in 0..64 {
        for len in 1..=(64 - start) {
            let run = if len == 64 {
                u64::MAX
            } else {
                ((1u64 << len) - 1) << start
            };
            cases.push(run);
        }
    }
    cases.extend((0..20_000).map(|_| rng.next() & rng.next()));
    cases.extend((0..20_000).map(|_| rng.next()));
    for bits in cases {
        for carry in [false, true] {
            let (want, want_carry) = compute_escaped_mask_64(bits, carry);
            let mut c = u64::from(carry);
            let got = escaped_mask_branchless(bits, &mut c);
            assert_eq!(got, want, "bits {bits:#x} carry {carry}");
            assert_eq!(c != 0, want_carry, "carry out for {bits:#x}");
        }
    }
}

#[test]
fn prefix_xor_is_parity() {
    let mut rng = Rng(7);
    for _ in 0..10_000 {
        let x = rng.next();
        let got = prefix_xor_shifts(x);
        for i in 0..64 {
            let parity = (x & (u64::MAX >> (63 - i))).count_ones() % 2 == 1;
            assert_eq!(got >> i & 1 == 1, parity);
        }
    }
}

#[test]
fn every_byte_value_in_every_lane_position_classifies_identically() {
    for b in 0..=255u8 {
        for pos in [0usize, 1, 15, 16, 31, 32, 47, 48, 62, 63] {
            let mut block = vec![b'a'; 64];
            block[pos] = b;
            assert_same(&block);
            let mut block = vec![b' '; 130];
            block[pos + 64] = b;
            assert_same(&block);
        }
    }
}

#[test]
fn random_inputs_match() {
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    for _ in 0..4_000 {
        let len = rng.below(400);
        assert_same(&random_input(&mut rng, len));
    }
}

#[test]
fn adversarial_backslash_runs_and_quotes_across_block_edges() {
    for run in 0..=140 {
        for start in [0usize, 1, 30, 31, 32, 33, 60, 61, 62, 63, 64, 65, 127] {
            for tail in [b"\"x\"".as_slice(), b"\"", b"", b"\\", b"\"\"\""] {
                let mut input = vec![b'"'; 1];
                input.extend(std::iter::repeat_n(b'x', start));
                input.extend(std::iter::repeat_n(b'\\', run));
                input.extend_from_slice(tail);
                input.extend_from_slice(b",[1]");
                assert_same(&input);
            }
        }
    }
    // Unbalanced quotes: every prefix of a quoted document.
    let doc = br#"{"a\"b":"c\\","d":["\\\"",1,true]}"#.repeat(5);
    for cut in 0..doc.len() {
        assert_same(&doc[..cut]);
        assert_same(&doc[cut..]);
    }
}

fn utf8_same(input: &[u8]) {
    let want = core::str::from_utf8(input).is_ok();
    assert_eq!(block_masks(input, Isa::Scalar).unwrap().1, want);
    for isa in simd_paths() {
        assert_eq!(
            block_masks(input, isa).unwrap().1,
            want,
            "{isa:?} on {input:x?}"
        );
    }
}

#[test]
fn utf8_two_byte_sequences_exhaustive_at_block_edges() {
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            for pad in [62usize, 63, 64] {
                let mut input = vec![b'a'; pad];
                input.push(a);
                input.push(b);
                utf8_same(&input);
            }
        }
    }
}

#[test]
fn utf8_three_and_four_byte_sequences_at_block_edges() {
    let leads: Vec<u8> = (0xc0..=0xff).collect();
    let conts: Vec<u8> = vec![
        0x00, 0x41, 0x7f, 0x80, 0x81, 0x8f, 0x90, 0x9f, 0xa0, 0xaf, 0xb0, 0xbf, 0xc0, 0xc2, 0xe0,
        0xf0, 0xff,
    ];
    for &a in &leads {
        for &b in &conts {
            for &c in &conts {
                for pad in [61usize, 62, 63, 64, 15, 31] {
                    let mut input = vec![b' '; pad];
                    input.extend_from_slice(&[a, b, c]);
                    utf8_same(&input);
                    for &d in &[0x41u8, 0x80, 0xbf, 0xc0] {
                        let mut four = input.clone();
                        four.push(d);
                        utf8_same(&four);
                    }
                }
            }
        }
    }
    // Every valid scalar value at block edges, and truncations of each.
    for cp in (0..=0x10ffffu32).step_by(97).filter_map(char::from_u32) {
        let mut buf = [0u8; 4];
        let enc = cp.encode_utf8(&mut buf).as_bytes();
        for pad in [60usize, 61, 62, 63] {
            let mut input = vec![b'"'; pad];
            input.extend_from_slice(enc);
            utf8_same(&input);
            input.truncate(pad + enc.len() - 1);
            utf8_same(&input);
        }
    }
    // Truncated sequence exactly at the end of a full block.
    for seq in [&[0xc3u8] as &[u8], &[0xe2, 0x82], &[0xf0, 0x9f, 0x98]] {
        let mut input = vec![b'a'; 64 - seq.len()];
        input.extend_from_slice(seq);
        utf8_same(&input);
        input.extend_from_slice(&[b'a'; 64]);
        utf8_same(&input);
    }
}

#[test]
fn structural_index_positions_match_across_paths() {
    let doc = br#" {"id": 42, "topic":"a\"b", "x":[true,null,-1.5e3], "s":"\u00e9\\"} "#;
    let mut want = StructuralIndex::new();
    want.build(doc, Isa::Scalar, IndexLimits::default())
        .unwrap();
    let positions: Vec<u8> = want
        .structurals()
        .iter()
        .map(|&p| doc[p as usize])
        .collect();
    // Opening and closing quotes, operators and scalar starts, nothing else.
    let text: String = positions.iter().map(|&b| b as char).collect();
    assert_eq!(text, r#"{"":4,"":"","":[t,n,-],"":""}"#);
    assert_eq!(want.escapes().len(), 3);
    let mut rng = Rng(42);
    let mut docs: Vec<Vec<u8>> = vec![doc.to_vec()];
    docs.extend((0..500).map(|_| {
        let n = rng.below(300);
        random_input(&mut rng, n)
    }));
    for input in docs {
        let mut a = StructuralIndex::new();
        let ra = a.build(&input, Isa::Scalar, IndexLimits::default());
        for isa in simd_paths() {
            let mut b = StructuralIndex::new();
            let rb = b.build(&input, isa, IndexLimits::default());
            assert_eq!(ra, rb);
            assert_eq!(a.structurals(), b.structurals());
            assert_eq!(a.escapes(), b.escapes());
            assert_eq!(a.first_string_control(), b.first_string_control());
            assert_eq!(a.unclosed_string(), b.unclosed_string());
        }
    }
}

#[test]
fn stage1_limits_are_typed_errors() {
    let mut index = StructuralIndex::new();
    let limits = IndexLimits {
        max_input_bytes: 4,
        max_entries: 3,
    };
    for isa in Isa::available() {
        let err = index.build(b"[1,2]", isa, limits).unwrap_err();
        assert_eq!(err.kind, vesper_serde::ErrorKind::InputLimit);
        // Four entries, room for three: truncated at the fourth.
        index.build(b"[1,2", isa, limits).unwrap();
        assert_eq!(index.structurals(), &[0, 1, 2]);
        assert_eq!(index.truncated_at(), Some(3));
        assert!(index.capacity_bytes() <= 3 * 4);
        let err = index.build(b"\"\xff\"", isa, limits).unwrap_err();
        assert_eq!((err.kind, err.offset), (vesper_serde::ErrorKind::Utf8, 1));
        index.build(b"\"ab", isa, limits).unwrap();
        assert!(index.unclosed_string());
        index.build(b"\"\x01\"", isa, limits).unwrap();
        assert_eq!(index.first_string_control(), Some(1));
    }
}
