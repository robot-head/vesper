//! The `World` facts @vesper/codegen-aggregates reads after the Place IR
//! program (`read_env`), shared by tests/self_host_codegen_aggregates.rs
//! and the native walk's body plans (#2581), which rewrite small enums
//! into words with them on the optimize path.

use std::collections::BTreeSet;
use vesper::check::World;
use vesper::ir::{Op, Program};

fn put_text(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u64).to_le_bytes());
    out.extend(s.as_bytes());
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
}

fn put(out: &mut Vec<u8>, x: u64) {
    out.extend(x.to_le_bytes());
}

/// The `World` facts the decisions read beyond check-data's world, after
/// the program section:
///
/// ```text
/// functions: n, n x (name text, quiet, fip)   quiet: empty effect row
/// element views: n, n x (span start, end)
/// per program function, in program order:
///     n, n x name text   the free names of its closure and handler bodies
/// per program function, in program order:
///     n, n x (lent, k, k x name text)   each closure instruction, in
///                        instruction order: whether it is a lent closure
///                        (§4.2) and the names its body uses
/// per program function, in program order:
///     ret, n, n x words  the ABI facts of `scalar_enums`
///                        (`aggregate_decisions::scalar_enum_facts`):
///                        whether its result travels as enum words, and
///                        for each call, whether the callee returns them
/// ```
///
/// A text is its byte length, then its bytes padded to 8.
pub fn facts(w: &World, program: &Program) -> Vec<u8> {
    let mut out = vec![];
    put(&mut out, w.functions.len() as u64);
    for (name, f) in &w.functions {
        put_text(&mut out, name);
        let quiet = f.effects.instances.is_empty() && f.effects.tail.is_none();
        put(&mut out, quiet as u64);
        put(&mut out, f.fip as u64);
    }
    put(&mut out, w.element_views.len() as u64);
    for (a, b) in w.element_views.keys() {
        put(&mut out, *a as u64);
        put(&mut out, *b as u64);
    }
    for f in program.functions.values() {
        let mut free = BTreeSet::new();
        for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
            if let Op::Closure { expr, .. } | Op::Handle { expr, .. } = &inst.op {
                free.extend(vesper::check::free_names(expr));
            }
        }
        put(&mut out, free.len() as u64);
        for n in &free {
            put_text(&mut out, n);
        }
    }
    for f in program.functions.values() {
        let closures: Vec<&vesper::ast::Expr> = f
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .filter_map(|i| match &i.op {
                Op::Closure { expr, .. } => Some(expr),
                _ => None,
            })
            .collect();
        put(&mut out, closures.len() as u64);
        for expr in closures {
            let (lent, used) = match &expr.kind {
                vesper::ast::ExprKind::Closure { params, body, .. } => {
                    let mut used = vesper::check::free_names(body);
                    for p in params {
                        used.remove(&p.name);
                    }
                    let key = vesper::ir::nested_name("closure", &body.span);
                    (w.lent_closures.contains_key(&key), used)
                }
                _ => (false, BTreeSet::new()),
            };
            put(&mut out, lent as u64);
            put(&mut out, used.len() as u64);
            for n in &used {
                put_text(&mut out, n);
            }
        }
    }
    for f in program.functions.values() {
        let (ret, calls) = vesper::native::aggregate_decisions::scalar_enum_facts(w, f);
        put(&mut out, ret as u64);
        put(&mut out, calls.len() as u64);
        for c in calls {
            put(&mut out, c as u64);
        }
    }
    out
}
