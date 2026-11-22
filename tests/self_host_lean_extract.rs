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
    cfg_input_with(world, false)
}

/// [`cfg_input`], then with `structured` the AST section ([`Ast`]).
fn cfg_input_with(world: &World, structured: bool) -> Option<Vec<u8>> {
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
    if structured {
        let mut a = Ast {
            enc,
            world,
            pats: vec![],
            pat_count: 0,
            exprs: vec![],
            expr_count: 0,
        };
        tail.extend(a.section());
    }
    let enc = &mut t.enc;
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
        // The structured extractor: sequence operations keep these off the CFG path.
        "tiled(8) copy struct P { hot m: u64, cold c: u8, }
         fn cs(inout s: TiledSeq[P], in v: ChunkSeq[u64]) { s.col_set[m](0, v.len()); }
         fn cg(in s: TiledSeq[P], in v: ChunkSeq[u64]) -> bool { s.col_get[m](0) == v.len() }",
        "copy struct Q { a: u64, b: u64, }
         fn rp(q: Q, in v: ChunkSeq[u64]) -> bool { let Q { a, b } = q; a == v.len() }",
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

// ---------------------------------------------------------------------------
// Part 3: the structured extractor (`Generator::function` over `Id`).

const NONE: u64 = u64::MAX;

// Expression kinds (`X_*` in packages/vesper-lean-extract/src/ast.ves).
const X_LITERAL: u64 = 1;
const X_NAME: u64 = 2;
const X_TUPLE: u64 = 3;
const X_STRUCT: u64 = 4;
const X_FIELD: u64 = 5;
const X_INDEX: u64 = 6;
const X_UNARY: u64 = 7;
const X_BINARY: u64 = 8;
const X_ASSIGN: u64 = 9;
const X_BLOCK: u64 = 10;
const X_CALL: u64 = 11;
const X_TRY: u64 = 12;
const X_IF: u64 = 13;
const X_MATCH: u64 = 14;
const X_LOOP: u64 = 15;
const X_CLOSURE: u64 = 21;
const X_HANDLE: u64 = 22;

/// The refusal `seq_lean::INOUT_SLICE`.
const INOUT_SLICE: &str =
    "Lean extraction does not model inout Slice arguments: they write through to their sequence";

/// The AST section of the structured extractor's input, after
/// [`cfg_input`]'s sections:
///
/// ```text
/// fip:       1 when the built-in Box is active (`fip::active`)
/// functions: n, per source function (name order): name, body or NONE,
///            np, np x (name, pass, type), result type, polymorphic
/// patterns:  n, n x (kind, start, end, box, payload), children first:
///            0 wild | 1 bind: name | 2 literal: literal |
///            3 tuple: n, ids | 4 constructor: name, named, n, (field, id)
/// exprs:     n, n x (kind, start, end, type, payload), children first
/// literal:   0 int: low, high | 1 float: bits, 0 | 2 bool: v, 0 |
///            3 string: name, 0
/// ```
///
/// An expression's type is `world.types` at its span after
/// `numeric::default_type`, or NONE. The payloads are in [`Ast::expr`].
/// What stays on the Rust side are the classification tables: which call
/// or index `seq::resolve` routes to a sequence operation (and the
/// refusals of `seq_op`), what `tiled::resolve_ir_call` resolves, the
/// numeric intrinsic name tables, `fip::is_box_pattern` and
/// `check::free_names` of a closure body.
struct Ast<'a> {
    enc: &'a mut check_words::Enc,
    world: &'a World,
    pats: Vec<u64>,
    pat_count: u64,
    exprs: Vec<u64>,
    expr_count: u64,
}

// Sequence operation codes (`Q_*` in ast.ves).
fn seq_code(op: vesper::seq::SeqOp) -> u64 {
    use vesper::seq::SeqOp as Q;
    match op {
        Q::New => 1,
        Q::WithCapacity => 2,
        Q::Len | Q::SliceLen => 3,
        Q::Push => 4,
        Q::Pop => 5,
        Q::Get | Q::SliceGet => 6,
        Q::Set => 7,
        Q::TryGet => 8,
        Q::Last => 9,
        Q::TrySet => 10,
        Q::Clear => 11,
        Q::Truncate => 12,
        Q::Reserve => 13,
        Q::Extend => 14,
        Q::Finish => 15,
        Q::AsSlice => 16,
        Q::SubSlice => 17,
        _ => 0,
    }
}

impl Ast<'_> {
    fn name(&mut self, s: &str) -> u64 {
        self.enc.name(s)
    }

    fn ty(&mut self, t: &Type) -> u64 {
        self.enc.ty(t)
    }

    fn type_of(&self, e: &vesper::ast::Expr) -> Option<Type> {
        self.world
            .types
            .get(&(e.span.start, e.span.end))
            .map(vesper::numeric::default_type)
    }

    fn literal(&mut self, l: &vesper::ast::Literal, out: &mut Vec<u64>) {
        use vesper::ast::Literal;
        match l {
            Literal::Int(n) => out.extend([0, *n as u64, (*n >> 64) as u64]),
            // A literal above i128::MAX (#1244): its bits, as an Int's.
            Literal::WideInt(n) => out.extend([0, *n as u64, (*n >> 64) as u64]),
            Literal::Float(x) => out.extend([1, x.to_bits(), 0]),
            Literal::Bool(b) => out.extend([2, *b as u64, 0]),
            Literal::String(s) => {
                let n = self.name(s);
                out.extend([3, n, 0]);
            }
        }
    }

    fn pattern(&mut self, p: &vesper::ast::Pattern) -> u64 {
        use vesper::ast::PatternKind;
        let mut w = vec![];
        let head = |kind: u64, w: &mut Vec<u64>| {
            w.extend([
                kind,
                p.span.start as u64,
                p.span.end as u64,
                vesper::fip::is_box_pattern(self.world, p) as u64,
            ])
        };
        match &p.kind {
            PatternKind::Wild => head(0, &mut w),
            PatternKind::Bind(n) => {
                head(1, &mut w);
                w.push(self.name(n));
            }
            PatternKind::Literal(l) => {
                head(2, &mut w);
                self.literal(l, &mut w);
            }
            PatternKind::Tuple(ps) => {
                let kids: Vec<u64> = ps.iter().map(|c| self.pattern(c)).collect();
                head(3, &mut w);
                w.push(kids.len() as u64);
                w.extend(kids);
            }
            PatternKind::Constructor {
                name,
                fields,
                named,
            } => {
                let kids: Vec<(u64, u64)> = fields
                    .iter()
                    .map(|(f, c)| {
                        let c = self.pattern(c);
                        (self.name(f), c)
                    })
                    .collect();
                head(4, &mut w);
                w.push(self.name(name));
                w.push(*named as u64);
                w.push(kids.len() as u64);
                for (f, c) in kids {
                    w.extend([f, c]);
                }
            }
        }
        self.pats.extend(w);
        self.pat_count += 1;
        self.pat_count - 1
    }

    fn opt(&mut self, e: &Option<Box<vesper::ast::Expr>>) -> u64 {
        match e {
            Some(e) => self.expr(e),
            None => NONE,
        }
    }

    /// A refusal or operation of `seq_op`: 0 | 1 code | 2 message.
    fn seq_route(
        &mut self,
        op: vesper::seq::SeqOp,
        first: Option<&vesper::ast::Expr>,
        out: &mut Vec<u64>,
    ) {
        use vesper::seq::SeqOp as Q;
        let refusal = match op {
            Q::Str(s) => Some(vesper::strings::lean_refusal(s)),
            Q::CopyFrom { .. }
            | Q::CopyWithin { .. }
            | Q::Fill
            | Q::Resize
            | Q::FillRange { .. }
            | Q::ExtendFromWithin { .. }
            | Q::ExtendReversed
            | Q::Wide(_)
            | Q::Swap
            | Q::Replace
            | Q::Vector(_) => Some(format!(
                "the Lean model of sequences has no ChunkSeq::{} yet",
                op.name()
            )),
            _ if op.byte_scan() => Some(format!(
                "the Lean model of sequences has no ChunkSeq::{} yet",
                op.name()
            )),
            Q::Hoisted(_) | Q::BoundsGuard { .. } => Some(format!(
                "the Lean model has no native-only ChunkSeq::{}",
                op.name()
            )),
            Q::SubSlice
                if first
                    .and_then(|r| self.type_of(r))
                    .is_some_and(|t| vesper::strings::is_text(&t)) =>
            {
                Some(vesper::strings::lean_refusal(
                    vesper::strings::StrOp::SubStr,
                ))
            }
            Q::SliceSet => Some(INOUT_SLICE.into()),
            Q::TrySet
                if first
                    .and_then(|r| self.type_of(r))
                    .is_some_and(|t| vesper::seq::is_slice_type(&t)) =>
            {
                Some(INOUT_SLICE.into())
            }
            _ => None,
        };
        match refusal {
            Some(m) => {
                let m = self.name(&m);
                out.extend([2, m]);
            }
            None => {
                let c = seq_code(op);
                assert!(c != 0, "unrouted sequence operation {op:?}");
                out.extend([1, c]);
            }
        }
    }

    /// The tiled route of a call: op (0 none), width or field name,
    /// skip-first, place operand (0 none, 1 receiver, 2 first argument).
    fn tiled_route(
        &mut self,
        e: &vesper::ast::Expr,
        callee: &vesper::ast::Expr,
        types: &[Type],
        args: &[vesper::ast::Arg],
        out: &mut Vec<u64>,
    ) {
        use vesper::ast::ExprKind;
        use vesper::tiled::TiledOp as T;
        let route = match &callee.kind {
            ExprKind::Name(n) => Some((n.as_str(), None)),
            ExprKind::Field(base, method)
                if self
                    .type_of(base)
                    .as_ref()
                    .is_some_and(vesper::tiled::is_seq_type) =>
            {
                Some((method.as_str(), Some(base.as_ref())))
            }
            _ => None,
        }
        .and_then(|(name, receiver)| {
            vesper::lean::tiled_route(self.world, name, receiver, types, args, e)
                .map(|op| (op, receiver.is_some()))
        });
        let Some((op, receiver)) = route else {
            out.extend([0, 0, 0, 0]);
            return;
        };
        let (code, extra) = match &op {
            T::New { .. } => (1, 0),
            T::WithCapacity { .. } => (2, 0),
            T::WithRegion { .. } => (3, 0),
            T::Len => (4, 0),
            T::TileCount { tile_width } => (5, u64::from(*tile_width)),
            T::Get { .. } => (6, 0),
            T::ColGet { field, .. } => (7, self.name(field)),
            T::Push { .. } => (8, 0),
            T::Set { .. } => (9, 0),
            T::ColSet { field, .. } => (10, self.name(field)),
            T::LoadTileVec { .. } => (11, 0),
        };
        let creates = matches!(
            op,
            T::New { .. } | T::WithCapacity { .. } | T::WithRegion { .. }
        );
        let place = if receiver {
            1
        } else if !creates && !args.is_empty() {
            2
        } else {
            0
        };
        out.extend([
            code,
            extra,
            matches!(op, T::WithRegion { .. }) as u64,
            place,
        ]);
    }

    /// The name class of a plain callee: float refusal (name or NONE),
    /// integer intrinsic, wrapping operator (name or NONE), Box::new.
    fn name_class(&mut self, n: &str, out: &mut Vec<u64>) {
        let float = vesper::float_math::lean_refusal(n).map_or(NONE, |m| self.name(&m));
        let int = (vesper::numeric::conversion(n).is_some()
            || vesper::numeric::rotate_intrinsic(n).is_some()) as u64;
        let wrapping = vesper::numeric::wrapping_intrinsic(n).map_or(NONE, |op| self.name(op));
        let boxed = (n == vesper::fip::BOX_NEW || n == vesper::fip::BOX_NEW_IN_CALLER) as u64;
        out.extend([float, int, wrapping, boxed]);
    }

    /// An expression row (children first); returns its index. Payloads:
    ///
    /// - literal: literal; name: name; tuple: n, ids;
    /// - struct: name, n, n x (field, id); field: base, name;
    /// - index: base, index, route (0 none, 1 element, 3 interval, 4
    ///   refusal: message);
    /// - unary: op, operand; binary: op, lhs, rhs; assign: op, place, value;
    /// - block: n, n x statement (0 expr: id | 1 let: pattern, type or
    ///   NONE, fallback type or NONE, value), tail or NONE;
    /// - call: callee, n, n x (pass, id), m, m types, sequence route (kind,
    ///   word), tiled route (4 words), name class (4 words);
    /// - try: operand; if: condition, then, else or NONE;
    /// - match: value, n, n x (pattern, body);
    /// - loops, ranges, break, continue and return (kind 15): nothing;
    /// - closure: arrow type or NONE, n, n param names, body, k, k free
    ///   names (`check::free_names` without the parameters);
    /// - handle: nothing.
    fn expr(&mut self, e: &vesper::ast::Expr) -> u64 {
        use vesper::ast::{ExprKind, Statement};
        let mut w: Vec<u64> = vec![];
        let kind = match &e.kind {
            ExprKind::Literal(l) => {
                self.literal(l, &mut w);
                X_LITERAL
            }
            ExprKind::Name(n) => {
                w.push(self.name(n));
                X_NAME
            }
            ExprKind::Tuple(es) => {
                let kids: Vec<u64> = es.iter().map(|c| self.expr(c)).collect();
                w.push(kids.len() as u64);
                w.extend(kids);
                X_TUPLE
            }
            ExprKind::Struct { name, fields } => {
                let kids: Vec<(u64, u64)> = fields
                    .iter()
                    .map(|(f, c)| {
                        let c = self.expr(c);
                        (self.name(f), c)
                    })
                    .collect();
                w.push(self.name(name));
                w.push(kids.len() as u64);
                for (f, c) in kids {
                    w.extend([f, c]);
                }
                X_STRUCT
            }
            ExprKind::Field(base, f) => {
                let b = self.expr(base);
                w.extend([b, self.name(f)]);
                X_FIELD
            }
            ExprKind::Index(base, index) => {
                let b = self.expr(base);
                let i = self.expr(index);
                w.extend([b, i]);
                if vesper::seq::interval(e).is_some() {
                    let mut r = vec![];
                    self.seq_route(vesper::seq::SeqOp::SubSlice, Some(base), &mut r);
                    if r[0] == 1 {
                        w.push(3);
                    } else {
                        w.extend([4, r[1]]);
                    }
                } else {
                    match self.type_of(base) {
                        Some(t)
                            if vesper::seq::is_seq_type(&t) || vesper::seq::is_slice_type(&t) =>
                        {
                            w.push(1)
                        }
                        _ => w.push(0),
                    }
                }
                X_INDEX
            }
            ExprKind::Unary(op, v) => {
                let v = self.expr(v);
                w.extend([self.name(op), v]);
                X_UNARY
            }
            ExprKind::Binary(op, a, b) => {
                let a = self.expr(a);
                let b = self.expr(b);
                w.extend([self.name(op), a, b]);
                X_BINARY
            }
            ExprKind::Assign(op, p, v) => {
                let p = self.expr(p);
                let v = self.expr(v);
                w.extend([self.name(op), p, v]);
                X_ASSIGN
            }
            ExprKind::Block { statements, tail } => {
                let mut ss = vec![];
                for s in statements {
                    match s {
                        Statement::Expr(x) => {
                            let x = self.expr(x);
                            ss.extend([0, x]);
                        }
                        Statement::Let { pat, ty, value, .. } => {
                            let p = self.pattern(pat);
                            let v = self.expr(value);
                            let t = if *ty == Type::Infer {
                                NONE
                            } else {
                                self.ty(ty)
                            };
                            let fallback = match &value.kind {
                                ExprKind::Struct { name, .. } => self.ty(&Type::named(name)),
                                ExprKind::Literal(l) => self.ty(&vesper::check::literal_type(l)),
                                _ => NONE,
                            };
                            ss.extend([1, p, t, fallback, v]);
                        }
                    }
                }
                let t = self.opt(tail);
                w.push(statements.len() as u64);
                w.extend(ss);
                w.push(t);
                X_BLOCK
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                let c = self.expr(callee);
                let kids: Vec<(u64, u64)> = args
                    .iter()
                    .map(|a| (check_words::pass_word(a.pass), self.expr(&a.expr)))
                    .collect();
                let ts: Vec<u64> = types.iter().map(|t| self.enc.ty(t)).collect();
                w.extend([c, kids.len() as u64]);
                for (p, x) in kids {
                    w.extend([p, x]);
                }
                w.push(ts.len() as u64);
                w.extend(ts);
                // `seq_expr`'s call branch (the scope test is the port's).
                let seq = match &callee.kind {
                    ExprKind::Name(n) if !self.world.functions.contains_key(n) => {
                        vesper::seq::resolve(n, None).map(|op| (op, args.first().map(|a| &a.expr)))
                    }
                    ExprKind::Field(base, method) => self.type_of(base).and_then(|t| {
                        vesper::seq::resolve(method, Some(&t))
                            .map(|op| (op.with_arity(args.len()), Some(base.as_ref())))
                    }),
                    _ => None,
                };
                match seq {
                    Some((op, first)) => self.seq_route(op, first, &mut w),
                    None => w.extend([0, 0]),
                }
                self.tiled_route(e, callee, types, args, &mut w);
                match &callee.kind {
                    ExprKind::Name(n) => self.name_class(n, &mut w),
                    _ => w.extend([NONE, 0, NONE, 0]),
                }
                X_CALL
            }
            ExprKind::Try(v) => {
                w.push(self.expr(v));
                X_TRY
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let c = self.expr(condition);
                let t = self.expr(then);
                let o = self.opt(otherwise);
                w.extend([c, t, o]);
                X_IF
            }
            ExprKind::Match { value, arms } => {
                let v = self.expr(value);
                let kids: Vec<(u64, u64)> = arms
                    .iter()
                    .map(|(p, b)| {
                        let p = self.pattern(p);
                        (p, self.expr(b))
                    })
                    .collect();
                w.extend([v, kids.len() as u64]);
                for (p, b) in kids {
                    w.extend([p, b]);
                }
                X_MATCH
            }
            ExprKind::Loop(_)
            | ExprKind::For { .. }
            | ExprKind::Range { .. }
            | ExprKind::Break(_)
            | ExprKind::Continue
            | ExprKind::Return(_) => X_LOOP,
            ExprKind::Closure { params, body, .. } => {
                let b = self.expr(body);
                let arrow = self
                    .world
                    .types
                    .get(&(e.span.start, e.span.end))
                    .cloned()
                    .map_or(NONE, |t| self.ty(&t));
                w.push(arrow);
                w.push(params.len() as u64);
                for p in params {
                    w.push(self.name(&p.name));
                }
                w.push(b);
                let mut free = vesper::check::free_names(body);
                for p in params {
                    free.remove(&p.name);
                }
                w.push(free.len() as u64);
                for n in &free {
                    w.push(self.name(n));
                }
                X_CLOSURE
            }
            ExprKind::Handle { .. } => X_HANDLE,
        };
        let ty = self.type_of(e).map_or(NONE, |t| self.ty(&t));
        self.exprs
            .extend([kind, e.span.start as u64, e.span.end as u64, ty]);
        self.exprs.extend(w);
        self.expr_count += 1;
        self.expr_count - 1
    }

    fn section(&mut self) -> Vec<u64> {
        let world = self.world;
        let mut out = vec![vesper::fip::active(world) as u64];
        out.push(world.functions.len() as u64);
        for f in world.functions.values() {
            out.push(self.name(&f.name));
            out.push(f.body.as_ref().map_or(NONE, |b| self.expr(b)));
            out.push(f.params.len() as u64);
            for p in &f.params {
                out.push(self.name(&p.name));
                out.push(check_words::pass_word(p.pass));
                out.push(self.ty(&p.ty));
            }
            out.push(self.ty(&f.result));
            let effectful = !f.effects.instances.is_empty() || f.effects.tail.is_some();
            let polymorphic = effectful
                || effectful_arrow(&f.result)
                || f.params.iter().any(|p| effectful_arrow(&p.ty));
            out.push(polymorphic as u64);
        }
        out.push(self.pat_count);
        out.extend(&self.pats);
        out.push(self.expr_count);
        out.extend(&self.exprs);
        out
    }
}

/// `lean_nodes::effectful_arrow`.
fn effectful_arrow(t: &Type) -> bool {
    match t {
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            !effects.instances.is_empty()
                || effects.tail.is_some()
                || params.iter().any(|(_, t)| effectful_arrow(t))
                || effectful_arrow(result)
        }
        Type::Named(_, ts) | Type::Tuple(ts) => ts.iter().any(effectful_arrow),
        _ => false,
    }
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
        "lean structured: {functions} functions, {generated} generated and {errors} refused by the structured extractor over Id; {skips} skipped by the port; {unencodable} worlds not encodable"
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
