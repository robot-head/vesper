//! Self-hosting SH2 lane 5 (#1177): `@vesper/check-layout` against
//! `src/layout.rs` over the corpus, in both forms the lane uses.
//!
//! - Text: every type the world mentions, laid out in Vesper over the whole
//!   world (the harness input), printed field by field, against
//!   `Layout::of` printed the same way.
//! - Words: the hybrid build's path. Each type is encoded with only the ADTs
//!   its layout can reach (`tests/common/check_words.rs`), the
//!   Vesper `layout_words` stream is read back into a `Layout`, and the
//!   result must equal `Layout::of` exactly (`Debug`, which shows every
//!   field, type and nested layout), errors included.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;

use check_harness::*;
use std::fmt::Write;
use std::sync::OnceLock;
use vesper::ast::Type;
use vesper::check::World;
use vesper::layout::{Layout, LayoutField, LayoutKind};

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-check-layout"))
}

fn write_fields(o: &mut String, fields: &[LayoutField]) {
    for f in fields {
        write!(o, "{}@{}: {} = (", f.name, f.offset, f.ty).unwrap();
        write_layout(o, &f.layout);
        o.push_str("); ");
    }
}

/// `write_layout` of packages/vesper-check-layout/src/lib.ves.
fn write_layout(o: &mut String, l: &Layout) {
    write!(o, "{}", l.size).unwrap();
    match &l.kind {
        LayoutKind::Scalar => o.push_str(" S"),
        LayoutKind::Product(fs) => {
            o.push_str(" P{");
            write_fields(o, fs);
            o.push('}');
        }
        LayoutKind::Enum(vs) => {
            o.push_str(" E{");
            for v in vs {
                write!(o, "{}(", v.name).unwrap();
                write_fields(o, &v.fields);
                o.push_str(");");
            }
            o.push('}');
        }
    }
}

/// The world's types the port lays out: `world_types` without those that
/// reach `i128`/`u128`, which the hybrid build leaves to Rust
/// (`wide_integers_stay_on_rust`). Corpus sources hold 128-bit types since
/// integer literals reach `u128::MAX` (#1244).
fn ported_types(w: &World) -> Vec<Type> {
    world_types(w)
        .into_iter()
        .filter(|t| check_words::layout_input(w, t).is_some())
        .collect()
}

fn rust_text(w: &World, types: &[Type]) -> String {
    let mut o = String::new();
    for t in types {
        write!(o, "T {t}: ").unwrap();
        match Layout::of(w, t) {
            Ok(l) => write_layout(&mut o, &l),
            Err(e) => write!(o, "error {e}").unwrap(),
        }
        o.push('\n');
    }
    o
}

fn vesper_text(f: &Entry, w: &World, types: &[Type]) -> String {
    f.text(&encode(w, |e, tail| {
        tail.push(types.len() as u64);
        for t in types {
            tail.push(e.ty(t));
        }
    }))
}

#[test]
fn layout_text_matches_rust() {
    let f = package().entry("check_layout_text", true);
    let bad = differential(
        "layout text",
        |c| rust_text(&c.world, &ported_types(&c.world)),
        |c| vesper_text(&f, &c.world, &ported_types(&c.world)),
    );
    assert_eq!(bad, 0);
}

/// The optimizer off agrees with it on, on every tenth world.
#[test]
fn layout_text_unoptimized_matches_rust() {
    let f = package().entry("check_layout_text", false);
    let mut bad = 0;
    for c in corpus().iter().step_by(10) {
        let types = ported_types(&c.world);
        if first_difference(
            &rust_text(&c.world, &types),
            &vesper_text(&f, &c.world, &types),
        )
        .is_some()
        {
            bad += 1;
        }
    }
    assert_eq!(bad, 0);
}

/// The hybrid build's word stream decodes to exactly `Layout::of`.
#[test]
fn layout_words_decode_to_rust_layouts() {
    let f = package().entry("check_layout_words", true);
    let (mut bad, mut checked, mut errors) = (0, 0, 0);
    for c in corpus() {
        for t in ported_types(&c.world) {
            let input = check_words::layout_input(&c.world, &t).expect("encodable corpus type");
            let vesper = check_words::decode_layout(&f.words(&input));
            let rust = Layout::of(&c.world, &t);
            checked += 1;
            errors += rust.is_err() as usize;
            if format!("{vesper:?}") != format!("{rust:?}") {
                bad += 1;
                if bad <= 3 {
                    eprintln!("{} {t}:\n  rust:   {rust:?}\n  vesper: {vesper:?}", c.name);
                }
            }
        }
    }
    eprintln!("layout words: {bad} of {checked} types differ ({errors} errors)");
    assert!(checked > 1000, "only {checked} types");
    assert_eq!(bad, 0);
}

/// A changed input changes the answer (the decoder really reads the type).
#[test]
fn layout_words_depend_on_the_type() {
    let f = package().entry("check_layout_words", true);
    let w = &corpus()[0].world;
    let a = check_words::decode_layout(
        &f.words(&check_words::layout_input(w, &Type::named("u64")).unwrap()),
    );
    let b = check_words::decode_layout(
        &f.words(
            &check_words::layout_input(
                w,
                &Type::Tuple(vec![Type::named("u64"), Type::named("u8")]),
            )
            .unwrap(),
        ),
    );
    assert_eq!(a.as_ref().map(|l| l.size), Ok(8));
    assert_eq!(b.as_ref().map(|l| l.size), Ok(16));
    let missing = check_words::decode_layout(
        &f.words(&check_words::layout_input(w, &Type::named("HybridLayoutMissing")).unwrap()),
    );
    assert_eq!(
        missing.unwrap_err(),
        "native layout is not defined for HybridLayoutMissing"
    );
}

/// A world whose types hold `i128` and `u128` (#1174), which no corpus
/// program uses yet.
const WIDE_WORLD: &str = "copy struct W { lo: u8, x: i128, y: u128 }
copy enum E { A(i128), B(u8, u128), C }
struct G[T] { v: T, n: i128 }
fn f(a: i128, b: u128, w: W) -> (u8, i128) { (w.lo, a) }
fn g(e: E) -> i128 { match e { E::A(x) => x, E::B(_, _) => 0, E::C => 1, } }
fn h(sink g: G[u128]) -> u128 { g.v }
fn main() -> i64 { 0 }";

/// `i128` and `u128` are two words, in Rust and in `@vesper/check-layout`
/// (SH5 lane 6, #1329): both forms agree with `Layout::of`, alone, in
/// tuples, in struct fields and in enum and generic payloads.
#[test]
fn wide_integers_match_rust() {
    let parsed = vesper::parser::parse(WIDE_WORLD);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let w = &checked.world;
    let mut types = world_types(w);
    for n in ["i128", "u128"] {
        let wide = Type::Named(n.into(), vec![]);
        types.push(Type::Tuple(vec![
            Type::Named("u8".into(), vec![]),
            wide.clone(),
        ]));
        types.push(Type::Named("Option".into(), vec![wide.clone()]));
        types.push(wide);
    }
    assert!(types.iter().any(|t| format!("{t}").contains("i128")));
    let text = package().entry("check_layout_text", true);
    let rust = rust_text(w, &types);
    assert_eq!(
        first_difference(&rust, &vesper_text(&text, w, &types)),
        None
    );
    assert!(rust.contains("16 S"), "{rust}");
    let words = package().entry("check_layout_words", true);
    for t in &types {
        let input = check_words::layout_input(w, t).expect("encodable wide type");
        let vesper = check_words::decode_layout(&words.words(&input));
        assert_eq!(
            format!("{vesper:?}"),
            format!("{:?}", Layout::of(w, t)),
            "{t}"
        );
    }
}

/// ABBA: `Layout::of` in Rust against the Vesper layout with its input
/// encoding and result decoding (the hybrid build's cost per call), one
/// call per type of every world.
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn layout_speed() {
    if !speed() {
        return;
    }
    let words = package().entry("check_layout_words", true);
    let cases: Vec<(&World, Vec<Type>)> = corpus()
        .iter()
        .map(|c| (&c.world, ported_types(&c.world)))
        .collect();
    abba(
        "layout per call (encode + Vesper + decode)",
        &mut || {
            time(|| {
                for (w, ts) in &cases {
                    for t in ts {
                        std::hint::black_box(Layout::of(w, t).ok());
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for (w, ts) in &cases {
                    for t in ts {
                        let input = check_words::layout_input(w, t).unwrap();
                        std::hint::black_box(check_words::decode_layout(&words.words(&input)).ok());
                    }
                }
            })
        },
    );
}
