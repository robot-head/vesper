//! Binary encoder and diagnostic decoder for `@vesper/check-fip`
//! (`fip::check_world`, SH2 lane 12, #1184). Shared between
//! `crates/vesper-hybrid` and `tests/self_host_check_fip.rs`.

#![allow(dead_code)]

use super::check_words::{Enc, pass_word};
use vesper::ast::{Diagnostic, Expr, ExprKind, Pass, Pattern, PatternKind, Statement, Type};
use vesper::check::World;

/// Operand words of an expression node (ast.ves): `a` to `e`.
const EXPR_OPS: usize = 5;
/// Operand words of a pattern node (ast.ves): `a` to `c`.
const PAT_OPS: usize = 3;
/// The count words of the task section: pool, expressions, patterns and
/// functions.
const TASK_COUNTS: usize = 4;

/// The fip task section: the pool of list words (each one word, its value
/// shifted past a two-bit tag: 0 an index, 1 a name, 2 a type), the
/// expression and pattern tables and the functions of `world.functions` in
/// name order (ast.ves documents the node layout).
///
/// A node is written packed: a head word (its kind, and the count of
/// operand words that follow above bit 8), a span word (start, and end
/// above bit 32), then its operands up to the last nonzero one.
#[derive(Default)]
struct FipEnc {
    pool: Vec<u64>,
    expr_count: u64,
    exprs: Vec<u64>,
    pat_count: u64,
    pats: Vec<u64>,
    /// List words being gathered, innermost list last: a node pushes its
    /// items here from `base` and [`FipEnc::close`]s them into the pool, so
    /// no list needs a vector of its own.
    stack: Vec<(u64, u64)>,
    /// The index of `Type::Infer` once interned (a statement's type word).
    infer: Option<u64>,
}

impl FipEnc {
    /// Moves the items gathered from `base` to the pool: their start there
    /// and their count.
    fn close(&mut self, base: usize) -> (u64, u64) {
        let at = self.pool.len() as u64;
        let n = (self.stack.len() - base) as u64;
        self.pool
            .extend(self.stack.drain(base..).map(|(tag, x)| (x << 2) | tag));
        (at, n)
    }

    /// Appends one packed node (see [`FipEnc`]) to `out`.
    fn node(out: &mut Vec<u64>, kind: u64, start: usize, end: usize, ops: &[u64]) {
        let n = ops.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1);
        out.push(kind | ((n as u64) << 8));
        out.push(start as u64 | ((end as u64) << 32));
        out.extend_from_slice(&ops[..n]);
    }

    fn idx(i: u64) -> (u64, u64) {
        (0, i)
    }

    fn pat(&mut self, enc: &mut Enc, p: &Pattern) -> u64 {
        let (kind, a, b, c) = match &p.kind {
            PatternKind::Wild => (1, 0, 0, 0),
            PatternKind::Bind(n) => (2, enc.name(n), 0, 0),
            PatternKind::Literal(_) => (3, 0, 0, 0),
            PatternKind::Tuple(ps) => {
                let base = self.stack.len();
                for q in ps {
                    let i = self.pat(enc, q);
                    self.stack.push(Self::idx(i));
                }
                let (at, n) = self.close(base);
                (4, at, n, 0)
            }
            PatternKind::Constructor { name, fields, .. } => {
                let base = self.stack.len();
                for (f, q) in fields {
                    let fname = enc.name(f);
                    let sub = self.pat(enc, q);
                    self.stack.push((1, fname));
                    self.stack.push(Self::idx(sub));
                }
                let name = enc.name(name);
                let (at, _) = self.close(base);
                (5, name, at, fields.len() as u64)
            }
        };
        let i = self.pat_count;
        self.pat_count += 1;
        let ops: [u64; PAT_OPS] = [a, b, c];
        Self::node(&mut self.pats, kind, p.span.start, p.span.end, &ops);
        i
    }

    /// The expressions `es`, encoded, as one pool list: its start and length.
    fn exprs<'e>(&mut self, enc: &mut Enc, es: impl IntoIterator<Item = &'e Expr>) -> (u64, u64) {
        let base = self.stack.len();
        for e in es {
            let i = self.expr(enc, e);
            self.stack.push(Self::idx(i));
        }
        self.close(base)
    }

    fn expr(&mut self, enc: &mut Enc, e: &Expr) -> u64 {
        let opt = |x: Option<u64>| x.map_or(0, |i| i + 1);
        let pass = |p: Pass| pass_word(p);
        let node: [u64; 1 + EXPR_OPS] = match &e.kind {
            ExprKind::Literal(_) => [1, 0, 0, 0, 0, 0],
            ExprKind::Name(n) => [2, enc.name(n), 0, 0, 0, 0],
            ExprKind::Tuple(es) => {
                let (at, n) = self.exprs(enc, es);
                [3, at, n, 0, 0, 0]
            }
            ExprKind::Struct { fields, .. } => {
                let (at, n) = self.exprs(enc, fields.iter().map(|(_, e)| e));
                [4, at, n, 0, 0, 0]
            }
            ExprKind::Block { statements, tail } => {
                let base = self.stack.len();
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value, .. } => {
                            let v = self.expr(enc, value);
                            let p = self.pat(enc, pat);
                            let t = enc.ty(ty);
                            self.stack.extend([Self::idx(p + 1), (2, t), Self::idx(v)]);
                        }
                        Statement::Expr(x) => {
                            let v = self.expr(enc, x);
                            // A statement's type word is unused: any type.
                            let t = match self.infer {
                                Some(t) => t,
                                None => *self.infer.insert(enc.ty(&Type::Infer)),
                            };
                            self.stack.extend([Self::idx(0), (2, t), Self::idx(v)]);
                        }
                    }
                }
                // The tail's lists go to the pool before the block's own.
                let t = tail.as_deref().map(|t| self.expr(enc, t));
                let (at, _) = self.close(base);
                [5, at, statements.len() as u64, opt(t), 0, 0]
            }
            ExprKind::Call { callee, args, .. } => {
                let c = self.expr(enc, callee);
                let base = self.stack.len();
                for a in args {
                    let x = self.expr(enc, &a.expr);
                    self.stack.extend([Self::idx(pass(a.pass)), Self::idx(x)]);
                }
                let (at, _) = self.close(base);
                [6, c, at, args.len() as u64, 0, 0]
            }
            ExprKind::Field(b, n) => {
                let x = self.expr(enc, b);
                [7, x, enc.name(n), 0, 0, 0]
            }
            ExprKind::Index(a, b) => {
                let x = self.expr(enc, a);
                let y = self.expr(enc, b);
                [8, x, y, 0, 0, 0]
            }
            ExprKind::Try(a) => [9, self.expr(enc, a), 0, 0, 0, 0],
            ExprKind::Unary(_, a) => [10, self.expr(enc, a), 0, 0, 0, 0],
            ExprKind::Binary(op, a, b) => {
                let x = self.expr(enc, a);
                let y = self.expr(enc, b);
                [11, x, y, matches!(op.as_str(), "&&" | "||") as u64, 0, 0]
            }
            ExprKind::Assign(_, a, b) => {
                let x = self.expr(enc, a);
                let y = self.expr(enc, b);
                [12, x, y, 0, 0, 0]
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let c = self.expr(enc, condition);
                let t = self.expr(enc, then);
                let o = otherwise.as_deref().map(|o| self.expr(enc, o));
                [13, c, t, opt(o), 0, 0]
            }
            ExprKind::Match { value, arms } => {
                let v = self.expr(enc, value);
                let base = self.stack.len();
                for (p, body) in arms {
                    let pi = self.pat(enc, p);
                    let bi = self.expr(enc, body);
                    self.stack.extend([Self::idx(pi), Self::idx(bi)]);
                }
                let (at, _) = self.close(base);
                [14, v, at, arms.len() as u64, 0, 0]
            }
            ExprKind::Closure { body, .. } => [15, self.expr(enc, body), 0, 0, 0, 0],
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                let (sp, sv) = match state {
                    Some((p, v)) => {
                        let pi = self.pat(enc, p);
                        (pi + 1, self.expr(enc, v))
                    }
                    None => (0, 0),
                };
                let b = self.expr(enc, body);
                let base = self.stack.len();
                self.stack.push(Self::idx(1 + ops.len() as u64));
                for c in std::iter::once(&**ret).chain(ops) {
                    let ci = self.expr(enc, &c.body);
                    // The clause's body, its pattern count, then its patterns.
                    self.stack
                        .extend([Self::idx(ci), Self::idx(c.pats.len() as u64)]);
                    for p in &c.pats {
                        let pi = self.pat(enc, p);
                        self.stack.push(Self::idx(pi));
                    }
                }
                let (list, _) = self.close(base);
                [16, enc.name(&effect.name), sp, sv, b, list]
            }
            k => {
                let kind = match k {
                    ExprKind::Loop(_) => 17,
                    ExprKind::For { .. } => 18,
                    ExprKind::Range { .. } => 19,
                    ExprKind::Break(_) => 20,
                    ExprKind::Return(_) => 21,
                    _ => 22,
                };
                let kids = k.control_children().unwrap_or_default();
                let (at, n) = self.exprs(enc, kids);
                [kind, at, n, 0, 0, 0]
            }
        };
        let i = self.expr_count;
        self.expr_count += 1;
        Self::node(
            &mut self.exprs,
            node[0],
            e.span.start,
            e.span.end,
            &node[1..],
        );
        i
    }
}

/// Writes the fip task section of `world` to `tail`.
pub fn fip_task(enc: &mut Enc, tail: &mut Vec<u64>, world: &World) {
    let mut fe = FipEnc::default();
    // Every function's words, one after another (they follow the tables).
    let mut fns = vec![];
    for f in world.functions.values() {
        fns.extend([enc.name(&f.name), f.fip as u64]);
        fns.extend([f.span.start as u64, f.span.end as u64]);
        fns.push(f.generics.len() as u64);
        for g in &f.generics {
            fns.extend([enc.name(&g.name), g.row as u64]);
        }
        fns.push(f.params.len() as u64);
        for p in &f.params {
            fns.extend([enc.name(&p.name), pass_word(p.pass), enc.ty(&p.ty)]);
            fns.extend([p.span.start as u64, p.span.end as u64]);
        }
        fns.push(enc.ty(&f.result));
        fns.push(enc.ty(&Type::Row(f.effects.clone())));
        fns.push(f.body.as_ref().map_or(0, |b| fe.expr(enc, b) + 1));
    }
    tail.reserve(TASK_COUNTS + fe.pool.len() + fe.exprs.len() + fe.pats.len() + fns.len());
    tail.push(fe.pool.len() as u64);
    tail.extend(&fe.pool);
    tail.push(fe.expr_count);
    tail.extend(&fe.exprs);
    tail.push(fe.pat_count);
    tail.extend(&fe.pats);
    tail.push(world.functions.len() as u64);
    tail.extend(fns);
}

/// Encode `world` and its fip task words into the binary input consumed by
/// `@vesper/check-fip::check_fip` / `check_fip_bytes`.
pub fn encode_world(world: &World) -> Vec<u8> {
    let mut enc = Enc::default();
    let w = enc.world(world);
    let mut tail = vec![];
    fip_task(&mut enc, &mut tail, world);
    enc.finish(&w, &tail)
}

/// Decode the packed text diagnostic lines produced by `@vesper/check-fip`:
/// each line is `{code} {start} {end} {message}`.
pub fn decode_diagnostics(words: &[u64]) -> Vec<Diagnostic> {
    let byte_len = words[0] as usize;
    let mut bytes = Vec::with_capacity(byte_len);
    for &w in &words[1..] {
        for shift in (0..64).step_by(8) {
            if bytes.len() == byte_len {
                break;
            }
            bytes.push(((w >> shift) & 0xff) as u8);
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut out = vec![];
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(4, ' ');
        let code = parts.next().expect("fip diag code");
        let start: usize = parts.next().expect("fip diag start").parse().expect("u32");
        let end: usize = parts.next().expect("fip diag end").parse().expect("u32");
        let message = parts.next().unwrap_or("");
        out.push(Diagnostic::new(code, message, start..end));
    }
    out
}
