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
const OPS: [(u32, &str); 8] = [
    (control::TAG, "tag"),
    (control::CAPTURE_SLOT, "capture slot"),
    (control::QUALIFY, "qualify"),
    (control::DISPATCH, "dispatch"),
    (control::TRAP, "trap"),
    (control::RESUME_SAVE, "resume save"),
    (control::RESUME_RESTORE, "resume restore"),
    (control::RETURN_TO, "return to"),
];

/// Requests answered from Vesper, per op (index = op number).
static ANSWERED: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];

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

/// The corpus worlds with a `main`, every `step()`-th.
fn worlds() -> Vec<&'static check_harness::Case> {
    check_harness::corpus()
        .iter()
        .filter(|c| c.world.functions.contains_key("main"))
        .step_by(step())
        .collect()
}

/// Whether to JIT-run `case`: every effect-free `main` without parameters
/// (the control family is rarer than the aggregate pilot's, so all of
/// them, not every fourth).
fn runs(case: &check_harness::Case) -> bool {
    let f = &case.world.functions["main"];
    f.params.is_empty() && f.effects.instances.is_empty() && f.effects.tail.is_none()
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
                runs(case),
            ));
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
