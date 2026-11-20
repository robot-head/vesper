//! Self-hosting SH1 lane 12 (#1155): the parse swap. `parse_words` of the
//! Vesper module (modules/parse, linked by build.rs) read back by
//! `parse_words::decode` must equal `vesper_syntax::parser::rust_parse`
//! exactly: the rowan CST, every diagnostic and the whole
//! `vesper_core::ast::Module` (`Parse: PartialEq`).
//!
//! The corpus is the one of tests/self_host_ast.rs: every `.ves` file in the
//! repository, the source strings of the parser test suites, hand-written
//! cases for every desugaring and limit, and a fuzzed corpus from a fixed
//! seed.
#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/parse.rs"]
mod binding;

#[allow(dead_code)]
#[path = "../src/parse_words.rs"]
mod parse_words;

use std::path::{Path, PathBuf};
use vesper_syntax::parser::{self, Parse};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn under_bazel() -> bool {
    std::env::var_os("TEST_SRCDIR").is_some()
}

fn vesper_parse(source: &str) -> Parse {
    parse_words::decode(source, &binding::parse_words(source))
}

/// The first difference between the two parses, if any.
fn compare(source: &str) -> Option<String> {
    let rust = parser::rust_parse(source);
    let vesper = vesper_parse(source);
    if rust == vesper {
        return None;
    }
    if rust.green != vesper.green {
        let events = |p: &Parse| -> Vec<String> {
            p.syntax()
                .preorder_with_tokens()
                .map(|e| format!("{e:?}"))
                .collect()
        };
        let (r, v) = (events(&rust), events(&vesper));
        let at = r.iter().zip(&v).position(|(a, b)| a != b);
        return Some(match at {
            Some(i) => format!("CST differs at event {i}: rust {}, vesper {}", r[i], v[i]),
            None => format!("CST lengths differ: rust {}, vesper {}", r.len(), v.len()),
        });
    }
    if rust.diagnostics != vesper.diagnostics {
        return Some(format!(
            "diagnostics: rust {:?}, vesper {:?}",
            rust.diagnostics, vesper.diagnostics
        ));
    }
    let r = format!("{:#?}", rust.module);
    let v = format!("{:#?}", vesper.module);
    let line = r
        .lines()
        .zip(v.lines())
        .enumerate()
        .find(|(_, (a, b))| a != b)
        .map_or_else(
            || "lengths differ".to_owned(),
            |(i, (a, b))| format!("line {i}: rust {a:?}, vesper {b:?}"),
        );
    Some(format!("module differs at {line}"))
}

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

/// Every repository `.ves` file parses the same.
#[test]
fn repository_sources_parse_the_same() {
    let files = repository_sources();
    // Bazel sees the declared `.ves` files only.
    let floor = if under_bazel() { 300 } else { 800 };
    assert!(files.len() >= floor, "{} sources", files.len());
    let mut failures = vec![];
    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        if let Some(d) = compare(&source) {
            failures.push(format!("{}: {d}", path.display()));
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

/// The sources of the parser test suites parse the same.
#[test]
fn parser_test_corpus_parses_the_same() {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root().join("crates/vesper-syntax/tests"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    for f in [
        "tests/self_host_parse.rs",
        "tests/self_host_front_end.rs",
        "tests/self_host_front_v2.rs",
        "tests/self_host_ast.rs",
    ] {
        files.push(root().join(f));
    }
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

/// Hand-written cases for every AST the parser builds by desugaring, and
/// the limits that empty the module.
#[test]
fn desugarings_and_limits_parse_the_same() {
    let mut cases: Vec<String> = [
        "",
        "fn",
        // Constants: every use becomes a call of `const$NAME`.
        "const A: u8 = 1; const B: [u8; 2] = [A; 2]; fn f() -> u8 { A + B.0 + g(A) } fn g(x: u8) -> u8 { let A = 1; x }",
        "pub const N: u64 = 0x10; impl S { fn f() -> u64 { N } } trait T { fn g(); } model M: T for S { fn g() { N; } }",
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
fn fuzzed_corpus_parses_the_same() {
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

/// The reader and the module agree on the layout version.
#[test]
fn formats_agree() {
    assert_eq!(binding::format(), parse_words::FORMAT);
}
