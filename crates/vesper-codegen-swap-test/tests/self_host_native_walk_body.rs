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
//! block bound. The Rust walk asks only for a plain frame (the optimize
//! path), so optimize off is compared too but emits nothing from Vesper.
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
    // `[status, source locations, parameter variables...]` from the walk.
    let [status, locations, vars @ ..] = lent.args else {
        return Err("a body request without its flags".into());
    };
    let mut args = vec![
        u64::from(lent.optimize),
        *status,
        *locations,
        vars.len() as u64,
    ];
    args.extend_from_slice(vars);
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
        1 => {
            EMITTED.fetch_add(1, Ordering::Relaxed);
            Ok(true)
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
        let before = [&EMITTED, &UNPLANNED].map(|c| c.load(Ordering::Relaxed));
        let worlds: Vec<_> = check_harness::corpus()
            .iter()
            .filter(|c| c.world.functions.contains_key("main"))
            .step_by(step())
            .collect();
        let mut total = Diff::default();
        for (i, case) in worlds.iter().enumerate() {
            total.add(both(&case.name, &case.world, runs(i, case)));
        }
        let [emitted, unplanned] = [&EMITTED, &UNPLANNED].map(|c| c.load(Ordering::Relaxed));
        let (emitted, unplanned) = (emitted - before[0], unplanned - before[1]);
        eprintln!(
            "native walk bodies: {}; {emitted} functions emitted from Vesper, \
             {unplanned} left to Rust",
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
