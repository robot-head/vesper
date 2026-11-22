//! Self-hosting SH2 lane 6 (#1178): the package assembler of
//! `src/packages.rs` (`Loader::assemble`, `Resolver`, `Renamer`) in
//! `packages/vesper-check-names-packages` against Rust.
//!
//! For every Loom package of the corpus (each directory with a `Loom.toml`
//! under `tests/fixtures`, `examples`, `packages` and `crates`, including
//! the loader's reject fixtures) and for mutant package graphs that make
//! each of the loader's diagnostics fire, Rust's `packages::load` is the
//! reference. The test mirrors the loader's I/O (`Loader::package`: the
//! manifests, the dependency closure and the module files, which stay in
//! Rust) to build the Vesper input, and both sides write one text:
//! the assembler's diagnostics (code, span, message and help), the
//! qualified-name map, the public items, and every module's renamed items,
//! trait bounds and model uses in the canonical AST dump of
//! `packages/vesper-ast/src/dump.ves` (the Rust dumper below is a copy of
//! tests/self_host_ast.rs's). The texts must be equal.
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use vesper::ast::*;
use vesper::packages;
use vesper_syntax::parser;

#[path = "common/check_harness.rs"]
mod check_harness;
use check_harness::{Entry, Package, abba, first_difference, repo, speed, time};

const PACKAGE: &str = "packages/vesper-check-names-packages";

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

// ---------------------------------------------------------------- the graph

/// One package of the loaded graph, as `Loader::package` builds it.
struct GraphPackage {
    name: String,
    deps: Vec<(String, usize)>,
    optional: Vec<String>,
}

/// One module: its package, path below the package root and file.
struct GraphModule {
    package: usize,
    path: Vec<String>,
    file: usize,
}

#[derive(Default)]
struct Graph {
    packages: Vec<GraphPackage>,
    by_path: BTreeMap<PathBuf, usize>,
    modules: Vec<GraphModule>,
    files: usize,
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(kind) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if kind.is_dir() {
            walk(&entry.path(), out);
        } else if kind.is_file() && name.ends_with(".ves") {
            out.push(entry.path());
        }
    }
}

fn is_module_segment(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl Graph {
    /// `Loader::package` without the checks whose failures stop assembly
    /// (those are manifest errors, and such graphs are not compared).
    fn package(&mut self, dir: &Path, stack: &mut Vec<PathBuf>) -> Option<usize> {
        let canonical = std::fs::canonicalize(dir).ok()?;
        if stack.contains(&canonical) {
            return None;
        }
        if let Some(&i) = self.by_path.get(&canonical) {
            return Some(i);
        }
        let text = std::fs::read_to_string(dir.join("Loom.toml")).ok()?;
        let manifest = vesper_loom_calculus::parse_manifest(&text).ok()?;
        let src = dir.join("src");
        let mut files = vec![];
        if src.is_dir() {
            walk(&src, &mut files);
        }
        let index = self.packages.len();
        for path in files {
            let relative = path.strip_prefix(&src).unwrap().with_extension("");
            let mut segments: Vec<String> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if segments.len() == 1 && matches!(segments[0].as_str(), "lib" | "main") {
                segments.clear();
            }
            let file = self.files;
            self.files += 1;
            if segments.iter().all(|s| is_module_segment(s)) {
                self.modules.push(GraphModule {
                    package: index,
                    path: segments,
                    file,
                });
            }
        }
        self.packages.push(GraphPackage {
            name: manifest.package.name.as_str().to_owned(),
            deps: vec![],
            optional: manifest
                .dependencies
                .iter()
                .filter(|(_, d)| d.optional)
                .map(|(n, _)| n.as_str().to_owned())
                .collect(),
        });
        self.by_path.insert(canonical.clone(), index);
        stack.push(canonical);
        for (dep_name, dep) in manifest.dependencies.iter().filter(|(_, d)| !d.optional) {
            let Some(path) = &dep.path else { continue };
            let dep_dir = dir.join(path);
            if !dep_dir.join("Loom.toml").is_file() {
                continue;
            }
            let Some(d) = self.package(&dep_dir, stack) else {
                continue;
            };
            if self.packages[d].name != dep_name.as_str() {
                continue;
            }
            self.packages[index]
                .deps
                .push((dep_name.as_str().to_owned(), d));
        }
        stack.pop();
        Some(index)
    }
}

// ---------------------------------------------------------------- the input

fn put(out: &mut Vec<u8>, x: u64) {
    out.extend(x.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put(out, s.len() as u64);
    out.extend(s.as_bytes());
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
}

/// The input of `assemble_text` (packages/vesper-check-names-packages/src/load.ves).
fn encode(g: &Graph, a: &packages::Assembled) -> Vec<u8> {
    let mut out = vec![];
    put(&mut out, g.packages.len() as u64);
    for p in &g.packages {
        put_str(&mut out, &p.name);
        put(&mut out, p.deps.len() as u64);
        for (n, d) in &p.deps {
            put_str(&mut out, n);
            put(&mut out, *d as u64);
        }
        put(&mut out, p.optional.len() as u64);
        for n in &p.optional {
            put_str(&mut out, n);
        }
    }
    put(&mut out, a.sources.files.len() as u64);
    for f in &a.sources.files {
        put_str(&mut out, &f.display);
        put(&mut out, f.base as u64);
    }
    put(&mut out, g.modules.len() as u64);
    for m in &g.modules {
        put(&mut out, m.package as u64);
        put(&mut out, m.path.len() as u64);
        for s in &m.path {
            put_str(&mut out, s);
        }
        put(&mut out, m.file as u64);
        put_str(&mut out, &a.sources.files[m.file].text);
    }
    out
}

// ---------------------------------------------------------------- the texts

fn diag_line(out: &mut String, file: Option<usize>, d: &Diagnostic) {
    assert!(d.related.is_empty(), "the loader writes no labels: {d:?}");
    write!(
        out,
        "diag {} {} {}..{} {}",
        file.map_or(u64::MAX, |f| f as u64),
        d.code,
        d.span.start,
        d.span.end,
        d.message
    )
    .unwrap();
    if let Some(h) = &d.help {
        write!(out, " help: {h}").unwrap();
    }
    out.push('\n');
}

/// Rust's text: `packages::load`'s assembler diagnostics (after the parse
/// diagnostics, which the loader reports first), qualified names, public
/// items and each module's slice of the assembled module.
fn rust_text(g: &Graph, a: &packages::Assembled) -> String {
    let parsed: Vec<_> = g
        .modules
        .iter()
        .map(|m| parser::parse(&a.sources.files[m.file].text))
        .collect();
    let skip: usize = parsed.iter().map(|p| p.diagnostics.len()).sum();
    let mut out = String::new();
    for d in &a.diagnostics[skip..] {
        diag_line(&mut out, d.file, &d.diagnostic);
    }
    for (k, v) in &a.sources.qualified {
        writeln!(out, "qualified {k} {v}").unwrap();
    }
    for p in &a.module.public_items {
        writeln!(out, "public {p}").unwrap();
    }
    let (mut items, mut uses, mut bounds) = (0, 0, 0);
    for p in &parsed {
        let m = &p.module;
        let slice = Module {
            items: a.module.items[items..items + m.items.len()].to_vec(),
            model_uses: a.module.model_uses[uses..uses + m.model_uses.len()].to_vec(),
            trait_bounds: a.module.trait_bounds[bounds..bounds + m.trait_bounds.len()].to_vec(),
            ..Module::default()
        };
        items += m.items.len();
        uses += m.model_uses.len();
        bounds += m.trait_bounds.len();
        out.push_str(&dump_module(&slice));
    }
    assert_eq!(items, a.module.items.len());
    out
}

/// The qualified-name lines in byte order (Vesper writes them in its
/// table's order; Rust's `BTreeMap` iterates sorted).
fn normalize(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    let first = lines.iter().position(|l| l.starts_with("qualified "));
    if let Some(a) = first {
        let n = lines[a..]
            .iter()
            .take_while(|l| l.starts_with("qualified "))
            .count();
        lines[a..a + n].sort_unstable();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

// ---------------------------------------------------------------- the package

fn package() -> &'static Package {
    static PACKAGE_ONCE: OnceLock<Package> = OnceLock::new();
    PACKAGE_ONCE.get_or_init(|| Package::load(PACKAGE))
}

fn assembler() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| package().entry("assemble_text", true))
}

/// A package graph both sides can assemble: loaded with no manifest
/// errors, and mirrored with the same files.
struct Loaded {
    graph: Graph,
    assembled: packages::Assembled,
}

fn load(dir: &Path) -> Option<Loaded> {
    let assembled = packages::load(dir).ok()?;
    if !assembled.manifest_errors.is_empty() {
        return None;
    }
    let mut graph = Graph::default();
    graph.package(dir, &mut vec![])?;
    assert_eq!(
        graph.files,
        assembled.sources.files.len(),
        "{}: the mirrored loader found other files",
        dir.display()
    );
    Some(Loaded { graph, assembled })
}

/// The first difference between the two texts of a package, if any.
fn compare(l: &Loaded) -> Option<String> {
    let rust = normalize(&rust_text(&l.graph, &l.assembled));
    let vesper = normalize(&assembler().text(&encode(&l.graph, &l.assembled)));
    first_difference(&rust, &vesper)
}

/// Every Loom package directory of the corpus, in path order.
fn corpus_packages() -> Vec<PathBuf> {
    fn find(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.path());
        for e in entries {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if matches!(&*name, "target" | ".git" | "node_modules")
                    || name.starts_with("bazel-")
                {
                    continue;
                }
                if p.join("Loom.toml").exists() {
                    out.push(p.clone());
                }
                find(&p, out);
            }
        }
    }
    let mut out = vec![];
    for d in ["tests/fixtures", "examples", "packages", "crates"] {
        find(&repo().join(d), &mut out);
    }
    out
}

/// Every corpus package assembles to the same program, names and
/// diagnostics in Vesper as in Rust.
#[test]
fn corpus_packages_assemble_like_rust() {
    let dirs = corpus_packages();
    let mut compared = 0;
    let mut diagnostics = 0;
    let mut failures = vec![];
    for dir in &dirs {
        let Some(l) = load(dir) else { continue };
        compared += 1;
        diagnostics += l.assembled.diagnostics.len();
        if let Some(d) = compare(&l) {
            failures.push(format!(
                "{}: {d}",
                dir.strip_prefix(repo()).unwrap_or(dir).display()
            ));
        }
    }
    eprintln!(
        "names/packages: {} of {compared} packages differ ({} directories, {diagnostics} loader diagnostics)",
        failures.len(),
        dirs.len()
    );
    // Bazel's test data holds the same corpus (BUILD.bazel), crate ports
    // included: they are most of the value enum users.
    let floor = 75;
    assert!(compared >= floor, "{compared} packages compared");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ---------------------------------------------------------------- mutants

/// A temporary package graph: `(package dir, Loom.toml, [(path, source)])`.
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A package of a mutant graph: its directory, `Loom.toml` and sources.
type MutantPackage<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

fn graph(label: &str, packages: &[MutantPackage]) -> Temp {
    let dir = std::env::temp_dir().join(format!(
        "vesper-names-packages-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for (p, manifest, files) in packages {
        let root = dir.join(p);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Loom.toml"), manifest).unwrap();
        for (path, source) in *files {
            let file = root.join("src").join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, source).unwrap();
        }
    }
    Temp(dir)
}

fn manifest(name: &str, deps: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n{deps}"
    )
}

/// Mutants that make every assembler diagnostic fire, each compared.
#[test]
fn every_assembler_diagnostic_matches_rust() {
    let dep = r#""@demo/geo" = { version = "^0.1", path = "../geo" }
"@demo/opt" = { version = "^0.1", path = "../opt", optional = true }
"#;
    let app = manifest("@demo/app", dep);
    let geo = manifest("@demo/geo", "");
    let opt = manifest("@demo/opt", "");
    let geo_files: &[(&str, &str)] = &[
        (
            "lib.ves",
            "pub use self::shapes::{area, Point};\npub use self::shapes::area as size;\npub fn root_item() -> u64 { 1 }\n",
        ),
        (
            "shapes.ves",
            "pub struct Point { x: u64, y: u64 }\npub enum Shape { Dot, Line(u64) }\npub fn area(p: Point) -> u64 { p.x * p.y }\nfn hidden_helper() -> u64 { 2 }\npub effect Tick { fn tick() -> u64; }\n",
        ),
        (
            "util/clamp.ves",
            "pub fn clamp(x: u64) -> u64 { x }\npub fn up() -> u64 { super::super::root_item() }\n",
        ),
    ];
    let cases: &[(&str, &[(&str, &str)])] = &[
        // E0111: no module, no item (with and without the `.vsys` help),
        // not a dependency, an optional dependency, `super` above the root.
        (
            "imports",
            &[
                (
                    "lib.ves",
                    "use @demo/geo::nope::{f};\nuse @demo/geo::{Missing, missing};\nuse @demo/other::{x};\nuse @demo/opt::{y};\nuse super::{z};\nuse @demo/geo::shapes::{hidden_helper};\nuse @demo/geo::shapes::{Point, area as a, Shape};\nuse self::m::{f as g, f as g2, k};\nuse self::m::{k as a};\nfn f() -> u64 { 0 }\nuse self::m::{f};\n",
                ),
                (
                    "m.ves",
                    "pub fn f() -> u64 { 1 }\npub fn k() -> u64 { 2 }\npub enum Local { Dot }\n",
                ),
            ],
        ),
        // Qualified paths: module-only, missing module, missing nested
        // module, a dependency's item, `super` above the root, a variant of
        // an unimported enum and a root item reached unimported.
        (
            "paths",
            &[
                (
                    "lib.ves",
                    "pub fn top() -> u64 { 1 }\npub enum Colour { Red, Green }\n",
                ),
                (
                    "a/b.ves",
                    "fn f() -> u64 { self::nope::g() + self::a::nope::h() + super::super::super::x() + @demo/geo::shapes::area(@demo/geo::shapes::Point { x: 1, y: 2 }) }\nfn n() -> u64 { super::super() }\nfn g() -> u64 { top() }\nfn h(c: u64) -> u64 { match Shape::Dot { Dot => 1, _ => 2 } }\nfn k() -> Colour { Red }\nfn l() -> u64 { self::top() + super::b::f() + @demo/geo::area(@demo/geo::Point { x: 1, y: 1 }) + @demo/geo::size(@demo/geo::Point { x: 2, y: 2 }) }\nfn m(p: @demo/geo::Point) -> u64 { @demo/geo::shapes::Shape::Line(3); p.x }\n",
                ),
            ],
        ),
        // E0113: the same variant, operation, trait or model in two modules,
        // and one item re-exported by the root under two names.
        (
            "globals",
            &[
                (
                    "lib.ves",
                    "pub use self::x::{f as one, f as two};\npub use self::y::{Tick};\nenum A { Dot }\neffect Tick { fn tick() -> u64; }\ntrait Show { fn show(self) -> u64; }\n",
                ),
                (
                    "x.ves",
                    "pub fn f() -> u64 { 1 }\npub enum B { Dot }\npub effect Other { fn tick() -> u64; }\ntrait Show { fn show(self) -> u64; }\n",
                ),
                ("y.ves", "pub effect Tick { fn ping() -> u64; }\n"),
            ],
        ),
        // Nodes the AST lowering shares where Rust clones them, in a file
        // with a nonzero base: an array repetition's element, an
        // or-pattern arm's body and a generic impl's bound, each holding a
        // name that is reported (so a revisit's diagnostic span shows).
        (
            "shared",
            &[
                (
                    "lib.ves",
                    "pub fn top() -> u64 { 1 }\npub trait Show[T] { fn show(self) -> u64; }\n",
                ),
                (
                    "s.ves",
                    "struct W[K] { k: K }\nimpl [K: Show[self::Missing]] W[K] { fn a(self) -> u64 { 1 }\nfn b(self) -> u64 { 2 } }\nfn rep(x: u64) -> u64 { let a = [top(); 2]; match x { 1 | 2 => top(), _ => 0 } }\n",
                ),
            ],
        ),
        // Constants and value enums (#1239): imports of constants and of
        // whole value enums (typed and untyped), re-exported enums, root
        // re-exports under new names, qualified members, member patterns,
        // and E0112/E0113 for a private enum, an enum name both declared
        // and imported, and one local name imported from two enums.
        (
            "values",
            &[
                (
                    "lib.ves",
                    "use self::codes::{Ascii, LIMIT, Mode, Hidden, TABLE};\nuse self::codes::{Ascii};\nuse self::other::{Ascii as A2};\npub use self::codes::{LIMIT as CAP, Ascii as Byte};\nuse self::chain::{Ascii as Again};\nuse self::other::{Ascii};\nenum Local: u8 { X = 1 }\nuse self::codes::{Local};\nfn f(x: u8) -> u64 { match x { Ascii::Colon => 1, A2::Q => 2, Again::Digit0 => 3, _ => 0 } }\nfn g() -> u8 { Ascii::Colon }\nfn h() -> u64 { LIMIT }\nfn k() -> u8 { self::codes::Ascii::Digit0 }\nfn m() -> u64 { Mode::B }\nfn t(x: u64) -> u64 { match x { TABLE => 1, LIMIT => 2, _ => 0 } }\nfn q() -> u64 { self::codes::LIMIT }\n",
                ),
                (
                    "codes.ves",
                    "pub enum Ascii: u8 { Colon = 58, Digit0 = 48 }\npub const LIMIT: u64 = 10;\npub enum Mode: _ { A, B }\nenum Hidden: u8 { Z = 1 }\npub const TABLE: u64 = LIMIT * 2;\npub enum Local: u8 { Y = 2 }\nfn own(x: u8) -> u8 { match x { Ascii::Colon => Ascii::Digit0, _ => Hidden::Z } }\n",
                ),
                ("other.ves", "pub enum Ascii: u8 { Q = 1 }\n"),
                (
                    "chain.ves",
                    "pub use self::codes::{Ascii};\nfn c() -> u64 { CAP }\nfn d() -> u8 { super::codes::Ascii::Nope }\n",
                ),
            ],
        ),
        // Private items, private and public re-export chains, generics,
        // trait bounds, handlers, closures and impl blocks across modules.
        (
            "scopes",
            &[
                (
                    "lib.ves",
                    "use self::inner::{Pair, make, Counter};\npub use self::inner::{secret};\nfn run[T](x: T) -> Pair { let f = |y: u64| -> u64 { y }; make(f(1)) }\nfn handled() -> u64 { handle Counter in { Counter::next() } with { return value => value, next(k) => k(1) } }\nimpl Pair { fn first(self) -> u64 { self.a } }\n",
                ),
                (
                    "inner.ves",
                    "pub struct Pair { a: u64, b: u64 }\npub fn make(a: u64) -> Pair { Pair { a: a, b: a } }\nfn secret() -> u64 { 3 }\npub effect Counter { fn next() -> u64; }\npub fn generic[T: mod copy](x: T) -> T { let y: T = x; y }\n",
                ),
            ],
        ),
    ];
    let mut bad = vec![];
    let mut fired = std::collections::BTreeSet::new();
    for (label, files) in cases {
        let t = graph(
            label,
            &[
                ("app", &app, files),
                ("geo", &geo, geo_files),
                ("opt", &opt, &[("lib.ves", "pub fn y() -> u64 { 1 }\n")]),
            ],
        );
        let Some(l) = load(&t.0.join("app")) else {
            panic!("{label}: the mutant does not load");
        };
        for d in &l.assembled.diagnostics {
            fired.insert(d.diagnostic.code.clone());
            if std::env::var_os("NAMES_PACKAGES_SHOW").is_some() {
                eprintln!("{label}: {:?} {}", d.diagnostic.span, d.diagnostic.message);
            }
        }
        if let Some(d) = compare(&l) {
            bad.push(format!("{label}: {d}"));
        }
    }
    eprintln!("names/packages mutants: fired {fired:?}");
    for code in ["E0111", "E0112", "E0113"] {
        assert!(fired.contains(code), "no mutant fired {code}");
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ---------------------------------------------------------------- speed

fn assemble_only() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| package().entry("assemble_only", true))
}

fn parser_entry() -> &'static Entry {
    static ENTRY: OnceLock<Entry> = OnceLock::new();
    ENTRY.get_or_init(|| package().entry("parse_modules", true))
}

/// The packages the timings use: the largest of the repository.
const TIMED: [&str; 4] = [
    "packages/vesper-compiler",
    "packages/vesper-ast",
    "packages/vesper-check-data",
    "packages/vesper-std/collections",
];

#[inline(never)]
fn rust_load(dir: &Path) -> usize {
    packages::load(dir).unwrap().module.items.len()
}

#[inline(never)]
fn rust_parse(l: &Loaded) -> usize {
    l.graph
        .modules
        .iter()
        .map(|m| {
            parser::parse(&l.assembled.sources.files[m.file].text)
                .module
                .items
                .len()
        })
        .sum()
}

#[inline(never)]
fn vesper_assemble(input: &[u8]) -> u64 {
    assemble_only().word(input)
}

#[inline(never)]
fn vesper_parse(input: &[u8]) -> u64 {
    parser_entry().word(input)
}

/// ABBA timing against Rust (`CHECK_HARNESS_SPEED=1`): Rust's
/// `packages::load` (read, parse, assemble) against the Vesper assembler
/// (decode, parse every module twice, assemble; no dump), and each side's parse
/// alone, so the assembler's own cost is the difference.
#[test]
#[ignore]
fn speed_against_rust() {
    if !speed() {
        return;
    }
    for dir in TIMED {
        let d = repo().join(dir);
        let Some(l) = load(&d) else { continue };
        let input = encode(&l.graph, &l.assembled);
        let (r, v) = abba(
            dir,
            &mut || {
                time(|| {
                    std::hint::black_box(rust_load(&d));
                })
            },
            &mut || {
                time(|| {
                    std::hint::black_box(vesper_assemble(&input));
                })
            },
        );
        let (rp, vp) = abba(
            dir,
            &mut || {
                time(|| {
                    std::hint::black_box(rust_parse(&l));
                })
            },
            &mut || {
                time(|| {
                    std::hint::black_box(vesper_parse(&input));
                })
            },
        );
        let ra = r.saturating_sub(rp);
        let va = v.saturating_sub(vp * 2);
        eprintln!(
            "{dir}: {} modules; whole: Rust {r:?} Vesper {v:?} ({:.2}x); parse: Rust {rp:?} Vesper {vp:?} ({:.2}x); assembler alone: Rust {ra:?} Vesper {va:?} ({:.2}x)",
            l.graph.modules.len(),
            v.as_secs_f64() / r.as_secs_f64(),
            vp.as_secs_f64() / rp.as_secs_f64(),
            va.as_secs_f64() / ra.as_secs_f64().max(1e-9),
        );
    }
}

/// One side of one timed package for callgrind
/// (`--toggle-collect='*self_host_check_names_packages::rust_load*'` and the
/// other measured functions): `NAMES_PACKAGES_SIDE` is `rust`, `rust-parse`,
/// `vesper` or `vesper-parse`, `NAMES_PACKAGES_DIR` the package.
#[test]
#[ignore]
fn callgrind_one_side() {
    let (Ok(side), Ok(dir)) = (
        std::env::var("NAMES_PACKAGES_SIDE"),
        std::env::var("NAMES_PACKAGES_DIR"),
    ) else {
        return;
    };
    let d = repo().join(dir);
    let l = load(&d).unwrap();
    let input = encode(&l.graph, &l.assembled);
    // Compile outside the measured call, and only what the side runs.
    match side.as_str() {
        "vesper" => drop(assemble_only()),
        "vesper-parse" => drop(parser_entry()),
        _ => {}
    }
    match side.as_str() {
        "rust" => drop(std::hint::black_box(rust_load(&d))),
        "rust-parse" => drop(std::hint::black_box(rust_parse(&l))),
        "vesper" => drop(std::hint::black_box(vesper_assemble(&input))),
        "vesper-parse" => drop(std::hint::black_box(vesper_parse(&input))),
        other => panic!("unknown side {other}"),
    }
}
