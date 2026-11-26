//! Self-hosting SH5 lane 18 (#1341). Part 1: the head of the extracted Lean
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
//!
//! Part 2: the CFG extractor (`lean_ir::extract_function_nodes`), per
//! source function, against `lean::cfg_function_nodes` over the corpus and
//! the Vesper programs of the Lean tests ([`cfg_input`] is the input).

#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;
#[path = "common/lean_words.rs"]
mod lean_words;

use check_harness::*;
use lean_words::*;
use std::sync::OnceLock;
use vesper::ast::{Adt, Effect, Effects, Field, Mode, Pass, Type, Variant};
use vesper::check::World;
use vesper::ir::Program;
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
        if vesper_cfg_text(&c.world, false) != vesper_cfg_text(&c.world, true) {
            bad += 1;
            eprintln!(
                "{}: optimized and unoptimized CFG extraction differ",
                c.name
            );
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

/// Per source function with IR (name order), `fn <name>` and what the CFG
/// extractor gives: `none` (the structured extractor's function), the
/// error, or the definitions as a module without imports.
fn rust_cfg_text(world: &World) -> String {
    let mut out = String::new();
    for (name, r) in vesper::lean::cfg_function_nodes(world, &vesper::ir::lower_world(world)) {
        out.push_str(&format!("fn {name}\n"));
        match r {
            None => out.push_str("none\n"),
            Some(Err(e)) => out.push_str(&format!("error: {e}\n")),
            Some(Ok(declarations)) => match (Module {
                imports: vec![],
                declarations,
            })
            .render()
            {
                Ok(text) => out.push_str(&text),
                Err(e) => out.push_str(&format!("error: {e}\n")),
            },
        }
    }
    out
}

fn cfg_entry(optimize: bool) -> &'static Entry {
    static ON: OnceLock<Entry> = OnceLock::new();
    static OFF: OnceLock<Entry> = OnceLock::new();
    let cell = if optimize { &ON } else { &OFF };
    cell.get_or_init(|| package().entry("extract_cfg_functions", optimize))
}

/// Vesper's text for [`rust_cfg_text`], or `None` when the input format
/// cannot hold the program (a literal over 64 bits).
fn vesper_cfg_text(world: &World, optimize: bool) -> Option<String> {
    cfg_input(world).map(|input| cfg_entry(optimize).text(&input))
}

#[test]
fn cfg_functions_match_rust_on_the_corpus() {
    let mut skipped = 0;
    let (mut functions, mut taken, mut errors) = (0, 0, 0);
    let bad = differential(
        "lean cfg",
        |c| {
            let text = rust_cfg_text(&c.world);
            functions += text.lines().filter(|l| l.starts_with("fn ")).count();
            let none = text.lines().filter(|l| *l == "none").count();
            let refused = text.lines().filter(|l| l.starts_with("error: ")).count();
            taken += text.lines().filter(|l| l.starts_with("fn ")).count() - none;
            errors += refused;
            text
        },
        |c| match vesper_cfg_text(&c.world, true) {
            Some(text) => text,
            None => {
                skipped += 1;
                rust_cfg_text(&c.world)
            }
        },
    );
    eprintln!(
        "lean cfg: {functions} functions, {taken} taken by the CFG extractor, {errors} refused; {skipped} worlds not encodable"
    );
    assert_eq!(bad, 0);
}

/// The string literals of a Rust test file (plain and raw), unescaped.
fn string_literals(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i] == b'\'' {
            // a char literal such as '"' or '\\'
            if let Some(end) = src[i + 1..].find('\'').filter(|e| *e <= 4) {
                i += end + 2;
                continue;
            }
        }
        if b[i] == b'r' && (b.get(i + 1) == Some(&b'"') || b.get(i + 1) == Some(&b'#')) {
            let mut j = i + 1;
            let mut hashes = 0;
            while b.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if b.get(j) == Some(&b'"') {
                let close = format!("\"{}", "#".repeat(hashes));
                if let Some(end) = src[j + 1..].find(&close) {
                    out.push(src[j + 1..j + 1 + end].to_owned());
                    i = j + 1 + end + close.len();
                    continue;
                }
            }
        }
        if b[i] == b'"' {
            let mut text = String::new();
            let mut j = i + 1;
            let chars: Vec<char> = src[j..].chars().take(1 << 20).collect();
            let mut k = 0;
            let mut ok = false;
            while k < chars.len() {
                let c = chars[k];
                if c == '"' {
                    ok = true;
                    break;
                }
                if c == '\\' {
                    k += 1;
                    match chars.get(k) {
                        Some('n') => text.push('\n'),
                        Some('t') => text.push('\t'),
                        Some('0') => text.push('\0'),
                        Some('\\') => text.push('\\'),
                        Some('"') => text.push('"'),
                        Some('\'') => text.push('\''),
                        Some('\n') => {
                            while chars.get(k + 1).is_some_and(|c| c.is_whitespace()) {
                                k += 1;
                            }
                        }
                        Some(c) => {
                            text.push('\\');
                            text.push(*c);
                        }
                        None => {}
                    }
                } else {
                    text.push(c);
                }
                k += 1;
            }
            j += chars[..k.min(chars.len())]
                .iter()
                .map(|c| c.len_utf8())
                .sum::<usize>();
            if ok {
                out.push(text);
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// The Vesper programs in the Lean tests (tests/lean*.rs) and the tiled
/// sequence tests (tests/tiled.rs) that check without diagnostics: they
/// reach forms the corpus rarely has (closures, tiled operations, state
/// calls).
fn test_sources() -> &'static [(String, World)] {
    static SOURCES: OnceLock<Vec<(String, World)>> = OnceLock::new();
    SOURCES.get_or_init(|| {
        let mut files: Vec<_> = std::fs::read_dir(repo().join("tests"))
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    (n.starts_with("lean") || n == "tiled.rs") && n.ends_with(".rs")
                })
            })
            .collect();
        files.sort();
        let quiet = quiet_panics();
        let mut out = vec![];
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            for (k, text) in string_literals(&src).into_iter().enumerate() {
                if !text.contains("fn ") {
                    continue;
                }
                let r = std::panic::catch_unwind(|| {
                    let parsed = vesper::parser::parse(&text);
                    if !parsed.diagnostics.is_empty() {
                        return None;
                    }
                    let checked = vesper::check::check(&parsed.module);
                    checked.diagnostics.is_empty().then_some(checked.world)
                });
                if let Ok(Some(world)) = r {
                    let name = format!("{}#{k}", f.file_name().unwrap().to_string_lossy());
                    out.push((name, world));
                }
            }
        }
        drop(quiet);
        out
    })
}

/// A world with source function `from` renamed `to` (its IR follows).
fn renamed(world: &World, from: &str, to: &str) -> World {
    let mut w = world.clone();
    if let Some(mut f) = w.functions.remove(from) {
        f.name = to.into();
        w.functions.insert(to.into(), f);
    }
    w
}

/// Mutants for the refusals' order: names Lean cannot spell on functions
/// whose types it also refuses.
fn cfg_mutants() -> Vec<(String, World)> {
    let sources = [
        "fn wide() -> i128 { 1 }",
        "fn wide(x: i128) -> u8 { 1 }",
        "fn wide(x: u8) -> u8 { let y: u128 = 2; x }",
        "fn wide(inout x: u8, y: u8) -> i128 { x = y; 1 }",
        "fn plain(x: u8) -> u8 { x }",
        // Tiled sequence operations (the corpus has none on the CFG path).
        "tiled(8) copy struct P { hot m: u64, cold c: u8, }
         fn mk() -> TiledSeq[P] { TiledSeq::new[P]() }
         fn cap(n: u64) -> TiledSeq[P] { TiledSeq::with_capacity[P](n) }
         fn n(in s: TiledSeq[P]) -> u64 { s.len() }
         fn tiles(in s: TiledSeq[P]) -> u64 { s.tile_count() }
         fn put(inout s: TiledSeq[P], p: P) { s.push(p); }
         fn cs(inout s: TiledSeq[P]) { s.col_set[m](0, 5); }
         fn st(inout s: TiledSeq[P], p: P) { s.set(0, p); }
         fn cg(in s: TiledSeq[P]) -> u64 { s.col_get[m](0) }
         fn g(in s: TiledSeq[P]) -> P { s.get(0) }",
        // The structured extractor: sequence operations keep these off the CFG path.
        "tiled(8) copy struct P { hot m: u64, cold c: u8, }
         fn cs(inout s: TiledSeq[P], in v: ChunkSeq[u64]) { s.col_set[m](0, v.len()); }
         fn cg(in s: TiledSeq[P], in v: ChunkSeq[u64]) -> bool { s.col_get[m](0) == v.len() }",
        "copy struct Q { a: u64, b: u64, }
         fn rp(q: Q, in v: ChunkSeq[u64]) -> bool { let Q { a, b } = q; a == v.len() }",
        // A handler nested in an operation clause of the same operation:
        // the outer computation's `inc` is the outer clause's.
        "linear effect Inc { fn inc(inout x: i64); }
         fn main() -> i64 {
             let mut x = 40;
             handle Inc in { Inc::inc(inout x); x } with {
                 return n => n,
                 inc(n, k) => {
                     let mut z = 1;
                     let w = handle Inc in { Inc::inc(inout z); z } with {
                         return m => m,
                         inc(m, j) => { m = wrapping_add(m, 3); j(()) },
                     };
                     n = wrapping_add(n, w);
                     k(())
                 },
             }
         }",
        // Contracts: pure, stated precondition, Fail labels, recursion
        // (fuel), generics, inout state and refused shapes.
        "fn f(x: u64) -> u64 { x } fn requires_f(x: u64) -> bool { x > 1 } fn ensures_f(x: u64, r: u64) -> bool { r == x } fn main() -> u64 { 0 }",
        "fn f(x: u64) -> u64 eff [Fail[ArithOverflow]] { x + 1 } fn ensures_f(x: u64, r: u64) -> bool { r > x } fn main() -> u64 { 0 }",
        "fn f(n: u64) -> u64 eff [left: Fail[u64], right: Fail[bool]] { n } fn ensures_f(n: u64, r: u64) -> bool { r == n } fn main() -> u64 { 0 }",
        "fn down(n: u64) -> u64 { if n == 0 { 0 } else { down(wrapping_sub(n, 1)) } } fn ensures_down(n: u64, r: u64) -> bool { r == 0 } fn main() -> u64 { down(3) }",
        "fn ev(n: u64) -> bool { if n == 0 { true } else { od(wrapping_sub(n, 1)) } } fn od(n: u64) -> bool { if n == 0 { false } else { ev(wrapping_sub(n, 1)) } } fn ensures_ev(n: u64, r: bool) -> bool { true } fn main() -> u64 { 0 }",
        "fn pick[T: mod copy](x: T, y: T) -> T { x } fn ensures_pick[U: mod copy](x: U, y: U, r: U) -> bool { true } fn main() -> u64 { 0 }",
        "fn replace[T](inout x: u64, value: u64) -> u64 { let old = x; x = value; old } fn ensures_replace[U](before: u64, value: u64, result: u64, after: u64) -> bool { result == before && after == value } fn main() -> u64 { 0 }",
        "fn bad_reset(inout x: u64) { x = 0; } fn ensures_bad_reset(before: u64, result: (), after: u64) -> bool { after == before } fn main() -> u64 { 42 }",
        "fn f(x: u64) -> u64 { x } fn ensures_f(x: u64) -> bool { true } fn main() -> u64 { 0 }",
        "fn f(x: u64) -> u64 { x } fn ensures_f(x: u64, r: u64) -> u64 { r } fn main() -> u64 { 0 }",
        "fn f(x: u64) -> u64 { x } fn ensures_f(x: u64, r: u64) -> bool eff [Fail[ArithOverflow]] { r + 1 > x } fn main() -> u64 { 0 }",
        "fn f[T](x: T) -> bool { true } fn requires_f(x: u64) -> bool { true } fn ensures_f[U](x: U, result: bool) -> bool { true } fn main() -> u64 { 0 }",
        "fn apply(sink f: fn(u64) -> u64, x: u64) -> u64 { f(x) } fn spin(n: u64) -> u64 { if n == 0 { 0 } else { apply(spin, wrapping_sub(n, 1)) } } fn ensures_spin(n: u64, r: u64) -> bool { true } fn main() -> u64 { 0 }",
    ];
    let mut out = vec![];
    for src in sources {
        let parsed = vesper::parser::parse(src);
        assert!(parsed.diagnostics.is_empty(), "{src}");
        let checked = vesper::check::check(&parsed.module);
        assert!(
            checked.diagnostics.is_empty(),
            "{src}: {:?}",
            checked.diagnostics
        );
        let name = checked.world.functions.keys().next().unwrap().clone();
        out.push((src.to_owned(), checked.world.clone()));
        for bad in ["«", "\u{202e}"] {
            out.push((
                format!("{src} renamed with {bad:?}"),
                renamed(&checked.world, &name, &format!("{name}{bad}")),
            ));
        }
    }
    out
}

#[test]
fn cfg_functions_match_rust_on_test_sources() {
    let mut bad = 0;
    let mut cases = test_sources().to_vec();
    cases.extend(cfg_mutants());
    let (mut functions, mut taken) = (0, 0);
    let (mut lambdas, mut tiled, mut names) = (0, 0, 0);
    for (name, world) in &cases {
        let r = rust_cfg_text(world);
        lambdas += r.matches("partial def «$lambda").count();
        tiled += r.matches("«Array».«emptyWithCapacity»").count()
            + r.matches("«Array».«setIfInBounds»").count()
            + r.matches("«Array».«push»").count();
        names += r.matches("error: unsupported Lean name").count();
        functions += r.lines().filter(|l| l.starts_with("fn ")).count();
        taken += r.lines().filter(|l| l.starts_with("fn ")).count()
            - r.lines().filter(|l| *l == "none").count();
        let Some(v) = vesper_cfg_text(world, true) else {
            continue;
        };
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if bad <= 8 {
                eprintln!("lean cfg {name} differs at {d}");
            }
        }
    }
    eprintln!(
        "lean cfg: {} test programs and mutants, {functions} functions, {taken} taken ({lambdas} lifted closure definitions, {tiled} tiled operations, {names} name refusals), {bad} differ",
        cases.len()
    );
    assert!(names >= 4 && tiled >= 5 && lambdas > 0);
    assert_eq!(bad, 0);
}

/// ABBA timing of the CFG extractor over the corpus's lowered programs,
/// Rust against Vesper (`CHECK_HARNESS_SPEED=1 cargo test --release --test
/// self_host_lean_extract abba_cfg -- --ignored --nocapture`).
#[test]
#[ignore]
fn abba_cfg() {
    if !speed() {
        return;
    }
    let cases: Vec<(World, Program, Vec<u8>)> = corpus()
        .iter()
        .filter_map(|c| {
            let input = cfg_input(&c.world)?;
            Some((c.world.clone(), vesper::ir::lower_world(&c.world), input))
        })
        .collect();
    let e = cfg_entry(true);
    abba(
        "lean cfg",
        &mut || {
            time(|| {
                for (w, p, _) in &cases {
                    std::hint::black_box(vesper::lean::cfg_function_nodes(w, p));
                }
            })
        },
        &mut || {
            time(|| {
                for (_, _, input) in &cases {
                    std::hint::black_box(e.text(input));
                }
            })
        },
    );
}

/// Per source function with IR (name order), `fn <name>` and what the
/// structured extractor gives over `Id`: `none` (the CFG extractor's, or a
/// function that needs the ambient monad), the error, or the definition as
/// a module without imports.
fn rust_structured_text(world: &World) -> String {
    let mut out = String::new();
    for (name, r) in vesper::lean::structured_function_nodes(world, &vesper::ir::lower_world(world))
    {
        out.push_str(&format!("fn {name}\n"));
        match r {
            None => out.push_str("none\n"),
            Some(Err(e)) => out.push_str(&format!("error: {e}\n")),
            Some(Ok(d)) => match (Module {
                imports: vec![],
                declarations: vec![Declaration::Definition(d)],
            })
            .render()
            {
                Ok(text) => out.push_str(&text),
                Err(e) => out.push_str(&format!("error: {e}\n")),
            },
        }
    }
    out
}

fn structured_entry(optimize: bool) -> &'static Entry {
    static ON: OnceLock<Entry> = OnceLock::new();
    static OFF: OnceLock<Entry> = OnceLock::new();
    let cell = if optimize { &ON } else { &OFF };
    cell.get_or_init(|| package().entry("extract_structured_functions", optimize))
}

/// Vesper's text for [`rust_structured_text`] (with `skip` for what the
/// port does not cover yet), or `None` when the input cannot hold the
/// program.
fn vesper_structured_text(world: &World, optimize: bool) -> Option<String> {
    cfg_input_with(world, true).map(|input| structured_entry(optimize).text(&input))
}

/// The per-function chunks of a `fn <name>` text.
fn chunks(text: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = vec![];
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with("fn ") {
            starts.push(at);
        }
        at += line.len();
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &s)| &text[s..starts.get(i + 1).copied().unwrap_or(text.len())])
        .collect()
}

/// Vesper's text with Rust's chunk for each `skip`, and the skip count.
fn fill_skips(rust: &str, vesper: &str) -> (String, usize) {
    let (r, v) = (chunks(rust), chunks(vesper));
    if r.len() != v.len() {
        return (vesper.to_string(), 0);
    }
    let mut out = String::new();
    let mut skips = 0;
    for (r, v) in r.iter().zip(&v) {
        if v.lines().nth(1) == Some("skip") {
            skips += 1;
            out.push_str(r);
        } else {
            out.push_str(v);
        }
    }
    (out, skips)
}

#[test]
fn structured_functions_match_rust_on_the_corpus() {
    let (mut functions, mut generated, mut errors, mut skips, mut unencodable) = (0, 0, 0, 0, 0);
    let bad = differential(
        "lean structured",
        |c| {
            let text = rust_structured_text(&c.world);
            for chunk in chunks(&text) {
                functions += 1;
                match chunk.lines().nth(1) {
                    Some("none") => {}
                    Some(l) if l.starts_with("error: ") => errors += 1,
                    _ => generated += 1,
                }
            }
            text
        },
        |c| {
            let rust = rust_structured_text(&c.world);
            match vesper_structured_text(&c.world, true) {
                Some(text) => {
                    let (text, s) = fill_skips(&rust, &text);
                    skips += s;
                    text
                }
                None => {
                    unencodable += 1;
                    rust
                }
            }
        },
    );
    eprintln!(
        "lean structured: {functions} functions, {generated} generated and {errors} refused by the structured extractor; {skips} skipped by the port; {unencodable} worlds not encodable"
    );
    assert_eq!(bad, 0);
}

#[test]
fn structured_functions_match_rust_on_test_sources() {
    let mut bad = 0;
    let mut cases = test_sources().to_vec();
    cases.extend(cfg_mutants());
    let (mut generated, mut errors, mut skips) = (0, 0, 0);
    for (name, world) in &cases {
        let r = rust_structured_text(world);
        for chunk in chunks(&r) {
            match chunk.lines().nth(1) {
                Some("none") => {}
                Some(l) if l.starts_with("error: ") => errors += 1,
                _ => generated += 1,
            }
        }
        let Some(v) = vesper_structured_text(world, true) else {
            continue;
        };
        let (v, s) = fill_skips(&r, &v);
        skips += s;
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if bad <= 8 {
                eprintln!("lean structured {name} differs at {d}");
            }
        }
    }
    eprintln!(
        "lean structured: {} test programs and mutants, {generated} generated, {errors} refused, {skips} skipped by the port, {bad} differ",
        cases.len()
    );
    assert!(generated > 0);
    assert_eq!(bad, 0);
}

/// ABBA timing of the structured extractor (with the CFG gate both sides
/// run first) over the corpus (`CHECK_HARNESS_SPEED=1 cargo test --release
/// --test self_host_lean_extract abba_structured -- --ignored --nocapture`).
#[test]
#[ignore]
fn abba_structured() {
    if !speed() {
        return;
    }
    let cases: Vec<(World, Program, Vec<u8>)> = corpus()
        .iter()
        .filter_map(|c| {
            let input = cfg_input_with(&c.world, true)?;
            Some((c.world.clone(), vesper::ir::lower_world(&c.world), input))
        })
        .collect();
    let e = structured_entry(true);
    abba(
        "lean structured",
        &mut || {
            time(|| {
                for (w, p, _) in &cases {
                    std::hint::black_box(vesper::lean::structured_function_nodes(w, p));
                }
            })
        },
        &mut || {
            time(|| {
                for (_, _, input) in &cases {
                    std::hint::black_box(e.text(input));
                }
            })
        },
    );
}

// ----- part 3c: the whole module ------------------------------------------------

/// The functions in the `-- contracts` section of a module text.
fn contract_count(text: &str) -> usize {
    text.split_once("-- contracts\n")
        .map_or(0, |(_, rest)| rest.lines().count())
}

/// Whether the world states a contract (`ensures_f` for a function `f`).
fn has_contract(world: &World) -> bool {
    world.functions.keys().any(|n| {
        n.strip_prefix("ensures_")
            .is_some_and(|f| world.functions.contains_key(f))
    })
}

/// `lean::extract_report`: the module, `-- proof` and the proof module,
/// `-- omitted` and the omitted executable definitions, then `-- totality`
/// with a `name: total|fuel|partial: reason` line per function; or
/// `error: <message>`.
fn rust_module_text(world: &World) -> String {
    let report = match vesper::lean::extract_report(world) {
        Ok(r) => r,
        Err(e) => return format!("error: {e}\n"),
    };
    let mut out = match report.module.render() {
        Ok(text) => text,
        Err(e) => return format!("error: {e}\n"),
    };
    out.push_str("-- proof\n");
    match report.proof_module.render() {
        Ok(text) => out.push_str(&text),
        Err(e) => return format!("{out}error: {e}\n"),
    }
    out.push_str("-- omitted\n");
    for n in &report.omitted_executable_definitions {
        out.push_str(&format!("{n}\n"));
    }
    out.push_str("-- totality\n");
    for (n, t) in &report.totality {
        match t {
            vesper::lean::Totality::Total => out.push_str(&format!("{n}: total\n")),
            vesper::lean::Totality::Fuel => out.push_str(&format!("{n}: fuel\n")),
            vesper::lean::Totality::Partial(r) => out.push_str(&format!("{n}: partial: {r}\n")),
        }
    }
    out.push_str("-- contracts\n");
    for n in &report.contracts {
        out.push_str(&format!("{n}\n"));
    }
    out
}

fn module_entry(optimize: bool) -> &'static Entry {
    static ON: OnceLock<Entry> = OnceLock::new();
    static OFF: OnceLock<Entry> = OnceLock::new();
    let cell = if optimize { &ON } else { &OFF };
    cell.get_or_init(|| package().entry("extract_module", optimize))
}

/// Vesper's [`rust_module_text`], or `None` when the input cannot hold the
/// program.
fn vesper_module_text(world: &World, optimize: bool) -> Option<String> {
    cfg_input_full(world, true, true).map(|input| module_entry(optimize).text(&input))
}

#[test]
fn modules_match_rust_on_the_corpus() {
    let (mut worlds, mut errors, mut contracts, mut unencodable) = (0, 0, 0, 0);
    let (mut fuel, mut partial, mut stated) = (0, 0, 0);
    let bad = differential(
        "lean module",
        |c| {
            let text = rust_module_text(&c.world);
            worlds += 1;
            if text.starts_with("error: ") {
                errors += 1;
            }
            fuel += text.lines().filter(|l| l.ends_with(": fuel")).count();
            stated += contract_count(&text);
            partial += text.lines().filter(|l| l.contains(": partial: ")).count();
            text
        },
        |c| {
            if has_contract(&c.world) {
                contracts += 1;
            }
            match vesper_module_text(&c.world, true) {
                Some(text) => text,
                None => {
                    unencodable += 1;
                    rust_module_text(&c.world)
                }
            }
        },
    );
    eprintln!(
        "lean module: {worlds} worlds ({contracts} with contracts, {stated} contracts stated), {errors} refused, {fuel} fuel companions, {partial} partial functions; {unencodable} not encodable take Rust's text"
    );
    assert_eq!(bad, 0);
}

#[test]
fn modules_match_rust_on_test_sources() {
    let mut bad = 0;
    let mut cases = test_sources().to_vec();
    cases.extend(cfg_mutants());
    let (mut compared, mut errors, mut contracts, mut stated) = (0, 0, 0, 0);
    for (name, world) in &cases {
        if has_contract(world) {
            contracts += 1;
        }
        let r = rust_module_text(world);
        let Some(v) = vesper_module_text(world, true) else {
            continue;
        };
        compared += 1;
        if r.starts_with("error: ") {
            errors += 1;
        }
        stated += contract_count(&r);
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if bad <= 8 {
                eprintln!("lean module {name} differs at {d}");
            }
        }
    }
    eprintln!(
        "lean module: {compared} test programs and mutants compared ({contracts} with contracts, {stated} contracts stated, {errors} refused), {bad} differ"
    );
    assert!(compared > 0);
    assert_eq!(bad, 0);
}

/// ABBA timing of the whole module over the corpus (`CHECK_HARNESS_SPEED=1
/// cargo test --release --test self_host_lean_extract abba_module --
/// --ignored --nocapture`). Rust lowers the world and each lifted closure
/// inside `extract_report`, so the Vesper side pays for [`lift`] too. The
/// word encoding is the test harness's (a Vesper front end hands the words
/// over directly), so it is timed and printed apart.
#[test]
#[ignore]
fn abba_module() {
    if !speed() {
        return;
    }
    let worlds: Vec<World> = corpus()
        .iter()
        .filter(|c| cfg_input_full(&c.world, true, true).is_some())
        .map(|c| c.world.clone())
        .collect();
    let e = module_entry(true);
    let inputs: Vec<Vec<u8>> = worlds
        .iter()
        .map(|w| cfg_input_full(w, true, true).unwrap())
        .collect();
    let encode = time(|| {
        for w in &worlds {
            std::hint::black_box(cfg_input_full(w, true, true));
        }
    });
    let run = time(|| {
        for input in &inputs {
            std::hint::black_box(e.text(input));
        }
    });
    let lifting = time(|| {
        for w in &worlds {
            std::hint::black_box(lift(w));
        }
    });
    eprintln!(
        "lean module: encoding {encode:?} (lowering and lifting {lifting:?}), extraction {run:?}"
    );
    abba(
        "lean module",
        &mut || {
            time(|| {
                for w in &worlds {
                    std::hint::black_box(rust_module_text(w));
                }
            })
        },
        &mut || {
            time(|| {
                for (w, input) in worlds.iter().zip(&inputs) {
                    std::hint::black_box(lift(w));
                    std::hint::black_box(e.text(input));
                }
            })
        },
    );
}

/// Literals past `i128::MAX` (`Literal::WideInt`): every path agrees with
/// Rust, which refuses `u128` today. The Vesper emitters print them as
/// natural numbers and refuse them as patterns, as Rust does, for when
/// `u128` is modeled.
#[test]
fn wide_literals_match_rust() {
    const WIDE: &str = "170141183460469231731687303715884105728";
    let sources = [
        format!("fn top() -> u128 {{ {WIDE} }}"),
        format!("fn above(x: u128) -> bool {{ x > {WIDE} }}"),
        format!(
            "fn once(n: u64) -> u128 {{ let mut x: u128 = 0; for i in 0..n {{ x = {WIDE}; }} x }}"
        ),
        format!("fn is_top(x: u128) -> bool {{ match x {{ {WIDE} => true, _ => false }} }}"),
        format!(
            "fn loop_top(n: u64) -> bool {{ let mut i: u128 = 0; for k in 0..n {{ i = {WIDE}; }} match i {{ {WIDE} => true, _ => false }} }}"
        ),
    ];
    let mut compared = 0;
    for src in &sources {
        let parsed = vesper::parser::parse(src);
        assert!(
            parsed.diagnostics.is_empty(),
            "{src}: {:?}",
            parsed.diagnostics
        );
        let checked = vesper::check::check(&parsed.module);
        assert!(
            checked.diagnostics.is_empty(),
            "{src}: {:?}",
            checked.diagnostics
        );
        let world = &checked.world;
        let pairs = [
            (rust_cfg_text(world), vesper_cfg_text(world, true)),
            (
                rust_structured_text(world),
                vesper_structured_text(world, true),
            ),
            (rust_module_text(world), vesper_module_text(world, true)),
        ];
        for (k, (rust, vesper)) in pairs.into_iter().enumerate() {
            let Some(vesper) = vesper else { continue };
            let vesper = if k == 1 {
                fill_skips(&rust, &vesper).0
            } else {
                vesper
            };
            compared += 1;
            assert_eq!(rust, vesper, "{src} (path {k})");
        }
    }
    assert!(compared >= sources.len(), "only {compared} texts compared");
}
