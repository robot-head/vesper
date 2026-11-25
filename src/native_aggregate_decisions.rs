//! The aggregate decisions of the native optimize path as text, for the
//! self-hosting differential of `@vesper/codegen-aggregates` (SH4 lane 8,
//! #1282; tests/self_host_codegen_aggregates.rs).
//!
//! Read-only: this module calls the decision functions of
//! `native_aggregates.rs` ([`super::aggregates::split_locals`],
//! [`super::aggregates::plan`], [`super::aggregates::split_sources`],
//! [`super::aggregates::split_words`], [`super::aggregates::plain_data`])
//! the way `Codegen` calls them for a top-level graph with no generic
//! arguments, and prints what they decide. It changes no lowering.
use super::aggregates::{self, Home};
use crate::check::World;
use crate::ir::{FunctionIr, Program};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The drop facts of `ty` (`fip::needs_drop`, `may_need_drop`,
/// `elem_needs_drop`, `type_contains_box` and `type_is_recursive`, which
/// picks the drop and equality helpers), or `None` when all are false.
fn drop_facts(world: &World, ty: &crate::ast::Type) -> Option<String> {
    let ty = crate::numeric::default_type(ty);
    let facts = [
        crate::fip::needs_drop(world, &ty),
        crate::fip::may_need_drop(world, &ty),
        crate::fip::elem_needs_drop(world, &ty),
        crate::fip::type_contains_box(world, &ty),
        super::type_is_recursive(world, &ty),
    ];
    if !facts.contains(&true) {
        return None;
    }
    let mut out = String::new();
    for (label, x) in ["nd", "mnd", "en", "cb", "rec"].iter().zip(facts) {
        let _ = write!(out, " {label}={}", x as u8);
    }
    Some(out)
}

fn path_text(path: &[String]) -> String {
    if path.is_empty() {
        "-".into()
    } else {
        path.join(".")
    }
}

/// One graph's decisions, one per line, in a fixed order:
///
/// - `local L pd=P sw=N` for every local: `plain_data` of its type (0/1)
///   and the word count of `split_words` (`-` when it is not splittable);
/// - `drops L ..` and `vdrops vV ..`: the drop facts ([`drop_facts`]) of
///   each local's and value's type, when one holds;
/// - `seqdrop L KIND`: the slots `fip::seq_drop_plan` frees, with what;
/// - `replace B I`: the writes `fip::replace_drops` lets free what they
///   replace;
/// - `temp B I vV` (`I` is `entry` at a block's entry): the owned
///   temporaries `fip::temporary_drops` drops;
/// - `split L P1 P2 ..` for every local `split_locals` splits, with the
///   field paths its uses read (`-` reads it whole);
/// - `home vV result`, `home vV field vP OFFSET` or
///   `home vV place PLACE BLOCK INDEX` for every value `plan` gives a home;
/// - `source vV L` for every read `split_sources` lends to split local `L`.
pub fn function_decisions(world: &World, f: &FunctionIr, out: &mut String) {
    let none = BTreeMap::new();
    let _ = writeln!(out, "fn {}", f.name);
    for (n, t) in &f.locals {
        let ty = crate::numeric::default_type(t);
        let pd = aggregates::plain_data(world, &ty) as u8;
        let sw =
            aggregates::split_words(world, &ty).map_or("-".to_owned(), |w| w.len().to_string());
        let _ = writeln!(out, "local {n} pd={pd} sw={sw}");
        if let Some(d) = drop_facts(world, t) {
            let _ = writeln!(out, "drops {n}{d}");
        }
    }
    for (v, t) in &f.values {
        if let Some(d) = drop_facts(world, t) {
            let _ = writeln!(out, "vdrops v{v}{d}");
        }
    }
    for (n, k) in crate::fip::seq_drop_plan(world, f) {
        let k = match k {
            crate::fip::SeqDrop::Seq => "seq",
            crate::fip::SeqDrop::Str => "str",
            crate::fip::SeqDrop::Owned => "owned",
            crate::fip::SeqDrop::Fields => "fields",
        };
        let _ = writeln!(out, "seqdrop {n} {k}");
    }
    for (b, i) in crate::fip::replace_drops(world, f, None) {
        let _ = writeln!(out, "replace {b} {i}");
    }
    for ((b, i), vs) in crate::fip::temporary_drops(world, f) {
        for v in vs {
            if i == crate::fip::AT_BLOCK_ENTRY {
                let _ = writeln!(out, "temp {b} entry v{v}");
            } else {
                let _ = writeln!(out, "temp {b} {i} v{v}");
            }
        }
    }
    let split = aggregates::split_locals(world, f, &none);
    for (n, reads) in &split {
        let _ = write!(out, "split {n}");
        for r in reads {
            let _ = write!(out, " {}", path_text(r));
        }
        out.push('\n');
    }
    let homes = aggregates::plan(world, f, &none, false, &split);
    for (v, h) in &homes {
        let _ = match h {
            Home::Result => writeln!(out, "home v{v} result"),
            Home::Field { parent, offset, .. } => {
                writeln!(out, "home v{v} field v{parent} {offset}")
            }
            Home::Place { place, at } => {
                writeln!(out, "home v{v} place {place} {} {}", at.0, at.1)
            }
        };
    }
    for (v, n) in aggregates::split_sources(f, &split) {
        let _ = writeln!(out, "source v{v} {n}");
    }
    scalar_enum_decisions(world, f, out);
}

/// The ABI facts `scalar_enums` reads from the `Codegen`, for a top-level
/// graph of an optimized build whose entry is `main`: whether the graph's
/// own result travels as the words of its enum shape (`returns`), and for
/// each `Op::Call` instruction, in program order, whether the callee
/// returns the words of the call's result shape (`call_words`; `false`
/// when the result has no shape).
pub fn scalar_enum_facts(world: &World, f: &FunctionIr) -> (bool, Vec<bool>) {
    let none = BTreeMap::new();
    let returns = world.functions.contains_key(&f.name)
        && result_words(world, &f.name, &none).is_some_and(|words| {
            let ty = crate::numeric::default_type(&f.result);
            aggregates::enum_shape(world, &ty).is_some_and(|s| s.words.len() as u32 == words)
        });
    let mut calls = vec![];
    for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
        let crate::ir::Op::Call { callee, types, .. } = &inst.op else {
            continue;
        };
        let ty = f.values.get(&inst.dest).map(crate::numeric::default_type);
        let shape = ty.and_then(|t| aggregates::enum_shape(world, &t));
        calls.push(shape.is_some_and(|shape| call_words(world, callee, types, &shape)));
    }
    (returns, calls)
}

/// The register-result words of instance `name` with `vars` (`None` for
/// the entry `main`, a boundary with the default convention).
fn result_words(
    world: &World,
    name: &str,
    vars: &BTreeMap<String, crate::ast::Type>,
) -> Option<u32> {
    if name == "main" {
        return None;
    }
    let g = super::concrete(world, name, vars).ok()?;
    super::NativeAbi::internal(world, &g).ok()?.result_words
}

/// `Codegen::ir_graph_at`'s `call_words` for a top-level graph.
fn call_words(
    world: &World,
    callee: &str,
    types: &[crate::ast::Type],
    shape: &aggregates::EnumShape,
) -> bool {
    let Some(g) = world.functions.get(callee) else {
        return false;
    };
    if super::specialized_function(world, g) {
        return true;
    }
    if world.dyn_objects.stubs.contains_key(callee) {
        return false;
    }
    let mut vars = BTreeMap::new();
    for (p, t) in g.generics.iter().zip(types) {
        vars.insert(p.name.clone(), crate::numeric::default_type(t));
    }
    result_words(world, callee, &vars) == Some(shape.words.len() as u32)
}

/// The small enums `scalar_enums` keeps as words (#1330), with the
/// [`scalar_enum_facts`]: `enum vV W..` for each value and the values of
/// its words, `enumlocal L` for each local, `enumcall vV` for each call
/// whose result the graph keeps as words and `enumret vV` for each
/// returned word value. Nothing when the graph keeps its enums in memory.
fn scalar_enum_decisions(world: &World, f: &FunctionIr, out: &mut String) {
    let (returns, _) = scalar_enum_facts(world, f);
    let returns = returns
        .then(|| aggregates::enum_shape(world, &crate::numeric::default_type(&f.result)))
        .flatten();
    let none = BTreeMap::new();
    let Some(s) = aggregates::scalar_enums(
        world,
        f,
        &none,
        returns.as_ref(),
        &|callee, types, shape| call_words(world, callee, types, shape),
        &std::collections::BTreeSet::new(),
    ) else {
        return;
    };
    for (v, ws) in &s.words {
        let _ = write!(out, "enum v{v}");
        for w in ws {
            let _ = write!(out, " v{w}");
        }
        out.push('\n');
    }
    for n in &s.locals {
        let _ = writeln!(out, "enumlocal {n}");
    }
    for v in s.calls.keys() {
        let _ = writeln!(out, "enumcall v{v}");
    }
    for v in s.returns.keys() {
        let _ = writeln!(out, "enumret v{v}");
    }
}

/// [`function_decisions`] of every function of `program` the native
/// optimize path plans (every function but `fip` ones), in name order.
pub fn program_decisions(world: &World, program: &Program) -> String {
    let mut out = String::new();
    for f in program.functions.values() {
        if world.functions.get(&f.name).is_some_and(|src| src.fip) {
            continue;
        }
        function_decisions(world, f, &mut out);
    }
    out
}
