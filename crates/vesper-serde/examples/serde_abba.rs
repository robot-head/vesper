//! ABBA benchmark of the Rust build (`vesper-serde-seed`) against the port
//! build (`vesper-serde`) of the crate's hot paths (port wave P1 lane 19,
//! #916): the scalar parse and the two-stage SIMD parse of a JSON document,
//! f64 decode of JSON numbers, f64 encode, and telemetry tile ingestion
//! (the workloads of `examples/simd_throughput.rs`).
//!
//! One binary holds one build, so the arms are two binaries, both built from
//! this file (`serde_abba_rust` is the seed package's copy):
//!
//! ```sh
//! cargo build --release -p vesper-serde-seed --example serde_abba_rust
//! cargo build --release -p vesper-serde --example serde_abba
//! target/release/examples/serde_abba driver \
//!     target/release/examples/serde_abba_rust target/release/examples/serde_abba
//! ```
//!
//! `window W N` prints the nanoseconds of N repetitions of workload W after
//! one warm-up repetition. `driver A B` runs, per workload, epochs of windows
//! in the order A B B A, pairs each epoch's A and B totals and reports the
//! median regression of B over A with `vesper_bench::analyze_pairs` against
//! the port plan's 2x stop rule (B at most +10,000 bp over A).
use std::hint::black_box;
use std::process::Command;
use std::time::Instant;
use vesper_serde::tiles::{TelemetryIngestor, TelemetryTiles};
use vesper_serde::{EncodeLimits, Value, encode_into};
use vesper_serde::{FloatLimits, IndexedJson, Limits, SimdParser};

const EPOCHS: usize = 9;
const WORKLOADS: [(&str, usize); 5] = [
    ("parse_scalar", 20),
    ("parse_simd", 20),
    ("f64_decode", 20),
    ("f64_encode", 20),
    ("tiles", 20),
];

fn document(items: usize) -> Vec<u8> {
    let mut s = String::from("[");
    for i in 0..items {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"id\":{i},\"name\":\"user \\\"{i}\\\" ünïcødé\",\"score\":{}.{},\"active\":{},\"text\":\"{}\",\"nested\":{{\"a\":[1,2,3],\"b\":null}}}}",
            i * 3,
            i % 1000,
            i % 2 == 0,
            "lorem ipsum dolor sit amet ".repeat(1 + i % 4)
        ));
    }
    s.push(']');
    s.into_bytes()
}

/// A JSON array of varied binary64 values in shortest form.
fn numbers(count: usize) -> Vec<u8> {
    let mut s = String::from("[");
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    for i in 0..count {
        if i > 0 {
            s.push(',');
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let v = match i % 4 {
            0 => (x % 1_000_000) as f64 / 100.0,
            1 => f64::from_bits(x >> 2 | 0x3000_0000_0000_0000),
            2 => (x % 10_000) as f64,
            _ => -((x % 1_000_000_007) as f64) * 1e-7,
        };
        s.push_str(&v.to_string());
    }
    s.push(']');
    s.into_bytes()
}

fn telemetry(lines: usize) -> Vec<u8> {
    (0..lines)
        .map(|i| {
            format!(
                "{{\"ts_unix_ms\":{},\"latency_us\":{},\"status_code\":{},\"flags\":{},\"service\":\"api-{}\",\"tags\":[\"a\",\"b\\u00e9\"],\"trace_id_hex\":\"{:032x}\"}}\n",
                1_700_000_000_000u64 + i as u64,
                (i * 37) % 1_000_000,
                [200, 201, 404, 500][i % 4],
                i % 16,
                i % 50,
                (i as u128).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            )
        })
        .collect::<String>()
        .into_bytes()
}

fn limits() -> Limits {
    Limits {
        max_input_bytes: 64 << 20,
        max_tokens: 1 << 22,
        max_work: usize::MAX / 4,
        max_index_bytes: 512 << 20,
        max_string_bytes: 64 << 20,
        max_total_string_bytes: 64 << 20,
        ..Limits::default()
    }
}

/// One repetition of a workload, timed by `window`.
fn workload(name: &str) -> Box<dyn FnMut() -> usize> {
    match name {
        "parse_scalar" => {
            let doc = document(2_000);
            Box::new(move || IndexedJson::parse(&doc, limits()).unwrap().token_count())
        }
        "parse_simd" => {
            let doc = document(2_000);
            let mut parser = SimdParser::new();
            Box::new(move || parser.parse(&doc, limits(), |d| d.token_count()).unwrap())
        }
        "f64_decode" => {
            // The document outlives the closure: leak it and its input.
            let text: &'static [u8] = numbers(20_000).leak();
            let json: &'static IndexedJson<'static> =
                Box::leak(Box::new(IndexedJson::parse(text, limits()).unwrap()));
            Box::new(move || {
                let mut n = 0;
                let mut items = json.root().elements().unwrap();
                while let Some(v) = items.peek() {
                    n += v.as_f64(FloatLimits::default()).unwrap().to_bits() as usize & 1;
                    items.skip().unwrap();
                }
                n
            })
        }
        "f64_encode" => {
            let values: Vec<Value<'static>> = (0..4_000)
                .map(|i| {
                    Value::F64(f64::from_bits(
                        0x3ff0_0000_0000_0000 ^ (i as u64 * 0x9e37_79b9),
                    ))
                })
                .collect();
            let values: &'static [Value<'static>] = values.leak();
            let mut out = vec![0u8; 1 << 20];
            let lim = EncodeLimits {
                max_output_bytes: 1 << 20,
                max_tokens: 1 << 20,
                max_work: usize::MAX / 4,
                ..EncodeLimits::default()
            };
            Box::new(move || encode_into(&Value::Array(values), &mut out, lim).unwrap())
        }
        "tiles" => {
            let ndjson = telemetry(5_000);
            let mut ingestor = TelemetryIngestor::new(limits());
            let mut tiles = TelemetryTiles::with_capacity(5_000).unwrap();
            Box::new(move || {
                tiles.clear();
                ingestor.ingest(&ndjson, &mut tiles).unwrap()
            })
        }
        _ => panic!("unknown workload {name}"),
    }
}

fn window(name: &str, reps: usize) -> u128 {
    let mut f = workload(name);
    black_box(f());
    let start = Instant::now();
    for _ in 0..reps {
        black_box(f());
    }
    start.elapsed().as_nanos()
}

fn run(bin: &str, name: &str, reps: usize) -> u64 {
    let out = Command::new(bin)
        .args(["window", name, &reps.to_string()])
        .output()
        .expect("run arm");
    assert!(out.status.success(), "{bin} failed");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("implementation") => println!("{}", vesper_serde::IMPLEMENTATION),
        Some("window") => println!("{}", window(&args[2], args[3].parse().unwrap())),
        Some("driver") => {
            let (a, b) = (&args[2], &args[3]);
            for (name, reps) in WORKLOADS {
                let mut pairs = Vec::new();
                let mut ratios = Vec::new();
                for _ in 0..EPOCHS {
                    let a1 = run(a, name, reps);
                    let b1 = run(b, name, reps);
                    let b2 = run(b, name, reps);
                    let a2 = run(a, name, reps);
                    ratios.push((b1 + b2) as f64 / (a1 + a2) as f64);
                    pairs.push(
                        vesper_bench::PairedSample::new(a1 + a2, b1 + b2, 2 * reps as u64).unwrap(),
                    );
                }
                ratios.sort_by(f64::total_cmp);
                let analysis = vesper_bench::analyze_pairs(&pairs, 10_000).unwrap();
                println!(
                    "{name}: median B/A {:.2} (epochs {:.2}..{:.2}); 2x gate {:?}, median regression bp {:?}",
                    ratios[EPOCHS / 2],
                    ratios[0],
                    ratios[EPOCHS - 1],
                    analysis.decision,
                    analysis.median_upper.regression_bp_bounds()
                );
            }
        }
        _ => {
            eprintln!("usage: serde_abba implementation | window W N | driver RUST_BIN PORT_BIN");
            std::process::exit(2);
        }
    }
}
