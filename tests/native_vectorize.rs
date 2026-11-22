//! Two-lane vectorization of counted element loops (#1474).
//!
//! Each program runs in the evaluator (optimized and not), the JIT
//! (optimized and not), and builds as x86-64 and AArch64 objects; every
//! backend must agree with the evaluator. The optimized CLIF must hold the
//! `i64x2` prologue where the loop qualifies.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-vectorize-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The optimized CLIF of `source` (`vesper emit --emit clif`).
fn clif(name: &str, source: &str) -> String {
    let temp = Temp::new();
    let file = temp.0.join(format!("{name}.ves"));
    fs::write(&file, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(["emit", file.to_str().unwrap(), "--emit", "clif"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Runs `main` everywhere, checks that every backend agrees with the
/// evaluator, and returns the optimized CLIF.
fn differential(name: &str, source: &str) -> String {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{name}: {:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{name}: {:?}", c.diagnostics);
    let expected = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = optimize;
        let value = runtime.run("main", vec![]).unwrap().to_string();
        assert_eq!(value, expected, "{name}: evaluator (optimize {optimize})");
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(jit, expected, "{name}: JIT (optimize {optimize})");
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        native::object_for_target(&c.world, "main", Some(target))
            .unwrap_or_else(|e| panic!("{name}: {target} object failed: {e}"));
    }
    clif(name, source)
}

/// A `main` that fills a 300-element sequence with a pattern, runs
/// `kernels` (statements over `d`), and folds the sequence to one value.
fn program(helpers: &str, kernels: &str) -> String {
    format!(
        "{helpers}
fn main() -> u64 {{
    let mut d: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in 0..300 {{ d.push(wrapping_add(wrapping_mul(i, 2654435761), 7)); }}
    {kernels}
    let mut h: u64 = 0;
    for i in 0..300 {{
        let x = match d.try_get(i) {{ Option::Some(x) => x, Option::None => 0 }};
        h = wrapping_add(wrapping_mul(h, 31), x);
    }}
    h
}}"
    )
}

fn vectorized(clif: &str) -> bool {
    clif.contains("i64x2")
}

/// Bench's subset-sum step, a descending loop, for rank 0, 1 and large
/// ranks, where `dist[s + rank]` for `s = max - j` may be the element the
/// next iteration reads (rank 1) or the same one (rank 0).
#[test]
fn descending_add_rank_matches_every_backend() {
    let helpers = "fn add_rank(inout dist: ChunkSeq[u64], max_sum: u64, rank: u64) {
    for j in 0..=max_sum {
        let s = wrapping_sub(max_sum, j);
        let v = match dist.try_get(s) { Option::Some(x) => x, Option::None => 0 };
        let t = wrapping_add(s, rank);
        let w = match dist.try_get(t) { Option::Some(x) => x, Option::None => 0 };
        dist.try_set(t, wrapping_add(w, v));
    }
}";
    for (max, rank) in [
        (0, 0),
        (1, 0),
        (2, 1),
        (3, 1),
        (5, 2),
        (40, 0),
        (40, 1),
        (100, 7),
        (200, 99),
        (150, 150),
    ] {
        let kernels = format!("add_rank(inout d, {max}, {rank});");
        let clif = differential(&format!("rank_{max}_{rank}"), &program(helpers, &kernels));
        assert!(
            vectorized(&clif),
            "add_rank({max}, {rank}) is not vectorized:\n{clif}"
        );
    }
    // Many ranks in a row, as Bench runs them.
    let kernels = "let mut m: u64 = 0;
    for k in 0..20 { add_rank(inout d, m, k); m = wrapping_add(m, k); }";
    differential("ranks", &program(helpers, kernels));
}

/// Ascending loops, exclusive and inclusive, with every trip count from 0
/// to 3 and a long one, and an invariant operand.
#[test]
fn ascending_loops_and_short_trip_counts() {
    let helpers = "fn mix(inout d: ChunkSeq[u64], lo: u64, n: u64, k: u64) {
    for j in lo..n {
        let a = match d.try_get(j) { Option::Some(x) => x, Option::None => 0 };
        let b = match d.try_get(wrapping_add(j, 50)) { Option::Some(x) => x, Option::None => 0 };
        d.try_set(j, wrapping_sub(wrapping_add(a, b) ^ k, b & k));
    }
}
fn mix_to(inout d: ChunkSeq[u64], lo: u64, n: u64, k: u64) {
    for j in lo..=n {
        let a = match d.try_get(j) { Option::Some(x) => x, Option::None => 0 };
        d.try_set(j, a | k);
    }
}";
    for n in [0, 1, 2, 3, 4, 5, 249] {
        let kernels =
            format!("mix(inout d, 0, {n}, 12345); mix(inout d, 7, wrapping_add(7, {n}), 99);");
        let clif = differential(&format!("mix_{n}"), &program(helpers, &kernels));
        assert!(vectorized(&clif), "mix is not vectorized:\n{clif}");
        let kernels = format!("mix_to(inout d, 3, wrapping_add(2, {n}), 4096);");
        differential(&format!("mix_to_{n}"), &program(helpers, &kernels));
    }
}

/// A recurrence: iteration `j` stores the element iteration `j + 1` loads,
/// so every pair must fall back to the scalar loop.
#[test]
fn a_store_feeding_the_next_load_falls_back() {
    let helpers = "fn scan(inout d: ChunkSeq[u64], n: u64) {
    for j in 0..n {
        let a = match d.try_get(j) { Option::Some(x) => x, Option::None => 0 };
        let b = match d.try_get(wrapping_add(j, 1)) { Option::Some(x) => x, Option::None => 0 };
        d.try_set(wrapping_add(j, 1), wrapping_add(a, b));
    }
}
fn scan_down(inout d: ChunkSeq[u64], top: u64) {
    for j in 0..top {
        let s = wrapping_sub(top, j);
        let a = match d.try_get(s) { Option::Some(x) => x, Option::None => 0 };
        let b = match d.try_get(wrapping_sub(s, 1)) { Option::Some(x) => x, Option::None => 0 };
        d.try_set(wrapping_sub(s, 1), wrapping_add(a, b));
    }
}";
    for n in [0, 1, 2, 3, 100, 299] {
        let kernels = format!("scan(inout d, {n}); scan_down(inout d, {n});");
        differential(&format!("scan_{n}"), &program(helpers, &kernels));
    }
}

/// Two stores of one iteration where the later one writes what the next
/// iteration's earlier one does: the lanes would run both iterations' first
/// store before the second, so the prologue must fall back (Codex review on
/// #1535).
#[test]
fn stores_that_overlap_across_iterations_fall_back() {
    let helpers = "fn pairs(inout d: ChunkSeq[u64], n: u64, k: u64) {
    for j in 0..n {
        d.try_set(j, k);
        d.try_set(wrapping_add(j, 1), wrapping_add(k, 1));
    }
}
fn pairs_apart(inout d: ChunkSeq[u64], n: u64, k: u64) {
    for j in 0..n {
        d.try_set(j, k);
        d.try_set(wrapping_add(j, 100), wrapping_add(k, 1));
    }
}";
    for n in [0, 1, 2, 3, 4, 7, 150] {
        let kernels = format!("pairs(inout d, {n}, 5); pairs_apart(inout d, {n}, 9);");
        differential(&format!("pairs_{n}"), &program(helpers, &kernels));
    }
}

/// A range that ends at `u64::MAX`: the lane loop's test for two more
/// iterations must not wrap past it (Codex review on #1535).
#[test]
fn a_range_ending_at_u64_max_stops_in_bounds() {
    let helpers = "fn top(inout d: ChunkSeq[u64], lo: u64) {
    for j in lo..18446744073709551615 {
        let i = wrapping_sub(j, lo);
        let a = match d.try_get(i) { Option::Some(x) => x, Option::None => 0 };
        d.try_set(i, wrapping_add(a, 3));
    }
}";
    for n in [1, 2, 3, 8, 9, 200] {
        let kernels = format!("top(inout d, wrapping_sub(18446744073709551615, {n}));");
        differential(&format!("top_{n}"), &program(helpers, &kernels));
    }
}
