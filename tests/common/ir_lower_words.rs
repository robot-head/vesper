//! The input of `@vesper/place-ir-lower` (self-hosting SH3 lane 13, #1252):
//! a checked world and its function bodies as words, for the Vesper port of
//! `ir::lower_world` (packages/vesper-place-ir-lower).
//!
//! The stream is `@vesper/check-data`'s names, types and world section
//! ([`Enc`] of tests/common/check_words.rs, which carries `world.types`,
//! `partial_ops` and `overflow_ops`), then the lowering section:
//!
//! ```text
//! pool:      n                          (closure and handler payloads)
//! functions: n, n x function            (world.functions, in key order)
//! function:  key, name, has body, body expr (or NONE),
//!            np, np x param,
//!            result type, effects (a Row type),
//!            ng, ng x (name, bound mode, row),
//!            ncap, ncap x (label, effect name)   (labeled effect instances)
//! effects:   n, n x (name, linear, ng, ng generic names, nops,
//!            nops x (op name, np, np x param, result type))
//! param:     name, pass, nproj, nproj x path, type, start, end
//! patterns:  n, n x pattern, post-order (children first)
//! pattern:   kind, start, end, then
//!            0 wild | 1 bind: name | 2 literal: literal |
//!            3 tuple: n, n pattern ids |
//!            4 constructor: name, named, n, n x (field name, pattern id)
//! exprs:     n, n x expr, post-order (children first)
//! expr:      kind (`X_*`), start, end, then the payload of the kind
//! path:      k, k names
//! literal:   0 int: low word, high word | 1 float: bits, 0 |
//!            2 bool: value, 0 | 3 string: name, 0
//! ```
//!
//! Each expression kind's payload is in [`Lw::expr`]. Expression and
//! pattern references are row indices; `NONE` is `u64::MAX`.
//!
//! **What stays on the Rust side.** The closure and handler expressions
//! `Op::Closure` and `Op::Handle` carry are payloads: the section names
//! them by index into the [`Pool`] `lower_input` returns. Calls and
//! indexing that `seq::resolve`, `tiled::resolve_ir_call`,
//! `VectorOp::resolve` or the `Box` builtins route to a payload operation
//! are written as [`X_FOREIGN`], and the port skips the function. Which call
//! or index is routed is read off the Rust lowering (`foreign`, the spans
//! of its `Seq`, `Tiled`, `Vector` and `Fip` instructions). The intrinsic
//! name tables of `vesper-core::numeric` and `capability_effects` are
//! written per call as a class word ([`intrinsic_class`]).
#![allow(dead_code)]

use super::check_words::{Enc, mode_word, pass_word};
use super::ir_words::{Payload, Pool};
use std::collections::BTreeSet;
use vesper::ast::{Clause, Expr, ExprKind, Literal, Param, Pattern, PatternKind, Statement, Type};
use vesper::check::World;
use vesper::ir::{Op, Program};

pub const NONE: u64 = u64::MAX;

// Expression kinds (`X_*` in packages/vesper-place-ir-lower/src/ast.ves).
pub const X_FOREIGN: u64 = 0;
pub const X_LITERAL: u64 = 1;
pub const X_NAME: u64 = 2;
pub const X_TUPLE: u64 = 3;
pub const X_STRUCT: u64 = 4;
pub const X_FIELD: u64 = 5;
pub const X_INDEX: u64 = 6;
pub const X_UNARY: u64 = 7;
pub const X_BINARY: u64 = 8;
pub const X_ASSIGN: u64 = 9;
pub const X_BLOCK: u64 = 10;
pub const X_CALL: u64 = 11;
pub const X_TRY: u64 = 12;
pub const X_IF: u64 = 13;
pub const X_MATCH: u64 = 14;
pub const X_LOOP: u64 = 15;
pub const X_FOR: u64 = 16;
pub const X_RANGE: u64 = 17;
pub const X_BREAK: u64 = 18;
pub const X_CONTINUE: u64 = 19;
pub const X_RETURN: u64 = 20;
pub const X_CLOSURE: u64 = 21;
pub const X_HANDLE: u64 = 22;

// Intrinsic class bits of a call of a plain name.
pub const I_WRAPPING: u64 = 1;
pub const I_BIT: u64 = 2;
pub const I_CONVERSION: u64 = 4;
pub const I_ROTATE: u64 = 8;
pub const I_FLOAT: u64 = 16;
/// A float math intrinsic of arity other than one (`fma`, `min`, ...).
pub const I_FLOAT_NOT_UNARY: u64 = 32;

/// The class word of a call of the plain name `name` (`Lower::expr`'s
/// intrinsic tests, before it looks at `world.functions`).
pub fn intrinsic_class(name: &str) -> u64 {
    let mut c = 0;
    if vesper::numeric::wrapping_intrinsic(name).is_some() {
        c |= I_WRAPPING;
    }
    if vesper::capability_effects::bit_intrinsic(name).is_some() {
        c |= I_BIT;
    }
    if vesper::numeric::conversion(name).is_some() {
        c |= I_CONVERSION;
    }
    if vesper::numeric::rotate_intrinsic(name).is_some() {
        c |= I_ROTATE;
    }
    if vesper::numeric::float::is_float_intrinsic(name) {
        c |= I_FLOAT;
        if vesper::numeric::float::math_intrinsic(name).is_some_and(|op| op.arity() != 1) {
            c |= I_FLOAT_NOT_UNARY;
        }
    }
    c
}

/// The spans of the instructions `lower_world` routed to a payload
/// operation (`Seq`, `Tiled`, `Vector`, `Fip`).
pub fn foreign(program: &Program) -> BTreeSet<(usize, usize)> {
    let mut out = BTreeSet::new();
    for f in program.functions.values() {
        for b in &f.blocks {
            for i in &b.instructions {
                if matches!(
                    i.op,
                    Op::Seq { .. } | Op::Tiled { .. } | Op::Vector { .. } | Op::Fip { .. }
                ) {
                    out.insert((i.span.start, i.span.end));
                }
            }
        }
    }
    out
}

/// The lowering section's writer.
pub struct Lw<'a> {
    pub enc: &'a mut Enc,
    world: &'a World,
    foreign: &'a BTreeSet<(usize, usize)>,
    /// The closure and handler expressions `Op::Closure` and `Op::Handle`
    /// refer to, in the order the section names them.
    pub pool: Pool,
    pats: Vec<u64>,
    pat_count: u64,
    exprs: Vec<u64>,
    expr_count: u64,
}

impl<'a> Lw<'a> {
    pub fn new(enc: &'a mut Enc, world: &'a World, foreign: &'a BTreeSet<(usize, usize)>) -> Self {
        Self {
            enc,
            world,
            foreign,
            pool: Pool::default(),
            pats: vec![],
            pat_count: 0,
            exprs: vec![],
            expr_count: 0,
        }
    }

    fn name(&mut self, s: &str) -> u64 {
        self.enc.name(s)
    }

    fn path(&mut self, p: &[String], out: &mut Vec<u64>) {
        out.push(p.len() as u64);
        for s in p {
            let n = self.name(s);
            out.push(n);
        }
    }

    fn literal(&mut self, l: &Literal, out: &mut Vec<u64>) {
        match l {
            Literal::Int(n) => out.extend([0, *n as u64, (*n >> 64) as u64]),
            Literal::Float(x) => out.extend([1, x.to_bits(), 0]),
            Literal::Bool(b) => out.extend([2, *b as u64, 0]),
            Literal::String(s) => {
                let n = self.name(s);
                out.extend([3, n, 0]);
            }
            // A literal past `i128::MAX`: Place IR tag 4, two raw words.
            Literal::WideInt(n) => out.extend([4, *n as u64, (*n >> 64) as u64]),
        }
    }

    /// A pattern row (children first); returns its index.
    pub fn pattern(&mut self, p: &Pattern) -> u64 {
        let mut w = vec![];
        let span = [p.span.start as u64, p.span.end as u64];
        match &p.kind {
            PatternKind::Wild => {
                w.push(0);
                w.extend(span);
            }
            PatternKind::Bind(n) => {
                w.push(1);
                w.extend(span);
                w.push(self.name(n));
            }
            PatternKind::Literal(l) => {
                w.push(2);
                w.extend(span);
                self.literal(l, &mut w);
            }
            PatternKind::Tuple(ps) => {
                let kids: Vec<u64> = ps.iter().map(|c| self.pattern(c)).collect();
                w.push(3);
                w.extend(span);
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
                w.push(4);
                w.extend(span);
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

    /// A pool entry for the closure or handler `e`; returns its index.
    fn payload(&mut self, e: &Expr) -> u64 {
        self.pool.items.push(Payload::Expr(e.clone()));
        self.pool.items.len() as u64 - 1
    }

    /// A parameter: name, pass, projections, type, span.
    fn param(&mut self, p: &Param, out: &mut Vec<u64>) {
        out.push(self.name(&p.name));
        out.push(pass_word(p.pass));
        out.push(p.projections.len() as u64);
        for q in &p.projections {
            self.path(q, out);
        }
        out.push(self.enc.ty(&p.ty));
        out.extend([p.span.start as u64, p.span.end as u64]);
    }

    /// `world.moves` at span `key`: n, n x (root, path).
    fn moves(&mut self, key: (usize, usize), out: &mut Vec<u64>) {
        let moves = self.world.moves.get(&key).cloned().unwrap_or_default();
        out.push(moves.len() as u64);
        for p in &moves {
            out.push(self.name(&p.root));
            self.path(&p.path, out);
        }
    }

    /// A handler clause (its patterns and body rows first): name, n, n
    /// patterns, body.
    fn clause(&mut self, c: &Clause, out: &mut Vec<u64>) {
        let pats: Vec<u64> = c.pats.iter().map(|p| self.pattern(p)).collect();
        let b = self.expr(&c.body);
        out.push(self.name(&c.name));
        out.push(pats.len() as u64);
        out.extend(pats);
        out.push(b);
    }

    fn opt(&mut self, e: &Option<Box<Expr>>) -> u64 {
        match e {
            Some(e) => self.expr(e),
            None => NONE,
        }
    }

    /// An expression row (children first); returns its index. Payloads:
    ///
    /// - literal: literal; name: name; tuple: n, n exprs;
    /// - struct: name, n, n x (field name, expr); field: base, name;
    /// - index: base, index; unary: op, operand; binary: op, lhs, rhs;
    /// - assign: op, place, value;
    /// - block: n, n x statement (0 expr: expr | 1 let: pattern, value),
    ///   tail or NONE;
    /// - call: callee, intrinsic class, n, n x (pass, expr), m, m types;
    /// - try: operand; if: condition, then, else or NONE;
    /// - match: value, n, n x (pattern, body);
    /// - loop: body; for: pattern, iter, body; range: start, end, inclusive;
    /// - break, return: value or NONE; continue and foreign: nothing;
    /// - closure: pool entry, n, n params, body, moves;
    /// - handle: pool entry, effect label (optional name), effect name,
    ///   n, n effect argument types, has state, state pattern, state
    ///   expr, body, n, then n + 1 clauses (return clause first: name, k,
    ///   k patterns, body), the handler's outgoing effects (a Row type),
    ///   moves;
    /// - moves (`world.moves` at the span): n, n x (root, path).
    pub fn expr(&mut self, e: &Expr) -> u64 {
        let key = (e.span.start, e.span.end);
        let routed = matches!(e.kind, ExprKind::Call { .. } | ExprKind::Index(..))
            && self.foreign.contains(&key);
        let mut w: Vec<u64> = vec![];
        let kind = if routed {
            X_FOREIGN
        } else {
            match &e.kind {
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
                            Statement::Let { pat, value, .. } => {
                                let p = self.pattern(pat);
                                let v = self.expr(value);
                                ss.extend([1, p, v]);
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
                    let class = match &callee.kind {
                        ExprKind::Name(n) => intrinsic_class(n),
                        _ => 0,
                    };
                    let kids: Vec<(u64, u64)> = args
                        .iter()
                        .map(|a| (pass_word(a.pass), self.expr(&a.expr)))
                        .collect();
                    let ts: Vec<u64> = types.iter().map(|t| self.enc.ty(t)).collect();
                    w.extend([c, class, kids.len() as u64]);
                    for (p, x) in kids {
                        w.extend([p, x]);
                    }
                    w.push(ts.len() as u64);
                    w.extend(ts);
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
                ExprKind::Loop(body) => {
                    w.push(self.expr(body));
                    X_LOOP
                }
                ExprKind::For { pat, iter, body } => {
                    let p = self.pattern(pat);
                    let i = self.expr(iter);
                    let b = self.expr(body);
                    w.extend([p, i, b]);
                    X_FOR
                }
                ExprKind::Range {
                    start,
                    end,
                    inclusive,
                } => {
                    let s = self.expr(start);
                    let t = self.expr(end);
                    w.extend([s, t, *inclusive as u64]);
                    X_RANGE
                }
                ExprKind::Break(v) => {
                    w.push(self.opt(v));
                    X_BREAK
                }
                ExprKind::Return(v) => {
                    w.push(self.opt(v));
                    X_RETURN
                }
                ExprKind::Continue => X_CONTINUE,
                ExprKind::Closure { params, body, .. } => {
                    let b = self.expr(body);
                    w.push(self.payload(e));
                    w.push(params.len() as u64);
                    for p in params {
                        self.param(p, &mut w);
                    }
                    w.push(b);
                    self.moves(key, &mut w);
                    X_CLOSURE
                }
                ExprKind::Handle {
                    effect,
                    state,
                    body,
                    ret,
                    ops,
                } => {
                    let st = state.as_ref().map(|(p, s)| (self.pattern(p), self.expr(s)));
                    let b = self.expr(body);
                    let mut clauses = vec![];
                    for c in std::iter::once(ret.as_ref()).chain(ops) {
                        self.clause(c, &mut clauses);
                    }
                    w.push(self.payload(e));
                    w.push(self.enc.opt_name(&effect.label));
                    w.push(self.name(&effect.name));
                    w.push(effect.args.len() as u64);
                    for t in &effect.args {
                        let t = self.enc.ty(t);
                        w.push(t);
                    }
                    match st {
                        Some((p, s)) => w.extend([1, p, s]),
                        None => w.extend([0, NONE, NONE]),
                    }
                    w.push(b);
                    w.push(ops.len() as u64);
                    w.extend(clauses);
                    let outgoing = self
                        .world
                        .handler_effects
                        .get(&key)
                        .cloned()
                        .unwrap_or_default();
                    w.push(self.enc.ty(&Type::Row(outgoing)));
                    self.moves(key, &mut w);
                    X_HANDLE
                }
            }
        };
        self.exprs
            .extend([kind, e.span.start as u64, e.span.end as u64]);
        self.exprs.extend(w);
        self.expr_count += 1;
        self.expr_count - 1
    }

    /// The whole lowering section of `w`.
    pub fn section(&mut self, w: &World) -> Vec<u64> {
        let mut fns = vec![w.functions.len() as u64];
        for (key, f) in &w.functions {
            let body = f.body.as_ref().map_or(NONE, |b| self.expr(b));
            let mut x = vec![self.name(key), self.name(&f.name)];
            x.push(f.body.is_some() as u64);
            x.push(body);
            x.push(f.params.len() as u64);
            for p in &f.params {
                self.param(p, &mut x);
            }
            x.push(self.enc.ty(&f.result));
            x.push(self.enc.ty(&Type::Row(f.effects.clone())));
            x.push(f.generics.len() as u64);
            for g in &f.generics {
                x.push(self.name(&g.name));
                x.push(mode_word(g.bound));
                x.push(g.row as u64);
            }
            let caps: Vec<(u64, u64)> = f
                .effects
                .instances
                .iter()
                .filter_map(|e| {
                    e.label
                        .as_ref()
                        .map(|l| (self.enc.name(l), self.enc.name(&e.name)))
                })
                .collect();
            x.push(caps.len() as u64);
            for (l, n) in caps {
                x.extend([l, n]);
            }
            fns.extend(x);
        }
        let mut effects = vec![w.effects.len() as u64];
        for (name, d) in &w.effects {
            effects.push(self.name(name));
            effects.push(d.linear as u64);
            effects.push(d.generics.len() as u64);
            for g in &d.generics {
                effects.push(self.name(&g.name));
            }
            effects.push(d.ops.len() as u64);
            for op in &d.ops {
                effects.push(self.name(&op.name));
                effects.push(op.params.len() as u64);
                for p in &op.params {
                    self.param(p, &mut effects);
                }
                effects.push(self.enc.ty(&op.result));
            }
        }
        let mut out = vec![self.pool.items.len() as u64];
        out.extend(fns);
        out.extend(effects);
        out.push(self.pat_count);
        out.extend(&self.pats);
        out.push(self.expr_count);
        out.extend(&self.exprs);
        out
    }
}

/// The input of `ir_lower_words`: check-data's names, types and world,
/// then the lowering section; and the payload pool its closures and
/// handlers refer to. `reference` is Rust's lowering of `w`, read only for
/// [`foreign`]. `None` when a type literal does not fit 64 bits.
pub fn lower_input(w: &World, reference: &Program) -> Option<(Vec<u8>, Pool)> {
    if !super::ir_words::encodable(w, reference) {
        return None;
    }
    let routed = foreign(reference);
    let mut enc = Enc::default();
    // The lowering section first: its types and names must be in the
    // tables `finish` writes ahead of every section.
    let mut lw = Lw::new(&mut enc, w, &routed);
    let section = lw.section(w);
    let pool = std::mem::take(&mut lw.pool);
    let world = enc.world(w);
    Some((enc.finish(&world, &section), pool))
}
