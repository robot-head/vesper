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
