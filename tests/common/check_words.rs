//! The word streams of the checker-side swaps (self-hosting SH2 lane 5,
//! #1177): `Layout::of` from `@vesper/check-layout` and `ir_check::validate`
//! from `@vesper/ir-check`.
//!
//! Compiler data cannot cross the export ABI as Rust types, so each call
//! encodes its input as little-endian words in the format of
//! `@vesper/check-data`'s decoder (`read_ctx` in
//! `packages/vesper-check-data/src/input.ves`, written by the differential
//! harness's `Enc` in `tests/common/check_harness.rs`, which [`Enc`] here
//! copies), and reads the Vesper result back into the Rust types:
//!
//! - [`layout_input`] / [`decode_layout`]: one type and only the ADTs its
//!   layout can reach (a layout reads nothing else of the world), and the
//!   stream `put_layout_words` writes (`packages/vesper-check-layout`).
//! - [`ir_input`] / [`decode_diagnostics`]: the whole world and the lowered
//!   program (the census's IR encoding), and the diagnostics.
//!
//! The hybrid build (crates/vesper-hybrid/src/hybrid.rs) includes this file,
//! and so do `tests/hybrid_layout.rs` and `tests/hybrid_ir_check.rs`, which
//! check both directions against Rust over the corpus. It lives under
//! `tests/common/` so the root package's Bazel tests can compile it (the
//! hybrid crate already reads the root package's files).
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use vesper::ast::{
    Diagnostic, DiagnosticLabel, Effect, Effects, Mode, Pass, Pattern, PatternKind, Type,
};
use vesper::check::World;
use vesper::layout::{Layout, LayoutField, LayoutKind, LayoutVariant};

// ------------------------------------------------------------------ encoder

/// The harness encoder (`Enc` in tests/common/check_harness.rs): names and
/// types are interned as they are first written, types in post-order.
#[derive(Default)]
pub struct Enc {
    pub names: BTreeMap<String, u64>,
    pub name_list: Vec<String>,
    pub types: BTreeMap<String, u64>,
    pub type_words: Vec<u64>,
    pub type_count: u64,
}

pub fn mode_word(m: Mode) -> u64 {
    match m {
        Mode::Copy => 0,
        Mode::Affine => 1,
        Mode::Linear => 2,
    }
}

pub fn pass_word(p: Pass) -> u64 {
    match p {
        Pass::In => 0,
        Pass::Inout => 1,
        Pass::Sink => 2,
    }
}

impl Enc {
    pub fn name(&mut self, s: &str) -> u64 {
        if let Some(i) = self.names.get(s) {
            return *i;
        }
        let i = self.name_list.len() as u64;
        self.names.insert(s.into(), i);
        self.name_list.push(s.into());
        i
    }

    pub fn opt_name(&mut self, s: &Option<String>) -> u64 {
        s.as_ref().map_or(0, |s| self.name(s) + 1)
    }

    fn row(&mut self, e: &Effects, out: &mut Vec<u64>) {
        let mut words = vec![e.instances.len() as u64];
        for i in &e.instances {
            let args: Vec<u64> = i.args.iter().map(|t| self.ty(t)).collect();
            words.push(self.opt_name(&i.label));
            words.push(self.name(&i.name));
            words.push(args.len() as u64);
            words.extend(args);
        }
        words.push(self.opt_name(&e.tail));
        out.extend(words);
    }

    /// The index of type `t` (its parts are written first). `None` when a
    /// literal does not fit the decoder's sign and 64-bit magnitude.
    pub fn ty(&mut self, t: &Type) -> u64 {
        let key = format!("{t:?}");
        if let Some(i) = self.types.get(&key) {
            return *i;
        }
        let mut words = vec![];
        match t {
            Type::IntLiteral(n) => {
                words.extend([0, (*n < 0) as u64, n.unsigned_abs() as u64]);
            }
            Type::Named(n, a) => {
                let args: Vec<u64> = a.iter().map(|t| self.ty(t)).collect();
                words.extend([1, self.name(n), args.len() as u64]);
                words.extend(args);
            }
            Type::Tuple(a) => {
                let args: Vec<u64> = a.iter().map(|t| self.ty(t)).collect();
                words.extend([2, args.len() as u64]);
                words.extend(args);
            }
            Type::Function {
                params,
                result,
                effects,
                mode,
            } => {
                let ps: Vec<(u64, u64)> = params
                    .iter()
                    .map(|(p, t)| (pass_word(*p), self.ty(t)))
                    .collect();
                let r = self.ty(result);
                words.extend([3, ps.len() as u64]);
                for (p, t) in ps {
                    words.extend([p, t]);
                }
                words.push(r);
                self.row(effects, &mut words);
                words.push(mode_word(*mode));
            }
            Type::Row(e) => {
                words.push(4);
                self.row(e, &mut words);
            }
            Type::Infer => words.push(5),
        }
        let i = self.type_count;
        self.type_count += 1;
        self.types.insert(key, i);
        self.type_words.extend(words);
        i
    }

    fn spans(out: &mut Vec<u64>, spans: &BTreeSet<(usize, usize)>) {
        out.push(spans.len() as u64);
        for (a, b) in spans {
            out.extend([*a as u64, *b as u64]);
        }
    }

    fn adt(&mut self, a: &vesper::ast::Adt, out: &mut Vec<u64>) {
        out.push(self.name(&a.name));
        out.push(mode_word(a.mode));
        out.push(a.generics.len() as u64);
        for g in &a.generics {
            out.push(self.name(&g.name));
            out.push(mode_word(g.bound));
            out.push(g.row as u64);
        }
        out.push(a.is_enum as u64);
        out.push(a.tile_width.map_or(0, |x| x as u64 + 1));
        out.push((a.span == (0..0)) as u64);
        out.push(a.variants.len() as u64);
        for v in &a.variants {
            out.push(self.name(&v.name));
            out.push(v.named as u64);
            out.push(v.fields.len() as u64);
            for f in &v.fields {
                out.push(self.name(&f.name));
                out.push(self.ty(&f.ty));
                out.push(f.modality.map_or(0, |m| mode_word(m) + 1));
            }
        }
    }

    /// The whole world section (`Enc::world` of the harness).
    pub fn world(&mut self, w: &World) -> Vec<u64> {
        let mut out = vec![vesper::fip::active(w) as u64, w.adts.len() as u64];
        for a in w.adts.values() {
            self.adt(a, &mut out);
        }
        out.push(w.effects.len() as u64);
        for (name, d) in &w.effects {
            out.push(self.name(name));
            out.push(d.linear as u64);
        }
        out.push(w.types.len() as u64);
        for ((a, b), t) in &w.types {
            out.extend([*a as u64, *b as u64]);
            out.push(self.ty(t));
        }
        Self::spans(&mut out, &w.partial_ops);
        Self::spans(&mut out, &w.overflow_ops);
        out
    }

    /// A world section with only the ADTs named in `adts` (in the world's
    /// order) and empty effect, expression-type and span tables.
    pub fn adts_world(&mut self, w: &World, adts: &BTreeSet<&str>) -> Vec<u64> {
        let mut out = vec![vesper::fip::active(w) as u64, adts.len() as u64];
        for name in adts {
            self.adt(&w.adts[*name], &mut out);
        }
        out.extend([0, 0, 0, 0]);
        out
    }

    /// The whole input: names, types, the world section, then `tail`.
    pub fn finish(&self, world: &[u64], tail: &[u64]) -> Vec<u8> {
        let mut out: Vec<u8> =
            Vec::with_capacity(8 * (world.len() + tail.len() + self.type_words.len() + 1));
        let put = |out: &mut Vec<u8>, x: u64| out.extend(x.to_le_bytes());
        put(&mut out, self.name_list.len() as u64);
        for n in &self.name_list {
            put(&mut out, n.len() as u64);
            out.extend(n.as_bytes());
            while !out.len().is_multiple_of(8) {
                out.push(0);
            }
        }
        put(&mut out, self.type_count);
        for x in &self.type_words {
            put(&mut out, *x);
        }
        for x in world {
            put(&mut out, *x);
        }
        for x in tail {
            put(&mut out, *x);
        }
        out
    }
}

/// Whether the decoder can read every literal of `t` (a sign and a 64-bit
/// magnitude until 128-bit integers land, SH2 lane 2).
pub fn encodable(t: &Type) -> bool {
    encodable_with(t, false)
}

/// [`encodable`], and with `wide` also `i128` and `u128` (#1174), which
/// `@vesper/check-layout` lays out as two words (SH5 lane 6, #1329). The
/// other Vesper checkers do not handle them yet: Rust answers for them.
pub fn encodable_with(t: &Type, wide: bool) -> bool {
    let all = |ts: &[Type]| ts.iter().all(|t| encodable_with(t, wide));
    match t {
        Type::IntLiteral(n) => n.unsigned_abs() <= u64::MAX as u128,
        Type::Named(n, _) if !wide && vesper::numeric::is_wide_name(n) => false,
        Type::Named(_, a) | Type::Tuple(a) => all(a),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            params.iter().all(|(_, t)| encodable_with(t, wide))
                && encodable_with(result, wide)
                && row_encodable(effects, wide)
        }
        Type::Row(e) => row_encodable(e, wide),
        Type::Infer => true,
    }
}

fn row_encodable(e: &Effects, wide: bool) -> bool {
    e.instances
        .iter()
        .all(|i| i.args.iter().all(|t| encodable_with(t, wide)))
}

// ------------------------------------------------------------------ layout

/// The ADTs `Layout::of(ty)` can visit: every ADT named in `ty`, and in the
/// field types of those, transitively. Generic parameters are over-
/// approximated (a parameter named like an ADT adds it), which is harmless.
pub fn reachable_adts<'w>(w: &'w World, ty: &Type) -> BTreeSet<&'w str> {
    fn names<'w>(w: &'w World, t: &Type, out: &mut Vec<&'w str>) {
        match t {
            Type::Named(n, a) => {
                if let Some((k, _)) = w.adts.get_key_value(n) {
                    out.push(k.as_str());
                }
                for t in a {
                    names(w, t, out);
                }
            }
            Type::Tuple(a) => a.iter().for_each(|t| names(w, t, out)),
            // Function types are scalars and rows have no layout; their parts
            // are never laid out.
            _ => {}
        }
    }
    let mut seen = BTreeSet::new();
    let mut pending = vec![];
    names(w, ty, &mut pending);
    while let Some(n) = pending.pop() {
        if !seen.insert(n) {
            continue;
        }
        for v in &w.adts[n].variants {
            for f in &v.fields {
                names(w, &f.ty, &mut pending);
            }
        }
    }
    seen
}

/// The input of `layout_words`: the reachable ADTs and one type index.
/// `None` when a literal cannot be encoded.
pub fn layout_input(w: &World, ty: &Type) -> Option<Vec<u8>> {
    let adts = reachable_adts(w, ty);
    if !encodable_with(ty, true)
        || adts.iter().any(|a| {
            w.adts[*a]
                .variants
                .iter()
                .any(|v| v.fields.iter().any(|f| !encodable_with(&f.ty, true)))
        })
    {
        return None;
    }
    let mut e = Enc::default();
    let world = e.adts_world(w, &adts);
    let t = e.ty(ty);
    Some(e.finish(&world, &[t]))
}

/// A cursor over a result stream; reading past the end yields 0, so a
/// corrupt stream decodes to garbage the differential tests catch, never a
/// panic of the compiler.
pub struct Words<'a> {
    words: &'a [u64],
    pos: usize,
}

impl<'a> Words<'a> {
    pub fn new(words: &'a [u64]) -> Self {
        Self { words, pos: 0 }
    }

    pub fn next(&mut self) -> u64 {
        let x = self.words.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        x
    }

    pub fn count(&mut self) -> usize {
        // A count never exceeds the words left; clamp a corrupt one.
        (self.next() as usize).min(self.words.len().saturating_sub(self.pos))
    }

    /// A byte length, then the bytes eight per word, little-endian.
    pub fn text(&mut self) -> String {
        let len = (self.next() as usize).min(8 * self.words.len().saturating_sub(self.pos));
        let mut bytes = Vec::with_capacity(len + 8);
        while bytes.len() < len {
            bytes.extend_from_slice(&self.next().to_le_bytes());
        }
        bytes.truncate(len);
        String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }

    pub fn done(&self) -> bool {
        self.pos == self.words.len()
    }
}

fn pass_of(x: u64) -> Pass {
    match x {
        1 => Pass::Inout,
        2 => Pass::Sink,
        _ => Pass::In,
    }
}

fn mode_of(x: u64) -> Mode {
    match x {
        1 => Mode::Affine,
        2 => Mode::Linear,
        _ => Mode::Copy,
    }
}

fn read_row(r: &mut Words, names: &[String], types: &[Type]) -> Effects {
    let name = |i: u64| names.get(i as usize).cloned().unwrap_or_default();
    let opt = |i: u64| (i != 0).then(|| name(i - 1));
    let ty = |i: u64| types.get(i as usize).cloned().unwrap_or(Type::Infer);
    let n = r.count();
    let mut instances = Vec::with_capacity(n);
    for _ in 0..n {
        let label = opt(r.next());
        let effect = name(r.next());
        let k = r.count();
        let args = (0..k).map(|_| ty(r.next())).collect();
        instances.push(Effect {
            label,
            name: effect,
            args,
        });
    }
    let tail = opt(r.next());
    Effects { instances, tail }
}

/// Reads one type of the result's type table (its parts are earlier).
fn read_type(r: &mut Words, names: &[String], types: &[Type]) -> Type {
    let name = |i: u64| names.get(i as usize).cloned().unwrap_or_default();
    let ty = |i: u64| types.get(i as usize).cloned().unwrap_or(Type::Infer);
    match r.next() {
        0 => {
            let negative = r.next() != 0;
            let magnitude = r.next() as i128;
            Type::IntLiteral(if negative { -magnitude } else { magnitude })
        }
        1 => {
            let n = name(r.next());
            let k = r.count();
            Type::Named(n, (0..k).map(|_| ty(r.next())).collect())
        }
        2 => {
            let k = r.count();
            Type::Tuple((0..k).map(|_| ty(r.next())).collect())
        }
        3 => {
            let k = r.count();
            let params = (0..k)
                .map(|_| {
                    let p = pass_of(r.next());
                    (p, ty(r.next()))
                })
                .collect();
            let result = Box::new(ty(r.next()));
            let effects = read_row(r, names, types);
            let mode = mode_of(r.next());
            Type::Function {
                params,
                result,
                effects,
                mode,
            }
        }
        4 => Type::Row(read_row(r, names, types)),
        _ => Type::Infer,
    }
}

fn read_layout(r: &mut Words, names: &[String], types: &[Type], depth: usize) -> Layout {
    let name = |i: u64| names.get(i as usize).cloned().unwrap_or_default();
    let ty = |i: u64| types.get(i as usize).cloned().unwrap_or(Type::Infer);
    let t = ty(r.next());
    let size = r.next() as u32;
    let kind = r.next();
    // Vesper refuses layouts deeper than 128 levels; a deeper stream is
    // corrupt.
    if depth > 256 {
        return Layout {
            ty: t,
            size,
            kind: LayoutKind::Scalar,
        };
    }
    let fields = |r: &mut Words| -> Vec<LayoutField> {
        let n = r.count();
        (0..n)
            .map(|_| {
                let fname = name(r.next());
                let fty = ty(r.next());
                let offset = r.next() as i64 as i32;
                let layout = Box::new(read_layout(r, names, types, depth + 1));
                LayoutField {
                    name: fname,
                    ty: fty,
                    offset,
                    layout,
                }
            })
            .collect()
    };
    let kind = match kind {
        1 => LayoutKind::Product(fields(r)),
        2 => {
            let n = r.count();
            LayoutKind::Enum(
                (0..n)
                    .map(|_| {
                        let vname = name(r.next());
                        LayoutVariant {
                            name: vname,
                            fields: fields(r),
                        }
                    })
                    .collect(),
            )
        }
        _ => LayoutKind::Scalar,
    };
    Layout { ty: t, size, kind }
}

/// Reads the stream of `put_layout_words` back into `Layout::of`'s result.
pub fn decode_layout(words: &[u64]) -> Result<Layout, String> {
    let mut r = Words::new(words);
    if r.next() != 1 {
        return Err(r.text());
    }
    let n = r.count();
    let names: Vec<String> = (0..n).map(|_| r.text()).collect();
    let k = r.count();
    let mut types = Vec::with_capacity(k);
    for _ in 0..k {
        let t = read_type(&mut r, &names, &types);
        types.push(t);
    }
    Ok(read_layout(&mut r, &names, &types, 0))
}

// ------------------------------------------------------------------ IR check

fn path_words(e: &mut Enc, path: &[String], out: &mut Vec<u64>) {
    out.push(path.len() as u64);
    for s in path {
        out.push(e.name(s));
    }
}

fn pattern_words(e: &mut Enc, p: &Pattern, out: &mut Vec<u64>) {
    let (s0, s1) = (p.span.start as u64, p.span.end as u64);
    match &p.kind {
        PatternKind::Wild => out.extend([0, s0, s1]),
        PatternKind::Bind(n) => {
            out.extend([1, s0, s1]);
            out.push(e.name(n));
        }
        PatternKind::Tuple(ps) => {
            out.extend([2, s0, s1, ps.len() as u64]);
            for c in ps {
                pattern_words(e, c, out);
            }
        }
        PatternKind::Constructor { name, fields, .. } => {
            // A multi-component path does not resolve (`World::constructor`),
            // so binding skips it (3); its fields still feed the origin index.
            let (kind, qual, short) = if name.matches("::").count() > 1 {
                (3, 0, 0)
            } else {
                match name.split_once("::") {
                    Some((q, s)) => (4, e.name(q) + 1, e.name(s)),
                    None => (4, 0, e.name(name)),
                }
            };
            out.extend([kind, s0, s1, qual, short, fields.len() as u64]);
            for (f, c) in fields {
                out.push(e.name(f));
                pattern_words(e, c, out);
            }
        }
        _ => out.extend([5, s0, s1]),
    }
}

fn args_words(args: &[vesper::ir::Argument], out: &mut Vec<u64>) {
    out.push(args.len() as u64);
    for a in args {
        out.push(pass_word(a.pass));
        out.push(a.value as u64);
    }
}

fn effectful_row(w: &World, row: &Effects) -> bool {
    row.tail.is_some()
        || row
            .instances
            .iter()
            .any(|e| w.effects.get(&e.name).is_some_and(|d| !d.linear))
}

/// Whether every type the IR check reads can be encoded.
fn program_encodable(w: &World, p: &vesper::ir::Program) -> bool {
    // Element view matches (#1173) are not in `@vesper/ir-check` yet: Rust
    // checks the worlds that have them.
    w.element_views.is_empty()
        && w.adts.values().all(|a| {
            a.variants
                .iter()
                .all(|v| v.fields.iter().all(|f| encodable(&f.ty)))
        })
        && w.types.values().all(encodable)
        && p.functions.values().all(|f| {
            f.params.iter().all(|p| encodable(&p.ty))
                && f.values.values().all(encodable)
                && f.blocks
                    .iter()
                    .all(|b| b.instructions.iter().all(|i| encodable(&i.ty)))
        })
}

/// The input of `ir_check_words`: the world, then the census's IR encoding
/// (name ranks in `BTreeMap<String, _>` order, the non-linear effects, and
/// each function's parameters, bounds, captures, block parameters and
/// blocks). The world queries the Vesper `World` cannot answer yet are
/// instruction flags (census gap C10): whether an instruction can abort,
/// whether a call or handler is effectful, state actions, evidence and
/// `Never` results. `None` when a literal cannot be encoded.
pub fn ir_input(w: &World, program: &vesper::ir::Program) -> Option<Vec<u8>> {
    use vesper::ir::{Op, Terminator};
    if !program_encodable(w, program) {
        return None;
    }
    let mut e = Enc::default();
    let mut body = vec![];
    let nonlinear: Vec<u64> = w
        .effects
        .iter()
        .filter(|(_, d)| !d.linear)
        .map(|(n, _)| n.clone())
        .collect::<Vec<_>>()
        .iter()
        .map(|n| e.name(n))
        .collect();
    body.push(nonlinear.len() as u64);
    body.extend(nonlinear);
    body.push(program.functions.len() as u64);
    for f in program.functions.values() {
        body.push(f.params.len() as u64);
        // A lent closure's body (§4.2, #1242) borrows its captures `in`.
        let lent = w.lent_closures.get(&f.name);
        for p in &f.params {
            body.push(e.name(&p.name));
            let borrowed =
                p.pass == Pass::Sink && lent.is_some_and(|names| names.contains(&p.name));
            body.push(pass_word(if borrowed { Pass::In } else { p.pass }));
            body.push(e.ty(&p.ty));
            body.push(p.span.start as u64);
            body.push(p.span.end as u64);
        }
        body.push(f.bounds.len() as u64);
        for (n, m) in &f.bounds {
            body.push(e.name(n));
            body.push(mode_word(*m));
        }
        body.push(f.external_captures.len() as u64);
        for n in &f.external_captures {
            body.push(e.name(n));
        }
        let params: Vec<u32> = f
            .blocks
            .iter()
            .flat_map(|b| b.params.iter().copied())
            .collect();
        body.push(params.len() as u64);
        for id in params {
            body.push(id as u64);
            let t = f.values.get(&id).cloned().unwrap_or(Type::Infer);
            body.push(e.ty(&t));
        }
        body.push(f.blocks.len() as u64);
        for b in &f.blocks {
            body.push(b.entry_token as u64);
            body.push(b.incoming_tokens.len() as u64);
            for (s, t) in &b.incoming_tokens {
                body.push(*s as u64);
                body.push(*t as u64);
            }
            body.push(b.outgoing_token as u64);
            body.push(b.params.len() as u64);
            for p in &b.params {
                body.push(*p as u64);
            }
            body.push(b.instructions.len() as u64);
            for i in &b.instructions {
                let span = (i.span.start, i.span.end);
                let mut flags = 0u64;
                if i.fallible || w.partial_ops.contains(&span) || w.overflow_ops.contains(&span) {
                    flags |= 1;
                }
                match &i.op {
                    Op::Call { callee, .. } => {
                        let effectful = if let Some(f) = w.functions.get(callee) {
                            let vars = w.calls.get(&span).cloned().unwrap_or_default();
                            effectful_row(w, &vesper::ast::substitute_effects(&f.effects, &vars))
                        } else {
                            callee
                                .split_once("::")
                                .and_then(|(x, _)| w.effects.get(x))
                                .is_some_and(|x| !x.linear)
                        };
                        if effectful {
                            flags |= 2;
                        }
                    }
                    Op::Handle { expr, .. }
                        if w.handler_effects
                            .get(&(expr.span.start, expr.span.end))
                            .is_some_and(|r| effectful_row(w, r)) =>
                    {
                        flags |= 2;
                    }
                    _ => {}
                }
                if vesper::ir::state_action(i) {
                    flags |= 4;
                }
                if i.token.is_some() {
                    flags |= 8;
                }
                if i.ty.name() == "Never" {
                    flags |= 16;
                }
                let (tin, tout) = i.token.unwrap_or((0, 0));
                body.push(i.dest as u64);
                body.push(e.ty(&i.ty));
                body.push(span.0 as u64);
                body.push(span.1 as u64);
                body.push(flags);
                body.push(tin as u64);
                body.push(tout as u64);
                let mut ops = vec![];
                let code = match &i.op {
                    Op::Constant(_) | Op::Unit | Op::Loop => 0,
                    Op::Read {
                        root,
                        path,
                        pass,
                        projections,
                    } => {
                        ops.push(e.name(root));
                        ops.push(pass_word(*pass));
                        ops.push(w.functions.contains_key(root) as u64);
                        path_words(&mut e, path, &mut ops);
                        ops.push(projections.len() as u64);
                        for p in projections {
                            path_words(&mut e, p, &mut ops);
                        }
                        1
                    }
                    Op::Bind { pattern, value } => {
                        ops.push(*value as u64);
                        pattern_words(&mut e, pattern, &mut ops);
                        2
                    }
                    Op::Write { place, op, value } => {
                        ops.push(*value as u64);
                        let mut parts = place.split('.');
                        ops.push(e.name(parts.next().unwrap_or("")));
                        let rest: Vec<String> = parts.map(str::to_owned).collect();
                        path_words(&mut e, &rest, &mut ops);
                        ops.push((op == "=") as u64);
                        3
                    }
                    Op::Unary(_, v) | Op::Exit { value: v, .. } => {
                        ops.push(*v as u64);
                        4
                    }
                    Op::Binary(op, a, b) => {
                        ops.push(vesper::strings::is_comparison(op) as u64);
                        ops.push(*a as u64);
                        ops.push(*b as u64);
                        5
                    }
                    Op::Tuple(vs) => {
                        ops.push(vs.len() as u64);
                        ops.extend(vs.iter().map(|v| *v as u64));
                        6
                    }
                    Op::Data(_, fs) => {
                        ops.push(fs.len() as u64);
                        ops.extend(fs.iter().map(|(_, v)| *v as u64));
                        6
                    }
                    Op::Vector { args, .. } => {
                        ops.push(args.len() as u64);
                        ops.extend(args.iter().map(|v| *v as u64));
                        7
                    }
                    Op::Tiled { args, .. } | Op::Seq { args, .. } | Op::Fip { args, .. } => {
                        args_words(args, &mut ops);
                        8
                    }
                    Op::Project(v, _) => {
                        ops.push(*v as u64);
                        9
                    }
                    Op::Index(v, n) => {
                        ops.push(*v as u64);
                        ops.push(*n as u64);
                        10
                    }
                    Op::Call { args, .. } => {
                        args_words(args, &mut ops);
                        11
                    }
                    Op::Indirect { callee, args } => {
                        ops.push(*callee as u64);
                        args_words(args, &mut ops);
                        12
                    }
                    Op::Try(v) => {
                        ops.push(*v as u64);
                        13
                    }
                    Op::Closure { moves, .. } | Op::Handle { moves, .. } => {
                        ops.push(moves.len() as u64);
                        for p in moves {
                            ops.push(e.name(&p.root));
                            path_words(&mut e, &p.path, &mut ops);
                        }
                        let sv = if let Op::Handle { state_value, .. } = &i.op {
                            *state_value
                        } else {
                            None
                        };
                        ops.push(sv.is_some() as u64);
                        ops.push(sv.unwrap_or(0) as u64);
                        if matches!(i.op, Op::Handle { .. }) {
                            15
                        } else {
                            14
                        }
                    }
                };
                body.push(code);
                body.extend(ops);
            }
            match &b.terminator {
                Terminator::Return(v) => {
                    body.push(0);
                    body.push(*v as u64);
                }
                Terminator::Jump { block, args } => {
                    body.push(1);
                    body.push(*block as u64);
                    body.push(args.len() as u64);
                    body.extend(args.iter().map(|v| *v as u64));
                }
                Terminator::Branch { yes, no, .. } => {
                    body.push(2);
                    body.push(*yes as u64);
                    body.push(*no as u64);
                }
                Terminator::Match { value, arms } => {
                    body.push(3);
                    body.push(arms.len() as u64);
                    body.extend(arms.iter().map(|(_, b)| *b as u64));
                    // The scrutinee (borrowed matching, #1149).
                    body.push(*value as u64);
                }
                Terminator::Unreachable => body.push(4),
            }
        }
    }
    let world = e.world(w);
    // Name ranks in BTreeMap<String, _> order (the Rust state maps' order).
    let mut sorted: Vec<(&String, usize)> = e
        .name_list
        .iter()
        .enumerate()
        .map(|(i, n)| (n, i))
        .collect();
    sorted.sort();
    let mut rank = vec![0u64; sorted.len()];
    for (r, (_, i)) in sorted.iter().enumerate() {
        rank[*i] = r as u64;
    }
    let mut tail = rank;
    tail.extend(body);
    Some(e.finish(&world, &tail))
}

/// Reads `ir_check_words`' result: a count, then per diagnostic its code,
/// message, span start and end, help (empty for none), and related labels
/// (count, then start, end and message each).
pub fn decode_diagnostics(words: &[u64]) -> Vec<Diagnostic> {
    let mut r = Words::new(words);
    let n = r.count();
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let code = r.text();
        let message = r.text();
        let start = r.next() as usize;
        let end = r.next() as usize;
        let help = r.text();
        let k = r.count();
        let related = (0..k)
            .map(|_| {
                let s = r.next() as usize;
                let e = r.next() as usize;
                DiagnosticLabel {
                    span: s..e,
                    message: r.text(),
                }
            })
            .collect();
        out.push(Diagnostic {
            code,
            message,
            span: start..end,
            related,
            help: (!help.is_empty()).then_some(help),
        });
    }
    out
}
