//! Self-hosting SH4 lane 7 (#1281): `@vesper/codegen-calls` against the
//! Rust native backend over the corpus.
//!
//! For each corpus world and a few entries of it, with optimize off and on:
//!
//! - **Instances and declarations.** The Vesper package discovers the
//!   entry's monomorphic instances, plans the internal calling
//!   conventions and declares each instance's symbol and signature. The
//!   Rust side is what `native::object_with_observer` emits: every defined
//!   function's instance key, source function, symbol, `FuncId` and
//!   Cranelift signature, in definition order. When Rust refuses the
//!   entry before defining anything, the messages must be equal.
//! - **Capability ABI.** `native::capability_abi_functions` (which reads
//!   the same discovery without compiling) against the package's `C`
//!   lines.
//!
//! Rust definitions stop at the first function whose emission fails, so a
//! failed compile still checks the declarations of the functions it
//! defined before that one (a prefix). An entry Rust refuses after
//! discovery but before its first definition (the IR passes, or the first
//! function's emission) checks only the capability lines; the test counts
//! those.
//!
//! `CODEGEN_CALLS_STEP` (default 4) takes every n-th world and
//! `CODEGEN_CALLS_ENTRIES` (default 2 on the corpus, 4 on the test sources)
//! entries per world; the report's numbers use 1 and 8.
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
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::check::World;
use vesper::native;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-codegen-calls"))
}

fn env_or(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

// ------------------------------------------------------------------ cases

/// Entries of `w`: `main` when there is one, then other non-generic
/// functions with bodies in name order, up to `n`.
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

/// Rust's side of one task: the capability lines, the declaration lines of
/// the defined functions, and the refusal when there is one.
struct RustTask {
    cap: String,
    lines: Vec<String>,
    err: Option<String>,
}

fn rust_task(w: &World, entry: &str, optimize: bool) -> RustTask {
    let cap = match native::capability_abi_functions(w, entry) {
        Ok(keys) => keys.iter().map(|k| format!("C {k}\n")).collect(),
        Err(e) => format!("C! {e}\n"),
    };
    let mut lines = vec![];
    let mut observe = |f: native::EmittedFunction| {
        if f.key.contains("::cold#") {
            return;
        }
        let sig = &f.function.signature;
        let params: Vec<String> = sig
            .params
            .iter()
            .map(|p| p.value_type.to_string())
            .collect();
        let returns: Vec<String> = sig
            .returns
            .iter()
            .map(|p| p.value_type.to_string())
            .collect();
        lines.push(format!(
            "I {} {} = {} @ {} : ({}) -> {}",
            f.func_id,
            f.key,
            f.source,
            f.symbol,
            params.join(", "),
            returns.join(", ")
        ));
    };
    let r = native::object_with_observer(w, entry, None, optimize, Some(&mut observe));
    RustTask {
        cap,
        lines,
        err: r.err(),
    }
}

/// One task's block of the Vesper text: (capability lines, declaration
/// lines, refusal).
fn vesper_task(block: &str) -> (String, Vec<String>, Option<String>) {
    let mut cap = String::new();
    let mut lines = vec![];
    let mut err = None;
    for l in block.lines() {
        if l.starts_with("C ") || l.starts_with("C! ") {
            cap.push_str(l);
            cap.push('\n');
        } else if l.starts_with("I ") {
            lines.push(l.to_string());
        } else if let Some(e) = l.strip_prefix("X ") {
            err = Some(e.to_string());
        }
    }
    (cap, lines, err)
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
    cap_lines: usize,
    bad: usize,
    rust: Duration,
    vesper: Duration,
}

/// Compares one world's tasks.
fn check_world(calls: &Entry, name: &str, w: &World, per: usize, tally: &mut Tally) {
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
    let text = calls.text(&input);
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
        let (cap, lines, err) = vesper_task(&block);
        let label = format!("{name} {entry} opt={optimize}");
        let mut ok = true;
        tally.cap_lines += rust.cap.lines().count();
        if let Some(d) = first_difference(&rust.cap, &cap) {
            ok = false;
            eprintln!("{label}: capability lines differ at {d}");
        }
        match (&rust.err, rust.lines.is_empty()) {
            (None, _) => {
                tally.full += 1;
                tally.functions += rust.lines.len();
                if err.is_some() || lines != rust.lines {
                    ok = false;
                    eprintln!(
                        "{label}: declarations differ: {:?}",
                        first_difference(&rust.lines.join("\n"), &lines.join("\n")).or(err.clone())
                    );
                }
            }
            (Some(_), false) => {
                tally.prefix += 1;
                tally.functions += rust.lines.len();
                if lines.len() < rust.lines.len() || lines[..rust.lines.len()] != rust.lines[..] {
                    ok = false;
                    eprintln!(
                        "{label}: declared prefix differs: {:?} ({:?})",
                        first_difference(&rust.lines.join("\n"), &lines.join("\n")),
                        err
                    );
                }
            }
            (Some(e), true) => match &err {
                Some(v) => {
                    tally.refused += 1;
                    if v != e {
                        ok = false;
                        eprintln!("{label}: refusals differ: rust {e:?}, vesper {v:?}");
                    }
                }
                None => tally.unverified += 1,
            },
        }
        if !ok {
            tally.bad += 1;
        }
        if std::env::var_os("CODEGEN_CALLS_SHOW").is_some() && !rust.lines.is_empty() {
            eprintln!(
                "{label}\nrust:\n{}\nvesper:\n{block}",
                rust.lines.join("\n")
            );
        }
    }
}

// ------------------------------------------------------------------ test sources

fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn instances_and_declarations_match_rust() {
    let tally = big_stack(|| {
        let mut t = Tally::default();
        let step = env_or("CODEGEN_CALLS_STEP", 4);
        let per = env_or("CODEGEN_CALLS_ENTRIES", 2);
        let calls = package().entry("codegen_calls_text", true);
        for case in corpus().iter().step_by(step) {
            check_world(&calls, &case.name, &case.world, per, &mut t);
        }
        t
    });
    eprintln!("codegen calls: {tally:#?}");
    assert!(tally.worlds > 20, "only {} worlds", tally.worlds);
    assert!(tally.full > 20, "only {} complete compiles", tally.full);
    assert_eq!(tally.bad, 0);
}

#[test]
fn instances_and_declarations_match_rust_on_test_sources() {
    let tally = big_stack(|| {
        let mut t = Tally::default();
        let per = env_or("CODEGEN_CALLS_ENTRIES", 4);
        let calls = package().entry("codegen_calls_text", true);
        for (name, w) in source_worlds() {
            check_world(&calls, &name, &w, per, &mut t);
        }
        t
    });
    eprintln!("codegen calls on test sources: {tally:#?}");
    assert!(tally.full > 50, "only {} complete compiles", tally.full);
    assert_eq!(tally.bad, 0);
}

/// The cost of discovery and declaration, over the corpus with optimize
/// on: Rust's `capability_abi_functions` (discovery and `concrete` per
/// instance, which lowers the world first) less one lowering per entry,
/// against the package's entry less the same input with no tasks (the
/// decoding of the world, program and calls sections).
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn codegen_calls_speed() {
    if !speed() {
        return;
    }
    big_stack(|| {
        let calls = package().entry("codegen_calls_text", true);
        let mut cases = vec![];
        for case in corpus() {
            let w = &case.world;
            let names = entries(w, 4);
            if names.is_empty() {
                continue;
            }
            let tasks: Vec<(String, bool)> = names.iter().map(|n| (n.clone(), true)).collect();
            let program = vesper::ir_swap::lower(w);
            let (Some(full), Some(empty)) = (
                calls_input(w, &program, &tasks),
                calls_input(w, &program, &[]),
            ) else {
                continue;
            };
            cases.push((w, names, full, empty));
        }
        let mut discover = || {
            time(|| {
                for (w, names, _, _) in &cases {
                    for n in names {
                        let _ = native::capability_abi_functions(w, n);
                    }
                }
            })
        };
        let mut lower = || {
            time(|| {
                for (w, names, _, _) in &cases {
                    for _ in names {
                        std::hint::black_box(vesper::ir_swap::lower(w));
                    }
                }
            })
        };
        let mut vesper_full = || {
            time(|| {
                for (_, _, full, _) in &cases {
                    std::hint::black_box(calls.text(full));
                }
            })
        };
        let mut vesper_empty = || {
            time(|| {
                for (_, _, _, empty) in &cases {
                    std::hint::black_box(calls.text(empty));
                }
            })
        };
        let (rd, vf) = abba("discovery + entry", &mut discover, &mut vesper_full);
        let (rl, ve) = abba("lowering + decode only", &mut lower, &mut vesper_empty);
        let r = rd.saturating_sub(rl);
        let v = vf.saturating_sub(ve);
        eprintln!(
            "codegen calls over {} worlds: rust discovery {r:?} (of {rd:?}), vesper discovery + declarations {v:?} (of {vf:?}), ratio {:.2}",
            cases.len(),
            v.as_secs_f64() / r.as_secs_f64()
        );
    });
}
