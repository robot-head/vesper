//! Self-hosting SH4 lane 15 (#1289): `@vesper/wasm-backend` against the
//! Rust wasm backend (src/wasm_backend).
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;
#[path = "common/wasm_words.rs"]
mod wasm_words;

use check_harness::*;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use vesper::check::World;
use vesper::wasm_backend::{self, EffectLowering, WasmError, WasmOptions};
use wasm_words::*;

/// One program the backend is asked to compile.
pub struct WasmCase {
    pub name: String,
    pub world: World,
}

/// The program literals of tests/wasm_backend.rs (every string literal
/// that defines `main` and checks), then every corpus world with a `main`.
pub fn cases() -> &'static [WasmCase] {
    static C: OnceLock<Vec<WasmCase>> = OnceLock::new();
    C.get_or_init(|| {
        let mut out = vec![];
        let src = std::fs::read_to_string(repo().join("tests/wasm_backend.rs")).unwrap();
        for (i, lit) in string_literals(&src).into_iter().enumerate() {
            if !lit.contains("fn main") {
                continue;
            }
            let p = vesper::parser::parse(&lit);
            if !p.diagnostics.is_empty() {
                continue;
            }
            let c = vesper::check::check(&p.module);
            if c.diagnostics.is_empty() {
                out.push(WasmCase {
                    name: format!("tests/wasm_backend.rs#{i}"),
                    world: c.world,
                });
            }
        }
        for c in corpus() {
            if c.world.functions.contains_key("main") {
                out.push(WasmCase {
                    name: c.name.clone(),
                    world: c.world.clone(),
                });
            }
        }
        out
    })
}

/// The `"..."` and `r#"..."#` literals of a Rust source, unescaped.
fn string_literals(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i] == b'r' && b.get(i + 1) == Some(&b'#') && b.get(i + 2) == Some(&b'"') {
            let start = i + 3;
            let end = src[start..]
                .find("\"#")
                .map(|e| start + e)
                .unwrap_or(b.len());
            out.push(src[start..end].to_string());
            i = end + 2;
            continue;
        }
        if b[i] == b'\'' && b.get(i + 2) == Some(&b'\'') {
            i += 3;
            continue;
        }
        if b[i] == b'"' {
            let mut s = String::new();
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                    match b.get(i) {
                        Some(b'n') => s.push('\n'),
                        Some(b't') => s.push('\t'),
                        Some(b'\\') => s.push('\\'),
                        Some(b'"') => s.push('"'),
                        Some(b'\n') => {
                            while i + 1 < b.len() && (b[i + 1] == b' ' || b[i + 1] == b'\n') {
                                i += 1;
                            }
                        }
                        Some(c) => s.push(*c as char),
                        None => {}
                    }
                    i += 1;
                    continue;
                }
                let ch = src[i..].chars().next().unwrap();
                s.push(ch);
                i += ch.len_utf8();
            }
            out.push(s);
            i += 1;
            continue;
        }
        i += 1;
    }
    out
}

pub fn configs() -> [WasmOptions; 4] {
    let mk = |optimize, effects| WasmOptions {
        optimize,
        effects,
        ..WasmOptions::default()
    };
    [
        mk(true, EffectLowering::StackSwitching),
        mk(false, EffectLowering::StackSwitching),
        mk(true, EffectLowering::Portable),
        mk(false, EffectLowering::Portable),
    ]
}

/// Rust's refusal of `loop`, `for`, `break` and `return`, which the port
/// lowers (structured control flow over the cyclic block graph).
fn loop_refusal(e: &WasmError) -> bool {
    matches!(e, WasmError::Unsupported { detail, .. } if detail == vesper::loops::WASM_REFUSAL)
}

/// `main` on the evaluator: its value's display, or "trap".
fn evaluate(world: &World) -> String {
    let mut r = vesper::runtime::Runtime::new(world);
    r.fuel = 10_000_000;
    match r.run("main", vec![]) {
        Ok(v) => v.to_string(),
        Err(_) => "trap".into(),
    }
}

/// A `run_main` result (`[S64(3)]`, `[Bool(true)]`, "trap") against the
/// evaluator's display of a scalar (`3`, `true`, "trap").
fn same_run(wasm: &str, evaluator: &str) -> bool {
    if wasm == "trap" || evaluator == "trap" {
        return wasm == evaluator;
    }
    if wasm == "[]" {
        return evaluator == "()";
    }
    let inner = wasm
        .strip_prefix('[')
        .and_then(|w| w.strip_suffix(']'))
        .and_then(|w| w.split_once('('))
        .and_then(|(_, v)| v.strip_suffix(')'));
    match inner {
        Some(v) => v == evaluator,
        // Not a scalar: only a trap on both sides counts as equal here.
        None => false,
    }
}

fn error_class(e: &WasmError) -> String {
    match e {
        WasmError::Unsupported { construct, .. } => format!("Unsupported({construct:?})"),
        WasmError::Entry(_) => "Entry".into(),
        WasmError::Limit(_) => "Limit".into(),
        WasmError::Invalid(_) => "Invalid".into(),
        WasmError::Encode(_) => "Encode".into(),
    }
}

/// What the Rust backend accepts over `cases()` under each option set (the
/// report's census; about 5 min in a debug build).
#[test]
#[ignore]
fn rust_acceptance_census() {
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    for c in cases() {
        for (k, o) in configs().iter().enumerate() {
            let r = std::panic::catch_unwind(|| wasm_backend::compile(&c.world, "main", o));
            let key = match r {
                Ok(Ok(_)) => "ok".to_string(),
                Ok(Err(e)) => error_class(&e),
                Err(_) => "panic".into(),
            };
            if k == 0 && key == "ok" {
                eprintln!("ok: {}", c.name);
            }
            *classes.entry(format!("{k} {key}")).or_default() += 1;
        }
    }
    std::panic::set_hook(quiet);
    eprintln!("{} cases", cases().len());
    for (k, n) in classes {
        eprintln!("{k}: {n}");
    }
}

// ------------------------------------------------------------ differential

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-wasm-backend"))
}

fn engine(stack_switching: bool) -> wasmtime::Engine {
    let mut c = wasmtime::Config::new();
    c.wasm_component_model(true);
    c.consume_fuel(true);
    if stack_switching {
        c.wasm_function_references(true);
        c.wasm_exceptions(true);
        c.wasm_stack_switching(true);
    }
    wasmtime::Engine::new(&c).expect("engine")
}

/// Run a nullary command `main` under Wasmtime; its result, or the trap.
pub fn run_main(component: &[u8], stack_switching: bool) -> String {
    use wasmtime::component::{Component, Linker, Val};
    let engine = engine(stack_switching);
    let component = match Component::new(&engine, component) {
        Ok(c) => c,
        Err(e) => return format!("load: {e}"),
    };
    let linker = Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    store.set_fuel(100_000_000).unwrap();
    let instance = match linker.instantiate(&mut store, &component) {
        Ok(i) => i,
        Err(e) => return format!("instantiate: {e}"),
    };
    let iface = instance
        .get_export_index(&mut store, None, wasm_backend::ENTRY_INTERFACE)
        .expect("entry interface");
    let main = instance
        .get_export_index(&mut store, Some(&iface), "main")
        .expect("main export");
    let func = instance.get_func(&mut store, main).expect("main func");
    let mut results = vec![Val::Bool(false); func.ty(&store).results().len()];
    match func.call(&mut store, &[], &mut results) {
        Ok(()) => format!("{results:?}"),
        Err(_) => "trap".into(),
    }
}

#[derive(Default, Debug)]
pub struct Tally {
    pub compared: usize,
    pub identical: usize,
    pub errors_agree: usize,
    pub not_ported: usize,
    pub not_encodable: usize,
    pub ran: usize,
    /// Programs with loops, which Rust refuses: the port compiles them
    /// (`loops`, and `loops_ran` of them ran like the evaluator) or refuses
    /// a later construct (`loops_refused`).
    pub loops: usize,
    pub loops_ran: usize,
    pub loops_refused: usize,
    pub diffs: Vec<String>,
}

/// Every case under every configuration: Rust `compile` against the port.
pub fn differential_run(step: usize, execute: bool) -> Tally {
    let f = package().entry("wasm_backend_compile", true);
    let mut t = Tally::default();
    for c in cases().iter().step_by(step) {
        for (k, o) in configs().iter().enumerate() {
            let Some(program) = backend_program(&c.world, o) else {
                continue;
            };
            let Some(input) = wasm_input(&c.world, &program, "main", o) else {
                t.not_encodable += 1;
                continue;
            };
            let rust = wasm_backend::compile(&c.world, "main", o);
            let port = outcome(&f.words(&input));
            t.compared += 1;
            let tag = format!("{} [{k}]", c.name);
            match (&rust, &port) {
                (_, Outcome::NotPorted(_)) => t.not_ported += 1,
                (Err(e), Outcome::Module { core, wit, .. }) if loop_refusal(e) => {
                    t.loops += 1;
                    if execute && k == 0 && c.world.functions["main"].params.is_empty() {
                        let component = wrap(core.clone(), wit, o.world);
                        let ours = run_main(&component, false);
                        let theirs = evaluate(&c.world);
                        if !same_run(&ours, &theirs) {
                            t.diffs
                                .push(format!("{tag}: run {ours} vs evaluator {theirs}"));
                        }
                        t.loops_ran += 1;
                    }
                }
                (Err(e), Outcome::Error(..)) if loop_refusal(e) => t.loops_refused += 1,
                (
                    Ok(a),
                    Outcome::Module {
                        core,
                        wit,
                        functions,
                    },
                ) => {
                    if a.functions != *functions {
                        t.diffs
                            .push(format!("{tag}: {} vs {functions} functions", a.functions));
                    } else if &a.core != core {
                        t.diffs.push(format!(
                            "{tag}: core differs ({} vs {} bytes)",
                            a.core.len(),
                            core.len()
                        ));
                    } else if &a.wit != wit {
                        t.diffs
                            .push(format!("{tag}: WIT differs\n{}\n---\n{wit}", a.wit));
                    } else {
                        let component = wrap(core.clone(), wit, o.world);
                        if component != a.component {
                            t.diffs.push(format!("{tag}: component differs"));
                            continue;
                        }
                        t.identical += 1;
                        if execute
                            && k == 0
                            && c.world.functions["main"].params.is_empty()
                            && !a.stack_switching
                        {
                            let ours = run_main(&component, false);
                            let theirs = run_main(&a.component, false);
                            if ours != theirs {
                                t.diffs.push(format!("{tag}: run {ours} vs {theirs}"));
                            }
                            t.ran += 1;
                        }
                    }
                }
                (Err(e), Outcome::Error(class, _)) if &error_class(e) == class => {
                    t.errors_agree += 1
                }
                _ => t.diffs.push(format!(
                    "{tag}: rust {:?} port {:?}",
                    rust.as_ref().map(|_| "module").map_err(error_class),
                    match &port {
                        Outcome::Module { core, .. } => format!("module of {} bytes", core.len()),
                        o => format!("{o:?}"),
                    }
                )),
            }
        }
    }
    t
}

/// Every `n`-th case (`WASM_PORT_STEP`, default 1: every case takes about
/// 10 min in a debug build, most of it compiling the package).
fn step() -> usize {
    std::env::var("WASM_PORT_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(1)
}

#[test]
fn port_matches_rust() {
    let t = differential_run(step(), true);
    eprintln!(
        "compared {} identical {} errors agree {} not ported {} not encodable {} ran {}; \
         loops {} (ran {}, refused later {})",
        t.compared,
        t.identical,
        t.errors_agree,
        t.not_ported,
        t.not_encodable,
        t.ran,
        t.loops,
        t.loops_ran,
        t.loops_refused
    );
    for d in t.diffs.iter().take(40) {
        eprintln!("DIFF {d}");
    }
    assert!(t.diffs.is_empty(), "{} differences", t.diffs.len());
}

/// ABBA timing over the cases the port compiles to a module, under the
/// default options. The Rust side is `compile` less the IR it is handed
/// (`lower_world` + `optimize`) and the wit-component wrap, which the port
/// leaves to Rust; the port side is the JIT entry on the encoded input.
/// Run with `CHECK_HARNESS_SPEED=1 --ignored` in a release build.
#[test]
#[ignore]
fn timing() {
    if !speed() {
        return;
    }
    let f = package().entry("wasm_backend_compile", true);
    let o = WasmOptions::default();
    let mut work = vec![];
    for c in cases() {
        let Some(program) = backend_program(&c.world, &o) else {
            continue;
        };
        let Some(input) = wasm_input(&c.world, &program, "main", &o) else {
            continue;
        };
        if let (Ok(a), Outcome::Module { .. }) = (
            wasm_backend::compile(&c.world, "main", &o),
            outcome(&f.words(&input)),
        ) {
            work.push((c, input, a));
        }
    }
    let reps = 5;
    let mut rust = || {
        let mut d = std::time::Duration::ZERO;
        for (c, _, a) in &work {
            for _ in 0..reps {
                let total = time(|| {
                    std::hint::black_box(wasm_backend::compile(&c.world, "main", &o).unwrap());
                });
                let front = time(|| {
                    std::hint::black_box(backend_program(&c.world, &o));
                });
                let wrap_t = time(|| {
                    std::hint::black_box(wrap(a.core.clone(), &a.wit, o.world));
                });
                d += total.saturating_sub(front + wrap_t);
            }
        }
        d
    };
    let mut port = || {
        time(|| {
            for (_, input, _) in &work {
                for _ in 0..reps {
                    std::hint::black_box(f.words(input));
                }
            }
        })
    };
    let encode = time(|| {
        for (c, _, _) in &work {
            for _ in 0..reps {
                let p = backend_program(&c.world, &o).unwrap();
                std::hint::black_box(wasm_input(&c.world, &p, "main", &o));
            }
        }
    });
    let front = time(|| {
        for (c, _, _) in &work {
            for _ in 0..reps {
                std::hint::black_box(backend_program(&c.world, &o));
            }
        }
    });
    eprintln!(
        "{} modules; encoding the input {:?}",
        work.len(),
        encode.saturating_sub(front)
    );
    abba("wasm backend", &mut rust, &mut port);
}

/// The hybrid `wasm` hook's answer (`compile_with`, as crates/vesper-hybrid
/// installs it) is Rust's whole artifact, field for field, wherever it
/// answers; it declines exactly the errors and the constructs not ported.
#[test]
fn hook_artifact_matches_rust() {
    let f = package().entry("wasm_backend_compile", true);
    let (mut answered, mut declined) = (0, 0);
    for c in cases().iter().step_by(3) {
        for o in configs().iter() {
            let rust = wasm_backend::compile(&c.world, "main", o);
            let port = compile_with(&c.world, "main", o, |input| f.words(input));
            match (&rust, port) {
                (Ok(a), Some(b)) => {
                    answered += 1;
                    assert_eq!(format!("{a:?}"), format!("{b:?}"), "{}", c.name);
                }
                (_, None) => declined += 1,
                // Loops: Rust refuses them, the port lowers them
                // (`loops_run_like_the_evaluator`).
                (Err(e), Some(_)) if loop_refusal(e) => answered += 1,
                (Err(e), Some(_)) => panic!("{}: the hook answered where Rust fails: {e}", c.name),
            }
        }
    }
    eprintln!("hook answered {answered}, declined {declined}");
    assert!(answered > 100, "the hook answered only {answered}");
}

/// Loop programs, which Rust refuses: the port's module (structured control
/// flow over the cyclic block graph) runs under Wasmtime like the evaluator,
/// under every option set.
const LOOP_PROGRAMS: &[&str] = &[
    // `loop` with a counter and `break`.
    "fn main() -> i64 { let mut i: i64 = 0; let mut s: i64 = 0; \
     loop { if i == 10 { break; } s = wrapping_add(s, i); i = wrapping_add(i, 1); } s }",
    // `for` over exclusive and inclusive ranges.
    "fn main() -> i64 { let mut s: i64 = 0; for i in 0..5 { s = wrapping_add(s, i); } \
     for j in 1..=4 { s = wrapping_mul(s, j); } s }",
    // Nested `for` with an inner `break`.
    "fn tri(n: i64) -> i64 { let mut c: i64 = 0; for i in 0..n { for j in 0..n { \
     if j > i { break; } c = wrapping_add(c, 1); } } c } fn main() -> i64 { tri(9) }",
    // Nested `loop`s that leave with values.
    "fn main() -> i64 { let mut a: i64 = 0; let found = loop { let inner = loop { \
     a = wrapping_add(a, 3); if a % 7 == 0 { break a; } }; if inner > 50 { break inner; } }; \
     wrapping_add(found, a) }",
    // `continue` in a `for` and in a `loop`.
    "fn main() -> i64 { let mut s: i64 = 0; for i in 0..20 { if i % 3 == 0 { continue; } \
     s = wrapping_add(s, i); } let mut k: i64 = 0; loop { k = wrapping_add(k, 1); \
     if k < 5 { continue; } break; } wrapping_add(s, k) }",
    // `return` from inside a loop, not in tail position.
    "fn find(n: i64) -> i64 { for i in 0..100 { if wrapping_mul(i, i) > n { return i; } } -1 } \
     fn main() -> i64 { wrapping_add(find(50), find(100000)) }",
    // Early `return` before a loop, and a `match` in the body.
    "enum Step { Up, Down, Stay } \
     fn step(i: i64) -> Step { if i % 3 == 0 { Step::Up } else if i % 3 == 1 { Step::Down } else { Step::Stay } } \
     fn walk(n: i64) -> i64 { if n < 0 { return 0; } let mut x: i64 = 0; for i in 0..n { \
     x = match step(i) { Step::Up => wrapping_add(x, 2), Step::Down => wrapping_sub(x, 1), Step::Stay => x }; } x } \
     fn main() -> i64 { wrapping_add(walk(30), walk(-1)) }",
    // A struct updated across iterations, and a `while`-style loop.
    "struct P { a: i64, b: i64 } fn main() -> i64 { let mut p = P { a: 0, b: 1 }; \
     let mut n: i64 = 0; loop { if n >= 20 { break; } let t = wrapping_add(p.a, p.b); \
     p = P { a: p.b, b: t }; n = wrapping_add(n, 1); } p.a }",
    // A loop whose `break` carries a bool.
    "fn main() -> bool { let mut i: i64 = 1; loop { i = wrapping_mul(i, 2); if i > 1000 { break i % 2 == 0; } } }",
    // Many iterations, a loop in a callee, and a loop-carried call.
    "fn collatz(n0: i64) -> i64 { let mut n = n0; let mut k: i64 = 0; loop { if n == 1 { break; } \
     if n % 2 == 0 { n = n / 2; } else { n = wrapping_add(wrapping_mul(n, 3), 1); } k = wrapping_add(k, 1); } k } \
     fn main() -> i64 { let mut best: i64 = 0; for i in 1..300 { let c = collatz(i); if c > best { best = c; } } best }",
];

#[test]
fn loops_run_like_the_evaluator() {
    let f = package().entry("wasm_backend_compile", true);
    let mut ran = 0;
    for (i, src) in LOOP_PROGRAMS.iter().enumerate() {
        let p = vesper::parser::parse(src);
        assert!(p.diagnostics.is_empty(), "#{i}: {:?}", p.diagnostics);
        let c = vesper::check::check(&p.module);
        assert!(c.diagnostics.is_empty(), "#{i}: {:?}", c.diagnostics);
        let expected = evaluate(&c.world);
        for (k, o) in configs().iter().enumerate() {
            let rust = wasm_backend::compile(&c.world, "main", o);
            assert!(
                matches!(&rust, Err(e) if loop_refusal(e)),
                "#{i} [{k}]: Rust now accepts loops; compare byte for byte instead"
            );
            let program = backend_program(&c.world, o).expect("program");
            let input = wasm_input(&c.world, &program, "main", o).expect("input");
            let (core, wit) = match outcome(&f.words(&input)) {
                Outcome::Module { core, wit, .. } => (core, wit),
                o => panic!("#{i} [{k}]: the port refused {src}: {o:?}"),
            };
            let component = wrap(core, &wit, o.world);
            let got = run_main(&component, false);
            assert!(
                same_run(&got, &expected),
                "#{i} [{k}]: wasm {got} vs evaluator {expected}\n{src}"
            );
            ran += 1;
        }
    }
    eprintln!("{ran} loop compilations ran like the evaluator");
}
