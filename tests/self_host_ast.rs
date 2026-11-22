//! Self-hosting SH1 lane 9 (#1152): the AST in `packages/vesper-ast`
//! against the Rust AST (`vesper_syntax::parser::parse(source).module`).
//! The Vesper package is compiled once, as optimized native code, and its
//! `ast_dump` entry runs on each source. Both ASTs are written in one
//! canonical text dump (one line per node, preorder, see [`dump_module`]
//! and `packages/vesper-ast/src/dump.ves`), which must be equal byte for
//! byte.
//!
//! The corpus is every `.ves` file in the repository, the source strings of
//! the parser test suites, hand-written cases (every desugaring the parser
//! does: constants, array splats, open intervals, regions, projections,
//! trait bounds, plus the limits that give an empty module), float and
//! string literal values, and a fuzzed corpus of edited repository sources
//! and token soup from a fixed seed.
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use vesper::ast::*;
use vesper::{check, native, packages};
use vesper_syntax::parser;

const PACKAGE: &str = "packages/vesper-ast";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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

fn rust_dump(source: &str) -> String {
    dump_module(&parser::parse(source).module)
}

// ---------------------------------------------------------------- the package

/// The compiled AST package. `WordFunction` holds the JIT module.
struct Ast {
    dump: native::WordFunction,
    count: native::WordFunction,
}

fn load_checked(dir: &Path) -> check::Checked {
    let assembled = packages::load(dir).unwrap();
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    let checked = check::check(&assembled.module);
    let shown: Vec<_> = checked
        .diagnostics
        .iter()
        .map(|d| assembled.sources.localize(d))
        .collect();
    assert!(checked.diagnostics.is_empty(), "{shown:?}");
    checked
}

fn ast() -> &'static Ast {
    static AST: OnceLock<Ast> = OnceLock::new();
    AST.get_or_init(|| {
        let checked = load_checked(&root().join(PACKAGE));
        let compile = |entry| {
            native::WordFunction::compile(&checked.world, entry, true)
                .unwrap_or_else(|e| panic!("{entry}: {e}"))
        };
        Ast {
            dump: compile("ast_dump"),
            count: compile("ast_count"),
        }
    })
}

/// The `Str` value `{len, 0, data}` of `source`, which an `in Str`
/// parameter takes by address.
fn str_words(source: &str) -> [u64; 3] {
    [source.len() as u64, 0, source.as_ptr() as u64]
}

fn call(f: &native::WordFunction, source: &str) -> u64 {
    let words = str_words(source);
    // SAFETY: the entry takes one `in Str`, passed as the address of its
    // three words, which live across the call; `source` outlives it.
    unsafe { f.call(&[words.as_ptr() as u64]) }.unwrap()
}

/// Reads and frees a returned `ChunkSeq[u64]` (header `{len, cap, data}`).
fn take_words(handle: u64) -> Vec<u64> {
    // SAFETY: `handle` is a live sequence header the entry returned, whose
    // `data` holds `len` packed u64 elements.
    let words = unsafe {
        let header = handle as *const u64;
        let len = *header as usize;
        let data = *header.add(2) as *const u64;
        if len == 0 {
            vec![]
        } else {
            std::slice::from_raw_parts(data, len).to_vec()
        }
    };
    vesper::seq::native_runtime::vesper_seq_free(handle);
    words
}

/// Unpacks `ast_dump`'s result: the dump and the lowering's desync count.
fn unpack(words: &[u64]) -> (String, u64) {
    let len = words[0] as usize;
    let mut bytes = Vec::with_capacity(len);
    for w in &words[2..] {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes.truncate(len);
    (String::from_utf8_lossy(&bytes).into_owned(), words[1])
}

fn vesper_dump(source: &str) -> (String, u64) {
    unpack(&take_words(call(&ast().dump, source)))
}

/// The first differing line of two dumps, with context.
fn first_difference(rust: &str, vesper: &str) -> String {
    let r: Vec<&str> = rust.lines().collect();
    let v: Vec<&str> = vesper.lines().collect();
    for i in 0..r.len().max(v.len()) {
        if r.get(i) != v.get(i) {
            let from = i.saturating_sub(3);
            let show =
                |lines: &[&str]| lines[from.min(lines.len())..(i + 2).min(lines.len())].join("\n");
            return format!(
                "line {i}:\n--- rust\n{}\n--- vesper\n{}",
                show(&r),
                show(&v)
            );
        }
    }
    "dumps differ only in trailing bytes".into()
}

/// Compares one source; returns the difference, if any.
fn compare(source: &str) -> Option<String> {
    let rust = rust_dump(source);
    let (vesper, desync) = vesper_dump(source);
    if rust != vesper {
        return Some(first_difference(&rust, &vesper));
    }
    (desync != 0).then(|| format!("the lowering left the CST's token order {desync} times"))
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

fn under_bazel() -> bool {
    std::env::var_os("TEST_SRCDIR").is_some()
}

/// Every repository `.ves` file gives the same AST.
#[test]
fn repository_sources_match_the_rust_ast() {
    let files = repository_sources();
    // Bazel sees the declared `.ves` files of the root package only.
    let floor = if under_bazel() { 300 } else { 800 };
    assert!(files.len() >= floor, "{} sources", files.len());
    let mut failures = vec![];
    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        if let Some(d) = compare(&source) {
            failures.push(format!(
                "{}: {d}",
                path.strip_prefix(root()).unwrap_or(path).display()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} sources differ:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}

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

/// The sources of the parser test suites give the same AST.
#[test]
fn parser_test_corpus_matches_the_rust_ast() {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root().join("crates/vesper-syntax/tests"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.push(root().join("tests/self_host_parse.rs"));
    files.push(root().join("tests/self_host_front_end.rs"));
    files.push(root().join("tests/self_host_front_v2.rs"));
    files.sort();
    let mut cases = 0;
    let mut failures = vec![];
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, source) in rust_string_literals(&text).into_iter().enumerate() {
            cases += 1;
            if let Some(d) = compare(&source) {
                failures.push(format!("{} literal {i} {source:?}: {d}", path.display()));
            }
        }
    }
    if !under_bazel() {
        assert!(cases >= 200, "{cases} corpus sources");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Hand-written cases for every AST the parser builds by desugaring, and
/// the limits that empty the module.
#[test]
fn desugarings_and_limits_match_the_rust_ast() {
    let mut cases: Vec<String> = [
        "",
        "fn",
        // Constants: every use becomes a call of `const$NAME`.
        "const A: u8 = 1; const B: [u8; 2] = [A; 2]; fn f() -> u8 { A + B.0 + g(A) } fn g(x: u8) -> u8 { let A = 1; x }",
        "pub const N: u64 = 0x10; impl S { fn f() -> u64 { N } } trait T { fn g(); } model M: T for S { fn g() { N; } }",
        "pub enum C: u8 { A, B = 4, D, E = C::D | 1, F = C::E << 2, G = C::F - 100, H = C::G + 1, } enum N: i32 { M = -2, O, P, Q = N::O + 1 } enum Z: u8 {} enum B: bool { T = true }",
        "enum E: u64 { A = 18446744073709551615, B, C = 1 << 64, D = 2 / 0, E = (1), F = { 3 } } fn f(x: u8) -> u8 { match (x, 1) { (E::A, C::B) | (LIMIT, _) => 1, (C::B(y), N::M) => y, _ => 2 } } const LIMIT: u8 = 3;",
        "enum H: _ { A = 6, B, C = H::B << 3 } fn f(x: i32) -> u64 { match H::A { H::B => 1, _ => if x == H::C { H::A } else { 0 } } }",
        "enum C: u8 { A = 1 } fn f(x: u8) { let C::A = x; for C::A in 0..2 {} handle E state C::A = 0 in { 0 } with { return C::A => 1, op(C::A) => 2 } }",
        // Array types and literals, with splats of pure and impure operands.
        "fn f(a: [[u8; 3]; 2], b: [(u8, u16); 2], c: [u8; 0], d: [u8; 70000], e: [[u8; 300]; 300]) {}",
        "fn f() { [1; 3]; [-x; 2]; [a + b * c; 2]; [f(); 3]; [{ 1 }; 2]; [(1, 2); 2]; [x.y; 2]; [[0; 2]; 2]; [f(); 0]; [1; 70000] }",
        // Open intervals copy their base with synthetic spans.
        "fn f(s: S) { s[..]; s[a..]; s[..b]; s.a.b[..]; s.a.b[c..]; (s)[1..]; f()[..]; s[..][..]; s[1 + 2..]; s[a..b] }",
        // Regions are handlers of `Region`.
        "fn f() { region r { 1 } region r(16) { x } region r(0) { } region r(1073741825) {} region r(x) {} region r() {} }",
        // Projections, dyn and shapes.
        "fn f(x: <T as Tr[u8]>::A, y: T::B, z: dyn Show[u8], w: dyn FnMut(in u8) -> u8 eff [e], v: [M, N * K * 2, 128], u: A[u8, 16]) {}",
        "fn f(a: fn(in i64, inout u8, sink S) -> i64 eff [Io], b: fn @copy () -> (), c: fn @ () -> u8, d: fn @linear (u8)) {}",
        // Trait bounds and their owners.
        "fn f[T: Ord + Hash, U: mod copy + Show, N: dim, e: eff](x: T) where T: Eq, (U, T): Show[u8] {}",
        "struct S[T: Ord] { x: T } enum E[T: Hash, U: Eq] { A(T), B { u: U } } effect F[T: Ord] { fn op(x: T); } trait Tr[A: Show] {}",
        "impl[K: Ord, V: Show] Map[K, V] { fn get[Q: Eq](in self) where Q: Hash {} fn put() {} } impl[T: Ord] (A, B) { fn f() {} }",
        "impl[T: Ord] Show for Vec[T] where T: Show { type A = T; fn show(in self) {} } impl @a/b::Tr[u8] for X {} impl fn() for X {}",
        "model M[T: Ord]: Show[T] for Vec[T] where T: Hash { fn show[U: Eq](in self) {} } pub model N: T for X {}",
        "trait Show[A: Ord, e: eff] { type Out: mod copy; type In; fn show[B: Hash](in self) -> Self::Out; huh }",
        // Items, fields and variants.
        "pub fn f() {} pub @fip fn g() {} @fip pub fn h() {} @fip fn i() {} pub copy struct T { pub x: u8, hot y: u8 @@ copy, cold z: u8 @@ } linear enum E {} affine effect F {}",
        "tiled(16) struct A { x: u32 } tiled(8_) struct B {} tiled(4294967296) struct C {} wire struct D { a: u8 } tiled(8) enum E {}",
        "enum E { A, B(i64, (u8, u8)), C { x: i64, y: [u8; 2] }, D(), } struct S { } struct T { x: u8, }",
        "use @a/b-c::m::{x, Y as Z}; use self::m; use super::super::m::{a, b,}; pub use self::m::f; use model M; use x::y;",
        "fn f(in self, inout a: u8, sink b: S, c, d.e.{f, g.h}: T, i.{j.{k, l}, m}) {}",
        // Expressions.
        "fn f() { -1; - -1; -x; !x; ~x; -1.5; - 0; a = b = c; a += 1; a.b.c?.d(); S { x, y: 1 }; if S {} {}; (1,); (); (1, 2); ((1)) }",
        "fn f() { 1 + 2 * 3 - 4 / 5 % 6 << 7 >> 8 & 9 ^ 10 | 11 == 12 != 13 < 14 <= 15 > 16 >= 17 && 18 || 19 }",
        "fn f() { f[u8](1); g[T, U](in x, inout y, sink z); Box::new[dyn Show](x); h[e: E[u8] | r, x, T | e](y); TiledSeq::col_get[c](t) }",
        "fn f[e: eff](xs: (i64, i64), e: i64) -> i64 { xs[e] } fn g[e: eff](xs: (i64, i64)) -> i64 { xs[e] } fn h[r: eff]() { x[r](1) }",
        "fn f() { |x: i64, y| x + y; || 1; |x| @copy -> i64 eff [] x; |a| @ a; |in a, inout b| eff [e] a }",
        "fn f() { handle E state s = 0 in { op() } with { return x => x, op(a, k) => k(a), } handle l: E[i64] in {} with { return _ => 0 } }",
        "fn f() { handle self::m::E in { 0 } with { return (a, B { c }) => c op(x) => x } handle E in {} with }",
        "fn f() { for x in xs {} for (a, b) in 0..=n {} for i in 0..n { continue; } loop { break 1; } break; return; return 1 }",
        "fn break() {} fn f() { break; continue; for(1); loop {} } fn continue() {} fn g(for: i64) { for } fn h(break: u8) { break }",
        "fn f() { match x { -1 => 0, -1.5 => 1, \"s\" => 2, true => 3, S { a, b: _ } => 4, (c, D(e)) => 5, E::F::G => 6, _ => { 7 } 8 => 9 } }",
        "fn f() { let mut x: i64 = 1; let mut: i64 = 2; let (a, B { c, d: _ }, C(e)) = y; let x = { use model N; 1 }; }",
        "fn f() { if a {} b; loop {} c; for x in 0..n {} d; match x {} e; (if a {} else {}) f; if a { 1 } else if b { 2 } else { 3 } }",
        "fn f() -> self::m::T[u8] eff [self::E, a: super::F, @a/b::G[u8] | r] { g[self::T](x); self::m::f(); super::P { a: 1 }; @a/b::E::V }",
        // Literal values.
        "fn f() { 0x1f; 1_000; 18446744073709551615; 18446744073709551616; 0x_; \"a\\n\\t\\\"\\\\\\/\\b\\f\\r\"; \"\\u00e9\\uD834\\uDD1E\"; \"\\uD800\"; \"\\q\"; \"é\" }",
        "fn f() { 1.5; 0.1; 1e10; 1E-5; 2.5e+3; 1e400; 1e-400; 4.9e-324; 2.4e-324; 2.5e-324; 1.7976931348623157e308; 1.7976931348623159e308; 0.000; 00.5e-3 }",
        // Or-patterns (#1163): one arm per alternative sharing the body.
        "fn f(x: E) -> u8 { match x { E::A | E::B => 1, E::C(n) | E::D(n) => n, _ => { let y = 2; y } } }",
        "fn f(x: u8) -> u8 { match x { 1 | 2 | 3 => match x { 4 | 5 => 0, _ => 1 }, _ => 2 } }",
        // Test blocks and attributes (#1171) outside test mode.
        "test { fn helper() {} @test fn t() {} } fn break() {} test { fn loop() {} } fn g() { loop }",
        "@test fn a() {} @test(should_fail) pub fn b() {} pub @test_case(1, (2, 3)) fn c(x: u8) {} @fip @test pub fn d() {}",
        "@test struct S {} @test_case fn e() {} test fn h() {}",
        // A tile width equal to u32::MAX is still present.
        "tiled(4294967295) struct S {} tiled(4294967294) struct T {} tiled(4294967296) struct U {} tiled() struct V {}",
        // Recovery.
        "} ) ] fn f() { ) } let x = 1; 42",
        "fn f() { x[ } fn g() { f( } fn h() { S { a: } }",
        "fn f() { let x = ; let y = ; } fn g() { let c = ; }",
        "fn f(x i64) { let y = ; } fn good() {}",
        "fn f(let: u8, if: u8) { x.match; s.impl } fn (+) {} fn f[]() {} struct {} enum { A } effect {}",
        // Contextual `effect`, `handle`, `sink` and `inout` (#1244).
        "fn effect() {} fn handle(x: u64) -> u64 { x } struct S { sink: u64, inout: u8, effect: u8, handle: u8 }",
        "fn f(sink: u64, inout: u64, sink s: S, inout inout: u64, sink sink: S) { g(sink, inout); g(sink x, inout y); g(sink.a, sink(1), sink[0], sink - 1); }",
        "fn f() { let effect = 1; let handle = effect; handle = 2; let s = S { sink, handle: 1 }; s.handle(); for sink in 0..2 {} match x { effect => effect } }",
        "fn f() { handle E in { 0 } with { return x => x } handle(1); let h = handle; }",
        "fn f() { g(sink S { a }, inout self.x, sink 1, sink if a { b } else { c }, sink { 1 }) }",
        // Literals past u64::MAX (#1244).
        "fn f() { 18446744073709551616; 340282366920938463463374607431768211455; 0xffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff; 340282366920938463463374607431768211456 }",
        "fn f() { -170141183460469231731687303715884105728; - -170141183460469231731687303715884105728; -340282366920938463463374607431768211455; -18446744073709551616 }",
        "fn f(x: i128) { match x { -170141183460469231731687303715884105728 => 1, -170141183460469231731687303715884105729 => 2, 340282366920938463463374607431768211455 => 3, -18446744073709551617 => 4, _ => 0 } }",
        "const A: i128 = -170141183460469231731687303715884105728; const B: u128 = 340282366920938463463374607431768211455; const C: i128 = 18446744073709551616; const D: i128 = -C; fn f() -> i128 { match A { A => 1, C => 2, _ => 3 } }",
        "enum W: i128 { A = 18446744073709551616, B, C = -18446744073709551616, D, E = -1, F, G = 170141183460469231731687303715884105727 } enum U: u128 { A = 340282366920938463463374607431768211455 }",
        "fn f() { region r(18446744073709551616) { 1 } }",
        // SH5 lane 4 (#1327): `if`/`match` operands, `handle` statements,
        // `copy`/`affine` names, reserved keywords.
        "fn f(a: bool, b: bool) -> bool { a && if b { true } else { false } } fn g(a: bool) -> bool { a || match x { 0 => true, _ => false } }",
        "fn f() { 1 + if a { 2 } else { 3 } * 4; x == match y { _ => 1 }.len(); if a && if b { c } else { d } { 1 } else { 2 }; !if a { b } else { c }; if a { } -1 }",
        "fn f() { handle E in { 0 } with { return x => x } let y = 1; region r { 1 } y }",
        "fn copy(copy: u64) -> u64 { copy } fn affine() {} struct S { copy: u64, affine: u8 } fn f() { let copy = copy(1); let s = S { copy, affine: 2 }; s.copy(); copy[0]; match copy { copy => copy } g(sink copy, inout affine, sink with); for copy in 0..2 {} }",
        "copy struct P { x: u64 } pub affine enum E { A } struct H { p: P @@ copy } fn id[T: mod copy + Ord](x: fn @copy (u64) -> u64) {} fn c() { |x| @affine x; }",
        "fn f() { let in = 1; let fn = 2; let (a, mod) = (1, 2); } fn g(match: u64, struct: u8) {} fn h() { let x = s.impl; }",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Depth and height limits empty the module.
    for n in [60, 64, 126, 127, 128, 129, 200] {
        cases.push(format!("fn f() {{ {}1{} }}", "(".repeat(n), ")".repeat(n)));
        cases.push(format!("fn f() {}{}", "{".repeat(n), "}".repeat(n)));
        cases.push(format!("fn f() {{ x{} }}", " + y".repeat(n)));
        cases.push(format!("fn f() {{ {}1 }}", "-".repeat(n)));
    }
    // The or-pattern expansion budget: a body copied too often keeps one
    // alternative.
    for alts in [2, 300, 500] {
        let pats = vec!["1"; alts].join(" | ");
        let body = format!("{{ {} 0 }}", "x; ".repeat(300));
        cases.push(format!(
            "fn f(x: u8) -> u8 {{ match x {{ {pats} => {body}, _ => 1 }} }}"
        ));
    }
    // Diagnostics past the 128 limit, and the token limit.
    for n in [127, 128, 129] {
        cases.push(format!("fn f() {{ {} }}", "let = ;".repeat(n)));
    }
    for n in [32767, 32768, 32769] {
        cases.push(format!("fn f() {{ {} }}", "x;".repeat(n)));
    }
    let mib = 1 << 20;
    cases.push(format!("fn f() {{}} {}", " ".repeat(mib)));
    let mut failures = vec![];
    for (i, source) in cases.iter().enumerate() {
        if let Some(d) = compare(source) {
            let shown: String = source.chars().take(160).collect();
            failures.push(format!("case {i} {shown:?}: {d}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A deterministic generator (SplitMix64).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Float literals: their f64 values (`str::parse`) match bit for bit,
/// including subnormals, halfway cases, overflow and long mantissas.
#[test]
fn float_literals_match_rust_parse() {
    let mut rng = Rng(0x666c_6f61_7473_0001);
    let mut literals: Vec<String> = [
        "0.0",
        "1.0",
        "0.1",
        "0.2",
        "0.3",
        "1e308",
        "1e309",
        "2e308",
        "1.7976931348623157e308",
        "1.7976931348623158e308",
        "1.7976931348623159e308",
        "4.9406564584124654e-324",
        "2.4703282292062327e-324",
        "2.4703282292062328e-324",
        "2.2250738585072014e-308",
        "2.2250738585072011e-308",
        "9007199254740993.0",
        "9007199254740992.0",
        "9007199254740995.0",
        "123456789012345678901234567890.0",
        "0.000000000000000000000000000001e30",
        "1e-400",
        "1e99999999999999999999",
        "1e-99999999999999999999",
        "3.14159265358979323846264338327950288419716939937510",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // A halfway case with a long tail: 2^53 + 1 written with 900 digits.
    literals.push(format!("9007199254740993.{}1", "0".repeat(900)));
    literals.push(format!("9007199254740993.{}", "0".repeat(900)));
    for _ in 0..2000 {
        let digits = 1 + rng.below(25);
        let mut m = String::new();
        for i in 0..digits {
            let d = rng.below(10);
            m.push(char::from(
                b'0' + if i == 0 { d.max(1) as u8 } else { d as u8 },
            ));
        }
        let point = rng.below(digits);
        let mant = format!("{}.{}0", &m[..point.max(1)], &m[point.max(1)..]);
        let exp = rng.below(700) as i64 - 350;
        literals.push(match rng.below(3) {
            0 => mant,
            1 => format!("{mant}e{exp}"),
            _ => format!("{m}e{exp}"),
        });
    }
    let mut failures = vec![];
    for chunk in literals.chunks(200) {
        let source = format!("fn f() {{ {} }}", chunk.join("; "));
        if let Some(d) = compare(&source) {
            failures.push(d);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Fragments the fuzzer inserts (those of tests/self_host_front_v2.rs plus
/// the forms the AST desugars).
const FRAGMENTS: &[&str] = &[
    "fn",
    "let",
    "struct",
    "enum",
    "effect",
    "impl",
    "copy",
    "affine",
    "linear",
    "mod",
    "eff",
    "in",
    "inout",
    "sink",
    "self",
    "handle",
    "state",
    "with",
    "return",
    "match",
    "if",
    "else",
    "true",
    "false",
    "use",
    "model",
    "pub",
    "@fip",
    "trait",
    "tiled(8)",
    "wire",
    "for",
    "where",
    "as",
    "type",
    "dim",
    "dyn",
    "hot",
    "cold",
    "mut",
    "region",
    "loop",
    "break",
    "continue",
    "_",
    "TiledSeq",
    "load_tile_vec",
    "u8",
    "i64",
    "bool",
    "Unit",
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    ",",
    ";",
    ":",
    "::",
    ".",
    "..",
    "..=",
    "?",
    "@",
    "@@",
    "->",
    "=>",
    "=",
    "+=",
    "-=",
    "*=",
    "/=",
    "%=",
    "|=",
    "&=",
    "+",
    "-",
    "*",
    "/",
    "%",
    "==",
    "!=",
    "<",
    "<=",
    ">",
    ">=",
    "<<",
    ">>",
    "&&",
    "||",
    "!",
    "|",
    "&",
    "^",
    "~",
    "$",
    "x",
    "Y",
    "0",
    "1.5",
    "2e3",
    "0x1f",
    "0x_",
    "18446744073709551616",
    "\"s\"",
    "\"\\q\"",
    "\"\\uD800\"",
    "\"\\u00e9\"",
    "\"",
    "\\",
    "/*",
    "*/",
    "//",
    "é",
    "`",
    "#",
    "'",
    "\n",
    " ",
    "\t",
    "const A: u8 = 1;",
    "A",
    "[x; 2]",
    "[f(); 2]",
    "[u8; 4]",
    "s[..]",
    "s[a..]",
    "region r ",
    "<T as Tr>::A",
    "T::A",
    "super::",
    "self::",
    "@a/b::",
    "[T: Ord]",
    "where T: Ord",
];

fn mutate(rng: &mut Rng, source: &str) -> String {
    let mut s: Vec<char> = source.chars().collect();
    let edits = 1 + rng.below(6);
    for _ in 0..edits {
        let at = rng.below(s.len() + 1);
        match rng.below(5) {
            0 if !s.is_empty() => {
                let len = 1 + rng.below(12);
                let end = (at + len).min(s.len());
                s.drain(at.min(s.len())..end);
            }
            1 if !s.is_empty() => {
                let from = rng.below(s.len());
                let len = (1 + rng.below(40)).min(s.len() - from);
                let piece: Vec<char> = s[from..from + len].to_vec();
                s.splice(at..at, piece);
            }
            2 => s.truncate(at),
            _ => {
                let f = FRAGMENTS[rng.below(FRAGMENTS.len())];
                s.splice(at..at, f.chars());
            }
        }
    }
    s.into_iter().collect()
}

fn soup(rng: &mut Rng) -> String {
    let n = 1 + rng.below(80);
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(FRAGMENTS[rng.below(FRAGMENTS.len())]);
        if rng.below(3) != 0 {
            s.push(' ');
        }
    }
    s
}

/// Edited repository sources and token soup from a fixed seed (the
/// front end v2 fuzzer's generator; `VESPER_AST_FUZZ` sets the count).
#[test]
fn fuzzed_corpus_matches_the_rust_ast() {
    let mut files = repository_sources();
    files.retain(|p| std::fs::metadata(p).is_ok_and(|m| m.len() < 64 * 1024));
    let sources: Vec<String> = files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    assert!(!sources.is_empty());
    let rounds: usize = std::env::var("VESPER_AST_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    let mut rng = Rng(0x5348_3037_6672_6f6e);
    let mut failures = vec![];
    for round in 0..rounds {
        let source = if round % 4 == 3 {
            soup(&mut rng)
        } else {
            let base = &sources[rng.below(sources.len())];
            mutate(&mut rng, base)
        };
        if let Some(d) = compare(&source) {
            let shown: String = source.chars().take(300).collect();
            failures.push(format!("round {round} {shown:?}: {d}"));
            if failures.len() >= 10 {
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Building an AST frees every sequence it allocates.
#[test]
fn lowering_frees_its_arenas() {
    let a = ast();
    let source = std::fs::read_to_string(root().join(PACKAGE).join("src/lower.ves")).unwrap();
    let _ = call(&a.count, &source);
    let before = vesper::seq::native_runtime::live_allocations();
    let nodes = call(&a.count, &source);
    let after = vesper::seq::native_runtime::live_allocations();
    assert!(nodes > 10_000, "{nodes} nodes");
    assert_eq!(
        before, after,
        "live sequences before and after building an AST"
    );
}

// ---------------------------------------------------------------- backends

fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1099511628211).wrapping_add(v)
}

/// `ast_dump`'s words for the Rust dump of `source`.
fn dump_words(source: &str) -> Vec<u64> {
    let dump = rust_dump(source);
    let bytes = dump.as_bytes();
    let mut words = vec![bytes.len() as u64, 0];
    for chunk in bytes.chunks(8) {
        let mut w = [0u8; 8];
        w[..chunk.len()].copy_from_slice(chunk);
        words.push(u64::from_le_bytes(w));
    }
    words
}

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-ast-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A driver package that folds the words of `ast_dump` over the sources
/// into one digest (`main`), depending on the AST package by path.
fn digest_driver(sources: &[&str]) -> (Temp, u64) {
    let temp = Temp::new();
    let ast = std::fs::canonicalize(root().join(PACKAGE)).unwrap();
    let manifest = format!(
        "[package]\nname = \"@local/ast-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Fail\", \"Alloc\"]\n\n[dependencies]\n\"@vesper/ast\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
        ast.display().to_string().replace('\\', "/")
    );
    let mut main = String::from(
        "use @vesper/ast::{ast_dump};\n\nfn mix(acc: u64, v: u64) -> u64 {\n    wrapping_add(wrapping_mul(acc, 1099511628211), v)\n}\n\nfn digest(in s: Str) -> u64 {\n    let rows = ast_dump(in s);\n    let mut acc: u64 = 7;\n    for i in 0..rows.len() {\n        acc = match rows.try_get(i) {\n            Option::Some(v) => mix(acc, v),\n            Option::None => acc,\n        };\n    }\n    acc\n}\n\nfn main() -> u64 {\n    let mut acc: u64 = 1;\n",
    );
    let mut expected = 1u64;
    for s in sources {
        main.push_str(&format!(
            "    acc = mix(acc, digest(in {}));\n",
            serde_json::to_string(s).unwrap()
        ));
        expected = mix(expected, dump_words(s).into_iter().fold(7, mix));
    }
    main.push_str("    acc\n}\n");
    std::fs::write(temp.0.join("Loom.toml"), manifest).unwrap();
    std::fs::create_dir_all(temp.0.join("src")).unwrap();
    std::fs::write(temp.0.join("src/main.ves"), main).unwrap();
    (temp, expected)
}

/// Small sources that reach every lowering path the evaluator can afford.
const SMALL: &[&str] = &[
    "",
    "const A: u8 = 1; fn f() -> u8 { A + [A; 2].0 }",
    "use @a/b::{c as d}; pub fn f[T: Ord, e: eff](in x: T) -> T eff [e] where T: Ord { let y = x; y }",
    "struct S { pub x: i64, hot y: u8 } enum E { A, B(i64), C { x: u8 } } impl S { fn f(in self) {} }",
    "trait T[A: Tr] { type X: mod copy; fn f(in self); } model M: T for X { type X = i64; fn f(in self) {} }",
    "fn f() { match x { -1 => 0, S { a, b: _ } => 1, (c, D(e)) => 2 } handle E state s = 0 in { op() } with { return v => v, op(k) => k(1) } }",
    "fn f() { for i in 0..n { if a { continue; } else { break; } } loop {} region r(16) { x[a..b]; s[..]; f[u8](1); |y| y + 1 } }",
    "fn f() { 1.5; 2e-3; \"a\\u00e9\\n\"; -1; [f(); 2] }",
];

/// The JIT without optimization, the evaluator and an executable agree
/// with the Rust AST (the optimized JIT is the rest of this file).
#[test]
fn backends_agree_on_small_sources() {
    let (dir, expected) = digest_driver(SMALL);
    let c = load_checked(&dir.0);
    for optimize in [false, true] {
        let got = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}): {e}"))
            .to_string();
        assert_eq!(got, expected.to_string(), "JIT (optimize {optimize})");
    }
    let mut runtime = vesper::runtime::Runtime::new(&c.world);
    runtime.optimize = false;
    runtime.fuel = 4_000_000_000;
    let got = runtime.run("main", vec![]).unwrap().to_string();
    assert_eq!(got, expected.to_string(), "evaluator");
    let links = cfg!(unix)
        && std::process::Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
    if links {
        let exe = dir.0.join("driver");
        let built = std::process::Command::new(env!("CARGO_BIN_EXE_vesper"))
            .arg("build")
            .arg(&dir.0)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "vesper build failed\n{}{}",
            String::from_utf8_lossy(&built.stdout),
            String::from_utf8_lossy(&built.stderr)
        );
        let status = std::process::Command::new(&exe).output().unwrap().status;
        assert_eq!(status.code(), Some((expected & 0xff) as i32), "executable");
    }
}
