//! Port speed lane 5 (#1025): the inliner charges a callee what it costs
//! at the call site, after the site's constant arguments decide branches and
//! matches, and inlines the hot part of a callee that is still too large,
//! calling the whole callee on the cold side.
//!
//! Every program runs through the reference evaluator (optimized and not),
//! the JIT with `optimize` on and off, and object emission for x86-64 and
//! AArch64 (as in `tests/native_inline.rs`). IR-level checks name which
//! calls the inliner replaced, fully or partially.
use vesper::{
    check,
    ir_inline::{self, Budget, Refusal},
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
    for optimize in [true, false] {
        assert_eq!(
            native::jit_with_options(&w, "main", optimize)
                .unwrap_or_else(|e| panic!("JIT optimize={optimize}: {e}"))
                .to_string(),
            expected,
            "JIT optimize={optimize}"
        );
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for optimize in [true, false] {
            let (obj, _) = native::object_with_options(&w, "main", Some(target), optimize)
                .unwrap_or_else(|e| panic!("{target} optimize={optimize}: {e}"));
            assert_eq!(&obj[..4], b"\x7fELF");
        }
    }
}

/// Lower, optimize and inline `source` with `budget`, check the token
/// evidence, and return the report and program.
fn inlined_with(source: &str, budget: &Budget) -> (ir_inline::Report, vesper::ir::Program) {
    let w = world(source);
    let mut program = vesper::ir::lower_world(&w);
    vesper::ir_opt::optimize(&w, &mut program).unwrap();
    let report = ir_inline::inline(&w, &mut program, budget);
    let evidence = vesper::ir_check::evidence(&program);
    assert!(evidence.is_empty(), "{evidence:?}");
    (report, program)
}

fn inlined(source: &str) -> (ir_inline::Report, vesper::ir::Program) {
    inlined_with(source, &Budget::default())
}

fn calls(program: &vesper::ir::Program, caller: &str, callee: &str) -> usize {
    program.functions[caller]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(&i.op, vesper::ir::Op::Call { callee: c, .. } if c == callee))
        .count()
}

/// A body well over [`Budget::small_callee`]: a loop of 32 rounds of mixing.
fn heavy(name: &str, param: &str) -> String {
    let mut body = String::new();
    for k in 0..12 {
        body.push_str(&format!(
            "h = wrapping_add(wrapping_mul(h, {}), rotate_left(h, {})); ",
            2 * k + 3,
            k + 1
        ));
    }
    format!(
        "fn {name}({param}: u64) -> u64 {{ let mut h = {param}; for i in 0..4 {{ {body} }} h }}"
    )
}

/// `mode` is a constant at every site, so each copy keeps one arm: the
/// callee is over budget whole and fits at each site.
#[test]
fn constant_arguments_decide_branches() {
    let source = format!(
        "{} {} \
        fn pick(mode: u64, x: u64) -> u64 {{ if mode == 0 {{ wrapping_add(x, 1) }} else if mode == 1 {{ mix_a(x) }} else {{ mix_b(x) }} }} \
        fn run(x: u64) -> u64 {{ wrapping_add(pick(0, x), pick(0, wrapping_add(x, 5))) }} \
        fn main() -> u64 {{ wrapping_add(run(10), pick(1, 3)) }}",
        heavy("mix_a", "a"),
        heavy("mix_b", "b")
    );
    let w = world(&source);
    let expected = evaluate(&w, false);
    agree(&source, &expected);
    // Without folding the callee would not fit: two heavy callees inline
    // into it first.
    let (report, program) = inlined(&source);
    assert_eq!(calls(&program, "run", "pick"), 0, "{report:?}");
    assert!(report.folded >= 2, "{report:?}");
    // In `run`, the copies keep only `x + 1`: no copy of a heavy body.
    let run = &program.functions["run"];
    assert!(
        run.blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .all(|i| !matches!(i.op, vesper::ir::Op::Loop)),
        "a dead arm's loop is still in run"
    );
}

/// A nullary constructor argument decides a `match` on it.
#[test]
fn known_tags_decide_matches() {
    let source = format!(
        "copy enum Mode {{ Fast, Slow, Other }} {} \
        fn go(m: Mode, x: u64) -> u64 {{ match m {{ Mode::Fast => wrapping_add(x, 2), Mode::Slow => mix_a(x), Mode::Other => mix_a(wrapping_add(x, 9)) }} }} \
        fn run(x: u64) -> u64 {{ wrapping_add(go(Mode::Fast, x), go(Mode::Fast, 7)) }} \
        fn main() -> u64 {{ wrapping_add(run(1), wrapping_add(go(Mode::Slow, 4), go(Mode::Other, 4))) }}",
        heavy("mix_a", "a")
    );
    let w = world(&source);
    let expected = evaluate(&w, false);
    agree(&source, &expected);
    let (report, program) = inlined(&source);
    assert_eq!(calls(&program, "run", "go"), 0, "{report:?}");
}

/// BBR's `mul_div_sat`: a cheap common case and a wide division loop (here
/// unrolled four times, so the whole callee is over budget). The copy keeps
/// the common case and calls the callee for the rest.
const MUL_DIV: &str = "fn div(a: u64, b: u64) -> u64 { if b == 0 { 0 } else { a / b } } \
    fn div_wide_sat(hi: u64, lo: u64, d: u64) -> u64 { \
        if d == 0 { return 18446744073709551615; } \
        if hi == 0 { return div(lo, d); } \
        if hi >= d { return 18446744073709551615; } \
        let mut rem = hi; let mut rest = lo; let mut q: u64 = 0; \
        for i in 0..16 { let top = rem >> 63; rem = (rem << 1) | (rest >> 63); rest = rest << 1; q = q << 1; \
            if top == 1 || rem >= d { rem = wrapping_sub(rem, d); q = q | 1; } let top = rem >> 63; rem = (rem << 1) | (rest >> 63); rest = rest << 1; q = q << 1; \
            if top == 1 || rem >= d { rem = wrapping_sub(rem, d); q = q | 1; } let top = rem >> 63; rem = (rem << 1) | (rest >> 63); rest = rest << 1; q = q << 1; \
            if top == 1 || rem >= d { rem = wrapping_sub(rem, d); q = q | 1; } let top = rem >> 63; rem = (rem << 1) | (rest >> 63); rest = rest << 1; q = q << 1; \
            if top == 1 || rem >= d { rem = wrapping_sub(rem, d); q = q | 1; } } \
        q } \
    fn mul_div_sat(a: u64, b: u64, c: u64) -> u64 { \
        if a <= 4294967295 && b <= 4294967295 { return div(wrapping_mul(a, b), c); } \
        let (lo, hi) = mul_wide(a, b); \
        div_wide_sat(hi, lo, c) } \
    fn step(x: u64, y: u64) -> u64 { wrapping_add(mul_div_sat(x, y, 1000), mul_div_sat(y, 3, 7)) } \
    fn main() -> u64 { \
        let big: u64 = 1 << 40; \
        wrapping_add(wrapping_add(step(1500, 2), step(big, 3)), wrapping_add(step(big, big), mul_div_sat(big, big, 0))) }";

#[test]
fn partial_inlining_keeps_the_hot_path() {
    let w = world(MUL_DIV);
    let expected = evaluate(&w, false);
    agree(MUL_DIV, &expected);
    let (report, program) = inlined(MUL_DIV);
    assert_eq!(report.partial.get("mul_div_sat"), Some(&2), "{report:?}");
    // Each partial copy calls the whole callee once, on its cold side.
    assert_eq!(calls(&program, "step", "mul_div_sat"), 2, "{report:?}");
    // The hot path holds no call: `div` and the multiply are inline.
    assert_eq!(calls(&program, "step", "div"), 0, "{report:?}");
}

/// A derived `Ord::cmp` over many fields: the copy compares the first
/// field and calls the whole comparison only when it is equal.
#[test]
fn derived_cmp_splits_after_the_first_field() {
    let source = "copy struct K { a: u32, b: u32, c: u32, d: u32, e: u32, f: u32, g: u32, h: u32, i: u32, j: u32, k: u32, l: u32 } \
        impl Ord for K {} \
        fn mk(a: u32, l: u32) -> K { K { a: a, b: 1, c: 2, d: 3, e: 4, f: 5, g: 6, h: 7, i: 8, j: 9, k: 10, l: l } } \
        fn rank(in x: K, in y: K) -> u64 { match x.cmp(in y) { Ordering::Less => 1, Ordering::Equal => 2, Ordering::Greater => 3 } } \
        fn main() -> u64 { \
            let p = mk(1, 5); let q = mk(2, 5); let r = mk(1, 6); \
            wrapping_add(wrapping_add(wrapping_mul(rank(in p, in q), 100), wrapping_mul(rank(in q, in p), 10)), wrapping_add(rank(in p, in r), wrapping_mul(rank(in p, in p), 1000))) }";
    agree(source, "2131");
    let (report, _) = inlined(source);
    assert!(
        report.partial.keys().any(|k| k.contains("cmp"))
            || report.callees.keys().any(|k| k.contains("cmp")),
        "{report:?}"
    );
}

/// `Budget::callee_growth` bounds what the copies of one callee add.
#[test]
fn callee_growth_caps_copies() {
    let source = "fn f(x: u64) -> u64 { wrapping_add(wrapping_mul(x, 3), rotate_left(x, 7)) } \
        fn main() -> u64 { wrapping_add(wrapping_add(f(1), f(2)), wrapping_add(f(3), f(4))) }";
    agree(source, &evaluate(&world(source), false));
    let budget = Budget {
        callee_growth: 2 * ir_inline_cost_of_f(source),
        ..Budget::default()
    };
    let (report, program) = inlined_with(source, &budget);
    assert_eq!(report.callees.get("f"), Some(&2), "{report:?}");
    assert_eq!(calls(&program, "main", "f"), 2);
    assert_eq!(report.refused["f"], [(Refusal::Growth, 2)].into());
}

fn ir_inline_cost_of_f(source: &str) -> usize {
    let w = world(source);
    let mut program = vesper::ir::lower_world(&w);
    vesper::ir_opt::optimize(&w, &mut program).unwrap();
    ir_inline::cost(&w, &program.functions["f"])
}

/// A known argument whose parameter the callee writes is not folded.
#[test]
fn written_parameters_are_not_folded() {
    let source = format!(
        "{} \
        fn g(mode: u64, x: u64) -> u64 {{ let mut m = mode; if x > 5 {{ m = 1; }} if m == 0 {{ wrapping_add(x, 1) }} else {{ mix_a(x) }} }} \
        fn main() -> u64 {{ wrapping_add(g(0, 2), g(0, 9)) }}",
        heavy("mix_a", "a")
    );
    let w = world(&source);
    let expected = evaluate(&w, false);
    agree(&source, &expected);
}

/// Copies of `f` that arrive inside an inlined wrapper count against `f`'s
/// cap too.
#[test]
fn callee_growth_counts_nested_copies() {
    let source = "fn f(x: u64) -> u64 { wrapping_add(wrapping_mul(x, 3), rotate_left(x, 7)) } \
        fn w(x: u64) -> u64 { wrapping_add(f(x), 1) } \
        fn main() -> u64 { wrapping_add(wrapping_add(w(1), w(2)), wrapping_add(w(3), w(4))) }";
    agree(source, &evaluate(&world(source), false));
    let budget = Budget {
        callee_growth: 3 * ir_inline_cost_of_f(source),
        ..Budget::default()
    };
    let (report, program) = inlined_with(source, &budget);
    // One copy of `f` inside `w`, then two copies of `w` into `main`: three.
    assert!(report.growth["f"] <= budget.callee_growth, "{report:?}");
    assert_eq!(calls(&program, "main", "w"), 2, "{report:?}");
}
