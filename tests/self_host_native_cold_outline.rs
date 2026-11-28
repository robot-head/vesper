//! `@vesper/native-cold-outline` (packages/vesper-native-cold-outline)
//! against `native_cold_outline::outline` (#1829).
//!
//! Every function the native optimize pipeline hands the outliner, over
//! the corpus `main`s and the programs of tests/native_cold_outline.rs, is
//! captured before the pass (`native_cold_outline::capture`). Rust runs
//! the pass on one copy in a fresh object module; the Vesper package reads
//! another in the words of
//! crates/vesper-cranelift-sys/src/clif_words.rs and answers with its
//! decisions, which `clif_outline::apply_outline` builds in a second fresh
//! module. The parents, the helpers (keys, ids and functions) and any
//! error must be equal.
//!
//! - `NATIVE_COLD_OUTLINE_STRIDE=n` keeps every n-th corpus world.
//! - `CHECK_HARNESS_SPEED=1` runs the ABBA timing (release build).
#[path = "common/check_harness.rs"]
mod check_harness;
// The Cranelift shim's extraction (#2634), function words and edit replay
// (#2606), included by path: the root crate cannot depend on
// vesper-cranelift-sys (a Bazel dev-dependency cycle).
#[path = "../crates/vesper-cranelift-sys/src/clif_outline.rs"]
mod clif_outline;
#[path = "../crates/vesper-cranelift-sys/src/clif_words.rs"]
mod clif_words;

use check_harness::*;
use cranelift_codegen::ir;
use cranelift_module::Module as _;
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::sync::OnceLock;
use vesper::{check, native, native_cold_outline, parser};

fn entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| {
        Package::load("packages/vesper-native-cold-outline").entry("outline_words", true)
    })
}

fn module() -> ObjectModule {
    let isa = native::object_isa(None).unwrap();
    ObjectModule::new(
        ObjectBuilder::new(isa, "cold", cranelift_module::default_libcall_names()).unwrap(),
    )
}

type Outcome = Result<(String, Vec<(u32, String, String)>), String>;

fn shown(
    func: &ir::Function,
    helpers: Vec<(cranelift_module::FuncId, String, ir::Function)>,
) -> (String, Vec<(u32, String, String)>) {
    (
        func.display().to_string(),
        helpers
            .into_iter()
            .map(|(id, key, f)| (id.as_u32(), key, f.display().to_string()))
            .collect(),
    )
}

fn rust(key: &str, f: &ir::Function) -> Outcome {
    let mut m = module();
    let mut func = f.clone();
    let helpers = native_cold_outline::outline(&mut m, key, &mut func)?;
    Ok(shown(&func, helpers))
}

fn vesper(key: &str, f: &ir::Function) -> Outcome {
    let mut m = module();
    let ptr = m.target_config().pointer_type();
    let words = entry().words(&clif_words::encode_with(f, false, Some(ptr)));
    let mut func = f.clone();
    let helpers = clif_outline::apply_outline(&mut m, key, &mut func, &words)?;
    Ok(shown(&func, helpers))
}

/// `Ok(helpers)` when the two agree.
fn compare(key: &str, f: &ir::Function) -> Result<usize, String> {
    let (a, b) = (rust(key, f), vesper(key, f));
    match (&a, &b) {
        (Ok((pa, ha)), Ok((pb, hb))) => {
            if ha.len() != hb.len() {
                return Err(format!(
                    "Rust outlines {} regions, Vesper {}",
                    ha.len(),
                    hb.len()
                ));
            }
            for ((ia, ka, fa), (ib, kb, fb)) in ha.iter().zip(hb) {
                if ia != ib || ka != kb {
                    return Err(format!("helper {ka} ({ia}) vs {kb} ({ib})"));
                }
                if let Some(d) = first_difference(fa, fb) {
                    return Err(format!("helper {ka}: {d}"));
                }
            }
            match first_difference(pa, pb) {
                Some(d) => Err(format!("parent: {d}")),
                None => Ok(ha.len()),
            }
        }
        (Err(x), Err(y)) if x == y => Ok(0),
        _ => Err(format!(
            "rust {:?}, vesper {:?}",
            a.as_ref().map(|_| ()),
            b.as_ref().map(|_| ())
        )),
    }
}

fn checked(source: &str) -> vesper::check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c.world
}

/// The functions the outliner gets for `world`'s optimized `main`.
fn captured(world: &vesper::check::World) -> Vec<(String, ir::Function)> {
    let (_, funcs) =
        native_cold_outline::capture(|| native::object_with_options(world, "main", None, true));
    funcs
}

/// The program sources of tests/native_cold_outline.rs (each
/// `let source = "...";`).
fn kernel_sources() -> Vec<String> {
    let text = std::fs::read_to_string(repo().join("tests/native_cold_outline.rs")).unwrap();
    let mut out = vec![];
    for part in text.split("let source = \"").skip(1) {
        if let Some(end) = part.find("\";") {
            out.push(part[..end].to_string());
        }
    }
    out
}

fn run_all<'a>(
    worlds: impl Iterator<Item = (String, &'a vesper::check::World)>,
) -> (usize, usize, usize) {
    let (mut funcs, mut regions, mut bad) = (0, 0, 0);
    for (name, world) in worlds {
        for (key, f) in captured(world) {
            funcs += 1;
            match compare(&key, &f) {
                Ok(n) => regions += n,
                Err(e) => {
                    bad += 1;
                    if bad <= 6 {
                        eprintln!("native cold outline: {name} {key}: {e}");
                    }
                }
            }
        }
    }
    (funcs, regions, bad)
}

#[test]
fn kernels_match_native_cold_outline() {
    let worlds: Vec<(String, vesper::check::World)> = kernel_sources()
        .iter()
        .enumerate()
        .map(|(i, s)| (format!("kernel {i}"), checked(s)))
        .collect();
    assert!(worlds.len() >= 4, "found {} kernel programs", worlds.len());
    let (funcs, regions, bad) = run_all(worlds.iter().map(|(n, w)| (n.clone(), w)));
    eprintln!(
        "native cold outline kernels: {bad} differ over {funcs} functions; Rust outlines {regions} regions"
    );
    assert!(regions >= 3, "the kernels outline only {regions} regions");
    assert_eq!(bad, 0);
}

fn stride() -> usize {
    std::env::var("NATIVE_COLD_OUTLINE_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

#[test]
fn corpus_matches_native_cold_outline() {
    let start = std::time::Instant::now();
    let cases = corpus();
    let (funcs, regions, bad) = run_all(
        cases
            .iter()
            .step_by(stride())
            .map(|c| (c.name.clone(), &c.world)),
    );
    eprintln!(
        "native cold outline corpus: {bad} differ over {funcs} functions of {} worlds; \
         Rust outlines {regions} regions; {:.1?}",
        cases.len().div_ceil(stride()),
        start.elapsed()
    );
    println!("flip-gate differential differences={bad} cases={funcs}");
    assert_eq!(bad, 0);
}

/// ABBA: `native_cold_outline::outline` against the Vesper decisions with
/// encode, call and `apply_outline`, over every captured function of the
/// corpus and the kernels. Both build the helpers in a fresh module.
#[test]
fn flip_gate() {
    if !speed() {
        return;
    }
    let mut funcs: Vec<(String, ir::Function)> =
        corpus().iter().flat_map(|c| captured(&c.world)).collect();
    for s in kernel_sources() {
        funcs.extend(captured(&checked(&s)));
    }
    let rust = || {
        time(|| {
            for (k, f) in &funcs {
                std::hint::black_box(rust(k, f).ok());
            }
        })
        .as_nanos()
    };
    let vesper = || {
        time(|| {
            for (k, f) in &funcs {
                std::hint::black_box(vesper(k, f).ok());
            }
        })
        .as_nanos()
    };
    for _ in 0..8 {
        let (a, b, c, d) = (rust(), vesper(), vesper(), rust());
        println!("flip-gate pairs rust={} vesper={}", a + d, b + c);
    }
}
