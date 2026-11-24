//! Binary encoder and diagnostic decoder for `@vesper/check-effects`
//! (`capability_effects::check_world`, SH2 lane 8, #1180). Shared between
//! `crates/vesper-hybrid` and `tests/self_host_check_effects.rs`.

#![allow(dead_code)]

use super::check_words::{Enc, pass_word};
use vesper::ast::{
    Arg, Clause, Diagnostic, Effects, Expr, ExprKind, Literal, Param, Pattern, PatternKind,
    Statement, Type,
};
use vesper::check::World;

const NONE: u64 = u32::MAX as u64;

/// The AST tables of one world: expressions (8 words each), patterns (6
/// words each) and the list pool, laid out as `prog.ves` reads them.
#[derive(Default)]
struct Ast {
    exprs: Vec<u64>,
    expr_count: u64,
    pats: Vec<u64>,
    pat_count: u64,
    pool: Vec<u64>,
}

impl Ast {
    fn node(&mut self, kind: u64, e: &Expr, f: [u64; 5]) -> u64 {
        self.exprs
            .extend([kind, e.span.start as u64, e.span.end as u64]);
        self.exprs.extend(f);
        self.expr_count += 1;
        self.expr_count - 1
    }

    fn list(&mut self, words: Vec<u64>) -> u64 {
        let first = self.pool.len() as u64;
        self.pool.extend(words);
        first
    }

    fn opt(&mut self, enc: &mut Enc, e: &Option<Box<Expr>>) -> u64 {
        e.as_ref().map_or(NONE, |e| self.expr(enc, e))
    }

    fn pat(&mut self, enc: &mut Enc, p: &Pattern) -> u64 {
        let (kind, a, b, c) = match &p.kind {
            PatternKind::Wild => (1, 0, 0, 0),
            PatternKind::Bind(n) => (2, enc.name(n), 0, 0),
            PatternKind::Literal(_) => (3, 0, 0, 0),
            PatternKind::Tuple(ps) => {
                let ids: Vec<u64> = ps.iter().map(|p| self.pat(enc, p)).collect();
                let n = ids.len() as u64;
                (4, self.list(ids), n, 0)
            }
            PatternKind::Constructor { name, fields, .. } => {
                let mut words = vec![];
                for (f, sub) in fields {
                    let s = self.pat(enc, sub);
                    words.extend([enc.name(f), s]);
                }
                let n = fields.len() as u64;
                let name = enc.name(name);
                (5, name, self.list(words), n)
            }
        };
        self.pats
            .extend([kind, p.span.start as u64, p.span.end as u64, a, b, c]);
        self.pat_count += 1;
        self.pat_count - 1
    }

    fn row(enc: &mut Enc, e: &Effects) -> u64 {
        enc.ty(&Type::Row(e.clone()))
    }

    fn params(&mut self, enc: &mut Enc, ps: &[Param]) -> (u64, u64) {
        let mut words = vec![];
        for p in ps {
            words.extend([enc.name(&p.name), pass_word(p.pass), enc.ty(&p.ty)]);
        }
        (self.list(words), ps.len() as u64)
    }

    fn clause(&mut self, enc: &mut Enc, c: &Clause) -> [u64; 4] {
        let ids: Vec<u64> = c.pats.iter().map(|p| self.pat(enc, p)).collect();
        let n = ids.len() as u64;
        let first = self.list(ids);
        let body = self.expr(enc, &c.body);
        [enc.name(&c.name), first, n, body]
    }

    fn expr(&mut self, enc: &mut Enc, e: &Expr) -> u64 {
        match &e.kind {
            ExprKind::Literal(l) => {
                let k = match l {
                    Literal::Int(_) | Literal::WideInt(_) => 1,
                    Literal::Float(_) => 2,
                    Literal::Bool(_) => 3,
                    Literal::String(_) => 4,
                };
                self.node(1, e, [k, 0, 0, 0, 0])
            }
            ExprKind::Name(n) => {
                let n = enc.name(n);
                self.node(2, e, [n, 0, 0, 0, 0])
            }
            ExprKind::Tuple(es) => {
                let ids: Vec<u64> = es.iter().map(|x| self.expr(enc, x)).collect();
                let n = ids.len() as u64;
                let first = self.list(ids);
                self.node(3, e, [first, n, 0, 0, 0])
            }
            ExprKind::Struct { name, fields } => {
                let mut words = vec![];
                for (f, v) in fields {
                    let v = self.expr(enc, v);
                    words.extend([enc.name(f), v]);
                }
                let n = fields.len() as u64;
                let first = self.list(words);
                let name = enc.name(name);
                self.node(4, e, [name, first, n, 0, 0])
            }
            ExprKind::Block { statements, tail } => {
                let mut words = vec![];
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value, .. } => {
                            let v = self.expr(enc, value);
                            let p = self.pat(enc, pat);
                            words.extend([1, p, enc.ty(ty), v]);
                        }
                        Statement::Expr(x) => {
                            let v = self.expr(enc, x);
                            words.extend([2, NONE, NONE, v]);
                        }
                    }
                }
                let n = statements.len() as u64;
                let first = self.list(words);
                let t = self.opt(enc, tail);
                self.node(5, e, [first, n, t, 0, 0])
            }
            ExprKind::Call { callee, args, .. } => {
                let c = self.expr(enc, callee);
                let words = args_words(self, enc, args);
                let n = args.len() as u64;
                let first = self.list(words);
                self.node(6, e, [c, first, n, 0, 0])
            }
            ExprKind::Field(base, name) => {
                let b = self.expr(enc, base);
                let n = enc.name(name);
                self.node(7, e, [b, n, 0, 0, 0])
            }
            ExprKind::Index(a, b) => {
                let a = self.expr(enc, a);
                let b = self.expr(enc, b);
                self.node(8, e, [a, b, 0, 0, 0])
            }
            ExprKind::Try(v) => {
                let v = self.expr(enc, v);
                self.node(9, e, [v, 0, 0, 0, 0])
            }
            ExprKind::Unary(op, v) => {
                let v = self.expr(enc, v);
                let op = enc.name(op);
                self.node(10, e, [op, v, 0, 0, 0])
            }
            ExprKind::Binary(op, l, r) => {
                let l = self.expr(enc, l);
                let r = self.expr(enc, r);
                let op = enc.name(op);
                self.node(11, e, [op, l, r, 0, 0])
            }
            ExprKind::Assign(op, l, r) => {
                let l = self.expr(enc, l);
                let r = self.expr(enc, r);
                let op = enc.name(op);
                self.node(12, e, [op, l, r, 0, 0])
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let c = self.expr(enc, condition);
                let t = self.expr(enc, then);
                let o = self.opt(enc, otherwise);
                self.node(13, e, [c, t, o, 0, 0])
            }
            ExprKind::Match { value, arms } => {
                let v = self.expr(enc, value);
                let mut words = vec![];
                for (p, body) in arms {
                    let p = self.pat(enc, p);
                    let b = self.expr(enc, body);
                    words.extend([p, b]);
                }
                let n = arms.len() as u64;
                let first = self.list(words);
                self.node(14, e, [v, first, n, 0, 0])
            }
            ExprKind::Closure {
                params,
                result,
                effects,
                body,
                ..
            } => {
                let (first, n) = self.params(enc, params);
                let r = enc.ty(result);
                let row = Self::row(enc, effects);
                let b = self.expr(enc, body);
                self.node(15, e, [first, n, r, row, b])
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                let (sp, sv) = match state {
                    Some((p, v)) => {
                        let v = self.expr(enc, v);
                        (self.pat(enc, p), v)
                    }
                    None => (NONE, NONE),
                };
                let b = self.expr(enc, body);
                let mut words = vec![1 + ops.len() as u64];
                words.extend(self.clause(enc, ret));
                for op in ops {
                    words.extend(self.clause(enc, op));
                }
                let first = self.list(words);
                let name = enc.name(&effect.name);
                self.node(16, e, [name, sp, sv, b, first])
            }
            ExprKind::Loop(body) => {
                let b = self.expr(enc, body);
                self.node(17, e, [b, 0, 0, 0, 0])
            }
            ExprKind::For { pat, iter, body } => {
                let p = self.pat(enc, pat);
                let i = self.expr(enc, iter);
                let b = self.expr(enc, body);
                self.node(18, e, [p, i, b, 0, 0])
            }
            ExprKind::Range { start, end, .. } => {
                let a = self.expr(enc, start);
                let b = self.expr(enc, end);
                self.node(19, e, [a, b, 0, 0, 0])
            }
            ExprKind::Break(v) => {
                let v = self.opt(enc, v);
                self.node(20, e, [v, 0, 0, 0, 0])
            }
            ExprKind::Return(v) => {
                let v = self.opt(enc, v);
                self.node(21, e, [v, 0, 0, 0, 0])
            }
            ExprKind::Continue => self.node(22, e, [0; 5]),
        }
    }
}

fn args_words(ast: &mut Ast, enc: &mut Enc, args: &[Arg]) -> Vec<u64> {
    let mut words = vec![];
    for a in args {
        let x = ast.expr(enc, &a.expr);
        words.extend([pass_word(a.pass), x]);
    }
    words
}

/// The words of one function (a `Fun` row of prog.ves) and its parameters.
fn function_words(
    ast: &mut Ast,
    enc: &mut Enc,
    world: &World,
    key: &str,
    f: &vesper::ast::Function,
    out: &mut Vec<u64>,
) {
    let carried = vesper::float_math::carried_effects(world, &f.name, &f.effects);
    let body = f.body.as_ref().map_or(NONE, |b| ast.expr(enc, b));
    out.extend([
        enc.name(key),
        enc.name(&f.name),
        f.span.start as u64,
        f.span.end as u64,
        enc.ty(&f.result),
        Ast::row(enc, &f.effects),
        Ast::row(enc, &carried),
        body,
        f.params.len() as u64,
    ]);
    for p in &f.params {
        out.extend([enc.name(&p.name), pass_word(p.pass), enc.ty(&p.ty)]);
    }
}

/// The task words: the AST tables, then the functions, the effect
/// declarations and the call instantiations.
fn encode_task(enc: &mut Enc, tail: &mut Vec<u64>, world: &World) {
    let mut ast = Ast::default();
    let mut funs = vec![world.functions.len() as u64];
    for (key, f) in &world.functions {
        function_words(&mut ast, enc, world, key, f, &mut funs);
    }
    let mut effs = vec![world.effects.len() as u64];
    for (key, d) in &world.effects {
        effs.extend([
            enc.name(key),
            d.linear as u64,
            d.span.start as u64,
            d.span.end as u64,
            d.generics.len() as u64,
        ]);
        for g in &d.generics {
            effs.push(enc.name(&g.name));
        }
        effs.push(d.ops.len() as u64);
        for op in &d.ops {
            function_words(&mut ast, enc, world, &op.name, op, &mut effs);
        }
    }
    let mut calls = vec![world.calls.len() as u64];
    for ((a, b), vars) in &world.calls {
        calls.extend([*a as u64, *b as u64, vars.len() as u64]);
        for (n, t) in vars {
            calls.extend([enc.name(n), enc.ty(t)]);
        }
    }
    tail.push(ast.expr_count);
    tail.extend(&ast.exprs);
    tail.push(ast.pat_count);
    tail.extend(&ast.pats);
    tail.push(ast.pool.len() as u64);
    tail.extend(&ast.pool);
    tail.extend(funs);
    tail.extend(effs);
    tail.extend(calls);
}

/// Encode `world` and its capability-effects task words into the binary input
/// consumed by `@vesper/check-effects::check_effects` / `check_effects_bytes`.
pub fn encode_world(world: &World) -> Vec<u8> {
    let mut enc = Enc::default();
    let w = enc.world(world);
    let mut tail = vec![];
    encode_task(&mut enc, &mut tail, world);
    enc.finish(&w, &tail)
}

fn read_u32(bytes: &[u8], pos: &mut usize) -> u32 {
    let b = [
        bytes[*pos],
        bytes[*pos + 1],
        bytes[*pos + 2],
        bytes[*pos + 3],
    ];
    *pos += 4;
    u32::from_le_bytes(b)
}

fn read_str(bytes: &[u8], pos: &mut usize, len: usize) -> String {
    let s = String::from_utf8_lossy(&bytes[*pos..*pos + len]).into_owned();
    *pos += len;
    s
}

/// Decode the packed binary diagnostics produced by `@vesper/check-effects::write_bin`.
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
    let mut pos = 0;
    let count = read_u32(&bytes, &mut pos) as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let start = read_u32(&bytes, &mut pos) as usize;
        let end = read_u32(&bytes, &mut pos) as usize;
        let code_len = read_u32(&bytes, &mut pos) as usize;
        let msg_len = read_u32(&bytes, &mut pos) as usize;
        let help_len = read_u32(&bytes, &mut pos) as usize;
        let code = read_str(&bytes, &mut pos, code_len);
        let message = read_str(&bytes, &mut pos, msg_len);
        let help_str = read_str(&bytes, &mut pos, help_len);
        let mut d = Diagnostic::new(&code, message, start..end);
        if help_str != "-" {
            d = d.with_help(help_str);
        }
        out.push(d);
    }
    out
}
