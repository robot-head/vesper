//! Stage 2 retirement (#1397), claim #2053: `@vesper/run`
//! (packages/vesper-run) against the Rust evaluator `Runtime::run`, the
//! machine behind `vesper run`.
//!
//! Every corpus world with a runnable `main` (no parameters, no effects:
//! the driver's `run` entry checks), and the programs of [`PROGRAMS`], run
//! under each of [`CONFIGS`] (the optimizer on and off, the default fuel
//! and small fuel limits, so fuel exhaustion lands on every kind of step).
//! The Vesper machine must print the same value or fail with the same
//! error as Rust, or decline the program (a feature a later slice ports);
//! `run_matches_rust` fails on any difference and reports what the
//! declined programs lack.
//!
//! `run_cost` (ignored; `CHECK_HARNESS_SPEED=1`): ABBA of the Vesper
//! machine (encoding included) against `Runtime::run` (lowering and
//! optimizing included) on the programs it runs.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;
#[path = "common/run_words.rs"]
mod run_words;

use check_harness::*;
use run_words::{Answer, decode, program, run_input, world_declines};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use vesper::check::World;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-run"))
}

fn entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| package().entry("run_str", true))
}

/// (optimize, fuel) of each run.
const CONFIGS: &[(bool, u64)] = &[
    (true, 10_000_000),
    (false, 10_000_000),
    (true, 1),
    (true, 3),
    (true, 17),
    (false, 40),
];

/// Programs for the machine's paths the corpus reaches rarely.
const PROGRAMS: &[(&str, &str)] = &[
    (
        "arith",
        "fn main() -> i64 { let a: i64 = 7; let b = wrapping_sub(wrapping_mul(a, 6), 2); b }",
    ),
    (
        "kinds",
        "fn main() -> (u8, i8, u16, i32, u64, bool) { \
         let a: u8 = 250; let b: u8 = 10; let c: i8 = -100; let d: i8 = 100; \
         let e: u16 = 65535; let f: i32 = -2147483648; let g: u64 = 18446744073709551615; \
         let h: u8 = 3; let i: u8 = 4; \
         (wrapping_add(a, b), wrapping_sub(c, d), e, f, g, h < i) }",
    ),
    (
        "bits",
        "fn main() -> (u32, u8, u8, i32, u8, i64) { \
         let one: u32 = 1; let x: u8 = 129; let y: u8 = 1; let z: u8 = 9; let five: i32 = 5; \
         let m: u8 = 12; let n: i64 = -8; \
         (bit_shl(one, 31), rotate_left(x, 1), bit_shl(y, z), ~five, m & 10 | 1 ^ 3, bit_shr(n, 1)) }",
    ),
    (
        "conversions",
        "fn main() -> (u8, i16, u64, u8) { \
         let a: i8 = -3; let b: u32 = 7; let c: i64 = 300; \
         (wrapping_u8(c), widen_i16(a), widen_u64(b), wrapping_u8(-1)) }",
    ),
    (
        "data",
        "enum Shape { Circle(i64), Rect { w: i64, h: i64 }, Dot } \
         fn area(s: Shape) -> i64 { match s { Shape::Circle(r) => wrapping_mul(3, wrapping_mul(r, r)), \
         Shape::Rect { w, h } => wrapping_mul(w, h), Shape::Dot => 0 } } \
         fn main() -> (i64, i64, i64, Shape, Shape, Shape) { \
         (area(Shape::Circle(2)), area(Shape::Rect { w: 3, h: 4 }), area(Shape::Dot), \
         Shape::Rect { w: 1, h: 2 }, Shape::Circle(5), Shape::Dot) }",
    ),
    (
        "struct_fields",
        "struct P { y: i64, x: i64, z: bool } \
         fn main() -> P { let mut p = P { y: 1, x: 2, z: false }; p.x = wrapping_add(p.x, 40); \
         p.z = true; p }",
    ),
    (
        "inout",
        "fn bump(inout n: i64) { n = wrapping_add(n, 1); } \
         struct C { a: i64, b: (i64, i64) } \
         fn main() -> C { let mut c = C { a: 1, b: (2, 3) }; bump(inout c.a); \
         bump(inout c.a); c }",
    ),
    (
        "loops",
        "fn main() -> i64 { let mut s = 0; for i in 0..100 { if bit_and(i, 3) == 0 { continue; } \
         s = wrapping_add(s, i); if s > 2000 { break; } } s }",
    ),
    (
        // A loop body's pattern rebinds each iteration (the environment
        // must stay bounded, as Rust's map does).
        "long_loop",
        "fn main() -> i64 { let mut s = 0; for i in 0..200000 { let j = wrapping_mul(i, 3); \
         s = wrapping_add(s, j); } s }",
    ),
    (
        "handlers",
        "fn d(a: u64, b: u64) -> u64 { handle Fail[RuntimeError] in { a / b } with { \
         return v => wrapping_add(v, 1), raise(e, k) => 0 } } \
         fn o(a: u8) -> u8 { handle Fail[ArithOverflow] in { a + 200 } with { \
         return v => v, raise(e, k) => 7 } } \
         fn main() -> (u64, u64, u8, u8) { (d(7, 2), d(7, 0), o(3), o(100)) }",
    ),
    (
        "nested_handlers",
        "fn inner(a: u64, b: u64) -> u64 eff [Fail[RuntimeError]] { a % b } \
         fn mid(a: u64, b: u64) -> u64 eff [Fail[RuntimeError]] { \
         handle Fail[ArithOverflow] in { let x: u8 = 255; let y = x + 1; wrapping_add(inner(a, b), widen_u64(y)) } \
         with { return v => v, raise(e, k) => 5 } } \
         fn main() -> (u64, u64) { (handle Fail[RuntimeError] in { mid(9, 0) } with { return v => v, raise(e, k) => 3 }, \
         handle Fail[RuntimeError] in { mid(9, 4) } with { return v => v, raise(e, k) => 3 }) }",
    ),
    (
        "try_raise",
        "fn f(x: i64) -> Result[i64, String] { if x > 0 { Result::Ok(x) } else { Result::Err(\"neg\") } } \
         fn g(x: i64) -> i64 eff [Fail[String]] { let v = f(x)?; wrapping_mul(v, 2) } \
         fn h() -> i64 eff [Fail[String]] { Fail::raise(\"boom\") } \
         fn main() -> (i64, i64, i64) { \
         (handle Fail[String] in { g(4) } with { return v => v, raise(e, k) => 99 }, \
         handle Fail[String] in { g(-1) } with { return v => v, raise(e, k) => 99 }, \
         handle Fail[String] in { h() } with { return v => v, raise(e, k) => 98 }) }",
    ),
    (
        // An effectful function value binds the incoming capability
        // (`bind_callable_evidence`): the outer handler answers.
        "bound_evidence",
        "effect Ask { fn ask() -> i64; } fn ask() -> i64 eff [Ask] { Ask::ask() } \
         fn apply(sink f: fn() -> i64 eff [Ask]) -> i64 eff [Ask] { handle inner: Ask in { f() } \
         with { return x => x, ask(k) => 2, } } \
         fn main() -> i64 { let f = ask; handle outer: Ask in { apply(sink f) } \
         with { return x => x, ask(k) => 1, } }",
    ),
    (
        // A clause writes an `inout` operation argument before aborting.
        "inout_operation",
        "effect Stop { fn stop(inout x: i64) -> i64; } fn main() -> i64 { let mut x = 0; \
         let r = handle Stop in { x += 5; Stop::stop(inout x); x = wrapping_add(x, 100); 1 } \
         with { return v => v, stop(x, k) => { x = wrapping_add(x, 30); 7 }, }; \
         wrapping_add(wrapping_mul(r, 1000), x) }",
    ),
    (
        "aborting_operation",
        "effect Stop { fn stop() -> i64; } fn main() -> i64 { let mut x = 0; \
         let r = handle Stop in { x += 5; Stop::stop(); x += 100; 1 } \
         with { return v => v, stop(k) => 7, }; wrapping_add(wrapping_mul(r, 1000), x) }",
    ),
    (
        "resumed_continuations",
        "effect Ask { fn ask(x: i64) -> i64; } \
         fn f(n: i64) -> i64 eff [Ask] { let mut s = 0; for i in 0..n { s = wrapping_add(s, Ask::ask(i)); } s } \
         fn main() -> (i64, i64) { (handle Ask in { f(10) } with { return v => wrapping_mul(v, 2), \
         ask(x, k) => k(wrapping_mul(x, x)) }, handle Ask in { wrapping_add(Ask::ask(1), 100) } \
         with { return v => v, ask(x, k) => wrapping_add(k(x), 1000) }) }",
    ),
    (
        "aborting_loop",
        "effect Stop { fn stop() -> i64; } fn main() -> i64 { let mut s = 0; for i in 0..2000 { \
         s = wrapping_add(s, handle Stop in { wrapping_add(Stop::stop(), i) } \
         with { return v => v, stop(k) => 2, }); } s }",
    ),
    (
        "state_handlers",
        "linear effect Counter { fn next() -> i64; } \
         fn main() -> (i64, (i64, i64)) { (handle Counter state c = 0 in { \
         wrapping_add(Counter::next(), Counter::next()) } with { \
         return (v, c) => wrapping_add(wrapping_mul(v, 10), c), next(c, k) => k(c, wrapping_add(c, 1)), }, \
         handle Counter state c = 5 in { Counter::next() } with { \
         return p => p, next(c, k) => k(c, wrapping_mul(c, 2)), }) }",
    ),
    (
        "unhandled_in_clause",
        "fn d(a: u64, b: u64) -> u64 eff [Fail[RuntimeError]] { a / b } \
         fn main() -> u64 { handle Fail[RuntimeError] in { \
         handle Fail[RuntimeError] in { d(1, 0) } with { return v => v, raise(e, k) => d(2, 0) } \
         } with { return v => v, raise(e, k) => 11 } }",
    ),
    (
        "recursion",
        "fn fib(n: i64) -> i64 { if n < 2 { n } else { wrapping_add(fib(wrapping_sub(n, 1)), \
         fib(wrapping_sub(n, 2))) } } fn main() -> i64 { fib(20) }",
    ),
    (
        "strings",
        "fn main() -> (String, bool, bool, bool) { \
         (\"a\\\"b\\\\c\\n\\td\\r\", \"abc\" < \"abd\", \"x\" == \"x\", \"b\" >= \"ab\") }",
    ),
    (
        "unit_and_tuples",
        "fn f(t: (i64, (bool, i64))) -> i64 { let (a, (b, c)) = t; if b { a } else { c } } \
         fn main() -> ((), i64, i64, (i64,)) { ((), f((1, (true, 2))), f((1, (false, 2))), (4,)) }",
    ),
    (
        "function_values",
        "fn inc(n: i64) -> i64 { wrapping_add(n, 1) } fn ap(f: fn(i64) -> i64, n: i64) -> i64 { f(n) } \
         fn main() -> (i64, fn(i64) -> i64) { (ap(inc, 41), inc) }",
    ),
    (
        "match_literals",
        "fn name(n: u8) -> i64 { match n { 0 => 10, 1 => 20, 255 => 30, _ => 40 } } \
         fn main() -> (i64, i64, i64, i64) { (name(0), name(1), name(255), name(7)) }",
    ),
    (
        "unbounded",
        "fn spin(n: i64) -> i64 { spin(wrapping_add(n, 1)) } fn main() -> i64 { spin(0) }",
    ),
    (
        "options",
        "fn pick(b: bool) -> Option[i64] { if b { Option::Some(3) } else { Option::None } } \
         fn main() -> (Option[i64], Option[i64], bool) { (pick(true), pick(false), pick(true) == pick(true)) }",
    ),
];

/// One runnable world.
struct Case {
    name: String,
    world: &'static World,
}

/// Whether the driver's `run` entry checks accept `main`
/// (`run_entry_*` in modules/driver/src/lib.ves).
fn runnable(world: &World) -> bool {
    world.functions.get("main").is_some_and(|f| {
        f.params.is_empty() && f.effects.instances.is_empty() && f.effects.tail.is_none()
    })
}

fn cases() -> &'static [Case] {
    static C: OnceLock<Vec<Case>> = OnceLock::new();
    C.get_or_init(|| {
        let mut out = vec![];
        for c in corpus() {
            if runnable(&c.world) {
                out.push(Case {
                    name: c.name.clone(),
                    world: &c.world,
                });
            }
        }
        for (name, source) in PROGRAMS {
            let parsed = vesper::parser::parse(source);
            if !parsed.diagnostics.is_empty() {
                eprintln!("program {name} does not parse: {:?}", parsed.diagnostics);
                continue;
            }
            let checked = vesper::check::check(&parsed.module);
            if !checked.diagnostics.is_empty() {
                // A program the checker rejects says nothing about the
                // machine; report it so it gets fixed.
                eprintln!("program {name} does not check: {:?}", checked.diagnostics);
                continue;
            }
            if !runnable(&checked.world) {
                eprintln!("program {name}: main is not runnable");
                continue;
            }
            out.push(Case {
                name: format!("program {name}"),
                world: Box::leak(Box::new(checked.world)),
            });
        }
        out
    })
}

fn rust_answer(world: &World, optimize: bool, fuel: u64) -> Answer {
    let mut runtime = vesper::runtime::Runtime::new(world);
    runtime.fuel = fuel;
    runtime.optimize = optimize;
    match runtime.run("main", vec![]) {
        Ok(v) => Answer::Value(v.to_string()),
        Err(e) => Answer::Error(e),
    }
}

fn vesper_answer(world: &World, optimize: bool, fuel: u64) -> Answer {
    if let Some(why) = world_declines(world) {
        return Answer::Declined(why.into());
    }
    let program = match program(world, optimize) {
        Ok(p) => p,
        Err(e) => return Answer::Error(e),
    };
    let Some(input) = run_input(world, &program, "main", fuel) else {
        return Answer::Declined("a type check-data cannot encode".into());
    };
    decode(&entry().words(&input))
}

#[test]
fn run_matches_rust() {
    let mut ran = 0usize;
    let mut declined: BTreeMap<String, usize> = BTreeMap::new();
    // One full reason per kind, so the report names an example.
    let mut example: BTreeMap<String, String> = BTreeMap::new();
    let mut programs_ran = 0usize;
    let mut diffs = vec![];
    for case in cases() {
        let mut any = false;
        for &(optimize, fuel) in CONFIGS {
            let rust = quietly(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    rust_answer(case.world, optimize, fuel)
                }))
            });
            let Ok(rust) = rust else {
                continue;
            };
            match vesper_answer(case.world, optimize, fuel) {
                Answer::Declined(why) => {
                    if fuel == CONFIGS[0].1 && optimize {
                        // Group by the reason's kind, not its function.
                        let kind = why.split(" in ").next().unwrap_or(&why);
                        let kind = kind.split(" of ").next().unwrap_or(kind);
                        *declined.entry(kind.to_string()).or_default() += 1;
                        example
                            .entry(kind.to_string())
                            .or_insert_with(|| format!("{}: {why}", case.name));
                    }
                }
                ours => {
                    ran += 1;
                    any = true;
                    if ours != rust {
                        diffs.push(format!(
                            "{} (optimize {optimize}, fuel {fuel}):\n  rust:   {rust:?}\n  vesper: {ours:?}",
                            case.name
                        ));
                    }
                }
            }
        }
        if any {
            programs_ran += 1;
        }
    }
    eprintln!(
        "run: {programs_ran} of {} programs ran in Vesper ({ran} runs), {} differences",
        cases().len(),
        diffs.len()
    );
    let mut reasons: Vec<_> = declined.into_iter().collect();
    reasons.sort_by_key(|r| std::cmp::Reverse(r.1));
    for (why, n) in reasons {
        eprintln!("  declined {n}: {why} (e.g. {})", example[&why]);
    }
    for d in diffs.iter().take(20) {
        eprintln!("{d}");
    }
    assert!(diffs.is_empty(), "{} differences", diffs.len());
    assert!(programs_ran > 0, "no program ran in Vesper");
}

#[test]
#[ignore]
fn run_cost() {
    if !speed() {
        return;
    }
    let mut ours = vec![];
    for case in cases() {
        if matches!(
            vesper_answer(case.world, true, CONFIGS[0].1),
            Answer::Declined(_)
        ) {
            continue;
        }
        ours.push(case);
    }
    let mut rust = || {
        time(|| {
            for c in &ours {
                std::hint::black_box(rust_answer(c.world, true, CONFIGS[0].1));
            }
        })
    };
    let mut vesper = || {
        time(|| {
            for c in &ours {
                std::hint::black_box(vesper_answer(c.world, true, CONFIGS[0].1));
            }
        })
    };
    let label = format!("run over {} programs", ours.len());
    let (_, _, hi) = abba_ci(&label, 15, &mut rust, &mut vesper);
    assert!(hi <= 1.5, "the 95% CI upper bound {hi:.3}x is above 1.50x");
}
