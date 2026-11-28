//! The handler facts @vesper/codegen-scalar's body plans read after the
//! Place IR program (#2657): what an `Op::Handle` instruction's expression
//! holds, which the Place IR keeps on the Rust side (a pool row with no
//! label). The native walk's handled bodies and clauses are nested graphs
//! of the program (`ir::nested_name`), emitted inline where the handler is
//! and where a clause runs.

use std::collections::BTreeSet;
use vesper::ast::{ExprKind, Pattern, PatternKind, Type};
use vesper::check::World;
use vesper::ir::{Op, Program, nested_name};

/// The first word of the section: "HANDLERS" in ASCII, so that a planner
/// input without it (the hybrid adapter's) reads on to the next section.
pub const HANDLER_FACTS: u64 = u64::from_le_bytes(*b"HANDLERS");

/// A graph the program does not hold.
const NO_GRAPH: u64 = u64::MAX;

/// A pattern's kind in the facts.
const PATTERN_WILD: u64 = 0;
const PATTERN_BIND: u64 = 1;
const PATTERN_OTHER: u64 = 2;

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

fn put_pattern(out: &mut Vec<u8>, p: &Pattern) {
    match &p.kind {
        PatternKind::Wild => put(out, PATTERN_WILD),
        PatternKind::Bind(n) => {
            put(out, PATTERN_BIND);
            put_text(out, n);
        }
        _ => put(out, PATTERN_OTHER),
    }
}

/// The type name of an effect argument: a named type with no arguments,
/// else empty (no handler of the body plans matches it).
fn arg_name(t: &Type) -> &str {
    match t {
        Type::Named(n, args) if args.is_empty() => n,
        _ => "",
    }
}

/// The handler facts, after the program section:
///
/// ```text
/// HANDLER_FACTS
/// per program function, in program order:
///     n, n x handler       each `Op::Handle`, in block and instruction order
/// handler:
///     effect name text, k, k x argument type name text,
///     labelled, stateful, region            (0 or 1 each)
///     body graph                            program index, or NO_GRAPH
///     return pattern, return clause graph
///     m, m x (operation name text, p, p x pattern, clause graph)
///     f, f x free name text                 of the clauses, in name order
///     g, g x free name text                 of the whole expression
/// pattern:
///     PATTERN_WILD | PATTERN_BIND, name text | PATTERN_OTHER
/// ```
///
/// A text is its byte length, then its bytes padded to 8.
pub fn facts(_world: &World, program: &Program) -> Vec<u8> {
    let index = |name: String| {
        program
            .functions
            .keys()
            .position(|n| *n == name)
            .map_or(NO_GRAPH, |i| i as u64)
    };
    let mut out = vec![];
    put(&mut out, HANDLER_FACTS);
    for f in program.functions.values() {
        let handlers: Vec<_> = f
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .filter_map(|i| match &i.op {
                Op::Handle { expr, .. } => match &expr.kind {
                    ExprKind::Handle {
                        effect,
                        state,
                        body,
                        ret,
                        ops,
                    } => Some((expr, effect, state, body, ret, ops)),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        put(&mut out, handlers.len() as u64);
        for (expr, effect, state, body, ret, ops) in handlers {
            put_text(&mut out, &effect.name);
            put(&mut out, effect.args.len() as u64);
            for a in &effect.args {
                put_text(&mut out, arg_name(a));
            }
            put(&mut out, u64::from(effect.label.is_some()));
            put(&mut out, u64::from(state.is_some()));
            put(
                &mut out,
                u64::from(vesper::regions::block_capacity(effect, ret).is_some()),
            );
            put(&mut out, index(nested_name("handler_body", &body.span)));
            match ret.pats.as_slice() {
                [p] => put_pattern(&mut out, p),
                _ => put(&mut out, PATTERN_OTHER),
            }
            put(
                &mut out,
                index(nested_name("handler_clause", &ret.body.span)),
            );
            put(&mut out, ops.len() as u64);
            let mut free = vesper::check::clause_free_names(ret);
            for op in ops {
                put_text(&mut out, &op.name);
                put(&mut out, op.pats.len() as u64);
                for p in &op.pats {
                    put_pattern(&mut out, p);
                }
                put(
                    &mut out,
                    index(nested_name("handler_clause", &op.body.span)),
                );
                free.extend(vesper::check::clause_free_names(op));
            }
            let free: BTreeSet<String> = free;
            put(&mut out, free.len() as u64);
            for n in &free {
                put_text(&mut out, n);
            }
            let used = vesper::check::free_names(expr);
            put(&mut out, used.len() as u64);
            for n in &used {
                put_text(&mut out, n);
            }
        }
    }
    out
}
