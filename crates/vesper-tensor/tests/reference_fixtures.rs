//! Differential harness over the golden vectors in `fixtures/reference/`
//! (see `fixtures/reference/MANIFEST.md`). Every expected byte there was
//! produced by an independent implementation (llama.cpp, gguf-py, Hugging
//! Face `tokenizers` cross-checked with `tiktoken`, ml_dtypes), never by this
//! crate. The same files are the acceptance vectors for the future
//! Vesper-source port of the Tensor formats.
//!
//! What runs here against the Rust crate:
//! * Q4_K: `formats::quantize_q4_k` reproduces llama.cpp's bytes.
//! * Q8_0 / Q4_0 weights: the crate's quantizers reproduce gguf-py's bytes.
//! * Q8 activations: a test-side transliteration of GGML's generic CPU
//!   kernels (`quantize_row_q8_K_ref`, `ggml_vec_dot_{q4_K_q8_K, q8_0_q8_0,
//!   q4_0_q8_0}_generic`) reproduces the generator's bytes and FP32 results
//!   bit for bit; the crate has no Q8-activation path (deferred to the port).
//! * FP8 per-channel, per-128x128-block and MXFP8 scaling: the crate's
//!   `Fp8::encode_saturating` applied with each rule reproduces ml_dtypes'
//!   codes; the scaling rules themselves are spelled out here.
//! * GPT-2 BPE: the case file is well formed (the crate has no byte-level
//!   BPE; parity was asserted between the two peers at generation time).
use vesper_tensor::formats::{quantize_q4_k, Fp8};
use vesper_tensor::host::decode_fp16;
use vesper_tensor::quant::{quantize_q4_0, quantize_q8_0};

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("../fixtures/reference/", $name)).as_slice()
    };
}

const ROWS: usize = 96;
const COLS: usize = 1536;

fn f32s(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

fn smollm_rows() -> Vec<f32> {
    fixture!("q4k-smollm-blk11-ffn_down.bf16")
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| f32::from_bits(u32::from(u16::from_le_bytes(*c)) << 16))
        .collect()
}

#[test]
fn q4_k_bytes_equal_llama_cpp() {
    let x = smollm_rows();
    assert_eq!(x.len(), ROWS * COLS);
    let ours = quantize_q4_k(&x, COLS).unwrap();
    let theirs = fixture!("q4k-smollm-blk11-ffn_down.q4k");
    assert_eq!(ours.len(), theirs.len());
    for (b, (o, t)) in ours.chunks(144).zip(theirs.chunks(144)).enumerate() {
        assert_eq!(o, t, "Q4_K super-block {b} differs from llama.cpp");
    }
}

#[test]
fn q8_0_and_q4_0_weights_equal_gguf_py() {
    let x = smollm_rows();
    assert_eq!(
        quantize_q8_0(&x, COLS).unwrap(),
        fixture!("q8-weights.q8_0")
    );
    assert_eq!(
        quantize_q4_0(&x, COLS).unwrap(),
        fixture!("q8-weights.q4_0")
    );
}

// --- GGML generic kernels (ggml-cpu/quants.c), transliterated -------------

fn nearest_int(v: f32) -> i32 {
    ((v + 12_582_912.0f32).to_bits() as i32 & 0x007f_ffff) - 0x0040_0000
}

/// `quantize_row_q8_K_ref` as bytes: f32 d, 256 x i8, 16 x i16 bsums.
fn quantize_q8_k(x: &[f32]) -> Vec<u8> {
    let mut out = Vec::new();
    for blk in x.chunks(256) {
        let (mut amax, mut max) = (0.0f32, 0.0f32);
        for &v in blk {
            if v.abs() > amax {
                amax = v.abs();
                max = v;
            }
        }
        if amax == 0.0 {
            out.extend_from_slice(&[0u8; 4 + 256 + 32]);
            continue;
        }
        let iscale = -127.0f32 / max;
        let qs: Vec<i8> = blk
            .iter()
            .map(|&v| nearest_int(iscale * v).min(127) as i8)
            .collect();
        out.extend_from_slice(&(1.0f32 / iscale).to_le_bytes());
        out.extend(qs.iter().map(|&q| q as u8));
        for g in qs.chunks(16) {
            let s: i32 = g.iter().map(|&q| i32::from(q)).sum();
            out.extend_from_slice(&(s as i16).to_le_bytes());
        }
    }
    out
}

fn half(b: &[u8], at: usize) -> f32 {
    decode_fp16(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn scale_min_k4(j: usize, s: &[u8]) -> (i32, i32) {
    let (a, b) = if j < 4 {
        (s[j] & 63, s[j + 4] & 63)
    } else {
        (
            (s[j + 4] & 0x0f) | ((s[j - 4] >> 6) << 4),
            (s[j + 4] >> 4) | ((s[j] >> 6) << 4),
        )
    };
    (i32::from(a), i32::from(b))
}

/// One weight format of the Q8 differential test.
struct DotCase {
    name: &'static str,
    weights: &'static [u8],
    row_bytes: usize,
    expected: &'static [u8],
    dot: fn(&[u8], &[u8]) -> f32,
}

fn dot_q4_k_q8_k(row: &[u8], y: &[u8]) -> f32 {
    let mut sums = [0.0f32; 8];
    let mut sumf = 0.0f32;
    for (x, y) in row.chunks(144).zip(y.chunks(292)) {
        let yd = f32::from_le_bytes([y[0], y[1], y[2], y[3]]);
        let yq = &y[4..260];
        let mut a = [0i32; 256];
        for j in 0..4 {
            for l in 0..32 {
                let q = x[16 + 32 * j + l];
                a[64 * j + l] = i32::from(q & 15);
                a[64 * j + 32 + l] = i32::from(q >> 4);
            }
        }
        let mut sumi = 0i32;
        for j in 0..16 {
            let bsum = i32::from(i16::from_le_bytes([y[260 + 2 * j], y[261 + 2 * j]]));
            sumi += bsum * scale_min_k4(j / 2, &x[4..16]).1;
        }
        let mut aux32 = [0i32; 8];
        for j in 0..8 {
            let scale = scale_min_k4(j, &x[4..16]).0;
            for k in 0..32 {
                aux32[k % 8] += scale * (i32::from(yq[32 * j + k] as i8) * a[32 * j + k]);
            }
        }
        let d = half(x, 0) * yd;
        for l in 0..8 {
            sums[l] += d * aux32[l] as f32;
        }
        sumf -= half(x, 2) * yd * sumi as f32;
    }
    for s in sums {
        sumf += s;
    }
    sumf
}

fn dot_q8_0_q8_0(row: &[u8], y: &[u8]) -> f32 {
    let mut sumf = 0.0f32;
    for (x, y) in row.chunks(34).zip(y.chunks(34)) {
        let sumi: i32 = (2..34)
            .map(|j| i32::from(x[j] as i8) * i32::from(y[j] as i8))
            .sum();
        sumf += sumi as f32 * (half(x, 0) * half(y, 0));
    }
    sumf
}

fn dot_q4_0_q8_0(row: &[u8], y: &[u8]) -> f32 {
    let mut sumf = 0.0f32;
    for (x, y) in row.chunks(18).zip(y.chunks(34)) {
        let (mut s0, mut s1) = (0i32, 0i32);
        for j in 0..16 {
            s0 += (i32::from(x[2 + j] & 15) - 8) * i32::from(y[2 + j] as i8);
            s1 += (i32::from(x[2 + j] >> 4) - 8) * i32::from(y[18 + j] as i8);
        }
        sumf += (s0 + s1) as f32 * half(x, 0) * half(y, 0);
    }
    sumf
}

#[test]
fn q8_activation_dots_match_ggml_generic() {
    let acts = f32s(fixture!("q8-activations.f32"));
    let (q8k, q8_0) = (
        fixture!("q8-activations.q8_k"),
        fixture!("q8-activations.q8_0"),
    );
    let cases = [
        DotCase {
            name: "q4_k",
            weights: fixture!("q4k-smollm-blk11-ffn_down.q4k"),
            row_bytes: 6 * 144,
            expected: fixture!("q8-dots-q4_k_q8_k.f32"),
            dot: dot_q4_k_q8_k,
        },
        DotCase {
            name: "q8_0",
            weights: fixture!("q8-weights.q8_0"),
            row_bytes: 48 * 34,
            expected: fixture!("q8-dots-q8_0_q8_0.f32"),
            dot: dot_q8_0_q8_0,
        },
        DotCase {
            name: "q4_0",
            weights: fixture!("q8-weights.q4_0"),
            row_bytes: 48 * 18,
            expected: fixture!("q8-dots-q4_0_q8_0.f32"),
            dot: dot_q4_0_q8_0,
        },
    ];
    for (a, act) in acts.chunks(COLS).enumerate() {
        let yk = quantize_q8_k(act);
        assert_eq!(
            yk,
            q8k[a * 6 * 292..(a + 1) * 6 * 292],
            "Q8_K activation {a}"
        );
        let y0 = quantize_q8_0(act, COLS).unwrap();
        assert_eq!(
            y0,
            q8_0[a * 48 * 34..(a + 1) * 48 * 34],
            "Q8_0 activation {a}"
        );
        for c in &cases {
            let (name, row_bytes) = (c.name, c.row_bytes);
            let expected = f32s(c.expected);
            let y = if name == "q4_k" { &yk } else { &y0 };
            for r in 0..ROWS {
                let got = (c.dot)(&c.weights[r * row_bytes..(r + 1) * row_bytes], y);
                assert_eq!(
                    got.to_bits(),
                    expected[a * ROWS + r].to_bits(),
                    "{name} act {a} row {r}"
                );
            }
        }
    }
}

// --- FP8 scaling rules -----------------------------------------------------

fn fp8_kinds() -> [(Fp8, &'static str, i32); 2] {
    [(Fp8::E4M3, "e4m3", 8), (Fp8::E5M2, "e5m2", 15)]
}

fn amax(v: impl Iterator<Item = f32>) -> f32 {
    v.fold(0.0f32, |m, x| m.max(x.abs()))
}

#[test]
fn fp8_channel_block_and_mxfp8_codes_equal_ml_dtypes() {
    let x = f32s(fixture!("fp8-input.f32"));
    let n = 256;
    let expect = |kind: &str, what: &str| -> Vec<u8> {
        match (kind, what) {
            ("e4m3", "channel.scales") => fixture!("fp8-channel-e4m3.scales"),
            ("e4m3", "channel.codes") => fixture!("fp8-channel-e4m3.codes"),
            ("e5m2", "channel.scales") => fixture!("fp8-channel-e5m2.scales"),
            ("e5m2", "channel.codes") => fixture!("fp8-channel-e5m2.codes"),
            ("e4m3", "block.scales") => fixture!("fp8-block128-e4m3.scales"),
            ("e4m3", "block.codes") => fixture!("fp8-block128-e4m3.codes"),
            ("e5m2", "block.scales") => fixture!("fp8-block128-e5m2.scales"),
            ("e5m2", "block.codes") => fixture!("fp8-block128-e5m2.codes"),
            ("e4m3", "mx.scales") => fixture!("mxfp8-e4m3.e8m0"),
            ("e4m3", "mx.codes") => fixture!("mxfp8-e4m3.codes"),
            ("e5m2", "mx.scales") => fixture!("mxfp8-e5m2.e8m0"),
            ("e5m2", "mx.codes") => fixture!("mxfp8-e5m2.codes"),
            _ => unreachable!("no fixture {kind} {what}"),
        }
        .to_vec()
    };
    for (fp8, kind, emax) in fp8_kinds() {
        let mx = fp8.max_finite();
        let scale_of = |a: f32| if a > 0.0 { a / mx } else { 1.0 };
        // Per channel.
        let scales = f32s(&expect(kind, "channel.scales"));
        let codes = expect(kind, "channel.codes");
        for r in 0..n {
            let row = &x[r * n..(r + 1) * n];
            let s = scale_of(amax(row.iter().copied()));
            assert_eq!(s.to_bits(), scales[r].to_bits(), "{kind} channel scale {r}");
            for (c, &v) in row.iter().enumerate() {
                assert_eq!(
                    fp8.encode_saturating(v / s),
                    codes[r * n + c],
                    "{kind} channel r{r} c{c}"
                );
            }
        }
        // Per 128x128 block.
        let scales = f32s(&expect(kind, "block.scales"));
        let codes = expect(kind, "block.codes");
        for bi in 0..2 {
            for bj in 0..2 {
                let at = |r: usize, c: usize| x[(bi * 128 + r) * n + bj * 128 + c];
                let s = scale_of(amax((0..128 * 128).map(|i| at(i / 128, i % 128))));
                assert_eq!(
                    s.to_bits(),
                    scales[bi * 2 + bj].to_bits(),
                    "{kind} block scale"
                );
                for i in 0..128 * 128 {
                    let (r, c) = (bi * 128 + i / 128, bj * 128 + i % 128);
                    assert_eq!(
                        fp8.encode_saturating(at(i / 128, i % 128) / s),
                        codes[r * n + c]
                    );
                }
            }
        }
        // MXFP8: shared E8M0 exponent floor(log2(amax)) - emax_elem.
        let e8 = expect(kind, "mx.scales");
        let codes = expect(kind, "mx.codes");
        for (b, blk) in x.chunks(32).enumerate() {
            let a = amax(blk.iter().copied());
            let e = if a == 0.0 {
                -127
            } else {
                (((a.to_bits() >> 23) & 0xff) as i32 - 127 - emax).clamp(-127, 127)
            };
            assert!(a == 0.0 || a.is_normal(), "fixture amax is normal");
            assert_eq!(i32::from(e8[b]), e + 127, "{kind} MX scale {b}");
            let unit = 2f64.powi(e);
            for (i, &v) in blk.iter().enumerate() {
                let code = fp8.encode_saturating((f64::from(v) / unit) as f32);
                assert_eq!(code, codes[b * 32 + i], "{kind} MX block {b} elem {i}");
                // Decoded value = fp8 * 2^(e8 - 127).
                let back = f64::from(fp8.decode(code)) * unit;
                assert!(back.is_finite());
            }
        }
    }
}

#[test]
fn gpt2_case_file_is_well_formed() {
    let b = fixture!("gpt2-cases.bin");
    let u32_at = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
    let count = u32_at(0);
    let mut at = 4;
    for _ in 0..count {
        let len = u32_at(at);
        let text = std::str::from_utf8(&b[at + 4..at + 4 + len]).expect("UTF-8 text");
        at += 4 + len;
        let n = u32_at(at);
        at += 4;
        let ids: Vec<usize> = (0..n).map(|i| u32_at(at + 4 * i)).collect();
        at += 4 * n;
        assert_eq!(text.is_empty(), ids.is_empty());
        assert!(ids.iter().all(|&id| id < 50_257), "GPT-2 vocabulary");
        // Byte-level BPE never needs more tokens than bytes.
        assert!(ids.len() <= text.len());
    }
    assert_eq!(at, b.len());
    assert!(count >= 40);
    let tokenizer = fixture!("gpt2-tokenizer.json");
    assert!(tokenizer.starts_with(br#"{"version":"1.0""#));
}
