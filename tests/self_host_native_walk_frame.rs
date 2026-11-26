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
//! Part 3b (`the_entry_plans_swap_matches_rust`) installs the package at
//! the `entry_plans` swap hook of `vesper::codegen_swap`, so the walk emits
//! each entry block from the Vesper plan and (part 4a) takes the internal
//! calling conventions from it in place of `NativeAbi::plan`, and proves 0
//! CLIF and 0 object diffs against the Rust plans.
//!
//! Part 5 (#2056) covers the rest of the walk's compiles: Beacon trace hosts
//! (`the_entry_plans_swap_matches_rust_on_trace_hosts`) and library objects
//! at the `export_plans` hook (`the_export_plans_swap_matches_rust`).
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
#[path = "common/native_walk_words.rs"]
mod native_walk_words;

use check_harness::*;
use codegen_calls_sources::source_worlds;
use codegen_calls_words::{calls_input, calls_input_flags};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use vesper::check::World;
use vesper::codegen_swap::{self, CodegenHooks};
use vesper::ir::Program;
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
        } else if l.starts_with("B ")
            || l.starts_with("I ")
            || l.starts_with("V ")
            || l.starts_with('W')
        {
            // The convention and the instance (parts 4a and 4b), checked by
            // the swap test.
            key = None;
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
        let quiet = quiet_panics();
        let rust = std::panic::catch_unwind(|| rust_task(w, entry, *optimize));
        drop(quiet);
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

fn frames_entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    // Compiled with hooks off so building the entry never asks the hook.
    E.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            package().entry("native_walk_frames", true)
        })
    })
}

/// Compiles that asked the hook, that it answered, and the plans it gave.
static SWAP_CALLS: AtomicU64 = AtomicU64::new(0);
static SWAP_ANSWERED: AtomicU64 = AtomicU64::new(0);
static SWAP_PLANS: AtomicU64 = AtomicU64::new(0);
/// Nanoseconds spent in the hook (encoding, the package, decoding).
static SWAP_NANOS: AtomicU64 = AtomicU64::new(0);

/// The package's plans of one task (`trace`: a Beacon trace host).
fn package_plans(
    world: &World,
    program: &Program,
    entry: &str,
    optimize: bool,
    trace: bool,
) -> Option<native::EntryPlans> {
    let flags = optimize as u64 | (trace as u64) << 1;
    calls_input_flags(world, program, &[(entry.to_string(), flags)])
        .and_then(|input| native_walk_words::decode_entry_plans(&frames_entry().words(&input)))
}

/// The `entry_plans` hook: the package plans the one task.
fn vesper_entry_plans(
    world: &World,
    program: &Program,
    entry: &str,
    optimize: bool,
    trace: bool,
) -> Option<native::EntryPlans> {
    SWAP_CALLS.fetch_add(1, Ordering::Relaxed);
    let start = Instant::now();
    let plans = package_plans(world, program, entry, optimize, trace);
    SWAP_NANOS.fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    let plans = plans?;
    SWAP_ANSWERED.fetch_add(1, Ordering::Relaxed);
    SWAP_PLANS.fetch_add(plans.frames.len() as u64, Ordering::Relaxed);
    Some(plans)
}

#[test]
fn frame_plan_text_round_trips() {
    let text = "trace=1 fail=1 caller=0 result=buffer 3\nP a value\nP b load i32\n\
                A f g\nK $effect:Fail native-fail-exit:0\n";
    let plan = native::FramePlan::parse(text).expect("a plan");
    assert_eq!(plan.result, native::FrameResult::Buffer(3));
    assert!(plan.params[1].1.is_some());
    assert_eq!(plan.text(), text);
    assert_eq!(native::FramePlan::parse("trace=0 fail=0"), None);
    assert_eq!(
        native::FramePlan::parse("trace=0 fail=0 caller=0 result=value\nP a load v9\n"),
        None
    );
}

/// With the package at the `entry_plans` hook, every entry compile of the
/// sampled corpus worlds and the test sources gives Rust's CLIF and object.
#[test]
fn the_entry_plans_swap_matches_rust() {
    let (compiled, bad, calls, answered, plans, times) = big_stack(|| {
        let _ = frames_entry();
        let hooks = CodegenHooks {
            entry_plans: Some(vesper_entry_plans),
            ..CodegenHooks::default()
        };
        let step = env_or("NATIVE_WALK_FRAME_STEP", 4);
        let per = env_or("NATIVE_WALK_FRAME_ENTRIES", 2);
        let sources = source_worlds();
        let worlds = corpus()
            .iter()
            .step_by(step)
            .map(|c| (&c.name, &c.world))
            .chain(sources.iter().map(|(n, w)| (n, w)));
        let (mut compiled, mut bad) = (0usize, 0usize);
        let (mut rust_time, mut hooked_time) = (Duration::ZERO, Duration::ZERO);
        for (name, w) in worlds {
            for entry in entries(w, per) {
                for optimize in [false, true] {
                    let quiet = quiet_panics();
                    let start = Instant::now();
                    let rust = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        native::object_with_options(w, &entry, None, optimize)
                    }));
                    rust_time += start.elapsed();
                    let start = Instant::now();
                    let hooked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        codegen_swap::with_hooks(hooks, || {
                            native::object_with_options(w, &entry, None, optimize)
                        })
                    }));
                    hooked_time += start.elapsed();
                    drop(quiet);
                    let label = format!("{name} {entry} opt={optimize}");
                    // A task whose Rust compile panics is unsupported; one
                    // that panics only with the hook is a difference.
                    let Ok(rust) = rust else {
                        continue;
                    };
                    let Ok(hooked) = hooked else {
                        bad += 1;
                        eprintln!("{label}: the hooked compile panicked");
                        continue;
                    };
                    match (rust, hooked) {
                        (Ok((robj, rclif)), Ok((hobj, hclif))) => {
                            compiled += 1;
                            if rclif != hclif || robj != hobj {
                                bad += 1;
                                let diff = first_difference(&rclif, &hclif)
                                    .unwrap_or_else(|| "object bytes differ".into());
                                eprintln!("{label}: {diff}");
                            }
                        }
                        (Err(r), Err(h)) if r == h => {}
                        (r, h) => {
                            bad += 1;
                            eprintln!("{label}: rust {:?}, hooked {:?}", r.err(), h.err());
                        }
                    }
                }
            }
        }
        let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
        (
            compiled,
            bad,
            load(&SWAP_CALLS),
            load(&SWAP_ANSWERED),
            load(&SWAP_PLANS),
            (
                rust_time,
                hooked_time,
                Duration::from_nanos(load(&SWAP_NANOS)),
            ),
        )
    });
    eprintln!(
        "entry_plans swap: {compiled} compiles, {bad} differ; hook asked {calls}, \
         answered {answered}, {plans} Vesper plans; compiles {:?} Rust, {:?} hooked \
         ({:?} in the hook)",
        times.0, times.1, times.2
    );
    assert!(compiled > 50, "only {compiled} compiles");
    assert!(
        answered * 2 > calls,
        "the hook answered {answered} of {calls}"
    );
    assert_eq!(bad, 0);
}

/// Trace-host compiles that asked the hook, the plans it gave and how many
/// of those take the trace flag (part 5).
static TRACE_CALLS: AtomicU64 = AtomicU64::new(0);
static TRACE_PLANS: AtomicU64 = AtomicU64::new(0);
static TRACE_FLAGGED: AtomicU64 = AtomicU64::new(0);

/// The `entry_plans` hook of the trace-host test, with its own counters.
fn vesper_trace_plans(
    world: &World,
    program: &Program,
    entry: &str,
    optimize: bool,
    trace: bool,
) -> Option<native::EntryPlans> {
    let plans = package_plans(world, program, entry, optimize, trace)?;
    if trace {
        TRACE_CALLS.fetch_add(1, Ordering::Relaxed);
        TRACE_PLANS.fetch_add(plans.frames.len() as u64, Ordering::Relaxed);
        let flagged = plans.frames.values().filter(|p| p.trace_fail).count();
        TRACE_FLAGGED.fetch_add(flagged as u64, Ordering::Relaxed);
    }
    Some(plans)
}

/// Beacon programs: the accepted trace fixtures, a few shapes of
/// tests/trace_effects.rs, and the corpus worlds where Beacon is active.
fn trace_worlds() -> Vec<(String, World)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/trace_effects/accept");
    let mut sources: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "ves"))
        .map(|p| {
            (
                p.display().to_string(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    sources.sort();
    let inline = [
        "fn walk(n: u64) -> u64 eff [Trace] {
            if n == 0 {
                0
            } else {
                let s = Trace::start_span(0, \"step\");
                let v = walk(wrapping_sub(n, 1));
                Trace::emit_ok(s, n);
                wrapping_add(v, n)
            }
        }
        fn main() -> u64 eff [Trace] { walk(3) }",
        "fn twice() -> u64 eff [Trace] {
            let a = Trace::start_span(0, \"a\");
            Trace::emit_ok(a, 1);
            let b = Trace::start_span(0, \"b\");
            Trace::emit_ok(b, 2);
            2
        }
        fn pair(x: u64, inout y: u64) -> (u64, u64) eff [Trace] {
            let s = Trace::start_span(0, \"pair\");
            y = wrapping_add(y, x);
            Trace::emit_ok(s, x);
            (x, y)
        }
        fn main() -> u64 eff [Trace] {
            let mut y = 4;
            let (a, b) = pair(twice(), inout y);
            wrapping_add(a, b)
        }",
        "fn plain(x: u64) -> u64 { wrapping_mul(x, 3) }
        fn main() -> u64 eff [Trace] { let c = Trace::current_ctx(); plain(c.span_id) }",
    ];
    for (i, src) in inline.iter().enumerate() {
        sources.push((format!("inline {i}"), src.to_string()));
    }
    let mut out = vec![];
    for (name, src) in sources {
        let p = vesper::parser::parse(&src);
        assert!(p.diagnostics.is_empty(), "{name}: {:?}", p.diagnostics);
        let c = vesper::check::check(&p.module);
        assert!(c.diagnostics.is_empty(), "{name}: {:?}", c.diagnostics);
        out.push((name, c.world));
    }
    for case in corpus() {
        if vesper::trace_effects::active(&case.world) {
            out.push((case.name.clone(), case.world.clone()));
        }
    }
    out
}

/// Part 5: trace-host compiles (`object_traced_with_options`) ask the hook
/// too, and with the package's plans give Rust's CLIF and object.
#[test]
fn the_entry_plans_swap_matches_rust_on_trace_hosts() {
    let (compiled, bad, rust_time, hooked_time) = big_stack(|| {
        let _ = frames_entry();
        let hooks = CodegenHooks {
            entry_plans: Some(vesper_trace_plans),
            ..CodegenHooks::default()
        };
        let (mut compiled, mut bad) = (0usize, 0usize);
        let (mut rust_time, mut hooked_time) = (Duration::ZERO, Duration::ZERO);
        for (name, w) in trace_worlds() {
            for entry in entries(&w, 4) {
                for optimize in [false, true] {
                    let quiet = quiet_panics();
                    let start = Instant::now();
                    let rust = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        native::object_traced_with_options(&w, &entry, None, optimize)
                    }));
                    rust_time += start.elapsed();
                    let start = Instant::now();
                    let hooked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        codegen_swap::with_hooks(hooks, || {
                            native::object_traced_with_options(&w, &entry, None, optimize)
                        })
                    }));
                    hooked_time += start.elapsed();
                    drop(quiet);
                    let label = format!("{name} {entry} opt={optimize}");
                    let Ok(rust) = rust else {
                        continue;
                    };
                    let Ok(hooked) = hooked else {
                        bad += 1;
                        eprintln!("{label}: the hooked compile panicked");
                        continue;
                    };
                    match (rust, hooked) {
                        (Ok((robj, rclif)), Ok((hobj, hclif))) => {
                            compiled += 1;
                            if rclif != hclif || robj != hobj {
                                bad += 1;
                                let diff = first_difference(&rclif, &hclif)
                                    .unwrap_or_else(|| "object bytes differ".into());
                                eprintln!("{label}: {diff}");
                            }
                        }
                        (Err(r), Err(h)) if r == h => {}
                        (r, h) => {
                            bad += 1;
                            eprintln!("{label}: rust {:?}, hooked {:?}", r.err(), h.err());
                        }
                    }
                }
            }
        }
        (compiled, bad, rust_time, hooked_time)
    });
    let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
    let (calls, plans, flagged) = (load(&TRACE_CALLS), load(&TRACE_PLANS), load(&TRACE_FLAGGED));
    eprintln!(
        "entry_plans trace hosts: {compiled} compiles, {bad} differ; {calls} trace-host \
         answers, {plans} plans, {flagged} with the trace flag; compiles {rust_time:?} Rust, \
         {hooked_time:?} hooked"
    );
    assert!(compiled >= 10, "only {compiled} compiles");
    assert!(flagged > 0, "no plan took the trace flag");
    assert_eq!(bad, 0);
}

/// Library builds that asked the export hook, and the plans it gave.
static EXPORT_CALLS: AtomicU64 = AtomicU64::new(0);
static EXPORT_ANSWERED: AtomicU64 = AtomicU64::new(0);
static EXPORT_PLANS: AtomicU64 = AtomicU64::new(0);

/// The `export_plans` hook: one library task over the export roots.
fn vesper_export_plans(
    world: &World,
    program: &Program,
    roots: &[&str],
    optimize: bool,
) -> Option<native::EntryPlans> {
    EXPORT_CALLS.fetch_add(1, Ordering::Relaxed);
    let tasks: Vec<(String, u64)> = roots
        .iter()
        .enumerate()
        .map(|(i, r)| (r.to_string(), optimize as u64 | if i == 0 { 4 } else { 8 }))
        .collect();
    let plans = calls_input_flags(world, program, &tasks)
        .and_then(|input| native_walk_words::decode_entry_plans(&frames_entry().words(&input)))?;
    EXPORT_ANSWERED.fetch_add(1, Ordering::Relaxed);
    EXPORT_PLANS.fetch_add(plans.frames.len() as u64, Ordering::Relaxed);
    Some(plans)
}

/// Part 5: library objects (`native_export::compile_world`, every sampled
/// entry exported) built with the package at the `export_plans` hook give
/// Rust's object bytes and errors.
#[test]
fn the_export_plans_swap_matches_rust() {
    use vesper::native::export::{LibraryOptions, compile_world};
    let (compiled, bad, rust_time, hooked_time) = big_stack(|| {
        let _ = frames_entry();
        let hooks = CodegenHooks {
            export_plans: Some(vesper_export_plans),
            ..CodegenHooks::default()
        };
        let step = env_or("NATIVE_WALK_FRAME_STEP", 4);
        let per = env_or("NATIVE_WALK_FRAME_ENTRIES", 4);
        let sources = source_worlds();
        let worlds = corpus()
            .iter()
            .step_by(step)
            .map(|c| (&c.name, &c.world))
            .chain(sources.iter().map(|(n, w)| (n, w)));
        let (mut compiled, mut bad) = (0usize, 0usize);
        let (mut rust_time, mut hooked_time) = (Duration::ZERO, Duration::ZERO);
        for (name, w) in worlds {
            let public: std::collections::BTreeSet<String> = entries(w, per).into_iter().collect();
            if public.is_empty() {
                continue;
            }
            for optimize in [false, true] {
                let options = LibraryOptions {
                    library: "walk",
                    target: None,
                    optimize,
                };
                let quiet = quiet_panics();
                let start = Instant::now();
                let rust = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    compile_world(w, &public, &options)
                }));
                rust_time += start.elapsed();
                let start = Instant::now();
                let hooked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    codegen_swap::with_hooks(hooks, || compile_world(w, &public, &options))
                }));
                hooked_time += start.elapsed();
                drop(quiet);
                let label = format!("{name} opt={optimize}");
                let Ok(rust) = rust else {
                    continue;
                };
                let Ok(hooked) = hooked else {
                    bad += 1;
                    eprintln!("{label}: the hooked build panicked");
                    continue;
                };
                match (rust, hooked) {
                    (Ok(r), Ok(h)) => {
                        compiled += 1;
                        if r.object != h.object {
                            bad += 1;
                            eprintln!("{label}: object bytes differ");
                        }
                    }
                    (Err(r), Err(h)) if r.to_string() == h.to_string() => {}
                    (r, h) => {
                        bad += 1;
                        eprintln!(
                            "{label}: rust {:?}, hooked {:?}",
                            r.err().map(|e| e.to_string()),
                            h.err().map(|e| e.to_string())
                        );
                    }
                }
            }
        }
        (compiled, bad, rust_time, hooked_time)
    });
    let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
    let (calls, answered, plans) = (
        load(&EXPORT_CALLS),
        load(&EXPORT_ANSWERED),
        load(&EXPORT_PLANS),
    );
    eprintln!(
        "export_plans swap: {compiled} libraries, {bad} differ; hook asked {calls}, answered \
         {answered}, {plans} Vesper plans; builds {rust_time:?} Rust, {hooked_time:?} hooked"
    );
    assert!(compiled > 50, "only {compiled} libraries");
    assert!(
        answered * 2 > calls,
        "the hook answered {answered} of {calls}"
    );
    assert_eq!(bad, 0);
}
