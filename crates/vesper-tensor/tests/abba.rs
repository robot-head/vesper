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

macro_rules! kv_script {
    ($krate:ident, $ops:expr, $format:expr) => {{
        use $krate::kv::*;
        let format = if $format {
            KvFormat::F32
        } else {
            KvFormat::Bf16
        };
        let mut pool = KvPool::with_format(
            KvConfig {
                heads: 2,
                head_dim: 2,
                pages: 4,
                max_sequences: 3,
                max_tokens_per_sequence: 20,
                max_bytes: 1 << 16,
            },
            format,
        )
        .unwrap();
        let mut log = Vec::new();
        let mut live: Vec<Option<Sequence>> = vec![None, None, None];
        let mut marks: Vec<Checkpoint> = Vec::new();
        for &(op, which, arg) in $ops {
            let which = which % 3;
            let entry = match op % 8 {
                0 => match pool.begin() {
                    Ok(s) => {
                        let old = live[which].replace(s);
                        if let Some(old) = old {
                            format!("{:?}", pool.release(old).map_err(|f| f.error))
                        } else {
                            "begin".into()
                        }
                    }
                    Err(e) => format!("{e:?}"),
                },
                1..=3 => match live[which].as_mut() {
                    Some(s) => {
                        let position = if arg % 7 == 0 {
                            arg % 50
                        } else {
                            pool.len(s).unwrap()
                        };
                        let fill = (arg % 251) as u8;
                        let r = if $format {
                            let v = [f32::from_bits(arg as u32 | 1); 4];
                            pool.append_f32(s, position, &v, &v)
                        } else {
                            let bytes = [fill; 8];
                            let step = Bf16Step {
                                heads: 2,
                                head_dim: if arg % 13 == 0 { 3 } else { 2 },
                                bytes: &bytes,
                            };
                            pool.append(s, position, step, step)
                        };
                        format!("{r:?}")
                    }
                    None => "none".into(),
                },
                4 => match live[which].as_ref() {
                    Some(s) => {
                        let len = pool.len(s).unwrap();
                        let mut out = format!("len {len} pages {:?}", pool.page_count(s));
                        for p in 0..len + 1 {
                            out.push_str(&format!(
                                " {:?}",
                                pool.read(s, p).map(|t| (t.key.to_vec(), t.value.to_vec()))
                            ));
                        }
                        for i in 0..pool.page_count(s).unwrap() + 1 {
                            out.push_str(&format!(
                                " {:?}",
                                pool.read_page(s, i).map(|p| (
                                    p.first_position,
                                    p.tokens,
                                    p.keys.to_vec()
                                ))
                            ));
                        }
                        out.push_str(&format!(
                            " {:?}",
                            pool.read_head(s, 0, arg % 3).map(|t| t.key.to_vec())
                        ));
                        out
                    }
                    None => "none".into(),
                },
                5 => match live[which].as_ref() {
                    Some(s) => match pool.checkpoint(s) {
                        Ok(c) => {
                            marks.push(c);
                            "mark".into()
                        }
                        Err(e) => format!("{e:?}"),
                    },
                    None => "none".into(),
                },
                6 => match (live[which].as_mut(), marks.len()) {
                    (Some(s), n) if n > 0 => {
                        let m = &marks[arg % n];
                        format!("{:?}", pool.rollback(s, m))
                    }
                    (Some(s), _) => format!("{:?}", pool.truncate(s, arg % 20)),
                    _ => "none".into(),
                },
                _ => match live[which].take() {
                    Some(s) => match pool.release(s) {
                        Ok(()) => "released".into(),
                        Err(f) => {
                            live[which] = Some(f.sequence);
                            format!("{:?}", f.error)
                        }
                    },
                    None => "none".into(),
                },
            };
            log.push(format!("{entry} {:?}", pool.stats()));
        }
        log
    }};
}

#[test]
fn both_builds_run_the_same_kv_scripts() {
    let mut x = 17u64;
    let mut seen = Vec::new();
    for script in 0..60 {
        // Each script starts by filling two sequences past their token limit
        // and the pool's pages, then appending to a third.
        let fill = [0, 1, 2].into_iter().flat_map(|which| {
            std::iter::once((0, which, 1)).chain(std::iter::repeat_n((1, which, 1), 22))
        });
        let ops: Vec<(usize, usize, usize)> = fill
            .chain((0..300).map(|_| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (
                    (x >> 40) as usize % 8,
                    (x >> 20) as usize % 3,
                    (x >> 4) as usize % 1000,
                )
            }))
            .collect();
        let f32_pool = script % 2 == 1;
        let rust = kv_script!(seed, &ops, f32_pool);
        assert_eq!(
            rust,
            kv_script!(vesper_tensor, &ops, f32_pool),
            "script {script}"
        );
        seen.extend(rust);
    }
    // The scripts reach every outcome they can.
    let all = seen.join("\n");
    for outcome in [
        "Ok(())",
        "released",
        "mark",
        "Position",
        "TokenLimit",
        "OutOfPages",
        "SequenceLimit",
        "StaleCheckpoint",
        "Shape",
        "Ok((",
    ] {
        assert!(all.contains(outcome), "no {outcome}");
    }
    // Configuration checks.
    for (heads, head_dim, pages, sequences, tokens, bytes) in [
        (0, 1, 1, 1, 1, 64),
        (1, 1, 4097, 1, 1, 1 << 20),
        (usize::MAX, 2, 1, 1, 1, 64),
        (64, 64, 64, 1, 16, 1 << 20),
        (1, 1, 2, 1, 33, 1 << 10),
        (1, 1, 4096, 1024, 65_536, 1 << 26),
        (1, 1, 2, 2, 32, 128),
    ] {
        let config = |k: &str| {
            format!(
                "{k} {:?}",
                (heads, head_dim, pages, sequences, tokens, bytes)
            )
        };
        let r = seed::kv::KvPool::new(seed::kv::KvConfig {
            heads,
            head_dim,
            pages,
            max_sequences: sequences,
            max_tokens_per_sequence: tokens,
            max_bytes: bytes,
        })
        .map(|p| p.stats());
        let v = vesper_tensor::kv::KvPool::new(vesper_tensor::kv::KvConfig {
            heads,
            head_dim,
            pages,
            max_sequences: sequences,
            max_tokens_per_sequence: tokens,
            max_bytes: bytes,
        })
        .map(|p| p.stats());
        assert_eq!(
            format!("{:?}", r),
            format!("{:?}", v),
            "{}",
            config("config")
        );
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_kv_append_read() {
    macro_rules! run {
        ($krate:ident) => {{
            use $krate::kv::*;
            let mut pool = KvPool::with_format(
                KvConfig {
                    heads: 8,
                    head_dim: 64,
                    pages: 64,
                    max_sequences: 1,
                    max_tokens_per_sequence: 1024,
                    max_bytes: 64 << 20,
                },
                KvFormat::F32,
            )
            .unwrap();
            let step = values(9, 8 * 64);
            move || {
                let mut s = pool.begin().unwrap();
                let mut sum = 0u64;
                for p in 0..1024 {
                    pool.append_f32(&mut s, p, &step, &step).unwrap();
                    sum += u64::from(pool.read(&s, p).unwrap().key[0]);
                }
                for i in 0..pool.page_count(&s).unwrap() {
                    sum += pool.read_page(&s, i).unwrap().tokens as u64;
                }
                pool.truncate(&mut s, 100).unwrap();
                pool.release(s).unwrap();
                sum
            }
        }};
    }
    abba(
        "K/V pool: 1,024 FP32 appends and reads, 8x64 heads",
        run!(seed),
        run!(vesper_tensor),
    );
}

/// `text` without the arena owner tags, which differ between the builds:
/// `DimId(3, 17)` becomes `DimId(3)`.
fn without_owners(text: String) -> String {
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("DimId(") {
        let (head, tail) = rest.split_at(at + 6);
        out.push_str(head);
        let close = tail.find(')').unwrap();
        out.push_str(tail[..close].split(',').next().unwrap());
        rest = &tail[close..];
    }
    out.push_str(rest);
    out
}

/// A shape script: an arena built from `ops`, every node normalized under
/// two environments, and contractions, matmuls and GQA checks over its nodes
/// planned under the first and discharged under the second.
macro_rules! shape_script {
    ($krate:ident, $ops:expr, $seed:expr) => {{
        use $krate::shape::*;
        const NAMES: [&str; 6] = ["K", "Batch", "heads", "N", "a_1", "M"];
        let mut x: u64 = $seed;
        let mut next = move || {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            x >> 33
        };
        let mut log = Vec::new();
        let mut d = Dimensions::new(48, 7, 6).unwrap();
        let mut ids = Vec::new();
        for _ in 0..$ops {
            let r = match next() % 6 {
                0 => d.constant(match next() % 5 {
                    0 => 0,
                    1 => u64::MAX / 3,
                    2 => 1 << 40,
                    _ => next() % 9,
                }),
                1 | 2 => d.symbol(if next() % 17 == 0 {
                    "bad name"
                } else {
                    NAMES[(next() % 6) as usize]
                }),
                _ if ids.is_empty() => d.constant(2),
                _ => {
                    let a = ids[(next() as usize) % ids.len()];
                    let b = ids[(next() as usize) % ids.len()];
                    d.multiply(a, b)
                }
            };
            match r {
                Ok(id) => ids.push(id),
                Err(e) => log.push(format!("Err({e:?})")),
            }
        }
        let env = |seed: u64| {
            let mut env = Environment::new();
            for (i, name) in NAMES.iter().enumerate() {
                match (seed >> (3 * i)) % 8 {
                    0..=2 => (),
                    3 => {
                        env.insert((*name).into(), 0);
                    }
                    4 => {
                        env.insert((*name).into(), u64::MAX / 5);
                    }
                    v => {
                        env.insert((*name).into(), 2 * v);
                    }
                }
            }
            env.insert("unused".into(), 3);
            env
        };
        let plan = env(next());
        let bind = env(next());
        for &id in &ids {
            log.push(format!("{:?}", d.normalize(id, &plan)));
            log.push(format!("{:?}", d.value(id, &bind)));
        }
        let pick = |r: u64| ids[(r as usize) % ids.len()];
        for _ in 0..12 {
            if ids.is_empty() {
                break;
            }
            let (a, b) = (pick(next()), pick(next()));
            let checked = d.unify(a, b, &plan);
            log.push(format!("{checked:?}"));
            if let Ok(c) = checked {
                log.push(format!("{:?}", c.discharge(&d, &bind)));
            }
            let axes = [pick(next()), pick(next()), pick(next()), pick(next())];
            let block = [None, Some(0), Some(2), Some(32)][(next() % 4) as usize];
            let checked = d.matmul(axes, block, &plan);
            log.push(format!("{checked:?}"));
            if let Ok(c) = checked {
                log.push(format!("{:?}", c.discharge(&d, &bind)));
            }
            log.push(format!(
                "{:?}",
                d.resolved_matmul(axes, block, &bind, 1 << 20)
            ));
            let checked = d.gqa_rope(pick(next()), pick(next()), pick(next()), &plan);
            log.push(format!("{checked:?}"));
            if let Ok(c) = checked {
                log.push(format!("{:?}", c.discharge(&d, &bind)));
            }
        }
        let (q, k) = (next() % 12, next() % 5);
        log.push(format!(
            "{:?} {:?}",
            gqa_ratio(q, k),
            gqa_kv_head(q, k, next() % 12)
        ));
        without_owners(log.join("\n"))
    }};
}

#[test]
fn both_builds_plan_the_same_shapes() {
    let mut joined = String::new();
    for seed in 0..300u64 {
        let ops = 4 + seed % 60;
        let rust = shape_script!(seed, ops, seed);
        let port = shape_script!(vesper_tensor, ops, seed);
        assert_eq!(rust, port, "shape script {seed}");
        joined.push_str(&rust);
    }
    // The scripts reach every outcome, not just the easy ones.
    for outcome in [
        "\"Batch\", \"K\"",
        "\"K\", \"K\"",
        "Err(Overflow)",
        "Err(Limit(\"symbol factors\"))",
        "Err(Limit(\"dimension nodes/depth\"))",
        "Err(InvalidName)",
        "Err(Unresolved)",
        "Err(Shape(\"contraction mismatch\"))",
        "Err(Shape(\"zero dimension\"))",
        "Err(Shape(\"zero block size\"))",
        "Err(Shape(\"block alignment\"))",
        "Err(Shape(\"invalid GQA ratio\"))",
        "Err(Limit(\"matmul output\"))",
        "Fits(DimId(",
        "ProductFits(DimId(",
        "GqaRatio {",
        "Divisible(DimId(",
        "Ok([",
        "Ok(())",
    ] {
        assert!(joined.contains(outcome), "no script reached {outcome}");
    }
}

/// A llama-style plan: symbolic batch, sequence and head axes, a few
/// thousand products, normalized and checked as `attention.rs` does.
#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_shape_plan() {
    macro_rules! run {
        ($krate:ident) => {{
            use $krate::shape::*;
            let env = Environment::from([
                ("Batch".into(), 4),
                ("Seq".into(), 512),
                ("Heads".into(), 32),
                ("KvHeads".into(), 8),
            ]);
            move || {
                let mut sum = 0u64;
                let mut d = Dimensions::new(4096, 32, 32).unwrap();
                let batch = d.symbol("Batch").unwrap();
                let seq = d.symbol("Seq").unwrap();
                let heads = d.symbol("Heads").unwrap();
                let kv = d.symbol("KvHeads").unwrap();
                let width = d.constant(128).unwrap();
                let tokens = d.multiply(batch, seq).unwrap();
                for layer in 0..64u64 {
                    let hidden = d.multiply(heads, width).unwrap();
                    let kv_width = d.multiply(kv, width).unwrap();
                    let ffn = d.constant(11008 + layer).unwrap();
                    let planned = d
                        .matmul(
                            [tokens, hidden, hidden, kv_width],
                            Some(32),
                            &Environment::new(),
                        )
                        .unwrap();
                    sum += planned.obligations().len() as u64;
                    planned.discharge(&d, &env).unwrap();
                    let [m, n] = d
                        .resolved_matmul([tokens, hidden, hidden, ffn], Some(32), &env, 1 << 30)
                        .unwrap();
                    sum += m ^ n;
                    d.gqa_rope(heads, kv, width, &env)
                        .unwrap()
                        .discharge(&d, &env)
                        .unwrap();
                    let wide = d.multiply(tokens, hidden).unwrap();
                    sum += d
                        .normalize(wide, &Environment::new())
                        .unwrap()
                        .symbols
                        .len() as u64;
                }
                sum
            }
        }};
    }
    abba(
        "shape plan: 64 llama layers, matmul/GQA plans and discharges",
        run!(seed),
        run!(vesper_tensor),
    );
}

/// A tiny llama2.c checkpoint: header `shape`, then every weight run in
/// order, with `nan_at` (a float index) replaced by NaN.
fn tiny_checkpoint(shape: [i32; 7], nan_at: Option<usize>) -> Vec<u8> {
    let [dim, hidden, layers, heads, kv_heads, vocab, seq_len] =
        shape.map(|v| v.unsigned_abs() as usize);
    let kv_dim = kv_heads * (dim / heads.max(1));
    let head_dim = dim / heads.max(1);
    let mut floats = vocab * dim
        + layers * (2 * dim + 2 * dim * dim + 2 * kv_dim * dim + 3 * hidden * dim)
        + dim
        + seq_len * head_dim;
    if shape[5] < 0 {
        floats += vocab * dim;
    }
    let mut out = Vec::new();
    for v in shape {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let v = values(17, floats);
    for (i, x) in v.into_iter().enumerate() {
        let x = if Some(i) == nan_at { f32::NAN } else { x };
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

macro_rules! read_checkpoint {
    ($krate:ident, $bytes:expr) => {
        match $krate::llama::Model::from_llama2c($bytes) {
            Ok(m) => format!(
                "{:?}",
                (
                    m.config(),
                    m.layers(),
                    m.rms_final(),
                    m.embedding().to_f32(),
                    m.classifier().to_f32()
                )
            ),
            Err(e) => format!("{e:?}"),
        }
    };
}

macro_rules! read_tokenizer {
    ($krate:ident, $bytes:expr, $vocab:expr) => {
        match $krate::llama::Tokenizer::from_llama2c($bytes, $vocab) {
            Ok(t) => {
                let mut log = format!("{} {}", t.vocab(), t.max_token_length());
                for i in 0..t.vocab() as u32 {
                    log.push_str(&format!(
                        " {:?}{:x}{:?}{:?}",
                        t.piece(i),
                        t.score(i).unwrap().to_bits(),
                        t.decode(1, i),
                        t.decode(5, i)
                    ));
                }
                log
            }
            Err(e) => format!("{e:?}"),
        }
    };
}

#[test]
fn both_builds_read_the_same_llama2c_files() {
    let mut outcomes = std::collections::BTreeSet::new();
    let mut compare = |bytes: &[u8]| {
        let (r, v) = (
            read_checkpoint!(seed, bytes),
            read_checkpoint!(vesper_tensor, bytes),
        );
        assert_eq!(r, v, "checkpoint of {} bytes", bytes.len());
        let kind = r.split('(').next().unwrap();
        outcomes.insert(if kind.is_empty() { "Ok" } else { kind }.to_owned());
    };
    let base = [8, 12, 2, 4, 2, 10, 6];
    let good = tiny_checkpoint(base, None);
    compare(&good);
    compare(&tiny_checkpoint([8, 12, 2, 4, 2, -10, 6], None));
    for cut in 0..good.len() {
        compare(&good[..cut]);
    }
    let mut long = good.clone();
    long.push(0);
    compare(&long);
    let floats = (good.len() - 28) / 4;
    for at in (0..floats).step_by(7) {
        compare(&tiny_checkpoint(base, Some(at)));
    }
    for field in 0..7 {
        for v in [0, -1, i32::MIN, 3, 5, 7, 64, 8193, 65537, 1 << 21, i32::MAX] {
            let mut bytes = good.clone();
            bytes[4 * field..4 * field + 4].copy_from_slice(&v.to_le_bytes());
            compare(&bytes);
            compare(&bytes[..28 + 4 * 400]);
        }
    }
    for shape in [
        [8, 12, 2, 3, 3, 10, 6],
        [12, 12, 1, 4, 3, 10, 6],
        [6, 12, 1, 2, 2, 10, 6],
    ] {
        compare(&tiny_checkpoint(shape, None));
    }
    compare(include_bytes!("../models/stories260K.bin"));
    for e in ["Ok", "Malformed", "Limit", "Shape", "NonFinite"] {
        assert!(
            outcomes.contains(e),
            "no checkpoint reached {e}: {outcomes:?}"
        );
    }

    let mut outcomes = std::collections::BTreeSet::new();
    let mut compare = |bytes: &[u8], vocab: usize| {
        let (r, v) = (
            read_tokenizer!(seed, bytes, vocab),
            read_tokenizer!(vesper_tensor, bytes, vocab),
        );
        assert_eq!(r, v, "tokenizer of {} bytes", bytes.len());
        outcomes.insert(r.split([' ', '(']).next().unwrap().to_owned());
    };
    compare(TOK512, 512);
    for vocab in [0, 1, 511, 513, (1 << 20) + 1] {
        compare(TOK512, vocab);
    }
    for cut in (0..TOK512.len()).step_by(13) {
        compare(&TOK512[..cut], 512);
    }
    for at in (0..64).chain((64..TOK512.len()).step_by(29)) {
        for b in [0u8, 0x80, 0xff, 0x7f] {
            let mut bytes = TOK512.to_vec();
            bytes[at] = b;
            compare(&bytes, 512);
        }
    }
    // Pieces the decoder treats specially.
    let mut crafted = 64i32.to_le_bytes().to_vec();
    let pieces: [&[u8]; 16] = [
        b"<0x0A>", b"<0x+A>", b"<0x-A>", b"<0xG1>", b" <0x41>", b"<0x41", b"<0x07>", b"\x07", b" ",
        b"  ", b" a", b"<0xff>", b"<0xFF>", b"\t", b"\x0b", b"<0x0B>",
    ];
    for p in pieces {
        crafted.extend_from_slice(&1.5f32.to_le_bytes());
        crafted.extend_from_slice(&(p.len() as i32).to_le_bytes());
        crafted.extend_from_slice(p);
    }
    compare(&crafted, pieces.len());
    for e in ["Malformed", "Limit", "NonFinite"] {
        assert!(
            outcomes.contains(e),
            "no tokenizer reached {e}: {outcomes:?}"
        );
    }
}

/// Scalar-kernel paged attention: every query head, both schedules, both
/// K/V formats, GQA ratios 1-4, causal limits and non-finite keys.
macro_rules! attention_script {
    ($krate:ident, $seed:expr) => {{
        use $krate::attention::*;
        use $krate::kv::*;
        use $krate::simd::Kernel;
        let seed: u64 = $seed;
        let kv_heads = 1 + (seed % 3) as usize;
        let group = 1 + (seed / 3 % 4) as usize;
        let head_dim = [2, 4, 6, 16][(seed / 12 % 4) as usize];
        let format = if seed % 2 == 0 {
            KvFormat::F32
        } else {
            KvFormat::Bf16
        };
        let mut pool = KvPool::with_format(
            KvConfig {
                heads: kv_heads,
                head_dim,
                pages: 8,
                max_sequences: 2,
                max_tokens_per_sequence: 100,
                max_bytes: 1 << 20,
            },
            format,
        )
        .unwrap();
        let mut s = pool.begin().unwrap();
        let tokens = 1 + (seed * 7 % 60) as usize;
        let width = kv_heads * head_dim;
        for p in 0..tokens {
            let mut k = values(seed * 131 + p as u64, width);
            let v = values(seed * 137 + p as u64, width);
            if seed % 23 == 5 && p == tokens / 2 {
                k[0] = f32::INFINITY;
            }
            match format {
                KvFormat::F32 => pool.append_f32(&mut s, p, &k, &v).unwrap(),
                KvFormat::Bf16 => {
                    let bytes = |x: &[f32]| -> Vec<u8> {
                        x.iter()
                            .flat_map(|f| ((f.to_bits() >> 16) as u16).to_le_bytes())
                            .collect()
                    };
                    let (kb, vb) = (bytes(&k), bytes(&v));
                    let step = |bytes| Bf16Step {
                        heads: kv_heads,
                        head_dim,
                        bytes,
                    };
                    pool.append(&mut s, p, step(&kb[..]), step(&vb[..]))
                        .unwrap()
                }
            }
        }
        let query_heads = kv_heads * group;
        let q = values(seed * 139, query_heads * head_dim);
        let shape = AttentionShape {
            query_heads,
            kv_heads,
            head_dim,
        };
        let mut log = Vec::new();
        for mode in [AttentionMode::TwoPass, AttentionMode::Online] {
            for visible in [
                None,
                Some(1),
                Some(tokens / 2),
                Some(tokens),
                Some(tokens + 1),
            ] {
                let mut scores = vec![0.0f32; query_heads * tokens];
                let mut out = vec![0.0f32; query_heads * head_dim];
                let r = paged_gqa_attention_with(
                    &pool,
                    &s,
                    shape,
                    &q,
                    &mut scores,
                    &mut out,
                    mode,
                    Kernel::Scalar,
                    visible,
                );
                log.push(format!(
                    "{r:?} {:?}",
                    out.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
                ));
            }
        }
        let mut out = vec![0.0f32; query_heads * head_dim];
        log.push(format!(
            "{:?}",
            paged_gqa_attention(
                &pool,
                &s,
                shape,
                &q,
                &mut [],
                &mut out,
                AttentionMode::TwoPass
            )
        ));
        pool.release(s).unwrap();
        log.join("\n")
    }};
}

#[test]
fn both_builds_attend_the_same() {
    let mut joined = String::new();
    for seed in 0..96u64 {
        let rust = attention_script!(seed, seed);
        let port = attention_script!(vesper_tensor, seed);
        assert_eq!(rust, port, "attention script {seed}");
        joined.push_str(&rust);
    }
    for outcome in [
        "Ok(())",
        "Err(NonFinite)",
        "Err(Shape(\"attention beyond the stored tokens\"))",
    ] {
        assert!(joined.contains(outcome), "no script reached {outcome}");
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_attention() {
    macro_rules! run {
        ($krate:ident, $mode:ident) => {{
            use $krate::attention::*;
            use $krate::kv::*;
            use $krate::simd::Kernel;
            let mut pool = KvPool::with_format(
                KvConfig {
                    heads: 4,
                    head_dim: 64,
                    pages: 32,
                    max_sequences: 1,
                    max_tokens_per_sequence: 512,
                    max_bytes: 16 << 20,
                },
                KvFormat::F32,
            )
            .unwrap();
            let mut s = pool.begin().unwrap();
            for p in 0..512 {
                let k = values(p as u64, 256);
                pool.append_f32(&mut s, p, &k, &k).unwrap();
            }
            let q = values(99, 16 * 64);
            let shape = AttentionShape {
                query_heads: 16,
                kv_heads: 4,
                head_dim: 64,
            };
            let mut scores = vec![0.0f32; 16 * 512];
            let mut out = vec![0.0f32; 16 * 64];
            move || {
                paged_gqa_attention_with(
                    &pool,
                    &s,
                    shape,
                    &q,
                    &mut scores,
                    &mut out,
                    AttentionMode::$mode,
                    Kernel::Scalar,
                    None,
                )
                .unwrap();
                checksum(&out)
            }
        }};
    }
    abba(
        "attention, two-pass, 16 query / 4 K/V heads x 64, 512 tokens",
        run!(seed, TwoPass),
        run!(vesper_tensor, TwoPass),
    );
    abba(
        "attention, online, 16 query / 4 K/V heads x 64, 512 tokens",
        run!(seed, Online),
        run!(vesper_tensor, Online),
    );
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_llama2c_load() {
    const MODEL: &[u8] = include_bytes!("../models/stories260K.bin");
    abba(
        "llama2.c load, stories260K and tok512",
        || {
            let m = seed::llama::Model::from_llama2c(MODEL).unwrap();
            let t = seed::llama::Tokenizer::from_llama2c(TOK512, 512).unwrap();
            m.rms_final()[0].to_bits() as u64 + t.vocab() as u64
        },
        || {
            let m = vesper_tensor::llama::Model::from_llama2c(MODEL).unwrap();
            let t = vesper_tensor::llama::Tokenizer::from_llama2c(TOK512, 512).unwrap();
            m.rms_final()[0].to_bits() as u64 + t.vocab() as u64
        },
    );
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_llama2c_decode() {
    let rust = seed::llama::Tokenizer::from_llama2c(TOK512, 512).unwrap();
    let port = vesper_tensor::llama::Tokenizer::from_llama2c(TOK512, 512).unwrap();
    // Every (previous, token) pair class: BOS before a piece, plain pieces
    // and the byte-fallback pieces.
    let pairs: Vec<(u32, u32)> = (0..4096u32)
        .map(|i| ((i * 7) % 512, (i * 13) % 512))
        .collect();
    abba(
        "llama2.c decode, 4,096 tokens of tok512",
        || {
            pairs
                .iter()
                .map(|&(p, t)| rust.decode(p, t).unwrap().len() as u64)
                .sum()
        },
        || {
            pairs
                .iter()
                .map(|&(p, t)| port.decode(p, t).unwrap().len() as u64)
                .sum()
        },
    );
}

macro_rules! open_weights {
    ($krate:ident, $path:expr) => {
        match $krate::mmap::Mapping::open($path) {
            Ok(m) => format!("{} {:?}", m.is_mapped(), m.bytes()),
            Err(e) => format!("{e:?}"),
        }
    };
}

#[test]
fn both_builds_open_the_same_weight_files() {
    let dir = std::env::temp_dir().join(format!("vesper-tensor-abba-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let empty = dir.join("empty.bin");
    let small = dir.join("small.bin");
    std::fs::write(&empty, b"").unwrap();
    std::fs::write(&small, b"weights").unwrap();
    let mut outcomes = Vec::new();
    for path in [empty, small, dir.join("missing.bin"), dir.clone()] {
        let rust = open_weights!(seed, &path);
        assert_eq!(
            rust,
            open_weights!(vesper_tensor, &path),
            "{}",
            path.display()
        );
        outcomes.push(rust);
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
        &outcomes[..2],
        ["false []", "true [119, 101, 105, 103, 104, 116, 115]"]
    );
}
