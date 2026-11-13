//! Measure stage 1 and full-parse throughput on each available path.
//!
//! `cargo run --release -p vesper-serde --example simd_throughput`
//! Reports the best of several timed repetitions; numbers are machine- and
//! load-dependent and are only meaningful together with the CPU they ran on.
use std::hint::black_box;
use std::time::{Duration, Instant};
use vesper_serde::simd::{IndexLimits, Isa, StructuralIndex};
use vesper_serde::tiles::{TelemetryIngestor, TelemetryTiles};
use vesper_serde::{IndexedJson, Limits, SimdParser};

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

fn mb_per_s(bytes: usize, f: &mut dyn FnMut()) -> f64 {
    f();
    let mut best = Duration::MAX;
    for _ in 0..7 {
        let start = Instant::now();
        let mut reps = 0;
        while start.elapsed() < Duration::from_millis(150) {
            f();
            reps += 1;
        }
        best = best.min(start.elapsed() / reps);
    }
    bytes as f64 / best.as_secs_f64() / 1e6
}

fn main() {
    let doc = document(4000);
    let ndjson = telemetry(20_000);
    let big = Limits {
        max_input_bytes: 64 << 20,
        max_tokens: 1 << 24,
        max_string_bytes: 1 << 20,
        max_total_string_bytes: 64 << 20,
        max_work: 1 << 30,
        max_index_bytes: 1 << 30,
        ..Limits::default()
    };
    println!(
        "document {} bytes, NDJSON {} bytes in {} lines; detected {:?}",
        doc.len(),
        ndjson.len(),
        20_000,
        Isa::detect()
    );
    let index_limits = IndexLimits {
        max_input_bytes: 64 << 20,
        max_structurals: 64 << 20,
    };
    println!(
        "{:<8} {:>12} {:>12} {:>12}",
        "path", "stage1 MB/s", "parse MB/s", "tiles MB/s"
    );
    for isa in Isa::available() {
        let mut index = StructuralIndex::new();
        let stage1 = mb_per_s(doc.len(), &mut || {
            index.build(black_box(&doc), isa, index_limits).unwrap();
        });
        let mut parser = SimdParser::with_isa(isa).unwrap();
        let parse = mb_per_s(doc.len(), &mut || {
            parser
                .parse(black_box(&doc), big, |d| black_box(d.token_count()))
                .unwrap();
        });
        let mut ingestor = TelemetryIngestor::with_isa(isa, Limits::default()).unwrap();
        let mut tiles = TelemetryTiles::with_capacity(20_000).unwrap();
        let tile = mb_per_s(ndjson.len(), &mut || {
            tiles.clear();
            ingestor.ingest(black_box(&ndjson), &mut tiles).unwrap();
        });
        println!(
            "{:<8} {stage1:>12.0} {parse:>12.0} {tile:>12.0}",
            format!("{isa:?}")
        );
    }
    let scalar = mb_per_s(doc.len(), &mut || {
        black_box(
            IndexedJson::parse(black_box(&doc), big)
                .unwrap()
                .token_count(),
        );
    });
    let serde = mb_per_s(doc.len(), &mut || {
        black_box(serde_json::from_slice::<serde_json::Value>(black_box(&doc)).unwrap());
    });
    println!("recursive-descent IndexedJson::parse: {scalar:.0} MB/s");
    println!("serde_json::from_slice::<Value> (builds a DOM, for scale): {serde:.0} MB/s");
}
