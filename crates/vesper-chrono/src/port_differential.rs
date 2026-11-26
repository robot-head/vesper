//! The Vesper port against the Rust code it backs (kept for the seed copy):
//! differentials on generated inputs, and ABBA timings that print the flip
//! gate's `flip-gate pairs` line (crates/vesper-chrono/flip-gate.toml).
use std::hint::black_box;
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

use crate::formatter::{self, FRAME_LEN, FSYNC, FormatterLimits, FormatterMode};

/// xorshift64*, so every run sees the same inputs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A source ID for an ID-change byte: mostly trace sources, sometimes the
/// null, flush and trigger IDs, rarely a reserved one.
fn source_id(r: &mut Rng) -> u8 {
    match r.below(32) {
        0 => 0,
        1 => 0x7B,
        2 => 0x7D,
        3 => 0x70 + r.below(16) as u8,
        _ => 1 + r.below(0x6F) as u8,
    }
}

/// One formatter frame: flag-carrying bytes are ID changes a quarter of the
/// time, and the auxiliary byte is random (so a change at byte 14 with its
/// bit set, BadAuxBit, happens too).
fn frame(r: &mut Rng, out: &mut Vec<u8>) {
    for k in 0..15 {
        let b = if k % 2 == 0 && r.below(4) == 0 {
            (source_id(r) << 1) | 1
        } else if k % 2 == 0 {
            (r.next() as u8) & 0xFE
        } else {
            r.next() as u8
        };
        out.push(b);
    }
    let aux = r.next() as u8;
    out.push(if r.below(8) == 0 { aux } else { aux & 0x7F });
}

/// A capture of `frames` frames, with full-frame syncs between some of them
/// in TPIU mode, and sometimes a truncated or extended tail.
fn capture(r: &mut Rng, mode: FormatterMode, frames: usize) -> Vec<u8> {
    let mut out = Vec::new();
    if mode == FormatterMode::FrameSync {
        for _ in 0..r.below(3) {
            out.push(r.next() as u8);
        }
        if r.below(32) != 0 {
            out.extend_from_slice(&FSYNC);
        }
    }
    for _ in 0..frames {
        if mode == FormatterMode::FrameSync && r.below(6) == 0 {
            out.extend_from_slice(&FSYNC);
        }
        frame(r, &mut out);
    }
    match r.below(8) {
        0 => {
            let cut = r.below(FRAME_LEN as u64) as usize;
            out.truncate(out.len().saturating_sub(cut));
        }
        1 => out.extend_from_slice(&FSYNC[..1 + r.below(4) as usize]),
        _ => {}
    }
    out
}

#[test]
fn deformat_matches_the_rust_deformatter() {
    let mut r = Rng(0xC0DE_5167);
    let mut ok = 0;
    for case in 0..20_000 {
        let mode = if case % 2 == 0 {
            FormatterMode::Frames
        } else {
            FormatterMode::FrameSync
        };
        let frames = r.below(12) as usize;
        let mut bytes = capture(&mut r, mode, frames);
        if r.below(4) == 0 && !bytes.is_empty() {
            let at = r.below(bytes.len() as u64) as usize;
            bytes[at] ^= 1 << r.below(8);
        }
        let limits = FormatterLimits {
            max_output: if r.below(4) == 0 {
                r.below(64) as usize
            } else {
                FormatterLimits::default().max_output
            },
        };
        let port = formatter::deformat(&bytes, mode, &limits);
        let rust = formatter::deformat_reference(&bytes, mode, &limits);
        assert_eq!(port, rust, "case {case}: {bytes:02x?} {mode:?} {limits:?}");
        ok += usize::from(rust.is_ok());
    }
    // Both outcomes are well covered.
    assert!((4_000..16_000).contains(&ok), "{ok} of 20000 decoded");
}

struct Arm<F: FnMut() -> u64>(F);

impl<F: FnMut() -> u64> BenchCase for Arm<F> {
    type Input = ();
    type Output = u64;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<(), ()> {
        Ok(())
    }
    fn run(&mut self, (): ()) -> Result<u64, ()> {
        Ok((self.0)())
    }
}

fn report(name: &str, run: RunConfig, rust: impl FnMut() -> u64, port: impl FnMut() -> u64) {
    let report = run_abba(run, &mut Arm(rust), &mut Arm(port)).expect("ABBA run");
    let pairs = report.pairs();
    let line: Vec<String> = pairs
        .iter()
        .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
        .collect();
    println!("flip-gate pairs {name} {}", line.join(" "));
    let analysis = analyze_pairs(pairs, 5_000).expect("analysis");
    if let Some(i) = analysis.interval {
        println!(
            "{name}: 95% median interval of Vesper/Rust [{:.3}, {:.3}], {:?}",
            i.lower.candidate_total() as f64 / i.lower.baseline_total() as f64,
            i.upper.candidate_total() as f64 / i.upper.baseline_total() as f64,
            analysis.decision
        );
    }
}

/// A 64 KiB ETR buffer of two interleaved sources with some padding: an ID
/// change on one flag-carrying byte in 32 (a source switch every four
/// frames or so).
fn etr_buffer() -> Vec<u8> {
    let mut r = Rng(0xE7B);
    let mut out = Vec::new();
    while out.len() < 64 * 1024 {
        for k in 0..15 {
            let b = if k % 2 == 0 && r.below(32) == 0 {
                (([0x10u8, 0x11, 0x00][r.below(3) as usize]) << 1) | 1
            } else if k % 2 == 0 {
                (r.next() as u8) & 0xFE
            } else {
                r.next() as u8
            };
            out.push(b);
        }
        out.push((r.next() as u8) & 0x7F);
    }
    out
}

fn deformat_run(bytes: &[u8], port: bool) -> u64 {
    let limits = FormatterLimits::default();
    let d = if port {
        formatter::deformat(bytes, FormatterMode::Frames, &limits)
    } else {
        formatter::deformat_reference(bytes, FormatterMode::Frames, &limits)
    }
    .unwrap();
    d.frames as u64 + d.stream(0x10).len() as u64
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_formatter() {
    let bytes = etr_buffer();
    assert_eq!(deformat_run(&bytes, false), deformat_run(&bytes, true));
    let run = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 8,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    report(
        "formatter",
        run,
        || deformat_run(black_box(&bytes), false),
        || deformat_run(black_box(&bytes), true),
    );
}
