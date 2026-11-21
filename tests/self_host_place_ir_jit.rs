//! Self-hosting SH3 lane 1 (#1240): the IR swap point end to end. Every
//! corpus program with a `main` runs through the native JIT (optimize on
//! and off) twice, in two child processes of this test binary: once with
//! the Rust passes, once with `src/ir_swap.rs`'s identity hooks installed,
//! which send the program through `@vesper/place-ir` (Rust -> words ->
//! Vesper tables -> words -> Rust) after every native pass. The results
//! must be equal.
//!
//! A hook can be installed once per process, hence the children; the
//! parent runs `child_runs_corpus_mains` with `PLACE_IR_JIT_CHILD` set.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::sync::OnceLock;
use vesper::check::World;
use vesper::ir::Program;

const CHILD: &str = "PLACE_IR_JIT_CHILD";
const OUT: &str = "PLACE_IR_JIT_OUT";

fn round_trip_entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| Package::load("packages/vesper-place-ir").entry("place_ir_words", true))
}

/// Programs the hooks sent through Vesper.
static ROUND_TRIPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `program` through `@vesper/place-ir` and back, in place.
fn round_trip(world: Option<&World>, program: &mut Program) {
    ROUND_TRIPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Some(p) = ir_words::through_vesper(world, program, |i| round_trip_entry().words(i)) {
        *program = p;
    }
}

/// The identity hooks: each Rust pass, then the round trip.
const IDENTITY: vesper::ir_swap::IrPasses = vesper::ir_swap::IrPasses {
    lower: Some(|w| {
        let mut p = vesper::ir::lower_world(w);
        round_trip(Some(w), &mut p);
        p
    }),
    opt: Some(|w, p| {
        let n = vesper::ir_opt::optimize(w, p)?;
        round_trip(Some(w), p);
        Ok(n)
    }),
    inline: Some(|w, p, b| {
        let r = vesper::ir_inline::inline(w, p, b);
        round_trip(Some(w), p);
        r
    }),
    hoist: Some(|p| {
        let n = vesper::bounds_hoist::run(p);
        round_trip(None, p);
        n
    }),
};

/// Every corpus world with a parameterless `main` (single files only: a
/// package's entry is its own), every `PLACE_IR_JIT_STEP`-th (default 2).
fn mains() -> Vec<&'static Case> {
    let step = std::env::var("PLACE_IR_JIT_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(2);
    corpus()
        .iter()
        .filter(|c| !c.name.ends_with('/'))
        .filter(|c| {
            c.world
                .functions
                .get("main")
                .is_some_and(|f| f.params.is_empty())
        })
        .step_by(step)
        .collect()
}

/// Every corpus main through the JIT, optimize on and off, one line each.
fn run_mains() -> String {
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut text = String::new();
    for c in mains() {
        for optimize in [true, false] {
            let r = std::panic::catch_unwind(|| {
                vesper::native::jit_with_options(&c.world, "main", optimize)
                    .map(|v| format!("{v:?}"))
            });
            let shown = match r {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => format!("error: {e}"),
                Err(_) => "panic".into(),
            };
            text.push_str(&format!("{} opt={optimize}: {shown}\n", c.name));
        }
    }
    std::panic::set_hook(quiet);
    text
}

#[test]
fn child_runs_corpus_mains() {
    let Some(mode) = std::env::var_os(CHILD) else {
        return;
    };
    let out = std::env::var_os(OUT).expect("output path");
    if mode == "vesper" {
        // Compile the round-trip entry before the hooks exist: its own JIT
        // build goes through the swap point too.
        round_trip_entry();
        vesper::ir_swap::install(IDENTITY).unwrap();
    }
    // Debug builds lower and JIT deep corpus programs with large frames;
    // the default 2 MiB test thread overflows, so run on a big stack.
    let text = std::thread::Builder::new()
        .stack_size(1 << 29)
        .spawn(run_mains)
        .unwrap()
        .join()
        .unwrap();
    if mode == "vesper" {
        let n = ROUND_TRIPS.load(std::sync::atomic::Ordering::Relaxed);
        eprintln!("jit child: {n} programs went through @vesper/place-ir");
        assert!(n > 100, "the identity hooks ran only {n} times");
    }
    std::fs::write(out, text).unwrap();
}

fn run_child(mode: &str) -> String {
    let dir = std::env::temp_dir().join(format!("place-ir-jit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join(mode);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_runs_corpus_mains",
            "--test-threads",
            "1",
            "--nocapture",
        ])
        .env(CHILD, mode)
        .env(OUT, &out)
        .status()
        .unwrap();
    assert!(status.success(), "{mode} child failed: {status}");
    let text = std::fs::read_to_string(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    text
}

#[test]
fn jit_results_match_through_the_identity_hooks() {
    if std::env::var_os(CHILD).is_some() {
        return;
    }
    let rust = run_child("rust");
    let vesper = run_child("vesper");
    let runs = rust.lines().count();
    let ok = rust
        .lines()
        .filter(|l| !l.contains(": error: ") && !l.ends_with(": panic"))
        .count();
    eprintln!(
        "jit: {runs} runs ({ok} returned a value) with the Rust passes and through the identity hooks"
    );
    assert!(ok > 20, "only {ok} corpus mains ran");
    if let Some(d) = first_difference(&rust, &vesper) {
        panic!("jit results differ at {d}");
    }
}
