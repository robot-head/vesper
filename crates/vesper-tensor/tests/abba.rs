//! ABBA benchmarks of the Vesper port against the Rust build (port wave P1
//! lane 8, #905), with `vesper-bench`: dequantization (Q4_K and Q8_0 rows),
//! a Q8_0 matrix-vector product, Q4_K quantization, top-p sampling and BPE
//! prompt encoding. Both
//! builds are in one process: `vesper_tensor` is the port-backed package and
//! `seed` the Rust seed copy (a dev-dependency). The benchmarks are ignored
//! by default; run them with
//!
//! ```text
//! cargo test --release -p vesper-tensor --test abba -- --ignored --nocapture --test-threads=1
//! ```
use std::hint::black_box;

use vesper_bench::{analyze_pairs, run_abba, BenchCase, Invocation, RunConfig};

/// Deterministic pseudo-random values in about `[-1, 1]`.
fn values(seed: u64, n: usize) -> Vec<f32> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((x >> 40) as f32 / (1u64 << 23) as f32) - 1.0
        })
        .collect()
}

struct Case<F>(F);
impl<F: FnMut() -> u64> BenchCase for Case<F> {
    type Input = ();
    type Output = u64;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<(), ()> {
        Ok(())
    }
    fn run(&mut self, (): ()) -> Result<u64, ()> {
        Ok(black_box((self.0)()))
    }
}

fn checksum(v: &[f32]) -> u64 {
    v.iter()
        .fold(0u64, |h, x| h.rotate_left(5) ^ u64::from(x.to_bits()))
}

/// Runs one ABBA comparison and prints the Vesper/Rust ratio with the 2x
/// stop rule of the wave plan.
fn abba(name: &str, rust: impl FnMut() -> u64, vesper: impl FnMut() -> u64) {
    let run = RunConfig {
        epochs: 20,
        warmup_epochs: 2,
        operations_per_window: 4,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let report = run_abba(run, &mut Case(rust), &mut Case(vesper)).expect("ABBA run");
    let pairs = report.pairs();
    let mut ratios: Vec<f64> = pairs
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let per_op = |total: u64, p: &vesper_bench::PairedSample| {
        total as f64 / p.operations_per_arm() as f64 / 1e3
    };
    let mut rust_us: Vec<f64> = pairs
        .iter()
        .map(|p| per_op(p.baseline_total(), p))
        .collect();
    let mut vesper_us: Vec<f64> = pairs
        .iter()
        .map(|p| per_op(p.candidate_total(), p))
        .collect();
    rust_us.sort_by(f64::total_cmp);
    vesper_us.sort_by(f64::total_cmp);
    let mid = pairs.len() / 2;
    println!(
        "{name}: {} epochs; median per op rust {:.1} us, vesper {:.1} us; Vesper/Rust median {:.3} (min {:.3}, max {:.3})",
        pairs.len(),
        rust_us[mid],
        vesper_us[mid],
        ratios[mid],
        ratios[0],
        ratios[ratios.len() - 1]
    );
    let analysis = analyze_pairs(pairs, 10_000).expect("analysis");
    if let Some(i) = analysis.interval {
        println!(
            "{name}: 95% interval of Vesper/Rust [{:.3}, {:.3}]; 2x gate {:?}",
            i.lower.candidate_total() as f64 / i.lower.baseline_total() as f64,
            i.upper.candidate_total() as f64 / i.upper.baseline_total() as f64,
            analysis.decision
        );
    }
}

const ROWS: usize = 64;
const COLS: usize = 4096;

macro_rules! matrices {
    ($krate:ident, $format:ident) => {{
        let data = values(7, ROWS * COLS);
        $krate::quant::Matrix::f32(ROWS, COLS, data)
            .unwrap()
            .quantize($krate::quant::WeightFormat::$format)
            .unwrap()
    }};
}

#[test]
fn both_builds_agree_on_the_workloads() {
    for (r, v) in [
        (matrices!(seed, Q4K), matrices!(vesper_tensor, Q4K)),
        (matrices!(seed, Q8_0), matrices!(vesper_tensor, Q8_0)),
    ] {
        assert_eq!(r.raw_bytes(), v.raw_bytes());
        assert_eq!(
            checksum(&r.to_f32().unwrap()),
            checksum(&v.to_f32().unwrap())
        );
    }
    assert_eq!(seed::IMPLEMENTATION, "rust");
    assert_eq!(vesper_tensor::IMPLEMENTATION, "vesper");
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_dequantize_q4_k() {
    let r = matrices!(seed, Q4K);
    let v = matrices!(vesper_tensor, Q4K);
    abba(
        "dequantize Q4_K 64x4096",
        || checksum(&r.to_f32().unwrap()),
        || checksum(&v.to_f32().unwrap()),
    );
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_dequantize_q8_0() {
    let r = matrices!(seed, Q8_0);
    let v = matrices!(vesper_tensor, Q8_0);
    abba(
        "dequantize Q8_0 64x4096",
        || checksum(&r.to_f32().unwrap()),
        || checksum(&v.to_f32().unwrap()),
    );
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_matvec_q8_0() {
    let r = matrices!(seed, Q8_0);
    let v = matrices!(vesper_tensor, Q8_0);
    let x = values(3, COLS);
    let (mut ro, mut vo) = (vec![0.0; ROWS], vec![0.0; ROWS]);
    abba(
        "matvec Q8_0 64x4096 (scalar kernel)",
        || {
            r.matvec(&x, &mut ro).unwrap();
            checksum(&ro)
        },
        || {
            v.matvec(&x, &mut vo).unwrap();
            checksum(&vo)
        },
    );
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_quantize_q4_k() {
    let data = values(11, 16 * COLS);
    abba(
        "quantize Q4_K 16x4096",
        || seed::formats::quantize_q4_k(&data, COLS).unwrap().len() as u64,
        || {
            vesper_tensor::formats::quantize_q4_k(&data, COLS)
                .unwrap()
                .len() as u64
        },
    );
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_sample_top_p() {
    const VOCAB: usize = 32_000;
    let logits = values(5, VOCAB)
        .into_iter()
        .map(|v| v * 8.0)
        .collect::<Vec<_>>();
    let mut r = seed::sampling::Sampler::new(
        VOCAB,
        seed::sampling::SamplerConfig {
            temperature: 0.8,
            top_p: 0.9,
            seed: 42,
        },
    )
    .unwrap();
    let mut v = vesper_tensor::sampling::Sampler::new(
        VOCAB,
        vesper_tensor::sampling::SamplerConfig {
            temperature: 0.8,
            top_p: 0.9,
            seed: 42,
        },
    )
    .unwrap();
    abba(
        "sample temperature 0.8 top-p 0.9, vocab 32000",
        || u64::from(r.sample(&logits).unwrap()),
        || u64::from(v.sample(&logits).unwrap()),
    );
}

/// A deterministic ASCII prompt of about `n` bytes: words of 2 to 9 letters.
fn prompt(n: usize) -> Vec<u8> {
    let mut x = 7u64;
    let mut out = Vec::with_capacity(n + 10);
    while out.len() < n {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let len = 2 + (x >> 60) as usize % 8;
        for k in 0..len {
            out.push(b'a' + ((x >> (8 * k as u32 % 56)) % 26) as u8);
        }
        out.push(b' ');
    }
    out
}

const TOK512: &[u8] = include_bytes!("../models/tok512.bin");

#[test]
fn both_builds_encode_the_same_prompt() {
    let r = seed::llama::Tokenizer::from_llama2c(TOK512, 512).unwrap();
    let v = vesper_tensor::llama::Tokenizer::from_llama2c(TOK512, 512).unwrap();
    let text = prompt(4096);
    assert_eq!(
        r.encoder().encode(&text, true, true).unwrap(),
        v.encoder().encode(&text, true, true).unwrap()
    );
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_bpe_encode() {
    let r = seed::llama::Tokenizer::from_llama2c(TOK512, 512).unwrap();
    let v = vesper_tensor::llama::Tokenizer::from_llama2c(TOK512, 512).unwrap();
    let (re, ve) = (r.encoder(), v.encoder());
    let text = prompt(16 * 1024);
    abba(
        "BPE encode 16 KiB prompt (tok512)",
        || re.encode(&text, true, false).unwrap().len() as u64,
        || ve.encode(&text, true, false).unwrap().len() as u64,
    );
}

/// A safetensors file whose header is `header`, with `data` zero bytes.
fn safetensors_file(header: &[u8], data: usize) -> Vec<u8> {
    let mut file = (header.len() as u64).to_le_bytes().to_vec();
    file.extend_from_slice(header);
    file.resize(file.len() + data, 0);
    file
}

fn read_safetensors<T: std::fmt::Debug, M: std::fmt::Debug, E: std::fmt::Debug>(
    r: Result<(T, M), E>,
) -> String {
    match r {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("error {e:?}"),
    }
}

#[test]
fn both_builds_read_the_same_safetensors_headers() {
    const VALID: &[&str] = &[
        "{",
        r#""__metadata__""#,
        ":",
        "{",
        r#""k""#,
        ":",
        r#""vé😀""#,
        "}",
        ",",
        r#""w""#,
        ":",
        "{",
        r#""dtype""#,
        ":",
        r#""F32""#,
        ",",
        r#""shape""#,
        ":",
        "[",
        "2",
        "]",
        ",",
        r#""data_offsets""#,
        ":",
        "[",
        "0",
        ",",
        "8",
        "]",
        "}",
        "}",
    ];
    const NOISE: &[&str] = &[
        "{",
        "}",
        "[",
        "]",
        ":",
        ",",
        " ",
        "\t",
        "0",
        "01",
        "1.5",
        "-1",
        "1e3",
        "18446744073709551615",
        "18446744073709551616",
        "true",
        "false",
        "null",
        "nul",
        r#""a""#,
        r#""\ud800""#,
        r#""\udc00x""#,
        r#""\ud800A""#,
        r#""\x""#,
        "\"\u{1}\"",
        "\"\u{e9}\"",
        r#""\"\\\/\b\f\n\r\t""#,
        r#""\u12""#,
        "\"",
        "x",
    ];
    let mut x = 99u64;
    let mut next = |n: usize| {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (x >> 33) as usize % n
    };
    let mut cases: Vec<Vec<u8>> = vec![VALID.concat().into_bytes()];
    for _ in 0..3000 {
        let mut tokens: Vec<&str> = VALID.to_vec();
        for _ in 0..1 + next(3) {
            let at = next(tokens.len());
            match next(3) {
                0 => {
                    tokens.remove(at);
                }
                1 => tokens.insert(at, NOISE[next(NOISE.len())]),
                _ => tokens[at] = NOISE[next(NOISE.len())],
            }
        }
        cases.push(tokens.concat().into_bytes());
    }
    for _ in 0..1000 {
        let tokens: Vec<&str> = (0..next(12)).map(|_| NOISE[next(NOISE.len())]).collect();
        cases.push(tokens.concat().into_bytes());
    }
    cases.push(b"{\"a\":\"\xff\"}".to_vec());
    cases.push(b"{\"a\":\"\xed\xa0\x80\"}".to_vec());
    cases.push(b"{\"a\":\"\xf4\x90\x80\x80\"}".to_vec());
    cases.push(b"{\"a\":\"\xe0\x80\x80\"}".to_vec());
    cases.push(b"{\"a\":\"\xf0\x9f\x98\x80\xc3\"}".to_vec());
    cases.push(b"[[[[[[[[[]]]]]]]]]".to_vec());
    cases.push(b"[[[[[[[[[[]]]]]]]]]]".to_vec());
    let mut big = String::from("{");
    for i in 0..5000 {
        big.push_str(&format!(
            "\"t{i}\":{{\"dtype\":\"U8\",\"shape\":[0],\"data_offsets\":[0,0]}},"
        ));
    }
    big.push_str("\"t7\":1}");
    cases.push(big.into_bytes());
    for header in &cases {
        let file = safetensors_file(header, 8);
        let r = seed::safetensors::SafeTensors::from_vec(file.clone())
            .map(|s| (s.tensors().to_vec(), s.metadata().to_vec()));
        let v = vesper_tensor::safetensors::SafeTensors::from_vec(file)
            .map(|s| (s.tensors().to_vec(), s.metadata().to_vec()));
        assert_eq!(
            read_safetensors(r),
            read_safetensors(v),
            "{}",
            String::from_utf8_lossy(header)
        );
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_safetensors_header() {
    let mut header = String::from("{");
    for i in 0..5000 {
        if i > 0 {
            header.push(',');
        }
        header.push_str(&format!(
            "\"model.layers.{i}.weight\":{{\"dtype\":\"U8\",\"shape\":[0],\"data_offsets\":[0,0]}}"
        ));
    }
    header.push('}');
    let file = safetensors_file(header.as_bytes(), 0);
    abba(
        "safetensors header, 5000 tensors (JSON reader)",
        || {
            seed::safetensors::SafeTensors::from_vec(file.clone())
                .unwrap()
                .tensors()
                .len() as u64
        },
        || {
            vesper_tensor::safetensors::SafeTensors::from_vec(file.clone())
                .unwrap()
                .tensors()
                .len() as u64
        },
    );
}

fn read_gguf(r: Result<impl GgufView, impl std::fmt::Debug>) -> String {
    match r {
        Ok(g) => g.view(),
        Err(e) => format!("error {e:?}"),
    }
}

trait GgufView {
    fn view(&self) -> String;
}
macro_rules! gguf_view {
    ($krate:ident) => {
        impl GgufView for $krate::gguf::Gguf {
            fn view(&self) -> String {
                format!(
                    "{} {} {:?} {:?}",
                    self.version(),
                    self.alignment(),
                    self.metadata(),
                    self.tensors()
                )
            }
        }
    };
}
gguf_view!(seed);
gguf_view!(vesper_tensor);

/// A GGUF file with every metadata type, written by the Rust seed.
fn sample_gguf(alignment: Option<u32>) -> Vec<u8> {
    use seed::gguf::{write, GgmlType, TensorData, Value};
    let mut metadata = vec![
        ("general.name".to_string(), Value::String("tiny é".into())),
        ("a.u8".into(), Value::U8(7)),
        ("a.i8".into(), Value::I8(-7)),
        ("a.u16".into(), Value::U16(700)),
        ("a.i16".into(), Value::I16(-700)),
        ("a.u32".into(), Value::U32(70_000)),
        ("a.i32".into(), Value::I32(-70_000)),
        ("a.f32".into(), Value::F32(-1.5)),
        ("a.bool".into(), Value::Bool(true)),
        ("a.u64".into(), Value::U64(1 << 40)),
        ("a.i64".into(), Value::I64(-(1 << 40))),
        ("a.f64".into(), Value::F64(2.25)),
        (
            "a.strings".into(),
            Value::Array(vec![Value::String("x".into()), Value::String("yz".into())]),
        ),
        (
            "a.floats".into(),
            Value::Array(vec![Value::F32(1.0), Value::F32(-0.5)]),
        ),
        ("a.empty".into(), Value::Array(vec![])),
    ];
    if let Some(a) = alignment {
        metadata.push(("general.alignment".into(), Value::U32(a)));
    }
    let tensors = vec![
        TensorData {
            name: "w".into(),
            dims: vec![32, 2],
            ty: GgmlType::Q8_0,
            bytes: vec![1; 68],
        },
        TensorData {
            name: "v".into(),
            dims: vec![3],
            ty: GgmlType::F32,
            bytes: vec![2; 12],
        },
    ];
    write(&metadata, &tensors).unwrap()
}

#[test]
fn both_builds_read_the_same_gguf_files() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![
        sample_gguf(None),
        sample_gguf(Some(64)),
        std::fs::read(dir.join("fixtures/interop.gguf")).unwrap(),
        std::fs::read(dir.join("fixtures/stories260K-q8_0.gguf")).unwrap(),
    ];
    let mut x = 5u64;
    let mut next = |n: usize| {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (x >> 33) as usize % n
    };
    // Corrupt counts fail with the count limits, without sizing buffers from
    // them (the file is 64 MiB, so `len / 8` alone would allow 8M entries).
    let mut huge = sample_gguf(None);
    huge[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    huge[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    huge.resize(64 << 20, 0);
    files.push(huge.clone());
    huge[8..16].copy_from_slice(&2u64.to_le_bytes());
    files.push(huge);
    let base: Vec<Vec<u8>> = files[..4].to_vec();
    for file in &base[..2] {
        // Every truncation of the header, and single-byte edits in it.
        for len in 0..file.len().min(700) {
            files.push(file[..len].to_vec());
        }
        for _ in 0..3000 {
            let mut f = file.clone();
            let at = next(f.len().min(700));
            f[at] = match next(4) {
                0 => 0,
                1 => 0xff,
                2 => f[at] ^ (1 << next(8)),
                _ => next(256) as u8,
            };
            files.push(f);
        }
    }
    let mut errors = std::collections::BTreeSet::new();
    for file in files {
        let r = read_gguf(seed::gguf::Gguf::from_vec(file.clone()));
        let v = read_gguf(vesper_tensor::gguf::Gguf::from_vec(file));
        assert_eq!(r, v);
        if r.starts_with("error") {
            errors.insert(r);
        }
    }
    // The edits reach most of the reader's checks.
    assert!(errors.len() >= 12, "{errors:#?}");
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_gguf_header() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let file = std::fs::read(dir.join("fixtures/stories260K-q8_0.gguf")).unwrap();
    abba(
        "GGUF header, stories260K (512-piece vocabulary)",
        || {
            seed::gguf::Gguf::from_vec(file.clone())
                .unwrap()
                .metadata()
                .len() as u64
        },
        || {
            vesper_tensor::gguf::Gguf::from_vec(file.clone())
                .unwrap()
                .metadata()
                .len() as u64
        },
    );
}
