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
        |c| rust_text(&c.world, &world_types(&c.world)),
        |c| vesper_text(&f, &c.world, &world_types(&c.world)),
    );
    assert_eq!(bad, 0);
}

/// The optimizer off agrees with it on, on every tenth world.
#[test]
fn layout_text_unoptimized_matches_rust() {
    let f = package().entry("check_layout_text", false);
    let mut bad = 0;
    for c in corpus().iter().step_by(10) {
        let types = world_types(&c.world);
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
        for t in world_types(&c.world) {
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

/// `i128` and `u128` (#1174) are two words in Rust and not in the
/// check-data format yet, so the hybrid build sends them to Rust.
#[test]
fn wide_integers_stay_on_rust() {
    let w = &corpus()[0].world;
    for n in ["i128", "u128"] {
        let wide = Type::Named(n.into(), vec![]);
        assert!(check_words::layout_input(w, &wide).is_none(), "{n}");
        let pair = Type::Tuple(vec![Type::Named("u8".into(), vec![]), wide]);
        assert!(check_words::layout_input(w, &pair).is_none(), "({n})");
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
        .map(|c| (&c.world, world_types(&c.world)))
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
