//! The Rust side of the parse swap (self-hosting SH1 lane 12, #1155): reads
//! the word stream of `parse_words` (modules/parse/src/lib.ves, which
//! documents the layout) back into a `vesper_syntax::parser::Parse`: the
//! rowan `GreenNode` from the CST builder events, the diagnostics, and the
//! `vesper_core::ast::Module` from the AST arenas of `@vesper/ast`.
//!
//! The arenas are read in place (each is a slice of the stream); only the
//! Rust tree itself is allocated. Field meanings follow `@vesper/ast`
//! (packages/vesper-ast/src/lib.ves) and its canonical dump (dump.ves).
//! A malformed stream is a bug in the Vesper module, so it panics.
use rowan::{GreenNodeBuilder, SyntaxKind};
use vesper_syntax::ast::*;
use vesper_syntax::parser::Parse;

/// The layout version this reader understands (`format()` in lib.ves).
pub const FORMAT: u64 = 1;

const NONE: u32 = u32::MAX;
const TAG_SHIFT: u32 = 62;
const TAG_TOKEN: u64 = 0;
const TAG_START: u64 = 1;
const TAG_FINISH: u64 = 2;
const OFFSET_BITS: u32 = 24;
const OFFSET_MASK: u64 = (1 << OFFSET_BITS) - 1;
const KIND_SHIFT: u32 = 48;
const KIND_MASK: u64 = 0xff;

/// The cursor over the stream.
struct Words<'a> {
    w: &'a [u64],
    at: usize,
}

impl<'a> Words<'a> {
    fn next(&mut self) -> u64 {
        let v = self.w[self.at];
        self.at += 1;
        v
    }
    fn count(&mut self) -> usize {
        self.next() as usize
    }
    fn take(&mut self, n: usize) -> &'a [u64] {
        let s = &self.w[self.at..self.at + n];
        self.at += n;
        s
    }
    /// `len` bytes packed eight per word, little-endian.
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let words = self.take(len.div_ceil(8));
        let mut out = Vec::with_capacity(words.len() * 8);
        for w in words {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.truncate(len);
        out
    }
    /// An arena: its row count, then `width` words per row.
    fn arena(&mut self, width: usize) -> Arena<'a> {
        let n = self.count();
        Arena {
            rows: self.take(n * width),
            width,
        }
    }
}

#[derive(Clone, Copy)]
struct Arena<'a> {
    rows: &'a [u64],
    width: usize,
}

impl<'a> Arena<'a> {
    fn row(&self, i: u32) -> &'a [u64] {
        let at = i as usize * self.width;
        &self.rows[at..at + self.width]
    }
}

/// Decodes the stream of `parse_words(source)`.
pub fn decode(source: &str, words: &[u64]) -> Parse {
    let mut r = Words { w: words, at: 0 };
    let format = r.next();
    assert_eq!(format, FORMAT, "vesper-hybrid: parse_words format");
    let mut builder = GreenNodeBuilder::new();
    let events = r.count();
    for _ in 0..events {
        let e = r.next();
        match e >> TAG_SHIFT {
            TAG_TOKEN => {
                let start = (e & OFFSET_MASK) as usize;
                let end = ((e >> OFFSET_BITS) & OFFSET_MASK) as usize;
                let kind = ((e >> KIND_SHIFT) & KIND_MASK) as u16;
                builder.token(SyntaxKind(kind), &source[start..end]);
            }
            TAG_START => builder.start_node(SyntaxKind((e & KIND_MASK) as u16)),
            TAG_FINISH => builder.finish_node(),
            tag => panic!("vesper-hybrid: CST event tag {tag}"),
        }
    }
    let green = builder.finish();
    let n = r.count();
    let mut diagnostics = Vec::with_capacity(n);
    for _ in 0..n {
        let code = match r.next() {
            1 => "E0001",
            2 => "E0002",
            3 => "E0003",
            other => panic!("vesper-hybrid: parser diagnostic code {other}"),
        };
        let start = r.count();
        let end = r.count();
        let len = r.count();
        let message = String::from_utf8(r.bytes(len)).expect("diagnostic message is UTF-8");
        diagnostics.push(Diagnostic::new(code, message, start..end));
    }
    let module = if r.next() == 0 {
        Module::default()
    } else {
        Ast::read(&mut r).module()
    };
    assert_eq!(r.at, words.len(), "vesper-hybrid: trailing parse words");
    Parse {
        module,
        green,
        diagnostics,
    }
}

/// The arenas of `@vesper/ast`'s `Ast`, in the stream's order.
struct Ast<'a> {
    text: String,
    starts: &'a [u64],
    lits: Arena<'a>,
    exprs: Arena<'a>,
    pats: Arena<'a>,
    types: Arena<'a>,
    stmts: Arena<'a>,
    args: Arena<'a>,
    clauses: Arena<'a>,
    params: Arena<'a>,
    effects: Arena<'a>,
    effect_nodes: Arena<'a>,
    generics: Arena<'a>,
    fields: Arena<'a>,
    variants: Arena<'a>,
    adts: Arena<'a>,
    fns: Arena<'a>,
    effect_decls: Arena<'a>,
    impls: Arena<'a>,
    traits: Arena<'a>,
    assoc_decls: Arena<'a>,
    trait_refs: Arena<'a>,
    wheres: Arena<'a>,
    assoc_binds: Arena<'a>,
    confs: Arena<'a>,
    items: Arena<'a>,
    model_uses: Arena<'a>,
    bounds: Arena<'a>,
    uses: Arena<'a>,
    use_names: Arena<'a>,
    lists: Arena<'a>,
    pool: &'a [u64],
    m_items: &'a [u64],
    m_model_uses: &'a [u64],
    m_bounds: &'a [u64],
    m_uses: &'a [u64],
    m_public: &'a [u64],
}

fn u(v: u64) -> u32 {
    v as u32
}

fn span(start: u64, end: u64) -> Span {
    start as usize..end as usize
}

fn mode(m: u64) -> Mode {
    match m {
        0 => Mode::Copy,
        1 => Mode::Affine,
        2 => Mode::Linear,
        other => panic!("vesper-hybrid: mode {other}"),
    }
}

/// An optional mode: 3 is `None`.
fn opt_mode(m: u64) -> Option<Mode> {
    (m != 3).then(|| mode(m))
}

fn pass(p: u64) -> Pass {
    match p {
        0 => Pass::In,
        1 => Pass::Inout,
        2 => Pass::Sink,
        other => panic!("vesper-hybrid: pass {other}"),
    }
}

/// The `u128` with words `hi`:`lo`.
fn wide(hi: u64, lo: u64) -> u128 {
    (u128::from(hi) << 64) | u128::from(lo)
}

impl<'a> Ast<'a> {
    fn read(r: &mut Words<'a>) -> Self {
        let n = r.count();
        let starts = r.take(n + 1);
        let len = r.count();
        let text = String::from_utf8(r.bytes(len)).expect("interned names are UTF-8");
        let lits = r.arena(4);
        let exprs = r.arena(8);
        let pats = r.arena(6);
        let types = r.arena(5);
        let stmts = r.arena(5);
        let args = r.arena(3);
        let clauses = r.arena(3);
        let params = r.arena(6);
        let effects = r.arena(2);
        let effect_nodes = r.arena(3);
        let generics = r.arena(3);
        let fields = r.arena(4);
        let variants = r.arena(3);
        let adts = r.arena(9);
        let fns = r.arena(9);
        let effect_decls = r.arena(6);
        let impls = r.arena(3);
        let traits = r.arena(7);
        let assoc_decls = r.arena(4);
        let trait_refs = r.arena(5);
        let wheres = r.arena(4);
        let assoc_binds = r.arena(4);
        let confs = r.arena(10);
        let items = r.arena(2);
        let model_uses = r.arena(5);
        let bounds = r.arena(5);
        let uses = r.arena(7);
        let use_names = r.arena(4);
        let lists = r.arena(2);
        let mut seq = || {
            let n = r.count();
            r.take(n)
        };
        let pool = seq();
        let m_items = seq();
        let m_model_uses = seq();
        let m_bounds = seq();
        let m_uses = seq();
        let m_public = seq();
        Ast {
            text,
            starts,
            lits,
            exprs,
            pats,
            types,
            stmts,
            args,
            clauses,
            params,
            effects,
            effect_nodes,
            generics,
            fields,
            variants,
            adts,
            fns,
            effect_decls,
            impls,
            traits,
            assoc_decls,
            trait_refs,
            wheres,
            assoc_binds,
            confs,
            items,
            model_uses,
            bounds,
            uses,
            use_names,
            lists,
            pool,
            m_items,
            m_model_uses,
            m_bounds,
            m_uses,
            m_public,
        }
    }

    // ------------------------------------------------------------ leaves

    fn name(&self, sym: u64) -> String {
        let sym = sym as usize;
        self.text[self.starts[sym] as usize..self.starts[sym + 1] as usize].to_owned()
    }

    fn opt_name(&self, sym: u64) -> Option<String> {
        (u(sym) != NONE).then(|| self.name(sym))
    }

    /// The words of list `id` of the pool.
    fn list(&self, id: u64) -> &'a [u64] {
        let r = self.lists.row(u(id));
        let first = r[0] as usize;
        &self.pool[first..first + r[1] as usize]
    }

    fn lit(&self, i: u64) -> Literal {
        let r = self.lits.row(u(i));
        match r[0] {
            // The 128-bit magnitude is `r[3]`:`r[2]`; a negative Int's is at
            // most 2^127 (`i128::MIN`).
            1 if r[1] == 1 => Literal::Int((wide(r[3], r[2]) as i128).wrapping_neg()),
            1 => Literal::Int(wide(r[3], r[2]) as i128),
            5 => Literal::WideInt(wide(r[3], r[2])),
            2 => Literal::Float(f64::from_bits(r[2])),
            3 => Literal::Bool(r[2] == 1),
            4 => Literal::String(self.name(r[2])),
            other => panic!("vesper-hybrid: literal kind {other}"),
        }
    }

    // ------------------------------------------------------------ types

    fn types(&self, list: u64) -> Vec<Type> {
        self.list(list).iter().map(|&t| self.ty(t)).collect()
    }

    fn ty(&self, t: u64) -> Type {
        let r = self.types.row(u(t));
        match r[0] {
            1 => Type::Named(self.name(r[1]), self.types(r[2])),
            2 => Type::Tuple(self.types(r[1])),
            3 => Type::Function {
                params: self
                    .list(r[1])
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|p| (pass(p[0]), self.ty(p[1])))
                    .collect(),
                result: Box::new(self.ty(r[2])),
                effects: self.effects(r[3]),
                mode: mode(r[4]),
            },
            4 => Type::Row(self.effects(r[1])),
            5 => Type::Infer,
            other => panic!("vesper-hybrid: type kind {other}"),
        }
    }

    fn effect(&self, e: u64) -> Effect {
        let r = self.effect_nodes.row(u(e));
        Effect {
            label: self.opt_name(r[0]),
            name: self.name(r[1]),
            args: self.types(r[2]),
        }
    }

    fn effects(&self, e: u64) -> Effects {
        let r = self.effects.row(u(e));
        Effects {
            instances: self.list(r[0]).iter().map(|&x| self.effect(x)).collect(),
            tail: self.opt_name(r[1]),
        }
    }

    // ------------------------------------------------------------ patterns

    fn pats(&self, list: u64) -> Vec<Pattern> {
        self.list(list).iter().map(|&p| self.pat(p)).collect()
    }

    fn pat(&self, p: u64) -> Pattern {
        let r = self.pats.row(u(p));
        let kind = match r[0] {
            1 => PatternKind::Wild,
            2 => PatternKind::Bind(self.name(r[3])),
            3 => PatternKind::Literal(self.lit(r[3])),
            4 => PatternKind::Tuple(self.pats(r[3])),
            5 => PatternKind::Constructor {
                name: self.name(r[3]),
                fields: self
                    .list(r[4])
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|f| (self.name(f[0]), self.pat(f[1])))
                    .collect(),
                named: r[5] == 1,
            },
            other => panic!("vesper-hybrid: pattern kind {other}"),
        };
        Pattern {
            kind,
            span: span(r[1], r[2]),
        }
    }

    // ------------------------------------------------------------ expressions

    fn exprs(&self, list: u64) -> Vec<Expr> {
        self.list(list).iter().map(|&e| self.expr(e)).collect()
    }

    fn boxed(&self, e: u64) -> Box<Expr> {
        Box::new(self.expr(e))
    }

    fn opt_boxed(&self, e: u64) -> Option<Box<Expr>> {
        (u(e) != NONE).then(|| self.boxed(e))
    }

    fn param(&self, p: u64) -> Param {
        let r = self.params.row(u(p));
        Param {
            name: self.name(r[0]),
            pass: pass(r[1]),
            projections: self
                .list(r[2])
                .iter()
                .map(|&l| self.list(l).iter().map(|&n| self.name(n)).collect())
                .collect(),
            ty: self.ty(r[3]),
            span: span(r[4], r[5]),
        }
    }

    fn clause(&self, c: u64) -> Clause {
        let r = self.clauses.row(u(c));
        Clause {
            name: self.name(r[0]),
            pats: self.pats(r[1]),
            body: self.expr(r[2]),
        }
    }

    fn stmt(&self, s: u64) -> Statement {
        let r = self.stmts.row(u(s));
        if r[0] == 1 {
            Statement::Let {
                pat: self.pat(r[1]),
                ty: self.ty(r[2]),
                value: self.expr(r[3]),
                mutable: r[4] == 1,
            }
        } else {
            Statement::Expr(self.expr(r[3]))
        }
    }

    fn arg(&self, a: u64) -> Arg {
        let r = self.args.row(u(a));
        Arg {
            pass: pass(r[0]),
            explicit: r[1] == 1,
            expr: self.expr(r[2]),
        }
    }

    fn expr(&self, e: u64) -> Expr {
        let r = self.exprs.row(u(e));
        let (a, b, c, d, x) = (r[3], r[4], r[5], r[6], r[7]);
        let kind = match r[0] {
            1 => ExprKind::Literal(self.lit(a)),
            2 => ExprKind::Name(self.name(a)),
            3 => ExprKind::Tuple(self.exprs(a)),
            4 => ExprKind::Struct {
                name: self.name(a),
                fields: self
                    .list(b)
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|f| (self.name(f[0]), self.expr(f[1])))
                    .collect(),
            },
            5 => ExprKind::Block {
                statements: self.list(a).iter().map(|&s| self.stmt(s)).collect(),
                tail: self.opt_boxed(b),
            },
            6 => ExprKind::Call {
                callee: self.boxed(a),
                types: self.types(b),
                args: self.list(c).iter().map(|&x| self.arg(x)).collect(),
            },
            7 => ExprKind::Field(self.boxed(a), self.name(b)),
            8 => ExprKind::Index(self.boxed(a), self.boxed(b)),
            9 => ExprKind::Try(self.boxed(a)),
            10 => ExprKind::Unary(self.name(a), self.boxed(b)),
            11 => ExprKind::Binary(self.name(a), self.boxed(b), self.boxed(c)),
            12 => ExprKind::Assign(self.name(a), self.boxed(b), self.boxed(c)),
            13 => ExprKind::If {
                condition: self.boxed(a),
                then: self.boxed(b),
                otherwise: self.opt_boxed(c),
            },
            14 => ExprKind::Match {
                value: self.boxed(a),
                arms: self
                    .list(b)
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|arm| (self.pat(arm[0]), self.expr(arm[1])))
                    .collect(),
            },
            15 => ExprKind::Closure {
                params: self.list(a).iter().map(|&p| self.param(p)).collect(),
                mode: opt_mode(b & 255),
                infer_effects: b >= 256,
                result: self.ty(c),
                effects: self.effects(d),
                body: self.boxed(x),
            },
            16 => {
                let clauses = self.list(x);
                ExprKind::Handle {
                    effect: self.effect(a),
                    state: (u(b) != NONE).then(|| (self.pat(b), self.boxed(c))),
                    body: self.boxed(d),
                    ret: Box::new(self.clause(clauses[0])),
                    ops: clauses[1..].iter().map(|&k| self.clause(k)).collect(),
                }
            }
            17 => ExprKind::Loop(self.boxed(a)),
            18 => ExprKind::For {
                pat: self.pat(a),
                iter: self.boxed(b),
                body: self.boxed(c),
            },
            19 => ExprKind::Range {
                start: self.boxed(a),
                end: self.boxed(b),
                inclusive: c == 1,
            },
            20 => ExprKind::Break(self.opt_boxed(a)),
            21 => ExprKind::Return(self.opt_boxed(a)),
            22 => ExprKind::Continue,
            other => panic!("vesper-hybrid: expression kind {other}"),
        };
        Expr {
            kind,
            span: span(r[1], r[2]),
        }
    }

    // ------------------------------------------------------------ items

    fn generics(&self, list: u64) -> Vec<Generic> {
        self.list(list)
            .iter()
            .map(|&g| {
                let r = self.generics.row(u(g));
                Generic {
                    name: self.name(r[0]),
                    bound: mode(r[1]),
                    row: r[2] == 1,
                }
            })
            .collect()
    }

    fn function(&self, f: u64) -> Function {
        let r = self.fns.row(u(f));
        Function {
            name: self.name(r[0]),
            fip: r[1] == 1,
            generics: self.generics(r[2]),
            params: self.list(r[3]).iter().map(|&p| self.param(p)).collect(),
            result: self.ty(r[4]),
            effects: self.effects(r[5]),
            body: (u(r[6]) != NONE).then(|| self.expr(r[6])),
            span: span(r[7], r[8]),
        }
    }

    fn functions(&self, list: u64) -> Vec<Function> {
        self.list(list).iter().map(|&f| self.function(f)).collect()
    }

    fn trait_ref(&self, t: u64) -> TraitRef {
        let r = self.trait_refs.row(u(t));
        TraitRef {
            package: self.opt_name(r[0]),
            name: self.name(r[1]),
            args: self.types(r[2]),
            span: span(r[3], r[4]),
        }
    }

    fn adt(&self, x: u64) -> Adt {
        let r = self.adts.row(u(x));
        Adt {
            name: self.name(r[0]),
            mode: mode(r[1]),
            generics: self.generics(r[2]),
            variants: self
                .list(r[3])
                .iter()
                .map(|&v| {
                    let v = self.variants.row(u(v));
                    Variant {
                        name: self.name(v[0]),
                        fields: self.list(v[1]).iter().map(|&f| self.field(f)).collect(),
                        named: v[2] == 1,
                    }
                })
                .collect(),
            is_enum: r[4] == 1,
            tile_width: (r[5] == 1).then_some(u(r[6])),
            span: span(r[7], r[8]),
        }
    }

    fn field(&self, f: u64) -> Field {
        let r = self.fields.row(u(f));
        Field {
            name: self.name(r[0]),
            ty: self.ty(r[1]),
            modality: opt_mode(r[2]),
            temperature: match r[3] {
                0 => None,
                1 => Some(FieldTemperature::Hot),
                2 => Some(FieldTemperature::Cold),
                other => panic!("vesper-hybrid: field temperature {other}"),
            },
        }
    }

    fn item(&self, i: u64) -> Item {
        let r = self.items.row(u(i));
        let node = r[1];
        match r[0] {
            1 => Item::Adt(self.adt(node)),
            2 => Item::Function(Box::new(self.function(node))),
            3 => {
                let n = self.effect_decls.row(u(node));
                Item::Effect(EffectDecl {
                    name: self.name(n[0]),
                    linear: n[1] == 1,
                    generics: self.generics(n[2]),
                    ops: self.functions(n[3]),
                    span: span(n[4], n[5]),
                })
            }
            4 => {
                let n = self.impls.row(u(node));
                Item::Impl {
                    ty: self.ty(n[0]),
                    generics: self.generics(n[1]),
                    methods: self.functions(n[2]),
                }
            }
            5 => {
                let n = self.traits.row(u(node));
                Item::Trait(TraitDecl {
                    name: self.name(n[0]),
                    public: n[1] == 1,
                    generics: self.generics(n[2]),
                    assoc: self
                        .list(n[3])
                        .iter()
                        .map(|&s| {
                            let s = self.assoc_decls.row(u(s));
                            AssocTypeDecl {
                                name: self.name(s[0]),
                                bound: mode(s[1]),
                                span: span(s[2], s[3]),
                            }
                        })
                        .collect(),
                    methods: self.functions(n[4]),
                    span: span(n[5], n[6]),
                })
            }
            6 => Item::Conformance(self.conformance(node)),
            other => panic!("vesper-hybrid: item kind {other}"),
        }
    }

    fn conformance(&self, c: u64) -> Conformance {
        let n = self.confs.row(u(c));
        Conformance {
            kind: match self.opt_name(n[0]) {
                None => ConformanceKind::Canonical,
                Some(name) => ConformanceKind::Model {
                    name,
                    public: n[1] == 1,
                },
            },
            generics: self.generics(n[2]),
            trait_ref: self.trait_ref(n[3]),
            self_ty: self.ty(n[4]),
            where_clauses: self
                .list(n[5])
                .iter()
                .map(|&w| {
                    let w = self.wheres.row(u(w));
                    WhereClause {
                        ty: self.ty(w[0]),
                        trait_ref: self.trait_ref(w[1]),
                        span: span(w[2], w[3]),
                    }
                })
                .collect(),
            assoc: self
                .list(n[6])
                .iter()
                .map(|&s| {
                    let s = self.assoc_binds.row(u(s));
                    AssocTypeBinding {
                        name: self.name(s[0]),
                        ty: self.ty(s[1]),
                        span: span(s[2], s[3]),
                    }
                })
                .collect(),
            methods: self.functions(n[7]),
            span: span(n[8], n[9]),
        }
    }

    fn module(&self) -> Module {
        Module {
            sugar_origins: Vec::new(),
            items: self.m_items.iter().map(|&i| self.item(i)).collect(),
            model_uses: self
                .m_model_uses
                .iter()
                .map(|&m| {
                    let r = self.model_uses.row(u(m));
                    ModelUse {
                        model: self.name(r[0]),
                        span: span(r[1], r[2]),
                        scope: span(r[3], r[4]),
                    }
                })
                .collect(),
            trait_bounds: self
                .m_bounds
                .iter()
                .map(|&b| {
                    let r = self.bounds.row(u(b));
                    TraitBoundSyntax {
                        param: self.name(r[0]),
                        trait_ref: self.trait_ref(r[1]),
                        span: span(r[2], r[3]),
                        owner: self.name(r[4]),
                    }
                })
                .collect(),
            uses: self
                .m_uses
                .iter()
                .map(|&x| {
                    let r = self.uses.row(u(x));
                    UseDecl {
                        package: self.opt_name(r[0]),
                        supers: r[1] as usize,
                        path: self.list(r[2]).iter().map(|&s| self.name(s)).collect(),
                        names: self
                            .list(r[3])
                            .iter()
                            .map(|&n| {
                                let n = self.use_names.row(u(n));
                                UseName {
                                    name: self.name(n[0]),
                                    alias: self.opt_name(n[1]),
                                    span: span(n[2], n[3]),
                                }
                            })
                            .collect(),
                        public: r[4] == 1,
                        span: span(r[5], r[6]),
                    }
                })
                .collect(),
            public_items: self.m_public.iter().map(|&p| self.name(p)).collect(),
        }
    }
}
