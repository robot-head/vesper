//! Self-hosting SH0 lane 7 (#1129): the front end v2 in
//! `packages/vesper-front` against the Rust parser (`vesper_syntax::parser::
//! parse`). The Vesper package is compiled once, as optimized native code,
//! and its `parse_tables` entry runs on each source. Its result rows (see
//! `packages/vesper-front/src/lib.ves`) must equal the same rows read off
//! the rowan tree and diagnostics of the Rust parser: every CST token's
//! kind, range and parent node, every node's kind, parent and range, and
//! every diagnostic's code, message and span.
//!
//! The corpus is every `.ves` file in the repository, the source strings of
//! the parser test suites (`crates/vesper-syntax/tests`,
//! `tests/self_host_parse.rs`), hand-written limit cases (depth, diagnostic,
//! token and source-size limits) and a fuzzed corpus of edited repository
//! sources and token soup from a fixed seed.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use vesper::{check, native, packages};
use vesper_syntax::parser;

const PACKAGE: &str = "packages/vesper-front";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// One parse, as rows. Tokens: kind, start, end, parent. Nodes: kind,
/// parent (the node count for the root), start, end. Diagnostics: code
/// number, start, end, message.
#[derive(Debug, PartialEq, Eq, Default)]
struct Tables {
    tokens: Vec<[u64; 4]>,
    nodes: Vec<[u64; 4]>,
    diagnostics: Vec<(u64, u64, u64, String)>,
}

/// The Rust parser's CST and diagnostics as rows.
fn rust_tables(source: &str) -> Tables {
    let parse = parser::parse(source);
    let mut t = Tables::default();
    fn walk(node: &parser::SyntaxNode, parent: u64, t: &mut Tables) {
        let id = t.nodes.len() as u64;
        let range = node.text_range();
        t.nodes.push([
            u64::from(node.kind().0),
            parent,
            u64::from(u32::from(range.start())),
            u64::from(u32::from(range.end())),
        ]);
        for child in node.children_with_tokens() {
            if let Some(n) = child.as_node() {
                walk(n, id, t);
            } else if let Some(tok) = child.as_token() {
                let r = tok.text_range();
                t.tokens.push([
                    u64::from(tok.kind().0),
                    u64::from(u32::from(r.start())),
                    u64::from(u32::from(r.end())),
                    id,
                ]);
            }
        }
    }
    walk(&parse.syntax(), u64::MAX, &mut t);
    let count = t.nodes.len() as u64;
    t.nodes[0][1] = count;
    for d in &parse.diagnostics {
        let code = match d.code.as_str() {
            "E0001" => 1,
            "E0002" => 2,
            "E0003" => 3,
            other => panic!("unexpected parser code {other}"),
        };
        t.diagnostics.push((
            code,
            d.span.start as u64,
            d.span.end as u64,
            d.message.clone(),
        ));
    }
    t
}

/// The compiled front end. `WordFunction` holds the JIT module.
struct Front {
    tables: native::WordFunction,
    counts: native::WordFunction,
    lex: native::WordFunction,
}

fn front() -> &'static Front {
    static FRONT: OnceLock<Front> = OnceLock::new();
    FRONT.get_or_init(|| {
        let assembled = packages::load(&root().join(PACKAGE)).unwrap();
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
        let compile = |entry| {
            native::WordFunction::compile(&checked.world, entry, true)
                .unwrap_or_else(|e| panic!("{entry}: {e}"))
        };
        Front {
            tables: compile("parse_tables"),
            counts: compile("parse_counts"),
            lex: compile("lex_count"),
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

fn vesper_tables(source: &str) -> Tables {
    let words = take_words(call(&front().tables, source));
    let (leaves, nodes, diags) = (words[0] as usize, words[1] as usize, words[2] as usize);
    let mut at = 3;
    let mut row = || {
        let r = [words[at], words[at + 1], words[at + 2], words[at + 3]];
        at += 4;
        r
    };
    let mut t = Tables::default();
    for _ in 0..leaves {
        t.tokens.push(row());
    }
    for _ in 0..nodes {
        t.nodes.push(row());
    }
    for _ in 0..diags {
        let (code, start, end, len) = (
            words[at],
            words[at + 1],
            words[at + 2],
            words[at + 3] as usize,
        );
        at += 4;
        let bytes: Vec<u8> = words[at..at + len].iter().map(|&b| b as u8).collect();
        at += len;
        t.diagnostics.push((
            code,
            start,
            end,
            String::from_utf8(bytes).expect("UTF-8 message"),
        ));
    }
    assert_eq!(at, words.len(), "trailing result words");
    t
}

/// The first difference between two parses, for the failure message.
fn first_difference(source: &str, rust: &Tables, vesper: &Tables) -> String {
    let around = |at: u64| {
        let at = (at as usize).min(source.len());
        let mut lo = at.saturating_sub(40);
        while !source.is_char_boundary(lo) {
            lo -= 1;
        }
        let mut hi = (at + 40).min(source.len());
        while !source.is_char_boundary(hi) {
            hi += 1;
        }
        format!("{:?}", &source[lo..hi])
    };
    for (i, (r, v)) in rust.tokens.iter().zip(&vesper.tokens).enumerate() {
        if r != v {
            return format!("token {i}: rust {r:?}, vesper {v:?} near {}", around(r[1]));
        }
    }
    if rust.tokens.len() != vesper.tokens.len() {
        return format!(
            "token count: rust {}, vesper {}",
            rust.tokens.len(),
            vesper.tokens.len()
        );
    }
    for (i, (r, v)) in rust.nodes.iter().zip(&vesper.nodes).enumerate() {
        if r != v {
            return format!("node {i}: rust {r:?}, vesper {v:?} near {}", around(r[2]));
        }
    }
    if rust.nodes.len() != vesper.nodes.len() {
        return format!(
            "node count: rust {}, vesper {}",
            rust.nodes.len(),
            vesper.nodes.len()
        );
    }
    for (i, (r, v)) in rust.diagnostics.iter().zip(&vesper.diagnostics).enumerate() {
        if r != v {
            return format!(
                "diagnostic {i}: rust {r:?}, vesper {v:?} near {}",
                around(r.1)
            );
        }
    }
    format!(
        "diagnostic count: rust {:?}, vesper {:?}",
        rust.diagnostics, vesper.diagnostics
    )
}

/// Compares one source; returns the difference, if any.
fn compare(source: &str) -> Option<String> {
    let rust = rust_tables(source);
    let vesper = vesper_tables(source);
    (rust != vesper).then(|| first_difference(source, &rust, &vesper))
}

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

/// Every repository `.ves` file gives the same CST and diagnostics.
#[test]
fn repository_sources_match_the_rust_parser() {
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

/// The string literals of a Rust test file that look like Vesper sources.
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
            // A char literal or a lifetime: skip `'x'` and `'\x'` forms.
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
                "fn ", "struct ", "enum ", "trait ", "use ", "impl ", "effect ", "model ",
            ]
            .iter()
            .any(|k| s.contains(k))
    });
    out
}

/// The sources of the parser test suites give the same CST and diagnostics.
#[test]
fn parser_test_corpus_matches_the_rust_parser() {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root().join("crates/vesper-syntax/tests"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.push(root().join("tests/self_host_parse.rs"));
    files.push(root().join("tests/self_host_front_end.rs"));
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

/// Hand-written edge cases: every lexer rule's corner, recovery paths and
/// every limit (depth, diagnostics, tokens and source bytes).
#[test]
fn edge_cases_and_limits_match_the_rust_parser() {
    let mut cases: Vec<String> = [
        // Qualified paths, `pub use`, `super` and open intervals (#1106).
        "use super::super::m::{a, B as C, state as linear};",
        "pub use self::m::f; pub use @a/b-c::m::T; pub use super::x; pub use x::y; pub use;",
        "use x::y; use super; use super::; use self::with::linear::f;",
        "fn f(s: S) { s[..]; s[a..]; s[..b]; s.a.b[..]; (s)[1..]; f()[..]; s[..][..]; s[1 + 2..] }",
        "fn f() { s[..; s[a.. }",
        "fn f(state: u8, with: u8) -> u8 { let linear = state + with; linear.with.state }",
        "fn f() -> self::m::T[u8] eff [self::E, a: super::F, @a/b::G[u8] | r] { g[self::T](x); g[super::T](x); g[@a/b::T](x) }",
        "fn f() { let x = super::m::P { a: 1 }; match x { self::E::A(y) => y, @a/b::E::B { q } => q, super::C => 0 } }",
        "fn f() { handle self::m::E in { 0 } with { return x => x } }",
        "fn f() { @a /b::c; @a/b; self::; super::1 }",
        "fn f(let: u8, if: u8) { x.match; s.impl }",
        "fn f[r](x: T[self::m::U, r]) {}",
        // Fixed-size arrays and module constants (#1115).
        "const A: u8 = 1; pub const B: [u8; 2] = [1; 2]; const c: u8 = 1; pub const; const A u8 = ;",
        "fn f(a: [[u8; 300]; 300], b: [(u8, u8); 40000], c: [T; ], d: [u8; x], e: [u8; 0x10], g: [u8; 1_0], h: [u8; 99999999999999999999]) {}",
        "fn f(a: [u8, (x; 1)], b: [[u8; 2]], c: [(u8; 2)], d: [u8; 2) {}",
        "fn f() { [0; 70000]; [[1, 2]; 40000]; [f(); 0]; [(1, 2); 3]; [x; y]; [1; 0x10]; [-x; 2]; [a + b * c; 2]; [x = 1; 2] }",
        "fn f() { [{ (1, 2) }; 40000]; [{ 1; }; 70000]; [(1, (2, 3)); 30000]; []; [1, 2, 3,]; [1; 2, 3]; [1, 2; 3]; [; 2] }",
        "fn f() { [[[1; 300]; 300]; 0]; [(); 70000]; [f(); 70000]; [f(); 0]; ((1, 2), [3; 4]) }",
        "",
        " ",
        "fn",
        "\"abc",
        "é#`x",
        "1.e5 1.5e+ 0x 0x_ 1_000.5 1e5 12ee 0b1 1. 00.5e-3x 1E-x 0X1F 9_ _9 1.2e3.4",
        "\"a\\\nb\"",
        "/* /* */",
        "/**/ /*/ */",
        "\"\\é\" \"é",
        "..= .. ... ..=. @@@ ::: -> => == != <= >= << >> && || |= &= += -= *= /= %=",
        "a'b \u{1} \u{b} \\",
        "fn f() { \"\\uD800\" \"\\uDC00\" \"\\uD834\\uDD1E\" \"\\uD834\\u0041\" \"\\/\" \"\\U0041\" \"\\uD834\" }",
        "fn f() { \"a\u{1}\" \"a\u{7f}\tb\" \"\\u00E9\\u00e9\" \"\\u12\" }",
        "fn f() -> u64 { 18446744073709551615 + 18446744073709551616 + 0xffffffffffffffff + 0x10000000000000000 + 0x0000000000000000000000000001 + 0x_ }",
        "tiled(8) struct A { x: u32 } tiled(4294967296) struct B {} tiled(0x8) struct C {} tiled(8_) struct D {} tiled(x) struct E {} tiled(8) fn f() {}",
        "fn f() { region r(0) { 1 } region r(1073741824) { 1 } region r(1073741825) { 1 } region r(x) {} region r { } }",
        "fn f() { match 1 { -true => 0, -thing => 1, -1 => 2, -1.5 => 3, -\"s\" => 4, } }",
        "fn f() { let x = 1 x }",
        "fn f() { match 1 { 1 => 2 3 => 4 } }",
        "fn f() { a::b @@ @ -> => == $ }",
        "fn f() { if a {} b; loop {} c; for x in 0..n {} d; match x {} e; (if a {} else {}) f }",
        "fn break() {} fn f() { break; continue; for(1); loop {} } fn continue() {} fn g(for: i64) { for }",
        "fn f[e: eff](xs: (i64, i64), e: i64) -> i64 { xs[e] } fn g[e: eff](xs: (i64, i64)) -> i64 { xs[e] }",
        "fn f() { TiledSeq::load_tile_vec[c](x); t.col_get[c](x); t.col_set[c](); t.other[c](x) }",
        "fn f() { Box::new[dyn Show](x); f[fn() -> i64](g); f[u8](1); f[Unit](1); x[y]; x[a..b]; x[A](); x[[M]](y) }",
        "trait T[A: Tr, B: Other] {} effect E[T: Ord] {} trait U {}",
        "effect E[T: Ord] { fn op(); } trait U {}",
        "use @scope/name::{a, b as c}; use self::x::y; use self::{}; use a::b; pub use @x/y::z; use @a/b-c::d::{E as F, g}; use model M;",
        "pub fn f() {} pub @fip fn g() {} @fip pub fn h() {} @fip struct S {} pub copy struct T {} pub trait X {} pub model M: T for X {}",
        "impl[T: Ord] Vec[T] { fn f(in self) {} pub fn g() {} @fip fn h() {} } impl Show for X { type A = B; fn f() {} } impl @a/b::C for D {} impl fn() for X {} impl (A, B) for X {}",
        "model M[e: eff]: T[e] for X where X: Y, Z: W { type A = i64; fn f(in self) -> <T as Tr>::A {} }",
        "struct S { pub x: i64, pub: i64, hot y: u8, cold z: u8, pub hot w: u8, v: u8 @@ copy, u: u8 @@ }",
        "enum E { A, B(i64, (u8, u8)), C { x: i64 }, D() } wire struct W { a: u32 } copy enum F {}",
        "fn f(in x: T, inout y: U, sink z: V, w.a.{b, c.d}: W, q) -> fn @copy (in i64) -> i64 eff [e] eff [a: E[x] | r, F, | t] where T: Ord {}",
        "fn f() -> A[e: E[u8] | r, x, u8, 16, dyn T[x], fn() | e, (A, B) | r, [M, N * 4, 128]] {}",
        "fn f() { let mut x: i64 = 1; let mut: i64 = 2; let (a, B { c, d: _ }, C(e)) = y; |x: i64, y| x + y; || 1; |x| @copy -> i64 eff [] x; }",
        "fn f() { handle E state s = 0 in { op() } with { return x => x, op(a, k) => k(a), } handle l: E[i64] in {} with { return _ => 0 } }",
        "fn f() { -1; - -1; -x; !x; ~x; a = b = c; a += 1; a.b.c?.d(); S { x, y: 1 }; if S {} {} ; (1,); (); (1, 2) }",
        "fn f() { 1 + 2 * 3 - 4 / 5 % 6 << 7 >> 8 & 9 ^ 10 | 11 == 12 != 13 < 14 <= 15 > 16 >= 17 && 18 || 19 }",
        "fn f() { let x = ; let y = ; } fn g() { let c = ; }",
        "fn f(x i64) { let y = ; } fn good() {}",
        "fn f() { ` } fn g() { ` }",
        "fn é() {} fn g() { ` }",
        "fn f() { \"open }",
        "fn f() {} /* open",
        "trait T { huh fn f(in self); type A mod copy; type B: mod; }",
        "fn f[e]() {} fn g() -> A[fn() | e] {} fn h[T: mod copy + Ord + Hash, U: mod, N: dim]() {}",
        "} ) ] fn f() { ) } let x = 1; 42",
        "fn f() { x[ } fn g() { f( } fn h() { S { a: } }",
        "fn f() -> <T as Tr[x]>::A {} fn g() -> T::A {} fn h() -> <T Tr>::A {}",
        "fn f() { for x in xs {} for (a, b) in 0..=n {} for 1 in x {} for S { a } in x {} }",
        "fn f() { use model M; let x = { use model N; 1 }; }",
        "struct",
        "fn f(",
        "fn f() { match",
        "impl",
        "use",
        "use @",
        "/// doc\nfn f() {} // trailing",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Depth: nested parentheses, blocks, types, patterns, unary chains,
    // postfix and binary chains around the 128 limit.
    for n in [60, 63, 64, 65, 126, 127, 128, 129, 130, 200] {
        cases.push(format!("fn f() {{ {}1{} }}", "(".repeat(n), ")".repeat(n)));
        cases.push(format!("fn f() {}{}", "{".repeat(n), "}".repeat(n)));
        cases.push(format!(
            "fn f() -> {}i64{} {{}}",
            "(".repeat(n),
            ")".repeat(n)
        ));
        cases.push(format!(
            "fn f() {{ let {}x{} = 1; }}",
            "(".repeat(n),
            ")".repeat(n)
        ));
        cases.push(format!("fn f() {{ {}x }}", "-".repeat(n)));
        cases.push(format!("fn f() {{ {}1 }}", "-".repeat(n)));
        cases.push(format!("fn f() {{ x{} }}", ".y".repeat(n)));
        cases.push(format!("fn f() {{ x{} }}", "?".repeat(n)));
        cases.push(format!("fn f() {{ x{} }}", " + y".repeat(n)));
        cases.push(format!("fn f() {{ x{} }}", " = y".repeat(n)));
        cases.push(format!("fn f() {{ {}x }}", "|a| ".repeat(n)));
        cases.push(format!(
            "fn f() {{ {}1{} }}",
            "if a { ".repeat(n),
            " }".repeat(n)
        ));
        cases.push(format!("fn f(a.{}b) {{}}", "a.".repeat(n)));
        cases.push(format!(
            "fn f() -> A{}{} {{}}",
            "[A".repeat(n),
            "]".repeat(n)
        ));
    }
    // Heights: a parenthesized chain resets the depth but not the AST
    // height, so these reach the height limit (`record_expr`) first. The
    // atoms cover literal folding (`-1` is one literal, `-x` a unary).
    for n in [60, 62, 63, 64, 65, 66, 70] {
        for atom in [
            "x", "-1", "- -1", "-x", "x.y", "f(x)", "x?", "{ x }", "-1.5",
        ] {
            let inner = vec![atom; n].join(" + ");
            let outer = format!(" + {atom}").repeat(n);
            cases.push(format!("fn f() {{ ({inner}){outer} }}"));
            cases.push(format!("fn f() {{ ({inner}).y{} }}", ".y".repeat(n)));
            cases.push(format!("fn f() {{ (({inner}) + {atom}){outer} }}"));
            cases.push(format!("fn f() {{ {{ ({inner}) }}{outer}; }}"));
            cases.push(format!(
                "fn f() {{ match ({inner}) {{ _ => ({inner}){outer} }} }}"
            ));
        }
    }
    // Diagnostics: past 128 lexical and parse errors.
    for n in [100, 127, 128, 129, 200] {
        cases.push("`".repeat(n));
        cases.push(format!("fn f() {{ {} }}", "let = ;".repeat(n)));
        cases.push(format!("{}fn f() {{}}", "` ".repeat(n)));
        cases.push(format!("{} `", ")".repeat(n)));
    }
    // Tokens: around the 65,536-token limit.
    for n in [32767, 32768, 32769, 40000] {
        cases.push("a ".repeat(n));
        cases.push(format!("fn f() {{ {} }}", "x;".repeat(n)));
    }
    // Source bytes: around the 1 MiB budget, with a split UTF-8 character.
    let mib = 1 << 20;
    cases.push("a".repeat(mib));
    cases.push("a".repeat(mib + 1));
    cases.push(format!("{}é", "a".repeat(mib - 1)));
    cases.push(format!("{}éé", "a".repeat(mib - 2)));
    let mut failures = vec![];
    for (i, source) in cases.iter().enumerate() {
        if let Some(d) = compare(source) {
            let shown: String = source.chars().take(120).collect();
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

/// Fragments the fuzzer inserts: every token kind, every keyword and
/// contextual word, broken literals and comments, and non-ASCII text.
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

/// Edited repository sources and token soup from a fixed seed.
#[test]
fn fuzzed_corpus_matches_the_rust_parser() {
    let mut files = repository_sources();
    files.retain(|p| std::fs::metadata(p).is_ok_and(|m| m.len() < 64 * 1024));
    let sources: Vec<String> = files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    assert!(!sources.is_empty());
    let rounds: usize = std::env::var("VESPER_FRONT_FUZZ")
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
            let shown: String = source.chars().take(200).collect();
            failures.push(format!("round {round} {shown:?}: {d}"));
            if failures.len() >= 10 {
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The front end frees every sequence it allocates.
#[test]
fn parsing_frees_its_tables() {
    let f = front();
    let source = std::fs::read_to_string(root().join(PACKAGE).join("src/parse.ves")).unwrap();
    // Warm up, then compare the live count around a second parse.
    let _ = call(&f.counts, &source);
    let before = vesper::seq::native_runtime::live_allocations();
    let counts = call(&f.counts, &source);
    let after = vesper::seq::native_runtime::live_allocations();
    assert!(counts >> 32 > 1000, "{counts:#x}");
    assert_eq!(before, after, "live sequences before and after a parse");
    let tokens = call(&f.lex, &source);
    assert_eq!(tokens, counts >> 32);
    assert_eq!(vesper::seq::native_runtime::live_allocations(), after);
}

// ---------------------------------------------------------------- backends

fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1099511628211).wrapping_add(v)
}

/// The fold `DIGEST_DRIVER` computes over the result rows of one parse.
fn rows_digest(t: &Tables) -> u64 {
    let mut words = vec![
        t.tokens.len() as u64,
        t.nodes.len() as u64,
        t.diagnostics.len() as u64,
    ];
    for r in t.tokens.iter().chain(&t.nodes) {
        words.extend_from_slice(r);
    }
    for (code, start, end, message) in &t.diagnostics {
        words.extend_from_slice(&[*code, *start, *end, message.len() as u64]);
        words.extend(message.bytes().map(u64::from));
    }
    words.into_iter().fold(7, mix)
}

/// A driver package that folds the result rows of `parse_tables` over the
/// sources into one digest (`main`), depending on the front end by path.
fn digest_driver(sources: &[&str]) -> (Temp, u64) {
    let temp = Temp::new();
    let front = std::fs::canonicalize(root().join(PACKAGE)).unwrap();
    let manifest = format!(
        "[package]\nname = \"@local/front-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Fail\"]\n\n[dependencies]\n\"@vesper/front\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
        front.display().to_string().replace('\\', "/")
    );
    let mut main = String::from(
        "use @vesper/front::{parse_tables};\n\nfn mix(acc: u64, v: u64) -> u64 {\n    wrapping_add(wrapping_mul(acc, 1099511628211), v)\n}\n\nfn digest(in s: Str) -> u64 {\n    let rows = parse_tables(in s);\n    let mut acc: u64 = 7;\n    for i in 0..rows.len() {\n        acc = match rows.try_get(i) {\n            Option::Some(v) => mix(acc, v),\n            Option::None => acc,\n        };\n    }\n    acc\n}\n\nfn main() -> u64 {\n    let mut acc: u64 = 1;\n",
    );
    let mut expected = 1u64;
    for s in sources {
        main.push_str(&format!(
            "    acc = mix(acc, digest(in {}));\n",
            serde_json::to_string(s).unwrap()
        ));
        expected = mix(expected, rows_digest(&rust_tables(s)));
    }
    main.push_str("    acc\n}\n");
    std::fs::write(temp.0.join("Loom.toml"), manifest).unwrap();
    std::fs::create_dir_all(temp.0.join("src")).unwrap();
    std::fs::write(temp.0.join("src/main.ves"), main).unwrap();
    (temp, expected)
}

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-front-v2-{}-{}",
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

fn load_driver(dir: &Path) -> check::Checked {
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

/// Small sources that reach every parser path the evaluator can afford.
const SMALL: &[&str] = &[
    "",
    "fn f() -> u64 { 0x_ + 18446744073709551616 } /* open",
    "use @a/b::{c as d}; pub fn f[T: Ord, e: eff](in x: T) -> T eff [e] where T: Ord { let y = x; y }",
    "struct S { pub x: i64, hot y: u8 } enum E { A, B(i64), C { x: u8 } } impl S { fn f(in self) {} }",
    "trait T[A: Tr] { type X: mod copy; fn f(in self); } model M: T for X { type X = i64; fn f(in self) {} }",
    "fn f() { match x { -1 => 0, S { a, b: _ } => 1, (c, D(e)) => 2 } handle E state s = 0 in { op() } with { return v => v, op(k) => k(1) } }",
    "fn f() { for i in 0..n { if a { continue; } else { break; } } loop {} region r(16) { x[a..b]; f[u8](1); |y| y + 1 } }",
    "fn f() { \"\\uD800\" ` é let = ; }",
];

/// The JIT without optimization, the evaluator and an executable agree
/// with the Rust parser (the optimized JIT is the rest of this file).
#[test]
fn backends_agree_on_small_sources() {
    let (dir, expected) = digest_driver(SMALL);
    let c = load_driver(&dir.0);
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
