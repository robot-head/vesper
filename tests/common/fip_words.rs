//! Binary encoder and diagnostic decoder for `@vesper/check-fip`
//! (`fip::check_world`, SH2 lane 12, #1184). Shared between
//! `crates/vesper-hybrid` and `tests/self_host_check_fip.rs`.

#![allow(dead_code)]

use super::check_words::{Enc, pass_word};
use vesper::ast::{Diagnostic, Expr, ExprKind, Pass, Pattern, PatternKind, Statement, Type};
use vesper::check::World;

/// The fip task section: the pool of list words (each a tag and a value: 0
/// an index, 1 a name, 2 a type), the expression and pattern tables and the
/// functions of `world.functions` in name order (ast.ves documents the
/// node layout).
#[derive(Default)]
struct FipEnc {
    pool: Vec<(u64, u64)>,
    exprs: Vec<[u64; 8]>,
    pats: Vec<[u64; 6]>,
}

impl FipEnc {
    fn list(&mut self, words: &[(u64, u64)]) -> u64 {
        let at = self.pool.len() as u64;
        self.pool.extend_from_slice(words);
        at
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
                let items: Vec<_> = ps.iter().map(|q| Self::idx(self.pat(enc, q))).collect();
                (4, self.list(&items), items.len() as u64, 0)
            }
            PatternKind::Constructor { name, fields, .. } => {
                let mut items = vec![];
                for (f, q) in fields {
                    let fname = enc.name(f);
                    let sub = self.pat(enc, q);
                    items.push((1, fname));
                    items.push(Self::idx(sub));
                }
                let name = enc.name(name);
                (5, name, self.list(&items), fields.len() as u64)
            }
        };
        let i = self.pats.len() as u64;
        self.pats
            .push([kind, p.span.start as u64, p.span.end as u64, a, b, c]);
        i
    }

    fn exprs(&mut self, enc: &mut Enc, es: &[&Expr]) -> Vec<(u64, u64)> {
        es.iter().map(|e| Self::idx(self.expr(enc, e))).collect()
    }

    fn expr(&mut self, enc: &mut Enc, e: &Expr) -> u64 {
        let opt = |x: Option<u64>| x.map_or(0, |i| i + 1);
        let pass = |p: Pass| pass_word(p);
        let node: [u64; 6] = match &e.kind {
            ExprKind::Literal(_) => [1, 0, 0, 0, 0, 0],
            ExprKind::Name(n) => [2, enc.name(n), 0, 0, 0, 0],
            ExprKind::Tuple(es) => {
                let items = self.exprs(enc, &es.iter().collect::<Vec<_>>());
                [3, self.list(&items), items.len() as u64, 0, 0, 0]
            }
            ExprKind::Struct { fields, .. } => {
                let items = self.exprs(enc, &fields.iter().map(|(_, e)| e).collect::<Vec<_>>());
                [4, self.list(&items), items.len() as u64, 0, 0, 0]
            }
            ExprKind::Block { statements, tail } => {
                let mut items = vec![];
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value, .. } => {
                            let v = self.expr(enc, value);
                            let p = self.pat(enc, pat);
                            let t = enc.ty(ty);
                            items.extend([Self::idx(p + 1), (2, t), Self::idx(v)]);
                        }
                        Statement::Expr(x) => {
                            let v = self.expr(enc, x);
                            // A statement's type word is unused: any type.
                            let t = enc.ty(&Type::Infer);
                            items.extend([Self::idx(0), (2, t), Self::idx(v)]);
                        }
                    }
                }
                let t = tail.as_deref().map(|t| self.expr(enc, t));
                [5, self.list(&items), statements.len() as u64, opt(t), 0, 0]
            }
            ExprKind::Call { callee, args, .. } => {
                let c = self.expr(enc, callee);
                let mut items = vec![];
                for a in args {
                    let x = self.expr(enc, &a.expr);
                    items.extend([Self::idx(pass(a.pass)), Self::idx(x)]);
                }
                [6, c, self.list(&items), args.len() as u64, 0, 0]
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
                let mut items = vec![];
                for (p, body) in arms {
                    let pi = self.pat(enc, p);
                    let bi = self.expr(enc, body);
                    items.extend([Self::idx(pi), Self::idx(bi)]);
                }
                [14, v, self.list(&items), arms.len() as u64, 0, 0]
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
                let mut items = vec![Self::idx(1 + ops.len() as u64)];
                for c in std::iter::once(&**ret).chain(ops) {
                    let ci = self.expr(enc, &c.body);
                    let ps: Vec<_> = c.pats.iter().map(|p| Self::idx(self.pat(enc, p))).collect();
                    items.push(Self::idx(ci));
                    items.push(Self::idx(ps.len() as u64));
                    items.extend(ps);
                }
                [16, enc.name(&effect.name), sp, sv, b, self.list(&items)]
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
                let items = self.exprs(enc, &kids);
                [kind, self.list(&items), items.len() as u64, 0, 0, 0]
            }
        };
        let i = self.exprs.len() as u64;
        self.exprs.push([
            node[0],
            e.span.start as u64,
            e.span.end as u64,
            node[1],
            node[2],
            node[3],
            node[4],
            node[5],
        ]);
        i
    }
}

/// Writes the fip task section of `world` to `tail`.
fn fip_task(enc: &mut Enc, tail: &mut Vec<u64>, world: &World) {
    let mut fe = FipEnc::default();
    let mut fns = vec![];
    for f in world.functions.values() {
        let mut words = vec![enc.name(&f.name), f.fip as u64];
        words.extend([f.span.start as u64, f.span.end as u64]);
        words.push(f.generics.len() as u64);
        for g in &f.generics {
            words.extend([enc.name(&g.name), g.row as u64]);
        }
        words.push(f.params.len() as u64);
        for p in &f.params {
            words.extend([enc.name(&p.name), pass_word(p.pass), enc.ty(&p.ty)]);
            words.extend([p.span.start as u64, p.span.end as u64]);
        }
        words.push(enc.ty(&f.result));
        words.push(enc.ty(&Type::Row(f.effects.clone())));
        words.push(f.body.as_ref().map_or(0, |b| fe.expr(enc, b) + 1));
        fns.push(words);
    }
    tail.push(fe.pool.len() as u64);
    for (tag, x) in &fe.pool {
        tail.extend([*tag, *x]);
    }
    tail.push(fe.exprs.len() as u64);
    for e in &fe.exprs {
        tail.extend(e);
    }
    tail.push(fe.pats.len() as u64);
    for p in &fe.pats {
        tail.extend(p);
    }
    tail.push(fns.len() as u64);
    for f in fns {
        tail.extend(f);
    }
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
