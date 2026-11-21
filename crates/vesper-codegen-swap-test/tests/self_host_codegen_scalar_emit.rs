//! Self-hosting SH4 lane 6 (#1280), part 2: scalar emission from Vesper
//! through the `scalar` hook of lane 1's codegen swap point
//! (src/codegen_swap.rs), compared with the Rust lowering per function
//! (CLIF text), as object bytes and as JIT runs, optimize on and off.
//!
//! The Vesper side has two halves:
//!
//! - `codegen_scalar_plan` (@vesper/codegen-scalar) reads the program the
//!   native backend is emitting, as Place IR words with the checker's
//!   world, and plans each instruction it emits: the `expr_at` arm, the
//!   condition code or operation, and the narrow kind `coerce` reduces
//!   the result to. Instructions it does not plan stay on the Rust code.
//! - `scalar_hook` (@vesper/codegen-scalar-emit) emits one planned
//!   instruction over the lent builder, checking the operand lanes that
//!   only the builder knows.
//!
//! The glue here ([`scalar`]) only joins them: it encodes the lent program
//! once, looks the request's site up in the plan and passes the plan's
//! words, the optimizer flag (`Lent::optimize`) and the operand values to the hook.
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
use vesper::codegen_swap::{self, CodegenHooks, Lent, NO_VALUE};
use vesper::ir::Program;
use vesper::native::WordFunction;
use vesper_codegen_swap_test::*;

/// The plan's words per record: `[function, block, instruction, request,
/// a, b]`.
const RECORD: usize = 6;
/// `scalar_hook`'s numbers for the raising arms' tests.
const TEST_OVERFLOW: u32 = 11;
const TEST_DIVISOR: u32 = 12;

/// The plan entry and the hook, compiled once with no hook in effect (a
/// hook compiled under its own `with_hooks` would wait on itself).
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
                plan: package.entry("codegen_scalar_plan", true),
                hook: hook_function(&checked, "scalar_hook"),
            }
        })
    })
}

/// One program's plan: per `(function, block, instruction)`, the request
/// and its two immediates.
struct Plan {
    key: (usize, usize, usize),
    sites: HashMap<(String, u32, u32), (u32, u64, u64)>,
}

thread_local! {
    static PLAN: RefCell<Option<Plan>> = const { RefCell::new(None) };
    /// Unplanned requests by IR operation (the report's coverage table).
    static LEFT: RefCell<std::collections::BTreeMap<String, u64>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
}

/// The IR operation at `lent`'s site, by variant name (with the operator
/// of a `Unary` or `Binary`).
fn op_name(lent: &Lent<'_, '_>) -> String {
    let inst = lent
        .program
        .functions
        .get(lent.site.function)
        .and_then(|f| f.blocks.get(lent.site.block as usize))
        .and_then(|b| b.instructions.get(lent.site.inst as usize));
    match inst.map(|i| &i.op) {
        Some(vesper::ir::Op::Unary(op, _)) => format!("Unary {op}"),
        Some(vesper::ir::Op::Binary(op, ..)) => format!("Binary {op}"),
        Some(vesper::ir::Op::Constant(l)) => {
            format!(
                "Constant {}",
                format!("{l:?}").split('(').next().unwrap_or("")
            )
        }
        Some(op) => format!("{op:?}")
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("")
            .to_string(),
        None => "?".into(),
    }
}

/// Instructions planned and emitted from Vesper, planned ones the hook
/// declined (operand lanes), and unplanned requests.
static EMITTED: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);
static UNPLANNED: AtomicU64 = AtomicU64::new(0);
/// Requests emitted from Vesper per hook request (plan 1-10, tests 11-12).
static BY_REQUEST: [AtomicU64; 13] = [const { AtomicU64::new(0) }; 13];

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
    let mut sites = HashMap::new();
    // A program the format cannot encode stays on the Rust lowering.
    if let Some((input, _)) = ir_words::program_input(world, program) {
        let names: Vec<&String> = program.functions.keys().collect();
        let words = vesper().plan.words(&input);
        assert_eq!(words.len() % RECORD, 0, "plan of {} words", words.len());
        for r in words.chunks(RECORD) {
            let name = names[r[0] as usize].clone();
            sites.insert((name, r[1] as u32, r[2] as u32), (r[3] as u32, r[4], r[5]));
        }
    }
    Plan { key, sites }
}

/// The `scalar` hook: the planned request of this instruction, emitted by
/// `scalar_hook` over the lent builder.
fn scalar(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    // The raising arms' tests (`codegen_swap::scalar`) go to the hook as
    // they come, renumbered past the plan's requests.
    let test = match lent.op {
        codegen_swap::INSTRUCTION => None,
        codegen_swap::scalar::OVERFLOW => Some(TEST_OVERFLOW),
        codegen_swap::scalar::DIVISOR => Some(TEST_DIVISOR),
        _ => return Ok(false),
    };
    if let Some(request) = test {
        let args = lent.args.to_vec();
        return emit(lent, request, &args);
    }
    let planned = PLAN.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell
            .as_ref()
            .is_none_or(|p| p.key != program_key(lent.program))
        {
            *cell = Some(plan_of(lent.world, lent.program));
        }
        let plan = cell.as_ref().expect("just planned");
        plan.sites
            .get(&(
                lent.site.function.to_string(),
                lent.site.block,
                lent.site.inst,
            ))
            .copied()
    });
    let Some((request, a, b)) = planned else {
        UNPLANNED.fetch_add(1, Ordering::Relaxed);
        let name = op_name(lent);
        LEFT.with(|l| *l.borrow_mut().entry(name).or_default() += 1);
        return Ok(false);
    };
    if lent.args.contains(&NO_VALUE) {
        REFUSED.fetch_add(1, Ordering::Relaxed);
        return Ok(false);
    }
    let mut args = vec![a, b, u64::from(lent.optimize)];
    args.extend_from_slice(lent.args);
    emit(lent, request, &args)
}

/// `scalar_hook` on `request` with `args`, over the lent handles.
fn emit(lent: &mut Lent<'_, '_>, request: u32, args: &[u64]) -> Result<bool, String> {
    let hook = &vesper().hook;
    let (answer, crossing) = vesper_cranelift_sys::lend_as(
        kind(lent),
        lent.builder,
        &mut *lent.module,
        request,
        args,
        |call| {
            // SAFETY: the hook takes one word, the lent call's address,
            // which is live for the whole call.
            unsafe { hook.call(&[call]) }
        },
    );
    let answer = answer?;
    if let Some(e) = crossing.error {
        return Err(format!("scalar hook (request {request}): {e}"));
    }
    if crossing.unreturned {
        return Err(format!(
            "scalar hook (request {request}) kept a lent handle"
        ));
    }
    if answer == 1 {
        EMITTED.fetch_add(1, Ordering::Relaxed);
        if let Some(c) = BY_REQUEST.get(request as usize) {
            c.fetch_add(1, Ordering::Relaxed);
        }
        lent.results.extend(crossing.results);
        Ok(true)
    } else {
        REFUSED.fetch_add(1, Ordering::Relaxed);
        Ok(false)
    }
}

fn hooks() -> CodegenHooks {
    CodegenHooks {
        scalar: Some(scalar),
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

/// 0 CLIF differences, byte-identical objects and equal runs over the
/// corpus, optimize on and off, with the scalar instructions emitted from
/// Vesper.
#[test]
fn vesper_scalar_emission_matches_the_rust_lowering() {
    big_stack(emission);
}

fn emission() {
    let before = [&EMITTED, &REFUSED, &UNPLANNED].map(|c| c.load(Ordering::Relaxed));
    let worlds: Vec<_> = check_harness::corpus()
        .iter()
        .filter(|c| c.world.functions.contains_key("main"))
        .step_by(step())
        .collect();
    let mut total = Diff::default();
    for (i, case) in worlds.iter().enumerate() {
        for optimize in [true, false] {
            PLAN.with(|p| *p.borrow_mut() = None);
            total.add(compare(
                &case.name,
                &case.world,
                "main",
                hooks(),
                optimize,
                runs(i, case),
            ));
        }
    }
    let [emitted, refused, unplanned] =
        [&EMITTED, &REFUSED, &UNPLANNED].map(|c| c.load(Ordering::Relaxed));
    let (emitted, refused, unplanned) = (
        emitted - before[0],
        refused - before[1],
        unplanned - before[2],
    );
    eprintln!(
        "codegen scalar emission: {}; {emitted} requests emitted from Vesper, \
         {refused} planned but declined, {unplanned} left to Rust",
        total.summary()
    );
    // `CODEGEN_SCALAR_DUMP=dir` writes both sides of each differing
    // function's CLIF there.
    if let Ok(dir) = std::env::var("CODEGEN_SCALAR_DUMP") {
        dump(&dir, &worlds, &total);
    }
    let names = [
        "",
        "zero",
        "iconst",
        "f64const",
        "icmp",
        "binop",
        "bnot",
        "not",
        "ineg",
        "shift",
        "convert",
        "overflow test",
        "divisor test",
    ];
    let by: Vec<String> = BY_REQUEST
        .iter()
        .zip(names)
        .skip(1)
        .map(|(c, n)| format!("{n} {}", c.load(Ordering::Relaxed)))
        .collect();
    eprintln!("emitted per request: {}", by.join(", "));
    LEFT.with(|l| {
        let mut left: Vec<_> = l.borrow().iter().map(|(k, v)| (*v, k.clone())).collect();
        left.sort_by(|a, b| b.cmp(a));
        let left: Vec<String> = left.iter().map(|(v, k)| format!("{k} {v}")).collect();
        eprintln!("left to Rust: {}", left.join(", "));
    });
    assert!(total.worlds > 0, "no corpus world compiled");
    assert!(emitted > 0, "the scalar hook never emitted");
    assert!(total.is_clean(), "{}", total.summary());
}

fn dump(dir: &str, worlds: &[&check_harness::Case], total: &Diff) {
    std::fs::create_dir_all(dir).expect("dump directory");
    for (w, f, _) in total.clif.iter().take(20) {
        let case = worlds
            .iter()
            .find(|c| &c.name == w)
            .expect("a compared world");
        for optimize in [true, false] {
            PLAN.with(|p| *p.borrow_mut() = None);
            let rust = build(&case.world, "main", optimize);
            let hooked = codegen_swap::with_hooks(hooks(), || build(&case.world, "main", optimize));
            if let (Ok(r), Ok(h)) = (rust, hooked)
                && r.clif.get(f) != h.clif.get(f)
            {
                let base = format!("{dir}/{}-{f}-{optimize}", w.replace('/', "_"));
                std::fs::write(
                    format!("{base}.rust.clif"),
                    r.clif.get(f).cloned().unwrap_or_default(),
                )
                .expect("write");
                std::fs::write(
                    format!("{base}.vesper.clif"),
                    h.clif.get(f).cloned().unwrap_or_default(),
                )
                .expect("write");
            }
        }
    }
}

/// The words of `codegen_swap::scalar` are a protocol with
/// `@vesper/codegen-scalar-emit` (`overflow_test` reads kinds as 1 `u8` ...
/// 10 `isize` and operators as 1 `+` ... 4 `/`): reordering
/// `numeric::INTEGER_KINDS` must not change them silently.
#[test]
fn scalar_request_words_are_pinned() {
    use codegen_swap::scalar::{kind_word, op_word};
    use vesper::numeric::IntegerKind as K;
    let kinds = [
        (K::U8, 1),
        (K::U16, 2),
        (K::U32, 3),
        (K::U64, 4),
        (K::Usize, 5),
        (K::I8, 6),
        (K::I16, 7),
        (K::I32, 8),
        (K::I64, 9),
        (K::Isize, 10),
    ];
    for (kind, word) in kinds {
        assert_eq!(kind_word(kind), Some(word), "{kind:?}");
    }
    assert_eq!(kind_word(K::I128), None);
    assert_eq!(kind_word(K::U128), None);
    for (op, word) in [("+", 1), ("-", 2), ("*", 3), ("/", 4)] {
        assert_eq!(op_word(op), Some(word), "{op}");
    }
    assert_eq!(op_word("%"), None);
}
