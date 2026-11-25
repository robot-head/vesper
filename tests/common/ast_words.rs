//! The checker AST crossing (front-end wiring slice 3, #1910): a Rust
//! `vesper_core::ast::Module` written as the `ast` section of the
//! `parse_words` stream (crates/vesper-hybrid/modules/parse/src/lib.ves
//! documents the layout; crates/vesper-hybrid/src/parse_words.rs reads it
//! back), so a Vesper port can read any module the Rust checker holds, not
//! only the parse of a source: `read_words` in
//! packages/vesper-ast/src/words.ves rebuilds the `@vesper/ast` arenas.
//!
//! The rows follow `@vesper/ast` (packages/vesper-ast/src/lib.ves). Nodes
//! are written in preorder, as the lowering writes them. Two things the
//! parser never builds also cross: `Type::IntLiteral` is type kind 6 with
//! its value as a literal (`a`), and `Module::sugar_origins` is dropped
//! (no Vesper pass reads it).
//!
//! The tests include it now, and the hybrid's checker swaps will include
//! it too (as `crates/vesper-hybrid/src/hybrid.rs` includes
//! `stack_words.rs`), so keep it free of test-only dependencies.
#![allow(dead_code)]

use std::collections::HashMap;
use vesper::ast::*;

/// The format of the `parse_words` stream this writes the `ast` section of.
pub const AST_WORDS_FORMAT: u64 = 1;

const NONE: u64 = u32::MAX as u64;

fn mode(m: Mode) -> u64 {
    match m {
        Mode::Copy => 0,
        Mode::Affine => 1,
        Mode::Linear => 2,
    }
}

fn opt_mode(m: Option<Mode>) -> u64 {
    m.map_or(3, mode)
}

fn pass(p: Pass) -> u64 {
    match p {
        Pass::In => 0,
        Pass::Inout => 1,
        Pass::Sink => 2,
    }
}

fn b(v: bool) -> u64 {
    u64::from(v)
}

/// One arena: rows of `W` words.
struct Arena<const W: usize> {
    rows: Vec<[u64; W]>,
}

impl<const W: usize> Arena<W> {
    fn new() -> Self {
        Self { rows: Vec::new() }
    }
    fn push(&mut self, row: [u64; W]) -> u64 {
        self.rows.push(row);
        (self.rows.len() - 1) as u64
    }
    /// A row filled in later (`set`), so a node precedes its children.
    fn reserve(&mut self) -> u64 {
        self.push([0; W])
    }
    fn set(&mut self, i: u64, row: [u64; W]) {
        self.rows[i as usize] = row;
    }
    fn write(&self, out: &mut Vec<u64>) {
        out.push(self.rows.len() as u64);
        for r in &self.rows {
            out.extend_from_slice(r);
        }
    }
}

/// The arenas being written.
struct Writer<'m> {
    syms: HashMap<&'m str, u64>,
    names: Vec<&'m str>,
    lits: Arena<4>,
    exprs: Arena<8>,
    pats: Arena<6>,
    types: Arena<5>,
    stmts: Arena<5>,
    args: Arena<3>,
    clauses: Arena<3>,
    params: Arena<6>,
    effects: Arena<2>,
    effect_nodes: Arena<3>,
    generics: Arena<3>,
    fields: Arena<4>,
    variants: Arena<3>,
    adts: Arena<9>,
    fns: Arena<9>,
    effect_decls: Arena<6>,
    impls: Arena<3>,
    traits: Arena<7>,
    assoc_decls: Arena<4>,
    trait_refs: Arena<5>,
    wheres: Arena<4>,
    assoc_binds: Arena<4>,
    confs: Arena<10>,
    items: Arena<2>,
    model_uses: Arena<5>,
    bounds: Arena<5>,
    uses: Arena<7>,
    use_names: Arena<4>,
    lists: Arena<2>,
    pool: Vec<u64>,
}

impl<'m> Writer<'m> {
    fn new() -> Self {
        let mut lists = Arena::new();
        // List 0 is the empty list (`ast_new`).
        lists.push([0, 0]);
        Writer {
            syms: HashMap::new(),
            names: Vec::new(),
            lits: Arena::new(),
            exprs: Arena::new(),
            pats: Arena::new(),
            types: Arena::new(),
            stmts: Arena::new(),
            args: Arena::new(),
            clauses: Arena::new(),
            params: Arena::new(),
            effects: Arena::new(),
            effect_nodes: Arena::new(),
            generics: Arena::new(),
            fields: Arena::new(),
            variants: Arena::new(),
            adts: Arena::new(),
            fns: Arena::new(),
            effect_decls: Arena::new(),
            impls: Arena::new(),
            traits: Arena::new(),
            assoc_decls: Arena::new(),
            trait_refs: Arena::new(),
            wheres: Arena::new(),
            assoc_binds: Arena::new(),
            confs: Arena::new(),
            items: Arena::new(),
            model_uses: Arena::new(),
            bounds: Arena::new(),
            uses: Arena::new(),
            use_names: Arena::new(),
            lists,
            pool: Vec::new(),
        }
    }

    fn name(&mut self, s: &'m str) -> u64 {
        if let Some(&i) = self.syms.get(s) {
            return i;
        }
        let i = self.names.len() as u64;
        self.names.push(s);
        self.syms.insert(s, i);
        i
    }

    fn opt_name(&mut self, s: &'m Option<String>) -> u64 {
        s.as_deref().map_or(NONE, |s| self.name(s))
    }

    fn list(&mut self, words: &[u64]) -> u64 {
        if words.is_empty() {
            return 0;
        }
        let first = self.pool.len() as u64;
        self.pool.extend_from_slice(words);
        self.lists.push([first, words.len() as u64])
    }

    fn lit(&mut self, l: &'m Literal) -> u64 {
        let row = match l {
            Literal::Int(n) => {
                let m = n.unsigned_abs();
                [1, b(*n < 0), m as u64, (m >> 64) as u64]
            }
            Literal::WideInt(n) => [5, 0, *n as u64, (*n >> 64) as u64],
            Literal::Float(f) => [2, 0, f.to_bits(), 0],
            Literal::Bool(v) => [3, 0, b(*v), 0],
            Literal::String(s) => [4, 0, self.name(s), 0],
        };
        self.lits.push(row)
    }

    // ------------------------------------------------------------ types

    fn types(&mut self, ts: &'m [Type]) -> u64 {
        let ids: Vec<u64> = ts.iter().map(|t| self.ty(t)).collect();
        self.list(&ids)
    }

    fn ty(&mut self, t: &'m Type) -> u64 {
        let at = self.types.reserve();
        let row = match t {
            Type::Named(n, args) => [1, self.name(n), self.types(args), 0, 0],
            Type::Tuple(args) => [2, self.types(args), 0, 0, 0],
            Type::Function {
                params,
                result,
                effects,
                mode: m,
            } => {
                let mut ps = Vec::with_capacity(params.len() * 2);
                for (p, t) in params {
                    ps.push(pass(*p));
                    ps.push(self.ty(t));
                }
                let ps = self.list(&ps);
                [3, ps, self.ty(result), self.effects(effects), mode(*m)]
            }
            Type::Row(e) => [4, self.effects(e), 0, 0, 0],
            Type::Infer => [5, 0, 0, 0, 0],
            Type::IntLiteral(n) => {
                let m = n.unsigned_abs();
                let l = self.lits.push([1, b(*n < 0), m as u64, (m >> 64) as u64]);
                [6, l, 0, 0, 0]
            }
        };
        self.types.set(at, row);
        at
    }

    fn effect(&mut self, e: &'m Effect) -> u64 {
        let label = self.opt_name(&e.label);
        let name = self.name(&e.name);
        let args = self.types(&e.args);
        self.effect_nodes.push([label, name, args])
    }

    fn effects(&mut self, e: &'m Effects) -> u64 {
        let ids: Vec<u64> = e.instances.iter().map(|x| self.effect(x)).collect();
        let instances = self.list(&ids);
        let tail = self.opt_name(&e.tail);
        self.effects.push([instances, tail])
    }

    // ------------------------------------------------------------ patterns

    fn pats(&mut self, ps: &'m [Pattern]) -> u64 {
        let ids: Vec<u64> = ps.iter().map(|p| self.pat(p)).collect();
        self.list(&ids)
    }

    fn pat(&mut self, p: &'m Pattern) -> u64 {
        let at = self.pats.reserve();
        let (kind, a, bb, c) = match &p.kind {
            PatternKind::Wild => (1, 0, 0, 0),
            PatternKind::Bind(n) => (2, self.name(n), 0, 0),
            PatternKind::Literal(l) => {
                let l = self.lit(l);
                let k = match p.integer_kind {
                    Some(k) => self.name(k.name()),
                    None => NONE,
                };
                (3, l, k, 0)
            }
            PatternKind::Tuple(ps) => (4, self.pats(ps), 0, 0),
            PatternKind::Constructor {
                name,
                fields,
                named,
            } => {
                let name = self.name(name);
                let mut fs = Vec::with_capacity(fields.len() * 2);
                for (f, p) in fields {
                    fs.push(self.name(f));
                    fs.push(self.pat(p));
                }
                (5, name, self.list(&fs), b(*named))
            }
        };
        self.pats
            .set(at, [kind, p.span.start as u64, p.span.end as u64, a, bb, c]);
        at
    }

    // ------------------------------------------------------------ expressions

    fn exprs(&mut self, es: &'m [Expr]) -> u64 {
        let ids: Vec<u64> = es.iter().map(|e| self.expr(e)).collect();
        self.list(&ids)
    }

    fn opt_expr(&mut self, e: &'m Option<Box<Expr>>) -> u64 {
        e.as_deref().map_or(NONE, |e| self.expr(e))
    }

    fn param(&mut self, p: &'m Param) -> u64 {
        let name = self.name(&p.name);
        let mut projections = Vec::with_capacity(p.projections.len());
        for path in &p.projections {
            let ids: Vec<u64> = path.iter().map(|n| self.name(n)).collect();
            projections.push(self.list(&ids));
        }
        let projections = self.list(&projections);
        let ty = self.ty(&p.ty);
        self.params.push([
            name,
            pass(p.pass),
            projections,
            ty,
            p.span.start as u64,
            p.span.end as u64,
        ])
    }

    fn params(&mut self, ps: &'m [Param]) -> u64 {
        let ids: Vec<u64> = ps.iter().map(|p| self.param(p)).collect();
        self.list(&ids)
    }

    fn clause(&mut self, c: &'m Clause) -> u64 {
        let name = self.name(&c.name);
        let pats = self.pats(&c.pats);
        let body = self.expr(&c.body);
        self.clauses.push([name, pats, body])
    }

    fn stmt(&mut self, s: &'m Statement) -> u64 {
        let row = match s {
            Statement::Let {
                pat,
                ty,
                value,
                mutable,
            } => [1, self.pat(pat), self.ty(ty), self.expr(value), b(*mutable)],
            Statement::Expr(e) => [2, NONE, NONE, self.expr(e), 0],
        };
        self.stmts.push(row)
    }

    fn expr(&mut self, e: &'m Expr) -> u64 {
        let at = self.exprs.reserve();
        let (kind, a, bb, c, d, x) = match &e.kind {
            ExprKind::Literal(l) => (1, self.lit(l), 0, 0, 0, 0),
            ExprKind::Name(n) => (2, self.name(n), 0, 0, 0, 0),
            ExprKind::Tuple(es) => (3, self.exprs(es), 0, 0, 0, 0),
            ExprKind::Struct { name, fields } => {
                let name = self.name(name);
                let mut fs = Vec::with_capacity(fields.len() * 2);
                for (f, v) in fields {
                    fs.push(self.name(f));
                    fs.push(self.expr(v));
                }
                (4, name, self.list(&fs), 0, 0, 0)
            }
            ExprKind::Block { statements, tail } => {
                let ids: Vec<u64> = statements.iter().map(|s| self.stmt(s)).collect();
                let ss = self.list(&ids);
                (5, ss, self.opt_expr(tail), 0, 0, 0)
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                let callee = self.expr(callee);
                let types = self.types(types);
                let mut ids = Vec::with_capacity(args.len());
                for arg in args {
                    let ex = self.expr(&arg.expr);
                    ids.push(self.args.push([pass(arg.pass), b(arg.explicit), ex]));
                }
                (6, callee, types, self.list(&ids), 0, 0)
            }
            ExprKind::Field(v, n) => {
                let v = self.expr(v);
                (7, v, self.name(n), 0, 0, 0)
            }
            ExprKind::Index(v, i) => {
                let v = self.expr(v);
                (8, v, self.expr(i), 0, 0, 0)
            }
            ExprKind::Try(v) => (9, self.expr(v), 0, 0, 0, 0),
            ExprKind::Unary(op, v) => {
                let op = self.name(op);
                (10, op, self.expr(v), 0, 0, 0)
            }
            ExprKind::Binary(op, l, r) => {
                let op = self.name(op);
                let l = self.expr(l);
                (11, op, l, self.expr(r), 0, 0)
            }
            ExprKind::Assign(op, l, r) => {
                let op = self.name(op);
                let l = self.expr(l);
                (12, op, l, self.expr(r), 0, 0)
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let cond = self.expr(condition);
                let then = self.expr(then);
                (13, cond, then, self.opt_expr(otherwise), 0, 0)
            }
            ExprKind::Match { value, arms } => {
                let v = self.expr(value);
                let mut ids = Vec::with_capacity(arms.len() * 2);
                for (p, body) in arms {
                    ids.push(self.pat(p));
                    ids.push(self.expr(body));
                }
                (14, v, self.list(&ids), 0, 0, 0)
            }
            ExprKind::Closure {
                params,
                mode: m,
                result,
                effects,
                infer_effects,
                body,
            } => {
                let ps = self.params(params);
                let m = opt_mode(*m) + if *infer_effects { 256 } else { 0 };
                let result = self.ty(result);
                let effects = self.effects(effects);
                (15, ps, m, result, effects, self.expr(body))
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                let effect = self.effect(effect);
                let (sp, sv) = match state {
                    Some((p, v)) => {
                        let p = self.pat(p);
                        (p, self.expr(v))
                    }
                    None => (NONE, NONE),
                };
                let body = self.expr(body);
                let mut clauses = Vec::with_capacity(ops.len() + 1);
                clauses.push(self.clause(ret));
                for op in ops {
                    clauses.push(self.clause(op));
                }
                (16, effect, sp, sv, body, self.list(&clauses))
            }
            ExprKind::Loop(body) => (17, self.expr(body), 0, 0, 0, 0),
            ExprKind::For { pat, iter, body } => {
                let p = self.pat(pat);
                let i = self.expr(iter);
                (18, p, i, self.expr(body), 0, 0)
            }
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                let s = self.expr(start);
                let e = self.expr(end);
                (19, s, e, b(*inclusive), 0, 0)
            }
            ExprKind::Break(v) => (20, self.opt_expr(v), 0, 0, 0, 0),
            ExprKind::Return(v) => (21, self.opt_expr(v), 0, 0, 0, 0),
            ExprKind::Continue => (22, 0, 0, 0, 0, 0),
        };
        self.exprs.set(
            at,
            [kind, e.span.start as u64, e.span.end as u64, a, bb, c, d, x],
        );
        at
    }

    // ------------------------------------------------------------ items

    fn generics(&mut self, gs: &'m [Generic]) -> u64 {
        let ids: Vec<u64> = gs
            .iter()
            .map(|g| {
                let name = self.name(&g.name);
                self.generics.push([name, mode(g.bound), b(g.row)])
            })
            .collect();
        self.list(&ids)
    }

    fn function(&mut self, f: &'m Function) -> u64 {
        let Function {
            name,
            fip,
            generics,
            params,
            result,
            effects,
            body,
            span,
        } = f;
        let at = self.fns.reserve();
        let name = self.name(name);
        let generics = self.generics(generics);
        let params = self.params(params);
        let result = self.ty(result);
        let effects = self.effects(effects);
        let body = body.as_ref().map_or(NONE, |e| self.expr(e));
        self.fns.set(
            at,
            [
                name,
                b(*fip),
                generics,
                params,
                result,
                effects,
                body,
                span.start as u64,
                span.end as u64,
            ],
        );
        at
    }

    fn functions(&mut self, fs: &'m [Function]) -> u64 {
        let ids: Vec<u64> = fs.iter().map(|f| self.function(f)).collect();
        self.list(&ids)
    }

    fn trait_ref(&mut self, t: &'m TraitRef) -> u64 {
        let package = self.opt_name(&t.package);
        let name = self.name(&t.name);
        let args = self.types(&t.args);
        self.trait_refs
            .push([package, name, args, t.span.start as u64, t.span.end as u64])
    }

    fn adt(&mut self, a: &'m Adt) -> u64 {
        let name = self.name(&a.name);
        let generics = self.generics(&a.generics);
        let mut vs = Vec::with_capacity(a.variants.len());
        for v in &a.variants {
            let vname = self.name(&v.name);
            let mut fs = Vec::with_capacity(v.fields.len());
            for f in &v.fields {
                let fname = self.name(&f.name);
                let ty = self.ty(&f.ty);
                let temperature = match f.temperature {
                    None => 0,
                    Some(FieldTemperature::Hot) => 1,
                    Some(FieldTemperature::Cold) => 2,
                };
                fs.push(
                    self.fields
                        .push([fname, ty, opt_mode(f.modality), temperature]),
                );
            }
            let fs = self.list(&fs);
            vs.push(self.variants.push([vname, fs, b(v.named)]));
        }
        let vs = self.list(&vs);
        self.adts.push([
            name,
            mode(a.mode),
            generics,
            vs,
            b(a.is_enum),
            b(a.tile_width.is_some()),
            u64::from(a.tile_width.unwrap_or(0)),
            a.span.start as u64,
            a.span.end as u64,
        ])
    }

    fn conformance(&mut self, c: &'m Conformance) -> u64 {
        let (model, public) = match &c.kind {
            ConformanceKind::Canonical => (NONE, 0),
            ConformanceKind::Model { name, public } => (self.name(name), b(*public)),
        };
        let generics = self.generics(&c.generics);
        let trait_ref = self.trait_ref(&c.trait_ref);
        let self_ty = self.ty(&c.self_ty);
        let mut ws = Vec::with_capacity(c.where_clauses.len());
        for w in &c.where_clauses {
            let ty = self.ty(&w.ty);
            let tr = self.trait_ref(&w.trait_ref);
            ws.push(
                self.wheres
                    .push([ty, tr, w.span.start as u64, w.span.end as u64]),
            );
        }
        let ws = self.list(&ws);
        let mut binds = Vec::with_capacity(c.assoc.len());
        for s in &c.assoc {
            let name = self.name(&s.name);
            let ty = self.ty(&s.ty);
            binds.push(
                self.assoc_binds
                    .push([name, ty, s.span.start as u64, s.span.end as u64]),
            );
        }
        let binds = self.list(&binds);
        let methods = self.functions(&c.methods);
        self.confs.push([
            model,
            public,
            generics,
            trait_ref,
            self_ty,
            ws,
            binds,
            methods,
            c.span.start as u64,
            c.span.end as u64,
        ])
    }

    fn item(&mut self, i: &'m Item) -> u64 {
        let (kind, node) = match i {
            Item::Adt(a) => (1, self.adt(a)),
            Item::Function(f) => (2, self.function(f)),
            Item::Effect(e) => {
                let name = self.name(&e.name);
                let generics = self.generics(&e.generics);
                let ops = self.functions(&e.ops);
                let n = self.effect_decls.push([
                    name,
                    b(e.linear),
                    generics,
                    ops,
                    e.span.start as u64,
                    e.span.end as u64,
                ]);
                (3, n)
            }
            Item::Impl {
                ty,
                generics,
                methods,
            } => {
                let ty = self.ty(ty);
                let generics = self.generics(generics);
                let methods = self.functions(methods);
                (4, self.impls.push([ty, generics, methods]))
            }
            Item::Trait(t) => {
                let name = self.name(&t.name);
                let generics = self.generics(&t.generics);
                let mut assoc = Vec::with_capacity(t.assoc.len());
                for s in &t.assoc {
                    let n = self.name(&s.name);
                    assoc.push(self.assoc_decls.push([
                        n,
                        mode(s.bound),
                        s.span.start as u64,
                        s.span.end as u64,
                    ]));
                }
                let assoc = self.list(&assoc);
                let methods = self.functions(&t.methods);
                let n = self.traits.push([
                    name,
                    b(t.public),
                    generics,
                    assoc,
                    methods,
                    t.span.start as u64,
                    t.span.end as u64,
                ]);
                (5, n)
            }
            Item::Conformance(c) => (6, self.conformance(c)),
        };
        self.items.push([kind, node])
    }

    fn module(&mut self, m: &'m Module) -> [Vec<u64>; 5] {
        let Module {
            items,
            sugar_origins: _,
            model_uses,
            trait_bounds,
            uses,
            public_items,
        } = m;
        let items = items.iter().map(|i| self.item(i)).collect();
        let model_uses = model_uses
            .iter()
            .map(|u| {
                let model = self.name(&u.model);
                self.model_uses.push([
                    model,
                    u.span.start as u64,
                    u.span.end as u64,
                    u.scope.start as u64,
                    u.scope.end as u64,
                ])
            })
            .collect();
        let bounds = trait_bounds
            .iter()
            .map(|t| {
                let param = self.name(&t.param);
                let tr = self.trait_ref(&t.trait_ref);
                let owner = self.name(&t.owner);
                self.bounds
                    .push([param, tr, t.span.start as u64, t.span.end as u64, owner])
            })
            .collect();
        let uses = uses
            .iter()
            .map(|u| {
                let package = self.opt_name(&u.package);
                let path: Vec<u64> = u.path.iter().map(|s| self.name(s)).collect();
                let path = self.list(&path);
                let mut names = Vec::with_capacity(u.names.len());
                for n in &u.names {
                    let name = self.name(&n.name);
                    let alias = self.opt_name(&n.alias);
                    names.push(self.use_names.push([
                        name,
                        alias,
                        n.span.start as u64,
                        n.span.end as u64,
                    ]));
                }
                let names = self.list(&names);
                self.uses.push([
                    package,
                    u.supers as u64,
                    path,
                    names,
                    b(u.public),
                    u.span.start as u64,
                    u.span.end as u64,
                ])
            })
            .collect();
        let public = public_items.iter().map(|p| self.name(p)).collect();
        [items, model_uses, bounds, uses, public]
    }
}

fn seq(out: &mut Vec<u64>, words: &[u64]) {
    out.push(words.len() as u64);
    out.extend_from_slice(words);
}

/// Appends the `ast` section of `parse_words` for `m` (after its leading
/// 1): the names, the 29 arenas, the pool and the five module lists.
pub fn put_module(out: &mut Vec<u64>, m: &Module) {
    let mut w = Writer::new();
    let lists = w.module(m);
    // Names: count, count + 1 offsets, then the text packed eight bytes
    // per word.
    out.push(w.names.len() as u64);
    let mut at = 0u64;
    out.push(0);
    let mut text_len = 0usize;
    for n in &w.names {
        at += n.len() as u64;
        text_len += n.len();
        out.push(at);
    }
    out.push(text_len as u64);
    let mut word = [0u8; 8];
    let mut k = 0;
    for n in &w.names {
        for &byte in n.as_bytes() {
            word[k] = byte;
            k += 1;
            if k == 8 {
                out.push(u64::from_le_bytes(word));
                word = [0; 8];
                k = 0;
            }
        }
    }
    if k != 0 {
        out.push(u64::from_le_bytes(word));
    }
    w.lits.write(out);
    w.exprs.write(out);
    w.pats.write(out);
    w.types.write(out);
    w.stmts.write(out);
    w.args.write(out);
    w.clauses.write(out);
    w.params.write(out);
    w.effects.write(out);
    w.effect_nodes.write(out);
    w.generics.write(out);
    w.fields.write(out);
    w.variants.write(out);
    w.adts.write(out);
    w.fns.write(out);
    w.effect_decls.write(out);
    w.impls.write(out);
    w.traits.write(out);
    w.assoc_decls.write(out);
    w.trait_refs.write(out);
    w.wheres.write(out);
    w.assoc_binds.write(out);
    w.confs.write(out);
    w.items.write(out);
    w.model_uses.write(out);
    w.bounds.write(out);
    w.uses.write(out);
    w.use_names.write(out);
    w.lists.write(out);
    seq(out, &w.pool);
    for l in &lists {
        seq(out, l);
    }
}

/// The `ast` section of `m` on its own.
pub fn module_words(m: &Module) -> Vec<u64> {
    let mut out = Vec::new();
    put_module(&mut out, m);
    out
}
