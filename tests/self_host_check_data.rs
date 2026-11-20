//! Self-hosting SH2 lane 4 (#1176): `@vesper/check-data` against the Rust
//! checker's data over the corpus. Each task writes one line per answer in
//! both languages and the texts must be equal: type `Display` and `Debug`,
//! `default_type`, `substitute` (with row splicing), and the `World`
//! lookups (`mode`, `field`, `constructor`, `vector::layout_words`, effectful
//! rows, expression types, partial and overflowing operations).
#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::collections::BTreeMap;
use std::fmt::Write;
use vesper::ast::{Effect, Effects, Mode, Type, substitute};
use vesper::check::World;

/// A type and the substitution applied to it, as (name, type) pairs.
type Subst = (Type, Vec<(String, Type)>);

/// Substitution cases of one world: each ADT field type under the generic
/// arguments of every instance of the ADT the world mentions, and each
/// function type with an open row whose tail is bound to a row (its own
/// instances again, which `substitute_effects` must not duplicate, plus one
/// more and a new tail).
fn substitutions(w: &World, types: &[Type]) -> Vec<Subst> {
    let mut out = vec![];
    for t in types {
        match t {
            Type::Named(n, args) if !args.is_empty() => {
                let Some(a) = w.adts.get(n) else { continue };
                let vars: Vec<(String, Type)> = a
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                for v in &a.variants {
                    for f in &v.fields {
                        out.push((f.ty.clone(), vars.clone()));
                    }
                }
            }
            Type::Function { effects, .. } => {
                if let Some(tail) = &effects.tail {
                    let mut instances = effects.instances.clone();
                    instances.push(Effect {
                        label: None,
                        name: "Fail".into(),
                        args: vec![Type::named("u8")],
                    });
                    let row = Type::Row(Effects {
                        instances,
                        tail: Some("check_data_tail".into()),
                    });
                    out.push((t.clone(), vec![(tail.clone(), row)]));
                }
            }
            _ => {}
        }
    }
    out.truncate(4000);
    out
}

fn types_task(w: &World) -> (Vec<Type>, Vec<Subst>) {
    let types = world_types(w);
    let subst = substitutions(w, &types);
    (types, subst)
}

fn rust_types(w: &World) -> String {
    let (types, subst) = types_task(w);
    let mut o = String::new();
    for t in &types {
        writeln!(o, "D {t}\nG {t:?}\nF {}", vesper::numeric::default_type(t)).unwrap();
    }
    for (t, vars) in &subst {
        let map: BTreeMap<String, Type> = vars.iter().cloned().collect();
        let s = substitute(t, &map);
        writeln!(o, "S {s}\nG {s:?}").unwrap();
    }
    o
}

fn types_input(w: &World) -> Vec<u8> {
    let (types, subst) = types_task(w);
    encode(w, |e, tail| {
        tail.push(types.len() as u64);
        for t in &types {
            tail.push(e.ty(t));
        }
        tail.push(subst.len() as u64);
        for (t, vars) in &subst {
            tail.push(e.ty(t));
            tail.push(vars.len() as u64);
            for (n, v) in vars {
                tail.push(e.name(n));
                tail.push(e.ty(v));
            }
        }
    })
}

/// The field names `World::field` is asked about for `t`: the fields of the
/// ADT it names (or, for a `TiledSeq`, of its element), a segment, tuple
/// indices and a missing name.
fn field_names(w: &World, t: &Type) -> Vec<String> {
    let mut out: Vec<String> = ["[..]", "0", "1", "check_data_missing"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let adt = match t {
        Type::Named(n, args) if n == "TiledSeq" => match args.first() {
            Some(Type::Named(e, _)) => w.adts.get(e),
            _ => None,
        },
        Type::Named(n, _) => w.adts.get(n),
        _ => None,
    };
    if let Some(a) = adt
        && let Some(v) = a.variants.first()
    {
        out.extend(v.fields.iter().map(|f| f.name.clone()));
    }
    out
}

fn effectful_row(w: &World, row: &Effects) -> bool {
    row.tail.is_some()
        || row
            .instances
            .iter()
            .any(|e| w.effects.get(&e.name).is_some_and(|d| !d.linear))
}

/// Constructor queries: every variant name bare, qualified by its ADT and by
/// another ADT, and a missing name. (qualifier, short name).
fn constructor_queries(w: &World) -> Vec<(Option<String>, String)> {
    let names: Vec<&String> = w.adts.keys().collect();
    let mut out = vec![(None, "CheckDataMissing".to_string())];
    for (i, a) in w.adts.values().enumerate() {
        let other = names[(i + 1) % names.len()];
        for v in &a.variants {
            out.push((None, v.name.clone()));
            out.push((Some(a.name.clone()), v.name.clone()));
            out.push((Some(other.clone()), v.name.clone()));
        }
    }
    out
}

/// Span probes: every expression span, every partial and overflowing
/// operation, and a span next to each expression that is none of them.
fn span_probes(w: &World) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = w.types.keys().copied().collect();
    out.extend(w.partial_ops.iter().copied());
    out.extend(w.overflow_ops.iter().copied());
    out.extend(w.types.keys().map(|(a, b)| (*a, b + 1)));
    out
}

/// Bound cases: each generic ADT's self type under its generics' bounds.
fn bound_cases(w: &World) -> Vec<(Type, Vec<(String, Mode)>)> {
    w.adts
        .values()
        .filter(|a| !a.generics.is_empty())
        .map(|a| {
            let ty = Type::Named(
                a.name.clone(),
                a.generics.iter().map(|g| Type::named(&g.name)).collect(),
            );
            (
                ty,
                a.generics
                    .iter()
                    .map(|g| (g.name.clone(), g.bound))
                    .collect(),
            )
        })
        .collect()
}

fn rust_world(w: &World) -> String {
    let mut o = String::new();
    let none = BTreeMap::new();
    for t in world_types(w) {
        writeln!(
            o,
            "M {}\nV {}",
            mode_word(w.mode(&t, &none)),
            vesper::vector::layout_words(&t).unwrap_or(0)
        )
        .unwrap();
        for name in field_names(w, &t) {
            match w.field(&t, std::slice::from_ref(&name)) {
                Some(f) => writeln!(o, "P {f}").unwrap(),
                None => o.push_str("P -\n"),
            }
        }
        match &t {
            Type::Function { effects, .. } | Type::Row(effects) => {
                writeln!(o, "E {}", effectful_row(w, effects) as u8).unwrap()
            }
            _ => {}
        }
    }
    for (t, bounds) in bound_cases(w) {
        let map: BTreeMap<String, Mode> = bounds.into_iter().collect();
        writeln!(o, "B {}", mode_word(w.mode(&t, &map))).unwrap();
    }
    for (q, short) in constructor_queries(w) {
        let name = match &q {
            Some(q) => format!("{q}::{short}"),
            None => short.clone(),
        };
        match w.constructor(&name) {
            Some((a, v)) => writeln!(o, "C {}::{}", a.name, v.name).unwrap(),
            None => o.push_str("C -\n"),
        }
    }
    for (a, b) in span_probes(w) {
        match w.types.get(&(a, b)) {
            Some(t) => writeln!(o, "T {t}").unwrap(),
            None => o.push_str("T -\n"),
        }
        writeln!(
            o,
            "O {}{}",
            w.partial_ops.contains(&(a, b)) as u8,
            w.overflow_ops.contains(&(a, b)) as u8
        )
        .unwrap();
    }
    o
}

fn world_input(w: &World) -> Vec<u8> {
    encode(w, |e, tail| {
        let types = world_types(w);
        tail.push(types.len() as u64);
        for t in &types {
            tail.push(e.ty(t));
            let names = field_names(w, t);
            tail.push(names.len() as u64);
            for n in &names {
                tail.push(e.name(n));
            }
        }
        let bounds = bound_cases(w);
        tail.push(bounds.len() as u64);
        for (t, bs) in &bounds {
            tail.push(e.ty(t));
            tail.push(bs.len() as u64);
            for (n, m) in bs {
                tail.push(e.name(n));
                tail.push(mode_word(*m));
            }
        }
        let queries = constructor_queries(w);
        tail.push(queries.len() as u64);
        for (q, short) in &queries {
            tail.push(q.as_ref().map_or(0, |q| e.name(q) + 1));
            tail.push(e.name(short));
        }
        let probes = span_probes(w);
        tail.push(probes.len() as u64);
        for (a, b) in probes {
            tail.extend([a as u64, b as u64]);
        }
    })
}

#[test]
fn type_display_debug_default_and_substitute_match_rust() {
    let f = check_data().entry("check_data_types", true);
    let bad = differential(
        "types",
        |c| rust_types(&c.world),
        |c| f.text(&types_input(&c.world)),
    );
    assert_eq!(bad, 0);
}

#[test]
fn world_lookups_match_rust() {
    let f = check_data().entry("check_data_world", true);
    let bad = differential(
        "world",
        |c| rust_world(&c.world),
        |c| f.text(&world_input(&c.world)),
    );
    assert_eq!(bad, 0);
}

/// The same tasks with the optimizer off: the JIT must agree with itself.
#[test]
fn unoptimized_jit_agrees() {
    let on = check_data().entry("check_data_types", true);
    let off = check_data().entry("check_data_types", false);
    let won = check_data().entry("check_data_world", true);
    let woff = check_data().entry("check_data_world", false);
    for c in corpus().iter().step_by(8) {
        let input = types_input(&c.world);
        assert_eq!(on.text(&input), off.text(&input), "{}", c.name);
        let input = world_input(&c.world);
        assert_eq!(won.text(&input), woff.text(&input), "{}", c.name);
    }
}

/// ABBA timing of both tasks (CHECK_HARNESS_SPEED=1). Rust builds its text
/// from its own data; Vesper's time includes decoding the input.
#[test]
#[ignore]
fn speed() {
    if !check_harness::speed() {
        return;
    }
    let ft = check_data().entry("check_data_types", true);
    let fw = check_data().entry("check_data_world", true);
    let inputs: Vec<(Vec<u8>, Vec<u8>)> = corpus()
        .iter()
        .map(|c| (types_input(&c.world), world_input(&c.world)))
        .collect();
    let encode_time = time(|| {
        for c in corpus() {
            std::hint::black_box((types_input(&c.world), world_input(&c.world)));
        }
    });
    eprintln!(
        "encode both inputs of {} worlds: {encode_time:?}",
        inputs.len()
    );
    abba(
        "types",
        &mut || {
            time(|| {
                corpus()
                    .iter()
                    .for_each(|c| drop(std::hint::black_box(rust_types(&c.world))))
            })
        },
        &mut || {
            time(|| {
                inputs
                    .iter()
                    .for_each(|(t, _)| drop(std::hint::black_box(ft.words(t))))
            })
        },
    );
    abba(
        "world",
        &mut || {
            time(|| {
                corpus()
                    .iter()
                    .for_each(|c| drop(std::hint::black_box(rust_world(&c.world))))
            })
        },
        &mut || {
            time(|| {
                inputs
                    .iter()
                    .for_each(|(_, w)| drop(std::hint::black_box(fw.words(w))))
            })
        },
    );
}
