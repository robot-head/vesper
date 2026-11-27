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
        // A closure performs to the handler it was made under, a function
        // value to the caller's handler where it is passed, and an open row
        // takes all of the caller's evidence.
        "evidence_binding",
        "effect Ask { fn ask() -> i64; } \
         fn twice(f: fn() -> i64 eff [Ask]) -> i64 eff [Ask] { handle Ask in \
         { wrapping_add(f(), Ask::ask()) } with { return x => x, ask(k) => k(100), } } \
         fn open[e: eff](f: fn() -> i64 eff [e]) -> i64 eff [e] { handle Ask in { f() } \
         with { return x => x, ask(k) => k(1000), } } \
         fn asked() -> i64 eff [Ask] { wrapping_add(Ask::ask(), 1) } \
         fn main() -> (i64, i64, i64, i64, i64) { handle Ask in { \
         let a = twice(|| Ask::ask()); let b = twice(asked); \
         let d = handle Ask in { twice(|| wrapping_mul(Ask::ask(), 2)) } \
         with { return x => x, ask(k) => k(7), }; \
         let e = open(asked); let g = open(|| Ask::ask()); (a, b, d, e, g) } \
         with { return x => x, ask(k) => k(3), } }",
    ),
    (
        // Float display (shortest digits, no exponent, `-0`, `NaN`, `inf`),
        // binary32 rounding, `%`, conversions, bits and literal patterns.
        "float_values",
        "fn kind(x: f64) -> i64 { match x { 0.5 => 1, 2.0 => 2, _ => 3 } } \
         fn kind32(x: f32) -> i64 { match x { 0.1 => 1, _ => 2 } } \
         fn main() -> (f64, f64, f64, f64, f32, f32, f64, f64, f64, (f64, f32, u64, u32, f32), \
         (i64, i64, i64, i64, bool, bool)) { \
         let tenth: f32 = 0.1; let third: f32 = 1.0; let z: f64 = 0.0; \
         (0.1 + 0.2, 1e300, 1.5e-7, -z, tenth * 3.0, third / 3.0, 7.5 % 2.0, z / z, -1.0 / z, \
         (widen_f64(tenth), nearest_f32(16777217.0), to_bits_f64(1.0), to_bits_f32(tenth), \
         from_bits_f32(1)), (kind(0.5), kind(2.0), kind(z), kind32(tenth), z == -z, z / z == z / z)) }",
    ),
    (
        // A labelled operation goes to the handler with its label, past an
        // inner unlabelled one of the same effect.
        "labelled_raise",
        "fn pick(n: u64) -> u64 eff [stop: Fail[u64]] { \
         let inner = handle Fail[u64] in { if n > 5 { stop.raise(n) } else { n } } \
         with { return v => v, raise(e, k) => wrapping_add(e, 100), }; wrapping_add(inner, 1) } \
         fn main() -> (u64, u64) { let a = handle stop: Fail[u64] in { pick(3) } \
         with { return v => v, raise(e, k) => wrapping_mul(e, 10), }; \
         let b = handle stop: Fail[u64] in { pick(7) } \
         with { return v => v, raise(e, k) => wrapping_mul(e, 10), }; (a, b) }",
    ),
    (
        // `Box::new` makes a `Box` data value that `Box(x)` patterns open.
        "box_values",
        "enum T { Leaf, Node { v: i64, next: Box[T] } } \
         fn sum(sink t: T) -> i64 { match t { T::Leaf => 0, \
         T::Node { v, next } => match next { Box(rest) => wrapping_add(v, sum(rest)) } } } \
         fn main() -> (i64, Box[i64]) { \
         let t = T::Node { v: 1, next: Box::new(T::Node { v: 2, next: Box::new(T::Leaf) }) }; \
         (sum(t), Box::new(5)) }",
    ),
    (
        // `return` in a handled body leaves the function, skipping the
        // return clause and the rest of the caller's body.
        "nested_return",
        "effect Ask { fn ask() -> i64; } \
         fn early(x: i64) -> i64 { let r = handle Ask in { if x > 0 { return wrapping_add(x, 1000); } \
         Ask::ask() } with { return v => wrapping_mul(v, 2), ask(k) => k(5), }; wrapping_add(r, 1) } \
         fn main() -> (i64, i64) { (early(1), early(0)) }",
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
        "sequences",
        "fn len(in s: Slice[u8]) -> u64 { s.len() } \
         fn run() -> (u64, ChunkSeq[u8], u32, u16, bool, u64) eff [Fail[RuntimeError]] { \
         let mut b: ChunkSeq[u8] = ChunkSeq::new[u8](); for i in 0..40 { b.push(wrapping_u8(i)); } \
         b.write_u32_be(4, 3735928559); let r = b.read_u32_be(4); let w = b.read_u16_le(8); \
         let ok = b.try_write_u64_le(36, 1); let mut c: ChunkSeq[u8] = ChunkSeq::new[u8](); \
         c.extend(in b[2..6]); c.extend(in b[30..33]); let head = match c.pop() { \
         Option::Some(x) => widen_u64(x), Option::None => 99 }; \
         (head, c, r, w, ok, len(in b[10..15])) } \
         fn main() -> (u64, ChunkSeq[u8], u32, u16, bool, u64) { handle Fail[RuntimeError] in { run() } \
         with { return v => v, raise(e, k) => (0, ChunkSeq::new[u8](), 0, 0, false, 0) } }",
    ),
    (
        "sequence_faults",
        "fn get(in s: Slice[u64], i: u64) -> u64 eff [Fail[RuntimeError]] { s[i] } \
         fn main() -> (u64, u64) { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); s.push(5); \
         (handle Fail[RuntimeError] in { get(in s.as_slice(), 3) } with { return v => v, raise(e, k) => 77 }, \
         handle Fail[RuntimeError] in { get(in s.as_slice(), 0) } with { return v => v, raise(e, k) => 78 }) }",
    ),
    (
        "closures",
        "fn apply(f: fn(i64) -> i64, x: i64) -> i64 { f(x) } \
         fn adder(n: i64) -> fn(i64) -> i64 { |x: i64| wrapping_add(x, n) } \
         fn main() -> (i64, i64, fn(i64) -> i64) { let k = 10; let mut s = 0; \
         for i in 0..5 { s = wrapping_add(s, apply(|x: i64| wrapping_mul(x, k), i)); } \
         let add3 = adder(3); (s, apply(|x: i64| wrapping_add(x, 1), 4), add3) }",
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
        "string_updates",
        "fn grow(inout s: String) { s.push_str(\"xy\"); let ok = s.push_char(66); } \
         fn pick(t: String) -> i64 { match t { \"xyB\" => 1, _ => 2 } } \
         fn total(in b: Slice[u8]) -> u64 { let mut n: u64 = 0; for i in 0..b.len() { \
         n = wrapping_add(n, match b.try_get(i) { Option::Some(x) => widen_u64(x), \
         Option::None => 0 }); } n } \
         fn main() -> (String, String, u64, u64, bool, bool, i64, i64) { \
         let mut s = String::new(); grow(inout s); let r = \"xyB\"; \
         let mut t = \"q\"; let wide = t.push_char(233); let bad = t.push_char(55296); \
         let mut c = \"zz\"; c.clear(); c.reserve(10); c.push_str(s[1..3]); \
         let b = handle Fail[RuntimeError] in { t.byte_at(9) } with \
         { return v => widen_u64(v), raise(e, k) => 99 }; \
         let n = wrapping_add(t.len(), b); let sum = total(t.as_bytes()); \
         let p = pick(r); let q = pick(\"x\"); (s, c, n, sum, wide, bad, p, q) }",
    ),
    (
        "string_view_bounds",
        "fn v(in s: Str) -> u64 { s.len() } fn main() -> u64 { let s = \"abc\"; v(s[1..5]) }",
    ),
    (
        "string_view_boundary",
        "fn v(in s: Str) -> u64 { s.len() } \
         fn main() -> u64 { let mut s = \"a\"; let ok = s.push_char(233); v(s[0..2]) }",
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
    (
        "region_ops",
        "linear struct T { v: i64 } \
         fn main() -> (bool, bool, u64, u64, i64, u64) { region r { let a = r.box(1); let b = a; \
         let c = r.box(2); let m = r.checkpoint(); let d = r.box(3); r.commit(m); let u = r.used(); \
         r.set(c, 5); let t = r.box(T { v: 4 }); let before = r.used(); let T { v } = r.take(t); \
         let n = region q(16) { let x = q.box(7); q.used() }; \
         (a == b, a == c, u, before, wrapping_add(wrapping_add(r.get(c), r.get(d)), v), n) } }",
    ),
    (
        "region_first_chunk",
        "fn main() -> i64 { region r(8) { let x = r.box((1, 2)); 0 } }",
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
