//! The shared checker world and its word answers (self-hosting SH5 lane 10,
//! #1333), against Rust: one `Checks` value per world serves every layout
//! and IR check of it, each answer equals the Rust pass's, and dropping
//! the value frees everything it held. Its own test binary, because the
//! live allocation counters are global to the process.
#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/checks.rs"]
mod checks_binding;

#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/check_layout.rs"]
mod check_layout_binding;

#[path = "../../../tests/common/check_words.rs"]
mod check_words;

#[allow(dead_code)]
#[path = "../src/crossing.rs"]
mod crossing;

use std::path::{Path, PathBuf};
use vesper::ast::Type;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// The `.ves` files of `dir`, sorted.
fn sources(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    out.retain(|p| p.extension().is_some_and(|e| e == "ves"));
    out.sort();
    out
}

/// The types to lay out in `w`: every ADT without generics, every field
/// type of every ADT (which holds the generic instances), a tuple, a
/// missing name (a failing layout) and each of those twice.
fn types(w: &vesper::check::World) -> Vec<Type> {
    let mut out: Vec<Type> = vec![];
    for a in w.adts.values() {
        if a.generics.is_empty() {
            out.push(Type::named(&a.name));
        }
        for v in &a.variants {
            for f in &v.fields {
                out.push(f.ty.clone());
            }
        }
    }
    out.push(Type::Tuple(vec![Type::named("u8"), Type::named("String")]));
    out.push(Type::named("SwapCrossingMissing"));
    let again = out.clone();
    out.extend(again);
    out
}

#[test]
fn shared_world_answers_match_rust_and_free_everything() {
    let root = repository();
    let mut files = sources(&root.join("examples"));
    files.extend(sources(&root.join("bootstrap/corpus")));
    let (mut worlds, mut layouts, mut checks, mut refused) = (0, 0, 0, 0);
    let before = checks_binding::live_allocations();
    for file in &files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let parsed = vesper::parser::parse(&source);
        let checked = vesper::check::check(&parsed.module);
        if !checked.diagnostics.is_empty() {
            continue;
        }
        let w = checked.world;
        let mut log = check_words::WorldLog::new(&w);
        let Some(mut shared) = crossing::Shared::open(&mut log, &w) else {
            refused += 1;
            continue;
        };
        worlds += 1;
        let program = vesper::ir::lower_world(&w);
        // An IR check first and last, with layouts between: both swaps
        // read the one world in either order.
        for round in 0..2 {
            if let Some(part) = log.ir_part(&w, &mut shared.cur, &program) {
                let words = shared.checks.validate(&part);
                let got = check_words::decode_diagnostics(&words);
                let want = vesper::ir_check::rust_validate(&w, &program);
                assert_eq!(got, want, "{}: IR check round {round}", file.display());
                checks += 1;
            }
            if round == 1 {
                break;
            }
            for t in types(&w) {
                let Some(part) = log.layout_part(&w, &mut shared.cur, &t) else {
                    continue;
                };
                shared.checks.layout_of(&part);
                let got = crossing::decode_layout(shared.checks.as_ref().out(), &log);
                let want = vesper::layout::rust_layout_of(&w, &t);
                assert_eq!(
                    format!("{got:?}"),
                    format!("{want:?}"),
                    "{}: layout of {t}",
                    file.display()
                );
                layouts += 1;
            }
        }
    }
    assert_eq!(
        checks_binding::live_allocations(),
        before,
        "a dropped Checks frees its world"
    );
    eprintln!(
        "swap crossing: {worlds} worlds ({refused} refused), {layouts} layouts, {checks} IR checks"
    );
    assert!(worlds > 10 && layouts > 100 && checks > 10);
}

/// Per-call costs of one repeated layout (the "1.4 us" of SH4 lane 2's
/// report): the Vesper call with nothing new to read and its answer's
/// decode, for SH4 lane 2's `LayoutInput` (text answer, fresh arena and
/// output each call, answer copied out) and this lane's shared `Checks`
/// (word answer read in place), against the Rust layout. Ignored: a
/// timing, run with `--ignored --nocapture` in a release build.
#[test]
#[ignore]
fn repeated_layout_call_costs() {
    let root = repository();
    let source = std::fs::read_to_string(root.join("examples/transaction.ves")).unwrap();
    let parsed = vesper::parser::parse(&source);
    let w = vesper::check::check(&parsed.module).world;
    const N: u32 = 20_000;
    let per = |t: std::time::Instant| t.elapsed().as_nanos() as f64 / f64::from(N);
    println!(
        "type | rust ns | lane 2 vesper ns | lane 2 decode ns | shared vesper ns | shared decode ns"
    );
    for name in w.adts.keys() {
        if !w.adts[name].generics.is_empty() {
            continue;
        }
        let ty = Type::named(name);
        let t = std::time::Instant::now();
        for _ in 0..N {
            std::hint::black_box(vesper::layout::rust_layout_of(&w, &ty)).ok();
        }
        let rust = per(t);
        // SH4 lane 2: a LayoutInput that already holds the type.
        let mut log = check_words::WorldLog::new(&w);
        let mut cur = check_words::Cursor::default();
        let mut input = check_layout_binding::LayoutInput::new();
        let Some(part) = log.layout_part(&w, &mut cur, &ty) else {
            continue;
        };
        let _ = input.layout_of(&part);
        let part = log.layout_part(&w, &mut cur, &ty).unwrap();
        let t = std::time::Instant::now();
        let mut words = vec![];
        for _ in 0..N {
            words = std::hint::black_box(input.layout_of(&part));
        }
        let old_vesper = per(t);
        let t = std::time::Instant::now();
        for _ in 0..N {
            std::hint::black_box(check_words::decode_layout(&words)).ok();
        }
        let old_decode = per(t);
        // This lane: the shared Checks.
        let mut log = check_words::WorldLog::new(&w);
        let mut shared = crossing::Shared::open(&mut log, &w).unwrap();
        let first = log.layout_part(&w, &mut shared.cur, &ty).unwrap();
        shared.checks.layout_of(&first);
        let part = log.layout_part(&w, &mut shared.cur, &ty).unwrap();
        let t = std::time::Instant::now();
        for _ in 0..N {
            std::hint::black_box(shared.checks.layout_of(&part));
        }
        let new_vesper = per(t);
        let t = std::time::Instant::now();
        for _ in 0..N {
            std::hint::black_box(crossing::decode_layout(shared.checks.as_ref().out(), &log)).ok();
        }
        let new_decode = per(t);
        println!(
            "{name} | {rust:.0} | {old_vesper:.0} | {old_decode:.0} | {new_vesper:.0} | {new_decode:.0}"
        );
    }
}
