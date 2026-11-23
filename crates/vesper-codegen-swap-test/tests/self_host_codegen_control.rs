//! The `control` family's leaf requests (self-hosting SH4 lane 10, #1284):
//! closure, function and continuation tags, capture slots, `qualify_tag`'s
//! select, `invoke_tag`'s dispatch chain and its trap, and the inlined
//! handlers' resumption saves, restores and return dispatch, emitted by
//! `control_hook` of packages/vesper-codegen-control-emit through the
//! `control` hook. Over the corpus, every function's CLIF text, every
//! object and every sampled JIT run must equal the Rust build's, with the
//! optimizer on and off. `CODEGEN_SWAP_STEP=n` compares every n-th world.
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;

use check_harness::repo;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use vesper::codegen_swap::{self, CodegenHooks, Lent, control};
use vesper::native::WordFunction;
use vesper_codegen_swap_test::*;

/// The ops of `codegen_swap::control`, by number.
const OPS: [(u32, &str); 37] = [
    (control::TAG, "tag"),
    (control::CAPTURE_SLOT, "capture slot"),
    (control::QUALIFY, "qualify"),
    (control::DISPATCH, "dispatch"),
    (control::TRAP, "trap"),
    (control::RESUME_SAVE, "resume save"),
    (control::RESUME_RESTORE, "resume restore"),
    (control::RETURN_TO, "return to"),
    (control::JOIN_BLOCK, "join block"),
    (control::SWITCH_PARAM, "switch param"),
    (control::JUMP_ARGS, "jump args"),
    (control::TAIL_RESUME, "tail resume"),
    (control::STACK_ADDR, "stack addr"),
    (control::HANDLE_SETUP, "handle setup"),
    (control::HANDLER_ENTRY, "handler entry"),
    (control::RERAISE_GUARD, "reraise guard"),
    (control::RERAISE_EACH, "reraise each"),
    (control::FREE_SINK_BRANCH, "free sink branch"),
    (control::FAIL_EXIT_PREP, "fail exit prep"),
    (control::FAIL_EXIT_RETURN, "fail exit return"),
    (control::RETURN_VALUES, "return values"),
    (control::TRACE_CHECK, "trace check"),
    (control::TRACE_CALL, "trace call"),
    (control::TRACE_ABORT, "trace abort"),
    (control::SWITCH_BLOCK, "switch block"),
    (control::DYN_PAIR_INIT, "dyn pair init"),
    (control::DYN_STORE_WORD, "dyn store word"),
    (control::DYN_DISPATCH, "dyn dispatch"),
    (control::DYN_DISPATCH_BOXED, "dyn dispatch boxed"),
    (control::DYN_BOX_INIT, "dyn box init"),
    (control::DYN_BOXED_PARTS, "dyn boxed parts"),
    (control::DYN_PAIR_PARTS, "dyn pair parts"),
    (control::DYN_CANDIDATE, "dyn candidate"),
    (control::DYN_VTABLE, "dyn vtable"),
    (control::DYN_SLOT_ADAPTER, "dyn slot adapter"),
    (control::DYN_DROP_OBJECT, "dyn drop object"),
    (control::DYN_DROP_BOXED, "dyn drop boxed"),
];

/// Requests answered from Vesper, per op (index = op number).
static ANSWERED: [AtomicU64; 38] = [const { AtomicU64::new(0) }; 38];

fn hook() -> &'static WordFunction {
    static HOOK: OnceLock<WordFunction> = OnceLock::new();
    // Compiled with no hook in effect, or the hook would call itself while
    // its own `OnceLock` is initializing.
    HOOK.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let checked = foreign_package(&repo(), "packages/vesper-codegen-control-emit");
            hook_function(&checked, "control_hook")
        })
    })
}

fn control_hook(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    let op = lent.op;
    let handled = vesper_hook(lent, hook())?;
    if handled && let Some(n) = ANSWERED.get(op as usize) {
        n.fetch_add(1, Ordering::Relaxed);
    }
    Ok(handled)
}

fn hooks() -> CodegenHooks {
    CodegenHooks {
        control: Some(control_hook),
        ..CodegenHooks::default()
    }
}

fn step() -> usize {
    std::env::var("CODEGEN_SWAP_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1)
}

/// Runs `f` on a thread with the CLI's deep stack.
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

/// Corpus worlds compared at every `step()`: each reaches an op that few
/// worlds do (`qualify_tag`), so a sampled run still answers every op.
const PINNED: [&str; 1] = ["tests/fixtures/native_handlers/widened_function_value.ves"];

/// The corpus worlds with a `main`, every `step()`-th, and the pinned ones.
fn worlds() -> Vec<&'static check_harness::Case> {
    let step = step();
    check_harness::corpus()
        .iter()
        .filter(|c| c.world.functions.contains_key("main"))
        .enumerate()
        .filter(|(i, c)| i % step == 0 || PINNED.contains(&c.name.as_str()))
        .map(|(_, c)| c)
        .collect()
}

/// Whether to JIT-run an entry world: every effect-free `main` without
/// parameters.
fn runs(world: &vesper::check::World) -> bool {
    let f = &world.functions["main"];
    f.params.is_empty() && f.effects.instances.is_empty() && f.effects.tail.is_none()
}

/// Programs for the ops the corpus may not reach. Since #1278 only an
/// owned aggregate capture takes a capture slot (word-sized and lent
/// captures live in variables).
const HANDWRITTEN: [(&str, &str); 6] = [
    (
        "owned aggregate capture",
        "struct Pair {\n    a: u64,\n    b: u64\n}\n\
         fn main() -> u64 {\n    let p = Pair {\n        a: 2,\n        b: 3\n    };\n    \
         let f = |x: u64| x ^ p.a ^ p.b;\n    f(4)\n}\n",
    ),
    (
        "dyn trait and boxed dyn",
        "struct Val { v: i64 }\n\
         trait Inner { fn val(in self) -> i64; }\n\
         impl Inner for Val { fn val(in self) -> i64 { self.v } }\n\
         struct Boxed { k: Box[dyn Inner] }\n\
         trait Shape {\n\
             fn area(in self) -> i64;\n\
             fn bump(inout self, by: i64);\n\
             fn take(sink self) -> i64;\n\
         }\n\
         impl Shape for i64 {\n\
             fn area(in self) -> i64 { self }\n\
             fn bump(inout self, by: i64) { self = wrapping_add(self, by); }\n\
             fn take(sink self) -> i64 { self }\n\
         }\n\
         impl Shape for Boxed {\n\
             fn area(in self) -> i64 { self.k.val() }\n\
             fn bump(inout self, by: i64) {\n\
                 let next = wrapping_add(self.k.val(), by);\n\
                 self.k = Box::new(Val { v: next });\n\
             }\n\
             fn take(sink self) -> i64 { self.k.val() }\n\
         }\n\
         fn eval(in s: dyn Shape, inout n: dyn Shape, sink d: dyn Shape, take_it: bool) -> i64 {\n\
             n.bump(3);\n\
             let a = s.area();\n\
             let b = if take_it { d.take() } else { 1 };\n\
             wrapping_add(a, wrapping_add(n.area(), b))\n\
         }\n\
         fn main() -> i64 {\n\
             let mut x: i64 = 10;\n\
             let bx: Box[dyn Shape] = Box::new(Boxed { k: Box::new(Val { v: 4 }) });\n\
             let a = bx.area();\n\
             let mut by: Box[dyn Shape] = Box::new[dyn Shape](5);\n\
             by.bump(2);\n\
             let c = eval(in bx, inout x, sink Boxed { k: Box::new(Val { v: 7 }) }, false);\n\
             wrapping_add(a, wrapping_add(by.area(), c))\n\
         }\n",
    ),
    (
        "dyn effectful dispatch and sink lend unwind",
        "linear effect Net { fn emit(v: i64) -> i64; }\n\
         struct Echo { k: i64 }\n\
         struct Count { n: i64 }\n\
         trait Transport {\n\
             fn send(in self, v: i64) -> i64 eff [Net];\n\
             fn finish(sink self) -> i64 eff [Net];\n\
         }\n\
         impl Transport for Echo {\n\
             fn send(in self, v: i64) -> i64 eff [Net] { Net::emit(wrapping_add(v, self.k)) }\n\
             fn finish(sink self) -> i64 eff [Net] { Net::emit(self.k) }\n\
         }\n\
         impl Transport for Count {\n\
             fn send(in self, v: i64) -> i64 eff [Net] { Net::emit(wrapping_add(v, self.n)) }\n\
             fn finish(sink self) -> i64 eff [Net] { Net::emit(self.n) }\n\
         }\n\
         fn send_both(in a: dyn Transport, sink b: dyn Transport) -> i64 eff [Net, Fail[i64]] {\n\
             let x = a.send(10);\n\
             if x < 0 { Fail::raise(x) }\n\
             let y = b.finish();\n\
             wrapping_add(x, y)\n\
         }\n\
         fn main() -> i64 {\n\
             let mut total: i64 = 0;\n\
             let a = handle Fail[i64] in {\n\
                 handle Net in {\n\
                     let b: Box[dyn Transport] = Box::new(Count { n: 3 });\n\
                     send_both(in Echo { k: 2 }, sink b)\n\
                 } with {\n\
                     return v => v,\n\
                     emit(v, k) => { total = wrapping_add(total, v); k(v) },\n\
                 }\n\
             } with {\n\
                 return v => wrapping_add(v, total),\n\
                 raise(e, k) => e,\n\
             };\n\
             let b = handle Fail[i64] in {\n\
                 handle Net in {\n\
                     let p: Box[dyn Transport] = Box::new(Echo { k: -20 });\n\
                     let q: Box[dyn Transport] = Box::new(Count { n: 5 });\n\
                     send_both(in p, sink q)\n\
                 } with {\n\
                     return v => v,\n\
                     emit(v, k) => k(v),\n\
                 }\n\
             } with {\n\
                 return v => v,\n\
                 raise(e, k) => wrapping_sub(0, e),\n\
             };\n\
             wrapping_add(a, b)\n\
         }\n",
    ),
    (
        "multi fail exit and reraise",
        "fn leaf(x: i64) -> i64 eff [Fail[i64]] {\n\
             if x < 0 { Fail::raise(x) } else { x }\n\
         }\n\
         fn mid(x: i64) -> i64 eff [Fail[i64], Fail[bool]] {\n\
             if x == 0 { Fail::raise(false) } else { leaf(x) }\n\
         }\n\
         fn run_case(x: i64) -> i64 {\n\
             handle Fail[i64] in {\n\
                 handle Fail[bool] in {\n\
                     mid(x)\n\
                 } with {\n\
                     return v => v,\n\
                     raise(b, k) => if b { 100 } else { 200 },\n\
                 }\n\
             } with {\n\
                 return v => v,\n\
                 raise(e, k) => wrapping_sub(0, e),\n\
             }\n\
         }\n\
         fn main() -> i64 {\n\
             wrapping_add(run_case(0), wrapping_add(run_case(-7), run_case(5)))\n\
         }\n",
    ),
    (
        "traced call",
        "fn step(n: u64) -> u64 eff [Trace] {\n\
             let s = Trace::start_span(0, \"step\");\n\
             Trace::emit_ok(s, n);\n\
             n\n\
         }\n\
         fn main() -> u64 eff [Trace] {\n\
             let a = step(1);\n\
             let b = step(2);\n\
             wrapping_add(a, b)\n\
         }\n",
    ),
    (
        "tail and non-tail continuations",
        "linear effect Ask { fn ask(x: i64) -> i64; }\n\
         fn work() -> i64 eff [Ask] {\n\
             wrapping_add(Ask::ask(3), Ask::ask(4))\n\
         }\n\
         fn main() -> i64 {\n\
             let t = handle Ask in { work() } with {\n\
                 return x => x,\n\
                 ask(x, k) => k(wrapping_add(x, 1)),\n\
             };\n\
             let nt = handle Ask in { work() } with {\n\
                 return x => x,\n\
                 ask(x, k) => wrapping_add(x, k(wrapping_mul(x, 2))),\n\
             };\n\
             wrapping_add(t, nt)\n\
         }\n",
    ),
];

/// The checked world of a hand-written program.
fn accepted(source: &str) -> vesper::check::Checked {
    let parsed = vesper::parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    checked
}

fn compare_traced(name: &str, checked: &vesper::check::Checked, optimize: bool) {
    let (rust_obj, rust_clif) =
        vesper::native::object_traced_with_options(&checked.world, "main", None, optimize)
            .unwrap_or_else(|e| panic!("{name}: rust traced object failed: {e}"));
    let (hooked_obj, hooked_clif) = codegen_swap::with_hooks(hooks(), || {
        vesper::native::object_traced_with_options(&checked.world, "main", None, optimize)
    })
    .unwrap_or_else(|e| panic!("{name}: hooked traced object failed: {e}"));
    assert_eq!(
        rust_clif, hooked_clif,
        "{name} (optimize {optimize}): traced CLIF differs"
    );
    assert_eq!(
        rust_obj, hooked_obj,
        "{name} (optimize {optimize}): traced object differs"
    );
    let cfg = vesper::trace_effects::host::TraceConfig::default();
    let rust_run = vesper::trace_effects::native_runtime::run_traced_native_with_options(
        checked, "main", cfg, optimize,
    )
    .unwrap_or_else(|e| panic!("{name}: rust traced JIT failed: {e}"));
    let hooked_run = codegen_swap::with_hooks(hooks(), || {
        vesper::trace_effects::native_runtime::run_traced_native_with_options(
            checked, "main", cfg, optimize,
        )
    })
    .unwrap_or_else(|e| panic!("{name}: hooked traced JIT failed: {e}"));
    assert_eq!(rust_run.value.to_string(), hooked_run.value.to_string());
    assert_eq!(rust_run.spans, hooked_run.spans);
}

/// The swap: 0 CLIF differences, byte-identical objects and equal runs,
/// optimize on and off, and every op answered from Vesper at least once.
#[test]
fn the_control_leaves_match_the_rust_lowering() {
    big_stack(control_swap);
}

fn control_swap() {
    let before: Vec<u64> = ANSWERED.iter().map(|n| n.load(Ordering::Relaxed)).collect();
    let mut total = Diff::default();
    for case in worlds() {
        for optimize in [true, false] {
            total.add(compare(
                &case.name,
                &case.world,
                "main",
                hooks(),
                optimize,
                runs(&case.world),
            ));
        }
    }
    for (name, source) in HANDWRITTEN {
        let checked = accepted(source);
        let run = runs(&checked.world);
        for optimize in [true, false] {
            if checked
                .world
                .functions
                .get("main")
                .is_some_and(|f| f.effects.instances.iter().any(|e| e.name == "Trace"))
            {
                compare_traced(name, &checked, optimize);
            } else {
                total.add(compare(
                    name,
                    &checked.world,
                    "main",
                    hooks(),
                    optimize,
                    run,
                ));
            }
        }
    }
    let counts: Vec<(&str, u64)> = OPS
        .iter()
        .map(|(op, name)| {
            let i = *op as usize;
            (*name, ANSWERED[i].load(Ordering::Relaxed) - before[i])
        })
        .collect();
    eprintln!(
        "codegen control swap: {}; answered {counts:?}",
        total.summary()
    );
    assert!(total.worlds > 0, "no corpus world compiled");
    assert!(total.is_clean(), "{}", total.summary());
    for (name, n) in counts {
        assert!(n > 0, "no corpus world emitted a control {name} request");
    }
}

/// A deliberately wrong hook (the closure-call trap is user 4, not 3) is
/// caught.
#[test]
fn a_divergent_control_hook_is_caught() {
    big_stack(divergent);
}

fn divergent() {
    fn wrong(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
        if lent.op != control::TRAP || lent.args != [3] {
            return Ok(false);
        }
        use vesper_cranelift_sys::cranelift_codegen::ir::{InstBuilder, TrapCode};
        lent.builder.ins().trap(TrapCode::unwrap_user(4));
        Ok(true)
    }
    let hooks = CodegenHooks {
        control: Some(wrong),
        ..CodegenHooks::default()
    };
    let mut total = Diff::default();
    for case in worlds() {
        total.add(compare(
            &case.name,
            &case.world,
            "main",
            hooks,
            false,
            false,
        ));
        if !total.clif.is_empty() {
            break;
        }
    }
    assert!(!total.clif.is_empty(), "{}", total.summary());
}

#[test]
fn the_session_entries_match_the_rust_lowering() {
    big_stack(session_entries);
}

fn session_entries() {
    use std::sync::atomic::AtomicUsize;
    use vesper::native::session::{
        Cancellation, ECANCELED, EffectBinding, Operation, OperationBinding,
        object as session_object,
    };

    static OP_HITS: [AtomicUsize; 5] = [const { AtomicUsize::new(0) }; 5];
    fn counted_session(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
        let ok = control_hook(lent)?;
        if ok && (38..=42).contains(&lent.op) {
            OP_HITS[(lent.op - 38) as usize].fetch_add(1, Ordering::Relaxed);
        }
        Ok(ok)
    }

    let pulse_bindings = vec![EffectBinding {
        effect: "Pulse".into(),
        ops: vec![
            OperationBinding::new(
                "pause",
                Operation::Yield,
                "Ticked",
                "TickFailed",
                Cancellation::Constructor("TickCancelled".into()),
            ),
            OperationBinding::new(
                "read",
                Operation::Read,
                "Got",
                "IoFailed",
                Cancellation::Constructor("IoCancelled".into()),
            ),
        ],
    }];
    let lease_bindings = vec![EffectBinding {
        effect: "Pulse".into(),
        ops: vec![
            OperationBinding::new(
                "pause",
                Operation::Yield,
                "Ticked",
                "TickFailed",
                Cancellation::ErrorCode(ECANCELED),
            ),
            OperationBinding::new(
                "read",
                Operation::Read,
                "Got",
                "IoFailed",
                Cancellation::ErrorCode(ECANCELED),
            ),
        ],
    }];
    let failing_bindings = vec![EffectBinding {
        effect: "Source".into(),
        ops: vec![OperationBinding::new(
            "read",
            Operation::Read,
            "Got",
            "IoFailed",
            Cancellation::ErrorCode(ECANCELED),
        )],
    }];
    let float_bindings = vec![EffectBinding {
        effect: "Pulse".into(),
        ops: vec![OperationBinding::new(
            "pause",
            Operation::Yield,
            "Ticked",
            "TickFailed",
            Cancellation::Constructor("TickCancelled".into()),
        )],
    }];
    let float_src = r#"
enum Wait { Ticked, TickFailed(u64), TickCancelled }
linear effect Pulse { fn pause() -> Wait; }
fn run_f64() -> f64 eff [Pulse] {
    match Pulse::pause() {
        Wait::Ticked => 1.5,
        Wait::TickFailed(_) => 2.5,
        Wait::TickCancelled => 3.5,
    }
}
fn run_f32() -> f32 eff [Pulse] {
    match Pulse::pause() {
        Wait::Ticked => 1.5,
        Wait::TickFailed(_) => 2.5,
        Wait::TickCancelled => 3.5,
    }
}
"#;

    let cases: [(&str, String, &str, &[EffectBinding]); 5] = [
        (
            "ticket_tally",
            std::fs::read_to_string(repo().join("examples/native-pulse/ticket_tally.ves")).unwrap(),
            "main",
            &pulse_bindings,
        ),
        (
            "lease_cleanup",
            std::fs::read_to_string(repo().join("examples/native-pulse/lease_cleanup.ves"))
                .unwrap(),
            "main",
            &lease_bindings,
        ),
        (
            "typed_failures",
            std::fs::read_to_string(repo().join("examples/native-pulse/typed_failures.ves"))
                .unwrap(),
            "main",
            &failing_bindings,
        ),
        ("session_f64", float_src.into(), "run_f64", &float_bindings),
        ("session_f32", float_src.into(), "run_f32", &float_bindings),
    ];

    let hooks = CodegenHooks {
        control: Some(counted_session),
        ..CodegenHooks::default()
    };
    let mut clif_diffs = vec![];
    let mut obj_diffs = vec![];
    for (name, src, entry, bindings) in &cases {
        let parsed = vesper::parser::parse(src);
        assert!(
            parsed.diagnostics.is_empty(),
            "{name}: {:?}",
            parsed.diagnostics
        );
        let checked = vesper::check::check(&parsed.module);
        assert!(
            checked.diagnostics.is_empty(),
            "{name}: {:?}",
            checked.diagnostics
        );
        let world = checked.world;
        for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
            let (r_obj, r_clif) = session_object(&world, entry, bindings, Some(target)).unwrap();
            let (h_obj, h_clif) = codegen_swap::with_hooks(hooks, || {
                session_object(&world, entry, bindings, Some(target))
            })
            .unwrap();
            if r_clif != h_clif {
                clif_diffs.push(format!(
                    "{name} [{target}]: {}",
                    first_difference(&r_clif, &h_clif)
                ));
            }
            if r_obj != h_obj {
                obj_diffs.push(format!("{name} [{target}]"));
            }
        }
    }
    assert!(
        clif_diffs.is_empty() && obj_diffs.is_empty(),
        "session diffs: clif={clif_diffs:?} obj={obj_diffs:?}"
    );
    for (i, op) in (38..=42).enumerate() {
        let hits = OP_HITS[i].load(Ordering::Relaxed);
        assert!(hits > 0, "session op {op} was never exercised");
    }
}
