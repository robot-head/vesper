//! Scalable-vector stage 1 (SVE, RVV) differential tests: block masks, the
//! UTF-8 verdict and the structural index must be bit-identical to the
//! scalar reference at whatever vector length the CPU (or QEMU) runs.
//!
//! On hosts without SVE or RVV these check the typed `IsaUnavailable`
//! error. `VESPER_REQUIRE_SVE=1` / `VESPER_REQUIRE_RVV=1` make a missing
//! path a failure, so the QEMU job cannot pass by skipping.
use vesper_serde::ErrorKind;
use vesper_serde::simd::scalable::{ScalableIsa, block_masks_scalable};
use vesper_serde::simd::{IndexLimits, Isa, StructuralIndex, block_masks};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn paths() -> Vec<ScalableIsa> {
    ScalableIsa::available().collect()
}

fn assert_same(input: &[u8]) {
    let (want, want_utf8) = block_masks(input, Isa::Scalar).unwrap();
    for isa in paths() {
        let (got, got_utf8) = block_masks_scalable(input, isa).unwrap();
        assert_eq!(got_utf8, want_utf8, "{isa:?} UTF-8 verdict on {input:x?}");
        assert_eq!(got.len(), want.len(), "{isa:?} block count");
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g, w, "{isa:?} block {i} of {input:x?}");
        }
    }
}

fn utf8_same(input: &[u8]) {
    let want = core::str::from_utf8(input).is_ok();
    for isa in paths() {
        assert_eq!(
            block_masks_scalable(input, isa).unwrap().1,
            want,
            "{isa:?} on {input:x?}"
        );
    }
}

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

fn required(isa: ScalableIsa) -> bool {
    let var = match isa {
        ScalableIsa::Sve => "VESPER_REQUIRE_SVE",
        ScalableIsa::Rvv => "VESPER_REQUIRE_RVV",
    };
    std::env::var(var).is_ok_and(|v| v == "1")
}

#[test]
fn scalable_paths_report_or_refuse() {
    for isa in ScalableIsa::ALL {
        match isa.vector_bytes() {
            Some(bytes) => {
                eprintln!("{} stage 1 at {} bits", isa.name(), bytes * 8);
                assert!(isa.is_available());
                assert!(bytes >= 16 && bytes.is_multiple_of(16));
            }
            None => {
                assert!(!required(isa), "{} required but unavailable", isa.name());
                assert!(!isa.is_available());
                let err = block_masks_scalable(b"[]", isa).unwrap_err();
                assert_eq!(err.kind, ErrorKind::IsaUnavailable);
                let err = block_masks_scalable(b"", isa).unwrap_err();
                assert_eq!(err.kind, ErrorKind::IsaUnavailable);
                let mut index = StructuralIndex::new();
                let err = index
                    .build_scalable(b"[]", isa, IndexLimits::default())
                    .unwrap_err();
                assert_eq!(err.kind, ErrorKind::IsaUnavailable);
            }
        }
    }
    if !cfg!(target_arch = "aarch64") {
        assert!(!ScalableIsa::Sve.is_available());
    }
    if !cfg!(target_arch = "riscv64") {
        assert!(!ScalableIsa::Rvv.is_available());
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
fn random_inputs_match_across_chunk_and_strip_edges() {
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    // Up to three 512-byte kernel chunks, so strips cross chunk edges at
    // every vector length.
    for _ in 0..1_500 {
        let len = rng.below(1_600);
        assert_same(&random_input(&mut rng, len));
    }
}

#[test]
fn adversarial_backslash_runs_and_quotes_across_block_edges() {
    for run in 0..=140 {
        for start in [0usize, 1, 31, 32, 63, 64, 65, 127, 510, 511, 512] {
            for tail in [b"\"x\"".as_slice(), b"\"", b"", b"\\"] {
                let mut input = vec![b'"'; 1];
                input.extend(std::iter::repeat_n(b'x', start));
                input.extend(std::iter::repeat_n(b'\\', run));
                input.extend_from_slice(tail);
                input.extend_from_slice(b",[1]");
                assert_same(&input);
            }
        }
    }
    let doc = br#"{"a\"b":"c\\","d":["\\\"",1,true]}"#.repeat(5);
    for cut in 0..doc.len() {
        assert_same(&doc[..cut]);
        assert_same(&doc[cut..]);
    }
}

#[test]
fn utf8_two_byte_sequences_exhaustive_at_block_and_chunk_edges() {
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            for pad in [62usize, 63, 64, 510, 511] {
                let mut input = vec![b'a'; pad];
                input.push(a);
                input.push(b);
                utf8_same(&input);
            }
        }
    }
}

#[test]
fn utf8_three_and_four_byte_sequences_at_edges() {
    let leads: Vec<u8> = (0xc0..=0xff).collect();
    let conts: Vec<u8> = vec![
        0x00, 0x41, 0x7f, 0x80, 0x81, 0x8f, 0x90, 0x9f, 0xa0, 0xaf, 0xb0, 0xbf, 0xc0, 0xc2, 0xe0,
        0xf0, 0xff,
    ];
    for &a in &leads {
        for &b in &conts {
            for &c in &conts {
                for pad in [61usize, 62, 63, 15, 509, 510] {
                    let mut input = vec![b' '; pad];
                    input.extend_from_slice(&[a, b, c]);
                    utf8_same(&input);
                    for &d in &[0x41u8, 0x80, 0xbf] {
                        let mut four = input.clone();
                        four.push(d);
                        utf8_same(&four);
                    }
                }
            }
        }
    }
    for cp in (0..=0x10ffffu32).step_by(97).filter_map(char::from_u32) {
        let mut buf = [0u8; 4];
        let enc = cp.encode_utf8(&mut buf).as_bytes();
        for pad in [61usize, 63, 510] {
            let mut input = vec![b'"'; pad];
            input.extend_from_slice(enc);
            utf8_same(&input);
            input.truncate(pad + enc.len() - 1);
            utf8_same(&input);
        }
    }
    // Truncated sequences exactly at the end of a block and of a chunk.
    for seq in [&[0xc3u8] as &[u8], &[0xe2, 0x82], &[0xf0, 0x9f, 0x98]] {
        for size in [64usize, 512, 1024] {
            let mut input = vec![b'a'; size - seq.len()];
            input.extend_from_slice(seq);
            utf8_same(&input);
            input.extend_from_slice(&[b'a'; 64]);
            utf8_same(&input);
        }
    }
}

#[test]
fn structural_index_matches_scalar() {
    let mut rng = Rng(42);
    let doc = br#" {"id": 42, "topic":"a\"b", "x":[true,null,-1.5e3], "s":"\u00e9\\"} "#;
    let mut docs: Vec<Vec<u8>> = vec![doc.to_vec(), doc.repeat(40)];
    docs.extend((0..300).map(|_| {
        let n = rng.below(1_200);
        random_input(&mut rng, n)
    }));
    for input in docs {
        for limits in [
            IndexLimits::default(),
            IndexLimits {
                max_input_bytes: 1 << 20,
                max_entries: 17,
            },
        ] {
            let mut a = StructuralIndex::new();
            let ra = a.build(&input, Isa::Scalar, limits);
            for isa in paths() {
                let mut b = StructuralIndex::new();
                let rb = b.build_scalable(&input, isa, limits);
                assert_eq!(ra, rb, "{isa:?}");
                assert_eq!(a.structurals(), b.structurals());
                assert_eq!(a.escapes(), b.escapes());
                assert_eq!(a.first_string_control(), b.first_string_control());
                assert_eq!(a.unclosed_string(), b.unclosed_string());
                assert_eq!(a.truncated_at(), b.truncated_at());
                assert_eq!(b.isa(), None);
            }
        }
    }
}

#[test]
fn stage1_limits_are_typed_errors() {
    let limits = IndexLimits {
        max_input_bytes: 4,
        max_entries: 3,
    };
    for isa in paths() {
        let mut index = StructuralIndex::new();
        let err = index.build_scalable(b"[1,2]", isa, limits).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InputLimit);
        index.build_scalable(b"[1,2", isa, limits).unwrap();
        assert_eq!(index.structurals(), &[0, 1, 2]);
        assert_eq!(index.truncated_at(), Some(3));
        let err = index.build_scalable(b"\"\xff\"", isa, limits).unwrap_err();
        assert_eq!((err.kind, err.offset), (ErrorKind::Utf8, 1));
        index.build_scalable(b"\"ab", isa, limits).unwrap();
        assert!(index.unclosed_string());
    }
}
