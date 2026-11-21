//! Self-hosting SH5 lane 18 (#1341), part 1: the head of the extracted Lean
//! module (`src/lean.rs` `extract_report` up to the source functions: the
//! prelude, the divergence class, the arithmetic simp set, the ADT groups
//! with their recursive-structure getters, the effect classes and their
//! CPS lifts, and the `Fail` models) in Vesper
//! (`packages/vesper-lean-extract`), compared byte for byte with Rust.
//!
//! The head does not read function bodies, so the Rust side extracts each
//! corpus world with its functions removed: that is exactly the head, and
//! for every world whose full extraction succeeds it is checked to be a
//! prefix of the full module's declarations.
//!
//! The input is the harness encoding of the world (lane 4) followed by the
//! effect section ([`effect_task`]), which `read_effects` in
//! `packages/vesper-lean-extract/src/input.ves` decodes.

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::sync::OnceLock;
use vesper::ast::{Adt, Effect, Effects, Field, Mode, Pass, Type, Variant};
use vesper::check::World;
use vesper_lean_syntax::{Declaration, Module};

/// The effect section: per effect of `world.effects` (name order), its
/// name, generic names, and per operation its name, parameters (pass and
/// type) and result type.
fn effect_task(enc: &mut Enc, tail: &mut Vec<u64>, world: &World) {
    tail.push(world.effects.len() as u64);
    for e in world.effects.values() {
        tail.push(enc.name(&e.name));
        tail.push(e.generics.len() as u64);
        for g in &e.generics {
            tail.push(enc.name(&g.name));
        }
        tail.push(e.ops.len() as u64);
        for op in &e.ops {
            tail.push(enc.name(&op.name));
            tail.push(op.params.len() as u64);
            for p in &op.params {
                tail.push(pass_word(p.pass));
                tail.push(enc.ty(&p.ty));
            }
            tail.push(enc.ty(&op.result));
        }
    }
}

pub fn head_input(world: &World) -> Vec<u8> {
    encode(world, |enc, tail| effect_task(enc, tail, world))
}

/// The world without its functions: what `extract_report` builds before the
/// first function (nothing after the head reads ADTs or effects only).
fn head_world(world: &World) -> World {
    let mut w = world.clone();
    w.functions.clear();
    w.function_results.clear();
    w
}

/// Rust's head text: the module, or `error: <message>`.
fn rust_text(world: &World) -> String {
    match vesper::lean::extract(&head_world(world)) {
        Ok(text) => text,
        Err(e) => format!("error: {e}\n"),
    }
}

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-lean-extract"))
}

fn entry(optimize: bool) -> &'static Entry {
    static ON: OnceLock<Entry> = OnceLock::new();
    static OFF: OnceLock<Entry> = OnceLock::new();
    let cell = if optimize { &ON } else { &OFF };
    cell.get_or_init(|| package().entry("extract_head", optimize))
}

fn vesper_text(world: &World, optimize: bool) -> String {
    entry(optimize).text(&head_input(world))
}

fn namespace(m: &Module) -> &[Declaration] {
    match m.declarations.last() {
        Some(Declaration::Namespace { declarations, .. }) => declarations,
        _ => panic!("extracted module ends with its namespace"),
    }
}

#[test]
fn head_matches_rust_on_the_corpus() {
    let corpus = corpus();
    let errors = corpus
        .iter()
        .filter(|c| rust_text(&c.world).starts_with("error: "))
        .count();
    let adts: usize = corpus.iter().map(|c| c.world.adts.len()).sum();
    let effects: usize = corpus.iter().map(|c| c.world.effects.len()).sum();
    eprintln!(
        "lean head: {} worlds, {adts} ADTs, {effects} effects, {errors} refused by Rust",
        corpus.len()
    );
    let bad = differential(
        "lean head",
        |c| rust_text(&c.world),
        |c| vesper_text(&c.world, true),
    );
    assert_eq!(bad, 0);
}

#[test]
fn unoptimized_jit_agrees() {
    let corpus = corpus();
    let mut bad = 0;
    for c in corpus.iter().step_by(7) {
        if vesper_text(&c.world, false) != vesper_text(&c.world, true) {
            bad += 1;
            eprintln!("{}: optimized and unoptimized JIT differ", c.name);
        }
    }
    assert_eq!(bad, 0);
}

fn named(n: &str, args: Vec<Type>) -> Type {
    Type::Named(n.into(), args)
}

fn function(params: Vec<(Pass, Type)>, result: Type, effects: Effects) -> Type {
    Type::Function {
        params,
        result: Box::new(result),
        effects,
        mode: Mode::Copy,
    }
}

fn io() -> Effects {
    Effects {
        instances: vec![Effect {
            label: None,
            name: "Fail".into(),
            args: vec![named("u8", vec![])],
        }],
        tail: None,
    }
}

/// A world with one extra structure (or enum) `name` whose fields have the
/// types `fields`.
fn with_adt(base: &World, name: &str, is_enum: bool, fields: Vec<Type>) -> World {
    let mut w = base.clone();
    let fields: Vec<Field> = fields
        .into_iter()
        .enumerate()
        .map(|(i, ty)| Field {
            name: format!("x{i}"),
            ty,
            modality: None,
            temperature: None,
        })
        .collect();
    w.adts.insert(
        name.into(),
        Adt {
            name: name.into(),
            mode: Mode::Copy,
            generics: vec![],
            variants: vec![Variant {
                name: if is_enum { "A".into() } else { name.into() },
                fields,
                named: true,
            }],
            is_enum,
            tile_width: None,
            span: 1..2,
        },
    );
    w
}

/// Mutants of the corpus's first world with effects: the refusals the head
/// can raise (128-bit fields, rows, effectful function fields, names Lean
/// cannot spell) and the type shapes the corpus rarely has (function fields
/// with `inout` parameters, results, tiled sequences, nested tuples).
fn mutants() -> Vec<(String, World)> {
    let base = &corpus()
        .iter()
        .find(|c| !c.world.effects.is_empty() && !c.world.adts.is_empty())
        .expect("a corpus world with effects")
        .world;
    let u8_ = || named("u8", vec![]);
    let cases: Vec<(&str, World)> = vec![
        (
            "i128 field",
            with_adt(base, "Wide", false, vec![named("i128", vec![])]),
        ),
        (
            "u128 enum field",
            with_adt(base, "Wide", true, vec![u8_(), named("u128", vec![])]),
        ),
        (
            "row field",
            with_adt(base, "Rowed", false, vec![Type::Row(io())]),
        ),
        (
            "effectful function field",
            with_adt(
                base,
                "Cb",
                false,
                vec![function(vec![(Pass::In, u8_())], u8_(), io())],
            ),
        ),
        (
            "function fields",
            with_adt(
                base,
                "Cb",
                false,
                vec![
                    function(
                        vec![(Pass::Inout, u8_()), (Pass::In, named("i32", vec![]))],
                        named("bool", vec![]),
                        Effects::default(),
                    ),
                    function(vec![], Type::Tuple(vec![u8_()]), Effects::default()),
                    function(
                        vec![(Pass::Sink, Type::Tuple(vec![]))],
                        Type::IntLiteral(-3),
                        Effects::default(),
                    ),
                ],
            ),
        ),
        (
            "result and tiled fields",
            with_adt(
                base,
                "Shapes",
                true,
                vec![
                    named("Result", vec![u8_(), named("String", vec![])]),
                    named("TiledSeq", vec![named("f32", vec![]), Type::IntLiteral(4)]),
                    Type::Tuple(vec![
                        u8_(),
                        Type::Tuple(vec![named("usize", vec![]), Type::Infer]),
                    ]),
                    Type::IntLiteral(i64::MAX as i128 + 1),
                ],
            ),
        ),
        (
            "unspellable name",
            with_adt(base, "A«B", false, vec![u8_()]),
        ),
        // Rust checks the declaration's name before its field types.
        (
            "unspellable name and i128 field",
            with_adt(base, "A«B", false, vec![named("i128", vec![])]),
        ),
        ("unspellable enum and u128 field", {
            let mut w = with_adt(base, "E«", true, vec![named("u128", vec![])]);
            w.adts.get_mut("E«").unwrap().variants[0].name = "V»".into();
            w
        }),
        ("unspellable variant and u128 field", {
            let mut w = with_adt(base, "Ev", true, vec![named("u128", vec![])]);
            w.adts.get_mut("Ev").unwrap().variants[0].name = "V»".into();
            w
        }),
        (
            "unspellable field type",
            with_adt(base, "Ok", false, vec![named("T\u{202e}", vec![])]),
        ),
    ];
    cases.into_iter().map(|(n, w)| (n.to_owned(), w)).collect()
}

#[test]
fn mutants_match_rust() {
    let mut bad = 0;
    let mut refused = 0;
    for (name, world) in mutants() {
        let r = rust_text(&world);
        let v = vesper_text(&world, true);
        refused += r.starts_with("error: ") as usize;
        eprintln!(
            "lean head mutant {name}: {}",
            v.lines()
                .find(|l| l.starts_with("error") || l.contains("x0"))
                .unwrap_or("")
        );
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            eprintln!("lean head mutant {name} differs at {d}");
        }
    }
    eprintln!(
        "lean head: {} mutants, {refused} refused by Rust, {bad} differ",
        mutants().len()
    );
    assert!(refused >= 9);
    assert_eq!(bad, 0);
}

/// The head of every world whose full extraction succeeds is the first
/// declarations of the full module: removing the functions changes nothing
/// before them.
#[test]
fn head_is_a_prefix_of_the_full_module() {
    let mut full = 0;
    let (mut head_bytes, mut full_bytes) = (0, 0);
    for c in corpus() {
        let Ok(module) = vesper::lean::extract_module(&c.world) else {
            continue;
        };
        full += 1;
        let head = vesper::lean::extract_module(&head_world(&c.world)).unwrap();
        let (h, f) = (namespace(&head), namespace(&module));
        assert!(h.len() <= f.len(), "{}", c.name);
        assert_eq!(h, &f[..h.len()], "{}", c.name);
        head_bytes += head.render().unwrap().len();
        full_bytes += module.render().unwrap().len();
    }
    eprintln!(
        "lean head: {full} of {} worlds extract in full; the head is {head_bytes} of their {full_bytes} bytes",
        corpus().len()
    );
    assert!(full > 0);
}

/// ABBA timing of the head, Rust against Vesper
/// (`CHECK_HARNESS_SPEED=1 cargo test --release --test
/// self_host_lean_extract abba -- --ignored --nocapture`).
#[test]
#[ignore]
fn abba_head() {
    if !speed() {
        return;
    }
    let cases: Vec<(World, Vec<u8>)> = corpus()
        .iter()
        .map(|c| (head_world(&c.world), head_input(&c.world)))
        .collect();
    let e = entry(true);
    abba(
        "lean head",
        &mut || {
            time(|| {
                for (w, _) in &cases {
                    std::hint::black_box(vesper::lean::extract(w).ok());
                }
            })
        },
        &mut || {
            time(|| {
                for (_, input) in &cases {
                    std::hint::black_box(e.text(input));
                }
            })
        },
    );
}
