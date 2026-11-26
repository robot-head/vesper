//! Self-hosting SH2 lane 9 (#1181): the region, sequence, string, array,
//! tiled and vector rules of `packages/vesper-check-regions-seqs` against
//! the Rust functions they port, over every checked corpus world and
//! mutants that make each ported diagnostic fire. Rust stays the default
//! checker.
//!
//! Each task's Rust text calls the compiler's own functions
//! (`vesper::regions`, `vesper::seq`, `vesper::strings`, `vesper::arrays`,
//! `vesper::tiled`, `vesper::vector`); the Vesper text comes from the
//! package's JIT entries over lane 4's encoded input. Diagnostics are
//! compared by code, message, help and span (none of these has related
//! labels).
#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use vesper::ast::{
    Diagnostic, Effect, Effects, Expr, ExprKind, FieldTemperature, Mode, Pass, Type,
};
use vesper::check::World;
use vesper::{arrays, regions, seq, strings, tiled, vector};

const PACKAGE: &str = "packages/vesper-check-regions-seqs";

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load(PACKAGE))
}

fn bit(b: bool) -> &'static str {
    if b { " 1" } else { " 0" }
}

fn diag_line(d: &Diagnostic) -> String {
    assert!(d.related.is_empty(), "{d:?}");
    format!(
        "{}|{}|{}|{}|{}\n",
        d.code,
        d.message,
        d.help.as_deref().unwrap_or(""),
        d.span.start,
        d.span.end
    )
}

fn named(n: &str, args: Vec<Type>) -> Type {
    Type::Named(n.into(), args)
}

fn row(instances: Vec<Effect>, tail: Option<&str>) -> Type {
    Type::Row(Effects {
        instances,
        tail: tail.map(Into::into),
    })
}

fn effect(name: &str, args: Vec<Type>) -> Effect {
    Effect {
        label: None,
        name: name.into(),
        args,
    }
}

/// The source-label map `normalize` runs with.
fn labels() -> BTreeMap<String, String> {
    [("r", "$cap1"), ("s", "$fncap:main:0"), ("q", "$k")]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

/// Types built around `t` that make the region, tiled and vector type
/// diagnostics fire (the corpus types are all valid).
fn type_mutants(t: &Type) -> Vec<Type> {
    let cap = |l: &str| regions::region_row(l);
    let u8_ = Type::named("u8");
    vec![
        named("RegionBox", vec![t.clone()]),
        named("RegionBox", vec![Type::named("r"), t.clone()]),
        named("RegionBox", vec![cap("$cap3"), t.clone()]),
        named("RegionBox", vec![cap("$fncap:f:2"), t.clone()]),
        named("Watermark", vec![row(vec![], Some("s"))]),
        named("Watermark", vec![cap("$cap9")]),
        named("Watermark", vec![t.clone(), t.clone()]),
        named("TiledSeq", vec![t.clone(), Type::named("r")]),
        named("TiledSeq", vec![t.clone()]),
        named("TiledSeq", vec![t.clone(), cap("$cap3")]),
        named("TiledSeq", vec![t.clone(), Type::named("zz")]),
        named("Vec", vec![t.clone(), Type::named("4")]),
        named("Vec", vec![t.clone(), Type::named("16")]),
        named("Mask", vec![t.clone(), Type::named("2")]),
        named("Vec", vec![t.clone(), Type::named("x")]),
        named("Vec", vec![u8_.clone(), Type::named("99999")]),
        named("Vec", vec![u8_.clone(), Type::named("0016")]),
        named("ChunkSeq", vec![t.clone()]),
        named("Slice", vec![t.clone()]),
        named("ChunkSeq", vec![named("Slice", vec![t.clone()])]),
        Type::Tuple(vec![t.clone(), t.clone(), t.clone()]),
        Type::Tuple(vec![t.clone(), u8_.clone()]),
        Type::Function {
            params: vec![(Pass::In, named("Slice", vec![t.clone()]))],
            result: Box::new(named("Str", vec![])),
            effects: Effects {
                instances: vec![
                    effect("Isa", vec![t.clone()]),
                    effect("Isa", vec![Type::named("Avx2")]),
                    effect("Isa", vec![]),
                    effect("Region", vec![]),
                ],
                tail: None,
            },
            mode: Mode::Copy,
        },
        row(
            vec![
                effect("Isa", vec![Type::named("Sve2")]),
                effect("Isa", vec![t.clone(), u8_]),
            ],
            None,
        ),
    ]
}

/// The corpus types of a world, then mutants of its first 24.
fn task_types(w: &World) -> Vec<Type> {
    let base = world_types(w);
    let mut out = base.clone();
    for t in base.iter().take(24) {
        out.extend(type_mutants(t));
    }
    out
}

/// Task "types" in Rust (the lines of `regions_seqs_types` in lib.ves).
fn types_text(w: &World, types: &[Type]) -> String {
    let labels = labels();
    let mut o = String::new();
    let mut prev: Option<Type> = None;
    for (i, t) in types.iter().enumerate() {
        let span = i..i + 1;
        o += &format!("T {t}\n");
        o += "R";
        o += bit(regions::is_handle_type(t));
        match regions::validate_type(t, span.clone()) {
            None => o += " -\n",
            Some(ds) => {
                o += &format!(" {}\n", ds.len());
                for d in &ds {
                    o += "  ";
                    o += &diag_line(d);
                }
            }
        }
        let norm = regions::normalize(t, &labels);
        o += &format!("N {}\n", regions::display(&norm));
        o += "M ";
        match prev
            .as_ref()
            .and_then(|p| regions::mismatch(p, &norm, span.clone()))
        {
            Some(d) => o += &diag_line(&d),
            None => o += "-\n",
        }
        prev = Some(norm);
        let dash = |t: Option<&Type>| t.map_or(" -".to_string(), |t| format!(" {t}"));
        o += "S";
        o += bit(seq::is_seq_type(t));
        o += bit(seq::is_slice_type(t));
        o += &dash(seq::elem(t));
        o += bit(seq::mentions_slice(t));
        o += bit(seq::holds_sequence(w, t));
        o += "\nX";
        o += bit(strings::is_string_type(t));
        o += bit(strings::is_str_type(t));
        o += bit(strings::mentions_str(t));
        match arrays::element(t) {
            Some((e, n)) => o += &format!(" {e} {n}"),
            None => o += " -",
        }
        o += "\nL";
        o += bit(tiled::is_seq_type(t));
        o += &dash(tiled::seq_elem(t));
        o += &format!(" {}", tiled::seq_region_label(t).unwrap_or("-"));
        match tiled::seq_adt(w, t) {
            Some((a, width)) => o += &format!(" {}/{width}", a.name),
            None => o += " -",
        }
        o += &format!(" {}", tiled::hot_elem_bytes(t).unwrap_or(0));
        for width in [8, 16, 32, 64] {
            match tiled::hot_vector_shape(t, width) {
                Some(s) => o += &format!(" {s}"),
                None => o += " -",
            }
        }
        o += "\nV";
        o += bit(vector::is_vector_spelling(t));
        match vector::shape_of(t) {
            Some((kind, s)) => {
                o += &format!(
                    " {}{s} {} {} {} {}",
                    kind.name(),
                    s.bits(),
                    s.chunks(),
                    s.lanes_per_chunk(),
                    s.required_profile().name()
                )
            }
            None => o += " -",
        }
        o += &format!(" {}", vector::type_error(t).unwrap_or("-".into()));
        match vector::layout_words(t) {
            Some(n) => o += &format!(" {n}\n"),
            None => o += " -\n",
        }
        let effects = match t {
            Type::Row(e) => Some(e),
            Type::Function { effects, .. } => Some(effects),
            _ => None,
        };
        for e in effects.iter().flat_map(|e| &e.instances) {
            o += "I ";
            match vector::isa_row_entry(e, span.clone()) {
                Some(d) => o += &diag_line(&d),
                None => o += "-\n",
            }
        }
    }
    o
}

fn types_input(w: &World, types: &[Type]) -> Vec<u8> {
    encode(w, |e, tail| {
        let labels = labels();
        tail.push(labels.len() as u64);
        for (a, b) in &labels {
            tail.push(e.name(a));
            tail.push(e.name(b));
        }
        tail.push(types.len() as u64);
        for t in types {
            tail.push(e.ty(t));
        }
    })
}

/// The diagnostic codes a Rust text contains, with their counts.
fn codes(text: &str, into: &mut BTreeMap<String, usize>) {
    for l in text.lines() {
        let l = l
            .trim_start()
            .trim_start_matches("M ")
            .trim_start_matches("I ");
        if let Some((code, _)) = l.split_once('|')
            && code.starts_with('E')
        {
            *into.entry(code.to_string()).or_default() += 1;
        }
    }
}

#[test]
fn type_rules_match_rust() {
    let f = package().entry("regions_seqs_types", true);
    let mut fired = BTreeMap::new();
    let bad = differential(
        "regions-seqs types",
        |c| {
            let t = types_text(&c.world, &task_types(&c.world));
            codes(&t, &mut fired);
            t
        },
        |c| f.text(&types_input(&c.world, &task_types(&c.world))),
    );
    eprintln!("regions-seqs types: diagnostics {fired:?}");
    assert_eq!(bad, 0);
    for code in ["E0200", "E0792", "E0797", "E1404"] {
        assert!(fired.contains_key(code), "{code} never fired");
    }
}

// ------------------------------------------------------------------ world

/// The world mutants: 0 is the world itself; 1 moves `Slice[T]` into
/// parameters, results and fields (E1520); 2 and 3 annotate ADTs with tile
/// widths and field temperatures (E1501-E1503).
fn world_mutant(w: &World, m: usize) -> World {
    let mut w = w.clone();
    let slice = |t: &Type| named("Slice", vec![t.clone()]);
    match m {
        1 => {
            for (j, f) in w.functions.values_mut().enumerate() {
                match (j % 4, f.params.first_mut()) {
                    (0, Some(p)) => p.ty = slice(&p.ty),
                    (1, Some(p)) => {
                        p.ty = slice(&p.ty);
                        p.pass = Pass::Sink;
                    }
                    (3, Some(p)) => p.ty = slice(&slice(&p.ty)),
                    _ => f.result = Type::Tuple(vec![slice(&Type::named("u8")), f.result.clone()]),
                }
            }
            for (k, a) in w.adts.values_mut().enumerate() {
                if k % 3 == 0
                    && let Some(f) = a.variants.iter_mut().flat_map(|v| &mut v.fields).next()
                {
                    f.ty = named("ChunkSeq", vec![slice(&f.ty)]);
                }
            }
        }
        2 | 3 => {
            let temps = [
                None,
                Some(FieldTemperature::Hot),
                Some(FieldTemperature::Cold),
            ];
            for (k, a) in w.adts.values_mut().enumerate() {
                let k = k + m;
                a.tile_width = match k % 5 {
                    0 => Some(8),
                    1 => Some(12),
                    2 => Some(16),
                    3 => None,
                    _ => Some(64),
                };
                for (x, f) in a
                    .variants
                    .iter_mut()
                    .flat_map(|v| &mut v.fields)
                    .enumerate()
                {
                    f.temperature = match k % 5 {
                        3 => (x == 0).then_some(FieldTemperature::Hot),
                        4 => Some(FieldTemperature::Cold),
                        _ => temps[(x + k) % 3],
                    };
                }
            }
        }
        _ => {}
    }
    w
}

fn world_text(w: &World) -> String {
    let mut o = String::new();
    for d in seq::check_world(w) {
        o += &diag_line(&d);
    }
    for d in tiled::check_world_adts(w) {
        o += &diag_line(&d);
    }
    o
}

fn world_input(w: &World) -> Vec<u8> {
    encode(w, |e, tail| {
        tail.push(w.functions.len() as u64);
        for f in w.functions.values() {
            tail.extend([e.name(&f.name), f.span.start as u64, f.span.end as u64]);
            tail.push(e.ty(&f.result));
            tail.push(f.params.len() as u64);
            for p in &f.params {
                tail.extend([e.name(&p.name), pass_word(p.pass), e.ty(&p.ty)]);
                tail.extend([p.span.start as u64, p.span.end as u64]);
            }
        }
        for a in w.adts.values() {
            tail.extend([a.span.start as u64, a.span.end as u64]);
        }
        for a in w.adts.values() {
            for f in a.variants.iter().flat_map(|v| &v.fields) {
                tail.push(match f.temperature {
                    None => 0,
                    Some(FieldTemperature::Hot) => 1,
                    Some(FieldTemperature::Cold) => 2,
                });
            }
        }
    })
}

#[test]
fn world_rules_match_rust() {
    let f = package().entry("regions_seqs_world", true);
    let mut fired = BTreeMap::new();
    for m in 0..4 {
        let bad = differential(
            &format!("regions-seqs world m{m}"),
            |c| {
                let t = world_text(&world_mutant(&c.world, m));
                codes(&t, &mut fired);
                t
            },
            |c| f.text(&world_input(&world_mutant(&c.world, m))),
        );
        assert_eq!(bad, 0, "mutant {m}");
    }
    eprintln!("regions-seqs world: diagnostics {fired:?}");
    for code in ["E1501", "E1502", "E1503", "E1520"] {
        assert!(fired.contains_key(code), "{code} never fired");
    }
}

// ------------------------------------------------------------------ resolve

const CALLEES: &[&str] = &[
    "len",
    "push",
    "pop",
    "get",
    "set",
    "try_get",
    "try_get_mut",
    "try_set",
    "clear",
    "truncate",
    "reserve",
    "extend",
    "extend_from_slice",
    "extend_ascii_lowercase",
    "resize",
    "extend_from_within",
    "try_extend_from_within",
    "extend_reversed",
    "as_slice",
    "as_slice_mut",
    "copy_from",
    "try_copy_from",
    "copy_within",
    "try_copy_within",
    "fill",
    "try_fill",
    "sum_be16",
    "eq",
    "cmp",
    "find_byte",
    "find_either",
    "find_not_in",
    "eq_ignore_ascii_case",
    "make_ascii_lowercase",
    "try_make_ascii_lowercase",
    "byte_at",
    "try_byte_at",
    "as_str",
    "as_bytes",
    "push_str",
    "push_char",
    "swap",
    "replace",
    "ChunkSeq::new",
    "ChunkSeq::with_capacity",
    "ChunkSeq::finish",
    "read_u16_le",
    "read_u16_be",
    "read_u32_le",
    "read_u32_be",
    "read_u64_le",
    "read_u64_be",
    "write_u16_le",
    "write_u16_be",
    "write_u32_le",
    "write_u32_be",
    "write_u64_le",
    "write_u64_be",
    "try_read_u16_le",
    "try_read_u16_be",
    "try_read_u32_le",
    "try_read_u32_be",
    "try_read_u64_le",
    "try_read_u64_be",
    "try_write_u16_le",
    "try_write_u16_be",
    "try_write_u32_le",
    "try_write_u32_be",
    "try_write_u64_le",
    "try_write_u64_be",
    "ChunkSeq::len",
    "Slice::get",
    "a::b::push_str",
    "String::new",
    "ChunkSeq::nope",
    "nope",
    "ChunkSeq::new::x",
    "::len",
    "read_u128_le",
    "",
];

fn receivers() -> Vec<Option<Type>> {
    let u8_ = Type::named("u8");
    vec![
        None,
        Some(named("ChunkSeq", vec![u8_.clone()])),
        Some(named("ChunkSeq", vec![Type::named("u64")])),
        Some(named("Slice", vec![u8_.clone()])),
        Some(named("Slice", vec![Type::named("T")])),
        Some(Type::named("String")),
        Some(Type::named("Str")),
        Some(named("Str", vec![u8_.clone()])),
        Some(named("ChunkSeq", vec![u8_.clone(), u8_])),
        Some(Type::named("u64")),
        Some(named("TiledSeq", vec![Type::named("P")])),
    ]
}

fn resolve_text() -> String {
    let mut o = String::new();
    for r in receivers() {
        let mut w = World::default();
        if let Some(t) = &r {
            w.types.insert((0, 1), t.clone());
        }
        for c in CALLEES {
            let Some(op) = seq::resolve(c, r.as_ref()) else {
                o += "-\n";
                continue;
            };
            let callee = match &r {
                None => Expr {
                    kind: ExprKind::Name(c.to_string()),
                    span: 2..3,
                },
                Some(_) => Expr {
                    kind: ExprKind::Field(
                        Box::new(Expr {
                            kind: ExprKind::Name("x".into()),
                            span: 0..1,
                        }),
                        c.to_string(),
                    ),
                    span: 0..3,
                },
            };
            o += &format!(
                "{op:?} {}{}{} {:?}{}{}{}{} {:?}\n",
                op.name(),
                bit(op.fallible()),
                bit(op.mutates()),
                op.receiver_pass(),
                bit(op.byte_scan()),
                bit(op.has_receiver()),
                bit(op.place_primitive()),
                bit(seq::fip_allocation(&w, &callee).is_some()),
                op.with_arity(3)
            );
        }
    }
    o
}

fn resolve_input() -> Vec<u8> {
    encode(&World::default(), |e, tail| {
        let rs = receivers();
        tail.push((rs.len() * CALLEES.len()) as u64);
        for r in &rs {
            for c in CALLEES {
                tail.push(e.name(c));
                tail.push(r.as_ref().map_or(0, |t| e.ty(t) + 1));
            }
        }
    })
}

#[test]
fn resolve_matches_rust() {
    let f = package().entry("regions_seqs_resolve", true);
    let rust = resolve_text();
    let vesper = f.text(&resolve_input());
    if let Some(d) = first_difference(&rust, &vesper) {
        panic!("resolve differs at {d}");
    }
    assert!(rust.lines().filter(|l| *l != "-").count() > 100);
}

/// The JIT with the optimizer off agrees with it on (one world each task).
#[test]
fn unoptimized_jit_agrees() {
    let pkg = package();
    let case = corpus()
        .iter()
        .max_by_key(|c| c.world.adts.len() + c.world.functions.len())
        .unwrap();
    for (name, input) in [
        (
            "regions_seqs_types",
            types_input(&case.world, &task_types(&case.world)),
        ),
        (
            "regions_seqs_world",
            world_input(&world_mutant(&case.world, 1)),
        ),
        ("regions_seqs_resolve", resolve_input()),
    ] {
        let on = pkg.entry(name, true).text(&input);
        let off = pkg.entry(name, false).text(&input);
        assert_eq!(on, off, "{name}");
    }
}

// ------------------------------------------------------------------ speed

/// ABBA of the type and world tasks over the corpus (CHECK_HARNESS_SPEED=1).
/// Each Vesper run includes decoding its input; the decode alone is timed
/// too and reported, so the rules' own time is the difference.
#[test]
#[ignore]
fn speed_abba() {
    if !check_harness::speed() {
        return;
    }
    let pkg = package();
    let types = pkg.entry("regions_seqs_types", true);
    let world = pkg.entry("regions_seqs_world", true);
    let decode = pkg.entry("regions_seqs_decode", true);
    let cases: Vec<_> = corpus()
        .iter()
        .map(|c| {
            let ts = task_types(&c.world);
            let input = types_input(&c.world, &ts);
            let ws: Vec<World> = (0..4).map(|m| world_mutant(&c.world, m)).collect();
            let winputs: Vec<Vec<u8>> = ws.iter().map(world_input).collect();
            (c, ts, input, ws, winputs)
        })
        .collect();
    let (r, v) = abba(
        "regions-seqs types",
        &mut || {
            time(|| {
                for (c, ts, ..) in &cases {
                    std::hint::black_box(types_text(&c.world, ts));
                }
            })
        },
        &mut || {
            time(|| {
                for (_, _, input, ..) in &cases {
                    std::hint::black_box(types.text(input));
                }
            })
        },
    );
    let d = (0..6)
        .map(|_| {
            time(|| {
                for (_, _, input, ..) in &cases {
                    std::hint::black_box(decode.text(input));
                }
            })
        })
        .min()
        .unwrap();
    eprintln!(
        "types: decode {d:?}; rules alone {:.2}x",
        (v.saturating_sub(d)).as_secs_f64() / r.as_secs_f64()
    );
    let (r, v) = abba(
        "regions-seqs world",
        &mut || {
            time(|| {
                for (.., ws, _) in &cases {
                    for w in ws {
                        std::hint::black_box(world_text(w));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for (.., winputs) in &cases {
                    for i in winputs {
                        std::hint::black_box(world.text(i));
                    }
                }
            })
        },
    );
    let d = (0..6)
        .map(|_| {
            time(|| {
                for (.., winputs) in &cases {
                    for i in winputs {
                        std::hint::black_box(decode.text(i));
                    }
                }
            })
        })
        .min()
        .unwrap();
    eprintln!(
        "world: decode {d:?}; rules alone {:.2}x",
        (v.saturating_sub(d)).as_secs_f64() / r.as_secs_f64()
    );
}

// ------------------------------------------------------------------ isa

/// `isa.ves`: `vector::check_world` (E1401-E1403) on the corpus files and
/// the vector tests' sources, against the Rust walk.
mod isa {
    use super::*;

    struct In(Vec<u8>);

    impl In {
        fn word(&mut self, x: u64) {
            self.0.extend_from_slice(&x.to_le_bytes());
        }
        fn text(&mut self, s: &str) {
            self.word(s.len() as u64);
            self.0.extend_from_slice(s.as_bytes());
            while !self.0.len().is_multiple_of(8) {
                self.0.push(0);
            }
        }
        fn ty(&mut self, t: &Type) {
            match t {
                Type::Named(n, args) => {
                    self.word(1);
                    self.text(n);
                    self.word(args.len() as u64);
                    for a in args {
                        self.ty(a);
                    }
                }
                Type::Tuple(items) => {
                    self.word(2);
                    self.word(items.len() as u64);
                    for a in items {
                        self.ty(a);
                    }
                }
                Type::Function { effects, .. } => {
                    self.word(3);
                    self.effects(effects);
                }
                _ => self.word(0),
            }
        }
        fn effects(&mut self, e: &Effects) {
            self.word(e.instances.len() as u64);
            for i in &e.instances {
                self.text(&i.name);
                self.word(i.args.len() as u64);
                for a in &i.args {
                    self.ty(a);
                }
            }
        }
    }

    /// The world facts `isa_text` reads.
    fn input(w: &World) -> Vec<u8> {
        let mut i = In(Vec::new());
        i.word(w.adts.len() as u64);
        for (name, a) in &w.adts {
            i.text(name);
            i.word(a.span.start as u64);
            i.word(a.span.end as u64);
            i.word(a.generics.len() as u64);
            for g in &a.generics {
                i.text(&g.name);
            }
            let fields: Vec<&Type> = a
                .variants
                .iter()
                .flat_map(|v| &v.fields)
                .map(|f| &f.ty)
                .collect();
            i.word(fields.len() as u64);
            for t in fields {
                i.ty(t);
            }
        }
        i.word(w.functions.len() as u64);
        for (name, f) in &w.functions {
            i.text(name);
            i.effects(&f.effects);
            match &f.body {
                Some(b) => {
                    i.word(1);
                    i.word(b.span.start as u64);
                    i.word(b.span.end as u64);
                }
                None => {
                    i.word(0);
                    i.word(0);
                    i.word(0);
                }
            }
        }
        i.word(w.types.len() as u64);
        for ((s, e), t) in &w.types {
            i.word(*s as u64);
            i.word(*e as u64);
            i.ty(t);
        }
        i.0
    }

    fn rust(w: &World) -> String {
        vector::check_world_probe(w)
            .iter()
            .map(|d| {
                let help = d
                    .help
                    .as_ref()
                    .map(|h| format!(" | {h}"))
                    .unwrap_or_default();
                format!(
                    "{} {}..{} {}{help}\n",
                    d.code, d.span.start, d.span.end, d.message
                )
            })
            .collect()
    }

    fn call(f: &vesper::native::WordFunction, input: &[u8], source: &str) -> String {
        let a = [input.len() as u64, 0, input.as_ptr() as u64];
        let s = [source.len() as u64, 0, source.as_ptr() as u64];
        // SAFETY: the entry takes two `in Str`s, each the address of its
        // three words `{len, 0, data}`, which live across the call, as do
        // `input` and `source`.
        unpack_text(&take_words(
            unsafe { f.call(&[a.as_ptr() as u64, s.as_ptr() as u64]) }.unwrap(),
        ))
    }

    /// The string literals of a Rust test file (plain and `r#"..."#`).
    fn literals(text: &str) -> Vec<String> {
        let b = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i..].starts_with(b"r#\"") {
                let start = i + 3;
                if let Some(end) = text[start..].find("\"#") {
                    out.push(text[start..start + end].to_string());
                    i = start + end + 2;
                    continue;
                }
                break;
            }
            if b[i] == b'"' && (i == 0 || b[i - 1] != b'\'') {
                let mut s = String::new();
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    if b[j] == b'\\' && j + 1 < b.len() {
                        match b[j + 1] {
                            b'n' => s.push('\n'),
                            b't' => s.push('\t'),
                            b'\n' => {
                                j += 2;
                                while j < b.len() && (b[j] == b' ' || b[j] == b'\n') {
                                    j += 1;
                                }
                                continue;
                            }
                            c => s.push(c as char),
                        }
                        j += 2;
                        continue;
                    }
                    let ch = text[j..].chars().next().unwrap();
                    s.push(ch);
                    j += ch.len_utf8();
                }
                out.push(s);
                i = j + 1;
                continue;
            }
            i += 1;
        }
        out
    }

    /// Sources for the walk's paths the tests reach rarely: dispatch,
    /// closures, function values, methods, widening, `load_tile_vec`,
    /// lane-index literals and ADTs holding vectors.
    const HAND: &[&str] = &[
        "fn f(a: Vec[u32, 8]) -> u32 { if Isa::supported[Avx2]() { Vec::reduce_add(a) } else { 0 } }",
        "fn f(a: Vec[u32, 8]) -> u32 eff [Isa[Avx2]] { let g = |v: Vec[u32, 8]| Vec::reduce_add(v); g(a) }",
        "fn f(a: Vec[u32, 8]) -> u32 { let g = |v: Vec[u32, 8]| -> u32 eff [Isa[Avx2]] { Vec::reduce_add(v) }; 0 }",
        "fn wide(a: Vec[u32, 8]) -> u32 eff [Isa[Avx2]] { Vec::reduce_add(a) }\nfn f(a: Vec[u32, 8]) -> u32 eff [Isa[Baseline128]] { wide(a) }",
        "fn wide(a: Vec[u32, 8]) -> u32 eff [Isa[Avx512]] { Vec::reduce_add(a) }\nfn f(a: Vec[u32, 8], g: fn(Vec[u32, 8]) -> u32 eff [Isa[Avx2]]) -> u32 { g(a) }",
        "struct P { v: Vec[u32, 4] }\nimpl P { fn sum(self) -> u32 eff [Isa[Avx2]] { Vec::reduce_add(self.v) } }\nfn f(p: P) -> u32 { p.sum() }",
        "struct P[T] { v: T }\nfn f(a: P[Vec[u32, 4]], b: P[Vec[u32, 4]]) -> bool { a == b }",
        "fn f(a: (Vec[u32, 4], u64), b: (Vec[u32, 4], u64)) -> bool { a != b }",
        "fn f(a: Vec[f32, 4]) -> Vec[f64, 4] eff [Isa[Baseline128]] { Vec::widen_f64(a) }",
        "fn f(a: Vec[u32, 16]) -> u32 eff [Isa[Avx2]] { Vec::extract(a, 3) }",
        "fn f(a: Vec[u32, 16], i: u64) -> u32 eff [Isa[Avx2]] { Vec::extract(a, i) }",
        "fn f(a: Vec[u32, 8]) -> Vec[u32, 8] { Vec::shuffle(a, 0, 1, 2, 3, 4, 5, 6, 7) }",
        "fn f() -> Vec[u32, 8] { Vec::splat[u32, 8](1) }",
        "fn f(in s: Slice[u32]) -> Vec[u32, 16] eff [Isa[Avx2]] { Vec::load[u32, 16](s, 0) }",
        "struct Vec[T, U] { a: T, b: U }\nfn f() -> u64 { 0 }",
        "fn f(a: Vec[u32, 8]) -> u32 eff [Isa[Sve2], Isa[Baseline128]] { Vec::reduce_add(a) }",
        "fn f(a: Vec[u32, 8]) -> u32 { handle Fail[u64] in { Vec::reduce_add(a) } with { return x => x, raise(e, k) => 0, } }",
        "fn f(a: Vec[u32, 8]) -> u32 { let mut n = 0; for i in 0..4 { n = Vec::reduce_add(a); } loop { break; } n }",
        "fn f(a: Vec[u32, 8], b: bool) -> u32 { match b { true => Vec::reduce_add(a), false => 0 } }",
    ];

    /// Corpus files (packages span several sources), every vector test's
    /// source that parses, and [`HAND`].
    fn cases() -> Vec<(String, String, World)> {
        let mut out = Vec::new();
        for c in corpus() {
            if c.name.ends_with('/') {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&c.name) else {
                continue;
            };
            out.push((c.name.clone(), src, c.world.clone()));
        }
        for file in [
            "tests/vector.rs",
            "tests/vector_f32.rs",
            "tests/vector_kernel.rs",
            "tests/vector_slice_io.rs",
            "tests/tiled.rs",
            "hand",
        ] {
            let text = std::fs::read_to_string(file).unwrap_or_default();
            let srcs = if file == "hand" {
                HAND.iter().map(|s| s.to_string()).collect()
            } else {
                literals(&text)
            };
            for (k, src) in srcs.into_iter().enumerate() {
                if !src.contains("fn ") {
                    continue;
                }
                let parsed = vesper::parser::parse(&src);
                if !parsed.diagnostics.is_empty() {
                    if file == "hand" {
                        eprintln!(
                            "isa: hand source {k} does not parse: {:?}",
                            parsed.diagnostics
                        );
                    }
                    continue;
                }
                let r = std::panic::catch_unwind(|| vesper::check::check(&parsed.module).world);
                if let Ok(world) = r {
                    out.push((format!("{file}#{k}"), src, world));
                }
            }
        }
        out
    }

    #[test]
    fn isa_walk_matches_rust() {
        let f = package().entry("regions_seqs_isa", true).0;
        let quiet = quiet_panics();
        let cases = cases();
        drop(quiet);
        let (mut bad, mut diags, mut missing) = (0, 0, 0);
        let mut codes = BTreeMap::<String, usize>::new();
        for (name, src, w) in &cases {
            let r = rust(w);
            let v = call(&f, &input(w), src);
            let (miss, v): (Vec<&str>, Vec<&str>) =
                v.lines().partition(|l| l.starts_with("missing "));
            missing += miss.len();
            let v: String = v.iter().map(|l| format!("{l}\n")).collect();
            for l in r.lines() {
                diags += 1;
                *codes.entry(l[..5].to_string()).or_default() += 1;
            }
            if r != v {
                bad += 1;
                if bad <= 5 {
                    eprintln!("isa: {name} differs:\n  rust:\n{r}  vesper:\n{v}");
                }
            }
        }
        eprintln!(
            "isa: {bad} of {} worlds differ; {diags} diagnostics {codes:?}; {missing} functions outside the source",
            cases.len()
        );
        assert!(codes.len() >= 3, "codes {codes:?}");
        assert_eq!(bad, 0);
    }

    /// What the walk costs: Rust's parse and `check_world` against the
    /// Vesper entry, which also parses the source and decodes its input
    /// (`ISA_COST_WALK_ONLY` times Rust's walk alone).
    #[test]
    #[ignore = "timing; run with CHECK_HARNESS_SPEED=1 and --ignored"]
    fn isa_walk_cost() {
        let f = package().entry("regions_seqs_isa", true).0;
        let quiet = quiet_panics();
        let cases = cases();
        drop(quiet);
        let inputs: Vec<Vec<u8>> = cases.iter().map(|(_, _, w)| input(w)).collect();
        // Rust's side with its parse, like the Vesper entry's.
        let parse = std::env::var_os("ISA_COST_WALK_ONLY").is_none();
        let (r, v) = abba(
            "isa walk",
            &mut || {
                time(|| {
                    for (_, src, w) in &cases {
                        if parse {
                            std::hint::black_box(vesper::parser::parse(src));
                        }
                        std::hint::black_box(vector::check_world_probe(w));
                    }
                })
            },
            &mut || {
                time(|| {
                    for ((_, src, _), i) in cases.iter().zip(&inputs) {
                        std::hint::black_box(call(&f, i, src));
                    }
                })
            },
        );
        eprintln!(
            "rust {r:?} vesper {v:?} ({:.2}x)",
            v.as_secs_f64() / r.as_secs_f64()
        );
    }
}
