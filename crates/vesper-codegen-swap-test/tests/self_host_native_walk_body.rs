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
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;
#[path = "../../../tests/common/check_words.rs"]
mod check_words;
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

/// One program's body plans, by function name.
struct Plan {
    key: (usize, usize, usize),
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

fn plan_of(world: &vesper::check::World, program: &Program) -> Plan {
    let key = program_key(program);
    let mut bodies = HashMap::new();
    // A program the format cannot encode stays on the Rust walk.
    if let Some((input, _)) = ir_words::program_input(world, program) {
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
        if cell
            .as_ref()
            .is_none_or(|p| p.key != program_key(lent.program))
        {
            *cell = Some(plan_of(lent.world, lent.program));
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
    let callees =
        codegen_swap::body::callees(lent.args).ok_or("a body request without its callees")?;
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
        let counters = [&EMITTED, &UNPLANNED, &WITH_CALLS, &CALLS, &CALL_DECLINED];
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
        let [emitted, unplanned, with_calls, calls, call_declined] =
            [0, 1, 2, 3, 4].map(|i| after[i] - before[i]);
        eprintln!(
            "native walk bodies: {}; {emitted} functions emitted from Vesper \
             ({with_calls} with {calls} direct calls), {unplanned} left to Rust, \
             {call_declined} declined for a call",
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
