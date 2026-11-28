//! Part 6a of the native walk (#2094, lane 11 of #1509): whole scalar
//! function bodies emitted from Vesper through the `body` hook of the
//! codegen swap point (src/codegen_swap.rs), compared with the Rust walk
//! per function (CLIF text), as object bytes and as JIT runs.
//!
//! The Vesper side has two halves:
//!
//! - `codegen_scalar_body_plan` (@vesper/codegen-scalar, `body.ves`) reads
//!   the program as Place IR words with the checker's world and, for each
//!   function part 6a covers, writes every builder call
//!   `Codegen::ir_graph_at` makes for its body: blocks, locals, value
//!   registers, block parameters, the lowering order with its seals, and
//!   each instruction and terminator.
//! - `body_hook` (@vesper/codegen-scalar-emit, `body.ves`) emits those
//!   calls over the lent builder.
//!
//! The glue here ([`body`]) only joins them: it plans the lent program
//! once, looks the function up by name and passes its words with the
//! optimizer flag, the status flag and the parameter variables the entry
//! block bound. The Rust walk asks only for a plain frame, and compares
//! both optimizer settings: since part 6c the unoptimized path, which
//! keeps every local in a stack slot, is emitted from Vesper too.
//! `CODEGEN_SCALAR_STEP=n` compares every n-th corpus world (default 4).
#[path = "../../../tests/common/aggregate_facts.rs"]
mod aggregate_facts;
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;
#[path = "../../../tests/common/check_words.rs"]
mod check_words;
#[path = "../../../tests/common/handler_facts.rs"]
mod handler_facts;
#[path = "../../../tests/common/ir_words.rs"]
mod ir_words;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use vesper::codegen_swap::{self, CodegenHooks, Lent};
use vesper::ir::Program;
use vesper::native::WordFunction;
use vesper_codegen_swap_test::*;

/// `body_hook`'s answer when it emitted the body (`HANDLED` in
/// @vesper/codegen-scalar-emit).
const HANDLED: u64 = 1;
/// `body_hook`'s answer to words that are not a body plan.
const BODY_ERROR: u64 = 2;

/// The plan entry and the hook, compiled once with no hook in effect.
struct Vesper {
    plan: check_harness::Entry,
    hook: WordFunction,
}

fn vesper() -> &'static Vesper {
    static V: OnceLock<Vesper> = OnceLock::new();
    V.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let package = check_harness::Package::load("packages/vesper-codegen-scalar");
            let checked = foreign_package(
                &check_harness::repo(),
                "packages/vesper-codegen-scalar/emit",
            );
            Vesper {
                plan: package.entry("codegen_scalar_body_plan", true),
                hook: hook_function(&checked, "body_hook"),
            }
        })
    })
}

/// One program's body plans, by function name, for one optimizer
/// setting.
struct Plan {
    key: ((usize, usize, usize), bool),
    bodies: HashMap<String, Vec<u64>>,
}

thread_local! {
    static PLAN: RefCell<Option<Plan>> = const { RefCell::new(None) };
}

/// Functions emitted from Vesper, and functions the Rust walk kept
/// (unplanned).
static EMITTED: AtomicU64 = AtomicU64::new(0);
static UNPLANNED: AtomicU64 = AtomicU64::new(0);
/// Of the functions emitted from Vesper, those with direct calls (part
/// 6b), and their calls; and the bodies the hook declined for a call the
/// walk does not make directly.
static WITH_CALLS: AtomicU64 = AtomicU64::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);
static CALL_DECLINED: AtomicU64 = AtomicU64::new(0);
/// Of the functions emitted from Vesper, those that return a struct or
/// tuple through the result pointer (#2411).
static SRET: AtomicU64 = AtomicU64::new(0);
/// Of the functions emitted from Vesper, those that build their result in
/// a local buffer and return its words in registers (#2581).
static REGISTER_RESULT: AtomicU64 = AtomicU64::new(0);
/// Of the functions emitted from Vesper, those with an `inout` scalar
/// parameter stored through its address (#2563).
static POINTERS: AtomicU64 = AtomicU64::new(0);

/// Identifies a lent program for the cache: its address, function count
/// and instruction count (a later compile may reuse the address).
fn program_key(p: &Program) -> (usize, usize, usize) {
    let insts = p
        .functions
        .values()
        .flat_map(|f| f.blocks.iter())
        .map(|b| b.instructions.len())
        .sum();
    (std::ptr::from_ref(p) as usize, p.functions.len(), insts)
}

fn plan_of(world: &vesper::check::World, program: &Program, optimize: bool) -> Plan {
    let key = (program_key(program), optimize);
    let mut bodies = HashMap::new();
    // A program the format cannot encode stays on the Rust walk. The
    // optimize path plans its graphs with small enums kept as words
    // (`aggregates::scalar_enums`, #2581), which the facts let the planner
    // rewrite.
    if let Some((mut input, _)) = ir_words::program_input(world, program) {
        // What each handler's expression holds (#2657), before the
        // optimize path's enum facts.
        input.extend(handler_facts::facts(world, program));
        if optimize {
            input.extend(aggregate_facts::facts(world, program));
        }
        let names: Vec<&String> = program.functions.keys().collect();
        let words = vesper().plan.words(&input);
        let mut rest = &words[..];
        while let [fi, n, tail @ ..] = rest {
            let n = *n as usize;
            assert!(
                tail.len() >= n,
                "a body plan of {n} words, {} left",
                tail.len()
            );
            bodies.insert(names[*fi as usize].clone(), tail[..n].to_vec());
            rest = &tail[n..];
        }
        assert!(rest.is_empty(), "a body plan's trailing words: {rest:?}");
    }
    Plan { key, bodies }
}

/// The `body` hook: the function's body plan, emitted by `body_hook` over
/// the lent builder.
fn body(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    if lent.op != codegen_swap::body::FUNCTION {
        return Ok(false);
    }
    let words = PLAN.with(|cell| {
        let mut cell = cell.borrow_mut();
        let key = (program_key(lent.program), lent.optimize);
        if cell.as_ref().is_none_or(|p| p.key != key) {
            *cell = Some(plan_of(lent.world, lent.program, lent.optimize));
        }
        let plan = cell.as_ref().expect("just planned");
        plan.bodies.get(lent.site.function).cloned()
    });
    let Some(words) = words else {
        UNPLANNED.fetch_add(1, Ordering::Relaxed);
        return Ok(false);
    };
    // `[status, source locations, n, n parameter variables, m, m callees]`
    // from the walk, forwarded after the optimizer flag.
    let (callees, sret, _) =
        codegen_swap::body::layout(lent.args).ok_or("a body request without its callees")?;
    let mut args = vec![u64::from(lent.optimize)];
    args.extend_from_slice(lent.args);
    args.extend(words);
    let hook = &vesper().hook;
    let (answer, crossing) = vesper_cranelift_sys::lend_as(
        kind(lent),
        lent.builder,
        &mut *lent.module,
        codegen_swap::body::FUNCTION,
        &args,
        |call| {
            // SAFETY: the hook takes one word, the lent call's address,
            // which is live for the whole call.
            unsafe { hook.call(&[call]) }
        },
    );
    let answer = answer?;
    let function = lent.site.function;
    if let Some(e) = crossing.error {
        return Err(format!("body hook ({function}): {e}"));
    }
    if crossing.unreturned {
        return Err(format!("body hook ({function}) kept a lent handle"));
    }
    match answer {
        HANDLED => {
            EMITTED.fetch_add(1, Ordering::Relaxed);
            if sret != codegen_swap::body::NO_VALUE {
                if sret >> codegen_swap::body::RESULT_WORDS_SHIFT == 0 {
                    SRET.fetch_add(1, Ordering::Relaxed);
                } else {
                    REGISTER_RESULT.fetch_add(1, Ordering::Relaxed);
                }
            }
            if codegen_swap::body::pointers(lent.args)
                .is_some_and(|p| p.iter().any(|a| *a != codegen_swap::body::NO_VALUE))
            {
                POINTERS.fetch_add(1, Ordering::Relaxed);
            }
            if !callees.is_empty() {
                WITH_CALLS.fetch_add(1, Ordering::Relaxed);
                CALLS.fetch_add(callees.len() as u64, Ordering::Relaxed);
            }
            Ok(true)
        }
        0 if callees.contains(&codegen_swap::body::NO_CALLEE) => {
            CALL_DECLINED.fetch_add(1, Ordering::Relaxed);
            Ok(false)
        }
        BODY_ERROR => Err(format!(
            "body hook ({function}): the plan's words do not read"
        )),
        _ => Ok(false),
    }
}

fn hooks() -> CodegenHooks {
    CodegenHooks {
        body: Some(body),
        ..CodegenHooks::default()
    }
}

fn step() -> usize {
    std::env::var("CODEGEN_SCALAR_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(4)
}

/// Runs `f` on a thread with the CLI's deep stack (the deepest corpus
/// worlds overflow a test thread's 2 MiB in debug builds).
fn big_stack<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn_scoped(s, f)
            .expect("spawn a compile thread")
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

/// Whether to JIT-run `case`: every fourth world whose `main` has no
/// effects and no parameters (lane 1's sampling).
fn runs(index: usize, case: &check_harness::Case) -> bool {
    let f = &case.world.functions["main"];
    index.is_multiple_of(4)
        && f.params.is_empty()
        && f.effects.instances.is_empty()
        && f.effects.tail.is_none()
}

/// Compares `world` optimize on and off, with fresh plans.
fn both(name: &str, world: &vesper::check::World, run: bool) -> Diff {
    let mut total = Diff::default();
    for optimize in [true, false] {
        PLAN.with(|p| *p.borrow_mut() = None);
        total.add(compare(name, world, "main", hooks(), optimize, run));
    }
    total
}

/// 0 CLIF differences, byte-identical objects and equal runs over the
/// corpus with whole scalar bodies emitted from Vesper.
#[test]
fn vesper_scalar_bodies_match_the_rust_walk() {
    big_stack(|| {
        let counters = [
            &EMITTED,
            &UNPLANNED,
            &WITH_CALLS,
            &CALLS,
            &CALL_DECLINED,
            &SRET,
            &REGISTER_RESULT,
            &POINTERS,
        ];
        let before = counters.map(|c| c.load(Ordering::Relaxed));
        let worlds: Vec<_> = check_harness::corpus()
            .iter()
            .filter(|c| c.world.functions.contains_key("main"))
            .step_by(step())
            .collect();
        let mut total = Diff::default();
        for (i, case) in worlds.iter().enumerate() {
            total.add(both(&case.name, &case.world, runs(i, case)));
        }
        let after = counters.map(|c| c.load(Ordering::Relaxed));
        let [
            emitted,
            unplanned,
            with_calls,
            calls,
            call_declined,
            sret,
            registers,
            pointers,
        ] = [0, 1, 2, 3, 4, 5, 6, 7].map(|i| after[i] - before[i]);
        eprintln!(
            "native walk bodies: {}; {emitted} functions emitted from Vesper \
             ({with_calls} with {calls} direct calls, {sret} with a struct result, \
             {registers} returning it in registers, {pointers} with inout scalars), \
             {unplanned} left to Rust, {call_declined} declined for a call",
            total.summary()
        );
        assert!(total.worlds > 0, "no corpus world compiled");
        assert!(emitted > 0, "the body hook never emitted");
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Scalar functions the corpus may sample thinly: a narrow local written
/// in one branch, a loop carrying a `bool` and a word, a float local and a
/// `Never` value flowing to a join (`unreachable` arms). None has effects,
/// so each is a plain frame.
const SCALARS: &str = "
fn pick(c: bool, x: u8) -> u8 {
    let mut y: u8 = x;
    if c {
        y = x & 15;
    }
    y
}
fn flip(a: u32, b: u32) -> u32 {
    let mut s: u32 = a;
    let mut go: bool = true;
    loop {
        if !go {
            break;
        }
        s = s ^ b;
        go = s != a;
    }
    s | (a & 3)
}
fn choose(c: bool, x: f64) -> f64 {
    let mut y: f64 = 0.5;
    if c {
        y = x;
    }
    y
}
fn sign(x: i16) -> i16 {
    if x < 0 {
        -1
    } else if x == 0 {
        0
    } else {
        1
    }
}
fn main() -> i64 {
    let f: i64 = if choose(true, 2.0) > 1.0 { 64 } else { 0 };
    widen_i64(pick(true, 200)) ^ widen_i64(pick(false, 7)) ^ widen_i64(flip(5, 9))
        ^ widen_i64(sign(-4)) ^ f
}
";

#[test]
fn vesper_scalar_bodies_match_across_blocks() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        assert!(
            emitted >= 4,
            "only {emitted} of the scalar functions came from Vesper"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Direct calls (part 6b): scalar callees called with narrow, `bool` and
/// float arguments, from branches, loops and other callers, and a call
/// the walk does not make directly (an effectful callee, inlined), which
/// keeps its caller in Rust.
const CALLS_SOURCE: &str = "
effect Tick {
    fn tick() -> i64;
}
fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}
fn digits(x: u8, odd: bool) -> u8 {
    if x < 10 {
        if odd {
            x | 1
        } else {
            x
        }
    } else {
        digits(x / 10, !odd)
    }
}
fn shrink(x: f64) -> f64 {
    if x < 1.0 {
        x
    } else {
        shrink(x * 0.5)
    }
}
fn gcd_sum(n: u64) -> u64 {
    let mut s: u64 = 0;
    let mut i: u64 = 1;
    loop {
        if i > n {
            break;
        }
        s = wrapping_add(s, gcd(i, 12));
        i = wrapping_add(i, 1);
    }
    s
}
fn pick(c: bool, x: u8) -> u8 {
    if c {
        digits(x, true)
    } else {
        digits(x, false)
    }
}
fn small(x: f64) -> f64 {
    shrink(shrink(x))
}
fn ticks() -> i64 eff [Tick] {
    wrapping_add(Tick::tick(), 1)
}
fn counted() -> i64 {
    handle Tick in { ticks() } with {
        return v => v,
        tick(k) => k(41),
    }
}
fn main() -> i64 {
    let h: i64 = if small(80.0) < 1.0 { 64 } else { 0 };
    wrapping_i64(gcd_sum(30)) ^ widen_i64(pick(true, 200)) ^ widen_i64(pick(false, 77)) ^ h
        ^ counted()
}
";

#[test]
fn vesper_bodies_with_direct_calls_match() {
    big_stack(|| {
        let parsed = vesper::parser::parse(CALLS_SOURCE);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the calls program does not build: {e}");
        }
        let counters = [&EMITTED, &WITH_CALLS, &CALLS];
        let before = counters.map(|c| c.load(Ordering::Relaxed));
        let total = both("calls", &checked.world, true);
        let after = counters.map(|c| c.load(Ordering::Relaxed));
        let [emitted, with_calls, calls] = [0, 1, 2].map(|i| after[i] - before[i]);
        eprintln!(
            "calls: {}; {emitted} functions emitted from Vesper, {with_calls} with \
             {calls} direct calls",
            total.summary()
        );
        assert!(
            with_calls >= 4,
            "only {with_calls} functions with calls came from Vesper"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6c scalar features: f32/f64 constants, unary/binary float ops and
/// intrinsics, float<->int conversions, bit builtins, compound local writes,
/// early `return` (`Op::Exit`), and scalar `match`.
const SCALARS_6C: &str = "
fn float_ops(a: f32, b: f32) -> f64 {
    let mut x: f32 = 1.5;
    x += a;
    x -= b;
    x *= 2.0;
    let y: f64 = widen_f64(-x);
    let m: f64 = fma(y, 0.5, min(abs(y), max(floor(y), ceil(y))));
    let r: f64 = round(trunc(m) + 0.25);
    let bits: u64 = to_bits_f64(r);
    from_bits_f64(bits)
}
fn bit_ops(x: u32, y: u32) -> i64 {
    let mut acc: u32 = x;
    acc = acc & 65535;
    acc = acc | y;
    acc = acc ^ wrapping_shl_u32(y, 3);
    let c: u32 = leading_zeros_u32(acc) ^ trailing_zeros_u32(acc) ^ count_ones_u32(acc);
    let b: u32 = bit_xor(bit_or(bit_and(acc, 255), c), wrapping_shr_u32(acc, 2));
    widen_i64(b)
}
fn classify(n: i64) -> i64 {
    if n < 0 {
        return -10;
    }
    match n {
        0 => 1,
        1 => 2,
        7 => 99,
        other => other ^ 3,
    }
}
fn main() -> i64 {
    let f: f64 = float_ops(2.25, 0.75);
    let i: i64 = saturating_i64(f);
    let b: i64 = bit_ops(17, 5);
    let c: i64 = classify(7) ^ classify(-2) ^ classify(4);
    i ^ b ^ c
}
";

#[test]
fn vesper_scalar_bodies_part_6c() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6C);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6c scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6c", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6c: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        assert!(
            emitted >= 8,
            "all 4 part 6c functions x 2 builds should come from Vesper, got {emitted}"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6d: erased capability-only effect rows (`ConstTime`, `Realtime`,
/// `DetFloat`) and 128-bit integer (`i128` / `u128`) scalar bodies.
const SCALARS_6D: &str = "
fn ct_mix(a: u64, b: u64) -> u64 eff [ConstTime] {
    (a ^ b) | (a & 255)
}
fn rt_step(x: i64) -> i64 eff [Realtime] {
    wrapping_add(x, 7)
}
fn df_add(a: f64, b: f64) -> f64 eff [DetFloat] {
    a + b
}
fn wide_arith(a: i128, b: i128) -> i128 {
    let mut s: i128 = wrapping_add(a, b);
    s = wrapping_sub(s, wrapping_mul(a, 3));
    let n: i128 = ~s;
    let m: i128 = wrapping_neg_i128(n);
    let sh: i128 = wrapping_shr_i128(m, 2);
    if sh < a || sh >= b {
        sh ^ a
    } else {
        sh | b
    }
}
fn wide_bits_and_match(x: u128, y: u128) -> u64 {
    let z: u128 = wrapping_shl_u128(x & y, 5) ^ wrapping_shr_u128(x | y, 3);
    let c: u32 = leading_zeros_u128(z) ^ trailing_zeros_u128(z) ^ count_ones_u128(z);
    let tag: u64 = match z {
        0 => 11,
        1 => 22,
        _ => 33,
    };
    wrapping_u64(z) ^ widen_u64(c) ^ tag
}
fn main() -> i64 {
    let a: i128 = widen_i128(rt_step(19));
    let b: i128 = widen_i128(wrapping_i64(ct_mix(13, 29)));
    let w: i128 = wide_arith(a, b);
    let u: u64 = wide_bits_and_match(widen_u128(100), widen_u128(200));
    let f: i64 = saturating_i64(df_add(1.5, 2.5));
    wrapping_i64(w) ^ wrapping_i64(u) ^ f
}
";

#[test]
fn vesper_scalar_bodies_part_6d() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6D);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6d scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6d", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6d: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 5 } else { 10 };
        assert!(
            emitted >= min_emitted,
            "part 6d functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6e: `i128` / `u128` multi-function calls, bitwise/wrapping ops, and
/// wide bit builtins (`leading_zeros_u128`, `trailing_zeros_u128`,
/// `count_ones_u128`, `wrapping_shl_u128`, `wrapping_shr_u128`).
const SCALARS_6E: &str = "
fn wide_compound(a: u64, b: u64) -> u128 {
    let mut acc: u128 = widen_u128(a) & 1023;
    let step: u128 = (widen_u128(b) & 63) | 1;
    acc = wrapping_add(acc, step);
    acc = wrapping_sub(acc, 1);
    acc = wrapping_mul(acc, 3);
    acc ^ step
}
fn main() -> i64 {
    let x: u128 = wide_compound(17, 29);
    wrapping_i64(wrapping_u64(x))
}
";

#[test]
fn vesper_scalar_bodies_part_6e() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6E);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6e scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6e", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6e: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 2 } else { 4 };
        assert!(
            emitted >= min_emitted,
            "part 6e functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6f: range-proven signed `%` (`IEX_SREM`), `bit_shr` (`BIT_INTR_SHR`),
/// and call-form bitwise/wide-convert builtins.
const SCALARS_6F: &str = "
fn signed_rem(x: i64) -> i64 {
    if x > -1000 && x < 1000 {
        (x % 7) ^ (x / 7)
    } else {
        0
    }
}
fn shift_right_ops(u: u32, s: i32) -> i64 {
    let a: u32 = bit_shr(u, 2);
    let b: i32 = bit_shr(s, 3);
    widen_i64(a) ^ widen_i64(b)
}
fn main() -> i64 {
    signed_rem(-45) ^ signed_rem(89) ^ shift_right_ops(255, -64)
}
";

#[test]
fn vesper_scalar_bodies_part_6f() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6F);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6f scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6f", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6f: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 2 } else { 4 };
        assert!(
            emitted >= min_emitted,
            "part 6f functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6g: range-proven unsigned `/` (`IEX_UDIV`) and `%` (`IEX_UREM`),
/// `widen_u64`, and `bool` `match` arms.
const SCALARS_6G: &str = "
fn unsigned_div_rem(x: u32, y: u64) -> u64 {
    let q32: u32 = x / 7;
    let r32: u32 = x % 7;
    let q64: u64 = y / 13;
    let r64: u64 = y % 13;
    widen_u64(q32) ^ widen_u64(r32) ^ q64 ^ r64
}
fn bool_pick(c: bool, x: i64) -> i64 {
    if c {
        x ^ 15
    } else {
        x ^ 240
    }
}
fn main() -> i64 {
    let u: u64 = unsigned_div_rem(100, 1000);
    bool_pick(u > 50, wrapping_i64(u))
}
";

#[test]
fn vesper_scalar_bodies_part_6g() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6G);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6g scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6g", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6g: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 2 } else { 4 };
        assert!(
            emitted >= min_emitted,
            "part 6g functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6h: narrow 1-word integer conversions (`u8`/`i8`/`u16`/`i16`/`u32`/`i32`
/// <-> `i64`/`u64`) and wrapping arithmetic (`wrapping_add`/`sub`/`mul`).
const SCALARS_6H: &str = "
fn narrow_converts(x: i64) -> i64 {
    let a: u8 = wrapping_u8(x);
    let b: i8 = wrapping_i8(x);
    let c: u16 = wrapping_u16(x);
    let d: i16 = wrapping_i16(x);
    let e: u32 = wrapping_u32(x);
    let f: i32 = wrapping_i32(x);
    widen_i64(a) ^ widen_i64(b) ^ widen_i64(c) ^ widen_i64(d) ^ widen_i64(e) ^ widen_i64(f)
}
fn wrap_chain(a: i64, b: i64) -> i64 {
    let s: i64 = wrapping_add(a, b);
    let d: i64 = wrapping_sub(s, wrapping_mul(a, 5));
    wrapping_add(d, narrow_converts(d))
}
fn main() -> i64 {
    wrap_chain(123456, -789)
}
";

#[test]
fn vesper_scalar_bodies_part_6h() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6H);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6h scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6h", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6h: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 2 } else { 4 };
        assert!(
            emitted >= min_emitted,
            "part 6h functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6i: `f32`/`f64` `match` arms, `f32` bitcasts/nearest/fma/min/max,
/// narrow saturating float-to-int conversions, `f64` `/=`, and 1-word shifts.
const SCALARS_6I: &str = "
fn f64_div_and_shifts(x: f64, u: u32, s: i32) -> i64 {
    let mut y: f64 = x;
    y /= 2.0;
    let n: f32 = nearest_f32(y);
    let sat: u64 = saturating_u64(y);
    let sh: i64 = widen_i64(u << 2) ^ widen_i64(u >> 1) ^ widen_i64(s >> 2);
    wrapping_i64(sat) ^ widen_i64(to_bits_f32(n)) ^ sh
}
fn main() -> i64 {
    f64_div_and_shifts(5.0, 17, -64)
}
";

#[test]
fn vesper_scalar_bodies_part_6i() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6I);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6i scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6i", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6i: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 2 } else { 4 };
        assert!(
            emitted >= min_emitted,
            "part 6i functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6j: narrow `u8`/`i8`/`u16`/`i16`/`u32`/`i32` wrapping arithmetic and
/// bit builtins across both optimized and unoptimized builds.
const SCALARS_6J: &str = "
fn narrow_wrap_and_bits(a: u8, b: u16, c: i16) -> i64 {
    let x: u8 = wrapping_add(a, 200);
    let y: u16 = wrapping_mul(b, 300);
    let z: i16 = wrapping_sub(c, 500);
    let n: i16 = wrapping_neg_i16(z);
    let b8: u32 = leading_zeros_u8(x) ^ trailing_zeros_u8(x) ^ count_ones_u8(x);
    let b16: u32 = leading_zeros_u16(y) ^ trailing_zeros_u16(y) ^ count_ones_u16(y);
    widen_i64(x) ^ widen_i64(y) ^ widen_i64(n) ^ widen_i64(b8) ^ widen_i64(b16)
}
fn main() -> i64 {
    narrow_wrap_and_bits(77, 1234, -321)
}
";

#[test]
fn vesper_scalar_bodies_part_6j() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6J);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6j scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6j", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6j: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 4 } else { 8 };
        assert!(
            emitted >= min_emitted,
            "part 6j functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Failure records (#2425): `fail_only` functions, which take a trailing
/// failure-record pointer (and, optimized, return a status word): checked
/// arithmetic on narrow and word kinds raising `ArithOverflow`, partial `/`
/// and `%` raising `RuntimeError`, and calls of other `fail_only` functions
/// (one or two instances, an enum or an `i64` error), whose failures are
/// raised again. Functions that `Fail::raise` themselves (`guard`, `code`)
/// and the handlers stay on the Rust walk.
const FAILS: &str = "
enum Oops {
    Small,
    Big,
}
fn add_checked(a: i64, b: i64) -> i64 eff [Fail[ArithOverflow]] {
    a + b
}
fn narrow_mul(a: u8, b: u8) -> u8 eff [Fail[ArithOverflow]] {
    a * b
}
fn guard(x: i64) -> i64 eff [Fail[Oops]] {
    if x < 0 {
        Fail::raise(Oops::Small);
    }
    if x > 100 {
        Fail::raise(Oops::Big);
    }
    wrapping_mul(x, 2)
}
fn both(x: i64, y: i64) -> i64 eff [Fail[Oops], Fail[ArithOverflow]] {
    let g: i64 = guard(x);
    add_checked(g, y)
}
fn mixed(a: u64, b: u32, c: i32, d: i8) -> u64 eff [Fail[ArithOverflow]] {
    let s: u64 = a + widen_u64(b);
    let t: u64 = s * 3 - a;
    let u: i32 = c - 7;
    let v: i8 = d * 2;
    let w: u32 = b + 1;
    t ^ wrapping_u64(widen_i64(u)) ^ wrapping_u64(widen_i64(v)) ^ widen_u64(w)
}
fn divide(a: i64, b: i64, c: u32, d: u32) -> i64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let q: i64 = a / b;
    let r: i64 = a % b;
    let s: u32 = c / d;
    let t: u32 = c % d;
    q ^ r ^ widen_i64(s) ^ widen_i64(t) ^ (q * r)
}
fn code(x: i64) -> i64 eff [Fail[i64]] {
    if x > 9 {
        Fail::raise(x);
    }
    wrapping_add(x, 1)
}
fn chain(x: i64) -> i64 eff [Fail[i64], Fail[ArithOverflow]] {
    let a: i64 = code(x);
    let b: i64 = code(a + 1);
    a * b
}
fn relay(x: i64, y: i64) -> i64 eff [Fail[Oops], Fail[ArithOverflow]] {
    let p: i64 = both(x, y);
    let q: i64 = guard(p);
    p + q
}
fn spin(n: u64) -> u64 eff [Fail[ArithOverflow]] {
    let mut s: u64 = 0;
    let mut i: u64 = 0;
    loop {
        if i >= n {
            break;
        }
        s = s + i * i;
        i = i + 1;
    }
    s
}
fn divide_or(b: i64) -> i64 eff [Fail[ArithOverflow]] {
    handle Fail[RuntimeError] in {
        divide(100, b, 50, 3)
    } with {
        return value => value,
        raise(error, resume) => 77,
    }
}
fn chain_or(x: i64) -> i64 eff [Fail[ArithOverflow]] {
    handle Fail[i64] in {
        chain(x)
    } with {
        return value => value,
        raise(error, resume) => error * 100,
    }
}
fn main() -> i64 {
    handle Fail[ArithOverflow] in {
        handle Fail[Oops] in {
            both(3, 4) ^ widen_i64(narrow_mul(3, 5)) ^ both(-1, 2) ^ relay(5, 6)
                ^ wrapping_i64(mixed(10, 20, 30, 4)) ^ wrapping_i64(spin(50))
                ^ chain_or(4) ^ chain_or(9) ^ divide_or(7) ^ divide_or(0)
        } with {
            return value => value,
            raise(error, resume) => match error {
                Oops::Small => 1000,
                Oops::Big => 2000,
            },
        }
    } with {
        return value => value,
        raise(overflow, resume) => -1,
    }
}
";

#[test]
fn vesper_fail_record_bodies_match() {
    big_stack(|| {
        let parsed = vesper::parser::parse(FAILS);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("fails", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "fails: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        // add_checked, narrow_mul, both, mixed, divide, chain, relay and
        // spin, in both builds (and again for the JIT runs off-cross).
        let min_emitted = if cross() { 16 } else { 32 };
        assert!(
            emitted >= min_emitted,
            "only {emitted} failure-record functions came from Vesper (expected {min_emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// `Fail` handlers (#2657): a handled call to a fallible callee, checked
/// arithmetic raising to a handler in the same function, clauses that
/// capture parameters and locals, handlers in sequence and nested, a raise
/// that passes an inner handler of another type, and an error bound in
/// the `raise` clause.
const HANDLERS: &str = "
fn bump(x: u8) -> u8 eff [Fail[ArithOverflow]] {
    x + 200
}
fn guarded(x: u8) -> u8 {
    handle Fail[ArithOverflow] in {
        bump(x)
    } with {
        return v => v,
        raise(_, _) => 0,
    }
}
fn local(x: u64) -> u64 {
    handle Fail[ArithOverflow] in {
        x * x * x
    } with {
        return v => wrapping_add(v, 1),
        raise(_, _) => 7,
    }
}
fn scaled(x: i64, k: i64) -> i64 {
    let base = wrapping_mul(k, 2);
    handle Fail[ArithOverflow] in {
        x * k + base
    } with {
        return v => wrapping_mul(v, k),
        raise(error, _) => base,
    }
}
fn nested(x: i64) -> i64 {
    handle Fail[RuntimeError] in {
        let y = handle Fail[ArithOverflow] in {
            x * 4611686018427387904
        } with {
            return v => v,
            raise(_, _) => 5,
        };
        handle Fail[ArithOverflow] in {
            y * 3
        } with {
            return v => wrapping_add(v, 1),
            raise(_, _) => -1,
        }
    } with {
        return v => v,
        raise(_, _) => -2,
    }
}
fn crossing(x: i64) -> i64 {
    handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            x * 4611686018427387904 / x
        } with {
            return v => v,
            raise(_, _) => 5,
        }
    } with {
        return v => v,
        raise(_, _) => -1,
    }
}
fn main() -> i64 {
    crossing(0) ^ crossing(1) ^ crossing(4) ^ widen_i64(guarded(10)) ^ widen_i64(guarded(100)) ^ wrapping_i64(local(5)) ^ scaled(2, 3)
        ^ scaled(4611686018427387904, 3) ^ nested(0) ^ nested(1) ^ nested(200)
}
";

#[test]
fn vesper_fail_handlers_match() {
    big_stack(|| {
        let parsed = vesper::parser::parse(HANDLERS);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("handlers", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "handlers: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        // bump, guarded, local, scaled, nested, crossing and main, in both
        // builds (and again for the JIT runs off-cross).
        let min_emitted = if cross() { 14 } else { 28 };
        assert!(
            emitted >= min_emitted,
            "only {emitted} handler functions came from Vesper (expected {min_emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Struct and tuple results through the result pointer (#2411): structs of
/// word, narrow, `bool` and float fields (an `f32` field leaves its word
/// to be zeroed), a tuple, a one-field struct smaller than a word, and a
/// struct built from parameters and arithmetic. Each is built in the
/// result pointer on the optimize path and copied to it otherwise; `main`
/// reads the fields, so it stays with the walk.
const SRET_SOURCE: &str = "
struct Pair {
    a: i64,
    b: u32,
}
struct Mixed {
    flag: bool,
    half: f32,
    whole: f64,
    small: i8,
}
struct Tiny {
    x: u16,
}
fn pair(x: i64) -> Pair {
    Pair { a: wrapping_mul(x, 3), b: wrapping_u32(x) ^ 5 }
}
fn mixed(x: f64, c: bool) -> Mixed {
    let s: i8 = if c { -3 } else { 4 };
    Mixed { flag: !c, half: 1.5, whole: x * 2.0, small: s }
}
fn split(x: u64) -> (u64, f64) {
    (x >> 1, 0.25)
}
fn tiny(x: u16) -> Tiny {
    Tiny { x: x | 1 }
}
fn main() -> i64 {
    let p: Pair = pair(7);
    let m: Mixed = mixed(1.25, true);
    let (q, f) = split(40);
    let t: Tiny = tiny(8);
    let g: i64 = if m.flag || m.whole > 2.0 { 64 } else { 0 };
    let h: i64 = if f < 1.0 && m.half > 1.0 { 128 } else { 0 };
    p.a ^ widen_i64(p.b) ^ widen_i64(m.small) ^ wrapping_i64(q) ^ widen_i64(t.x) ^ g ^ h
}
";

#[test]
fn vesper_struct_results_match() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SRET_SOURCE);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the struct result program does not build: {e}");
        }
        let before = SRET.load(Ordering::Relaxed);
        let total = both("struct_results", &checked.world, true);
        let sret = SRET.load(Ordering::Relaxed) - before;
        eprintln!(
            "struct_results: {}; {sret} struct results emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        // 4 functions, each with both optimizer settings. A cross target's
        // calling convention returns some of them differently (on Windows
        // x64 only 5 bodies take a result pointer).
        let min_sret = if cross() { 4 } else { 8 };
        assert!(
            sret >= min_sret,
            "only {sret} of the struct results came from Vesper (expected >= {min_sret})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Register results (#2581): on the optimize path a function that cannot
/// fail and returns a struct or tuple of at most two words builds it in a
/// local buffer and returns its words in registers (`FnAbi::result_words`):
/// a two-word struct, a one-word struct of narrow fields, a pair of words
/// and a struct with a field chosen by a branch. Unoptimized, the same
/// functions take a result pointer. `main` reads the fields, so it stays
/// with the walk.
const REGISTER_RESULT_SOURCE: &str = "
struct Span {
    start: i64,
    len: u32,
}
struct Bits {
    lo: u16,
    hi: u8,
    on: bool,
}
fn span(x: i64) -> Span {
    Span { start: wrapping_add(x, 9), len: wrapping_u32(x) & 255 }
}
fn bits(x: u16, on: bool) -> Bits {
    Bits { lo: x ^ 3, hi: 7, on: !on }
}
fn halves(x: u64) -> (u64, u64) {
    (x >> 32, x & 4294967295)
}
fn wide(x: i64, c: bool) -> Span {
    let n: u32 = if c { 2 } else { 3 };
    Span { start: wrapping_mul(x, 2), len: n }
}
fn main() -> i64 {
    let s: Span = span(5);
    let b: Bits = bits(40, false);
    let (hi, lo) = halves(77);
    let k: Span = wide(6, true);
    let on: i64 = if b.on { 256 } else { 0 };
    s.start ^ widen_i64(s.len) ^ widen_i64(b.lo) ^ widen_i64(b.hi) ^ wrapping_i64(lo)
        ^ wrapping_i64(hi) ^ k.start ^ widen_i64(k.len) ^ on
}
";

#[test]
fn vesper_register_results_match() {
    big_stack(|| {
        let parsed = vesper::parser::parse(REGISTER_RESULT_SOURCE);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the register result program does not build: {e}");
        }
        let before = REGISTER_RESULT.load(Ordering::Relaxed);
        let total = both("register_results", &checked.world, true);
        let registers = REGISTER_RESULT.load(Ordering::Relaxed) - before;
        eprintln!(
            "register_results: {}; {registers} register results emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        // A cross target makes no JIT run, so each body is emitted once.
        let min_registers = if cross() {
            MIN_REGISTER_RESULTS / 2
        } else {
            MIN_REGISTER_RESULTS
        };
        assert!(
            registers >= min_registers,
            "only {registers} of the register results came from Vesper \
             (expected >= {min_registers})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Register-result bodies `vesper_register_results_match` must see
/// emitted from Vesper: three of `REGISTER_RESULT_SOURCE`'s optimized
/// functions, each compiled for its CLIF text and again for the JIT run
/// (once on a cross target).
const MIN_REGISTER_RESULTS: u64 = 6;

/// Struct results of functions that can fail (#2581): `fail_only`
/// functions returning a struct or tuple through the result pointer, built
/// from checked arithmetic, partial division and a re-raised call. A
/// failure exit returns the word 0 beside its tag, as any return of a
/// result-pointer function does. The handler in `main` stays with the
/// walk.
const FAIL_SRET_SOURCE: &str = "
struct Sum {
    total: i64,
    count: u32,
}
fn sum(a: i64, b: i64) -> Sum eff [Fail[ArithOverflow]] {
    Sum { total: a + b, count: 2 }
}
fn ratio(a: u64, b: u64) -> (u64, u64) eff [Fail[RuntimeError]] {
    (a / b, a % b)
}
fn scaled(a: i64, k: i64) -> Sum eff [Fail[ArithOverflow]] {
    let t: i64 = add_one(a) * k;
    Sum { total: t, count: 1 }
}
fn add_one(x: i64) -> i64 eff [Fail[ArithOverflow]] {
    x + 1
}
fn main() -> i64 {
    handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            let s: Sum = sum(4, 5);
            let (q, r) = ratio(17, 5);
            let k: Sum = scaled(6, 3);
            s.total ^ widen_i64(s.count) ^ wrapping_i64(q) ^ wrapping_i64(r) ^ k.total
        } with {
            return value => value,
            raise(error, resume) => 99,
        }
    } with {
        return value => value,
        raise(error, resume) => 98,
    }
}
";

#[test]
fn vesper_fail_struct_results_match() {
    big_stack(|| {
        let parsed = vesper::parser::parse(FAIL_SRET_SOURCE);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the failing struct result program does not build: {e}");
        }
        let before = SRET.load(Ordering::Relaxed);
        let total = both("fail_struct_results", &checked.world, true);
        let sret = SRET.load(Ordering::Relaxed) - before;
        eprintln!(
            "fail_struct_results: {}; {sret} struct results emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        assert!(
            sret >= MIN_FAIL_STRUCT_RESULTS,
            "only {sret} of the failing struct results came from Vesper \
             (expected >= {MIN_FAIL_STRUCT_RESULTS})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Failing struct-result bodies `vesper_fail_struct_results_match` must
/// see emitted from Vesper: `sum`, `ratio` and `scaled`, in each build at
/// least once (off-cross each is compiled for its CLIF text and again for
/// the object, so 12 are emitted).
const MIN_FAIL_STRUCT_RESULTS: u64 = 6;

/// Small enums as words (#2581): on the optimize path an `Option` or a
/// small enum of word fields is kept as its words (`aggregates::scalar_enums`)
/// and a function that cannot fail returns them in registers. `find` and
/// `sign` return constructors and, recursively, a callee's words; `pick`
/// matches a callee's words. Recursion keeps the calls from being inlined.
const ENUM_WORDS_SOURCE: &str = "
enum Sign {
    Neg,
    Zero,
    Pos(u8),
}
fn find(x: i64) -> Option[i64] {
    if x > 100 {
        Option::None
    } else if x > 3 {
        Option::Some(wrapping_mul(x, 2))
    } else {
        find(wrapping_add(x, 5))
    }
}
fn sign(x: i64, depth: u32) -> Sign {
    if depth > 0 {
        sign(x, wrapping_sub(depth, 1))
    } else if x < 0 {
        Sign::Neg
    } else if x == 0 {
        Sign::Zero
    } else {
        Sign::Pos(7)
    }
}
fn pick(x: i64) -> i64 {
    match find(x) {
        Option::Some(v) => v,
        Option::None => 0,
    }
}
fn weigh(x: i64) -> i64 {
    match sign(x, 2) {
        Sign::Neg => 1,
        Sign::Zero => 2,
        Sign::Pos(n) => widen_i64(n),
    }
}
fn main() -> i64 {
    pick(2) ^ pick(500) ^ weigh(-4) ^ weigh(0) ^ weigh(9)
}
";

#[test]
fn vesper_enum_words_match() {
    big_stack(|| {
        let parsed = vesper::parser::parse(ENUM_WORDS_SOURCE);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the enum words program does not build: {e}");
        }
        let before = REGISTER_RESULT.load(Ordering::Relaxed);
        let emitted_before = EMITTED.load(Ordering::Relaxed);
        let total = both("enum_words", &checked.world, true);
        let registers = REGISTER_RESULT.load(Ordering::Relaxed) - before;
        let emitted = EMITTED.load(Ordering::Relaxed) - emitted_before;
        eprintln!(
            "enum_words: {}; {emitted} functions emitted from Vesper, {registers} returning \
             enum words",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        assert!(
            registers >= MIN_ENUM_WORD_RESULTS,
            "only {registers} enum word results came from Vesper \
             (expected >= {MIN_ENUM_WORD_RESULTS})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Enum-word bodies `vesper_enum_words_match` must see emitted from
/// Vesper: `find` and `sign` on the optimize path, at least once each.
const MIN_ENUM_WORD_RESULTS: u64 = 2;

/// Part 6k: 1-word unary (`~`, `-`, `!`), bitwise (`&`, `|`, `^`),
/// `wrapping_neg_i8`/`i32`/`i64`, and `u64` bit builtins across both
/// optimized and unoptimized builds.
const SCALARS_6K: &str = "
fn unary_and_bits_64(a: i8, b: i32, c: i64, u: u64, flag: bool) -> i64 {
    let n8: i8 = wrapping_neg_i8(a);
    let n32: i32 = wrapping_neg_i32(b);
    let n64: i64 = wrapping_neg_i64(c);
    let inv: u64 = ~u;
    let b64: u32 = leading_zeros_u64(inv) ^ trailing_zeros_u64(inv) ^ count_ones_u64(inv);
    let m: i64 = if !flag {
        widen_i64(n8) | widen_i64(n32)
    } else {
        widen_i64(n8) & widen_i64(n32)
    };
    n64 ^ m ^ widen_i64(b64) ^ wrapping_i64(inv)
}
fn main() -> i64 {
    unary_and_bits_64(-12, 345, -6789, 0x00ff_00ff_00ff_00ff, false)
        ^ unary_and_bits_64(5, -9, 42, 17, true)
}
";

#[test]
fn vesper_scalar_bodies_part_6k() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6K);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6k scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6k", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6k: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 4 } else { 8 };
        assert!(
            emitted >= min_emitted,
            "part 6k functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6l: signed and unsigned integer comparisons across narrow & 64-bit
/// types plus range-proven `+`, `-`, `*` across both optimized and unoptimized
/// builds.
const SCALARS_6L: &str = "
fn cmp_matrix(a: i8, b: u8, c: i32, d: u32, e: i64, f: u64) -> i64 {
    let mut acc: i64 = 0;
    if a < 10 && a >= -20 {
        acc = wrapping_add(acc, 1);
    }
    if b <= 200 && b > 50 {
        acc = wrapping_add(acc, 2);
    }
    if c != 0 && c <= 1000 {
        acc = wrapping_add(acc, 4);
    }
    if d >= 10 && d < 500 {
        acc = wrapping_add(acc, 8);
    }
    if e > -100 && e < 100 && f >= 1 && f <= 1000 {
        let s: i64 = wrapping_sub(wrapping_mul(wrapping_add(e, 10), 2), 3);
        acc = wrapping_add(acc, s);
    }
    acc
}
fn main() -> i64 {
    cmp_matrix(-5, 120, 42, 99, 25, 500) ^ cmp_matrix(50, 10, 0, 600, 200, 0)
}
";

#[test]
fn vesper_scalar_bodies_part_6l() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6L);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6l scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6l", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6l: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 4 } else { 8 };
        assert!(
            emitted >= min_emitted,
            "part 6l functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6m: `()` unit-returning helpers, scalar constants, and recursive
/// scalar calls across both optimized and unoptimized builds.
const SCALARS_6M: &str = "
fn noop(x: i64) -> () {
    let _ = x;
    ()
}
fn fib(n: i64) -> i64 {
    if n <= 1 {
        n
    } else {
        wrapping_add(fib(wrapping_sub(n, 1)), fib(wrapping_sub(n, 2)))
    }
}
fn main() -> i64 {
    noop(42);
    fib(10)
}
";

#[test]
fn vesper_scalar_bodies_part_6m() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6M);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6m scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6m", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6m: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 3 } else { 6 };
        assert!(
            emitted >= min_emitted,
            "part 6m functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6n: nested `loop`s with `break`/`continue`, early `return` from a
/// nested loop, and `f64` compound writes inside loops across both optimized
/// and unoptimized builds.
const SCALARS_6N: &str = "
fn nested_loops(rows: u32, cols: u32, stop: i64) -> i64 {
    let mut sum: i64 = 0;
    let mut acc: f64 = 1.0;
    let mut r: u32 = 0;
    loop {
        if r >= rows {
            break;
        }
        let mut c: u32 = 0;
        loop {
            if c >= cols {
                break;
            }
            c = wrapping_add(c, 1);
            if c == 2 {
                continue;
            }
            sum = wrapping_add(sum, widen_i64(wrapping_add(r, c)));
            acc += 0.25;
            if sum > stop {
                return sum ^ saturating_i64(acc);
            }
        }
        acc *= 1.5;
        r = wrapping_add(r, 1);
    }
    sum ^ saturating_i64(acc)
}
fn main() -> i64 {
    nested_loops(4, 5, 100) ^ nested_loops(4, 5, 12)
}
";

#[test]
fn vesper_scalar_bodies_part_6n() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6N);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6n scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6n", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6n: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 4 } else { 8 };
        assert!(
            emitted >= min_emitted,
            "part 6n functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6o: `bool` `==`/`!=`, integer-to-float (`widen_f64`/`widen_f32`/
/// `nearest_f32`/`nearest_f64`), and saturating float-to-int conversions across
/// both optimized and unoptimized builds.
const SCALARS_6O: &str = "
fn int_float_roundtrip(a: i32, b: u32, c: i64, d: u64, p: bool, q: bool) -> i64 {
    let f1: f64 = widen_f64(a) + widen_f64(b);
    let f2: f64 = nearest_f64(c) - nearest_f64(d);
    let f3: f32 = nearest_f32(a) + nearest_f32(b);
    let total: f64 = sqrt(abs(f1 + f2)) + widen_f64(f3);
    let s8: i8 = saturating_i8(total);
    let u8v: u8 = saturating_u8(total);
    let s16: i16 = saturating_i16(total);
    let u16v: u16 = saturating_u16(total);
    let s32: i32 = saturating_i32(total);
    let u32v: u32 = saturating_u32(total);
    let eq_bit: i64 = if p == q { 1 } else { 0 };
    let ne_bit: i64 = if p != q { 2 } else { 0 };
    widen_i64(s8) ^ widen_i64(u8v) ^ widen_i64(s16) ^ widen_i64(u16v) ^ widen_i64(s32)
        ^ widen_i64(u32v) ^ eq_bit ^ ne_bit
}
fn main() -> i64 {
    int_float_roundtrip(-15, 42, 1000, 250, true, false)
        ^ int_float_roundtrip(7, 9, -50, 12, true, true)
}
";

#[test]
fn vesper_scalar_bodies_part_6o() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6O);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6o scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6o", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6o: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 4 } else { 8 };
        assert!(
            emitted >= min_emitted,
            "part 6o functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6p: multi-capability erased effect rows (`eff [ConstTime, Realtime]`),
/// mixed-lane direct calls (`f32`, `f64`, `i128`, `u128`, `bool`, `u8`, `i16`),
/// and full `f32`/`f64` comparison + unary `-` coverage across both optimized
/// and unoptimized builds.
const SCALARS_6P: &str = "
fn ct_rt_mix(a: u8, b: i16, w: i128, u: u128) -> i128 eff [ConstTime, Realtime] {
    let base: i128 = wrapping_add(w, widen_i128(b));
    let mask: i128 = widen_i128(a);
    wrapping_sub(base ^ mask, wrapping_i128(u & 255))
}
fn flt_cmp_all(a: f32, b: f32, x: f64, y: f64) -> i64 {
    let mut acc: i64 = 0;
    if -a < b && a <= b {
        acc = wrapping_add(acc, 1);
    }
    if a != b && b > a {
        acc = wrapping_add(acc, 2);
    }
    if -x < y && x <= y && y >= x && x != y {
        acc = wrapping_add(acc, 4);
    }
    if a == a && x == x {
        acc = wrapping_add(acc, 8);
    }
    acc
}
fn main() -> i64 {
    let w: i128 = ct_rt_mix(17, -42, widen_i128(1000), widen_u128(99));
    let c: i64 = flt_cmp_all(1.25, 2.5, 3.5, 4.75);
    wrapping_i64(w) ^ c
}
";

#[test]
fn vesper_scalar_bodies_part_6p() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6P);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6p scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6p", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6p: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 4 } else { 8 };
        assert!(
            emitted >= min_emitted,
            "part 6p functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6q: `f32` math intrinsics (`sqrt`/`abs`/`floor`/`ceil`/`round`/`trunc`/
/// `min`/`max`/`fma`), `f32` compound writes (`+=`/`-=`/`*=`/`/=`), and `u128`
/// multi-branch recursive functions across both optimized and unoptimized
/// builds.
const SCALARS_6Q: &str = "
fn f32_math_all(a: f32, b: f32) -> i64 {
    let mut x: f32 = abs(a);
    x += b;
    x -= 0.5;
    x *= 2.0;
    x /= 4.0;
    let s: f32 = sqrt(x);
    let m: f32 = fma(s, 0.5, min(floor(x), max(ceil(x), round(trunc(x)))));
    widen_i64(to_bits_f32(m))
}
fn u128_step(n: u128, acc: u128) -> u128 {
    if n == 0 {
        acc
    } else if n <= 2 {
        wrapping_add(acc, n)
    } else if n > 5 && n != 7 {
        u128_step(wrapping_sub(n, 2), wrapping_add(acc, n))
    } else {
        u128_step(wrapping_sub(n, 1), acc ^ n)
    }
}
fn main() -> i64 {
    let f: i64 = f32_math_all(-3.5, 5.25);
    let u: u128 = u128_step(widen_u128(9), widen_u128(1));
    f ^ wrapping_i64(wrapping_u64(u))
}
";

#[test]
fn vesper_scalar_bodies_part_6q() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6Q);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6q scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6q", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6q: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 3 } else { 6 };
        assert!(
            emitted >= min_emitted,
            "part 6q functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6r: `Op::Project` scalar field projection on `LANE_PRODUCT` struct and
/// tuple values across both optimized and unoptimized builds.
const SCALARS_6R: &str = "
struct Pair {
    x: i64,
    y: f64,
    z: u32,
}
fn make_pair(x: i64, y: f64, z: u32) -> Pair {
    Pair { x: x, y: y, z: z }
}
fn project_pair(a: i64, b: f64, c: u32) -> i64 {
    let x: i64 = make_pair(a, b, c).x;
    let y: f64 = make_pair(a, b, c).y;
    let z: u32 = make_pair(a, b, c).z;
    wrapping_add(x, widen_i64(z)) ^ saturating_i64(y)
}
fn main() -> i64 {
    project_pair(100, 23.5, 42)
}
";

#[test]
fn vesper_scalar_bodies_part_6r() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6R);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6r scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6r", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6r: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 2 } else { 4 };
        assert!(
            emitted >= min_emitted,
            "part 6r functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6s: `INST_PROJECT` on `LANE_PRODUCT` struct literals with `i64` and
/// `f64` fields, asserting that the projecting function itself emits from
/// Vesper with 0 CLIF/object/run differences.
const SCALARS_6S: &str = "
struct Record {
    a: i64,
    b: f64,
    c: i64,
}
fn project_literal(x: i64, y: f64, z: i64) -> i64 {
    let a: i64 = Record { a: x, b: y, c: z }.a;
    let b: f64 = Record { a: x, b: y, c: z }.b;
    let c: i64 = Record { a: x, b: y, c: z }.c;
    wrapping_add(a, c) ^ saturating_i64(b)
}
fn main() -> i64 {
    project_literal(100, 12.5, 33) ^ project_literal(50, -4.5, -11)
}
";

#[test]
fn vesper_scalar_bodies_part_6s() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6S);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6s scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6s", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6s: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 2 } else { 4 };
        assert!(
            emitted >= min_emitted,
            "part 6s functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6t: `LANE_PRODUCT` `in` parameters and `Op::Read` scalar field paths
/// (`INST_READ_FIELD`), including single-step and nested field reads.
const SCALARS_6T: &str = "
struct Vec2 {
    x: i64,
    y: f64,
}
struct Nested {
    a: Vec2,
    z: i64,
}
fn read_vec(v: Vec2) -> i64 {
    wrapping_add(v.x, saturating_i64(v.y))
}
fn read_nested(n: Nested) -> i64 {
    wrapping_add(n.a.x, n.z) ^ saturating_i64(n.a.y)
}
fn main() -> i64 {
    let v: Vec2 = Vec2 { x: 40, y: 2.5 };
    let n: Nested = Nested { a: Vec2 { x: 100, y: 7.5 }, z: 23 };
    read_vec(v) ^ read_nested(n)
}
";

#[test]
fn vesper_scalar_bodies_part_6t() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6T);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6t scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6t", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6t: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 2 } else { 4 };
        assert!(
            emitted >= min_emitted,
            "part 6t functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6u: `LANE_PRODUCT` local variables (`Op::Bind`), `Op::Read` scalar
/// field paths on product locals, and `Pass::In` `Op::Read` of product locals.
const SCALARS_6U: &str = "
struct Vec2 {
    x: i64,
    y: f64,
}
fn local_product_fields(a: i64, b: f64) -> i64 {
    let p: Vec2 = Vec2 { x: a, y: b };
    wrapping_add(p.x, saturating_i64(p.y))
}
fn main() -> i64 {
    local_product_fields(100, 12.5) ^ local_product_fields(-30, 4.5)
}
";

#[test]
fn vesper_scalar_bodies_part_6u() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6U);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6u scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6u", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6u: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 2 } else { 4 };
        assert!(
            emitted >= min_emitted,
            "part 6u functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Part 6v: `PASS_SINK` `Op::Read` of `copy` `LANE_PRODUCT` values and
/// `Op::Data` with nested `LANE_PRODUCT` fields (`STORE_AGGREGATE`).
const SCALARS_6V: &str = "
copy struct Inner {
    x: i64,
    y: f64,
}
fn copy_and_read(a: i64, b: f64) -> i64 {
    let p: Inner = Inner { x: a, y: b };
    let q: Inner = p;
    wrapping_add(q.x, saturating_i64(q.y))
}
fn main() -> i64 {
    copy_and_read(100, 12.5) ^ copy_and_read(-30, 4.5)
}
";

#[test]
fn vesper_scalar_bodies_part_6v() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6V);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the part 6v scalar program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6v", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6v: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 2 } else { 4 };
        assert!(
            emitted >= min_emitted,
            "part 6v functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// `inout` scalar parameters (#2563): each is loaded at entry, read through
/// its address and stored through it at every definition, with a loop, a
/// narrow integer, a `bool`, a float and a compound write. `bump` and
/// `narrow` lend their own `inout` parameters on to themselves, and `twice`
/// and `main` lend register locals (a scratch word on the optimize path);
/// recursion keeps each callee a call through `ir_inline`.
const INOUT_SOURCE: &str = "
fn steps(n: u64) -> i64 {
    if n == 0 {
        return 0;
    }
    wrapping_add(steps(wrapping_sub(n, 1)), 1)
}
fn bump(inout x: i64, by: i64, n: u64) -> i64 {
    if n == 0 {
        return x;
    }
    let mut i: i64 = 0;
    loop {
        if i >= by {
            break;
        }
        x = wrapping_add(x, i);
        if x > 100 {
            x = wrapping_sub(x, 7);
        }
        i = wrapping_add(i, 1);
    }
    x = x ^ 5;
    bump(inout x, by, wrapping_sub(n, 1))
}
fn narrow(inout b: u8, inout flag: bool, n: u64) -> u8 {
    if n == 0 {
        return b;
    }
    b = b | 1;
    b = b & 127;
    flag = !flag;
    narrow(inout b, inout flag, wrapping_sub(n, 1))
}
fn scale(inout f: f64, k: f64, n: u64) -> f64 {
    if n == 0 {
        return f;
    }
    f = f * k + 0.5;
    f *= 0.75;
    if steps(n) > 3 { f } else { k }
}
fn twice(v: i64) -> i64 {
    let mut t: i64 = v;
    let r: i64 = bump(inout t, 2, 2);
    wrapping_add(r, t)
}
fn main() -> i64 {
    let mut x: i64 = 3;
    let mut b: u8 = 4;
    let mut flag: bool = false;
    let mut f: f64 = 1.5;
    let r: i64 = bump(inout x, 9, 3);
    let c: u8 = narrow(inout b, inout flag, 3);
    let g: f64 = scale(inout f, 1.25, 4);
    let h: i64 = if flag && g > 2.0 { 64 } else { 0 };
    r ^ x ^ widen_i64(c) ^ widen_i64(b) ^ h ^ twice(11)
}
";

#[test]
fn vesper_inout_scalar_parameters_match() {
    big_stack(|| {
        let parsed = vesper::parser::parse(INOUT_SOURCE);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the inout program does not build: {e}");
        }
        let (pointers_before, emitted_before) = (
            POINTERS.load(Ordering::Relaxed),
            EMITTED.load(Ordering::Relaxed),
        );
        let total = both("inout_scalars", &checked.world, true);
        let pointers = POINTERS.load(Ordering::Relaxed) - pointers_before;
        let emitted = EMITTED.load(Ordering::Relaxed) - emitted_before;
        eprintln!(
            "inout_scalars: {}; {pointers} bodies with inout scalars emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        // A cross target runs nothing, so each body is counted once per
        // optimizer setting instead of twice.
        let runs = if cross() { 1 } else { 2 };
        let min_inout = MIN_INOUT_BODIES * runs;
        let min_program = MIN_INOUT_PROGRAM_BODIES * runs;
        assert!(
            pointers >= min_inout,
            "only {pointers} inout bodies came from Vesper (expected >= {min_inout})"
        );
        // Every function, `twice` and `main` too (their locals lent
        // through scratch words on the optimize path).
        assert!(
            emitted >= min_program,
            "only {emitted} bodies came from Vesper (expected >= {min_program})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// Every body of `INOUT_SOURCE`'s six functions at both optimizer
/// settings, counted once per comparison as `MIN_INOUT_BODIES` is.
const MIN_INOUT_PROGRAM_BODIES: u64 = 12;

/// The bodies of `INOUT_SOURCE` with an `inout` parameter that must come
/// from Vesper: `bump`, `narrow` and `scale` at both optimizer settings,
/// once per comparison (a JIT-running host compares each setting twice).
const MIN_INOUT_BODIES: u64 = 6;

/// Strings and slices (#2653): `Str` and `String` `len`, `as_str`,
/// `as_bytes` and `byte_at` (raising `IndexOutOfBounds` through the
/// failure record), and slice `len` and checked elements of each stride
/// (`u8`, `u16`, `u32`, `i32`, `u64`, `f64`). `main` builds the sequences
/// and handles the raise, so it stays with the walk. Optimized, a slice
/// parameter is read once at entry (`bounds_hoist::header_hoists`), its
/// header words then loaded as read-only memory.
const STRINGS_SLICES: &str = "
fn text_len(in s: String) -> u64 {
    s.len()
}
fn view_len(in s: Str) -> u64 {
    wrapping_add(s.len(), 1)
}
fn whole_len(in s: String) -> u64 {
    view_len(in s.as_str())
}
fn bytes_len(in s: Str) -> u64 {
    words_of(in s.as_bytes())
}
fn words_of(in xs: Slice[u8]) -> u64 {
    wrapping_mul(xs.len(), 3)
}
fn byte_of(in s: Str, i: u64) -> u64 eff [Fail[RuntimeError]] {
    widen_u64(s.byte_at(i))
}
fn byte_pair(in s: String, i: u64) -> u64 eff [Fail[RuntimeError]] {
    let a: u8 = s.byte_at(i);
    let b: u8 = s.byte_at(wrapping_add(i, 1));
    widen_u64(a) ^ wrapping_mul(widen_u64(b), 256)
}
fn pick_word(in xs: Slice[u64], i: u64) -> u64 eff [Fail[RuntimeError]] {
    wrapping_add(xs[i], xs.len())
}
fn pick_small(in xs: Slice[u8], in ys: Slice[u32], i: u64) -> u64 eff [Fail[RuntimeError]] {
    wrapping_add(widen_u64(xs[i]), widen_u64(ys[i]))
}
fn pick_half(in xs: Slice[u16], in zs: Slice[i32], i: u64) -> i64 eff [Fail[RuntimeError]] {
    wrapping_add(widen_i64(xs[i]), widen_i64(zs[i]))
}
fn pick_float(in fs: Slice[f64], i: u64) -> f64 eff [Fail[RuntimeError]] {
    fs[i] * 2.0
}
fn main() -> i64 {
    let s: String = \"hello, world\";
    let mut w: ChunkSeq[u64] = ChunkSeq::new[u64]();
    w.push(7);
    w.push(11);
    let mut h: ChunkSeq[u16] = ChunkSeq::new[u16]();
    h.push(300);
    h.push(40000);
    let mut q: ChunkSeq[u32] = ChunkSeq::new[u32]();
    q.push(70000);
    q.push(5);
    let mut z: ChunkSeq[i32] = ChunkSeq::new[i32]();
    z.push(-3);
    z.push(9);
    let mut f: ChunkSeq[f64] = ChunkSeq::new[f64]();
    f.push(1.5);
    f.push(-2.25);
    let lens = text_len(in s) ^ view_len(in \"abc\") ^ whole_len(in s) ^ bytes_len(in \"wxyz\");
    handle Fail[RuntimeError] in {
        let a = byte_of(in \"abc\", 1) ^ byte_pair(in s, 3) ^ pick_word(in w.as_slice(), 1);
        let b = pick_small(in s.as_bytes(), in q.as_slice(), 0) ^ lens;
        let c = pick_half(in h.as_slice(), in z.as_slice(), 1);
        let d = saturating_i64(pick_float(in f.as_slice(), 1));
        let e = byte_of(in \"abc\", 3);
        wrapping_i64(a ^ b) ^ c ^ d ^ wrapping_i64(e)
    } with {
        return value => value,
        raise(error, resume) => 4242,
    }
}
";

#[test]
fn vesper_string_and_slice_bodies_match() {
    big_stack(|| {
        let parsed = vesper::parser::parse(STRINGS_SLICES);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("strings_slices", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "strings_slices: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        // The string and slice functions in both builds (and again for
        // the JIT runs off-cross).
        let min_emitted = if cross() { 20 } else { 40 };
        assert!(
            emitted >= min_emitted,
            "only {emitted} string and slice functions came from Vesper (expected {min_emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// A slice's `last` (#2653), whose `Option` the optimize path keeps as
/// its tag and payload words (`aggregates::scalar_enums`, `seq_option`),
/// on `u8`, `u32` and `i64` elements and an empty slice. Unoptimized the
/// `Option` is built in memory and matched there, which stays with the
/// walk.
const SLICE_LAST: &str = "
fn last_or(in xs: Slice[u32]) -> u64 {
    match xs.last() {
        Option::Some(x) => widen_u64(x),
        Option::None => 7,
    }
}
fn last_byte(in xs: Slice[u8]) -> u64 {
    match xs.last() {
        Option::Some(x) => wrapping_mul(widen_u64(x), 3),
        Option::None => 11,
    }
}
fn last_word(in xs: Slice[i64], d: i64) -> i64 {
    match xs.last() {
        Option::Some(x) => wrapping_add(x, d),
        Option::None => d,
    }
}
fn main() -> i64 {
    let mut q: ChunkSeq[u32] = ChunkSeq::new[u32]();
    q.push(70000);
    q.push(5);
    let e: ChunkSeq[u32] = ChunkSeq::new[u32]();
    let mut w: ChunkSeq[i64] = ChunkSeq::new[i64]();
    w.push(-40);
    let s: String = \"hello\";
    let a = last_or(in q.as_slice()) ^ last_or(in e.as_slice());
    let b = last_byte(in s.as_bytes()) ^ last_byte(in \"\".as_bytes());
    let c = last_word(in w.as_slice(), 3);
    wrapping_i64(a ^ wrapping_mul(b, 3)) ^ c
}
";

#[test]
fn vesper_slice_last_matches() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SLICE_LAST);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("slice_last", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "slice_last: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        // The three functions optimized (and again for the JIT run
        // off-cross).
        let min_emitted = if cross() { 3 } else { 6 };
        assert!(
            emitted >= min_emitted,
            "only {emitted} slice `last` functions came from Vesper (expected {min_emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

// String literals (`Codegen::string_literal`): empty, under a word, a
// whole word and longer, read through `len` and `byte_at`.
const STRING_LITERALS: &str = "
fn empty_len() -> u64 {
    \"\".len()
}
fn fresh() -> String {
    \"\"
}
fn named() -> String {
    \"named\"
}
fn short_byte(i: u64) -> u64 eff [Fail[RuntimeError]] {
    widen_u64(\"hey\".byte_at(i))
}
fn word_len(d: u64) -> u64 {
    wrapping_add(\"abcdefgh\".len(), d)
}
fn long_byte(i: u64) -> u64 eff [Fail[RuntimeError]] {
    widen_u64(\"the quick brown fox\".byte_at(i))
}
fn main() -> i64 {
    handle Fail[RuntimeError] in {
        let a = wrapping_add(empty_len(), short_byte(1));
        let b = wrapping_add(word_len(2), long_byte(10));
        let c = short_byte(3);
        let d = wrapping_add(fresh().len(), named().len());
        wrapping_i64(a ^ wrapping_mul(b, 7) ^ c ^ d)
    } with {
        return value => value,
        raise(error, resume) => 4242,
    }
}
";

#[test]
fn vesper_string_literals_match() {
    big_stack(|| {
        let parsed = vesper::parser::parse(STRING_LITERALS);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("string_literals", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "string_literals: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        // The four literal functions in both worlds (and again for the
        // JIT run off-cross).
        let min_emitted = if cross() { 8 } else { 16 };
        assert!(
            emitted >= min_emitted,
            "only {emitted} string literal functions came from Vesper (expected {min_emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// #2654: struct and tuple patterns (`Codegen::bind_ir` and
/// `pattern_condition` of a product) and enums kept in memory: their
/// constructors, enum locals and results, and constructor arms with and
/// without fields.
const SCALARS_6W: &str = "
copy struct Pt {
    x: i64,
    y: i64,
}
copy enum Shape {
    Dot,
    Rect(i64, i64),
    Tagged(i64, bool)
}
fn span(p: Pt) -> i64 {
    let Pt { x, y } = p;
    wrapping_sub(y, x)
}
fn pick(a: i64, b: bool) -> i64 {
    let t = (a, b);
    match t {
        (v, true) => v,
        (v, false) => wrapping_add(v, 1),
    }
}
fn shape(w: i64, h: i64) -> Shape {
    Rect(w, h)
}
fn area(s: Shape) -> i64 {
    match s {
        Dot => 0,
        Rect(w, h) => wrapping_mul(w, h),
        Tagged(n, b) => if b { n } else { 0 },
    }
}
fn main() -> i64 {
    let p = Pt { x: 3, y: 10 };
    let d = Dot;
    let t = Tagged(9, true);
    wrapping_add(
        wrapping_add(span(p), pick(5, false)),
        wrapping_add(area(shape(6, 7)), wrapping_add(area(d), area(t)))
    )
}
";

#[test]
fn vesper_scalar_bodies_product_patterns_and_enums() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6W);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the #2654 program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6w", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6w: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 4 } else { 8 };
        assert!(
            emitted >= min_emitted,
            "#2654 functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// #2654 slice 2: an instance of a generic struct, a struct joined at a
/// block parameter, and direct calls whose struct or enum result is
/// written through the storage the caller passes (`SRET_CALL`).
const SCALARS_6X: &str = "
copy struct Sealed[T] {
    value: T
}
copy struct Pt {
    x: i64,
    y: i64,
}
copy enum Side {
    Left,
    Right(i64)
}
fn reveal(s: Sealed[u64]) -> u64 {
    let Sealed { value } = s;
    value
}
fn hide(v: u64) -> Sealed[u64] {
    Sealed { value: v }
}
fn corner(c: bool, a: i64) -> Pt {
    let p = if c { Pt { x: a, y: 0 } } else { Pt { x: 0, y: a } };
    p
}
fn side(n: i64) -> Side {
    if n > 0 { Right(n) } else { Left }
}
fn weigh(n: i64) -> i64 {
    let p = corner(n > 2, n);
    match side(n) {
        Left => p.y,
        Right(k) => wrapping_add(k, p.x),
    }
}
fn main() -> i64 {
    let s = hide(41);
    let r = if reveal(s) == 41 { 1 } else { 0 };
    wrapping_add(wrapping_add(weigh(5), weigh(-3)), r)
}
";

#[test]
fn vesper_scalar_bodies_generic_products_joins_and_struct_calls() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SCALARS_6X);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        if let Err(e) = build(&checked.world, "main", true) {
            panic!("the #2654 slice 2 program does not build: {e}");
        }
        let before = EMITTED.load(Ordering::Relaxed);
        let total = both("scalars_6x", &checked.world, true);
        let emitted = EMITTED.load(Ordering::Relaxed) - before;
        eprintln!(
            "scalars_6x: {}; {emitted} functions emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        let min_emitted = if cross() { 4 } else { 8 };
        assert!(
            emitted >= min_emitted,
            "#2654 slice 2 functions should come from Vesper (expected >= {min_emitted}, got {emitted})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// `inout` arguments to `fail_only` callees (#2614), next to struct results
/// of `fail_only` functions built in the result pointer, with checked arithmetic, a partial division and a call of
/// another `fail_only` function raising through the failure record, whose
/// exits return a zero of the pointer type. `grown` lends a local `inout`
/// to the `fail_only` `grow`, so the local keeps a home slot on the
/// optimize path too, and `grow` stores through its own `inout` parameter.
const SRET_FAIL_SOURCE: &str = "
struct Pair {
    a: i64,
    b: u32,
}
struct Trio {
    q: i64,
    r: i64,
    big: bool,
}
fn add(x: i64, y: i64) -> i64 eff [Fail[ArithOverflow]] {
    x + y
}
fn pair_checked(x: i64, y: i64) -> Pair eff [Fail[ArithOverflow]] {
    Pair { a: x * y, b: wrapping_u32(x) ^ 9 }
}
fn trio(x: i64, d: i64) -> Trio eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let q: i64 = x / d;
    let r: i64 = x % d;
    Trio { q: q + 1, r: r, big: q > 10 }
}
fn relay(x: i64, y: i64) -> Pair eff [Fail[ArithOverflow]] {
    let s: i64 = add(x, y);
    Pair { a: s - 3, b: 4 }
}
fn grow(inout x: i64, by: i64) -> i64 eff [Fail[ArithOverflow]] {
    x = x * by;
    x + 1
}
fn grown(seed: i64) -> Pair eff [Fail[ArithOverflow]] {
    let mut v: i64 = seed;
    let a: i64 = grow(inout v, 3);
    let b: i64 = grow(inout v, 5);
    Pair { a: a + v, b: wrapping_u32(b) }
}
fn total(n: i64) -> i64 eff [Fail[ArithOverflow]] {
    let mut s: i64 = 0;
    let mut i: i64 = 0;
    loop {
        if i >= n {
            break;
        }
        s = add(s, i);
        i = i + 1;
    }
    s
}
fn main() -> i64 {
    handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            let p: Pair = pair_checked(total(6), 7);
            let t: Trio = trio(100, 7);
            let z: Trio = trio(1, 0);
            let r: Pair = relay(9223372036854775807, 1);
            let w: Pair = grown(4);
            let o: Pair = grown(4611686018427387904);
            let g: i64 = if t.big { 1 } else { 0 };
            p.a ^ widen_i64(p.b) ^ t.q ^ t.r ^ g ^ z.q ^ r.a ^ w.a ^ widen_i64(w.b) ^ o.a
        } with {
            return value => value,
            raise(error, resume) => 500,
        }
    } with {
        return value => value,
        raise(overflow, resume) => -1,
    }
}
";

#[test]
fn vesper_inout_arguments_to_fail_only_callees_match() {
    big_stack(|| {
        let parsed = vesper::parser::parse(SRET_FAIL_SOURCE);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let before = SRET.load(Ordering::Relaxed);
        let total = both("fail_struct_results", &checked.world, true);
        let sret = SRET.load(Ordering::Relaxed) - before;
        eprintln!(
            "fail_struct_results: {}; {sret} struct results emitted from Vesper",
            total.summary()
        );
        let ran = if cross() { 0 } else { 2 };
        assert_eq!((total.worlds, total.ran), (2, ran), "{}", total.summary());
        // `pair_checked`, `trio`, `relay` and `grown` at both optimizer settings,
        // once per comparison (a JIT-running host compares each twice). A
        // cross target may return some in registers instead.
        let min_sret = if cross() {
            MIN_FAIL_SRET / 2
        } else {
            MIN_FAIL_SRET
        };
        assert!(
            sret >= min_sret,
            "only {sret} fail_only struct results came from Vesper (expected >= {min_sret})"
        );
        assert!(total.is_clean(), "{}", total.summary());
    });
}

/// `SRET_FAIL_SOURCE`'s four struct results, both settings, both
/// comparisons of a JIT-running host.
const MIN_FAIL_SRET: u64 = 16;
