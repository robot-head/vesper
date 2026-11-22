//! Self-hosting SH3 lane 12 (#1251): bounds hoisting through the IR swap
//! point's `hoist` hook. Every sixth corpus program with a `main` runs
//! through the native JIT (optimize on and off) twice, in two child
//! processes of this test binary: once with Rust's `bounds_hoist::run`,
//! once with the hook installed, which hoists through `@vesper/bounds-hoist`
//! (Rust -> words -> Vesper tables, versioned -> words -> Rust). The other
//! passes stay Rust. The results must be equal.
//!
//! A hook can be installed once per process, hence the children; the
//! parent runs `child_runs_corpus_mains` with `BOUNDS_HOIST_JIT_CHILD` set.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/corpus_shard.rs"]
mod corpus_shard;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::sync::OnceLock;

const CHILD: &str = "BOUNDS_HOIST_JIT_CHILD";
const OUT: &str = "BOUNDS_HOIST_JIT_OUT";

fn hoist_entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| Package::load("packages/vesper-bounds-hoist").entry("hoist_pass", true))
}

/// Programs the hook hoisted in Vesper.
static HOISTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Only the `hoist` hook: `@vesper/bounds-hoist` in place of
/// `bounds_hoist::run`, or Rust for a program the words cannot carry.
const HOIST: vesper::ir_swap::IrPasses = vesper::ir_swap::IrPasses {
    lower: None,
    opt: None,
    inline: None,
    hoist: Some(|p| {
        match ir_words::through_vesper(None, p, |i| hoist_entry().words(i)) {
            Some(q) => {
                HOISTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                *p = q;
            }
            None => {
                vesper::bounds_hoist::run(p);
            }
        }
        0
    }),
};

/// Every corpus world with a parameterless `main` (single files only: a
/// package's entry is its own), every `BOUNDS_HOIST_JIT_STEP`-th (default 2).
fn mains() -> Vec<&'static Case> {
    let step = std::env::var("BOUNDS_HOIST_JIT_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(6);
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
        // Bazel shards the mains (BUILD.bazel `test_shards`); the children
        // inherit the shard variables.
        .enumerate()
        .filter(|(i, _)| corpus_shard::Shard::from_environment().contains(*i))
        .map(|(_, c)| c)
        .collect()
}

#[test]
fn child_runs_corpus_mains() {
    let Some(mode) = std::env::var_os(CHILD) else {
        return;
    };
    let out = std::env::var_os(OUT).expect("output path");
    if mode == "vesper" {
        // Compile the pass entry before the hook exists: its own JIT build
        // goes through the swap point too.
        hoist_entry();
        vesper::ir_swap::install(HOIST).unwrap();
    }
    // Debug builds recurse deeply compiling some corpus mains: run them on
    // a thread with a large stack, the same in both children.
    let text = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(|| {
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
        })
        .unwrap()
        .join()
        .unwrap();
    std::fs::write(out, text).unwrap();
    if mode == "vesper" {
        // The hook runs on the optimized path only (`opt=true` runs).
        let n = HOISTED.load(std::sync::atomic::Ordering::Relaxed);
        eprintln!("jit child: @vesper/bounds-hoist hoisted {n} programs");
        let least = corpus_shard::Shard::from_environment().share(20);
        assert!(n > least, "the hoist hook ran only {n} times");
    }
}

fn run_child(mode: &str) -> String {
    let dir = std::env::temp_dir().join(format!("bounds-hoist-jit-{}", std::process::id()));
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
fn jit_results_match_through_the_vesper_hoist_hook() {
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
        "jit: {runs} runs ({ok} returned a value) with Rust bounds hoisting and through @vesper/bounds-hoist"
    );
    let least = corpus_shard::Shard::from_environment().share(20);
    assert!(ok > least, "only {ok} corpus mains ran");
    if let Some(d) = first_difference(&rust, &vesper) {
        panic!("jit results differ at {d}");
    }
}
