//! Self-hosting SH2 lane 14 (#1186): `@vesper/check-tensor-types`, the
//! checker half of `src/tensor_types` in Vesper, against the Rust module.
//!
//! Both sides write one report per source: `inactive`, or `active`, every
//! tensor diagnostic (code, span, message, help and labels), the kinds of
//! every ADT's generics and every checked signature with its inferred
//! dimension preconditions. The Vesper side parses the source itself with
//! `@vesper/ast`; the Bridge wire layout checker stays in Rust and its
//! results are part of the input. The sources are every `.ves` file of the
//! corpus directories that parses, the tensor fixtures and checker test
//! sources, and mutants of the tensor fixtures that make each diagnostic
//! of the module fire.
#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use vesper::ast::Module;
use vesper::tensor_types;

/// The Rust report of one module.
fn rust_report(module: &Module) -> String {
    if !tensor_types::active(module) {
        return "inactive\n".into();
    }
    let a = tensor_types::analyze(module);
    let mut o = String::from("active\n");
    for d in &a.diagnostics {
        writeln!(
            o,
            "D {} @{}..{} {}",
            d.code, d.span.start, d.span.end, d.message
        )
        .unwrap();
        if let Some(h) = &d.help {
            writeln!(o, " H {h}").unwrap();
        }
        for r in &d.related {
            writeln!(o, " L @{}..{} {}", r.span.start, r.span.end, r.message).unwrap();
        }
    }
    for (n, ks) in &a.adt_kinds {
        write!(o, "K {n}").unwrap();
        for k in ks {
            write!(o, " {k:?}").unwrap();
        }
        o.push('\n');
    }
    for (n, s) in &a.signatures {
        writeln!(o, "S {n}").unwrap();
        for (g, k) in &s.generics {
            writeln!(o, " G {g} {k:?}").unwrap();
        }
        for (p, t) in &s.params {
            writeln!(o, " P {p:?} {t}").unwrap();
        }
        writeln!(o, " R {}", s.result).unwrap();
        if let Some((kv, hd)) = &s.pool {
            writeln!(o, " L {kv}, {hd}").unwrap();
        }
        for ob in &s.obligations {
            writeln!(o, " O {ob}").unwrap();
        }
    }
    o
}

fn put(out: &mut Vec<u8>, x: u64) {
    out.extend(x.to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put(out, b.len() as u64);
    out.extend(b);
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
}

/// The Vesper input: the source, then the wire checker's diagnostics and
/// wires (only read when the module is active).
fn input(source: &str, module: &Module) -> Vec<u8> {
    let mut out = vec![];
    put_bytes(&mut out, source.as_bytes());
    let (wires, diags) = if tensor_types::active(module) {
        tensor_types::wire::check(module)
    } else {
        Default::default()
    };
    put(&mut out, diags.len() as u64);
    for d in &diags {
        put(&mut out, d.code[1..].parse::<u64>().unwrap());
        put(&mut out, d.span.start as u64);
        put(&mut out, d.span.end as u64);
        put_bytes(&mut out, d.message.as_bytes());
    }
    put(&mut out, wires.len() as u64);
    for (name, w) in &wires {
        put_bytes(&mut out, name.as_bytes());
        put(&mut out, w.format.is_some() as u64);
        put_bytes(
            &mut out,
            tensor_types::wire::format_mismatch(name, w).as_bytes(),
        );
    }
    out
}

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-check-tensor-types"))
}

fn entry(optimize: bool) -> &'static Entry {
    static ON: OnceLock<Entry> = OnceLock::new();
    static OFF: OnceLock<Entry> = OnceLock::new();
    let cell = if optimize { &ON } else { &OFF };
    cell.get_or_init(|| package().entry("check_tensor_types", optimize))
}

fn elab_entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| package().entry("check_tensor_types_elaborate", true))
}

/// A parsed source.
struct Source {
    name: String,
    text: String,
    module: Module,
}

fn parse(name: String, text: String) -> Option<Source> {
    let parsed = vesper::parser::parse(&text);
    parsed.diagnostics.is_empty().then_some(Source {
        name,
        text,
        module: parsed.module,
    })
}

/// Every `.ves` file under the corpus directories that parses.
fn corpus_files() -> Vec<Source> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.path());
        for e in entries {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if !matches!(&*name, "target" | ".git" | "node_modules")
                    && !name.starts_with("bazel-")
                {
                    walk(&p, out);
                }
            } else if p.extension().is_some_and(|e| e == "ves") {
                out.push(p);
            }
        }
    }
    let mut paths = vec![];
    for d in ["tests/fixtures", "examples", "packages", "crates"] {
        walk(&repo().join(d), &mut paths);
    }
    paths
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            if text.len() > 1 << 20 {
                return None;
            }
            let name = p
                .strip_prefix(repo())
                .unwrap_or(&p)
                .display()
                .to_string()
                .replace('\\', "/");
            parse(name, text)
        })
        .collect()
}

/// Sources written for the checker tests (tests/tensor_types*.rs).
const CHECKER_SOURCES: &[&str] = &[
    "fn main() -> u64 { 1 }",
    "copy struct Tensor { n: u64 } fn main() -> u64 { let t = Tensor { n: 1 }; t.n }",
    "fn f(x: Tensor[F32, [2, 3]]) -> Tensor[F32, [3, 2]] eff [Tensor] { Tensor::transpose(x) }",
    "fn f[N: dim](x: Tensor[F32, [N]]) -> u64 eff [Tensor] { Tensor::argmax(x) }",
    "copy struct Pair[N: dim] { a: Tensor[F32, [N]], b: Tensor[F32, [N]] }
     fn sum[N: dim](p: Pair[N]) -> Tensor[F32, [N]] eff [Tensor] { Tensor::add(p.a, p.b) }
     fn main(x: Tensor[F32, [4]]) -> Tensor[F32, [4]] eff [Tensor] { sum(Pair { a: x, b: x }) }",
    "fn heads[H: dim, D: dim](q: Tensor[F32, [H, D * 2]]) -> Tensor[F32, [H, D * 2]] eff [Tensor] { Tensor::rope(q, 1) }
     fn main(q: Tensor[F32, [4, 6]]) -> Tensor[F32, [4, 6]] eff [Tensor] { heads(q) }",
    "enum Shape[N: dim] { One(Tensor[F32, [N]]), Two(Tensor[F32, [N]], Tensor[F32, [N]]) }
     fn f(x: Tensor[F32, [3]]) -> Shape[3] { Shape::Two(x, x) }",
    "fn f(x: (Tensor[F32, [2]], u64)) -> Tensor[F32, [2]] { x.0 }",
    "fn f(g: fn(Tensor[F32, [2]]) -> Tensor[F32, [2]], x: Tensor[F32, [2]]) -> Tensor[F32, [2]] { g(x) }",
    "fn f(x: Tensor[F32, [2]]) -> Tensor[F32, [2]] { let c = |y: Tensor[F32, [2]]| -> Tensor[F32, [2]] { y }; c(x) }",
    "fn f(x: Tensor[F32, [2]]) -> u64 { for i in 0..3 { let y = x; } match x { t => 1 } }",
];

/// The tensor fixtures (accepted and rejected).
fn tensor_fixtures() -> Vec<Source> {
    let mut out = vec![];
    for dir in [
        "tests/fixtures/tensor_types",
        "tests/fixtures/tensor_types/reject",
    ] {
        let mut paths: Vec<_> = std::fs::read_dir(repo().join(dir))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "ves"))
            .collect();
        paths.sort();
        for p in paths {
            let text = std::fs::read_to_string(&p).unwrap();
            let name = format!("{dir}/{}", p.file_name().unwrap().to_string_lossy());
            if let Some(s) = parse(name, text) {
                out.push(s);
            }
        }
    }
    out
}

/// A small deterministic generator (xorshift).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() % xs.len() as u64) as usize]
    }
}

/// Words of a source with their byte offsets.
fn words(src: &str) -> Vec<(usize, &str)> {
    let mut out = vec![];
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_alphanumeric() || b[i] == b'_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push((s, &src[s..i]));
        } else {
            i += 1;
        }
    }
    out
}

const REPLACEMENTS: &[&str] = &[
    "0",
    "1",
    "2",
    "3",
    "7",
    "16",
    "32",
    "64",
    "1099511627777",
    "99999999999999999999",
    "F32",
    "Bf16",
    "Tensor",
    "KvPageLease",
    "ActiveKvSeq",
    "Dims",
    "KvCachePool",
    "u16le",
    "ByteArray",
    "matmul",
    "matvec",
    "add",
    "mul",
    "swiglu",
    "reshape",
    "transpose",
    "softmax",
    "silu",
    "to_bf16",
    "to_f32",
    "argmax",
    "fill",
    "rms_norm",
    "rope",
    "qmatvec",
    "block_scale",
    "frobnicate",
    "begin_sequence",
    "lease_page",
    "append_kv_step",
    "write_page",
    "attend",
    "tokens",
    "page_tokens",
    "release_sequence",
    "release_page",
    "Option",
    "Result",
    "u64",
    "dim",
    "N",
    "M",
    "K",
    "linear",
    "copy",
];

/// One mutant of `src`: one to three word replacements, deletions of a
/// bracketed argument, or duplications.
fn mutate(rng: &mut Rng, src: &str) -> String {
    let mut s = src.to_string();
    let edits = 1 + rng.next() % 3;
    for _ in 0..edits {
        let ws = words(&s);
        if ws.is_empty() {
            break;
        }
        let (at, w) = *rng.pick(&ws);
        let end = at + w.len();
        match rng.next() % 6 {
            0..=2 => {
                let r = rng.pick(REPLACEMENTS);
                s.replace_range(at..end, r);
            }
            3 => {
                let other = rng.pick(&ws).1.to_string();
                s.replace_range(at..end, &other);
            }
            4 => {
                // Drop the word and a following comma.
                let mut e = end;
                if s[e..].starts_with(", ") {
                    e += 2;
                }
                s.replace_range(at..e, "");
            }
            _ => {
                let dup = format!("{w}, {w}");
                s.replace_range(at..end, &dup);
            }
        }
    }
    s
}

/// Mutants of the tensor fixtures that parse, deterministic.
fn mutants(per_fixture: usize) -> Vec<Source> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out = vec![];
    for f in tensor_fixtures() {
        let mut made = 0;
        let mut tries = 0;
        while made < per_fixture && tries < per_fixture * 8 {
            tries += 1;
            let text = mutate(&mut rng, &f.text);
            if let Some(s) = parse(format!("{}#{tries}", f.name), text) {
                out.push(s);
                made += 1;
            }
        }
    }
    out
}

fn vesper_report(source: &Source, optimize: bool) -> String {
    entry(optimize).text(&input(&source.text, &source.module))
}

/// Runs both sides over `sources`; returns the differing sources and the
/// diagnostic codes seen.
fn compare(label: &str, sources: &[Source], optimize: bool) -> (usize, BTreeMap<String, usize>) {
    let mut bad = 0;
    let mut codes = BTreeMap::new();
    let mut panics = 0;
    for s in sources {
        let module = s.module.clone();
        let Ok(rust) = std::panic::catch_unwind(move || rust_report(&module)) else {
            panics += 1;
            continue;
        };
        let vesper = vesper_report(s, optimize);
        for l in rust.lines() {
            if let Some(rest) = l.strip_prefix("D ") {
                *codes.entry(rest[..5].to_string()).or_default() += 1;
            }
        }
        if rust != vesper {
            bad += 1;
            if bad <= 5 {
                eprintln!(
                    "{label}: {} differs: {}",
                    s.name,
                    first_difference(&rust, &vesper).unwrap_or_default()
                );
            }
        }
    }
    let active = sources
        .iter()
        .filter(|s| tensor_types::active(&s.module))
        .count();
    eprintln!(
        "{label}: {} sources ({active} active), {bad} differ, {panics} Rust panics skipped; codes {codes:?}",
        sources.len()
    );
    (bad, codes)
}

#[test]
fn corpus_matches_the_rust_tensor_checker() {
    let files = corpus_files();
    assert!(!files.is_empty());
    let (bad, _) = compare("corpus", &files, true);
    assert_eq!(bad, 0);
}

#[test]
fn fixtures_and_checker_sources_match() {
    let mut sources = tensor_fixtures();
    for (i, s) in CHECKER_SOURCES.iter().enumerate() {
        sources.extend(parse(format!("checker source {i}"), s.to_string()));
    }
    let (bad, codes) = compare("fixtures", &sources, true);
    assert_eq!(bad, 0);
    assert!(codes.len() >= 10, "{codes:?}");
    // The optimizer does not change the answers.
    let (off, _) = compare("fixtures (no opt)", &sources, false);
    assert_eq!(off, 0);
}

#[test]
fn mutants_fire_every_diagnostic_and_match() {
    let per = std::env::var("TENSOR_MUTANTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let sources = mutants(per);
    let (bad, codes) = compare("mutants", &sources, true);
    assert_eq!(bad, 0);
    for c in 761..=771 {
        let code = format!("E0{c}");
        assert!(codes.contains_key(&code), "{code} never fired: {codes:?}");
    }
}

/// ABBA timing against Rust (`CHECK_HARNESS_SPEED=1`, release). Both sides
/// parse the source and run the tensor checker. The rejected fixtures stop
/// at the tensor diagnostics, so Rust's `analyze` skips the core checker it
/// otherwise runs on the elaborated module; the corpus timing is parse plus
/// `active` on every file.
#[test]
#[ignore]
fn abba_against_rust() {
    if !speed() {
        return;
    }
    let f = entry(true);
    let rejects: Vec<Source> = tensor_fixtures()
        .into_iter()
        .filter(|s| s.name.contains("/reject/"))
        .collect();
    let inputs: Vec<Vec<u8>> = rejects.iter().map(|s| input(&s.text, &s.module)).collect();
    abba(
        "rejected fixtures (parse + check)",
        &mut || {
            time(|| {
                for _ in 0..20 {
                    for s in &rejects {
                        let m = vesper::parser::parse(&s.text).module;
                        std::hint::black_box(rust_report(&m));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for _ in 0..20 {
                    for i in &inputs {
                        std::hint::black_box(f.text(i));
                    }
                }
            })
        },
    );
    let files = corpus_files();
    let inputs: Vec<Vec<u8>> = files.iter().map(|s| input(&s.text, &s.module)).collect();
    abba(
        "corpus (parse + active)",
        &mut || {
            time(|| {
                for s in &files {
                    let m = vesper::parser::parse(&s.text).module;
                    std::hint::black_box(tensor_types::active(&m));
                }
            })
        },
        &mut || {
            time(|| {
                for i in &inputs {
                    std::hint::black_box(f.text(i));
                }
            })
        },
    );
}

fn fmt_ast_ty(t: &vesper::ast::Type, o: &mut String) {
    use vesper::ast::Type;
    match t {
        Type::Named(n, args) => {
            o.push_str(n);
            if !args.is_empty() {
                o.push('[');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        o.push_str(", ");
                    }
                    fmt_ast_ty(a, o);
                }
                o.push(']');
            }
        }
        Type::Tuple(ts) => {
            o.push('(');
            for (i, x) in ts.iter().enumerate() {
                if i > 0 {
                    o.push_str(", ");
                }
                fmt_ast_ty(x, o);
            }
            o.push(')');
        }
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            o.push_str("fn(");
            for (i, (_, pt)) in params.iter().enumerate() {
                if i > 0 {
                    o.push_str(", ");
                }
                fmt_ast_ty(pt, o);
            }
            o.push_str(") -> ");
            fmt_ast_ty(result, o);
            if !effects.instances.is_empty() || effects.tail.is_some() {
                o.push_str(" with ");
                fmt_ast_row(effects, o);
            }
        }
        Type::Row(r) => fmt_ast_row(r, o),
        _ => o.push('_'),
    }
}

fn fmt_ast_eff(ef: &vesper::ast::Effect, o: &mut String) {
    if let Some(l) = &ef.label {
        o.push_str(l);
        o.push_str(": ");
    }
    o.push_str(&ef.name);
    if !ef.args.is_empty() {
        o.push('[');
        for (i, a) in ef.args.iter().enumerate() {
            if i > 0 {
                o.push_str(", ");
            }
            fmt_ast_ty(a, o);
        }
        o.push(']');
    }
}

fn fmt_ast_row(r: &vesper::ast::Effects, o: &mut String) {
    let mut first = true;
    for ef in &r.instances {
        if !first {
            o.push_str(", ");
        }
        first = false;
        fmt_ast_eff(ef, o);
    }
    if let Some(tail) = &r.tail {
        if !first {
            o.push_str(", ");
        }
        o.push_str("..");
        o.push_str(tail);
    }
}

fn fmt_ast_expr(e: &vesper::ast::Expr, o: &mut String) {
    use vesper::ast::{ExprKind, Literal, Statement};
    let s = e.span.start;
    let end = e.span.end;
    match &e.kind {
        ExprKind::Literal(Literal::Int(v)) if s >= 1048576 => {
            write!(o, "lit({v})@{s}..{end}").unwrap();
        }
        ExprKind::Name(n) if s >= 1048576 && n.starts_with("$dim_") => {
            write!(o, "name({n})@{s}..{end}").unwrap();
        }
        ExprKind::Name(n) if s >= 1048576 && n == "wrapping_mul" => {
            write!(o, "wrapping_mul@{s}..{end}").unwrap();
        }
        ExprKind::Literal(_) => write!(o, "E1@{s}..{end}").unwrap(),
        ExprKind::Name(_) => write!(o, "E2@{s}..{end}").unwrap(),
        ExprKind::Tuple(es) => {
            o.push('(');
            for (i, x) in es.iter().enumerate() {
                if i > 0 {
                    o.push_str(", ");
                }
                fmt_ast_expr(x, o);
            }
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Struct { name, fields } => {
            if s >= 1048576 && name == "Dims" {
                o.push_str("struct(Dims");
                for (fnm, fv) in fields {
                    write!(o, ", {fnm}: ").unwrap();
                    fmt_ast_expr(fv, o);
                }
                write!(o, ")@{s}..{end}").unwrap();
            } else {
                write!(o, "{name}{{").unwrap();
                for (i, (fnm, fv)) in fields.iter().enumerate() {
                    if i > 0 {
                        o.push_str(", ");
                    }
                    write!(o, "{fnm}: ").unwrap();
                    fmt_ast_expr(fv, o);
                }
                write!(o, "}}@{s}..{end}").unwrap();
            }
        }
        ExprKind::Block { statements, tail } => {
            o.push('{');
            for (i, st) in statements.iter().enumerate() {
                if i > 0 {
                    o.push_str("; ");
                }
                match st {
                    Statement::Let { ty, value, .. } => {
                        o.push_str("let:");
                        fmt_ast_ty(ty, o);
                        o.push('=');
                        fmt_ast_expr(value, o);
                    }
                    Statement::Expr(x) => fmt_ast_expr(x, o),
                }
            }
            if let Some(t) = tail {
                o.push_str("; ");
                fmt_ast_expr(t, o);
            }
            write!(o, "}}@{s}..{end}").unwrap();
        }
        ExprKind::Call {
            callee,
            types,
            args,
        } => {
            o.push_str("call(");
            fmt_ast_expr(callee, o);
            if !types.is_empty() {
                o.push('[');
                for (i, ty) in types.iter().enumerate() {
                    if i > 0 {
                        o.push_str(", ");
                    }
                    fmt_ast_ty(ty, o);
                }
                o.push(']');
            }
            for a in args {
                o.push_str(", ");
                fmt_ast_expr(&a.expr, o);
            }
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Field(x, _) => {
            o.push_str("U7(");
            fmt_ast_expr(x, o);
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Try(x) => {
            o.push_str("U9(");
            fmt_ast_expr(x, o);
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Loop(x) => {
            o.push_str("U17(");
            fmt_ast_expr(x, o);
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Unary(_, x) => {
            o.push_str("Un(");
            fmt_ast_expr(x, o);
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Index(a, b) => {
            o.push_str("B8(");
            fmt_ast_expr(a, o);
            o.push_str(", ");
            fmt_ast_expr(b, o);
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Range { start, end: re, .. } => {
            o.push_str("B19(");
            fmt_ast_expr(start, o);
            o.push_str(", ");
            fmt_ast_expr(re, o);
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Binary(_, a, b) => {
            o.push_str("Op11(");
            fmt_ast_expr(a, o);
            o.push_str(", ");
            fmt_ast_expr(b, o);
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Assign(_, a, b) => {
            o.push_str("Op12(");
            fmt_ast_expr(a, o);
            o.push_str(", ");
            fmt_ast_expr(b, o);
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            o.push_str("if(");
            fmt_ast_expr(condition, o);
            o.push_str(", ");
            fmt_ast_expr(then, o);
            if let Some(ow) = otherwise {
                o.push_str(", ");
                fmt_ast_expr(ow, o);
            }
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Match { value, arms } => {
            o.push_str("match(");
            fmt_ast_expr(value, o);
            for (_, body) in arms {
                o.push_str(", ");
                fmt_ast_expr(body, o);
            }
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::For { iter, body, .. } => {
            o.push_str("for(");
            fmt_ast_expr(iter, o);
            o.push_str(", ");
            fmt_ast_expr(body, o);
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Break(v) => {
            o.push_str("J20(");
            if let Some(x) = v {
                fmt_ast_expr(x, o);
            }
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Return(v) => {
            o.push_str("J21(");
            if let Some(x) = v {
                fmt_ast_expr(x, o);
            }
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Closure {
            params,
            result,
            effects,
            body,
            ..
        } => {
            o.push_str("fn(");
            for (i, p) in params.iter().enumerate() {
                if i > 0 {
                    o.push_str(", ");
                }
                fmt_ast_ty(&p.ty, o);
            }
            o.push_str(")->");
            fmt_ast_ty(result, o);
            if !effects.instances.is_empty() || effects.tail.is_some() {
                o.push_str(" with ");
                fmt_ast_row(effects, o);
            }
            o.push('{');
            fmt_ast_expr(body, o);
            write!(o, "}}@{s}..{end}").unwrap();
        }
        ExprKind::Handle {
            effect,
            state,
            body,
            ret,
            ops,
        } => {
            o.push_str("handle(");
            fmt_ast_eff(effect, o);
            if let Some((_, st)) = state {
                o.push_str(", ");
                fmt_ast_expr(st, o);
            }
            o.push_str(", ");
            fmt_ast_expr(body, o);
            o.push_str(", ");
            fmt_ast_expr(&ret.body, o);
            for op in ops {
                o.push_str(", ");
                fmt_ast_expr(&op.body, o);
            }
            write!(o, ")@{s}..{end}").unwrap();
        }
        ExprKind::Continue => write!(o, "C@{s}..{end}").unwrap(),
    }
}

fn rust_elaborate_report(module: &Module) -> String {
    let Some((elab_mod, base, syn)) = tensor_types::probe_elaborate(module) else {
        return "inactive\n".into();
    };
    let mut o = format!("base {base}\n");
    for item in elab_mod.items.iter().skip(7) {
        match item {
            vesper::ast::Item::Adt(a) => {
                write!(o, "ADT {}", a.name).unwrap();
                for g in &a.generics {
                    write!(o, " {}", g.name).unwrap();
                }
                o.push('\n');
                for v in &a.variants {
                    write!(o, " V {}", v.name).unwrap();
                    for f in &v.fields {
                        o.push(' ');
                        fmt_ast_ty(&f.ty, &mut o);
                    }
                    o.push('\n');
                }
            }
            vesper::ast::Item::Function(f) => {
                writeln!(o, "FN {}", f.name).unwrap();
                for g in &f.generics {
                    writeln!(o, " G {}", g.name).unwrap();
                }
                for p in &f.params {
                    write!(o, " P {}:", p.name).unwrap();
                    fmt_ast_ty(&p.ty, &mut o);
                    writeln!(o, "@{}..{}", p.span.start, p.span.end).unwrap();
                }
                o.push_str(" R ");
                fmt_ast_ty(&f.result, &mut o);
                o.push('\n');
                if !f.effects.instances.is_empty() || f.effects.tail.is_some() {
                    o.push_str(" E ");
                    fmt_ast_row(&f.effects, &mut o);
                    o.push('\n');
                }
                if let Some(b) = &f.body {
                    o.push_str(" B ");
                    fmt_ast_expr(b, &mut o);
                    o.push('\n');
                }
            }
            _ => {}
        }
    }
    for (s, orig) in &syn {
        writeln!(o, "SYN {s} -> @{}..{}", orig.start, orig.end).unwrap();
    }
    o
}

#[test]
fn elaborate_agrees_with_rust() {
    let f = elab_entry();
    let mut sources = tensor_fixtures();
    sources.extend(
        corpus_files()
            .into_iter()
            .filter(|s| tensor_types::active(&s.module)),
    );
    let mut tested = 0usize;
    let mut diffs = vec![];
    for s in &sources {
        let r = rust_elaborate_report(&s.module);
        let v = f.text(&input(&s.text, &s.module));
        tested += 1;
        if r != v {
            diffs.push((s.name.clone(), r, v));
        }
    }
    assert!(tested > 0, "no active tensor sources tested");
    if !diffs.is_empty() {
        let mut msg = format!(
            "{} of {tested} elaborate reports differ (first 3):\n",
            diffs.len()
        );
        for (name, r, v) in diffs.iter().take(3) {
            msg.push_str(&format!("--- {name} ---\nRUST:\n{r}\nVESPER:\n{v}\n"));
        }
        panic!("{msg}");
    }
}
