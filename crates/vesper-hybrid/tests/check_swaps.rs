//! The checker swaps (SH2 lane 5, #1177), called directly: the Vesper
//! `layout_of_words` and `validate_words` (modules/check_layout and
//! modules/ir_check, linked by build.rs) through the hybrid's encoder and
//! decoders (tests/common/check_words.rs) against the Rust `Layout::of`
//! and `ir_check::validate` on the examples and the bootstrap corpus. The root package's
//! tests/hybrid_layout.rs and tests/hybrid_ir_check.rs run the same
//! comparison over the whole corpus with the IR mutants.
#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/check_layout.rs"]
mod layout_binding;

#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/ir_check.rs"]
mod ir_check_binding;

#[path = "../../../tests/common/check_words.rs"]
mod check_words;

use std::collections::BTreeSet;
use std::path::PathBuf;
use vesper::ast::Type;
use vesper::check::World;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The checked worlds of the single-file programs under `examples/` and
/// `bootstrap/corpus/` that check with no diagnostics.
fn worlds() -> Vec<(String, World)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "ves") {
                out.push(p);
            }
        }
    }
    let mut files = vec![];
    walk(&root().join("examples"), &mut files);
    walk(&root().join("bootstrap/corpus"), &mut files);
    files.sort();
    let mut out = vec![];
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap();
        let parsed = vesper::parser::parse(&source);
        if !parsed.diagnostics.is_empty() {
            continue;
        }
        let checked = vesper::check::check(&parsed.module);
        if checked.diagnostics.is_empty() {
            out.push((path.display().to_string(), checked.world));
        }
    }
    assert!(out.len() >= 20, "only {} corpus worlds", out.len());
    out
}

/// Every ADT and field type of `w`, once each.
fn types(w: &World) -> Vec<Type> {
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    for a in w.adts.values() {
        let own = Type::Named(
            a.name.clone(),
            a.generics.iter().map(|g| Type::named(&g.name)).collect(),
        );
        for t in std::iter::once(own).chain(
            a.variants
                .iter()
                .flat_map(|v| v.fields.iter().map(|f| f.ty.clone())),
        ) {
            if seen.insert(format!("{t:?}")) {
                out.push(t);
            }
        }
    }
    out
}

#[test]
fn formats_are_the_decoders() {
    assert_eq!(layout_binding::format(), 1);
    assert_eq!(ir_check_binding::format(), 1);
}

#[test]
fn layout_matches_rust_on_the_examples() {
    let mut checked = 0;
    for (name, w) in worlds() {
        for t in types(&w) {
            let input = check_words::layout_input(&w, &t).unwrap();
            let vesper = check_words::decode_layout(&layout_binding::layout_of_words(&input));
            let rust = vesper::layout::rust_layout_of(&w, &t);
            assert_eq!(format!("{vesper:?}"), format!("{rust:?}"), "{name}: {t}");
            checked += 1;
        }
    }
    assert!(checked > 100, "only {checked} types");
}

#[test]
fn ir_check_matches_rust_on_the_examples() {
    let mut checked = 0;
    for (name, w) in worlds() {
        let program = vesper::ir::lower_world(&w);
        // Worlds with element views go to Rust (tests/common/check_words.rs).
        let Some(input) = check_words::ir_input(&w, &program) else {
            continue;
        };
        checked += 1;
        let vesper = check_words::decode_diagnostics(&ir_check_binding::validate_words(&input));
        assert_eq!(
            vesper,
            vesper::ir_check::rust_validate(&w, &program),
            "{name}"
        );
    }
    assert!(checked >= 20, "only {checked} worlds checked");
}
