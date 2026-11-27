//! Rust-exit gap wave 1, lane 11 (#1509), part 6a (#2094): the native
//! walk's block plan (`native::block_plan`: the order `ir_graph_at` lowers a
//! graph's blocks in, the blocks on a cycle, and the blocks it seals early)
//! from `@vesper/native-walk` (packages/vesper-codegen-walk/src/blocks.ves).
//!
//! - `vesper_block_plans_match_rust`: every graph the walk lowers in the
//!   corpus compiles (recorded at the `block_plan` hook), and seeded random
//!   graphs with self-loops, repeated edges and unreachable blocks, planned
//!   by Rust and by the package.
//! - `the_block_plan_swap_matches_rust`: the package installed at the
//!   `block_plan` hook gives 0 CLIF and 0 object differences, and a
//!   perturbed plan is caught.
//! - `block_plan_speed` (ignored; `CHECK_HARNESS_SPEED=1`): ABBA timing of
//!   the plans alone and of whole compiles.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;
#[path = "common/native_walk_words.rs"]
mod native_walk_words;

use check_harness::*;
use native_walk_words::{block_targets_input, decode_block_plan};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use vesper::check::World;
use vesper::codegen_swap::{self, CodegenHooks};
use vesper::native::{BlockPlan, block_plan};

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-codegen-walk"))
}

fn entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    // Compiled with hooks disabled so initializing the entry never recurses.
    E.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            package().entry("native_walk_block_plan", true)
        })
    })
}

/// Every `n`-th corpus world (`NATIVE_WALK_STEP`, default 4).
fn step() -> usize {
    std::env::var("NATIVE_WALK_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(4)
}

/// The package's plan of one graph, `None` when it declines.
fn vesper_plan(targets: &[Vec<usize>]) -> Option<BlockPlan> {
    decode_block_plan(&entry().words(&block_targets_input(targets)), targets.len())
}

fn big_stack(f: impl FnOnce() + Send) {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn_scoped(s, f)
            .expect("spawn a compile thread")
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

/// The corpus worlds with a `main`, every `step()`-th.
fn cases() -> Vec<(&'static str, &'static World)> {
    corpus()
        .iter()
        .filter(|c| c.world.functions.contains_key("main"))
        .step_by(step())
        .map(|c| (c.name.as_str(), &c.world))
        .collect()
}

static RECORDED: Mutex<Vec<Vec<Vec<usize>>>> = Mutex::new(vec![]);

/// A `block_plan` hook that records each graph and leaves the plan to Rust.
fn record(targets: &[Vec<usize>]) -> Option<BlockPlan> {
    RECORDED.lock().unwrap().push(targets.to_vec());
    None
}

/// Every graph the walk lowers while compiling the corpus cases, optimized
/// and not.
fn corpus_graphs() -> &'static [Vec<Vec<usize>>] {
    static G: OnceLock<Vec<Vec<Vec<usize>>>> = OnceLock::new();
    G.get_or_init(|| {
        let hooks = CodegenHooks {
            block_plan: Some(record),
            ..CodegenHooks::default()
        };
        big_stack(|| {
            for (_, world) in cases() {
                for optimize in [true, false] {
                    let _ = codegen_swap::with_hooks(hooks, || {
                        vesper::native::object_with_options(world, "main", None, optimize)
                    });
                }
            }
        });
        std::mem::take(&mut *RECORDED.lock().unwrap())
    })
}

/// Seeded random graphs: up to 40 blocks, 0-4 successors each (repeats and
/// self-loops allowed), so many have loops and unreachable blocks.
fn random_graphs(count: usize) -> Vec<Vec<Vec<usize>>> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |m: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % m) as usize
    };
    (0..count)
        .map(|_| {
            let n = 1 + next(40);
            let forward = next(2) == 0;
            (0..n)
                .map(|i| {
                    (0..next(5))
                        .map(|_| {
                            // Half the graphs mostly jump forward, like lowered
                            // structured code, so many stay acyclic.
                            if forward && next(8) != 0 && i + 1 < n {
                                i + 1 + next((n - i - 1) as u64)
                            } else {
                                next(n as u64)
                            }
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

#[test]
fn vesper_block_plans_match_rust() {
    let corpus = corpus_graphs();
    let random = random_graphs(4000);
    let (mut bad, mut looped) = (0usize, 0usize);
    for (label, graphs) in [("corpus", corpus), ("random", &random[..])] {
        for targets in graphs {
            let rust = block_plan(targets);
            looped += usize::from(!rust.cyclic.is_empty());
            match vesper_plan(targets) {
                Some(v) if v == rust => {}
                v => {
                    bad += 1;
                    if bad <= 5 {
                        eprintln!("block plan ({label}) {targets:?}: rust {rust:?}, vesper {v:?}");
                    }
                }
            }
        }
    }
    // A target out of range is the package's decline.
    assert_eq!(vesper_plan(&[vec![1]]), None);
    let unreached_graphs = corpus
        .iter()
        .chain(&random)
        .filter(|t| !reaches_all(t))
        .count();
    eprintln!(
        "block plans: {bad} of {} graphs differ ({} corpus, {} random; {looped} with loops, \
         {unreached_graphs} with unreachable blocks)",
        corpus.len() + random.len(),
        corpus.len(),
        random.len(),
    );
    assert!(corpus.len() > 100, "only {} corpus graphs", corpus.len());
    assert!(looped > 0 && unreached_graphs > 0);
    assert_eq!(bad, 0);
}

/// Whether every block is reachable from block 0.
fn reaches_all(targets: &[Vec<usize>]) -> bool {
    let mut seen = vec![false; targets.len()];
    let mut stack = vec![0];
    while let Some(b) = stack.pop() {
        if !std::mem::replace(&mut seen[b], true) {
            stack.extend(targets[b].iter().copied());
        }
    }
    seen.into_iter().all(|s| s)
}

static SWAP_CALLS: AtomicU64 = AtomicU64::new(0);
static SWAP_DECLINED: AtomicU64 = AtomicU64::new(0);

fn vesper_hook(targets: &[Vec<usize>]) -> Option<BlockPlan> {
    let plan = vesper_plan(targets);
    match plan {
        Some(_) => SWAP_CALLS.fetch_add(1, Ordering::Relaxed),
        None => SWAP_DECLINED.fetch_add(1, Ordering::Relaxed),
    };
    plan
}

/// A wrong plan: the blocks after the entry lowered in reverse, none sealed
/// early. The swap test must see it.
fn perturbed(targets: &[Vec<usize>]) -> Option<BlockPlan> {
    let mut p = block_plan(targets);
    if p.order.len() > 2 {
        p.order[1..].reverse();
    }
    p.sealed.clear();
    Some(p)
}

/// CLIF and object of `main`, or the error; a panic is an error too.
fn compile(
    world: &World,
    optimize: bool,
    hooks: CodegenHooks,
) -> Result<(Vec<u8>, String), String> {
    let _quiet = quiet_panics();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        codegen_swap::with_hooks(hooks, || {
            vesper::native::object_with_options(world, "main", None, optimize)
        })
    }))
    .unwrap_or_else(|_| Err("panicked".into()))
}

#[test]
fn the_block_plan_swap_matches_rust() {
    big_stack(|| {
        let _ = entry();
        let hooks = CodegenHooks {
            block_plan: Some(vesper_hook),
            ..CodegenHooks::default()
        };
        let wrong = CodegenHooks {
            block_plan: Some(perturbed),
            ..CodegenHooks::default()
        };
        let before = [&SWAP_CALLS, &SWAP_DECLINED].map(|c| c.load(Ordering::Relaxed));
        let (mut bad, mut compiled, mut caught) = (0usize, 0usize, 0usize);
        for (name, world) in cases() {
            for optimize in [true, false] {
                let rust = compile(world, optimize, CodegenHooks::default());
                let hooked = compile(world, optimize, hooks);
                if rust.is_ok() {
                    compiled += 1;
                    if compile(world, optimize, wrong) != rust {
                        caught += 1;
                    }
                }
                if rust != hooked {
                    bad += 1;
                    if bad <= 5 {
                        let diff = match (&rust, &hooked) {
                            (Ok((_, r)), Ok((_, h))) => first_difference(r, h)
                                .unwrap_or_else(|| "object bytes differ".into()),
                            (r, h) => format!(
                                "rust {:?}, hooked {:?}",
                                r.as_ref().err(),
                                h.as_ref().err()
                            ),
                        };
                        eprintln!("block plan swap: {name} (optimize {optimize}) differs: {diff}");
                    }
                }
            }
        }
        let after = [&SWAP_CALLS, &SWAP_DECLINED].map(|c| c.load(Ordering::Relaxed));
        let [calls, declined] = [0, 1].map(|i| after[i] - before[i]);
        eprintln!(
            "block plan swap: {bad} of {compiled} compiles differ ({calls} graphs from Vesper, \
             {declined} declined); the perturbed plan changed {caught}"
        );
        assert!(compiled > 20, "only {compiled} compiles");
        assert!(calls > 0, "hook never called");
        assert_eq!(declined, 0, "{declined} graphs were declined");
        assert!(caught > 0, "a wrong block plan went unnoticed");
        assert_eq!(bad, 0);
    });
}

#[test]
#[ignore = "timing; run with CHECK_HARNESS_SPEED=1 --ignored"]
fn block_plan_speed() {
    if !speed() {
        return;
    }
    let graphs = corpus_graphs();
    let _ = entry();
    if std::env::var_os("FLIP_GATE").is_some() {
        check_harness::abba(
            "block-plans",
            &mut || {
                time(|| {
                    for t in graphs {
                        std::hint::black_box(block_plan(t));
                    }
                })
            },
            &mut || {
                time(|| {
                    for t in graphs {
                        std::hint::black_box(vesper_plan(t));
                    }
                })
            },
        );
        return;
    }
    abba_ci(
        "block plans",
        20,
        &mut || {
            time(|| {
                for t in graphs {
                    std::hint::black_box(block_plan(t));
                }
            })
        },
        &mut || {
            time(|| {
                for t in graphs {
                    std::hint::black_box(vesper_plan(t));
                }
            })
        },
    );
    let hooks = CodegenHooks {
        block_plan: Some(vesper_plan),
        ..CodegenHooks::default()
    };
    let worlds = cases();
    big_stack(|| {
        abba_ci(
            "compiles",
            10,
            &mut || {
                time(|| {
                    for (_, w) in &worlds {
                        let _ = vesper::native::object_with_options(w, "main", None, true);
                    }
                })
            },
            &mut || {
                time(|| {
                    codegen_swap::with_hooks(hooks, || {
                        for (_, w) in &worlds {
                            let _ = vesper::native::object_with_options(w, "main", None, true);
                        }
                    })
                })
            },
        );
    });
}
