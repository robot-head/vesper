//! Stage 1 performance lane 1 (#931): the native Place IR inliner.
//!
//! Every program runs through the reference evaluator (optimized and not),
//! the JIT with `optimize` on (inliner and `opt_level=speed`) and off, and
//! object emission for x86-64 and AArch64. IR-level tests check which calls
//! the inliner replaces and that the result still has valid state tokens.
use vesper::{
    check,
    ir_inline::{self, Budget},
    native, parser,
    runtime::Runtime,
};

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}

fn evaluate(world: &check::World, optimize: bool) -> String {
    let mut runtime = Runtime::new(world);
    runtime.optimize = optimize;
    runtime.run("main", vec![]).unwrap().to_string()
}

/// Every backend computes `expected` for `main`.
fn agree(source: &str, expected: &str) {
    let w = world(source);
    assert_eq!(evaluate(&w, true), expected, "evaluator");
    assert_eq!(evaluate(&w, false), expected, "unoptimized evaluator");
    assert_eq!(
        native::jit_with_options(&w, "main", true)
            .unwrap()
            .to_string(),
        expected,
        "optimized JIT"
    );
    assert_eq!(
        native::jit_with_options(&w, "main", false)
            .unwrap()
            .to_string(),
        expected,
        "unoptimized JIT"
    );
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for optimize in [true, false] {
            let (obj, _) = native::object_with_options(&w, "main", Some(target), optimize)
                .unwrap_or_else(|e| panic!("{target} optimize={optimize}: {e}"));
            assert_eq!(&obj[..4], b"\x7fELF");
        }
    }
}

/// Lower, optimize and inline `source`, check the token evidence, and
/// return the report.
fn inlined(source: &str, budget: &Budget) -> (ir_inline::Report, vesper::ir::Program) {
    let w = world(source);
    let mut program = vesper::ir::lower_world(&w);
    vesper::ir_opt::optimize(&w, &mut program).unwrap();
    let report = ir_inline::inline(&w, &mut program, budget);
    let evidence = vesper::ir_check::evidence(&program);
    assert!(evidence.is_empty(), "{evidence:?}");
    (report, program)
}

fn calls(program: &vesper::ir::Program, caller: &str, callee: &str) -> usize {
    program.functions[caller]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(&i.op, vesper::ir::Op::Call { callee: c, .. } if c == callee))
        .count()
}

const NESTED: &str = "fn sq(x: i64) -> i64 { wrapping_mul(x, x) } \
    fn add3(a: i64, b: i64, c: i64) -> i64 { wrapping_add(wrapping_add(a, b), c) } \
    fn f(x: i64) -> i64 { add3(sq(x), sq(wrapping_add(x, 1)), 7) } \
    fn main() -> i64 { wrapping_add(f(3), f(4)) }";

#[test]
fn nested_calls() {
    agree(NESTED, "80");
    let (report, program) = inlined(NESTED, &Budget::default());
    assert!(report.sites >= 4, "{report:?}");
    assert_eq!(calls(&program, "f", "sq"), 0);
    assert_eq!(calls(&program, "f", "add3"), 0);
    assert_eq!(calls(&program, "main", "f"), 0);
}

#[test]
fn early_return_and_branches() {
    agree(
        "fn clamp(x: i64, lo: i64, hi: i64) -> i64 { if x < lo { return lo; } if x > hi { return hi; } x } \
         fn main() -> i64 { wrapping_add(wrapping_add(clamp(-5, 0, 10), wrapping_mul(clamp(50, 0, 10), 100)), wrapping_mul(clamp(7, 0, 10), 10000)) }",
        "71000",
    );
}

#[test]
fn inout_scalars_and_fields() {
    agree(
        "copy struct P { x: i64, y: i64 } \
         fn bump(inout v: i64) { v = wrapping_add(v, 1); } \
         fn shift(inout p: P, d: i64) { p.x = wrapping_add(p.x, d); bump(inout p.y); } \
         fn main() -> i64 { let mut p = P { x: 1, y: 2 }; shift(inout p, 10); shift(inout p, 5); bump(inout p.x); wrapping_add(wrapping_mul(p.x, 100), p.y) }",
        "1704",
    );
}

#[test]
fn borrowed_and_returned_aggregates() {
    agree(
        "copy struct P { x: i64, y: i64 } \
         fn sum(in p: P) -> i64 { wrapping_add(p.x, p.y) } \
         fn mk(a: i64) -> P { P { x: a, y: wrapping_add(a, 1) } } \
         fn swap(p: P) -> P { P { x: p.y, y: p.x } } \
         fn main() -> i64 { let mut acc = mk(0); for i in 0..5 { acc = swap(mk(wrapping_add(sum(in acc), i))); } wrapping_add(wrapping_mul(acc.x, 1000), acc.y) }",
        &{
            // Reference computation of the loop above.
            let (mut x, mut y) = (0i64, 1i64);
            for i in 0..5 {
                let a = x + y + i;
                (x, y) = (a + 1, a);
            }
            (x * 1000 + y).to_string()
        },
    );
}

#[test]
fn by_value_copy_is_not_aliased() {
    agree(
        "copy struct P { x: i64, y: i64 } \
         fn poke(p: P) -> i64 { let mut q = p; q.x = 99; q.x } \
         fn main() -> i64 { let p = P { x: 1, y: 2 }; let a = poke(p); wrapping_add(wrapping_mul(a, 10), p.x) }",
        "991",
    );
}

#[test]
fn loops_in_callees() {
    agree(
        "fn tri(n: i64) -> i64 { let mut s = 0; for i in 0..n { s = wrapping_add(s, i); } s } \
         fn main() -> i64 { let mut t = 0; for k in 0..6 { t = wrapping_add(t, tri(k)); } t }",
        "20",
    );
}

const FAIL_ROW: &str = "fn step(x: i64) -> i64 eff [Fail[RuntimeError]] { 84 / x } \
    fn twice(x: i64) -> i64 eff [Fail[RuntimeError]] { step(step(x)) } \
    fn run(x: i64) -> i64 { handle Fail[RuntimeError] in { twice(x) } with { return v => v, raise(e, k) => 7, } } \
    fn main() -> i64 { wrapping_add(wrapping_mul(run(2), 100), run(0)) }";

#[test]
fn fail_rows_map_onto_the_callers_row() {
    agree(FAIL_ROW, "207");
    let (_, program) = inlined(FAIL_ROW, &Budget::default());
    // Same `Fail[RuntimeError]` row: `step` is inlined into `twice`.
    assert_eq!(calls(&program, "twice", "step"), 0);
}

#[test]
fn checked_arithmetic_failure_reaches_the_handler() {
    agree(
        "fn inc(x: i64) -> i64 eff [Fail[ArithOverflow]] { x + 1 } \
         fn inc2(x: i64) -> i64 eff [Fail[ArithOverflow]] { inc(inc(x)) } \
         fn main() -> i64 { handle Fail[ArithOverflow] in { inc2(9223372036854775806) } with { return v => v, raise(e, k) => 42, } }",
        "42",
    );
}

#[test]
fn pure_callee_inside_a_fail_row() {
    agree(
        "fn half(x: i64) -> i64 { x / 2 } \
         fn safe(x: i64) -> i64 eff [Fail[RuntimeError]] { wrapping_add(half(84 / x), half(10)) } \
         fn main() -> i64 { handle Fail[RuntimeError] in { safe(2) } with { return v => v, raise(e, k) => 0, } }",
        "26",
    );
}

#[test]
fn generic_instances_keep_working() {
    agree(
        "fn id[T](sink x: T) -> T { x } \
         fn sq(x: i64) -> i64 { wrapping_mul(x, x) } \
         fn wrap[T](sink x: T) -> T { let _ = sq(3); x } \
         fn main() -> i64 { wrapping_add(id(sq(5)), wrapping_add(wrap(2), if wrap(true) { 100 } else { 0 })) }",
        "127",
    );
}

#[test]
fn sequences_borrowed_by_helpers() {
    agree(
        "fn count(in s: ChunkSeq[u64]) -> u64 { s.len() } \
         fn add(inout s: ChunkSeq[u64], v: u64) { s.push(v); } \
         fn twice(inout s: ChunkSeq[u64], v: u64) { add(inout s, v); add(inout s, v); } \
         fn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); add(inout s, 40); twice(inout s, 2); wrapping_add(wrapping_mul(count(in s), 10), s.len()) }",
        "33",
    );
}

#[test]
fn recursion_and_over_budget_callees_stay_calls() {
    let source = "fn fact(n: i64) -> i64 { if n <= 1 { 1 } else { wrapping_mul(n, fact(wrapping_sub(n, 1))) } } \
        fn even(n: i64) -> bool { if n == 0 { true } else { odd(wrapping_sub(n, 1)) } } \
        fn odd(n: i64) -> bool { if n == 0 { false } else { even(wrapping_sub(n, 1)) } } \
        fn big(x: i64) -> i64 { wrapping_add(wrapping_add(wrapping_add(x, 1), wrapping_add(x, 2)), wrapping_add(wrapping_add(x, 3), wrapping_add(x, 4))) } \
        fn main() -> i64 { let a = fact(5); let b = if even(10) { 1 } else { 0 }; wrapping_add(wrapping_add(a, b), wrapping_add(big(1), big(2))) }";
    agree(source, "153");
    let tight = Budget {
        small_callee: 4,
        single_site_callee: 4,
        ..Budget::default()
    };
    let (report, program) = inlined(source, &tight);
    assert_eq!(report.sites, 0, "{report:?}");
    assert_eq!(calls(&program, "main", "fact"), 1);
    assert_eq!(calls(&program, "main", "big"), 2);
    let (report, program) = inlined(source, &Budget::default());
    assert_eq!(calls(&program, "main", "fact"), 1, "recursive");
    assert_eq!(calls(&program, "main", "even"), 1, "mutually recursive");
    assert_eq!(calls(&program, "fact", "fact"), 1);
    assert_eq!(calls(&program, "main", "big"), 0, "{report:?}");
    let (report, _) = inlined(source, &Budget::none());
    assert_eq!(report, ir_inline::Report::default());
}

#[test]
fn closures_stay_calls_and_generic_callees_inline() {
    let source = "fn id[T](sink x: T) -> T { x } \
        fn apply(x: i64) -> i64 { let f = |y: i64| wrapping_add(y, 1); f(x) } \
        fn main() -> i64 { wrapping_add(id(1), apply(2)) }";
    agree(source, "4");
    let (_, program) = inlined(source, &Budget::default());
    // Generic callees inline as typed copies since #989
    // (`tests/native_inline_reach.rs`).
    assert_eq!(calls(&program, "main", "id"), 0, "generic");
    assert_eq!(calls(&program, "main", "apply"), 1, "closure body");
}

/// Executable objects inline when optimized (#959): `main`'s emitted code
/// calls none of the small helpers, the unoptimized object keeps every call,
/// and observing the build (the verifier hook) does not change its bytes.
/// Inlined helpers are still emitted, so verifier policies can name them.
#[test]
fn objects_inline_when_optimized() {
    let w = world(NESTED);
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for optimize in [false, true] {
            let (a, clif_a) =
                native::object_with_options(&w, "main", Some(target), optimize).unwrap();
            let mut emitted = Vec::new();
            let (b, clif_b) = native::object_with_observer(
                &w,
                "main",
                Some(target),
                optimize,
                Some(&mut |f| emitted.push(f)),
            )
            .unwrap();
            assert_eq!(a, b, "{target} optimize={optimize}");
            assert_eq!(clif_a, clif_b);
            let keys: Vec<_> = emitted.iter().map(|f| f.key.as_str()).collect();
            for key in ["main", "f", "add3", "sq"] {
                assert!(keys.contains(&key), "{keys:?}");
            }
            let main = emitted.iter().find(|f| f.key == "main").unwrap();
            assert_eq!(main.optimized, optimize);
            let provenance: Vec<_> = main.inlined.iter().map(String::as_str).collect();
            let expected: &[&str] = if optimize { &["add3", "f", "sq"] } else { &[] };
            assert_eq!(provenance, expected, "{target}");
            let calls_helper = main
                .callees
                .iter()
                .any(|(id, _)| emitted.iter().any(|f| f.func_id == *id && f.key != "main"));
            assert_eq!(calls_helper, !optimize, "{target}: {:?}", main.callees);
        }
    }
}

/// Callers holding `Box` values keep their calls: `Box` drops are placed
/// per block, and splitting a block at an inlined call once let the drop
/// pass free a list that was moved into a pending argument (the JIT then
/// recursed forever in `bounded_generics::sorted_insert_over_a_struct_key`).
#[test]
fn box_holding_callers_keep_their_calls() {
    let source = include_str!("fixtures/bounded_generics/sorted.ves");
    agree(source, "12152021");
    let (_, program) = inlined(source, &Budget::default());
    assert_eq!(calls(&program, "main", "key"), 4);
}
