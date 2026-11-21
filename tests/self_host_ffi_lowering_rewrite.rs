//! Self-hosting SH4 lane 13 (#1287), part 2: the `Rewriter` of
//! src/ffi_lowering.rs against its Vesper port,
//! `packages/vesper-ffi-lowering/rewrite` (`@vesper/ffi-rewrite`).
//!
//! The rewriter is a syntactic pass over the surface AST that
//! `check_module` runs twice: `Ffi[Lib]` rows become the library effect
//! `Ffi$Lib`, `Lib::f(..)` calls become its operations, and source misuse
//! of foreign names (values, handlers, handles) is a typed error. Pass 1
//! leaves slice parameters open and records each call's slice arguments;
//! the checker then gives their lengths, and pass 2 names one operation per
//! slice-length vector. The Vesper side parses each source with
//! `@vesper/ast` (which matches the Rust parser node for node,
//! tests/self_host_ast.rs), runs pass 1 and, when pass 1 succeeds, pass 2
//! with the lengths the Rust checker computed (`plan_probe::arities`), and
//! writes one text per pass: the first error by span, the pass-1 slice
//! sites or the pass-2 slice-length vectors and calls, then the canonical
//! dump of the rewritten module. The Rust side writes the same text from
//! the Rust passes (`plan_probe::rewrite`); the two must be equal byte for
//! byte. `check_module`'s drop items (src/ffi_lowering/drops.rs) are not
//! part of the input: they are generated, not source.
//!
//! The corpus is every source string of the FFI test suites and every
//! `.ves` fixture that names an imported library, each with the libraries
//! it names (direct and `isolated(mpk)`), hand-written cases and mutants
//! that make every error of the rewriter fire, and every `.ves` file of the
//! repository (every `FFI_REWRITE_STEP`-th, default 1) with the Bridge
//! library, where the rewrite is mostly a walk that changes nothing.
//!
//! # Input words
//!
//! Numbers are little-endian `u64` words; a string is its byte length then
//! its bytes (unpadded). The source is the entry's second argument.
//!
//! ```text
//! libraries: n, n x (handler (0 direct, 1 isolated(mpk)), canonical bytes)  -- sorted by name
//! pass 2:    0, or 1, n, n x (call start, call end, k, k lengths)  -- u64::MAX is a lent Slice
//! ```
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use vesper::ast::*;
use vesper::ffi_lowering::plan_probe;
use vesper::ffi_lowering::{FfiError, Handler, Imports};
use vesper::native;
use vesper_ffi::sys::{self, LibraryDescriptor};

#[path = "common/check_harness.rs"]
mod check_harness;
use check_harness::*;

const PACKAGE: &str = "packages/vesper-ffi-lowering/rewrite";

/// The targets a `@TARGET@` placeholder takes, with their `abi`.
const TARGETS: [(&str, &str); 4] = [
    ("x86_64-unknown-linux-gnu", "sysv64"),
    ("aarch64-unknown-linux-gnu", "aapcs64"),
    ("aarch64-apple-darwin", "darwin64"),
    ("x86_64-pc-windows-msvc", "win64"),
];

const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn root() -> PathBuf {
    repo()
}

// ---------------------------------------------------------------- the dump

/// Writes the canonical dump of a module. Keep in step with
/// `packages/vesper-ast/src/dump.ves`.
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

// ---------------------------------------------------------------- one case

/// One differential case: a source and the libraries it imports.
struct Case {
    label: String,
    source: String,
    libraries: Vec<(LibraryDescriptor, Handler)>,
}

impl Case {
    fn imports(&self) -> Result<Imports, FfiError> {
        let mut imports = Imports::new();
        for (d, h) in &self.libraries {
            imports.add_descriptor(d.clone(), *h)?;
        }
        Ok(imports)
    }
}

fn kind_name(e: &FfiError) -> String {
    format!("{:?}", e.kind)
}

fn err(e: &FfiError) -> String {
    let span = e
        .span
        .as_ref()
        .map_or_else(|| " -".to_owned(), |s| format!(" @{}..{}", s.start, s.end));
    format!("{}{span} {}", kind_name(e), q(&e.message))
}

fn lengths(v: &[usize]) -> String {
    let parts: Vec<String> = v
        .iter()
        .map(|&n| {
            if n == plan_probe::SOURCE_SLICE {
                "s".to_owned()
            } else {
                n.to_string()
            }
        })
        .collect();
    format!("[{}]", parts.join(","))
}

/// One pass's text (see the module documentation).
fn pass_text(o: &mut String, label: &str, module: &Module, r: &plan_probe::Rewritten, two: bool) {
    match &r.error {
        Some(e) => writeln!(o, "{label} err {}", err(e)).unwrap(),
        None => writeln!(o, "{label} ok").unwrap(),
    }
    if !two {
        for (call, slices) in &r.sites {
            writeln!(o, "site{}", sp(call)).unwrap();
            for (span, element, nullable, in_place) in slices {
                writeln!(
                    o,
                    " slice{} {element} nullable={} in_place={}",
                    sp(span),
                    b(*nullable),
                    b(*in_place)
                )
                .unwrap();
            }
        }
    } else {
        for ((library, symbol), set) in &r.instances {
            for v in set {
                writeln!(o, "instance {} {} {}", q(library), q(symbol), lengths(v)).unwrap();
            }
        }
        for c in &r.calls {
            let handler = match c.handler {
                Handler::Direct => "direct",
                Handler::IsolatedMpk => "isolated(mpk)",
            };
            writeln!(
                o,
                "call{} {} {} {handler} {}",
                sp(&c.span),
                q(&c.library),
                q(&c.symbol),
                lengths(&c.slice_lengths)
            )
            .unwrap();
        }
    }
    o.push_str(&dump_module(module));
}

/// The Rust text of a case, and the pass-2 arities it gave the Vesper side
/// (`None` when pass 2 does not run).
fn rust_text(case: &Case) -> (String, Option<plan_probe::Arities>) {
    let imports = case.imports().expect("corpus libraries import");
    let mut o = String::new();
    let generated = match plan_probe::generated(&imports) {
        Ok(g) => g,
        Err(e) => {
            writeln!(o, "G err {}", err(&e)).unwrap();
            return (o, None);
        }
    };
    let module = vesper::parser::parse(&case.source).module;
    let mut first = module.clone();
    let r1 = plan_probe::rewrite(&mut first, &imports, &generated, None);
    pass_text(&mut o, "pass1", &first, &r1, false);
    if r1.error.is_some() {
        return (o, None);
    }
    let Ok(arities) = plan_probe::arities(&module, &imports, &generated) else {
        return (o, None);
    };
    let mut second = module;
    let r2 = plan_probe::rewrite(&mut second, &imports, &generated, Some(&arities));
    pass_text(&mut o, "pass2", &second, &r2, true);
    (o, Some(arities))
}

fn input(case: &Case, arities: Option<&plan_probe::Arities>) -> Vec<u8> {
    let mut w: Vec<u8> = vec![];
    let word = |w: &mut Vec<u8>, x: u64| w.extend_from_slice(&x.to_le_bytes());
    let mut libs = case.libraries.clone();
    libs.sort_by(|a, b| a.0.name().cmp(b.0.name()));
    word(&mut w, libs.len() as u64);
    for (d, h) in &libs {
        word(
            &mut w,
            match h {
                Handler::Direct => 0,
                Handler::IsolatedMpk => 1,
            },
        );
        let bytes = d.canonical_bytes();
        word(&mut w, bytes.len() as u64);
        w.extend_from_slice(bytes);
    }
    match arities {
        None => word(&mut w, 0),
        Some(a) => {
            word(&mut w, 1);
            word(&mut w, a.len() as u64);
            for ((start, end), v) in a {
                word(&mut w, *start as u64);
                word(&mut w, *end as u64);
                word(&mut w, v.len() as u64);
                for &n in v {
                    word(&mut w, n as u64);
                }
            }
        }
    }
    w
}

// ---------------------------------------------------------------- Vesper

/// The package's `ffi_rewrite_text` entry, compiled once (unoptimized
/// unless `CHECK_HARNESS_SPEED` is set: the output does not depend on it).
fn text_entry() -> &'static native::WordFunction {
    static F: OnceLock<native::WordFunction> = OnceLock::new();
    F.get_or_init(|| compile("ffi_rewrite_text", speed()))
}

/// The optimized `ffi_rewrite_count` entry, for timing.
fn count_entry() -> &'static native::WordFunction {
    static F: OnceLock<native::WordFunction> = OnceLock::new();
    F.get_or_init(|| compile("ffi_rewrite_count", true))
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

fn call(f: &native::WordFunction, input: &[u8], source: &str) -> u64 {
    let a = [input.len() as u64, 0, input.as_ptr() as u64];
    let s = [source.len() as u64, 0, source.as_ptr() as u64];
    // SAFETY: the entry takes two `in Str`s, each the address of its three
    // words `{len, 0, data}`, which live across the call, as do `input` and
    // `source`.
    unsafe { f.call(&[a.as_ptr() as u64, s.as_ptr() as u64]) }.unwrap()
}

fn vesper_text(case: &Case, arities: Option<&plan_probe::Arities>) -> String {
    let i = input(case, arities);
    unpack_text(&take_words(call(text_entry(), &i, &case.source)))
}

/// Compares one case; returns the Rust text and the first difference.
fn compare(case: &Case) -> (String, Option<String>) {
    let (r, arities) = rust_text(case);
    let v = vesper_text(case, arities.as_ref());
    if r == v {
        return (r, None);
    }
    let d = first_difference(&r, &v).unwrap_or_else(|| "texts differ in trailing bytes".into());
    (r, Some(d))
}

// ---------------------------------------------------------------- corpus

/// The Rust test files whose string literals hold `.vsys` libraries and
/// the Vesper sources that call them.
const FFI_TESTS: [&str; 9] = [
    "tests/ffi_lowering.rs",
    "tests/ffi_handles.rs",
    "tests/ffi_affine_drops.rs",
    "tests/borrowed_box_match.rs",
    "tests/package_submodule_exports.rs",
    "tests/tree_abi.rs",
    "tests/vsys_packages.rs",
    "tests/dyn_effects.rs",
    "tests/test_runner_cli.rs",
];

/// Every library the corpus knows, first descriptor per name (host
/// target), from the `.vsys` files, the FFI test suites and the
/// hand-written libraries.
fn library_pool() -> Vec<LibraryDescriptor> {
    let mut sources = vec![];
    let mut files = vec![];
    walk(&root().join("tests"), &mut files, "vsys");
    walk(&root().join("packages"), &mut files, "vsys");
    walk(&root().join("crates"), &mut files, "vsys");
    files.sort();
    for f in &files {
        sources.push(std::fs::read_to_string(f).unwrap_or_default());
    }
    for t in FFI_TESTS {
        let text = std::fs::read_to_string(root().join(t)).unwrap_or_default();
        sources.extend(embedded_sources(&text));
    }
    sources.extend(HANDWRITTEN.iter().map(|s| s.to_string()));
    let mut pool: Vec<LibraryDescriptor> = vec![];
    for s in &sources {
        if let Ok(d) = sys::parse(&fill(s, TARGETS[0]), sys::Limits::default())
            && !pool.iter().any(|p| p.name() == d.name())
        {
            pool.push(d);
        }
    }
    pool
}

/// The libraries `source` names (`Ffi[Lib` or `Lib::`).
fn named_libraries(source: &str, pool: &[LibraryDescriptor]) -> Vec<LibraryDescriptor> {
    pool.iter()
        .filter(|d| {
            let n = d.name();
            source.contains(&format!("Ffi[{n}")) || source.contains(&format!("{n}::"))
        })
        .cloned()
        .collect()
}

/// Hand-written sources for the paths the suites do not reach, with the
/// library they import.
const HAND_SOURCES: &[(&str, &str)] = &[
    (
        "Shapes",
        r#"
fn handles() -> u64 eff [Ffi[Shapes]] {
    let c = Shapes::conn_new();
    let v = Shapes::view_get();
    let d = Conn { raw: 1 };
    let e = Conn;
    match c { Conn { raw } => raw, }
}
fn shapes() -> u64 eff [Ffi[Shapes]] {
    let a = Shapes::get_addr();
    let m = Shapes::mode(Mode::On);
    let w = Shapes::wide();
    let o = Shapes::ok();
    let n = Shapes::name(in "x".as_bytes());
    let t = Shapes::ints(in (1, 2), inout (3,), in (), in (4, 5, 6), in Some((7,)));
    let u = Shapes::ints(in (1, 2), inout (3,), in (), in (4,), in None);
    let x = Shapes::flats(in (), inout None, in (1, 2));
    let y = Shapes::sints(in (1,), in (2, 3), in ());
    let z = Shapes::sints(in (1,), in (2, 3));
    1
}
fn closures() -> u64 eff [Ffi[Shapes]] {
    let f = |x: u64| -> u64 eff [Ffi[Shapes]] { Shapes::sz(x) };
    let g: fn(u64) -> u64 eff [Ffi[Shapes]] = f;
    for i in 0..3 { Shapes::sz(i); }
    loop { break Shapes::sz(1); }
    if true { Shapes::sz(2) } else { Shapes::sz(3) }
}
"#,
    ),
    (
        "Bridge",
        r#"
copy struct Holder { f: fn(u64) -> u64 eff [Ffi[Bridge]] }
effect Probe { fn probe(x: u64) -> u64 eff [Ffi[Bridge]]; }
trait Tick { fn tick(self) -> u64 eff [Ffi[Bridge]]; }
impl Holder { fn go(self) -> u64 eff [Ffi[Bridge]] { Bridge::bridge_tick() } }
fn rows() -> u64 eff [Ffi[Bridge], Ffi[Nope], Ffi[Bridge, Bridge], Ffi[Bridge[u8]]] { 1 }
fn labelled() -> u64 eff [b: Ffi[Bridge]] { 1 }
fn realtime() -> u64 eff [Realtime, Ffi[Bridge]] { Bridge::bridge_tick() }
fn values() -> u64 eff [Ffi[Bridge]] {
    let f = Bridge::bridge_tick;
    let g = Bridge::nope(1);
    let h = Other::bridge_tick();
    let s = Bridge::bridge_copy(in (1, 2), inout (0, 0));
    let t = Bridge::bridge_copy(in (1, 2));
    let o = Bridge::bridge_opt_len(in Some((1, 2, 3)));
    let p = Bridge::bridge_opt_len(in None);
    let q = Bridge::bridge_rec_total(in (Rec { id: 1, value: 2 },));
    handle Ffi[Bridge] in { 1 } with { return x => x };
    handle Bridge in { 1 } with { return x => x };
    Bridge::bridge_tick()
}
"#,
    ),
];

/// Textual mutants of the Bridge fixture that make each rewriter error
/// fire inside a real module.
const MUTATIONS: &[(&str, &str)] = &[
    ("eff [Ffi[Bridge]]", "eff [Ffi[Brige]]"),
    ("eff [Ffi[Bridge]]", "eff [Ffi[Bridge, Bridge]]"),
    ("eff [Ffi[Bridge]]", "eff [l: Ffi[Bridge]]"),
    ("eff [Ffi[Bridge]]", "eff [Realtime, Ffi[Bridge]]"),
    ("Bridge::bridge_tick()", "Bridge::bridge_tock()"),
    ("Bridge::bridge_tick()", "Bridge::bridge_tick"),
    ("Bridge::bridge_sum(40, 2)", "Bridge::bridge_sum(40)"),
];

fn mutants(source: &str) -> Vec<String> {
    let mut out = vec![];
    for (from, to) in MUTATIONS {
        let mut at = 0;
        let mut k = 0;
        while let Some(i) = source[at..].find(from) {
            let i = at + i;
            if k % 3 == 0 {
                out.push(format!("{}{to}{}", &source[..i], &source[i + from.len()..]));
            }
            k += 1;
            at = i + from.len();
        }
    }
    out
}

fn host(d: &LibraryDescriptor, h: Handler) -> (LibraryDescriptor, Handler) {
    (d.clone(), h)
}

/// The FFI corpus: suites, fixtures, hand-written cases and mutants.
fn ffi_corpus() -> Vec<Case> {
    let pool = library_pool();
    let mut out = vec![];
    let mut push = |label: String, source: String, libs: Vec<(LibraryDescriptor, Handler)>| {
        let case = Case {
            label,
            source,
            libraries: libs,
        };
        if case.imports().is_ok() {
            out.push(case);
        }
    };
    let mut sources: Vec<(String, String)> = vec![];
    for t in FFI_TESTS {
        let text = std::fs::read_to_string(root().join(t)).unwrap_or_default();
        for (i, s) in rust_string_literals(&text).into_iter().enumerate() {
            if !s.contains("sys library") {
                sources.push((format!("{t}#{i}"), s));
            }
        }
    }
    let mut files = vec![];
    walk(&root().join("tests/fixtures"), &mut files, "ves");
    files.sort();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        sources.push((f.display().to_string(), text));
    }
    let bridge_fixture =
        std::fs::read_to_string(root().join("tests/fixtures/ffi_lowering/bridge.ves"))
            .expect("bridge fixture");
    for (i, m) in mutants(&bridge_fixture).into_iter().enumerate() {
        sources.push((format!("bridge mutant {i}"), m));
    }
    for (label, source) in sources {
        let libs = named_libraries(&source, &pool);
        if libs.is_empty() {
            continue;
        }
        let direct: Vec<_> = libs.iter().map(|d| host(d, Handler::Direct)).collect();
        let mpk: Vec<_> = libs.iter().map(|d| host(d, Handler::IsolatedMpk)).collect();
        push(label.clone(), source.clone(), direct);
        push(format!("{label} (mpk)"), source, mpk);
    }
    for (library, source) in HAND_SOURCES {
        let d = pool
            .iter()
            .find(|d| d.name() == *library)
            .unwrap_or_else(|| panic!("library {library}"));
        push(
            format!("hand {library}"),
            source.to_string(),
            vec![host(d, Handler::Direct)],
        );
    }
    out
}

/// Every `FFI_REWRITE_STEP`-th (default 1) `.ves` file of the repository,
/// with the Bridge library.
fn repository_corpus() -> Vec<Case> {
    let pool = library_pool();
    let bridge = pool
        .iter()
        .find(|d| d.name() == "Bridge")
        .expect("Bridge")
        .clone();
    let step: usize = std::env::var("FFI_REWRITE_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let mut files = vec![];
    for dir in ["packages", "examples", "crates", "tests"] {
        walk(&root().join(dir), &mut files, "ves");
    }
    files.sort();
    files
        .iter()
        .step_by(step)
        .filter_map(|f| {
            let source = std::fs::read_to_string(f).ok()?;
            Some(Case {
                label: f.display().to_string(),
                source,
                libraries: vec![(bridge.clone(), Handler::Direct)],
            })
        })
        .collect()
}

fn run(cases: &[Case]) -> (usize, BTreeSet<String>, usize) {
    let mut bad = 0;
    let mut kinds = BTreeSet::new();
    let mut lines = 0;
    for case in cases {
        let (r, d) = compare(case);
        lines += r.lines().count();
        for l in r.lines() {
            if let Some(rest) = l
                .strip_prefix("pass1 err ")
                .or_else(|| l.strip_prefix("pass2 err "))
                .or_else(|| l.strip_prefix("G err "))
            {
                kinds.insert(rest.split(' ').next().unwrap_or("").to_owned());
            }
            for key in ["site @", "instance ", "call @"] {
                if l.starts_with(key) {
                    kinds.insert(key.trim().to_owned());
                }
            }
            if l.starts_with("instance ") && l.ends_with(" []") {
                kinds.insert("instance []".to_owned());
            }
            if l.starts_with("call @") && l.ends_with("s]") {
                kinds.insert("call lent".to_owned());
            }
        }
        if let Some(d) = d {
            bad += 1;
            if bad <= 5 {
                eprintln!("{}: {d}", case.label);
            }
        }
    }
    (bad, kinds, lines)
}

#[test]
fn rewriter_matches_rust_on_the_ffi_corpus() {
    let mut cases = ffi_corpus();
    if let Ok(only) = std::env::var("FFI_REWRITE_ONLY") {
        cases.retain(|c| c.label.contains(&only));
    }
    let (bad, kinds, lines) = run(&cases);
    eprintln!(
        "{} cases, {lines} lines; {bad} differ; reached {kinds:?}",
        cases.len()
    );
    assert_eq!(bad, 0, "{bad} of {} cases differ", cases.len());
    if std::env::var_os("FFI_REWRITE_ONLY").is_none() {
        for key in [
            "MalformedCapability",
            "UnknownLibrary",
            "Realtime",
            "ForeignValue",
            "HandleMisuse",
            "ForeignHandler",
            "UnknownForeignFunction",
            "UnsupportedType",
            "site @",
            "instance",
            "instance []",
            "call @",
            "call lent",
        ] {
            assert!(kinds.contains(key), "{key} never fires: {kinds:?}");
        }
    }
}

#[test]
fn rewriter_matches_rust_on_the_repository_sources() {
    if std::env::var_os("TEST_SRCDIR").is_some() {
        // Under Bazel the repository's sources are not all runfiles.
        return;
    }
    let cases = repository_corpus();
    let (bad, _, lines) = run(&cases);
    eprintln!("{} files, {lines} lines; {bad} differ", cases.len());
    assert!(cases.len() > 100, "{} files", cases.len());
    assert_eq!(bad, 0, "{bad} of {} files differ", cases.len());
}

/// ABBA timing of both passes (parse, rewrite, text) in Vesper against the
/// Rust passes, over the FFI corpus.
#[test]
#[ignore = "timing; run with CHECK_HARNESS_SPEED=1 and --ignored"]
fn rewriter_speed() {
    let cases = ffi_corpus();
    let prepared: Vec<(&Case, Vec<u8>)> = cases
        .iter()
        .map(|c| {
            let (_, a) = rust_text(c);
            (c, input(c, a.as_ref()))
        })
        .collect();
    let entry = count_entry();
    let (r, v) = abba(
        "ffi rewriter",
        &mut || {
            time(|| {
                for c in &cases {
                    std::hint::black_box(rust_text(c));
                }
            })
        },
        &mut || {
            time(|| {
                for (c, i) in &prepared {
                    std::hint::black_box(call(entry, i, &c.source));
                }
            })
        },
    );
    eprintln!(
        "rust {r:?} vesper {v:?} ({:.2}x)",
        v.as_secs_f64() / r.as_secs_f64()
    );
}

/// A `.vsys` source with its placeholders filled for `target`.
fn fill(source: &str, target: (&str, &str)) -> String {
    source
        .replace("@DIGEST@", ZERO_DIGEST)
        .replace("@TARGET@", target.0)
        .replace("@ABI@", target.1)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, ext: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if !matches!(name.to_str(), Some("target" | ".git" | "node_modules"))
                && !name.to_string_lossy().starts_with("bazel-")
            {
                walk(&path, out, ext);
            }
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

/// `sys library` sources embedded in a Rust file: from the keyword to the
/// brace that closes the library body, with string escapes and format
/// braces undone (both spellings are tried).
fn embedded_sources(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut from = 0;
    while let Some(at) = text[from..].find("sys library") {
        let start = from + at;
        from = start + 1;
        let bytes = text.as_bytes();
        let mut i = start;
        let mut depth = 0i32;
        let mut opened = false;
        let mut paren = 0i32;
        while i < bytes.len() {
            match bytes[i] {
                b'(' => paren += 1,
                b')' => paren -= 1,
                b'{' if paren == 0 => {
                    depth += 1;
                    opened = true;
                }
                b'}' if paren == 0 => {
                    depth -= 1;
                }
                _ => {}
            }
            i += 1;
            if opened && depth <= 0 {
                break;
            }
        }
        let raw = &text[start..i.min(text.len())];
        let unescaped = raw
            .replace("\\n", "\n")
            .replace("\\\"", "\"")
            .replace("\\\n", "\n");
        out.push(unescaped.clone());
        out.push(unescaped.replace("{{", "{").replace("}}", "}"));
    }
    out
}
/// Hand-written libraries for the paths the repository's do not reach:
/// address, unit and handle parameters, pointer results, nested and
/// byte-array structs, every slice and pointer form, `errno`, `CStr`,
/// reserved and duplicate names.
const HANDWRITTEN: [&str; 6] = [
    r#"sys library Shapes (
    target = "x86_64-unknown-linux-gnu", abi = "sysv64",
    link = "dynamic", name = "shapes", artifact = "blake3:@DIGEST@",
    header = "shapes.h", pkg_config = "shapes ^1.2.3"
) {
    enum Mode: u8 { Off = 0, On = 3 }
    enum Wide: u64 { A = 1, B = 18446744073709551615 }
    wire struct Inner { a: u16le, b: i16be, pad: ZeroPad[4] }
    wire struct Outer { tag: ByteArray[4], inner: Inner, n: i32, z: i8, y: u8, w: u16, v: i64be }
    wire struct Flat { a: u64, b: i64, c: u64 }
    linear extern type Conn;
    affine extern type View drop view_free;
    fn view_free(sink v: View) -> ForeignVal[Unit];
    fn get_addr() -> ForeignVal[Ptr[u32]];
    fn mode(m: Mode) -> ForeignVal[Mode];
    fn wide() -> ForeignVal[Wide];
    fn ok() -> ForeignVal[bool] errno;
    fn sz(n: usize) -> ForeignVal[usize] errno;
    fn name(nocapture in s: CStr) -> ForeignVal[u32];
    fn names(nocapture in a: CStr, nocapture in b: CStr) -> ForeignVal[Mode] errno;
    fn outer(nocapture in o: Outer, nocapture out p: Outer, nocapture inout q: Option[Outer]) -> ForeignVal[u64];
    fn flats(nocapture in a: CSlice[Flat], nocapture inout b: Option[CSlice[Flat]], nocapture in c: CSlice[u16]) -> ForeignVal[Unit];
    fn ints(nocapture in a: CSlice[u32], nocapture inout b: CSlice[i64], nocapture in c: CSlice[usize], nocapture in d: CSlice[u64], nocapture in e: Option[CSlice[u8]]) -> ForeignVal[i16];
    fn sints(nocapture in a: CSlice[i8], nocapture in b: CSlice[i32], nocapture in c: CSlice[Inner]) -> ForeignVal[u16];
    fn ptrs(nocapture in a: i8, nocapture inout b: Option[i64], nocapture out c: usize) -> ForeignVal[i8];
    fn conn_new() -> ForeignVal[Conn];
    fn conn_maybe() -> ForeignVal[Option[Conn]];
    fn conn_must() -> ForeignVal[NonNull[Conn]];
    fn conn_use(in c: Conn, inout d: Conn, nocapture in data: CSlice[u8]) -> ForeignVal[u32] errno;
    fn conn_free(sink c: Conn) -> ForeignVal[Unit];
    fn view_get() -> ForeignVal[Option[View]];
}"#,
    r#"sys library Reserved (
    target = "aarch64-apple-darwin", abi = "darwin64",
    link = "static", name = "reserved", artifact = "blake3:@DIGEST@"
) {
    wire struct Result { a: u32 }
    fn f(nocapture in r: Result) -> ForeignVal[u32];
}"#,
    r#"sys library Clash (
    target = "x86_64-pc-windows-msvc", abi = "win64",
    link = "static", name = "clash", artifact = "blake3:@DIGEST@"
) {
    enum Clash: u32 { A = 1 }
    fn f() -> ForeignVal[Clash];
}"#,
    r#"sys library Twin (
    target = "x86_64-unknown-linux-gnu", abi = "sysv64",
    link = "static", name = "twin", artifact = "blake3:@DIGEST@"
) {
    enum Mode: u8 { Off = 0, On = 3 }
    wire struct Flat { a: u64, b: i64, c: u64 }
    affine extern type Peer;
    fn twin(nocapture in f: Flat, m: Mode) -> ForeignVal[Option[Peer]];
}"#,
    r#"sys library Skew (
    target = "aarch64-unknown-linux-gnu", abi = "aapcs64",
    link = "static", name = "skew", artifact = "blake3:@DIGEST@"
) {
    enum Mode: u8 { Off = 0, Half = 1, On = 3 }
    wire struct Flat { a: u64, b: i64 }
    linear extern type Conn;
    fn skew(nocapture in f: Flat) -> ForeignVal[Mode];
    fn skew_free(sink c: Conn) -> ForeignVal[Unit];
}"#,
    r#"sys library Invalid (
    target = "x86_64-unknown-linux-gnu", abi = "sysv64",
    link = "static", name = "invalid", artifact = "blake3:@DIGEST@"
) {
    wire struct ForeignInvalid { raw: u64 }
    fn f(nocapture in x: ForeignInvalid) -> ForeignVal[bool];
}"#,
];

/// The string literals of a Rust test file that look like Vesper sources
/// (as in tests/self_host_front_v2.rs).
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
