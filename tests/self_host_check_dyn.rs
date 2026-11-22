//! Self-hosting SH2 lane 11 (#1183): `src/dyn_fn.rs` and `src/dyn_trait.rs`
//! against their Vesper port, `packages/vesper-check-dyn`.
//!
//! Both passes are syntactic rewrites of the surface AST that run before
//! type checking (`pact_frontend::check_package_with`): function objects
//! become hidden traits and closure structs, then `dyn` parameters become
//! hidden bounded type parameters. The Vesper side parses each source with
//! `@vesper/ast` (which matches `vesper_syntax::parser::parse` node for
//! node, tests/self_host_ast.rs), runs both passes and writes one text: each
//! pass's diagnostics (code, span, message), the origins of the generated
//! code with `relocate` probes, the `params`, `forwards` and `boxed` tables,
//! then the canonical dump of the lowered module. The Rust side writes the
//! same text from the Rust passes; the two must be equal byte for byte.
//!
//! The corpus is every `.ves` file of the repository (with each pass also
//! forced on modules that do not need it), the source strings of the dyn
//! test suites, mutants of those that make every diagnostic of both passes
//! fire, and hand-written cases for the rest.
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use vesper::ast::*;
use vesper::{dyn_fn, dyn_trait, native};

#[path = "common/check_harness.rs"]
mod check_harness;
use check_harness::*;

const PACKAGE: &str = "packages/vesper-check-dyn";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn under_bazel() -> bool {
    std::env::var_os("TEST_SRCDIR").is_some()
}

// ---------------------------------------------------------------- the dump

/// Writes the canonical dump of a module (a copy of tests/self_host_ast.rs's,
/// which keeps it in step with `packages/vesper-ast/src/dump.ves`).
struct Dump {
    out: String,
}

fn q(s: &str) -> String {
    let mut out = String::from("\"");
    for &b in s.as_bytes() {
        if !(0x20..=0x7e).contains(&b) || b == b'"' || b == b'\\' {
            write!(out, "\\x{b:02x}").unwrap();
        } else {
            out.push(b as char);
        }
    }
    out.push('"');
    out
}

fn qo(s: &Option<String>) -> String {
    s.as_deref().map_or_else(|| "-".into(), q)
}

fn sp(s: &Span) -> String {
    format!(" @{}..{}", s.start, s.end)
}

fn mode(m: Mode) -> &'static str {
    match m {
        Mode::Copy => "copy",
        Mode::Affine => "affine",
        Mode::Linear => "linear",
    }
}

fn mode_opt(m: Option<Mode>) -> &'static str {
    m.map_or("-", mode)
}

fn pass(p: Pass) -> &'static str {
    match p {
        Pass::In => "in",
        Pass::Inout => "inout",
        Pass::Sink => "sink",
    }
}

fn b(v: bool) -> u8 {
    u8::from(v)
}

fn lit(l: &Literal) -> String {
    match l {
        Literal::Int(n) => format!("int {n}"),
        Literal::WideInt(n) => format!("int {n}"),
        Literal::Float(f) => format!("float {:#018x}", f.to_bits()),
        Literal::Bool(v) => format!("bool {v}"),
        Literal::String(s) => format!("str {}", q(s)),
    }
}

impl Dump {
    fn line(&mut self, depth: usize, text: &str) {
        for _ in 0..depth {
            self.out.push(' ');
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn effect(&mut self, e: &Effect, d: usize) {
        self.line(d, &format!("effect label={} {}", qo(&e.label), q(&e.name)));
        for t in &e.args {
            self.ty(t, d + 1);
        }
    }

    fn effects(&mut self, e: &Effects, d: usize) {
        self.line(d, &format!("effects tail={}", qo(&e.tail)));
        for i in &e.instances {
            self.effect(i, d + 1);
        }
    }

    fn ty(&mut self, t: &Type, d: usize) {
        match t {
            Type::Named(n, args) => {
                self.line(d, &format!("T {}", q(n)));
                for a in args {
                    self.ty(a, d + 1);
                }
            }
            Type::Tuple(args) => {
                self.line(d, "T tuple");
                for a in args {
                    self.ty(a, d + 1);
                }
            }
            Type::Function {
                params,
                result,
                effects,
                mode: m,
            } => {
                self.line(d, &format!("T fn mode={}", mode(*m)));
                for (p, t) in params {
                    self.line(d + 1, &format!("tparam pass={}", pass(*p)));
                    self.ty(t, d + 2);
                }
                self.line(d + 1, "result");
                self.ty(result, d + 2);
                self.effects(effects, d + 1);
            }
            Type::Row(e) => {
                self.line(d, "T row");
                self.effects(e, d + 1);
            }
            Type::Infer => self.line(d, "T _"),
            Type::IntLiteral(_) => self.line(d, "T int"),
        }
    }

    fn pat(&mut self, p: &Pattern, d: usize) {
        let s = sp(&p.span);
        match &p.kind {
            PatternKind::Wild => self.line(d, &format!("P _{s}")),
            PatternKind::Bind(n) => self.line(d, &format!("P bind {}{s}", q(n))),
            PatternKind::Literal(l) => self.line(d, &format!("P lit {}{s}", lit(l))),
            PatternKind::Tuple(ps) => {
                self.line(d, &format!("P tuple{s}"));
                for p in ps {
                    self.pat(p, d + 1);
                }
            }
            PatternKind::Constructor {
                name,
                fields,
                named,
            } => {
                self.line(d, &format!("P ctor {} named={}{s}", q(name), b(*named)));
                for (f, p) in fields {
                    self.line(d + 1, &format!("pfield {}", q(f)));
                    self.pat(p, d + 2);
                }
            }
        }
    }

    fn param(&mut self, p: &Param, d: usize) {
        self.line(
            d,
            &format!("param {} pass={}{}", q(&p.name), pass(p.pass), sp(&p.span)),
        );
        for proj in &p.projections {
            let mut s = String::from("proj");
            for n in proj {
                s.push(' ');
                s.push_str(&q(n));
            }
            self.line(d + 1, &s);
        }
        self.ty(&p.ty, d + 1);
    }

    fn clause(&mut self, c: &Clause, d: usize) {
        self.line(d, &format!("clause {}", q(&c.name)));
        for p in &c.pats {
            self.pat(p, d + 1);
        }
        self.expr(&c.body, d + 1);
    }

    fn stmt(&mut self, s: &Statement, d: usize) {
        match s {
            Statement::Let {
                pat,
                ty,
                value,
                mutable,
            } => {
                self.line(d, &format!("let mut={}", b(*mutable)));
                self.pat(pat, d + 1);
                self.ty(ty, d + 1);
                self.expr(value, d + 1);
            }
            Statement::Expr(e) => {
                self.line(d, "stmt");
                self.expr(e, d + 1);
            }
        }
    }

    fn expr(&mut self, e: &Expr, d: usize) {
        let s = sp(&e.span);
        match &e.kind {
            ExprKind::Literal(l) => self.line(d, &format!("E lit {}{s}", lit(l))),
            ExprKind::Name(n) => self.line(d, &format!("E name {}{s}", q(n))),
            ExprKind::Tuple(es) => {
                self.line(d, &format!("E tuple{s}"));
                for e in es {
                    self.expr(e, d + 1);
                }
            }
            ExprKind::Struct { name, fields } => {
                self.line(d, &format!("E struct {}{s}", q(name)));
                for (f, e) in fields {
                    self.line(d + 1, &format!("sfield {}", q(f)));
                    self.expr(e, d + 2);
                }
            }
            ExprKind::Block { statements, tail } => {
                self.line(d, &format!("E block{s}"));
                for st in statements {
                    self.stmt(st, d + 1);
                }
                if let Some(t) = tail {
                    self.line(d + 1, "tail");
                    self.expr(t, d + 2);
                }
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                self.line(d, &format!("E call{s}"));
                self.expr(callee, d + 1);
                self.line(d + 1, "targs");
                for t in types {
                    self.ty(t, d + 2);
                }
                for a in args {
                    self.line(
                        d + 1,
                        &format!("arg pass={} explicit={}", pass(a.pass), b(a.explicit)),
                    );
                    self.expr(&a.expr, d + 2);
                }
            }
            ExprKind::Field(x, f) => {
                self.line(d, &format!("E field {}{s}", q(f)));
                self.expr(x, d + 1);
            }
            ExprKind::Index(x, i) => {
                self.line(d, &format!("E index{s}"));
                self.expr(x, d + 1);
                self.expr(i, d + 1);
            }
            ExprKind::Try(x) => {
                self.line(d, &format!("E try{s}"));
                self.expr(x, d + 1);
            }
            ExprKind::Unary(op, x) => {
                self.line(d, &format!("E unary {}{s}", q(op)));
                self.expr(x, d + 1);
            }
            ExprKind::Binary(op, x, y) | ExprKind::Assign(op, x, y) => {
                let k = if matches!(e.kind, ExprKind::Binary(..)) {
                    "binary"
                } else {
                    "assign"
                };
                self.line(d, &format!("E {k} {}{s}", q(op)));
                self.expr(x, d + 1);
                self.expr(y, d + 1);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.line(d, &format!("E if{s}"));
                self.expr(condition, d + 1);
                self.expr(then, d + 1);
                if let Some(o) = otherwise {
                    self.line(d + 1, "else");
                    self.expr(o, d + 2);
                }
            }
            ExprKind::Match { value, arms } => {
                self.line(d, &format!("E match{s}"));
                self.expr(value, d + 1);
                for (p, x) in arms {
                    self.line(d + 1, "arm");
                    self.pat(p, d + 2);
                    self.expr(x, d + 2);
                }
            }
            ExprKind::Closure {
                params,
                mode: m,
                result,
                effects,
                infer_effects,
                body,
            } => {
                self.line(
                    d,
                    &format!(
                        "E closure mode={} infer={}{s}",
                        mode_opt(*m),
                        b(*infer_effects)
                    ),
                );
                for p in params {
                    self.param(p, d + 1);
                }
                self.line(d + 1, "result");
                self.ty(result, d + 2);
                self.effects(effects, d + 1);
                self.expr(body, d + 1);
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                self.line(d, &format!("E handle{s}"));
                self.effect(effect, d + 1);
                if let Some((p, x)) = state {
                    self.line(d + 1, "state");
                    self.pat(p, d + 2);
                    self.expr(x, d + 2);
                }
                self.expr(body, d + 1);
                self.clause(ret, d + 1);
                for c in ops {
                    self.clause(c, d + 1);
                }
            }
            ExprKind::Loop(x) => {
                self.line(d, &format!("E loop{s}"));
                self.expr(x, d + 1);
            }
            ExprKind::For { pat, iter, body } => {
                self.line(d, &format!("E for{s}"));
                self.pat(pat, d + 1);
                self.expr(iter, d + 1);
                self.expr(body, d + 1);
            }
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                self.line(d, &format!("E range inclusive={}{s}", b(*inclusive)));
                self.expr(start, d + 1);
                self.expr(end, d + 1);
            }
            ExprKind::Break(v) | ExprKind::Return(v) => {
                let k = if matches!(e.kind, ExprKind::Break(_)) {
                    "break"
                } else {
                    "return"
                };
                self.line(d, &format!("E {k}{s}"));
                if let Some(v) = v {
                    self.expr(v, d + 1);
                }
            }
            ExprKind::Continue => self.line(d, &format!("E continue{s}")),
        }
    }

    fn generics(&mut self, gs: &[Generic], d: usize) {
        for g in gs {
            self.line(
                d,
                &format!(
                    "generic {} bound={} row={}",
                    q(&g.name),
                    mode(g.bound),
                    b(g.row)
                ),
            );
        }
    }

    fn function(&mut self, f: &Function, d: usize) {
        self.line(
            d,
            &format!("fn {} fip={}{}", q(&f.name), b(f.fip), sp(&f.span)),
        );
        self.generics(&f.generics, d + 1);
        for p in &f.params {
            self.param(p, d + 1);
        }
        self.line(d + 1, "result");
        self.ty(&f.result, d + 2);
        self.effects(&f.effects, d + 1);
        if let Some(body) = &f.body {
            self.line(d + 1, "body");
            self.expr(body, d + 2);
        }
    }

    fn trait_ref(&mut self, t: &TraitRef, d: usize) {
        self.line(
            d,
            &format!(
                "trait_ref pkg={} {}{}",
                qo(&t.package),
                q(&t.name),
                sp(&t.span)
            ),
        );
        for a in &t.args {
            self.ty(a, d + 1);
        }
    }

    fn item(&mut self, item: &Item, d: usize) {
        match item {
            Item::Adt(a) => {
                let tile = a.tile_width.map_or_else(|| "-".into(), |w| w.to_string());
                self.line(
                    d,
                    &format!(
                        "adt {} mode={} enum={} tile={tile}{}",
                        q(&a.name),
                        mode(a.mode),
                        b(a.is_enum),
                        sp(&a.span)
                    ),
                );
                self.generics(&a.generics, d + 1);
                for v in &a.variants {
                    self.line(
                        d + 1,
                        &format!("variant {} named={}", q(&v.name), b(v.named)),
                    );
                    for f in &v.fields {
                        let temp = match f.temperature {
                            Some(FieldTemperature::Hot) => "hot",
                            Some(FieldTemperature::Cold) => "cold",
                            None => "-",
                        };
                        self.line(
                            d + 2,
                            &format!(
                                "field {} modality={} temp={temp}",
                                q(&f.name),
                                mode_opt(f.modality)
                            ),
                        );
                        self.ty(&f.ty, d + 3);
                    }
                }
            }
            Item::Function(f) => self.function(f, d),
            Item::Effect(e) => {
                self.line(
                    d,
                    &format!(
                        "effect_decl {} linear={}{}",
                        q(&e.name),
                        b(e.linear),
                        sp(&e.span)
                    ),
                );
                self.generics(&e.generics, d + 1);
                for f in &e.ops {
                    self.function(f, d + 1);
                }
            }
            Item::Impl {
                ty,
                generics,
                methods,
            } => {
                self.line(d, "impl");
                self.ty(ty, d + 1);
                self.generics(generics, d + 1);
                for f in methods {
                    self.function(f, d + 1);
                }
            }
            Item::Trait(t) => {
                self.line(
                    d,
                    &format!("trait {} pub={}{}", q(&t.name), b(t.public), sp(&t.span)),
                );
                self.generics(&t.generics, d + 1);
                for a in &t.assoc {
                    self.line(
                        d + 1,
                        &format!(
                            "assoc {} bound={}{}",
                            q(&a.name),
                            mode(a.bound),
                            sp(&a.span)
                        ),
                    );
                }
                for f in &t.methods {
                    self.function(f, d + 1);
                }
            }
            Item::Conformance(c) => {
                match &c.kind {
                    ConformanceKind::Canonical => {
                        self.line(d, &format!("conformance canonical{}", sp(&c.span)))
                    }
                    ConformanceKind::Model { name, public } => self.line(
                        d,
                        &format!(
                            "conformance model {} pub={}{}",
                            q(name),
                            b(*public),
                            sp(&c.span)
                        ),
                    ),
                }
                self.generics(&c.generics, d + 1);
                self.trait_ref(&c.trait_ref, d + 1);
                self.line(d + 1, "self");
                self.ty(&c.self_ty, d + 2);
                for w in &c.where_clauses {
                    self.line(d + 1, &format!("where{}", sp(&w.span)));
                    self.ty(&w.ty, d + 2);
                    self.trait_ref(&w.trait_ref, d + 2);
                }
                for a in &c.assoc {
                    self.line(d + 1, &format!("assoc_type {}{}", q(&a.name), sp(&a.span)));
                    self.ty(&a.ty, d + 2);
                }
                for f in &c.methods {
                    self.function(f, d + 1);
                }
            }
        }
    }
}

/// The canonical dump of `m`.
fn dump_module(m: &Module) -> String {
    let mut d = Dump { out: String::new() };
    d.line(0, "module");
    for item in &m.items {
        d.item(item, 1);
    }
    for u in &m.model_uses {
        d.line(
            1,
            &format!(
                "model_use {}{} scope{}",
                q(&u.model),
                sp(&u.span),
                sp(&u.scope)
            ),
        );
    }
    for t in &m.trait_bounds {
        d.line(
            1,
            &format!("bound {} owner={}{}", q(&t.param), q(&t.owner), sp(&t.span)),
        );
        d.trait_ref(&t.trait_ref, 2);
    }
    for u in &m.uses {
        d.line(
            1,
            &format!(
                "use pkg={} supers={} pub={}{}",
                qo(&u.package),
                u.supers,
                b(u.public),
                sp(&u.span)
            ),
        );
        for s in &u.path {
            d.line(2, &format!("seg {}", q(s)));
        }
        for n in &u.names {
            d.line(
                2,
                &format!("name {} alias={}{}", q(&n.name), qo(&n.alias), sp(&n.span)),
            );
        }
    }
    for p in &m.public_items {
        d.line(1, &format!("public {}", q(p)));
    }
    d.out
}

// ---------------------------------------------------------------- the Rust side

/// `dyn_fn::lower`'s span base: `serde_json`'s length of the module plus
/// 1,000,000. The Vesper side takes it as an argument (no JSON writer).
fn span_base(m: &Module) -> usize {
    serde_json::to_string(m).map(|s| s.len()).unwrap_or(0) + 1_000_000
}

fn put_diags(o: &mut String, ds: &[Diagnostic]) {
    for d in ds {
        writeln!(
            o,
            "D {} {} {} {}",
            d.code, d.span.start, d.span.end, d.message
        )
        .unwrap();
    }
}

fn put_origins(o: &mut String, l: &dyn_fn::Lowered) {
    writeln!(o, "origins {} {} {}", l.base, l.end, l.origins.len()).unwrap();
    for (s, span) in &l.origins {
        writeln!(o, "O {s} {} {}", span.start, span.end).unwrap();
    }
    let mut probes = vec![];
    if l.base > 0 {
        probes.push(l.base - 1);
    }
    probes.push(l.base);
    for (s, _) in &l.origins {
        probes.push(*s);
        probes.push(s + 1);
    }
    probes.push(l.end);
    for p in probes {
        let mut d = [Diagnostic::new("X", "", p..p + 1)];
        l.relocate(&mut d);
        writeln!(o, "R {p} {} {}", d[0].span.start, d[0].span.end).unwrap();
    }
}

/// The outcome text of both Rust passes on `source` (`run` in
/// packages/vesper-check-dyn/src/lib.ves writes the same).
#[allow(dead_code, reason = "a one-call form for debugging a case")]
fn rust_text(source: &str, force: bool) -> String {
    let module = vesper::parser::parse(source).module;
    let base = span_base(&module);
    rust_text_of(module, base, force)
}

fn rust_text_of(module: Module, _base: usize, force: bool) -> String {
    let mut o = String::new();
    let uses_fn = dyn_fn::uses_fn_objects(&module);
    write!(o, "fn uses={}", u8::from(uses_fn)).unwrap();
    let module = if uses_fn || force {
        match dyn_fn::lower(&module) {
            Err(ds) => {
                o.push_str(" ok=0\n");
                put_diags(&mut o, &ds);
                return o;
            }
            Ok(l) => {
                o.push_str(" ok=1\n");
                put_origins(&mut o, &l);
                l.module
            }
        }
    } else {
        o.push('\n');
        module
    };
    let uses = dyn_trait::uses_dyn(&module);
    write!(o, "dyn uses={}", u8::from(uses)).unwrap();
    if !(uses || force) {
        o.push('\n');
        o.push_str(&dump_module(&module));
        return o;
    }
    match dyn_trait::lower(&module) {
        Err(ds) => {
            o.push_str(" ok=0\n");
            put_diags(&mut o, &ds);
            o
        }
        Ok(l) => {
            o.push_str(" ok=1\n");
            for (owner, own) in &l.params {
                for (g, tr) in own {
                    writeln!(o, "P {owner} {g} {tr}").unwrap();
                }
            }
            for f in &l.forwards {
                write!(o, "F {} {} {}", f.name, f.call.start, f.call.end).unwrap();
                match &f.callee {
                    dyn_trait::Callee::Name(n) => write!(o, " N {n}").unwrap(),
                    dyn_trait::Callee::Method(s, m) => {
                        write!(o, " M {} {} {m}", s.start, s.end).unwrap()
                    }
                }
                writeln!(o, " {} {} {}", f.index, f.span.start, f.span.end).unwrap();
            }
            for b in &l.boxed {
                writeln!(o, "B {b}").unwrap();
            }
            o.push_str(&dump_module(&l.module));
            o
        }
    }
}

// ---------------------------------------------------------------- the Vesper side

/// The package's `check_dyn` entry, compiled once. The differential
/// compiles it unoptimized: under a debug-built compiler (Bazel) the
/// optimizer is most of the ~11 minutes the two optimized entries took,
/// and the passes' output does not depend on it. Timing uses
/// [`count_entry`], compiled optimized.
fn text_entry() -> &'static native::WordFunction {
    static F: OnceLock<native::WordFunction> = OnceLock::new();
    F.get_or_init(|| compile("check_dyn", speed()))
}

/// The optimized `check_dyn_count` entry, for timing.
fn count_entry() -> &'static native::WordFunction {
    static F: OnceLock<native::WordFunction> = OnceLock::new();
    F.get_or_init(|| compile("check_dyn_count", true))
}

fn compile(entry: &str, optimize: bool) -> native::WordFunction {
    let t = std::time::Instant::now();
    let f = Package::load(PACKAGE).entry(entry, optimize).0;
    eprintln!(
        "compiled {entry} (optimize {optimize}) in {:?}",
        t.elapsed()
    );
    f
}

fn call(f: &native::WordFunction, source: &str, base: usize, force: bool) -> u64 {
    let words = [source.len() as u64, 0, source.as_ptr() as u64];
    // SAFETY: the entry takes `in Str` (the address of its three words
    // `{len, 0, data}`, which live across the call, as does `source`), then
    // two `u64`s.
    unsafe { f.call(&[words.as_ptr() as u64, base as u64, u64::from(force)]) }.unwrap()
}

fn vesper_text(source: &str, base: usize, force: bool) -> String {
    unpack_text(&take_words(call(text_entry(), source, base, force)))
}

/// Compares one source; returns the first difference, if any.
fn compare(source: &str, force: bool) -> Option<String> {
    let module = vesper::parser::parse(source).module;
    let base = span_base(&module);
    let r = rust_text_of(module, base, force);
    let v = vesper_text(source, base, force);
    if r == v {
        return None;
    }
    Some(first_difference(&r, &v).unwrap_or_else(|| "texts differ in trailing bytes".into()))
}

/// The diagnostic codes of an outcome text.
fn codes(text: &str, out: &mut BTreeSet<String>) {
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix("D ") {
            out.insert(rest.split(' ').next().unwrap_or("").to_owned());
        }
    }
}

// ---------------------------------------------------------------- corpus

/// Every `.ves` file under the repository root, sorted.
fn repository_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if !matches!(
                    name.to_str(),
                    Some("target" | ".git" | "node_modules" | "bazel-out")
                ) && !name.to_string_lossy().starts_with("bazel-")
                {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "ves") {
                out.push(path);
            }
        }
    }
    let mut out = vec![];
    walk(&root(), &mut out);
    out.sort();
    out
}

/// The dyn test suites whose source strings are the checker-test corpus.
const DYN_TESTS: [&str; 4] = [
    "tests/dyn_trait.rs",
    "tests/dyn_box.rs",
    "tests/dyn_effects.rs",
    "tests/dyn_fn.rs",
];

fn dyn_test_sources() -> Vec<String> {
    let mut out = vec![];
    for f in DYN_TESTS {
        let text = std::fs::read_to_string(root().join(f)).unwrap();
        out.extend(rust_string_literals(&text));
    }
    out
}

/// Text edits that turn valid dyn programs into ones the passes reject
/// (or lower differently): each applies to every occurrence, one edit per
/// mutant.
const MUTATIONS: &[(&str, &str)] = &[
    ("dyn Fn(", "dyn FnMut("),
    ("dyn FnMut(", "dyn FnOnce("),
    ("dyn FnOnce(", "dyn Fn("),
    ("dyn Fn(", "dyn FnOnce("),
    ("in self", "inout self"),
    ("in self", "sink self"),
    ("inout self", "in self"),
    ("in dyn", "sink dyn"),
    ("sink dyn", "inout dyn"),
    ("dyn ", ""),
    (": i64 =", " ="),
    (": String =", " ="),
    ("|x|", "|x: bool|"),
    ("|x|", "|x, y|"),
    ("|", "| inout "),
    (") -> i64 {", ") -> bool {"),
    ("eff [", "eff [Tick, "),
    ("eff [", "eff [e | "),
    ("struct ", "linear struct "),
    ("copy struct ", "linear struct "),
    ("fn call", "fn call[T]"),
    ("(in self)", "()"),
    ("(in self", "(in self, in other: Self"),
    ("-> i64;", "-> Self;"),
    (
        "trait ",
        "trait Hidden { type A; fn get(in self) -> i64; }\ntrait ",
    ),
    ("Box[dyn ", "Box[dyn Box[dyn "),
    (
        "fn main",
        "fn uses[T](in t: T, in f: dyn Fn(i64) -> i64) -> i64 { let c = t; f(1) }\nfn main",
    ),
];

fn mutants(sources: &[String]) -> Vec<String> {
    let mut out = vec![];
    for s in sources {
        for (from, to) in MUTATIONS {
            if s.contains(from) {
                out.push(s.replace(from, to));
            }
        }
    }
    out
}

/// Programs for the rules the suites and mutants may not reach.
const HAND_WRITTEN: &[&str] = &[
    // E0630: an associated type, type parameters, no receiver, `Self`
    // outside the receiver, an open effect row.
    "trait T { type A; fn f(in self) -> i64; }\nfn g(in x: dyn T) -> i64 { x.f() }\n",
    "trait T { fn f[U](in self, in u: U) -> i64; }\nfn g(in x: dyn T) -> i64 { 0 }\n",
    "trait T { fn f() -> i64; }\nfn g(in x: dyn T) -> i64 { 0 }\n",
    "trait T { fn f(in self, in o: Self) -> i64; fn h(in self) -> Self; }\nfn g(in x: dyn T) -> i64 { 0 }\n",
    "trait T[E] { fn f(in self) -> i64 eff [IO | E]; }\nfn g(in x: dyn T[eff []]) -> i64 { 0 }\n",
    "trait T { fn f(in self) -> i64 eff [e]; }\nfn g(in x: Box[dyn T]) -> i64 { 0 }\n",
    // E0631: bound, returned, stored, captured, passed to a non-dyn parameter.
    "trait T { fn f(in self) -> i64; }\nfn g(in x: dyn T) -> i64 { let y = x; 0 }\n",
    "trait T { fn f(in self) -> i64; }\nstruct S { a: i64 }\nfn g(in x: dyn T) -> i64 { let c = |z: i64| x.f(); h(x, x.f()); (x, 1); S { a: x }; 0 }\nfn h(in a: i64, in b: i64) -> i64 { a }\n",
    "trait T { fn f(in self) -> i64; }\nfn g(in x: dyn T) -> i64 { match 1 { x => 0, _ => x.f() } }\nfn k(in x: dyn T) -> i64 { for x in 0..3 { } if true { x.f() } else { 0 } }\n",
    // E0632: elsewhere.
    "trait T { fn f(in self) -> i64; }\nstruct S { a: dyn T }\nfn g() -> dyn T { 0 }\n",
    "trait T { fn f(in self) -> i64; fn h(in self, in o: dyn T) -> i64; }\n",
    "trait T { fn f(in self) -> i64; }\nimpl dyn T { fn m(in self) -> i64 { 0 } }\n",
    "trait T { fn f(in self) -> i64; }\nstruct P { a: i64 }\nimpl T for P { fn f(in self) -> i64 { let z: dyn T = self; 0 } }\n",
    "trait T[A] { fn f(in self) -> i64; }\nfn g(in x: dyn T[dyn T[i64]]) -> i64 { 0 }\n",
    "trait T { fn f(in self) -> i64; }\neffect E { fn op(in x: dyn T) -> i64; }\n",
    "trait T { fn f(in self) -> i64; }\nfn g(in x: Box[dyn T]) -> Box[dyn T] { let b = Box::new[dyn T](1); x }\n",
    // dyn_fn: arity, written types and passes, result, effects, linear.
    "fn ap(in f: dyn Fn(i64) -> i64) -> i64 { f(1) }\nfn main() -> i64 { ap(|a, b| a) }\n",
    "fn ap(in f: dyn Fn(i64) -> i64) -> i64 { f(1) }\nfn main() -> i64 { ap(|a: bool| 1) + ap(|inout a| a) }\n",
    "fn ap(in f: dyn Fn(i64) -> i64) -> i64 { f(1) }\nfn main() -> i64 { ap(|a| -> bool true) }\n",
    "effect Tick { fn tick() -> i64; }\nfn ap(in f: dyn Fn(i64) -> i64 eff [Tick]) -> i64 { f(1) }\nfn main() -> i64 { ap(|a| eff [IO] a) }\n",
    "fn ap(in f: dyn FnMut(i64) -> i64) -> i64 { 0 }\nfn main() -> i64 { ap(linear |a| a) }\n",
    "linear struct L { v: i64 }\nfn ap(in f: dyn FnMut(i64) -> i64) -> i64 { 0 }\nfn once(sink f: dyn FnOnce(i64) -> i64) -> i64 { 0 }\nfn main(sink l: L) -> i64 { ap(|a| l.v) + once(|a| l.v) }\n",
    "fn ap(in f: dyn Fn(i64) -> i64) -> i64 { 0 }\nfn main() -> i64 { let k = h(); ap(|a| a + k) }\nfn h[T]() -> i64 { 0 }\n",
    "fn ap[T](in t: T) -> i64 { let f: Box[dyn Fn(T) -> i64] = Box::new(|a| 0); 0 }\n",
];

/// Every dyn test source, each mutant and the hand-written cases give the
/// same lowering, diagnostics and tables, and every diagnostic of both
/// passes fires.
#[test]
fn dyn_passes_match_rust_on_the_dyn_suites_and_mutants() {
    let sources = dyn_test_sources();
    assert!(sources.len() >= 40, "{} dyn test sources", sources.len());
    let mut all: Vec<String> = sources.clone();
    all.extend(mutants(&sources));
    all.extend(HAND_WRITTEN.iter().map(|s| (*s).to_owned()));
    let mut failures = vec![];
    let mut seen = BTreeSet::new();
    let mut lowered = 0;
    for (i, s) in all.iter().enumerate() {
        let module = vesper::parser::parse(s).module;
        let base = span_base(&module);
        let r = rust_text_of(module, base, false);
        codes(&r, &mut seen);
        if r.contains(" ok=1\n") {
            lowered += 1;
        }
        let v = vesper_text(s, base, false);
        if r != v {
            failures.push(format!(
                "case {i} {s:?}: {}",
                first_difference(&r, &v).unwrap_or_else(|| "trailing bytes".into())
            ));
        }
    }
    eprintln!(
        "dyn suites: {} cases ({} sources, {} lowered), codes {seen:?}",
        all.len(),
        sources.len(),
        lowered
    );
    let mut shown = failures.clone();
    shown.truncate(8);
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        all.len(),
        shown.join("\n")
    );
    for code in [
        "E0102", "E0200", "E0205", "E0207", "E0310", "E0401", "E0630", "E0631", "E0632", "E0633",
    ] {
        assert!(seen.contains(code), "no case makes {code} fire: {seen:?}");
    }
}

/// Every repository `.ves` file gives the same outcome, with each pass run
/// as the checker gates it and forced on.
#[test]
fn dyn_passes_match_rust_on_the_repository_sources() {
    let files = repository_sources();
    let floor = if under_bazel() { 300 } else { 800 };
    assert!(files.len() >= floor, "{} sources", files.len());
    let mut failures = vec![];
    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        for force in [false, true] {
            if let Some(d) = compare(&source, force) {
                failures.push(format!(
                    "{} (force {force}): {d}",
                    path.strip_prefix(root()).unwrap_or(path).display()
                ));
            }
        }
    }
    let mut shown = failures.clone();
    shown.truncate(8);
    assert!(
        failures.is_empty(),
        "{} of {} sources differ:\n{}",
        failures.len(),
        files.len(),
        shown.join("\n")
    );
}

/// ABBA timing of both passes (parse included on both sides) over the dyn
/// sources and over the repository sources forced on. Run with
/// `CHECK_HARNESS_SPEED=1 cargo test --release --test self_host_check_dyn
/// -- --ignored --nocapture`.
#[test]
#[ignore]
fn dyn_passes_speed() {
    if !speed() {
        return;
    }
    let f = count_entry();
    let mut sources = dyn_test_sources();
    sources.extend(mutants(&sources.clone()));
    let prepared: Vec<(String, usize)> = sources
        .iter()
        .map(|s| (s.clone(), span_base(&vesper::parser::parse(s).module)))
        .collect();
    let files: Vec<(String, usize)> = repository_sources()
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| {
            let b = span_base(&vesper::parser::parse(&s).module);
            (s, b)
        })
        .collect();
    for (label, set, force) in [
        ("dyn suites and mutants", &prepared, false),
        ("repository sources, forced", &files, true),
    ] {
        abba(
            label,
            &mut || {
                time(|| {
                    for (s, _) in set.iter() {
                        std::hint::black_box(rust_count(s, force));
                    }
                })
            },
            &mut || {
                time(|| {
                    for (s, b) in set.iter() {
                        std::hint::black_box(call(f, s, *b, force));
                    }
                })
            },
        );
    }
}

/// Both Rust passes on `source` (parse included), for timing.
fn rust_count(source: &str, force: bool) -> usize {
    let module = vesper::parser::parse(source).module;
    let module = if force || dyn_fn::uses_fn_objects(&module) {
        match dyn_fn::lower(&module) {
            Ok(l) => l.module,
            Err(d) => return d.len(),
        }
    } else {
        module
    };
    if force || dyn_trait::uses_dyn(&module) {
        match dyn_trait::lower(&module) {
            Ok(l) => l.module.items.len(),
            Err(d) => d.len(),
        }
    } else {
        module.items.len()
    }
}

/// The string literals of a Rust test file that look like Vesper sources
/// (tests/self_host_ast.rs's extractor).
fn rust_string_literals(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'\'' {
            if bytes.get(i + 2) == Some(&b'\'') {
                i += 3;
                continue;
            }
            if bytes.get(i + 1) == Some(&b'\\')
                && let Some(end) = text[i + 2..].find('\'')
            {
                i += end + 3;
                continue;
            }
            i += 1;
            continue;
        }
        if c == b'r'
            && matches!(bytes.get(i + 1), Some(b'#' | b'"'))
            && !bytes[i.saturating_sub(1)].is_ascii_alphanumeric()
        {
            let mut j = i + 1;
            let mut hashes = 0;
            while bytes.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if bytes.get(j) == Some(&b'"') {
                let close = format!("\"{}", "#".repeat(hashes));
                if let Some(end) = text[j + 1..].find(&close) {
                    out.push(text[j + 1..j + 1 + end].to_owned());
                    i = j + 1 + end + close.len();
                    continue;
                }
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            let mut j = i + 1;
            let mut s = String::new();
            let mut ok = true;
            while j < bytes.len() && bytes[j] != b'"' {
                if bytes[j] == b'\\' {
                    let e = bytes.get(j + 1).copied().unwrap_or(b' ');
                    j += 2;
                    match e {
                        b'n' => s.push('\n'),
                        b't' => s.push('\t'),
                        b'r' => s.push('\r'),
                        b'0' => s.push('\0'),
                        b'\\' => s.push('\\'),
                        b'"' => s.push('"'),
                        b'\'' => s.push('\''),
                        b'\n' => {
                            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                                j += 1;
                            }
                        }
                        b'u' => {
                            let end = text[j..].find('}').map(|e| j + e);
                            match end.and_then(|e| {
                                u32::from_str_radix(&text[j + 1..e], 16)
                                    .ok()
                                    .map(|v| (e, v))
                            }) {
                                Some((e, v)) => {
                                    match char::from_u32(v) {
                                        Some(ch) => s.push(ch),
                                        None => ok = false,
                                    }
                                    j = e + 1;
                                }
                                None => ok = false,
                            }
                        }
                        b'x' => {
                            match u8::from_str_radix(&text[j..(j + 2).min(text.len())], 16) {
                                Ok(v) if v < 128 => s.push(v as char),
                                _ => ok = false,
                            }
                            j += 2;
                        }
                        _ => ok = false,
                    }
                } else {
                    let ch = text[j..].chars().next().unwrap();
                    s.push(ch);
                    j += ch.len_utf8();
                }
            }
            if ok {
                out.push(s);
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out.retain(|s| {
        s.len() >= 6
            && [
                "fn ", "struct ", "enum ", "trait ", "use ", "impl ", "effect ", "model ", "const ",
            ]
            .iter()
            .any(|k| s.contains(k))
    });
    out
}
