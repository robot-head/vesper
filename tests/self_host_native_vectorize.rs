//! `@vesper/native-vectorize` (packages/vesper-native-vectorize) against
//! `native_vectorize::run` (#1829).
//!
//! Every function the native optimize pipeline hands the vectorizer, over
//! the corpus `main`s and the kernels of tests/native_vectorize.rs, is
//! captured before the pass (`native_vectorize::capture`). Rust runs the
//! pass on one copy; the Vesper package reads another in the words of
//! crates/vesper-cranelift-sys/src/clif_words.rs
//! and answers with the loop count and an edit
//! script, which `clif_words::apply` replays. The counts and the printed
//! functions must be equal.
//!
//! - `NATIVE_VECTORIZE_STRIDE=n` keeps every n-th corpus world (default 1).
//! - `CHECK_HARNESS_SPEED=1` runs the ABBA timing (release build).
#[path = "common/check_harness.rs"]
mod check_harness;
// The Cranelift shim's function words and edit replay (#2606), included
// by path: the root crate cannot depend on vesper-cranelift-sys (a Bazel
// dev-dependency cycle).
#[path = "../crates/vesper-cranelift-sys/src/clif_words.rs"]
mod clif_words;

use check_harness::*;
use cranelift_codegen::ir;
use std::sync::OnceLock;
use vesper::{check, native, native_vectorize, parser};

fn entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| {
        Package::load("packages/vesper-native-vectorize").entry("vectorize_words", true)
    })
}

/// The function after the Vesper pass, and its loop count.
fn through_vesper(f: &ir::Function, simd: bool) -> Result<(ir::Function, u64), String> {
    let words = entry().words(&clif_words::encode(f, simd));
    let (&count, script) = words.split_first().ok_or("no answer")?;
    let mut g = f.clone();
    clif_words::apply(&mut g, script)?;
    Ok((g, count))
}

/// How the two passes compare on `f`: `Ok(loops)` when they agree.
fn compare(f: &ir::Function, simd: bool) -> Result<usize, String> {
    let mut rust = f.clone();
    let n = if simd {
        native_vectorize::run_rust(&mut rust, simd)
    } else {
        native_vectorize::run(&mut rust, simd)
    };
    let (vesper, count) = through_vesper(f, simd)?;
    if count != n as u64 {
        return Err(format!("Rust vectorizes {n} loops, Vesper {count}"));
    }
    let (a, b) = (rust.display().to_string(), vesper.display().to_string());
    if a != b {
        let line = a
            .lines()
            .zip(b.lines())
            .enumerate()
            .find(|(_, (x, y))| x != y)
            .map(|(i, (x, y))| format!("line {i}:\n    rust:   {x}\n    vesper: {y}"))
            .unwrap_or_else(|| "lengths differ".into());
        return Err(line);
    }
    Ok(n)
}

/// The functions the vectorizer gets for `world`'s optimized `main`.
fn captured(world: &vesper::check::World) -> Vec<ir::Function> {
    let (_, funcs) =
        native_vectorize::capture(|| native::object_with_options(world, "main", None, true));
    funcs
}

fn checked(source: &str) -> vesper::check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c.world
}

/// A `main` over a 300-element sequence that runs `kernels`
/// (tests/native_vectorize.rs `program`).
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

/// The kernels of tests/native_vectorize.rs: loops that vectorize and
/// loops that must not.
fn kernel_programs() -> Vec<(&'static str, String)> {
    let add_rank = "fn add_rank(inout dist: ChunkSeq[u64], max_sum: u64, rank: u64) {
    for j in 0..=max_sum {
        let s = wrapping_sub(max_sum, j);
        let v = match dist.try_get(s) { Option::Some(x) => x, Option::None => 0 };
        let t = wrapping_add(s, rank);
        let w = match dist.try_get(t) { Option::Some(x) => x, Option::None => 0 };
        dist.try_set(t, wrapping_add(w, v));
    }
}";
    let mix = "fn mix(inout d: ChunkSeq[u64], lo: u64, n: u64, k: u64) {
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
    let scan = "fn scan(inout d: ChunkSeq[u64], n: u64) {
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
    let pairs = "fn pairs(inout d: ChunkSeq[u64], n: u64, k: u64) {
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
    let top = "fn top(inout d: ChunkSeq[u64], lo: u64) {
    for j in lo..18446744073709551615 {
        let i = wrapping_sub(j, lo);
        let a = match d.try_get(i) { Option::Some(x) => x, Option::None => 0 };
        d.try_set(i, wrapping_add(a, 3));
    }
}";
    let down = "fn cu(in s: ChunkSeq[u64], i: u64) -> u64 { match s.try_get(i) { Option::Some(x) => x, Option::None => 0 } }
fn inc(a: u64) -> u64 { wrapping_add(a, 1) }
fn dec(a: u64) -> u64 { wrapping_sub(a, 1) }
fn add(a: u64, b: u64) -> u64 { wrapping_add(a, b) }
fn add_rank(inout dist: ChunkSeq[u64], max_sum: u64, rank: u64) {
    let mut sum = inc(max_sum);
    loop {
        if sum == 0 {
            break;
        }
        sum = dec(sum);
        let v = cu(in dist, sum);
        let t = add(sum, rank);
        dist.try_set(t, add(cu(in dist, t), v));
    }
}
fn late(inout d: ChunkSeq[u64], n: u64) {
    let mut s = n;
    loop {
        if s == 0 {
            break;
        }
        let v = cu(in d, s);
        d.try_set(s, add(v, 1));
        s = dec(s);
    }
}
fn skips(inout d: ChunkSeq[u64], n: u64) {
    let mut s = n;
    loop {
        if s == 0 {
            break;
        }
        s = wrapping_sub(s, 2);
        let v = cu(in d, s);
        d.try_set(s, add(v, 1));
    }
}";
    let inlined = "fn add_rank(inout d: ChunkSeq[u64], n: u64) {
    for i in 0..n {
        let cur = match d.try_get(i) { Option::Some(v) => v, Option::None => 0 };
        d.try_set(i, wrapping_add(cur, 1));
    }
}";
    let widen = "fn dequant(in block: Slice[u8], inout out: Slice[u32], scale: f32) eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    for i in 0..32 {
        let q = wrapping_i8(block[2 + i]);
        out.set(i, to_bits_f32(widen_f32(q) * scale));
    }
}

fn main() -> u64 {
    let mut b = ChunkSeq::new[u8]();
    let mut out = ChunkSeq::new[u32]();
    for i in 0..34 {
        b.push(wrapping_u8(wrapping_add(i, 240)));
    }
    for _ in 0..32 {
        out.push(0);
    }
    handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            dequant(in b.as_slice(), inout out.as_slice_mut(), 0.25);
            let mut sum: u64 = 0;
            for i in 0..32 {
                let w = match out.try_get(i) { Option::Some(x) => x, Option::None => 0 };
                sum = wrapping_add(sum, widen_u64(w));
            }
            sum
        } with {
            return x => x,
            raise(_, _) => 998,
        }
    } with {
        return x => x,
        raise(_, _) => 997,
    }
}";
    vec![
        (
            "add_rank",
            program(
                add_rank,
                "let mut m: u64 = 0;\n    for k in 0..20 { add_rank(inout d, m, k); m = wrapping_add(m, k); }",
            ),
        ),
        (
            "mix",
            program(
                mix,
                "mix(inout d, 0, 249, 12345); mix(inout d, 7, 30, 99); mix_to(inout d, 3, 200, 4096);",
            ),
        ),
        (
            "scan",
            program(scan, "scan(inout d, 100); scan_down(inout d, 100);"),
        ),
        (
            "pairs",
            program(
                pairs,
                "pairs(inout d, 150, 5); pairs_apart(inout d, 150, 9);",
            ),
        ),
        (
            "top",
            program(
                top,
                "top(inout d, wrapping_sub(18446744073709551615, 200));",
            ),
        ),
        (
            "down",
            program(
                down,
                "add_rank(inout d, 150, 99); late(inout d, 100); skips(inout d, 100);",
            ),
        ),
        ("inlined", program(inlined, "add_rank(inout d, 200);")),
        ("widen", widen.to_string()),
    ]
}

fn stride() -> usize {
    std::env::var("NATIVE_VECTORIZE_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

/// Runs `compare` on every captured function of `worlds`; prints the first
/// few differences and returns (functions, loops, differences).
fn run_all<'a>(
    worlds: impl Iterator<Item = (String, &'a vesper::check::World)>,
) -> (usize, usize, usize) {
    let (mut funcs, mut loops, mut bad) = (0, 0, 0);
    for (name, world) in worlds {
        for (k, f) in captured(world).iter().enumerate() {
            funcs += 1;
            for simd in [true, false] {
                match compare(f, simd) {
                    Ok(n) => loops += n,
                    Err(e) => {
                        bad += 1;
                        if bad <= 6 {
                            eprintln!("native vectorize: {name} function {k} (simd {simd}): {e}");
                        }
                    }
                }
            }
        }
    }
    (funcs, loops, bad)
}

#[test]
fn kernels_match_native_vectorize_run() {
    let worlds: Vec<(String, vesper::check::World)> = kernel_programs()
        .into_iter()
        .map(|(name, source)| (name.to_string(), checked(&source)))
        .collect();
    let (funcs, loops, bad) = run_all(worlds.iter().map(|(n, w)| (n.clone(), w)));
    eprintln!(
        "native vectorize kernels: {bad} differ over {funcs} functions; Rust vectorizes {loops} loops"
    );
    assert!(loops >= 8, "the kernels vectorize only {loops} loops");
    assert_eq!(bad, 0);
}

#[test]
fn corpus_matches_native_vectorize_run() {
    let start = std::time::Instant::now();
    let cases = corpus();
    let (funcs, loops, bad) = run_all(
        cases
            .iter()
            .step_by(stride())
            .map(|c| (c.name.clone(), &c.world)),
    );
    eprintln!(
        "native vectorize corpus: {bad} differ over {funcs} functions of {} worlds; \
         Rust vectorizes {loops} loops; {:.1?}",
        cases.len().div_ceil(stride()),
        start.elapsed()
    );
    println!("flip-gate differential differences={bad} cases={funcs}");
    assert_eq!(bad, 0);
}

static VESPER_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The flip gate's hook: the Vesper pass while `VESPER_ON`, else Rust's.
fn switched(f: &mut ir::Function, simd: bool) -> usize {
    if !VESPER_ON.load(std::sync::atomic::Ordering::Relaxed) {
        return native_vectorize::run_rust(f, simd);
    }
    let (g, n) = through_vesper(f, simd).expect("the Vesper pass");
    *f = g;
    n as usize
}

/// ABBA: `native_vectorize::run` against the Vesper pass with its
/// encode, call and apply, over every captured corpus and kernel function.
#[test]
fn flip_gate() {
    if !speed() {
        return;
    }
    let mut funcs: Vec<ir::Function> = corpus().iter().flat_map(|c| captured(&c.world)).collect();
    for (_, source) in kernel_programs() {
        funcs.extend(captured(&checked(&source)));
    }
    let rust = || {
        let mut c = funcs.clone();
        time(|| {
            for f in &mut c {
                native_vectorize::run_rust(f, true);
            }
        })
        .as_nanos()
    };
    let vesper = || {
        time(|| {
            for f in &funcs {
                std::hint::black_box(through_vesper(f, true).unwrap());
            }
        })
        .as_nanos()
    };
    // The boundary alone: encode, read the tables, an empty script.
    let identity = || {
        time(|| {
            for f in &funcs {
                std::hint::black_box(through_vesper(f, false).unwrap());
            }
        })
        .as_nanos()
    };
    let encode = || {
        time(|| {
            for f in &funcs {
                std::hint::black_box(clif_words::encode(f, true));
            }
        })
        .as_nanos()
    };
    // `build`: the whole optimized object of every world's `main`, with the
    // installed hook switched to the Vesper pass or left on Rust's.
    // Compile the package first: its own build runs this pass.
    entry();
    let _ = native_vectorize::install(switched);
    let kernels: Vec<vesper::check::World> = kernel_programs()
        .into_iter()
        .map(|(_, source)| checked(&source))
        .collect();
    let worlds: Vec<&vesper::check::World> = kernels
        .iter()
        .chain(corpus().iter().map(|c| &c.world))
        .collect();
    let build = |on: bool| {
        VESPER_ON.store(on, std::sync::atomic::Ordering::Relaxed);
        let t = time(|| {
            for w in &worlds {
                std::hint::black_box(native::object_with_options(w, "main", None, true).ok());
            }
        })
        .as_nanos();
        VESPER_ON.store(false, std::sync::atomic::Ordering::Relaxed);
        t
    };
    for _ in 0..6 {
        let (a, b, c, d) = (build(false), build(true), build(true), build(false));
        println!("flip-gate build pairs rust={} vesper={}", a + d, b + c);
    }
    for _ in 0..8 {
        let (a, b, c, d) = (rust(), vesper(), vesper(), rust());
        println!(
            "flip-gate pairs rust={} vesper={} (boundary {}, encode {})",
            a + d,
            b + c,
            2 * identity(),
            2 * encode()
        );
    }
}
