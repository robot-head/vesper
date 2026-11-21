//! The SH4 codegen swap point (self-hosting SH4 lane 1, #1275):
//! `vesper::codegen_swap`, the shim's lent handles and the pilot swap.
//!
//! The pilot is the census lowering family (`alloc`, `alloc_uninit`,
//! `copy_bytes`, `load`, `store`, `offset`, `data_into`'s tag and zero
//! stores and `pattern_condition`'s pieces), emitted by
//! `aggregate_hook` of packages/vesper-codegen-base through the
//! `aggregate` hook. Over the corpus, every function's CLIF text, every
//! object and every sampled JIT run must equal the Rust build's, with the
//! optimizer on and off. `CODEGEN_SWAP_STEP=n` compares every n-th world
//! (default 1 under Cargo).
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;

use check_harness::repo;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use vesper::codegen_swap::{self, CodegenHooks, Family, Lent};
use vesper::native::WordFunction;
use vesper_codegen_swap_test::*;

/// `@vesper/codegen-base`, checked once, and its hooks JIT-compiled.
struct Hooks {
    aggregate: WordFunction,
    empty: WordFunction,
}

fn hooks() -> &'static Hooks {
    static HOOKS: OnceLock<Hooks> = OnceLock::new();
    // The hooks are compiled with no hook in effect: a test that first
    // calls one under `with_hooks` would otherwise call itself while its
    // own `OnceLock` is initializing, and wait forever.
    HOOKS.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let checked = foreign_package(&repo(), "packages/vesper-codegen-base");
            Hooks {
                aggregate: hook_function(&checked, "aggregate_hook"),
                empty: hook_function(&checked, "empty_hook"),
            }
        })
    })
}

fn aggregate(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    vesper_hook(lent, &hooks().aggregate)
}

fn pilot() -> CodegenHooks {
    CodegenHooks {
        aggregate: Some(aggregate),
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

/// Runs `f` on a thread with the CLI's deep stack: native compiles of the
/// deepest corpus worlds overflow a test thread's 2 MiB in debug builds.
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

/// Whether to JIT-run `case` (every fourth world whose `main` has no
/// effects and no parameters, as the checker harness samples).
fn runs(index: usize, case: &check_harness::Case) -> bool {
    let f = &case.world.functions["main"];
    index.is_multiple_of(4)
        && f.params.is_empty()
        && f.effects.instances.is_empty()
        && f.effects.tail.is_none()
}

#[test]
fn the_default_compiler_installs_no_hook() {
    let installed = codegen_swap::installed();
    for family in Family::ALL {
        assert!(installed.family(family).is_none(), "{}", family.name());
    }
    assert!(installed.export.is_none());
    assert!(installed.freestanding.is_none());
    assert!(installed.wasm.is_none());
    assert!(installed.ebpf.is_none());
    assert!(installed.verifiers.is_none());
}

#[test]
fn type_codes_match_the_shim() {
    use vesper_cranelift_sys::cranelift_codegen::ir::types;
    for t in [
        types::I8,
        types::I16,
        types::I32,
        types::I64,
        types::I128,
        types::F32,
        types::F64,
        types::I8X16,
        types::I16X8,
        types::I32X4,
        types::I64X2,
        types::F64X2,
    ] {
        assert_eq!(
            codegen_swap::ty_code(t),
            u64::from(vesper_cranelift_sys::codes::ty_code(t)),
            "{t}"
        );
    }
}

/// The pilot swap: 0 CLIF differences, byte-identical objects and equal
/// runs, optimize on and off.
#[test]
fn the_pilot_swap_matches_the_rust_lowering() {
    big_stack(pilot_swap);
}

fn pilot_swap() {
    let before = HANDLED.load(Ordering::Relaxed);
    let worlds = worlds();
    let mut total = Diff::default();
    for (i, case) in worlds.iter().enumerate() {
        for optimize in [true, false] {
            total.add(compare(
                &case.name,
                &case.world,
                "main",
                pilot(),
                optimize,
                runs(i, case),
            ));
        }
    }
    let handled = HANDLED.load(Ordering::Relaxed) - before;
    eprintln!(
        "codegen swap pilot: {}; {handled} requests answered from Vesper",
        total.summary()
    );
    assert!(total.worlds > 0, "no corpus world compiled");
    assert!(handled > 0, "the pilot hook never ran");
    assert!(total.is_clean(), "{}", total.summary());
}

/// A deliberately wrong hook (`pattern_condition`'s constant true is 2)
/// is caught: the harness can fail.
#[test]
fn a_divergent_hook_is_caught() {
    big_stack(divergent_hook);
}

fn divergent_hook() {
    fn wrong(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
        if lent.op != codegen_swap::aggregate::TRUE {
            return Ok(false);
        }
        let (_, crossing) = vesper_cranelift_sys::lend(
            lent.builder,
            &mut *lent.module,
            lent.op,
            lent.args,
            |call| {
                // SAFETY: the lent call's address, live for this closure.
                unsafe {
                    let b = vesper_cranelift_sys::vcl_call_builder(call);
                    let v = vesper_cranelift_sys::vcl_iconst(b, 1, 2);
                    vesper_cranelift_sys::vcl_call_result(call, u64::from(v));
                    vesper_cranelift_sys::vcl_call_return_builder(call, b);
                }
            },
        );
        assert!(crossing.error.is_none() && !crossing.unreturned);
        lent.results.extend(crossing.results);
        Ok(true)
    }
    let hooks = CodegenHooks {
        aggregate: Some(wrong),
        ..CodegenHooks::default()
    };
    let mut total = Diff::default();
    for case in worlds() {
        total.add(compare(&case.name, &case.world, "main", hooks, true, false));
        if !total.clif.is_empty() {
            break;
        }
    }
    // Only the CLIF text catches it: Cranelift folds a branch on either
    // nonzero constant to the same machine code, so the objects agree.
    // That is why the harness compares CLIF and not only bytes.
    assert!(!total.clif.is_empty(), "{}", total.summary());
}

/// A hook that declines everything leaves every build unchanged, and a
/// hook error fails the compile with its message.
#[test]
fn declined_requests_and_hook_errors() {
    big_stack(declined_and_errors);
}

fn declined_and_errors() {
    fn declines(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
        vesper_hook(lent, &hooks().empty)
    }
    fn fails(_: &mut Lent<'_, '_>) -> Result<bool, String> {
        Err("deliberate hook error".into())
    }
    let case = worlds()
        .into_iter()
        .find(|c| {
            let w = &c.world;
            build(w, "main", true).is_ok()
                && codegen_swap::with_hooks(
                    CodegenHooks {
                        aggregate: Some(fails),
                        ..CodegenHooks::default()
                    },
                    || build(w, "main", true).is_err(),
                )
        })
        .expect("a corpus world that lays out an aggregate");
    let err = codegen_swap::with_hooks(
        CodegenHooks {
            aggregate: Some(fails),
            ..CodegenHooks::default()
        },
        || build(&case.world, "main", true).unwrap_err(),
    );
    assert!(err.contains("deliberate hook error"), "{err}");
    let d = compare(
        &case.name,
        &case.world,
        "main",
        CodegenHooks {
            aggregate: Some(declines),
            ..CodegenHooks::default()
        },
        true,
        false,
    );
    assert!(d.is_clean() && d.worlds == 1, "{}", d.summary());
    // Every family hook declining changes nothing either.
    let all = CodegenHooks {
        scalar: Some(declines),
        call: Some(declines),
        aggregate: Some(declines),
        drops: Some(declines),
        seq: Some(declines),
        control: Some(declines),
        numeric: Some(declines),
        ffi: Some(declines),
        ..CodegenHooks::default()
    };
    let before = DECLINED.load(Ordering::Relaxed);
    let d = compare(&case.name, &case.world, "main", all, false, true);
    assert!(d.is_clean() && d.worlds == 1, "{}", d.summary());
    assert!(DECLINED.load(Ordering::Relaxed) > before);
}
