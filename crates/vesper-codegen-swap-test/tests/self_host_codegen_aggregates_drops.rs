//! Self-hosting SH4 lane 8 (#1282), part 2: native drop glue from
//! `@vesper/codegen-aggregates-emit`, through the `drops` hook of
//! `vesper::codegen_swap`.
//!
//! Ported so far: `codegen_swap::drops::DROP_BOX` (`Codegen::drop_box_value`:
//! the inline drop of a value that owns boxes, the recursive types' drop
//! helpers and the box frees). The request carries the value; this glue
//! appends the value's type with the ADTs it reaches as a check-data input
//! (`check_words::layout_input`), since the hook sees only words. Over the
//! corpus worlds whose objects free boxes, with the optimizer on and off,
//! every function's CLIF text, every object and every sampled JIT run must
//! equal the Rust build's. `CODEGEN_DROPS_STEP=n` compares every n-th world
//! (default 1).
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;
#[path = "../../../tests/common/check_words.rs"]
mod check_words;

use check_harness::repo;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use vesper::codegen_swap::{self, CodegenHooks, Lent};
use vesper::native::WordFunction;
use vesper_codegen_swap_test::*;

fn hook() -> &'static WordFunction {
    static HOOK: OnceLock<WordFunction> = OnceLock::new();
    // Compiled with no hook in effect (see the pilot test's note).
    HOOK.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let checked = foreign_package(&repo(), "packages/vesper-codegen-aggregates/emit");
            hook_function(&checked, "drops_hook")
        })
    })
}

/// `DROP_BOX` requests the Vesper hook answered, and the ones whose type
/// the glue could not encode (left to Rust).
static DROPS: AtomicU64 = AtomicU64::new(0);
static UNENCODED: AtomicU64 = AtomicU64::new(0);

/// The request's words: the value, then the type input's byte length and
/// its bytes, eight to a word.
fn request(lent: &Lent<'_, '_>) -> Option<Vec<u64>> {
    let ty = vesper::numeric::default_type(lent.ty?);
    let bytes = check_words::layout_input(lent.world, &ty)?;
    let mut args = Vec::with_capacity(2 + bytes.len().div_ceil(8));
    args.extend_from_slice(lent.args);
    args.push(bytes.len() as u64);
    for chunk in bytes.chunks(8) {
        let mut w = [0u8; 8];
        w[..chunk.len()].copy_from_slice(chunk);
        args.push(u64::from_le_bytes(w));
    }
    Some(args)
}

fn drops(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    if lent.op != codegen_swap::drops::DROP_BOX {
        return Ok(false);
    }
    let Some(args) = request(lent) else {
        UNENCODED.fetch_add(1, Ordering::Relaxed);
        return Ok(false);
    };
    let mut with_type = Lent {
        builder: &mut *lent.builder,
        module: &mut *lent.module,
        world: lent.world,
        program: lent.program,
        site: lent.site,
        op: lent.op,
        args: &args,
        ty: lent.ty,
        results: &mut *lent.results,
        jit: lent.jit,
        optimize: lent.optimize,
    };
    let handled = vesper_hook(&mut with_type, hook())?;
    if handled {
        DROPS.fetch_add(1, Ordering::Relaxed);
    }
    Ok(handled)
}

fn step() -> usize {
    std::env::var("CODEGEN_DROPS_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1)
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

/// The corpus worlds with a `main` whose host object frees boxes (it
/// names `vesper_box_free`), every `step()`-th.
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
                        .windows(b"vesper_box_free".len())
                        .any(|w| w == b"vesper_box_free")
                })
            })
            .collect()
    })
}

fn runs(case: &check_harness::Case) -> bool {
    let f = &case.world.functions["main"];
    f.params.is_empty() && f.effects.instances.is_empty() && f.effects.tail.is_none()
}

/// The drop glue swap: 0 CLIF differences, byte-identical objects and
/// equal runs, optimize on and off.
#[test]
fn box_drops_match_the_rust_emitter() {
    big_stack(drop_swap);
}

fn drop_swap() {
    let hooks = CodegenHooks {
        drops: Some(drops),
        ..CodegenHooks::default()
    };
    let before = DROPS.load(Ordering::Relaxed);
    let declined = DECLINED.load(Ordering::Relaxed);
    let mut total = Diff::default();
    for case in worlds() {
        for optimize in [true, false] {
            total.add(compare(
                &case.name,
                &case.world,
                "main",
                hooks,
                optimize,
                runs(case),
            ));
        }
    }
    let answered = DROPS.load(Ordering::Relaxed) - before;
    let declined = DECLINED.load(Ordering::Relaxed) - declined;
    eprintln!(
        "codegen drops: {} over {} worlds; {answered} box drops emitted from Vesper, \
         {declined} declined, {} not encodable",
        total.summary(),
        worlds().len(),
        UNENCODED.load(Ordering::Relaxed)
    );
    assert!(total.worlds > 0, "no corpus world compiled");
    assert!(total.is_clean(), "{}", total.summary());
}

/// The `Box` programs of the fip and box tests (tests/fip.rs,
/// tests/borrowed_box_match.rs): the corpus has few box drops. Each raw
/// string source there that checks and has a `main` is one world.
fn box_sources() -> Vec<(String, vesper::check::World)> {
    let mut out = vec![];
    for file in ["tests/fip.rs", "tests/borrowed_box_match.rs"] {
        let text = std::fs::read_to_string(repo().join(file)).unwrap_or_default();
        for (i, part) in text.split("r#\"").skip(1).enumerate() {
            let Some(src) = part.split("\"#").next() else {
                continue;
            };
            if !src.contains("Box") || !src.contains("fn main") {
                continue;
            }
            let parsed = vesper::parser::parse(src);
            if !parsed.diagnostics.is_empty() {
                continue;
            }
            let checked = vesper::check::check(&parsed.module);
            if checked.diagnostics.is_empty() && checked.world.functions.contains_key("main") {
                out.push((format!("{file}#{i}"), checked.world));
            }
        }
    }
    out
}

/// The drop glue swap over the box test programs, which have recursive
/// and enum box types.
#[test]
fn box_test_programs_match() {
    big_stack(|| {
        let hooks = CodegenHooks {
            drops: Some(drops),
            ..CodegenHooks::default()
        };
        let sources = box_sources();
        let before = DROPS.load(Ordering::Relaxed);
        let mut total = Diff::default();
        for (name, world) in &sources {
            for optimize in [true, false] {
                total.add(compare(name, world, "main", hooks, optimize, true));
            }
        }
        let answered = DROPS.load(Ordering::Relaxed) - before;
        eprintln!(
            "codegen drops (box tests): {} over {} programs; {answered} box drops emitted from Vesper",
            total.summary(),
            sources.len()
        );
        assert!(sources.len() > 3, "only {} box programs", sources.len());
        assert!(answered > 0, "the drops hook never ran");
        assert!(total.is_clean(), "{}", total.summary());
    });
}
