//! Rust-exit gap wave 1, lane 5 (#1502): cheap calls across the export ABI.
//!
//! The per-call cost the call-bound ports paid was mostly not the boundary
//! (an empty export costs about a nanosecond more than a Rust call; see
//! docs/parallel-results/rust-exit-gaps-export-calls.md) but small helpers
//! the inliner kept as calls: a callee whose `Fail` row is narrower than its
//! caller's (Resonance's slice accessor `f`, row `Fail[RuntimeError],
//! Fail[ArithOverflow]`, called from `k_pid_step`, which also raises
//! `Fail[PidFault]`). A callee whose row is within the caller's is now
//! inlined when it has no loop, handler or `Fail::raise`; its fallible
//! operations and calls raise by error type, so they perform the caller's
//! instance of the same error type. These tests check the decisions and that every backend still
//! computes the same values, each error reaching its own handler.
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

/// The evaluator and the JIT, optimized and not, compute `expected`.
fn agree(source: &str, expected: &str) {
    let w = world(source);
    assert_eq!(evaluate(&w, true), expected, "evaluator");
    assert_eq!(evaluate(&w, false), expected, "unoptimized evaluator");
    for optimize in [true, false] {
        assert_eq!(
            native::jit_with_options(&w, "main", optimize)
                .unwrap()
                .to_string(),
            expected,
            "JIT, optimize {optimize}"
        );
    }
}

fn inlined(source: &str) -> vesper::ir::Program {
    let w = world(source);
    let mut program = vesper::ir::lower_world(&w);
    vesper::ir_opt::optimize(&w, &mut program).unwrap();
    ir_inline::inline(&w, &mut program, &Budget::default());
    let evidence = vesper::ir_check::evidence(&program);
    assert!(evidence.is_empty(), "{evidence:?}");
    program
}

fn calls(program: &vesper::ir::Program, caller: &str, callee: &str) -> usize {
    program.functions[caller]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(&i.op, vesper::ir::Op::Call { callee: c, .. } if c == callee))
        .count()
}

/// `get` raises `Fail[RuntimeError]` (an index out of bounds) and
/// `Fail[ArithOverflow]`; `step` adds its own `Fail[Fault]` and calls `get`
/// three times. `run` handles each error separately, so a raise that picked
/// the wrong instance of the caller's row would land in the wrong handler
/// (or read the wrong payload).
const SUBSET: &str = "enum Fault { Negative(i64), Big } \
    fn get(in s: Slice[i64], i: u64) -> i64 eff [Fail[RuntimeError], Fail[ArithOverflow]] { s[i] + 1 } \
    fn step(in s: Slice[i64], i: u64) -> i64 eff [Fail[RuntimeError], Fail[ArithOverflow], Fail[Fault]] { \
        let a = get(in s, i); \
        if a < 0 { Fail::raise(Fault::Negative(a)); } \
        if a > 1000 { Fail::raise(Fault::Big); } \
        wrapping_add(a, wrapping_add(get(in s, 0), get(in s, 1))) \
    } \
    fn run(in s: Slice[i64], i: u64) -> i64 { \
        handle Fail[Fault] in { \
            handle Fail[ArithOverflow] in { \
                handle Fail[RuntimeError] in { step(in s, i) } with { return v => v, raise(e, k) => 1, } \
            } with { return v => v, raise(e, k) => 2, } \
        } with { return v => v, raise(e, k) => match e { Fault::Negative(n) => wrapping_sub(0, n), Fault::Big => 4, }, } \
    } \
    fn main() -> i64 { \
        let mut v = ChunkSeq::new[i64](); \
        v.push(10); v.push(20); v.push(-50); v.push(5000); v.push(9223372036854775807); \
        let ok = run(in v.as_slice(), 1); \
        let oob = run(in v.as_slice(), 9); \
        let over = run(in v.as_slice(), 4); \
        let neg = run(in v.as_slice(), 2); \
        let big = run(in v.as_slice(), 3); \
        wrapping_add(wrapping_mul(ok, 1000000), wrapping_add(wrapping_mul(oob, 100000), wrapping_add(wrapping_mul(over, 10000), wrapping_add(wrapping_mul(neg, 10), big)))) \
    }";

#[test]
fn a_narrower_fail_row_is_inlined() {
    // ok = 21 + 11 + 21 = 53; oob 1; over 2; neg -(-49) = 49; big 4.
    agree(SUBSET, "53120494");
    let program = inlined(SUBSET);
    assert_eq!(calls(&program, "step", "get"), 0, "get stays a call");
}

/// A callee with a loop and a narrower row stays a call: merged loop nests
/// spill in the native backend (Resonance's `accumulate2` into
/// `sum_inputs` went from 1.0G to 1.8G instructions).
const LOOPED: &str = "enum Fault { Bad } \
    fn total(in s: Slice[i64]) -> i64 eff [Fail[RuntimeError], Fail[ArithOverflow]] { \
        let mut t = 0; for i in 0..s.len() { t = t + s[i]; } t \
    } \
    fn checked(in s: Slice[i64]) -> i64 eff [Fail[RuntimeError], Fail[ArithOverflow], Fail[Fault]] { \
        let t = total(in s); if t < 0 { Fail::raise(Fault::Bad); } t \
    } \
    fn main() -> i64 { \
        let mut v = ChunkSeq::new[i64](); v.push(3); v.push(4); \
        handle Fail[Fault] in { \
            handle Fail[RuntimeError] in { \
                handle Fail[ArithOverflow] in { checked(in v.as_slice()) } with { return v => v, raise(e, k) => 0, } \
            } with { return v => v, raise(e, k) => 0, } \
        } with { return v => v, raise(e, k) => 0, } \
    }";

#[test]
fn a_narrower_row_with_a_loop_stays_a_call() {
    agree(LOOPED, "7");
    let program = inlined(LOOPED);
    assert_eq!(calls(&program, "checked", "total"), 1);
}

/// A callee whose row has an instance the caller lacks is never inlined
/// (it could not type-check otherwise, but the caller's handler can make up
/// the difference: here `outer` handles `Fail[Fault]` around the call).
const WIDER: &str = "enum Fault { Bad } \
    fn risky(x: i64) -> i64 eff [Fail[ArithOverflow], Fail[Fault]] { if x < 0 { Fail::raise(Fault::Bad); } x + 1 } \
    fn outer(x: i64) -> i64 eff [Fail[ArithOverflow]] { \
        handle Fail[Fault] in { risky(x) } with { return v => v, raise(e, k) => 100, } \
    } \
    fn main() -> i64 { \
        handle Fail[ArithOverflow] in { wrapping_add(outer(5), outer(-1)) } with { return v => v, raise(e, k) => 0, } \
    }";

#[test]
fn a_callee_row_the_caller_lacks_keeps_working() {
    agree(WIDER, "106");
}

/// A callee that raises with `Fail::raise` stays a call when its row is
/// narrower than the caller's: the raise records no error type, and the
/// native backend performs the innermost `Fail` in scope, which inlined
/// would be the caller's last instance (`Fail[ArithOverflow]` here) rather
/// than `Fail[Limit]` (tests/fixtures/verified_codegen/fail_try.ves).
const RAISES: &str = "copy struct Limit { at: i64, max: i64 } copy enum Bad { Zero } \
    fn over(x: i64) -> i64 eff [Fail[Limit]] { Fail::raise(Limit { at: x, max: 100 }) } \
    fn wide(x: i64) -> i64 eff [Fail[Limit], Fail[Bad], Fail[ArithOverflow]] { if x > 10 { over(x) } else { x } } \
    fn main() -> i64 { \
        handle Fail[ArithOverflow] in { \
            handle Fail[Bad] in { \
                handle Fail[Limit] in { wide(70) } with { return v => v, raise(l, k) => l.at * 10000 + l.max, } \
            } with { return v => v, raise(e, k) => 5, } \
        } with { return v => v, raise(e, k) => 99, } \
    }";

#[test]
fn a_narrower_row_with_a_raise_stays_a_call() {
    agree(RAISES, "700100");
    let program = inlined(RAISES);
    assert_eq!(calls(&program, "wide", "over"), 1);
}
