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
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use check_words::{Enc, encodable, pass_word};
use ir_words::{Payload, Tables};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::ast::{Expr, ExprKind, Function, Statement, Type};
use vesper::check::World;
use vesper::ir::Program;
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

// ------------------------------------------------------------------ input

/// `ExprKind` tags, in declaration order (`E_*` of data.ves).
fn expr_kind(k: &ExprKind) -> u64 {
    match k {
        ExprKind::Literal(_) => 0,
        ExprKind::Name(_) => 1,
        ExprKind::Tuple(_) => 2,
        ExprKind::Struct { .. } => 3,
        ExprKind::Block { .. } => 4,
        ExprKind::Call { .. } => 5,
        ExprKind::Field(..) => 6,
        ExprKind::Index(..) => 7,
        ExprKind::Try(_) => 8,
        ExprKind::Unary(..) => 9,
        ExprKind::Binary(..) => 10,
        ExprKind::Assign(..) => 11,
        ExprKind::If { .. } => 12,
        ExprKind::Match { .. } => 13,
        ExprKind::Closure { .. } => 14,
        ExprKind::Handle { .. } => 15,
        ExprKind::Loop(_) => 16,
        ExprKind::For { .. } => 17,
        ExprKind::Range { .. } => 18,
        ExprKind::Break(_) => 19,
        ExprKind::Return(_) => 20,
        ExprKind::Continue => 21,
    }
}

/// The direct subexpressions of `e`, in evaluation order: a call's callee
/// then its arguments, a block's `let` values and expression statements
/// then its tail, a handler's state, body, return clause and operation
/// clauses. Patterns and types are not expressions.
fn children(e: &Expr) -> Vec<&Expr> {
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => vec![],
        ExprKind::Tuple(es) => es.iter().collect(),
        ExprKind::Struct { fields, .. } => fields.iter().map(|(_, e)| e).collect(),
        ExprKind::Block { statements, tail } => statements
            .iter()
            .map(|s| match s {
                Statement::Let { value, .. } => value,
                Statement::Expr(e) => e,
            })
            .chain(tail.as_deref())
            .collect(),
        ExprKind::Call { callee, args, .. } => std::iter::once(&**callee)
            .chain(args.iter().map(|a| &a.expr))
            .collect(),
        ExprKind::Unary(_, e) | ExprKind::Field(e, _) | ExprKind::Try(e) => vec![e],
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Assign(_, a, b) => {
            vec![a, b]
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => [&**condition, &**then]
            .into_iter()
            .chain(otherwise.as_deref())
            .collect(),
        ExprKind::Match { value, arms } => std::iter::once(&**value)
            .chain(arms.iter().map(|(_, e)| e))
            .collect(),
        ExprKind::Closure { body, .. } => vec![body],
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => state
            .iter()
            .map(|(_, e)| &**e)
            .chain([&**body, &ret.body])
            .chain(ops.iter().map(|c| &c.body))
            .collect(),
        k => k.control_children().unwrap_or_default(),
    }
}

/// Pre-order rows `(kind, start, end, symbol + 1, children, size)`.
fn put_expr(e: &Expr, enc: &mut Enc, rows: &mut Vec<[u64; 6]>) {
    let at = rows.len();
    let sym = match &e.kind {
        ExprKind::Name(n) | ExprKind::Field(_, n) => enc.name(n) + 1,
        _ => 0,
    };
    let kids = children(e);
    rows.push([
        expr_kind(&e.kind),
        e.span.start as u64,
        e.span.end as u64,
        sym,
        kids.len() as u64,
        0,
    ]);
    for c in kids {
        put_expr(c, enc, rows);
    }
    rows[at][5] = (rows.len() - at) as u64;
}

fn function_types(f: &Function) -> Vec<Type> {
    let mut out: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
    out.push(f.result.clone());
    out.push(Type::Row(f.effects.clone()));
    out
}

/// Whether every type the calls section writes fits the decoder.
fn calls_encodable(w: &World) -> bool {
    w.functions
        .values()
        .all(|f| function_types(f).iter().all(encodable))
        && w.calls.values().flatten().all(|(_, t)| encodable(t))
        && w.dyn_objects
            .params
            .values()
            .flatten()
            .all(|(_, t)| encodable(t))
        && w.dyn_objects.vtables.iter().all(|v| {
            encodable(&v.object)
                && encodable(&v.self_ty)
                && v.methods
                    .iter()
                    .flat_map(|(_, vs)| vs.values())
                    .all(encodable)
        })
}

/// The calls section (packages/vesper-codegen-calls/src/data.ves).
fn calls_section(e: &mut Enc, w: &World, tasks: &[(String, bool)]) -> Vec<u64> {
    let mut out = vec![vesper::trace_effects::active(w) as u64];
    let mut exprs = vec![];
    out.push(w.functions.len() as u64);
    for f in w.functions.values() {
        out.push(e.name(&f.name));
        out.push(f.fip as u64);
        out.extend([f.span.start as u64, f.span.end as u64]);
        out.push(f.generics.len() as u64);
        for g in &f.generics {
            out.push(e.name(&g.name));
        }
        out.push(f.params.len() as u64);
        for p in &f.params {
            out.push(e.name(&p.name));
            out.push(pass_word(p.pass));
            out.push(e.ty(&p.ty));
        }
        out.push(e.ty(&f.result));
        out.push(e.ty(&Type::Row(f.effects.clone())));
        match &f.body {
            Some(b) => {
                out.push(exprs.len() as u64 + 1);
                put_expr(b, e, &mut exprs);
            }
            None => out.push(0),
        }
    }
    out.push(exprs.len() as u64);
    for r in &exprs {
        out.extend(r);
    }
    let pairs = |e: &mut Enc, vs: &std::collections::BTreeMap<String, Type>, out: &mut Vec<u64>| {
        out.push(vs.len() as u64);
        for (n, t) in vs {
            out.push(e.name(n));
            out.push(e.ty(t));
        }
    };
    out.push(w.calls.len() as u64);
    for ((s, t), vs) in &w.calls {
        out.extend([*s as u64, *t as u64]);
        pairs(e, vs, &mut out);
    }
    let d = &w.dyn_objects;
    out.push(d.params.len() as u64);
    for (f, vs) in &d.params {
        out.push(e.name(f));
        pairs(e, vs, &mut out);
    }
    out.push(d.stubs.len() as u64);
    for s in d.stubs.keys() {
        out.push(e.name(s));
    }
    out.push(d.vtables.len() as u64);
    for v in &d.vtables {
        out.push(e.ty(&v.object));
        out.push(e.ty(&v.self_ty));
        out.push(v.methods.len() as u64);
        for (m, vs) in &v.methods {
            out.push(e.name(m));
            pairs(e, vs, &mut out);
        }
    }
    let slots = vesper::fip::caller_slot_functions(w);
    out.push(slots.len() as u64);
    for s in &slots {
        out.push(e.name(s));
    }
    out.push(tasks.len() as u64);
    for (entry, optimize) in tasks {
        out.push(e.name(entry));
        out.push(*optimize as u64);
    }
    out
}

/// `Payload::label` of tests/common/ir_words.rs: what `program_input`
/// interns for each pool entry before writing the program section.
fn pool_label(p: &Payload) -> String {
    match p {
        Payload::Seq(x) => format!("{x:?}"),
        Payload::Vector(x) => format!("{x:?}"),
        Payload::Tiled(x) => format!("{x:?}"),
        Payload::Fip(x) => format!("{x:?}"),
        Payload::Expr(_) => String::new(),
    }
}

/// The package input: check-data's names, types and world, the program
/// section of `program`, then the calls section. `None` when a type does
/// not fit the decoder.
fn calls_input(w: &World, program: &Program, tasks: &[(String, bool)]) -> Option<Vec<u8>> {
    if !ir_words::encodable(w, program) || !calls_encodable(w) {
        return None;
    }
    let mut t = Tables::of(program);
    let labels: Vec<String> = t.pool.items.iter().map(pool_label).collect();
    for l in &labels {
        t.enc.name(l);
    }
    let mut tail = t.section();
    let world = t.enc.world(w);
    tail.extend(calls_section(&mut t.enc, w, tasks));
    Some(t.enc.finish(&world, &tail))
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

/// The test files whose Vesper source strings exercise the decisions this
/// package ports: function-value instances, `dyn` vtables and boxed
/// objects, the Fail status and capability ABIs, register results, fip
/// caller slots, wide integers and floats, handle drops.
const SOURCE_TESTS: &[&str] = &[
    "tests/native_fail_status.rs",
    "tests/capability_verifiers.rs",
    "tests/dyn_trait.rs",
    "tests/dyn_box.rs",
    "tests/dyn_effects.rs",
    "tests/dyn_fn.rs",
    "tests/fip.rs",
    "tests/integer_widths.rs",
    "tests/wide_integers.rs",
    "tests/native_block_param_homes.rs",
    "tests/native_hoist_calls.rs",
    "tests/ffi_handles.rs",
    "tests/checked_arithmetic.rs",
    "tests/arrays.rs",
    "tests/capability_effects.rs",
    "tests/floats.rs",
    "tests/native_handlers.rs",
    "tests/vector_f32.rs",
    "tests/native_export_f32.rs",
    "tests/native_spans.rs",
];

/// The string literals of a Rust test file that look like Vesper sources
/// (tests/self_host_check_dyn.rs's extractor).
fn rust_string_literals(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'\'' {
            if bytes.get(i + 2) == Some(&b'\'') {
                i += 3;
                continue;
            }
            if bytes.get(i + 1) == Some(&b'\\')
                && let Some(end) = text[i + 2..].find('\'')
            {
                i += end + 3;
                continue;
            }
            i += 1;
            continue;
        }
        if c == b'r'
            && matches!(bytes.get(i + 1), Some(b'#' | b'"'))
            && !bytes[i.saturating_sub(1)].is_ascii_alphanumeric()
        {
            let mut j = i + 1;
            let mut hashes = 0;
            while bytes.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if bytes.get(j) == Some(&b'"') {
                let close = format!("\"{}", "#".repeat(hashes));
                if let Some(end) = text[j + 1..].find(&close) {
                    out.push(text[j + 1..j + 1 + end].to_owned());
                    i = j + 1 + end + close.len();
                    continue;
                }
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            let mut j = i + 1;
            let mut s = String::new();
            let mut ok = true;
            while j < bytes.len() && bytes[j] != b'"' {
                if bytes[j] == b'\\' {
                    let e = bytes.get(j + 1).copied().unwrap_or(b' ');
                    j += 2;
                    match e {
                        b'n' => s.push('\n'),
                        b't' => s.push('\t'),
                        b'r' => s.push('\r'),
                        b'0' => s.push('\0'),
                        b'\\' => s.push('\\'),
                        b'"' => s.push('"'),
                        b'\'' => s.push('\''),
                        b'\n' => {
                            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                                j += 1;
                            }
                        }
                        b'u' => {
                            let end = text[j..].find('}').map(|e| j + e);
                            match end.and_then(|e| {
                                u32::from_str_radix(&text[j + 1..e], 16)
                                    .ok()
                                    .map(|v| (e, v))
                            }) {
                                Some((e, v)) => {
                                    match char::from_u32(v) {
                                        Some(ch) => s.push(ch),
                                        None => ok = false,
                                    }
                                    j = e + 1;
                                }
                                None => ok = false,
                            }
                        }
                        b'x' => {
                            match u8::from_str_radix(&text[j..(j + 2).min(text.len())], 16) {
                                Ok(v) if v < 128 => s.push(v as char),
                                _ => ok = false,
                            }
                            j += 2;
                        }
                        _ => ok = false,
                    }
                } else {
                    let ch = text[j..].chars().next().unwrap();
                    s.push(ch);
                    j += ch.len_utf8();
                }
            }
            if ok {
                out.push(s);
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out.retain(|s| {
        s.len() >= 6
            && [
                "fn ", "struct ", "enum ", "trait ", "use ", "impl ", "effect ", "model ", "const ",
            ]
            .iter()
            .any(|k| s.contains(k))
    });
    out
}

/// Every distinct source string of [`SOURCE_TESTS`] that parses and checks
/// clean, with its world.
fn source_worlds() -> Vec<(String, World)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = vec![];
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    for file in SOURCE_TESTS.iter().copied() {
        let Ok(text) = std::fs::read_to_string(repo().join(file)) else {
            continue;
        };
        // A `format!` template's braces are doubled; its de-doubled text is
        // the program when it has no placeholders.
        let literals = rust_string_literals(&text).into_iter().flat_map(|s| {
            let plain = s
                .contains("{{")
                .then(|| s.replace("{{", "{").replace("}}", "}"));
            std::iter::once(s).chain(plain)
        });
        for (i, src) in literals.enumerate() {
            if !seen.insert(src.clone()) {
                continue;
            }
            let r = std::panic::catch_unwind(|| {
                let parsed = vesper::parser::parse(&src);
                if !parsed.diagnostics.is_empty() {
                    return None;
                }
                let checked = vesper::check::check(&parsed.module);
                checked.diagnostics.is_empty().then_some(checked.world)
            });
            if let Ok(Some(w)) = r {
                out.push((format!("{file}#{i}"), w));
            }
        }
    }
    std::panic::set_hook(quiet);
    out
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
