//! ABBA benchmarks of Beacon's Vesper port against its Rust build, with
//! `vesper-bench`. Both builds are in one process: `vesper_beacon` is the
//! port (this package under `vesper-port`) and `vesper_beacon_rust` is the
//! frozen Rust seed package. Each workload is a hot path of the crate:
//! histogram records, exponential-histogram records, traceparent parse and
//! format, and the span start/finish/drain cycle. The benchmarks are
//! ignored by default; run them with
//!
//! ```text
//! cargo test --release -p vesper-beacon --features vesper-port \
//!     --test abba -- --ignored --nocapture --test-threads=1
//! ```
#![cfg(feature = "vesper-port")]

use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

const OPS: u64 = 4096;

fn values() -> Vec<u64> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    (0..OPS)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            // Latency-like spread: mostly small, some up to u64::MAX.
            x >> (x % 64)
        })
        .collect()
}

macro_rules! workloads {
    ($module:ident, $krate:ident) => {
        pub mod $module {
            use super::*;
            use ::$krate::*;
            use core::num::NonZeroU64;

            pub fn histogram(values: &[u64]) -> u128 {
                let mut h = Box::<FastLogHistogram>::default();
                let id = NonZeroU64::new(7).unwrap();
                for v in values {
                    h.record_with_exemplar(*v, id).unwrap();
                }
                black_box(h.sum() ^ u128::from(h.count(100).unwrap()))
            }

            pub fn exponential(values: &[u64]) -> u128 {
                let mut h = ExponentialHistogram::default();
                for v in values {
                    h.record(*v).unwrap();
                }
                black_box(h.sum() ^ h.positive_counts().len() as u128)
            }

            pub fn traceparent(values: &[u64]) -> u64 {
                let mut sum = 0u64;
                for v in values {
                    let ctx =
                        W3cSpanContext::new(u128::from(*v) << 64 | 1, *v | 1, *v as u8).unwrap();
                    let raw = ctx.to_traceparent();
                    let back = parse_traceparent_55(&raw).unwrap();
                    sum = sum.wrapping_add(back.span_id());
                }
                black_box(sum)
            }

            pub fn spans(values: &[u64]) -> u64 {
                let pool = SpanPool::<64>::new(99, NonZeroU64::new(1).unwrap(), 1).unwrap();
                let mut ring = TelemetryRing::<64>::default();
                let mut sum = 0u64;
                for chunk in values.chunks(64) {
                    let root = pool.start_span(None, SpanSchema::Request, 0, 0).unwrap();
                    let parent = root.context();
                    let mut open = Vec::with_capacity(64);
                    open.push(root);
                    for v in &chunk[1..] {
                        open.push(
                            pool.start_span(Some(parent), SpanSchema::StorageAppend, *v, 1)
                                .unwrap(),
                        );
                    }
                    for (i, span) in open.into_iter().enumerate() {
                        span.finish_ok(&mut ring, 2 + i as u64, PrimaryAttribute::ItemCount(1))
                            .unwrap();
                    }
                    sum = sum.wrapping_add(ring.records().map(|r| r.duration_ns).sum::<u64>());
                    let batch = ring.drain::<64>();
                    sum = sum.wrapping_add(batch.span_ids().iter().sum::<u64>());
                }
                black_box(sum)
            }
        }
    };
}
workloads!(rust_build, vesper_beacon_rust);
workloads!(port_build, vesper_beacon);

/// One build's run of a workload over the shared input.
type Workload = fn(&[u64]) -> u128;

struct Case(Workload);

impl BenchCase for Case {
    type Input = Vec<u64>;
    type Output = u128;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<Self::Input, ()> {
        Ok(values())
    }
    fn run(&mut self, input: Self::Input) -> Result<Self::Output, ()> {
        Ok((self.0)(&input))
    }
}

fn workload(name: &str) -> (Workload, Workload) {
    match name {
        "histogram" => (rust_build::histogram, port_build::histogram),
        "exponential" => (rust_build::exponential, port_build::exponential),
        "traceparent" => (
            |v| u128::from(rust_build::traceparent(v)),
            |v| u128::from(port_build::traceparent(v)),
        ),
        "spans" => (
            |v| u128::from(rust_build::spans(v)),
            |v| u128::from(port_build::spans(v)),
        ),
        _ => unreachable!(),
    }
}

const WORKLOADS: [&str; 4] = ["histogram", "exponential", "traceparent", "spans"];

/// The distribution-free 95% interval of the median of `sorted`: the order
/// statistics `x(k)` and `x(n + 1 - k)` (1-based) for the largest `k` with
/// `P(Binomial(n, 1/2) <= k - 1) <= 0.025` (for 30 epochs, `x(10)..x(21)`).
fn median_interval(sorted: &[f64]) -> (f64, f64) {
    let n = sorted.len();
    let mut cdf = 0.0;
    let mut choose = 1.0;
    let mut k = 0;
    for j in 0..n {
        // P(X = j), then P(X <= j)
        cdf += choose * 0.5f64.powi(n as i32);
        if cdf > 0.025 {
            break;
        }
        k = j + 1;
        choose = choose * (n - j) as f64 / (j + 1) as f64;
    }
    // `k` is now that largest 1-based rank; with too few samples for a 95%
    // interval it is 0 and the interval is the whole range.
    let i = k.saturating_sub(1).min(n / 2);
    (sorted[i], sorted[n - 1 - i])
}

#[test]
fn both_builds_compute_the_same_workloads() {
    assert_eq!(vesper_beacon::IMPLEMENTATION, "vesper");
    assert_eq!(vesper_beacon_rust::IMPLEMENTATION, "rust");
    let v = values();
    for name in WORKLOADS {
        let (rust, port) = workload(name);
        assert_eq!(rust(&v), port(&v), "{name}");
    }
}

fn abba(name: &str) {
    let (rust, port) = workload(name);
    let run = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 16,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let report = run_abba(run, &mut Case(rust), &mut Case(port)).expect("ABBA run");
    let pairs = report.pairs();
    let mut ratios: Vec<f64> = pairs
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let per_call = |total: u64, p: &vesper_bench::PairedSample| {
        total as f64 / p.operations_per_arm() as f64 / OPS as f64
    };
    let mut rust_ns: Vec<f64> = pairs
        .iter()
        .map(|p| per_call(p.baseline_total(), p))
        .collect();
    let mut port_ns: Vec<f64> = pairs
        .iter()
        .map(|p| per_call(p.candidate_total(), p))
        .collect();
    rust_ns.sort_by(f64::total_cmp);
    port_ns.sort_by(f64::total_cmp);
    let mid = pairs.len() / 2;
    println!(
        "ABBA {name}: {} epochs x {} ops per arm ({OPS} calls per op); median per call: \
         rust {:.1} ns, vesper {:.1} ns; Vesper/Rust median {:.2} (min {:.2}, max {:.2})",
        pairs.len(),
        pairs[0].operations_per_arm(),
        rust_ns[mid],
        port_ns[mid],
        ratios[mid],
        ratios[0],
        ratios[ratios.len() - 1]
    );
    let (lo, hi) = median_interval(&ratios);
    println!(
        "ABBA {name}: Vesper/Rust 95% interval of the median {lo:.2}..{hi:.2}; flip bar (upper end <= 1.50): {}",
        if hi <= 1.5 { "pass" } else { "fail" }
    );
    let analysis = analyze_pairs(pairs, 10_000).expect("analysis");
    println!(
        "ABBA {name}: 2x gate (max regression 10000 bp): {:?}",
        analysis.decision
    );
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_histogram() {
    abba("histogram");
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_exponential() {
    abba("exponential");
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_traceparent() {
    abba("traceparent");
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_spans() {
    abba("spans");
}
