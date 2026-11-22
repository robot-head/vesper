//! The word stream of the trait declaration pass (self-hosting SH2 lane 13,
//! #1185): `pact_frontend`'s declaration pass from `@vesper/check-pact`.
//!
//! [`declaration_input`] encodes the module the pass collects from (after
//! the function-object, `dyn` and builtin-trait lowerings) and the world of
//! its module without trait declarations, in the format of
//! `@vesper/check-data`'s `read_ctx` followed by the declaration section
//! (`read_decls` in `packages/vesper-check-pact/src/decl.ves`).
//! [`decode_declaration_text`] reads the answer (one `CODE START END
//! MESSAGE` line per diagnostic) back into Rust diagnostics.
//!
//! The hybrid build (crates/vesper-hybrid/src/hybrid.rs) includes this file
//! beside `check_words.rs`, and so does tests/self_host_check_pact.rs, which
//! checks the pass against Rust over the corpus, the checker's trait tests
//! and their mutants.
#![allow(dead_code)]

use super::check_words::{Enc, Words, encodable_with, mode_word, pass_word};
use vesper::ast::{ConformanceKind, Diagnostic, Function, Generic, Item, Module, TraitRef, Type};
use vesper::check::World;

pub fn span(t: &mut Vec<u64>, s: &std::ops::Range<usize>) {
    t.extend([s.start as u64, s.end as u64]);
}

pub fn generics(e: &mut Enc, t: &mut Vec<u64>, gs: &[Generic]) {
    t.push(gs.len() as u64);
    for g in gs {
        t.push(e.name(&g.name));
        t.push(mode_word(g.bound));
        t.push(g.row as u64);
    }
}

pub fn trait_ref(e: &mut Enc, t: &mut Vec<u64>, r: &TraitRef) {
    t.push(e.opt_name(&r.package));
    t.push(e.name(&r.name));
    t.push(r.args.len() as u64);
    for a in &r.args {
        t.push(e.ty(a));
    }
    span(t, &r.span);
}

/// A signature; effect operations and trait methods have no body, and a
/// conformance method's body is not read by the pass.
pub fn signature(e: &mut Enc, t: &mut Vec<u64>, f: &Function) {
    t.push(e.name(&f.name));
    span(t, &f.span);
    generics(e, t, &f.generics);
    t.push(f.params.len() as u64);
    for p in &f.params {
        t.push(e.name(&p.name));
        t.push(pass_word(p.pass));
        t.push(p.projections.len() as u64);
        for list in &p.projections {
            t.push(list.len() as u64);
            for n in list {
                t.push(e.name(n));
            }
        }
        t.push(e.ty(&p.ty));
        span(t, &p.span);
    }
    t.push(e.ty(&f.result));
    t.push(e.ty(&Type::Row(f.effects.clone())));
}

/// The declaration section (`read_decls` in decl.ves).
pub fn encode_declarations(
    e: &mut Enc,
    t: &mut Vec<u64>,
    m: &Module,
    base: &World,
    identity: &str,
) {
    t.push(e.name(identity));
    t.push(base.functions.len() as u64);
    for (name, f) in &base.functions {
        t.push(e.name(name));
        generics(e, t, &f.generics);
    }
    let traits: Vec<_> = m
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Trait(t) => Some(t),
            _ => None,
        })
        .collect();
    t.push(traits.len() as u64);
    for tr in traits {
        t.push(e.name(&tr.name));
        span(t, &tr.span);
        generics(e, t, &tr.generics);
        t.push(tr.assoc.len() as u64);
        for a in &tr.assoc {
            t.push(e.name(&a.name));
            t.push(mode_word(a.bound));
            span(t, &a.span);
        }
        t.push(tr.methods.len() as u64);
        for f in &tr.methods {
            signature(e, t, f);
        }
    }
    let confs: Vec<_> = m
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Conformance(c) => Some(c),
            _ => None,
        })
        .collect();
    t.push(confs.len() as u64);
    for c in confs {
        match &c.kind {
            ConformanceKind::Canonical => t.extend([0, e.name("")]),
            ConformanceKind::Model { name, .. } => t.extend([1, e.name(name)]),
        }
        generics(e, t, &c.generics);
        trait_ref(e, t, &c.trait_ref);
        t.push(e.ty(&c.self_ty));
        t.push(c.where_clauses.len() as u64);
        for w in &c.where_clauses {
            t.push(e.ty(&w.ty));
            trait_ref(e, t, &w.trait_ref);
        }
        t.push(c.assoc.len() as u64);
        for a in &c.assoc {
            t.push(e.name(&a.name));
            t.push(e.ty(&a.ty));
            span(t, &a.span);
        }
        t.push(c.methods.len() as u64);
        for f in &c.methods {
            signature(e, t, f);
        }
        span(t, &c.span);
    }
    t.push(m.model_uses.len() as u64);
    for u in &m.model_uses {
        t.push(e.name(&u.model));
        span(t, &u.span);
    }
    t.push(m.trait_bounds.len() as u64);
    for b in &m.trait_bounds {
        t.push(e.name(&b.param));
        trait_ref(e, t, &b.trait_ref);
        span(t, &b.span);
        t.push(e.name(&b.owner));
    }
    let items: Vec<_> = m
        .items
        .iter()
        .filter(|i| matches!(i, Item::Adt(_) | Item::Effect(_)))
        .collect();
    t.push(items.len() as u64);
    for i in items {
        match i {
            Item::Adt(a) => {
                t.push(1);
                t.push(e.name(&a.name));
                span(t, &a.span);
                let fields: Vec<_> = a.variants.iter().flat_map(|v| &v.fields).collect();
                t.push(fields.len() as u64);
                for f in fields {
                    t.push(e.ty(&f.ty));
                }
            }
            Item::Effect(d) => {
                t.push(0);
                t.push(e.name(&d.name));
                span(t, &d.span);
                t.push(d.ops.len() as u64);
                for f in &d.ops {
                    assert!(f.body.is_none(), "effect operation with a body");
                    signature(e, t, f);
                }
            }
            _ => unreachable!(),
        }
    }
}

/// The pass's input for `m` (the prepared module), `base` (the world of
/// `m` without trait declarations) and the package identity as `DefPath`
/// prints it, or `None` when a literal does not fit the decoder (a sign and
/// a 64-bit magnitude).
pub fn declaration_input(m: &Module, base: &World, identity: &str) -> Option<Vec<u8>> {
    let mut e = Enc::default();
    let w = e.world(base);
    let mut t = vec![];
    encode_declarations(&mut e, &mut t, m, base, identity);
    if !e.type_list.iter().all(|ty| encodable_with(ty, true)) {
        return None;
    }
    Some(e.finish(&w, &t))
}

/// The diagnostics of the pass's text answer: one `CODE START END MESSAGE`
/// line each (the pass attaches no help or labels).
pub fn decode_declaration_text(text: &str) -> Vec<Diagnostic> {
    text.lines()
        .map(|l| {
            let mut parts = l.splitn(4, ' ');
            let code = parts.next().unwrap_or("");
            let start = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            let end = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            let message = parts.next().unwrap_or("");
            Diagnostic::new(code, message, start..end)
        })
        .collect()
}

/// [`decode_declaration_text`] of the packed text `pass_words` returns
/// (a byte length, then the bytes eight per word).
pub fn decode_declarations(words: &[u64]) -> Vec<Diagnostic> {
    decode_declaration_text(&Words::new(words).text())
}
