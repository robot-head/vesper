//! Self-hosting SH4 lane 8 (#1282): `@vesper/codegen-aggregates` against
//! the aggregate decisions of the native optimize path
//! (src/native_aggregates.rs), function by function over the corpus.
//!
//! Every corpus world is lowered the way `vesper build` lowers it with
//! `optimize` on (FFI drops, `ir_opt`, the inliner, drop elaboration,
//! bounds hoisting). For every graph the native path plans, Rust's
//! decisions (`vesper::native::aggregate_decisions`) are compared with the
//! package's `aggregate_decisions` entry, line for line:
//!
//! - per local, `plain_data` of its type and `split_words`' word count;
//! - `split_locals`: the locals kept as word variables, with the paths
//!   their uses read;
//! - `plan`: every value's home (the result buffer, a field of a later
//!   constructor, or the destination of a plain write), after the window
//!   rule of `drop_unsafe_places`;
//! - `split_sources`: the reads that lend their storage to a split `let`;
//! - the drop facts of each local's and value's type, and the graph's
//!   `fip::seq_drop_plan`, `replace_drops` and `temporary_drops`;
//! - `scalar_enums` (#1330): the small enum values and locals kept as
//!   words, the values of their words, and the calls and returns that pass
//!   them as words;
//! - and the graph `scalar_enums` rewrites with those words
//!   ([`rewrite_matches_rust`]).
//!
//! The input is `@vesper/place-ir`'s (tests/common/ir_words.rs) followed by
//! [`facts`]: what the decisions read from the checker's `World` that
//! check-data does not carry.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::collections::BTreeSet;
use std::sync::OnceLock;
use vesper::check::World;
use vesper::ir::{Op, Program};

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-codegen-aggregates"))
}

/// Every `n`-th corpus world (`CODEGEN_AGGREGATES_STEP`); set it to 1 for
/// the whole corpus (the report's numbers). The default is every 4th world
/// in an optimized build and every 24th in a debug one (Bazel's test build,
/// where every 4th world passes its 900 s limit).
fn step() -> usize {
    std::env::var("CODEGEN_AGGREGATES_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(if cfg!(debug_assertions) { 24 } else { 4 })
}

/// One corpus world's program as native lowering receives it.
pub struct Prepared {
    pub name: String,
    pub world: &'static World,
    pub program: Program,
}

/// The native optimize path's passes before lowering; a pass that refuses
/// the program drops the world (`vesper build` stops there too).
fn native_program(w: &World) -> Option<Program> {
    let mut p = vesper::ffi_lowering::drops::lower_world(w);
    vesper::ir_opt::optimize(w, &mut p).ok()?;
    vesper::ir_inline::inline(w, &mut p, &vesper::ir_inline::Budget::default());
    vesper::fip::elaborate_non_fip_drops(w, &mut p);
    vesper::bounds_hoist::run(&mut p);
    Some(p)
}

pub fn prepared() -> &'static [Prepared] {
    static P: OnceLock<Vec<Prepared>> = OnceLock::new();
    P.get_or_init(|| {
        let only = std::env::var("CODEGEN_AGGREGATES_ONLY").ok();
        let mut out = vec![];
        let quiet = quiet_panics();
        for c in corpus().iter().step_by(step()) {
            if only.as_ref().is_some_and(|o| !c.name.contains(o.as_str())) {
                continue;
            }
            if let Ok(Some(program)) = std::panic::catch_unwind(|| native_program(&c.world)) {
                out.push(Prepared {
                    name: c.name.clone(),
                    world: &c.world,
                    program,
                });
            }
        }
        drop(quiet);
        out
    })
}

fn put_text(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u64).to_le_bytes());
    out.extend(s.as_bytes());
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
}

fn put(out: &mut Vec<u8>, x: u64) {
    out.extend(x.to_le_bytes());
}

/// The `World` facts the decisions read beyond check-data's world, after
/// the program section:
///
/// ```text
/// functions: n, n x (name text, quiet, fip)   quiet: empty effect row
/// element views: n, n x (span start, end)
/// per program function, in program order:
///     n, n x name text   the free names of its closure and handler bodies
/// per program function, in program order:
///     n, n x (lent, k, k x name text)   each closure instruction, in
///                        instruction order: whether it is a lent closure
///                        (§4.2) and the names its body uses
/// per program function, in program order:
///     ret, n, n x words  the ABI facts of `scalar_enums`
///                        (`aggregate_decisions::scalar_enum_facts`):
///                        whether its result travels as enum words, and
///                        for each call, whether the callee returns them
/// ```
///
/// A text is its byte length, then its bytes padded to 8.
pub fn facts(w: &World, program: &Program) -> Vec<u8> {
    let mut out = vec![];
    put(&mut out, w.functions.len() as u64);
    for (name, f) in &w.functions {
        put_text(&mut out, name);
        let quiet = f.effects.instances.is_empty() && f.effects.tail.is_none();
        put(&mut out, quiet as u64);
        put(&mut out, f.fip as u64);
    }
    put(&mut out, w.element_views.len() as u64);
    for (a, b) in w.element_views.keys() {
        put(&mut out, *a as u64);
        put(&mut out, *b as u64);
    }
    for f in program.functions.values() {
        let mut free = BTreeSet::new();
        for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
            if let Op::Closure { expr, .. } | Op::Handle { expr, .. } = &inst.op {
                free.extend(vesper::check::free_names(expr));
            }
        }
        put(&mut out, free.len() as u64);
        for n in &free {
            put_text(&mut out, n);
        }
    }
    for f in program.functions.values() {
        let closures: Vec<&vesper::ast::Expr> = f
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .filter_map(|i| match &i.op {
                Op::Closure { expr, .. } => Some(expr),
                _ => None,
            })
            .collect();
        put(&mut out, closures.len() as u64);
        for expr in closures {
            let (lent, used) = match &expr.kind {
                vesper::ast::ExprKind::Closure { params, body, .. } => {
                    let mut used = vesper::check::free_names(body);
                    for p in params {
                        used.remove(&p.name);
                    }
                    let key = vesper::ir::nested_name("closure", &body.span);
                    (w.lent_closures.contains_key(&key), used)
                }
                _ => (false, BTreeSet::new()),
            };
            put(&mut out, lent as u64);
            put(&mut out, used.len() as u64);
            for n in &used {
                put_text(&mut out, n);
            }
        }
    }
    for f in program.functions.values() {
        let (ret, calls) = vesper::native::aggregate_decisions::scalar_enum_facts(w, f);
        put(&mut out, ret as u64);
        put(&mut out, calls.len() as u64);
        for c in calls {
            put(&mut out, c as u64);
        }
    }
    out
}

/// The entry's input for `c`, or `None` when the program does not encode.
pub fn input(c: &Prepared) -> Option<Vec<u8>> {
    let (mut bytes, _) = ir_words::program_input(c.world, &c.program)?;
    bytes.extend(facts(c.world, &c.program));
    Some(bytes)
}

/// Sorts the lines of each `fn` section: both sides print a section's
/// lines grouped by kind, but only Rust iterates in name order.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut section: Vec<&str> = vec![];
    let flush = |out: &mut String, section: &mut Vec<&str>| {
        section.sort_unstable();
        for l in section.drain(..) {
            out.push_str(l);
            out.push('\n');
        }
    };
    for line in text.lines() {
        if line.starts_with("fn ") {
            flush(&mut out, &mut section);
            out.push_str(line);
            out.push('\n');
        } else {
            section.push(line);
        }
    }
    flush(&mut out, &mut section);
    out
}

/// Replaces each Rust `fn` section whose Vesper section is `wide` (a graph
/// that mentions `i128`/`u128`, which @vesper/check-layout cannot lay out
/// yet) with that same line; returns the text and how many it replaced.
pub fn skip_wide(rust: &str, vesper: &str) -> (String, usize) {
    fn sections(t: &str) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = vec![];
        for l in t.lines() {
            if l.starts_with("fn ") {
                out.push((l.to_owned(), vec![]));
            } else if let Some(last) = out.last_mut() {
                last.1.push(l.to_owned());
            }
        }
        out
    }
    let wide: BTreeSet<String> = sections(vesper)
        .into_iter()
        .filter(|(_, body)| body.len() == 1 && body[0] == "wide")
        .map(|(head, _)| head)
        .collect();
    let mut out = String::new();
    let mut n = 0;
    for (head, body) in sections(rust) {
        out.push_str(&head);
        out.push('\n');
        if wide.contains(&head) {
            n += 1;
            out.push_str("wide\n");
            continue;
        }
        for l in body {
            out.push_str(&l);
            out.push('\n');
        }
    }
    (out, n)
}

/// Graphs that mention a wide integer are left to Rust (not in the
/// corpus, so checked here): the others of the same program still match.
#[test]
fn wide_integer_graphs_are_left_to_rust() {
    let src = "struct P { a: i128, b: u64 }\n\
               struct Q { x: u64, y: u64 }\n\
               fn wide(x: i128) -> i128 { x }\n\
               fn holds(n: u64) -> P { P { a: 1, b: n } }\n\
               fn pair(n: u64) -> Q { let q = Q { x: n, y: n }; Q { x: q.x, y: q.y } }\n\
               fn main() -> u64 { wrapping_add(pair(3).x, holds(2).b) }\n";
    let parsed = vesper::parser::parse(src);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let world: &'static World = Box::leak(Box::new(checked.world));
    let c = Prepared {
        name: "wide".into(),
        world,
        program: native_program(world).expect("lowers"),
    };
    let entry = package().entry("aggregate_decisions", true);
    let ves = normalize(&entry.text(&input(&c).expect("encodes")));
    let rust = normalize(&vesper::native::aggregate_decisions::program_decisions(
        c.world, &c.program,
    ));
    let (rust, wide) = skip_wide(&rust, &ves);
    assert!(wide >= 2, "{ves}");
    assert!(ves.contains("fn pair\n"), "{ves}");
    assert_eq!(first_difference(&rust, &ves), None);
}

#[test]
fn decisions_match_rust() {
    let entry = package().entry("aggregate_decisions", true);
    let (mut bad, mut skipped, mut functions, mut lines) = (0, 0, 0, 0);
    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    let mut wide = 0;
    for c in prepared() {
        let Some(bytes) = input(c) else {
            skipped += 1;
            continue;
        };
        let rust = normalize(&vesper::native::aggregate_decisions::program_decisions(
            c.world, &c.program,
        ));
        functions += rust.lines().filter(|l| l.starts_with("fn ")).count();
        lines += rust.lines().count();
        for l in rust.lines() {
            let mut words = l.split(' ');
            let kind = words.next().unwrap_or("");
            let kind = match (kind, l.contains(" field "), l.contains(" place ")) {
                ("home", true, _) => "home field",
                ("home", _, true) => "home place",
                ("home", ..) => "home result",
                (k, ..) => k,
            };
            *kinds.entry(kind.to_owned()).or_default() += 1;
        }
        let ves = normalize(&entry.text(&bytes));
        let (rust, n) = skip_wide(&rust, &ves);
        wide += n;
        if let Some(d) = first_difference(&rust, &ves) {
            if let Ok(dir) = std::env::var("CODEGEN_AGGREGATES_DUMP") {
                let _ = std::fs::write(format!("{dir}/rust.txt"), &rust);
                let _ = std::fs::write(format!("{dir}/vesper.txt"), &ves);
            }
            bad += 1;
            if bad <= 8 {
                eprintln!("decisions: {} differs at {d}", c.name);
            }
        }
    }
    eprintln!(
        "decisions: {bad} of {} worlds differ, {skipped} not encodable, {functions} functions, {lines} lines",
        prepared().len()
    );
    eprintln!("decisions by kind: {kinds:?}; {wide} wide-integer graphs left to Rust");
    assert!(
        kinds.get("enum").is_some_and(|n| *n > 0),
        "no graph keeps a scalar enum as words: {kinds:?}"
    );
    let expected = corpus().len() / step() / 2;
    assert!(
        prepared().len() >= expected,
        "only {} worlds of {expected} expected",
        prepared().len()
    );
    assert_eq!(bad, 0);
}

/// The scalar-enum rewrite (#1330): every graph the native path plans,
/// rewritten by the package's `scalar_enum_rewrite` entry, against the
/// graph `scalar_enums` returns (`aggregate_decisions::scalar_enum_rewrite`),
/// `Debug`-equal function by function. The entry's output is the indices
/// of the wide-integer graphs it leaves to Rust, then the program.
#[test]
fn rewrite_matches_rust() {
    let entry = package().entry("scalar_enum_rewrite", true);
    let (mut bad, mut skipped, mut rewritten, mut functions) = (0, 0, 0, 0);
    for c in prepared() {
        let Some((mut bytes, pool)) = ir_words::program_input(c.world, &c.program) else {
            skipped += 1;
            continue;
        };
        bytes.extend(facts(c.world, &c.program));
        let out = entry.words(&bytes);
        let nw = out[0] as usize;
        let wide: BTreeSet<usize> = out[1..1 + nw].iter().map(|x| *x as usize).collect();
        let theirs = ir_words::decode_program(&out[1 + nw..], &pool);
        for (i, (name, f)) in c.program.functions.iter().enumerate() {
            if wide.contains(&i) || c.world.functions.get(name).is_some_and(|src| src.fip) {
                continue;
            }
            functions += 1;
            let ours = vesper::native::aggregate_decisions::scalar_enum_rewrite(c.world, f);
            rewritten += ours.is_some() as usize;
            let ours = ours.unwrap_or_else(|| f.clone());
            let Some(g) = theirs.functions.get(name) else {
                bad += 1;
                eprintln!("rewrite: {} {name}: missing", c.name);
                continue;
            };
            let (a, b) = (format!("{ours:#?}"), format!("{g:#?}"));
            if a != b {
                bad += 1;
                if bad <= 8 {
                    let line = a
                        .lines()
                        .zip(b.lines())
                        .enumerate()
                        .find(|(_, (x, y))| x != y)
                        .map(|(i, (x, y))| format!("line {i}:\n    rust:   {x}\n    vesper: {y}"))
                        .unwrap_or_else(|| "lengths differ".into());
                    eprintln!("rewrite: {} {name} differs at {line}", c.name);
                }
            }
        }
    }
    eprintln!(
        "rewrite: {bad} of {functions} graphs differ ({rewritten} rewritten), {skipped} worlds not encodable"
    );
    assert!(rewritten > 0, "no graph was rewritten");
    assert_eq!(bad, 0);
}

/// The cost of the decisions over the corpus, best of 5: Rust's decision
/// functions, the input's encoding, and the package's entry (decoding the
/// input, the decisions and the text).
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn decisions_cost() {
    if !speed() {
        return;
    }
    let entry = package().entry("aggregate_decisions", true);
    let inputs: Vec<(&Prepared, Vec<u8>)> = prepared()
        .iter()
        .filter_map(|c| Some((c, input(c)?)))
        .collect();
    let best = |f: &mut dyn FnMut()| (0..5).map(|_| time(&mut *f)).min().unwrap();
    let rust = best(&mut || {
        for (c, _) in &inputs {
            std::hint::black_box(vesper::native::aggregate_decisions::program_decisions(
                c.world, &c.program,
            ));
        }
    });
    let encode = best(&mut || {
        for (c, _) in &inputs {
            std::hint::black_box(input(c));
        }
    });
    let vesper = best(&mut || {
        for (_, bytes) in &inputs {
            std::hint::black_box(entry.words(bytes));
        }
    });
    eprintln!(
        "decisions cost over {} worlds: Rust {rust:?}, encode {encode:?}, Vesper {vesper:?} ({:.2}x)",
        inputs.len(),
        vesper.as_secs_f64() / rust.as_secs_f64()
    );
}

/// The cost of the scalar-enum rewrite over the corpus, best of 5: Rust's
/// `scalar_enums` of every graph the native path plans, and the package's
/// `scalar_enum_rewrite` entry (decoding the input, the decisions, the
/// rewrite and the program's words); the input's encoding and the words'
/// decoding are timed apart.
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn rewrite_cost() {
    if !speed() {
        return;
    }
    let entry = package().entry("scalar_enum_rewrite", true);
    let inputs: Vec<(&Prepared, Vec<u8>)> = prepared()
        .iter()
        .filter_map(|c| Some((c, input(c)?)))
        .collect();
    let best = |f: &mut dyn FnMut()| (0..5).map(|_| time(&mut *f)).min().unwrap();
    let rust = best(&mut || {
        for (c, _) in &inputs {
            for (name, f) in &c.program.functions {
                if c.world.functions.get(name).is_some_and(|src| src.fip) {
                    continue;
                }
                std::hint::black_box(vesper::native::aggregate_decisions::scalar_enum_rewrite(
                    c.world, f,
                ));
            }
        }
    });
    let vesper = best(&mut || {
        for (_, bytes) in &inputs {
            std::hint::black_box(entry.words(bytes));
        }
    });
    eprintln!(
        "rewrite cost over {} worlds: Rust {rust:?}, Vesper {vesper:?} ({:.2}x)",
        inputs.len(),
        vesper.as_secs_f64() / rust.as_secs_f64()
    );
}
