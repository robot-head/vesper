//! Lent walk state and re-entry into Rust (self-hosting SH5 lane 1, #1324):
//! `vesper::codegen_swap::walk`, `@vesper/codegen-walk`
//! (packages/vesper-codegen-base/walk) and the demonstration hook
//! (walk-demo/, `@vesper/codegen-walk-demo`).
//!
//! The demonstration hook answers every Place IR `Read`, `Bind` and `Write`
//! from Vesper, through the `scalar` family hook: the plain forms it emits
//! over the lent walk state, the rest by re-entering Rust for the place or
//! the whole instruction. Over the corpus every function's CLIF text, every
//! object and every sampled JIT run must equal the Rust build's, with the
//! optimizer on and off. `CODEGEN_SWAP_STEP=n` compares every n-th world.
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;

use check_harness::repo;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use vesper::codegen_swap::walk::{self, NONE, kind, query};
use vesper::codegen_swap::{self, CodegenHooks, Lent};
use vesper::native::WordFunction;
use vesper_codegen_swap_test::*;

/// The demonstration hook, JIT-compiled once with the shim's and the
/// walk's symbols bound.
fn demo() -> &'static WordFunction {
    static HOOK: OnceLock<WordFunction> = OnceLock::new();
    // Compiled with no hook in effect (a test calling it under
    // `with_hooks` would otherwise wait on its own `OnceLock`).
    HOOK.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let checked = foreign_package(&repo(), "crates/vesper-codegen-swap-test/walk-demo");
            let mut symbols = vesper_cranelift_sys::symbols();
            // The walk's symbols, with re-entries counted by kind.
            symbols.extend(
                walk::symbols()
                    .into_iter()
                    .filter(|(n, _)| *n != "vesper_walk_reenter"),
            );
            symbols.push(("vesper_walk_reenter", counted_reenter as *const u8));
            checked
                .word_function("walk_hook", &symbols, true)
                .unwrap_or_else(|e| panic!("walk_hook: {e}"))
        })
    })
}

/// Re-entries asked for, by kind (`reentry::INSTRUCTION` .. `PLACE_ADDRESS`).
static REENTRIES: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

extern "C" fn counted_reenter(kind: u32) {
    if let Some(c) = REENTRIES.get(kind as usize) {
        c.fetch_add(1, Ordering::Relaxed);
    }
    walk::reenter(kind)
}

/// Requests of a `Read`, `Bind` or `Write` (first calls only), those the
/// hook answered, and those it answered on the first call (emitted itself,
/// without re-entering).
static ASKED: AtomicU64 = AtomicU64::new(0);
static ANSWERED: AtomicU64 = AtomicU64::new(0);
static FIRST_CALL: AtomicU64 = AtomicU64::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);

fn scalar(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    let k = walk::query(query::KIND, 0, 0);
    let ours = matches!(k, kind::READ | kind::BIND | kind::WRITE);
    let first = walk::query(query::KEPT_COUNT, 0, 0) == 0;
    if ours {
        CALLS.fetch_add(1, Ordering::Relaxed);
        if first {
            ASKED.fetch_add(1, Ordering::Relaxed);
        }
    }
    let handled = vesper_hook(lent, demo())?;
    if ours && handled {
        ANSWERED.fetch_add(1, Ordering::Relaxed);
        if first {
            FIRST_CALL.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(handled)
}

fn hooks() -> CodegenHooks {
    CodegenHooks {
        scalar: Some(scalar),
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

/// The corpus worlds with a `main`, every `step()`-th, or only those whose
/// name contains one of the comma-separated `WALK_STATE_WORLD` when it is
/// set.
fn worlds() -> Vec<&'static check_harness::Case> {
    let only = std::env::var("WALK_STATE_WORLD").ok();
    check_harness::corpus()
        .iter()
        .filter(|c| c.world.functions.contains_key("main"))
        .filter(|c| {
            only.as_ref()
                .is_none_or(|o| o.split(',').any(|o| c.name.contains(o)))
        })
        .step_by(step())
        .collect()
}

fn runs(index: usize, case: &check_harness::Case) -> bool {
    let f = &case.world.functions["main"];
    index.is_multiple_of(4)
        && f.params.is_empty()
        && f.effects.instances.is_empty()
        && f.effects.tail.is_none()
}

/// The `.vsys` file the package manifest lists is its `// @export vsys`
/// directive, copied.
#[test]
fn the_walk_vsys_is_its_directive() {
    let dir = repo().join("packages/vesper-codegen-base/walk");
    let file = std::fs::read_to_string(dir.join("walk.vsys")).unwrap();
    let source = std::fs::read_to_string(dir.join("src/lib.ves")).unwrap();
    let directive: String = source
        .lines()
        .skip_while(|l| !l.starts_with("// @export vsys"))
        .skip(1)
        .map_while(|l| l.strip_prefix("// | ").or_else(|| l.strip_prefix("// |")))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(directive, file);
}

/// The demonstration hook checks and compiles with the walk's imports.
#[test]
fn the_walk_demo_compiles() {
    demo();
}

/// Outside a request the walk lends nothing.
#[test]
fn no_walk_outside_a_request() {
    assert_eq!(walk::query(query::KIND, 0, 0), NONE);
    assert_eq!(walk::query(query::HOME, 0, 0), NONE);
    walk::record(walk::effect::USED, 0, 0, 0);
    walk::reenter(walk::reentry::INSTRUCTION);
}

/// The demonstration: every `Read`, `Bind` and `Write` answered from
/// Vesper, with 0 CLIF, object and run differences, optimize on and off.
#[test]
fn the_walk_demo_matches_the_rust_lowering() {
    big_stack(walk_demo);
}

fn walk_demo() {
    let before = [&ASKED, &ANSWERED, &FIRST_CALL, &CALLS].map(|c| c.load(Ordering::Relaxed));
    let worlds = worlds();
    let mut total = Diff::default();
    let started = std::time::Instant::now();
    for (i, case) in worlds.iter().enumerate() {
        for optimize in [true, false] {
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
    let [asked, answered, first, calls] =
        [&ASKED, &ANSWERED, &FIRST_CALL, &CALLS].map(|c| c.load(Ordering::Relaxed));
    let (asked, answered, first, calls) = (
        asked - before[0],
        answered - before[1],
        first - before[2],
        calls - before[3],
    );
    let reentered = REENTRIES.each_ref().map(|c| c.load(Ordering::Relaxed));
    eprintln!(
        "walk state demo: {}; {asked} Read/Bind/Write requests, {answered} answered \
         ({first} emitted in Vesper, {} through re-entry: {} instruction, {} place value, \
         {} place address), {calls} hook calls, {:.1?}",
        total.summary(),
        answered - first,
        reentered[1],
        reentered[2],
        reentered[3],
        started.elapsed()
    );
    assert!(total.worlds > 0, "no corpus world compiled");
    assert!(first > 0, "the demo never emitted a request itself");
    assert!(answered > first, "the demo never re-entered");
    assert_eq!(asked, answered, "a Read/Bind/Write was declined");
    assert!(total.is_clean(), "{}", total.summary());
}

/// Protocol errors fail the compile: an unknown re-entry, and a hook that
/// re-enters and answers in one call.
#[test]
fn walk_protocol_errors_fail_the_compile() {
    big_stack(protocol_errors);
}

fn protocol_errors() {
    fn unknown(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
        let _ = lent;
        if walk::query(query::KIND, 0, 0) == kind::READ {
            walk::reenter(99);
        }
        Ok(false)
    }
    fn both(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
        use vesper_cranelift_sys::cranelift_codegen::ir::InstBuilder;
        if walk::query(query::KIND, 0, 0) != kind::BIND {
            return Ok(false);
        }
        walk::reenter(walk::reentry::INSTRUCTION);
        let v = lent
            .builder
            .ins()
            .iconst(vesper_cranelift_sys::cranelift_codegen::ir::types::I64, 0);
        lent.results.push(codegen_swap::word(v));
        Ok(true)
    }
    let case = worlds()
        .into_iter()
        .find(|c| {
            let w = &c.world;
            build(w, "main", false).is_ok()
                && codegen_swap::with_hooks(
                    CodegenHooks {
                        scalar: Some(both),
                        ..CodegenHooks::default()
                    },
                    || build(w, "main", false).is_err(),
                )
        })
        .expect("a corpus world with a binding");
    let err = codegen_swap::with_hooks(
        CodegenHooks {
            scalar: Some(both),
            ..CodegenHooks::default()
        },
        || build(&case.world, "main", false).unwrap_err(),
    );
    assert!(err.contains("re-entry 1 and answered"), "{err}");
    let err = codegen_swap::with_hooks(
        CodegenHooks {
            scalar: Some(unknown),
            ..CodegenHooks::default()
        },
        || build(&case.world, "main", false).unwrap_err(),
    );
    assert!(err.contains("unknown re-entry 99"), "{err}");
}

/// Debugging aid: writes the Rust and hooked CLIF text of the worlds
/// `WALK_STATE_WORLD` selects to `WALK_STATE_DUMP` (a directory), as
/// `<n>.rust.clif` and `<n>.hooked.clif`, optimize on (`.opt`) and off.
#[test]
#[ignore = "debugging aid; run with WALK_STATE_WORLD and WALK_STATE_DUMP"]
fn dump_walk_worlds() {
    big_stack(|| {
        let dir =
            std::path::PathBuf::from(std::env::var("WALK_STATE_DUMP").expect("WALK_STATE_DUMP"));
        std::fs::create_dir_all(&dir).unwrap();
        for (n, case) in worlds().iter().enumerate() {
            for optimize in [true, false] {
                let tag = if optimize { "opt" } else { "noopt" };
                let text = |b: Result<Build, String>| match b {
                    Ok(b) => b
                        .clif
                        .iter()
                        .map(|(k, v)| format!("; {k}\n{v}"))
                        .collect::<String>(),
                    Err(e) => e,
                };
                let rust = text(build(&case.world, "main", optimize));
                let hooked = text(codegen_swap::with_hooks(hooks(), || {
                    build(&case.world, "main", optimize)
                }));
                std::fs::write(dir.join(format!("{n}.{tag}.rust.clif")), rust).unwrap();
                std::fs::write(dir.join(format!("{n}.{tag}.hooked.clif")), hooked).unwrap();
            }
        }
    });
}
