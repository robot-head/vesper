//! The flip gate's in-harness ABBA (#2207): with `FLIP_GATE` set, a speed
//! test times its Rust and Vesper sides here and prints
//! `flip-gate pairs <workload> <rust_ns>:<vesper_ns> ...`, the line
//! `aspect ci flip-gate` reads (docs/maintenance/rust-exit-plan.md,
//! "Flipping a module").
use std::time::Duration;

/// ABBA rounds when `FLIP_GATE_EPOCHS` is unset.
pub const DEFAULT_EPOCHS: usize = 12;

/// Whether the gate asked for timing (`FLIP_GATE` set).
pub fn on() -> bool {
    std::env::var_os("FLIP_GATE").is_some()
}

/// `FLIP_GATE_EPOCHS`, or [`DEFAULT_EPOCHS`].
pub fn epochs() -> usize {
    std::env::var("FLIP_GATE_EPOCHS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_EPOCHS)
}

/// One warm-up of each side, then [`epochs`] rounds of rust, vesper,
/// vesper, rust, printed as one `flip-gate pairs` line. Returns the
/// rounds (each the sum of its two Rust and two Vesper times).
pub fn pairs(
    workload: &str,
    rust: &mut dyn FnMut() -> Duration,
    vesper: &mut dyn FnMut() -> Duration,
) -> Vec<(Duration, Duration)> {
    rust();
    vesper();
    let mut rounds = vec![];
    for _ in 0..epochs() {
        let (r1, v1, v2, r2) = (rust(), vesper(), vesper(), rust());
        rounds.push((r1 + r2, v1 + v2));
    }
    let line: String = rounds
        .iter()
        .map(|(r, v)| format!(" {}:{}", r.as_nanos().max(1), v.as_nanos().max(1)))
        .collect();
    // On a line of its own: with one test thread libtest has already
    // printed `test <name> ... ` without a newline, and the gate reads only
    // lines that start with `flip-gate pairs`.
    println!("\nflip-gate pairs {workload}{line}");
    rounds
}
