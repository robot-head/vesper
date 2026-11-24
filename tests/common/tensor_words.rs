//! Binary encoder for `&Module` -> `@vesper/ast::Ast` words and decoder
//! for `@vesper/check-tensor-types` diagnostics (`#1186`).
#![allow(dead_code)]

use std::collections::BTreeMap;
use vesper::ast::*;
use vesper::tensor_types;

fn put(out: &mut Vec<u8>, x: u64) {
    out.extend_from_slice(&x.to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put(out, b.len() as u64);
    out.extend_from_slice(b);
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
}

struct AstEnc {
    names: Vec<String>,
    name_map: BTreeMap<String, u32>,
    lits: Vec<[u64; 4]>,
    exprs: Vec<[u64; 8]>,
    pats: Vec<[u64; 6]>,
    types: Vec<[u64; 5]>,
    stmts: Vec<[u64; 5]>,
    args: Vec<[u64; 3]>,
    clauses: Vec<[u64; 3]>,
    params: Vec<[u64; 6]>,
    effects: Vec<[u64; 2]>,
    effect_nodes: Vec<[u64; 3]>,
    generics: Vec<[u64; 3]>,
    fields: Vec<[u64; 4]>,
    variants: Vec<[u64; 3]>,
    adts: Vec<[u64; 9]>,
    fns: Vec<[u64; 9]>,
    effect_decls: Vec<[u64; 6]>,
    impls: Vec<[u64; 3]>,
    traits: Vec<[u64; 7]>,
    confs: Vec<[u64; 10]>,
    items: Vec<[u64; 2]>,
    lists: Vec<[u64; 2]>,
    pool: Vec<u32>,
    m_items: Vec<u32>,
}

const NONE: u32 = u32::MAX;

impl AstEnc {
    fn new() -> Self {
        Self {
            names: Vec::new(),
            name_map: BTreeMap::new(),
            lits: Vec::new(),
            exprs: Vec::new(),
            pats: Vec::new(),
            types: Vec::new(),
            stmts: Vec::new(),
            args: Vec::new(),
            clauses: Vec::new(),
            params: Vec::new(),
            effects: Vec::new(),
            effect_nodes: Vec::new(),
            generics: Vec::new(),
            fields: Vec::new(),
            variants: Vec::new(),
            adts: Vec::new(),
            fns: Vec::new(),
            effect_decls: Vec::new(),
            impls: Vec::new(),
            traits: Vec::new(),
            confs: Vec::new(),
            items: Vec::new(),
            lists: Vec::new(),
            pool: Vec::new(),
            m_items: Vec::new(),
        }
    }

    fn sym(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.name_map.get(s) {
            return id;
        }
        let id = self.names.len() as u32;
        self.names.push(s.to_owned());
        self.name_map.insert(s.to_owned(), id);
        id
    }

    fn list(&mut self, items: &[u32]) -> u32 {
        if items.is_empty() {
            return 0;
        }
        let first = self.pool.len() as u64;
        let len = items.len() as u64;
        self.pool.extend_from_slice(items);
        self.lists.push([first, len]);
        self.lists.len() as u32
    }

    fn mode(m: Mode) -> u64 {
        match m {
            Mode::Copy => 0,
            Mode::Affine => 1,
            Mode::Linear => 2,
        }
    }

    fn pass(p: Pass) -> u64 {
        match p {
            Pass::In => 0,
            Pass::Inout => 1,
            Pass::Sink => 2,
        }
    }

    fn lit(&mut self, l: &Literal) -> u32 {
        let id = self.lits.len() as u32;
        let row = match l {
            Literal::Int(v) => {
                let neg = u64::from(*v < 0);
                let mag = v.unsigned_abs();
                [1, neg, mag as u64, (mag >> 64) as u64]
            }
            Literal::Float(f) => [2, 0, f.to_bits(), 0],
            Literal::Bool(b) => [3, 0, u64::from(*b), 0],
            Literal::String(s) => [4, 0, u64::from(self.sym(s)), 0],
            Literal::WideInt(m) => [5, 0, *m as u64, (*m >> 64) as u64],
        };
        self.lits.push(row);
        id
    }

    fn effect(&mut self, e: &Effect) -> u32 {
        let label = e.label.as_deref().map_or(NONE, |s| self.sym(s));
        let name = self.sym(&e.name);
        let args: Vec<u32> = e.args.iter().map(|t| self.ty(t)).collect();
        let args_list = self.list(&args);
        let id = self.effect_nodes.len() as u32;
        self.effect_nodes
            .push([u64::from(label), u64::from(name), u64::from(args_list)]);
        id
    }

    fn effects(&mut self, e: &Effects) -> u32 {
        let insts: Vec<u32> = e.instances.iter().map(|ef| self.effect(ef)).collect();
        let inst_list = self.list(&insts);
        let tail = e.tail.as_deref().map_or(NONE, |s| self.sym(s));
        let id = self.effects.len() as u32;
        self.effects.push([u64::from(inst_list), u64::from(tail)]);
        id
    }

    fn ty(&mut self, t: &Type) -> u32 {
        let row = match t {
            Type::Named(n, args) => {
                let ns = self.sym(n);
                let xs: Vec<u32> = args.iter().map(|a| self.ty(a)).collect();
                let xl = self.list(&xs);
                [1, u64::from(ns), u64::from(xl), 0, 0]
            }
            Type::Tuple(ts) => {
                let xs: Vec<u32> = ts.iter().map(|a| self.ty(a)).collect();
                let xl = self.list(&xs);
                [2, u64::from(xl), 0, 0, 0]
            }
            Type::Function {
                params,
                result,
                effects,
                mode,
            } => {
                let mut pairs = Vec::with_capacity(params.len() * 2);
                for (p, pt) in params {
                    pairs.push(Self::pass(*p) as u32);
                    pairs.push(self.ty(pt));
                }
                let pl = self.list(&pairs);
                let res = self.ty(result);
                let eff = self.effects(effects);
                let m = Self::mode(*mode);
                [3, u64::from(pl), u64::from(res), u64::from(eff), m]
            }
            Type::Row(r) => {
                let eff = self.effects(r);
                [4, u64::from(eff), 0, 0, 0]
            }
            Type::Infer => [5, 0, 0, 0, 0],
            Type::IntLiteral(_) => [6, 0, 0, 0, 0],
        };
        let id = self.types.len() as u32;
        self.types.push(row);
        id
    }

    fn pat(&mut self, p: &Pattern) -> u32 {
        let s = p.span.start as u64;
        let e = p.span.end as u64;
        let row = match &p.kind {
            PatternKind::Wild => [1, s, e, 0, 0, 0],
            PatternKind::Bind(n) => [2, s, e, u64::from(self.sym(n)), 0, 0],
            PatternKind::Literal(l) => {
                let li = self.lit(l);
                [3, s, e, u64::from(li), u64::from(NONE), 0]
            }
            PatternKind::Tuple(ps) => {
                let xs: Vec<u32> = ps.iter().map(|x| self.pat(x)).collect();
                let xl = self.list(&xs);
                [4, s, e, u64::from(xl), 0, 0]
            }
            PatternKind::Constructor {
                name,
                fields,
                named,
            } => {
                let ns = self.sym(name);
                let mut pairs = Vec::with_capacity(fields.len() * 2);
                for (fnm, fp) in fields {
                    pairs.push(self.sym(fnm));
                    pairs.push(self.pat(fp));
                }
                let fl = self.list(&pairs);
                [5, s, e, u64::from(ns), u64::from(fl), u64::from(*named)]
            }
        };
        let id = self.pats.len() as u32;
        self.pats.push(row);
        id
    }

    fn param(&mut self, p: &Param) -> u32 {
        let ns = self.sym(&p.name);
        let pass = Self::pass(p.pass);
        let mut projs = Vec::with_capacity(p.projections.len());
        for pr in &p.projections {
            let syms: Vec<u32> = pr.iter().map(|s| self.sym(s)).collect();
            projs.push(self.list(&syms));
        }
        let pl = self.list(&projs);
        let ty = self.ty(&p.ty);
        let id = self.params.len() as u32;
        self.params.push([
            u64::from(ns),
            pass,
            u64::from(pl),
            u64::from(ty),
            p.span.start as u64,
            p.span.end as u64,
        ]);
        id
    }

    fn arg(&mut self, a: &Arg) -> u32 {
        let ex = self.expr(&a.expr);
        let id = self.args.len() as u32;
        self.args
            .push([Self::pass(a.pass), u64::from(a.explicit), u64::from(ex)]);
        id
    }

    fn clause(&mut self, c: &Clause) -> u32 {
        let ns = self.sym(&c.name);
        let ps: Vec<u32> = c.pats.iter().map(|p| self.pat(p)).collect();
        let pl = self.list(&ps);
        let b = self.expr(&c.body);
        let id = self.clauses.len() as u32;
        self.clauses
            .push([u64::from(ns), u64::from(pl), u64::from(b)]);
        id
    }

    fn stmt(&mut self, s: &Statement) -> u32 {
        let row = match s {
            Statement::Let {
                pat,
                ty,
                value,
                mutable,
            } => {
                let p = self.pat(pat);
                let t = self.ty(ty);
                let v = self.expr(value);
                [
                    1,
                    u64::from(p),
                    u64::from(t),
                    u64::from(v),
                    u64::from(*mutable),
                ]
            }
            Statement::Expr(e) => {
                let v = self.expr(e);
                [2, 0, 0, u64::from(v), 0]
            }
        };
        let id = self.stmts.len() as u32;
        self.stmts.push(row);
        id
    }

    fn expr(&mut self, e: &Expr) -> u32 {
        let s = e.span.start as u64;
        let end = e.span.end as u64;
        let row = match &e.kind {
            ExprKind::Literal(l) => [1, s, end, u64::from(self.lit(l)), 0, 0, 0, 0],
            ExprKind::Name(n) => [2, s, end, u64::from(self.sym(n)), 0, 0, 0, 0],
            ExprKind::Tuple(es) => {
                let xs: Vec<u32> = es.iter().map(|x| self.expr(x)).collect();
                [3, s, end, u64::from(self.list(&xs)), 0, 0, 0, 0]
            }
            ExprKind::Struct { name, fields } => {
                let ns = self.sym(name);
                let mut pairs = Vec::with_capacity(fields.len() * 2);
                for (fnm, fv) in fields {
                    pairs.push(self.sym(fnm));
                    pairs.push(self.expr(fv));
                }
                let fl = self.list(&pairs);
                [4, s, end, u64::from(ns), u64::from(fl), 0, 0, 0]
            }
            ExprKind::Block { statements, tail } => {
                let ss: Vec<u32> = statements.iter().map(|st| self.stmt(st)).collect();
                let sl = self.list(&ss);
                let t = tail.as_ref().map_or(NONE, |x| self.expr(x));
                [5, s, end, u64::from(sl), u64::from(t), 0, 0, 0]
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                let c = self.expr(callee);
                let ts: Vec<u32> = types.iter().map(|t| self.ty(t)).collect();
                let tl = self.list(&ts);
                let ags: Vec<u32> = args.iter().map(|a| self.arg(a)).collect();
                let al = self.list(&ags);
                [6, s, end, u64::from(c), u64::from(tl), u64::from(al), 0, 0]
            }
            ExprKind::Field(x, f) => {
                let xi = self.expr(x);
                let fs = self.sym(f);
                [7, s, end, u64::from(xi), u64::from(fs), 0, 0, 0]
            }
            ExprKind::Index(a, b) => {
                let ai = self.expr(a);
                let bi = self.expr(b);
                [8, s, end, u64::from(ai), u64::from(bi), 0, 0, 0]
            }
            ExprKind::Try(x) => [9, s, end, u64::from(self.expr(x)), 0, 0, 0, 0],
            ExprKind::Unary(op, x) => [
                10,
                s,
                end,
                u64::from(self.sym(op)),
                u64::from(self.expr(x)),
                0,
                0,
                0,
            ],
            ExprKind::Binary(op, a, b) => {
                let ops = self.sym(op);
                let ai = self.expr(a);
                let bi = self.expr(b);
                [
                    11,
                    s,
                    end,
                    u64::from(ops),
                    u64::from(ai),
                    u64::from(bi),
                    0,
                    0,
                ]
            }
            ExprKind::Assign(op, a, b) => {
                let ops = self.sym(op);
                let ai = self.expr(a);
                let bi = self.expr(b);
                [
                    12,
                    s,
                    end,
                    u64::from(ops),
                    u64::from(ai),
                    u64::from(bi),
                    0,
                    0,
                ]
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let ci = self.expr(condition);
                let ti = self.expr(then);
                let oi = otherwise.as_ref().map_or(NONE, |x| self.expr(x));
                [
                    13,
                    s,
                    end,
                    u64::from(ci),
                    u64::from(ti),
                    u64::from(oi),
                    0,
                    0,
                ]
            }
            ExprKind::Match { value, arms } => {
                let vi = self.expr(value);
                let mut pairs = Vec::with_capacity(arms.len() * 2);
                for (p, body) in arms {
                    pairs.push(self.pat(p));
                    pairs.push(self.expr(body));
                }
                let al = self.list(&pairs);
                [14, s, end, u64::from(vi), u64::from(al), 0, 0, 0]
            }
            ExprKind::Closure {
                params,
                mode,
                result,
                effects,
                infer_effects,
                body,
            } => {
                let ps: Vec<u32> = params.iter().map(|p| self.param(p)).collect();
                let pl = self.list(&ps);
                let m = mode.map_or(3, Self::mode) + 256 * u64::from(*infer_effects);
                let res = self.ty(result);
                let eff = self.effects(effects);
                let bi = self.expr(body);
                [
                    15,
                    s,
                    end,
                    u64::from(pl),
                    m,
                    u64::from(res),
                    u64::from(eff),
                    u64::from(bi),
                ]
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                let ef = self.effect(effect);
                let (sp, se) = match state {
                    Some((p, ex)) => (self.pat(p), self.expr(ex)),
                    None => (NONE, 0),
                };
                let bi = self.expr(body);
                let mut cs = Vec::with_capacity(1 + ops.len());
                cs.push(self.clause(ret));
                for c in ops {
                    cs.push(self.clause(c));
                }
                let cl = self.list(&cs);
                [
                    16,
                    s,
                    end,
                    u64::from(ef),
                    u64::from(sp),
                    u64::from(se),
                    u64::from(bi),
                    u64::from(cl),
                ]
            }
            ExprKind::Loop(b) => [17, s, end, u64::from(self.expr(b)), 0, 0, 0, 0],
            ExprKind::For { pat, iter, body } => {
                let pi = self.pat(pat);
                let ii = self.expr(iter);
                let bi = self.expr(body);
                [
                    18,
                    s,
                    end,
                    u64::from(pi),
                    u64::from(ii),
                    u64::from(bi),
                    0,
                    0,
                ]
            }
            ExprKind::Range {
                start,
                end: re,
                inclusive,
            } => {
                let si = self.expr(start);
                let ei = self.expr(re);
                [
                    19,
                    s,
                    end,
                    u64::from(si),
                    u64::from(ei),
                    u64::from(*inclusive),
                    0,
                    0,
                ]
            }
            ExprKind::Break(v) => {
                let vi = v.as_ref().map_or(NONE, |x| self.expr(x));
                [20, s, end, u64::from(vi), 0, 0, 0, 0]
            }
            ExprKind::Return(v) => {
                let vi = v.as_ref().map_or(NONE, |x| self.expr(x));
                [21, s, end, u64::from(vi), 0, 0, 0, 0]
            }
            ExprKind::Continue => [22, s, end, 0, 0, 0, 0, 0],
        };
        let id = self.exprs.len() as u32;
        self.exprs.push(row);
        id
    }

    fn generic(&mut self, g: &Generic) -> u32 {
        let ns = self.sym(&g.name);
        let id = self.generics.len() as u32;
        self.generics.push([u64::from(ns), 0, u64::from(g.row)]);
        id
    }

    fn func(&mut self, f: &Function) -> u32 {
        let ns = self.sym(&f.name);
        let gs: Vec<u32> = f.generics.iter().map(|g| self.generic(g)).collect();
        let gl = self.list(&gs);
        let ps: Vec<u32> = f.params.iter().map(|p| self.param(p)).collect();
        let pl = self.list(&ps);
        let res = self.ty(&f.result);
        let eff = self.effects(&f.effects);
        let body = f.body.as_ref().map_or(NONE, |b| self.expr(b));
        let id = self.fns.len() as u32;
        self.fns.push([
            u64::from(ns),
            u64::from(f.fip),
            u64::from(gl),
            u64::from(pl),
            u64::from(res),
            u64::from(eff),
            u64::from(body),
            f.span.start as u64,
            f.span.end as u64,
        ]);
        id
    }

    fn adt(&mut self, a: &Adt) -> u32 {
        let ns = self.sym(&a.name);
        let gs: Vec<u32> = a.generics.iter().map(|g| self.generic(g)).collect();
        let gl = self.list(&gs);
        let mut vs = Vec::with_capacity(a.variants.len());
        for v in &a.variants {
            let vns = self.sym(&v.name);
            let mut fs = Vec::with_capacity(v.fields.len());
            for f in &v.fields {
                let fns = self.sym(&f.name);
                let fty = self.ty(&f.ty);
                let mod_w = f.modality.map_or(3, Self::mode);
                let temp = match f.temperature {
                    None => 0,
                    Some(FieldTemperature::Hot) => 1,
                    Some(FieldTemperature::Cold) => 2,
                };
                let fid = self.fields.len() as u32;
                self.fields
                    .push([u64::from(fns), u64::from(fty), mod_w, temp]);
                fs.push(fid);
            }
            let fl = self.list(&fs);
            let vid = self.variants.len() as u32;
            self.variants
                .push([u64::from(vns), u64::from(fl), u64::from(v.named)]);
            vs.push(vid);
        }
        let vl = self.list(&vs);
        let (tiled, tw) = match a.tile_width {
            Some(w) => (1u64, u64::from(w)),
            None => (0u64, 0u64),
        };
        let id = self.adts.len() as u32;
        self.adts.push([
            u64::from(ns),
            Self::mode(a.mode),
            u64::from(gl),
            u64::from(vl),
            u64::from(a.is_enum),
            tiled,
            tw,
            a.span.start as u64,
            a.span.end as u64,
        ]);
        id
    }

    fn encode_module(&mut self, m: &Module) {
        for item in &m.items {
            let row = match item {
                Item::Adt(a) => [1, u64::from(self.adt(a))],
                Item::Function(f) => [2, u64::from(self.func(f))],
                Item::Effect(e) => {
                    let ns = self.sym(&e.name);
                    let gs: Vec<u32> = e.generics.iter().map(|g| self.generic(g)).collect();
                    let gl = self.list(&gs);
                    let ops: Vec<u32> = e.ops.iter().map(|op| self.func(op)).collect();
                    let ol = self.list(&ops);
                    let eid = self.effect_decls.len() as u32;
                    self.effect_decls.push([
                        u64::from(ns),
                        u64::from(e.linear),
                        u64::from(gl),
                        u64::from(ol),
                        e.span.start as u64,
                        e.span.end as u64,
                    ]);
                    [3, u64::from(eid)]
                }
                Item::Impl {
                    ty,
                    generics,
                    methods,
                } => {
                    let ti = self.ty(ty);
                    let gs: Vec<u32> = generics.iter().map(|g| self.generic(g)).collect();
                    let gl = self.list(&gs);
                    let ms: Vec<u32> = methods.iter().map(|f| self.func(f)).collect();
                    let ml = self.list(&ms);
                    let iid = self.impls.len() as u32;
                    self.impls
                        .push([u64::from(ti), u64::from(gl), u64::from(ml)]);
                    [4, u64::from(iid)]
                }
                Item::Trait(t) => {
                    let ns = self.sym(&t.name);
                    let gs: Vec<u32> = t.generics.iter().map(|g| self.generic(g)).collect();
                    let gl = self.list(&gs);
                    let ms: Vec<u32> = t.methods.iter().map(|f| self.func(f)).collect();
                    let ml = self.list(&ms);
                    let tid = self.traits.len() as u32;
                    self.traits.push([
                        u64::from(ns),
                        u64::from(t.public),
                        u64::from(gl),
                        0,
                        u64::from(ml),
                        t.span.start as u64,
                        t.span.end as u64,
                    ]);
                    [5, u64::from(tid)]
                }
                Item::Conformance(c) => {
                    let (model, public) = match &c.kind {
                        ConformanceKind::Canonical => (NONE, 0u64),
                        ConformanceKind::Model { name, public } => {
                            (self.sym(name), u64::from(*public))
                        }
                    };
                    let gs: Vec<u32> = c.generics.iter().map(|g| self.generic(g)).collect();
                    let gl = self.list(&gs);
                    let st = self.ty(&c.self_ty);
                    let ms: Vec<u32> = c.methods.iter().map(|f| self.func(f)).collect();
                    let ml = self.list(&ms);
                    let cid = self.confs.len() as u32;
                    self.confs.push([
                        u64::from(model),
                        public,
                        u64::from(gl),
                        0,
                        u64::from(st),
                        0,
                        0,
                        u64::from(ml),
                        c.span.start as u64,
                        c.span.end as u64,
                    ]);
                    [6, u64::from(cid)]
                }
            };
            let id = self.items.len() as u32;
            self.items.push(row);
            self.m_items.push(id);
        }
    }
}

fn write_arena<const N: usize>(out: &mut Vec<u8>, rows: &[[u64; N]]) {
    put(out, rows.len() as u64);
    for r in rows {
        for &w in r {
            put(out, w);
        }
    }
}

pub fn encode_tensor_module(module: &Module) -> (String, Vec<u8>) {
    let mut enc = AstEnc::new();
    enc.encode_module(module);
    let mut names_str = String::new();
    let mut out = Vec::new();
    put(&mut out, enc.names.len() as u64);
    for n in &enc.names {
        put(&mut out, n.len() as u64);
        names_str.push_str(n);
    }
    write_arena(&mut out, &enc.lits);
    write_arena(&mut out, &enc.exprs);
    write_arena(&mut out, &enc.pats);
    write_arena(&mut out, &enc.types);
    write_arena(&mut out, &enc.stmts);
    write_arena(&mut out, &enc.args);
    write_arena(&mut out, &enc.clauses);
    write_arena(&mut out, &enc.params);
    write_arena(&mut out, &enc.effects);
    write_arena(&mut out, &enc.effect_nodes);
    write_arena(&mut out, &enc.generics);
    write_arena(&mut out, &enc.fields);
    write_arena(&mut out, &enc.variants);
    write_arena(&mut out, &enc.adts);
    write_arena(&mut out, &enc.fns);
    write_arena(&mut out, &enc.effect_decls);
    write_arena(&mut out, &enc.impls);
    write_arena(&mut out, &enc.traits);
    write_arena(&mut out, &enc.confs);
    write_arena(&mut out, &enc.items);
    write_arena(&mut out, &enc.lists);
    put(&mut out, enc.pool.len() as u64);
    for &p in &enc.pool {
        put(&mut out, u64::from(p));
    }
    put(&mut out, enc.m_items.len() as u64);
    for &it in &enc.m_items {
        put(&mut out, u64::from(it));
    }
    // Wire diagnostics and checked wires from Rust `wire::check`.
    let (wires, diags) = tensor_types::wire::check(module);
    put(&mut out, diags.len() as u64);
    for d in &diags {
        let code: u64 = d.code.trim_start_matches('E').parse().unwrap_or(0);
        put(&mut out, code);
        put(&mut out, d.span.start as u64);
        put(&mut out, d.span.end as u64);
        put_bytes(&mut out, d.message.as_bytes());
    }
    put(&mut out, wires.len() as u64);
    for (name, w) in &wires {
        put_bytes(&mut out, name.as_bytes());
        put(&mut out, u64::from(w.format.is_some()));
        put_bytes(
            &mut out,
            tensor_types::wire::format_mismatch(name, w).as_bytes(),
        );
    }
    (names_str, out)
}

pub fn decode_tensor_diags(words: &[u64]) -> Option<Vec<Diagnostic>> {
    if words.first().copied().unwrap_or(0) == 0 {
        return None;
    }
    let nd = words.get(1).copied().unwrap_or(0) as usize;
    let mut at = 2usize;
    let mut diags = Vec::with_capacity(nd);
    for _ in 0..nd {
        let code_num = words.get(at).copied().unwrap_or(0);
        let start = words.get(at + 1).copied().unwrap_or(0) as usize;
        let end = words.get(at + 2).copied().unwrap_or(0) as usize;
        let nbytes = words.get(at + 3).copied().unwrap_or(0) as usize;
        at += 4;
        let nwords = nbytes.div_ceil(8);
        let mut buf = Vec::with_capacity(nbytes);
        for i in 0..nwords {
            buf.extend_from_slice(&words.get(at + i).copied().unwrap_or(0).to_le_bytes());
        }
        buf.truncate(nbytes);
        at += nwords;
        let msg = String::from_utf8_lossy(&buf).into_owned();
        diags.push(Diagnostic::new(&format!("E{code_num:04}"), msg, start..end));
    }
    Some(diags)
}
