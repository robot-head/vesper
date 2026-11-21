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

use check_harness::*;
use std::sync::OnceLock;
use vesper::ast::{
    Adt, Effect, Effects, ExprKind, Field, Function, Mode, Param, Pass, Type, Variant,
};
use vesper::check::World;
use vesper::ir::{FunctionIr, Op, Program};
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

// ------------------------------------------------------------ part 2

/// A world and its lowered program with every closure the CFG extractor
/// would lift (`lean_ir::extract_closures`) added as its own function,
/// lowered the way Rust lowers it (in the world extended by the lifted
/// function), and the closure table: per closure instruction its owner,
/// destination, lifted name and captured slots.
struct Lifted {
    world: World,
    program: Program,
    closures: Vec<(String, u32, String, Vec<String>)>,
}

fn lift(world: &World) -> Lifted {
    let program = vesper::ir::lower_world(world);
    let mut out = Lifted {
        world: world.clone(),
        program: program.clone(),
        closures: vec![],
    };
    for f in program.functions.values() {
        lift_function(world, f, &mut out);
    }
    out
}

fn lift_function(world: &World, f: &FunctionIr, out: &mut Lifted) {
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        let Op::Closure { expr, aliases, .. } = &i.op else {
            continue;
        };
        let ExprKind::Closure { params, body, .. } = &expr.kind else {
            continue;
        };
        let Type::Function {
            params: typed,
            result,
            effects,
            ..
        } = &i.ty
        else {
            continue;
        };
        let mut free = vesper::check::free_names(body);
        for p in params {
            free.remove(&p.name);
        }
        let mut captured = vec![];
        let mut captures = vec![];
        for n in free {
            if let Some(slot) = aliases.get(&n) {
                let Some(ty) = f.locals.get(slot) else {
                    return;
                };
                captured.push(Param {
                    name: n,
                    ty: ty.clone(),
                    pass: Pass::Sink,
                    projections: vec![],
                    span: expr.span.clone(),
                });
                captures.push(slot.clone());
            }
        }
        let mut actual = params.clone();
        for (p, (pass, ty)) in actual.iter_mut().zip(typed) {
            p.pass = *pass;
            p.ty = ty.clone();
        }
        captured.extend(actual);
        let name = format!("$lambda_{}_{}", f.name, i.dest);
        let Some(generics) = world.functions.get(&f.name).map(|g| g.generics.clone()) else {
            return;
        };
        let function = Function {
            name: name.clone(),
            fip: false,
            params: captured,
            result: *result.clone(),
            effects: effects.clone(),
            generics,
            body: Some(*body.clone()),
            span: expr.span.clone(),
        };
        let mut extended = world.clone();
        extended.functions.insert(name.clone(), function.clone());
        let program = vesper::ir::lower_world(&extended);
        let Some(ir) = program.functions.get(&name) else {
            continue;
        };
        out.world.functions.insert(name.clone(), function);
        out.program.functions.insert(name.clone(), ir.clone());
        out.closures
            .push((f.name.clone(), i.dest, name.clone(), captures));
        lift_function(&extended, ir, out);
    }
}

/// The input of `extract_cfg_functions`: check-data's names, types and
/// world, the program section (tests/common/ir_words.rs) of the lifted
/// program, then
///
/// ```text
/// sigs:     n, per function of the lifted world (name order): name,
///           source (1, or 0 for a lifted closure), ng, ng x (name, row),
///           np, np x (pass, k, k x (len, len names), type), effectful
/// calls:    n, per call span of the program: start, end, k, k x (name, type)
/// closures: n, per closure: owner, dest, lifted name, nc, nc slot names
/// ```
///
/// `None` when the program has a literal the format cannot hold.
fn cfg_input(world: &World) -> Option<Vec<u8>> {
    let l = lift(world);
    if !ir_words::encodable(&l.world, &l.program) {
        return None;
    }
    let mut t = ir_words::Tables::of(&l.program);
    t.label_names();
    let mut tail = t.section();
    let enc = &mut t.enc;
    tail.push(l.world.functions.len() as u64);
    for f in l.world.functions.values() {
        tail.push(enc.name(&f.name));
        tail.push(world.functions.contains_key(&f.name) as u64);
        tail.push(f.generics.len() as u64);
        for g in &f.generics {
            tail.push(enc.name(&g.name));
            tail.push(g.row as u64);
        }
        tail.push(f.params.len() as u64);
        for p in &f.params {
            tail.push(check_words::pass_word(p.pass));
            tail.push(p.projections.len() as u64);
            for q in &p.projections {
                tail.push(q.len() as u64);
                for n in q {
                    tail.push(enc.name(n));
                }
            }
            tail.push(enc.ty(&p.ty));
        }
        tail.push((!f.effects.instances.is_empty() || f.effects.tail.is_some()) as u64);
    }
    let spans: std::collections::BTreeSet<(usize, usize)> = l
        .program
        .functions
        .values()
        .flat_map(|f| f.blocks.iter().flat_map(|b| &b.instructions))
        .filter(|i| matches!(i.op, Op::Call { .. }))
        .map(|i| (i.span.start, i.span.end))
        .collect();
    let calls: Vec<_> = spans
        .iter()
        .filter_map(|s| l.world.calls.get(s).map(|v| (s, v)))
        .collect();
    tail.push(calls.len() as u64);
    for ((a, b), vars) in calls {
        tail.extend([*a as u64, *b as u64, vars.len() as u64]);
        for (n, ty) in vars {
            tail.push(enc.name(n));
            tail.push(enc.ty(ty));
        }
    }
    tail.push(l.closures.len() as u64);
    for (owner, dest, name, captures) in &l.closures {
        tail.push(enc.name(owner));
        tail.push(*dest as u64);
        tail.push(enc.name(name));
        tail.push(captures.len() as u64);
        for c in captures {
            tail.push(enc.name(c));
        }
    }
    let w = enc.world(&l.world);
    Some(enc.finish(&w, &tail))
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
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
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
        std::panic::set_hook(quiet);
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
