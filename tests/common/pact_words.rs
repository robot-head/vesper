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

/// Trailing metadata after `encode_declarations` for `session_open` (SH2 lane 13 part 2b).
#[allow(clippy::too_many_arguments)]
pub fn encode_session_meta(
    e: &mut Enc,
    t: &mut Vec<u64>,
    m: &Module,
    base: &World,
    prelude: &World,
    pkg_name: &str,
    pkg_ver: &str,
    max_depth: u64,
    max_steps: u64,
    max_fix: u64,
    fingerprint: &str,
) {
    t.push(e.name(pkg_name));
    t.push(e.name(pkg_ver));
    t.push(max_depth);
    t.push(max_steps);
    t.push(max_fix);
    t.push(e.name(fingerprint));
    t.push(prelude.adts.len() as u64);
    for name in prelude.adts.keys() {
        t.push(e.name(name));
    }
    let mut effs: Vec<(bool, &str, &[Generic])> = vec![];
    for (name, decl) in &prelude.effects {
        effs.push((false, name.as_str(), &decl.generics));
    }
    for item in &m.items {
        if let Item::Effect(d) = item
            && base.effects.contains_key(&d.name)
        {
            effs.push((true, d.name.as_str(), &d.generics));
        }
    }
    t.push(effs.len() as u64);
    for (is_local, name, gs) in effs {
        t.push(is_local as u64);
        t.push(e.name(name));
        generics(e, t, gs);
    }
}

/// Full byte input for `session_open` / `check_pact_session`.
#[allow(clippy::too_many_arguments)]
pub fn session_input(
    m: &Module,
    base: &World,
    prelude: &World,
    pkg_name: &str,
    pkg_ver: &str,
    max_depth: u64,
    max_steps: u64,
    max_fix: u64,
    fingerprint: &str,
) -> Option<Vec<u8>> {
    let mut e = Enc::default();
    let w = e.world(base);
    let mut t = vec![];
    let id = format!("{pkg_name}@{pkg_ver}");
    encode_declarations(&mut e, &mut t, m, base, &id);
    encode_session_meta(
        &mut e,
        &mut t,
        m,
        base,
        prelude,
        pkg_name,
        pkg_ver,
        max_depth,
        max_steps,
        max_fix,
        fingerprint,
    );
    if !e.type_list.iter().all(|ty| encodable_with(ty, true)) {
        return None;
    }
    Some(e.finish(&w, &t))
}

/// A `(self_ty, trait_name, args, brand)` goal or assumption tuple in [`SessionQuery`].
pub type SessionGoal = (Type, String, Vec<(bool, Type)>, Option<String>);

/// Binary query payload consumed by `session_solve` (`QuerySpec`).
pub struct SessionQuery<'a> {
    pub env_params: &'a [Generic],
    pub assumptions: &'a [SessionGoal],
    pub scopes: &'a [Vec<String>],
    pub infer: &'a [(String, u8)],
    pub goal: SessionGoal,
}

fn push_q_goal(e: &mut Enc, t: &mut Vec<u64>, g: &SessionGoal) {
    t.push(e.ty(&g.0));
    t.push(e.name(&g.1));
    t.push(g.2.len() as u64);
    for (is_row, ty) in &g.2 {
        t.push(*is_row as u64);
        t.push(e.ty(ty));
    }
    t.push(e.opt_name(&g.3));
}

pub fn encode_session_query(q: &SessionQuery<'_>) -> Vec<u8> {
    let mut e = Enc::default();
    let mut t = vec![];
    t.push(q.env_params.len() as u64);
    for g in q.env_params {
        t.push(e.name(&g.name));
        t.push(mode_word(g.bound));
        t.push(g.row as u64);
    }
    t.push(q.assumptions.len() as u64);
    for a in q.assumptions {
        push_q_goal(&mut e, &mut t, a);
    }
    t.push(q.scopes.len() as u64);
    for s in q.scopes {
        t.push(s.len() as u64);
        for m in s {
            t.push(e.name(m));
        }
    }
    t.push(q.infer.len() as u64);
    for (n, k) in q.infer {
        t.push(e.name(n));
        t.push(*k as u64);
    }
    push_q_goal(&mut e, &mut t, &q.goal);
    e.finish(&[], &t)
}

/// Decodes a single `session_solve` word vector (`Ok((conf_idx, selection))` or `Err((tag, msg))`).
pub fn decode_solve_words(words: &[u64]) -> Result<(usize, String), (u8, String)> {
    let (res, _) = decode_solve_at(words, 0);
    res
}

fn decode_packed_text_at(words: &[u64], at: usize) -> (String, usize) {
    let len = words.get(at).copied().unwrap_or(0) as usize;
    let text = Words::new(&words[at..]).text();
    let next = at + 1 + len.div_ceil(8);
    (text, next)
}

type SolveOutcome = Result<(usize, String), (u8, String)>;

fn decode_solve_at(words: &[u64], at: usize) -> (SolveOutcome, usize) {
    let status = words[at];
    let val = words[at + 1];
    let (msg, next) = decode_packed_text_at(words, at + 2);
    let res = if status == 0 {
        Ok((val as usize, msg))
    } else {
        Err((val as u8, msg))
    };
    (res, next)
}

pub struct DecodedSessionRun {
    pub diagnostics: Vec<Diagnostic>,
    pub ok: bool,
    pub revision: String,
    pub answers: Vec<Result<(usize, String), (u8, String)>>,
}

/// Decodes session diagnostics (multiline messages use `\x01` inside each record line).
pub fn decode_session_text(text: &str) -> Vec<Diagnostic> {
    decode_declaration_text(text)
        .into_iter()
        .map(|mut d| {
            if d.message.contains('\x01') {
                d.message = d.message.replace('\x01', "\n");
            }
            d
        })
        .collect()
}

pub fn decode_session_words(words: &[u64]) -> Vec<Diagnostic> {
    decode_session_text(&Words::new(words).text())
}

/// Decodes the output word slice of `check_pact_queries`.
pub fn decode_session_queries(words: &[u64]) -> DecodedSessionRun {
    let (diag_text, mut at) = decode_packed_text_at(words, 0);
    let diagnostics = decode_session_text(&diag_text);
    let ok = words.get(at).copied().unwrap_or(0) != 0;
    at += 1;
    if !ok {
        return DecodedSessionRun {
            diagnostics,
            ok: false,
            revision: String::new(),
            answers: vec![],
        };
    }
    let (revision, next) = decode_packed_text_at(words, at);
    at = next;
    let nq = words.get(at).copied().unwrap_or(0) as usize;
    at += 1;
    let mut answers = Vec::with_capacity(nq);
    for _ in 0..nq {
        let (ans, next_at) = decode_solve_at(words, at);
        answers.push(ans);
        at = next_at;
    }
    DecodedSessionRun {
        diagnostics,
        ok: true,
        revision,
        answers,
    }
}

/// Binary query payload for `check_pact_resolve`.
pub struct ResolveCallPayload<'a> {
    pub span: &'a vesper::ast::Span,
    pub function: &'a str,
    pub generics: &'a [vesper::ast::Generic],
    pub gen_bounds: &'a [Vec<(String, Vec<Type>)>],
    pub conf_bounds: &'a [(Type, String, Vec<Type>)],
    pub scopes: &'a [Vec<String>],
    pub callee: &'a str,
    pub path: bool,
    pub receiver: &'a Type,
}

/// Binary query payload for `check_pact_normalize`.
pub struct NormalizePayload<'a> {
    pub span: &'a vesper::ast::Span,
    pub function: &'a str,
    pub generics: &'a [vesper::ast::Generic],
    pub gen_bounds: &'a [Vec<(String, Vec<Type>)>],
    pub conf_bounds: &'a [(Type, String, Vec<Type>)],
    pub scopes: &'a [Vec<String>],
    pub ty: &'a Type,
}

fn push_cx_header(
    e: &mut Enc,
    t: &mut Vec<u64>,
    generics: &[vesper::ast::Generic],
    gen_bounds: &[Vec<(String, Vec<Type>)>],
    conf_bounds: &[(Type, String, Vec<Type>)],
    scopes: &[Vec<String>],
) {
    t.push(generics.len() as u64);
    for (gi, g) in generics.iter().enumerate() {
        t.push(e.name(&g.name));
        t.push(mode_word(g.bound));
        t.push(u64::from(g.row));
        let bounds = gen_bounds.get(gi).map(Vec::as_slice).unwrap_or(&[]);
        t.push(bounds.len() as u64);
        for (bt, bargs) in bounds {
            t.push(e.name(bt));
            t.push(bargs.len() as u64);
            for a in bargs {
                t.push(e.ty(a));
            }
        }
    }
    t.push(conf_bounds.len() as u64);
    for (gty, bt, bargs) in conf_bounds {
        t.push(e.ty(gty));
        t.push(e.name(bt));
        t.push(bargs.len() as u64);
        for a in bargs {
            t.push(e.ty(a));
        }
    }
    t.push(scopes.len() as u64);
    for sc in scopes {
        t.push(sc.len() as u64);
        for m in sc {
            t.push(e.name(m));
        }
    }
}

pub fn encode_resolve_call(q: &ResolveCallPayload<'_>) -> Option<Vec<u8>> {
    let mut e = Enc::default();
    let mut t = vec![];
    t.push(q.span.start as u64);
    t.push(q.span.end as u64);
    t.push(e.name(q.function));
    push_cx_header(
        &mut e,
        &mut t,
        q.generics,
        q.gen_bounds,
        q.conf_bounds,
        q.scopes,
    );
    t.push(e.name(q.callee));
    t.push(u64::from(q.path));
    t.push(e.ty(q.receiver));
    Some(e.finish(&[], &t))
}

pub fn encode_normalize(q: &NormalizePayload<'_>) -> Option<Vec<u8>> {
    let mut e = Enc::default();
    let mut t = vec![];
    t.push(q.span.start as u64);
    t.push(q.span.end as u64);
    t.push(e.name(q.function));
    push_cx_header(
        &mut e,
        &mut t,
        q.generics,
        q.gen_bounds,
        q.conf_bounds,
        q.scopes,
    );
    t.push(e.ty(q.ty));
    Some(e.finish(&[], &t))
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodedResolveOutcome {
    None,
    Ok(String),
    Err(Vec<Diagnostic>),
}

pub fn decode_resolve_outcomes(
    words: &[u64],
) -> (Vec<Diagnostic>, bool, Vec<DecodedResolveOutcome>) {
    let (diag_text, mut at) = decode_packed_text_at(words, 0);
    let diagnostics = decode_session_text(&diag_text);
    let ok = words.get(at).copied().unwrap_or(0) != 0;
    at += 1;
    if !ok {
        return (diagnostics, false, vec![]);
    }
    let nq = words.get(at).copied().unwrap_or(0) as usize;
    at += 1;
    let mut outcomes = Vec::with_capacity(nq);
    for _ in 0..nq {
        let status = words.get(at).copied().unwrap_or(0);
        at += 1;
        let (text, next_at) = decode_packed_text_at(words, at);
        at = next_at;
        let item = match status {
            0 => DecodedResolveOutcome::None,
            1 => DecodedResolveOutcome::Ok(text),
            _ => DecodedResolveOutcome::Err(decode_session_text(&text)),
        };
        outcomes.push(item);
    }
    (diagnostics, true, outcomes)
}
