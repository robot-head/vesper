//! Rust-exit gap wave 1, lane 11 (#1509), part 3a (#1876): the entry-block
//! plan of `@vesper/native-walk` (packages/vesper-codegen-walk/src/frame.ves)
//! against the Rust walk's `native::frame_plan` over the corpus.
//!
//! For each corpus world and a few entries of it, with optimize off and on,
//! the Rust side is the plan each defined function was lowered with
//! (`EmittedFunction::frame`, from `native::object_with_observer`): which
//! trailing entry-block parameters are the trace flag, the failure record
//! and the caller slot, where the result is built (a scalar value, the
//! leading result pointer, a register-result buffer or words the body
//! keeps), how each parameter reaches its variable, the function-value
//! parameters the instance binds and the capability identities the body
//! starts with. The Vesper side plans every instance the calls family
//! discovers, from the same input as tests/self_host_codegen_calls.rs.
//!
//! Rust stops at the first function whose emission fails, so a failed
//! compile checks the plans of the functions it defined before that one.
//! An entry Rust refuses before its first definition is compared only when
//! the package refuses it too (a declaration refusal); the test counts the
//! rest as unverified, as the calls test does.
//!
//! `NATIVE_WALK_FRAME_STEP` (default 4) takes every n-th corpus world and
//! `NATIVE_WALK_FRAME_ENTRIES` (default 2 on the corpus, 4 on the test
//! sources) entries per world; the report's numbers use 1 and 4.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/codegen_calls_sources.rs"]
mod codegen_calls_sources;
#[path = "common/codegen_calls_words.rs"]
mod codegen_calls_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use codegen_calls_sources::source_worlds;
use codegen_calls_words::calls_input;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::check::World;
use vesper::native;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-codegen-walk"))
}

fn env_or(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Entries of `w`: `main` when there is one, then other non-generic
/// functions with bodies in name order, up to `n` (the calls test's).
fn entries(w: &World, n: usize) -> Vec<String> {
    let mut out = vec![];
    if w.functions.get("main").is_some_and(|f| f.body.is_some()) {
        out.push("main".to_string());
    }
    for (name, f) in &w.functions {
        if out.len() >= n {
            break;
        }
        if name != "main" && f.body.is_some() && f.generics.is_empty() && !name.contains('$') {
            out.push(name.clone());
        }
    }
    out
}

/// Rust's side of one task: the plan of every defined function by instance
/// key, and the error when the compile failed.
struct RustTask {
    frames: BTreeMap<String, String>,
    err: Option<String>,
}

fn rust_task(w: &World, entry: &str, optimize: bool) -> RustTask {
    let mut frames = BTreeMap::new();
    let mut observe = |f: native::EmittedFunction| {
        if let Some(frame) = &f.frame {
            frames.insert(f.key.clone(), frame.text());
        }
    };
    let r = native::object_with_observer(w, entry, None, optimize, Some(&mut observe));
    RustTask {
        frames,
        err: r.err(),
    }
}

/// One task's block of the Vesper text: the plans by key, and the refusal.
fn vesper_task(block: &str) -> (BTreeMap<String, String>, Option<String>) {
    let mut frames: BTreeMap<String, String> = BTreeMap::new();
    let mut key: Option<String> = None;
    let mut err = None;
    for l in block.lines() {
        if let Some(k) = l.strip_prefix("F ") {
            key = Some(k.to_string());
            frames.insert(k.to_string(), String::new());
        } else if let Some(e) = l.strip_prefix("X ") {
            err = Some(e.to_string());
            key = None;
        } else if let Some(k) = &key {
            let text = frames.get_mut(k).unwrap();
            text.push_str(l);
            text.push('\n');
        }
    }
    (frames, err)
}

#[derive(Default, Debug)]
struct Tally {
    worlds: usize,
    skipped: usize,
    tasks: usize,
    full: usize,
    prefix: usize,
    refused: usize,
    unverified: usize,
    functions: usize,
    with_fail_record: usize,
    with_caller_slot: usize,
    sret: usize,
    buffers: usize,
    words: usize,
    loads: usize,
    function_args: usize,
    identities: usize,
    bad: usize,
    rust: Duration,
    vesper: Duration,
}

impl Tally {
    fn count(&mut self, plan: &str) {
        self.functions += 1;
        let head = plan.lines().next().unwrap_or("");
        self.with_fail_record += head.contains("fail=1") as usize;
        self.with_caller_slot += head.contains("caller=1") as usize;
        self.sret += head.ends_with("result=sret") as usize;
        self.buffers += head.contains("result=buffer") as usize;
        self.words += head.contains("result=words") as usize;
        for l in plan.lines() {
            self.loads += (l.starts_with("P ") && l.contains(" load ")) as usize;
            self.function_args += l.starts_with("A ") as usize;
            self.identities += l.starts_with("K ") as usize;
        }
    }
}

/// Compares one world's tasks.
fn check_world(frames: &Entry, name: &str, w: &World, per: usize, tally: &mut Tally) {
    let names = entries(w, per);
    if names.is_empty() {
        return;
    }
    let tasks: Vec<(String, bool)> = names
        .iter()
        .flat_map(|n| [(n.clone(), false), (n.clone(), true)])
        .collect();
    let program = vesper::ir_swap::lower(w);
    let Some(input) = calls_input(w, &program, &tasks) else {
        tally.skipped += 1;
        return;
    };
    tally.worlds += 1;
    let start = Instant::now();
    let text = frames.text(&input);
    tally.vesper += start.elapsed();
    let mut blocks: Vec<String> = vec![];
    for l in text.lines() {
        if l.starts_with("E ") {
            blocks.push(String::new());
        } else if let Some(b) = blocks.last_mut() {
            b.push_str(l);
            b.push('\n');
        }
    }
    assert_eq!(blocks.len(), tasks.len(), "{name}: {text}");
    for ((entry, optimize), block) in tasks.iter().zip(blocks) {
        tally.tasks += 1;
        let start = Instant::now();
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let rust = std::panic::catch_unwind(|| rust_task(w, entry, *optimize));
        std::panic::set_hook(quiet);
        tally.rust += start.elapsed();
        let Ok(rust) = rust else {
            tally.unverified += 1;
            continue;
        };
        let (plans, err) = vesper_task(&block);
        let label = format!("{name} {entry} opt={optimize}");
        let mut ok = true;
        match (&rust.err, rust.frames.is_empty()) {
            (None, _) | (Some(_), false) => {
                if rust.err.is_none() {
                    tally.full += 1;
                    if err.is_some() {
                        ok = false;
                        eprintln!("{label}: Rust compiled, Vesper refused: {err:?}");
                    }
                } else {
                    tally.prefix += 1;
                }
                for (key, plan) in &rust.frames {
                    tally.count(plan);
                    match plans.get(key) {
                        Some(v) if v == plan => {}
                        Some(v) => {
                            ok = false;
                            eprintln!(
                                "{label}: plan of {key} differs: {:?}",
                                first_difference(plan, v)
                            );
                        }
                        None => {
                            ok = false;
                            eprintln!("{label}: no Vesper plan for {key} ({err:?})");
                        }
                    }
                }
            }
            (Some(e), true) => match &err {
                Some(v) if plans.is_empty() => {
                    tally.refused += 1;
                    if v != e {
                        ok = false;
                        eprintln!("{label}: refusals differ: rust {e:?}, vesper {v:?}");
                    }
                }
                _ => tally.unverified += 1,
            },
        }
        if !ok {
            tally.bad += 1;
        }
        if std::env::var_os("NATIVE_WALK_FRAME_SHOW").is_some() {
            eprintln!("{label}\nrust: {:#?}\nvesper:\n{block}", rust.frames);
        }
    }
}

fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn frame_plans_match_rust() {
    let tally = big_stack(|| {
        let mut t = Tally::default();
        let step = env_or("NATIVE_WALK_FRAME_STEP", 4);
        let per = env_or("NATIVE_WALK_FRAME_ENTRIES", 2);
        let frames = package().entry("native_walk_frames", true);
        for case in corpus().iter().step_by(step) {
            check_world(&frames, &case.name, &case.world, per, &mut t);
        }
        t
    });
    eprintln!("native walk frames: {tally:#?}");
    assert!(tally.worlds > 20, "only {} worlds", tally.worlds);
    assert!(tally.full > 20, "only {} complete compiles", tally.full);
    assert_eq!(tally.bad, 0);
}

#[test]
fn frame_plans_match_rust_on_test_sources() {
    let tally = big_stack(|| {
        let mut t = Tally::default();
        let per = env_or("NATIVE_WALK_FRAME_ENTRIES", 4);
        let frames = package().entry("native_walk_frames", true);
        for (name, w) in source_worlds() {
            check_world(&frames, &name, &w, per, &mut t);
        }
        t
    });
    eprintln!("native walk frames on test sources: {tally:#?}");
    assert!(tally.full > 50, "only {} complete compiles", tally.full);
    assert_eq!(tally.bad, 0);
}
