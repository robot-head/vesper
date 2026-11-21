//! Self-hosting SH4 lane 9 (#1283), part 2: native emission of sequences,
//! bytes and strings from `@vesper/codegen-seq-emit`, through the `seq`
//! hook of `vesper::codegen_swap`.
//!
//! Ported so far: `codegen_swap::seq::RUNTIME`, the AOT sequence runtime
//! (src/seq/aot_runtime.rs) each object that uses a sequence defines. Over
//! the corpus, with the optimizer on and off, every function's CLIF text,
//! every object and every sampled JIT run must equal the Rust build's, and
//! the objects for the other native targets (Windows, where no fault
//! message is written, and AArch64) must be byte-identical too.
//! `CODEGEN_SEQ_STEP=n` compares every n-th world (default 1 under Cargo, 4 under Bazel).
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;

use check_harness::repo;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use vesper::codegen_swap::{self, CodegenHooks, Lent};
use vesper::native::WordFunction;
use vesper_codegen_swap_test::*;

fn hook() -> &'static WordFunction {
    static HOOK: OnceLock<WordFunction> = OnceLock::new();
    // Compiled with no hook in effect (see the pilot test's note).
    HOOK.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let checked = foreign_package(&repo(), "packages/vesper-codegen-seq/emit");
            hook_function(&checked, "seq_hook")
        })
    })
}

fn seq(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    vesper_hook(lent, hook())
}

/// Every request the Vesper hook has answered with the runtime: the
/// hook declines everything else.
static RUNTIMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn counted(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    let op = lent.op;
    let handled = seq(lent)?;
    if handled {
        assert_eq!(op, codegen_swap::seq::RUNTIME, "the hook answered op {op}");
        RUNTIMES.fetch_add(1, Ordering::Relaxed);
    }
    Ok(handled)
}

/// Every `step()`-th world: `CODEGEN_SEQ_STEP`, else 1 under Cargo and 4
/// under Bazel, whose CI run shares one time limit with the whole suite.
fn step() -> usize {
    std::env::var("CODEGEN_SEQ_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(if std::env::var_os("TEST_SRCDIR").is_some() {
            4
        } else {
            1
        })
}

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

/// The corpus worlds with a `main` whose host object defines the
/// sequence runtime (its live counter symbol), every `step()`-th.
fn worlds() -> &'static [&'static check_harness::Case] {
    static W: OnceLock<Vec<&'static check_harness::Case>> = OnceLock::new();
    W.get_or_init(|| {
        check_harness::corpus()
            .iter()
            .filter(|c| c.world.functions.contains_key("main"))
            .step_by(step())
            .filter(|c| {
                build(&c.world, "main", true).is_ok_and(|b| {
                    b.object
                        .windows(b"vesper_seq_live".len())
                        .any(|w| w == b"vesper_seq_live")
                })
            })
            .collect()
    })
}

fn runs(index: usize, case: &check_harness::Case) -> bool {
    let f = &case.world.functions["main"];
    index.is_multiple_of(4)
        && f.params.is_empty()
        && f.effects.instances.is_empty()
        && f.effects.tail.is_none()
}

/// The runtime swap on the host: 0 CLIF differences, byte-identical
/// objects and equal runs, optimize on and off.
#[test]
fn the_runtime_matches_the_rust_emitter() {
    big_stack(host_swap);
}

fn host_swap() {
    let hooks = CodegenHooks {
        seq: Some(counted),
        ..CodegenHooks::default()
    };
    let before = RUNTIMES.load(Ordering::Relaxed);
    let declined = DECLINED.load(Ordering::Relaxed);
    let mut total = Diff::default();
    // Builds whose runtime did not come from the hook.
    let mut elsewhere = vec![];
    for (i, case) in worlds().iter().enumerate() {
        for optimize in [true, false] {
            let at = RUNTIMES.load(Ordering::Relaxed);
            total.add(compare(
                &case.name,
                &case.world,
                "main",
                hooks,
                optimize,
                runs(i, case),
            ));
            if RUNTIMES.load(Ordering::Relaxed) == at {
                elsewhere.push(format!("{}@{optimize}", case.name));
            }
        }
    }
    let runtimes = RUNTIMES.load(Ordering::Relaxed) - before;
    let declined = DECLINED.load(Ordering::Relaxed) - declined;
    eprintln!(
        "codegen seq: {}; {runtimes} runtimes defined from Vesper, {declined} requests declined; \
         runtime not requested in {elsewhere:?}",
        total.summary()
    );
    assert!(total.worlds > 0, "no corpus world compiled");
    assert!(runtimes > 0, "the runtime hook never ran");
    assert!(total.is_clean(), "{}", total.summary());
}

/// The other native targets: Windows (no fault message, no `write`
/// import) and AArch64 Linux and macOS.
#[test]
fn the_runtime_matches_on_other_targets() {
    big_stack(other_targets);
}

fn other_targets() {
    const TARGETS: [&str; 3] = [
        "x86_64-pc-windows-msvc",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
    ];
    let before = RUNTIMES.load(Ordering::Relaxed);
    let hooks = CodegenHooks {
        seq: Some(counted),
        ..CodegenHooks::default()
    };
    let (mut compared, mut differ) = (0, vec![]);
    // Every fourth world: the runtime is the same in each object, and
    // only the code around it changes.
    for case in worlds().iter().step_by(4) {
        for target in TARGETS {
            let rust = vesper::native::object_with_options(&case.world, "main", Some(target), true);
            let hooked = codegen_swap::with_hooks(hooks, || {
                vesper::native::object_with_options(&case.world, "main", Some(target), true)
            });
            match (rust, hooked) {
                (Ok((r, rc)), Ok((h, hc))) => {
                    compared += 1;
                    if r != h || rc != hc {
                        differ.push(format!("{} {target}", case.name));
                    }
                }
                (Err(_), Err(_)) => {}
                (r, h) => differ.push(format!(
                    "{} {target}: {:?} vs {:?}",
                    case.name,
                    r.err(),
                    h.err()
                )),
            }
        }
    }
    let runtimes = RUNTIMES.load(Ordering::Relaxed) - before;
    eprintln!(
        "codegen seq, other targets: {compared} objects, {} differ, {runtimes} runtimes from Vesper",
        differ.len()
    );
    assert!(compared > 0 && runtimes > 0);
    assert!(differ.is_empty(), "{differ:?}");
}

/// A hook that answers the runtime request without defining anything is
/// caught: the object (or the compile) differs.
#[test]
fn a_missing_runtime_is_caught() {
    big_stack(missing_runtime);
}

fn missing_runtime() {
    fn nothing(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
        Ok(lent.op == codegen_swap::seq::RUNTIME)
    }
    let hooks = CodegenHooks {
        seq: Some(nothing),
        ..CodegenHooks::default()
    };
    let caught = worlds().iter().take(20).any(|case| {
        let rust = build(&case.world, "main", true);
        let hooked = codegen_swap::with_hooks(hooks, || build(&case.world, "main", true));
        match (rust, hooked) {
            (Ok(r), Ok(h)) => r.object != h.object,
            (Ok(_), Err(_)) => true,
            _ => false,
        }
    });
    assert!(caught, "a hook that defines no runtime went unnoticed");
}
