//! Self-hosting Stage 1 (design §42.3): the Vesper-source lexer and parser in
//! `packages/vesper-compiler` run on the Stage 0 compiler (the reference
//! evaluator and the Cranelift JIT) over the repository's own `.ves` sources,
//! and their token streams and CST shapes are compared with `vesper-syntax`,
//! the Rust seed.
//!
//! The Vesper program cannot return a buffer, so each phase folds its output
//! into a 63-bit digest that this test recomputes from the seed's tokens and
//! rowan CST: tokens as (kind, start, end), all trivia and the EOF sentinel
//! included; the CST as preorder events (open: kind and start offset; close:
//! end offset). Errors come back as `-1 - (code << 32 | offset)`.
use std::path::{Path, PathBuf};
use vesper::parser::{self as seed, SyntaxNode};
use vesper::{check, lexer, native, parser, runtime::Runtime, runtime::Value};

/// The package sources, in compilation order (see its Loom.toml).
const FRONT_END: [&str; 5] = [
    "packages/vesper-compiler/src/lexer.ves",
    "packages/vesper-compiler/src/cursor.ves",
    "packages/vesper-compiler/src/types_items.ves",
    "packages/vesper-compiler/src/expressions.ves",
    "packages/vesper-compiler/src/driver.ves",
];
const CODE_LEX: i64 = 1;
const CODE_EXPECTED: i64 = 2;
const CODE_OVERFLOW: i64 = 3;
const CODE_INVALID: i64 = 4;
const CODE_LIMIT: i64 = 5;
const CODE_UNSUPPORTED: i64 = 16;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn mix(h: i64, x: i64) -> i64 {
    let m = h.wrapping_add(x).wrapping_mul(0x100000001b3);
    m.wrapping_add(m / 2147483648)
}

fn digest(h: i64) -> i64 {
    if h < 0 { (-1i64).wrapping_sub(h) } else { h }
}

const OFFSET_BASIS: i64 = 0xcbf29ce484222325_u64 as i64;

/// The seed's token-stream digest, or None when the seed reports a lexical
/// diagnostic or limit (the Vesper lexer must then fail).
fn seed_tokens(source: &str) -> Option<i64> {
    let lexed = lexer::lex_with_limits(source, lexer::LexLimits::default()).ok()?;
    if !lexed.diagnostics.is_empty() || lexed.limit.is_some() {
        return None;
    }
    let mut h = OFFSET_BASIS;
    for token in &lexed.tokens {
        h = mix(h, token.kind as u16 as i64);
        h = mix(h, token.span.start as i64);
        h = mix(h, token.span.end as i64);
    }
    Some(digest(mix(h, lexed.tokens.len() as i64)))
}

fn cst_events(node: &SyntaxNode, h: &mut i64) {
    let range = node.text_range();
    *h = mix(*h, node.kind().0 as i64 + 1000);
    *h = mix(*h, u32::from(range.start()) as i64);
    for child in node.children() {
        cst_events(&child, h);
    }
    *h = mix(*h, 3000);
    *h = mix(*h, u32::from(range.end()) as i64);
}

/// The seed's CST digest, or None when the seed reports any diagnostic.
fn seed_cst(source: &str) -> Option<i64> {
    let parsed = seed::parse_with_limits(source, seed::ParseLimits::default()).ok()?;
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let mut h = OFFSET_BASIS;
    cst_events(&parsed.syntax(), &mut h);
    Some(digest(h))
}

/// The source as Vesper code. Leaves hold 64 bytes in a tuple indexed
/// dynamically; internal functions dispatch over at most 8 children. A single
/// comparison tree over every byte would be correct too, but the native
/// backend's CLIF grows quadratically with the nesting of one function's
/// `if` blocks (4.3 million CLIF lines for 593 leaves), so the tree is split
/// into small functions.
fn source_module(bytes: &[u8]) -> String {
    const LEAF: usize = 64;
    const FAN: usize = 8;
    const EFFECTS: &str = "eff [Fail[RuntimeError], Fail[ArithOverflow]]";
    let mut out = String::from("\n");
    let mut nodes: Vec<(String, usize)> = vec![];
    for (k, chunk) in bytes.chunks(LEAF).enumerate() {
        let start = k * LEAF;
        let mut values: Vec<String> = chunk.iter().map(u8::to_string).collect();
        values.resize(LEAF, "0".into());
        let name = format!("source_leaf_{k}");
        out.push_str(&format!(
            "fn {name}(i: i64) -> i64 {EFFECTS} {{ ({})[i - {start}] }}\n",
            values.join(",")
        ));
        nodes.push((name, start));
    }
    fn tree(group: &[(String, usize)], out: &mut String) {
        if group.len() == 1 {
            out.push_str(&format!("{}(i)", group[0].0));
            return;
        }
        let mid = group.len() / 2;
        out.push_str(&format!("if i < {} {{ ", group[mid].1));
        tree(&group[..mid], out);
        out.push_str(" } else { ");
        tree(&group[mid..], out);
        out.push_str(" }");
    }
    let mut level = 0;
    while nodes.len() > 1 {
        level += 1;
        let mut next = vec![];
        for (g, group) in nodes.chunks(FAN).enumerate() {
            let name = format!("source_node_{level}_{g}");
            let mut body = String::new();
            tree(group, &mut body);
            out.push_str(&format!(
                "fn {name}(i: i64) -> i64 {EFFECTS} {{ {body} }}\n"
            ));
            next.push((name, group[0].1));
        }
        nodes = next;
    }
    let n = bytes.len();
    let top = nodes
        .first()
        .map_or_else(|| "-1".to_owned(), |(name, _)| format!("{name}(i)"));
    out.push_str(&format!(
        "fn source_len() -> i64 {{ {n} }}\n\
         fn source_byte(i: i64) -> i64 {{ if i < 0 || i >= {n} {{ -1 }} else {{ \
         handle Fail[ArithOverflow] in {{ handle Fail[RuntimeError] in {{ {top} }} with {{ \
         return v => v, raise(e, r) => -1, }} }} with {{ return v => v, raise(e, r) => -1, }} }} }}\n"
    ));
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Digest(i64),
    Error { code: i64, at: i64 },
}

fn decode(value: &Value) -> Outcome {
    let Value::Int(v) = value else {
        panic!("entry returned {value:?}");
    };
    if *v >= 0 {
        Outcome::Digest(*v)
    } else {
        let e = -1 - *v;
        Outcome::Error {
            code: e >> 32,
            at: e & 0xffff_ffff,
        }
    }
}

struct Program {
    world: check::World,
}

fn program(front_end: &str, bytes: &[u8]) -> Program {
    let text = format!("{front_end}{}", source_module(bytes));
    let limits = parser::ParseLimits {
        max_source_bytes: lexer::HARD_SOURCE_BYTES,
        max_tokens: lexer::HARD_TOKENS,
        ..Default::default()
    };
    let parsed = parser::parse_with_limits(&text, limits).expect("program limits");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    Program {
        world: checked.world,
    }
}

fn evaluate(program: &Program, entry: &str) -> Outcome {
    let mut runtime = Runtime::new(&program.world);
    runtime.fuel = u64::MAX / 2;
    runtime.optimize = false;
    decode(&runtime.run(entry, vec![]).expect(entry))
}

fn jit(program: &Program, entry: &str) -> Outcome {
    decode(&native::jit_with_options(&program.world, entry, false).expect(entry))
}

/// Every `.ves` source of the root package (examples, test fixtures and
/// this package), sorted.
fn fixtures() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "ves") {
                out.push(path);
            }
        }
    }
    let mut out = vec![];
    for dir in ["examples", "tests/fixtures", "packages"] {
        walk(&root().join(dir), &mut out);
    }
    out.sort();
    out
}

fn read_front_end() -> String {
    FRONT_END
        .iter()
        .map(|f| std::fs::read_to_string(root().join(f)).expect("front end source"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(512 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

#[derive(Default, Debug)]
struct Tally {
    tokens_matched: usize,
    tokens_rejected: usize,
    cst_matched: usize,
    cst_unsupported: usize,
    cst_rejected: usize,
    native: usize,
}

/// Sources above this size run on the JIT only: the evaluator needs about
/// 40 s (debug) for each of the front end's own 10 KB files, which put the
/// test over Bazel's 300 s timeout on macOS.
const EVALUATOR_MAX_BYTES: usize = 6 * 1024;

/// Compares one source with the seed: on the evaluator (and also the JIT
/// when `native`), or on the JIT alone when the source is large.
fn compare(front_end: &str, name: &str, source: &str, native: bool, tally: &mut Tally) {
    let trace = std::env::var_os("SELF_HOST_TRACE").is_some();
    let started = std::time::Instant::now();
    let program = program(front_end, source.as_bytes());
    let checked = started.elapsed();
    let (lexed, parsed) = if source.len() > EVALUATOR_MAX_BYTES {
        tally.native += 1;
        (jit(&program, "lex_main"), jit(&program, "parse_main"))
    } else {
        let lexed = evaluate(&program, "lex_main");
        let parsed = evaluate(&program, "parse_main");
        if native {
            assert_eq!(jit(&program, "lex_main"), lexed, "{name}: JIT lexer");
            assert_eq!(jit(&program, "parse_main"), parsed, "{name}: JIT parser");
            tally.native += 1;
        }
        (lexed, parsed)
    };
    if trace {
        eprintln!(
            "{name}: check {checked:?}, run {:?}, total {:?}",
            started.elapsed() - checked,
            started.elapsed()
        );
    }
    match seed_tokens(source) {
        Some(expected) => {
            assert_eq!(lexed, Outcome::Digest(expected), "{name}: token stream");
            tally.tokens_matched += 1;
        }
        None => {
            assert!(
                matches!(lexed, Outcome::Error { code, .. } if code == CODE_LEX || code == CODE_LIMIT),
                "{name}: the seed rejects the tokens, the Vesper lexer returned {lexed:?}"
            );
            tally.tokens_rejected += 1;
        }
    }
    match (seed_cst(source), parsed) {
        (Some(expected), Outcome::Digest(actual)) => {
            assert_eq!(actual, expected, "{name}: CST shape");
            tally.cst_matched += 1;
        }
        (Some(_), Outcome::Error { code, at }) => {
            assert!(
                code >= CODE_UNSUPPORTED || code == CODE_LIMIT,
                "{name}: seed parses cleanly but the Vesper parser failed with code {code} at {at}"
            );
            tally.cst_unsupported += 1;
        }
        (None, Outcome::Error { code, .. }) => {
            assert_ne!(
                code, CODE_OVERFLOW,
                "{name}: arithmetic overflow in the parser"
            );
            tally.cst_rejected += 1;
        }
        (None, Outcome::Digest(_)) => {
            panic!("{name}: the seed reports diagnostics but the Vesper parser returned a tree")
        }
    }
}

/// Every repository source, the front end's own five files included (the
/// compiler parses itself), on four worker threads; the JIT runs on every
/// sixteenth source and on every source above `EVALUATOR_MAX_BYTES`.
#[test]
fn differential_over_repository_sources() {
    let front_end = std::sync::Arc::new(read_front_end());
    let files = fixtures();
    for own in FRONT_END {
        assert!(files.contains(&root().join(own)), "{own}");
    }
    let files = std::sync::Arc::new(files);
    const WORKERS: usize = 4;
    let workers: Vec<_> = (0..WORKERS)
        .map(|w| {
            let front_end = front_end.clone();
            let files = files.clone();
            std::thread::Builder::new()
                .stack_size(512 << 20)
                .spawn(move || {
                    let mut tally = Tally::default();
                    for (i, path) in files.iter().enumerate().skip(w).step_by(WORKERS) {
                        let source = std::fs::read_to_string(path).unwrap();
                        let name = path.strip_prefix(root()).unwrap().display().to_string();
                        compare(&front_end, &name, &source, i % 16 == 0, &mut tally);
                    }
                    tally
                })
                .unwrap()
        })
        .collect();
    let mut tally = Tally::default();
    for worker in workers {
        let t = worker.join().expect("worker");
        tally.tokens_matched += t.tokens_matched;
        tally.tokens_rejected += t.tokens_rejected;
        tally.cst_matched += t.cst_matched;
        tally.cst_unsupported += t.cst_unsupported;
        tally.cst_rejected += t.cst_rejected;
        tally.native += t.native;
    }
    eprintln!("{} sources: {tally:?}", files.len());
    assert!(files.len() >= 100, "{}", files.len());
    assert!(tally.tokens_matched >= 100, "{tally:?}");
    assert!(tally.cst_matched >= 50, "{tally:?}");
    assert!(tally.native >= 8, "{tally:?}");
}

/// What the Vesper front end must report for a hand-written input.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// The seed accepts it and both phases must match the seed.
    Seed,
    /// The seed lexes it but reports a parse diagnostic: tokens must match
    /// and the parser must fail with this code.
    ParseError(i64),
    /// The seed rejects the tokens: the lexer must fail at this offset.
    LexError(i64),
    /// The seed accepts it; the Vesper parser rejects it as unsupported
    /// with this construct code (tokens must still match).
    Unsupported(i64),
}

fn check_case(front_end: &str, source: &str, expect: Expect) {
    let program = program(front_end, source.as_bytes());
    let lexed = evaluate(&program, "lex_main");
    let parsed = evaluate(&program, "parse_main");
    let tokens = seed_tokens(source);
    let cst = seed_cst(source);
    match expect {
        Expect::Seed => {
            assert_eq!(lexed, Outcome::Digest(tokens.expect(source)), "{source}");
            assert_eq!(parsed, Outcome::Digest(cst.expect(source)), "{source}");
        }
        Expect::ParseError(code) => {
            assert_eq!(lexed, Outcome::Digest(tokens.expect(source)), "{source}");
            assert!(cst.is_none(), "the seed accepts {source}");
            assert!(
                matches!(parsed, Outcome::Error { code: c, .. } if c == code),
                "{source}: {parsed:?}"
            );
        }
        Expect::LexError(at) => {
            assert!(tokens.is_none(), "the seed lexes {source}");
            assert_eq!(lexed, Outcome::Error { code: CODE_LEX, at }, "{source}");
            assert_eq!(parsed, Outcome::Error { code: CODE_LEX, at }, "{source}");
        }
        Expect::Unsupported(what) => {
            assert_eq!(lexed, Outcome::Digest(tokens.expect(source)), "{source}");
            assert!(cst.is_some(), "the seed rejects {source}");
            assert!(
                matches!(parsed, Outcome::Error { code, .. } if code == CODE_UNSUPPORTED + what),
                "{source}: {parsed:?}"
            );
        }
    }
}

/// `fn f(x: i64) -> bool { <prefix * n><atom> }`.
fn deep(prefix: &str, n: usize, atom: &str) -> String {
    format!("fn f(x: i64) -> bool {{ {}{atom} }}", prefix.repeat(n))
}

/// `fn f(x: i64) -> i64 { ((...1...)) }` with `n` parentheses.
fn deep_parens(n: usize) -> String {
    format!(
        "fn f(x: i64) -> i64 {{ {}1{} }}",
        "(".repeat(n),
        ")".repeat(n)
    )
}

/// `fn f(x: i64) -> i64 { <head><tail * n> }`.
fn chain(head: &str, tail: &str, n: usize) -> String {
    format!("fn f(x: i64) -> i64 {{ {head}{} }}", tail.repeat(n))
}

/// Lexical edge cases, literal checks, typed errors and the unsupported
/// subset, each against the seed.
#[test]
fn edge_cases_and_typed_errors() {
    on_big_stack(|| {
        let front_end = read_front_end();
        let cases: &[(&str, Expect)] = &[
            // Longest-match numbers: floats, exponents, hex and underscores.
            ("fn f() -> f64 { 1.5e3 + 2e-4 + 3.25E+2 }", Expect::Seed),
            (
                "fn f() -> u64 { 0x_ff + 0XAB_cd + 1_000 + 007 }",
                Expect::Seed,
            ),
            (
                "fn f() -> u64 { 18446744073709551615 + 0xffffffffffffffff }",
                Expect::Seed,
            ),
            // `1.x` is an integer, a dot and a field name; `1e` an int and a name.
            ("fn f(x: i64) -> i64 { 1.x }", Expect::Seed),
            ("fn f() -> i64 { 1e }", Expect::ParseError(CODE_EXPECTED)),
            // Nested block comments, line comments and every trivia byte.
            ("/* a /* b */ c */ fn f() {}\t\r\n\x0c// end", Expect::Seed),
            // Two-byte punctuation and keywords versus identifiers.
            (
                "fn f(inout x: i64, y: i64) { x += 1; x -= y; x *= 2; x /= 3; x %= 4; }",
                Expect::Seed,
            ),
            (
                "fn selfish(iff: bool, inn: bool) -> bool { iff && inn || !iff != inn }",
                Expect::Seed,
            ),
            (
                "fn f() -> String { \"a\\n\\t\\\"\\\\\\/ é\" }",
                Expect::Seed,
            ),
            ("fn f() -> i64 { region r(1073741824) { 1 } }", Expect::Seed),
            // Literal checks the seed performs after lexing.
            (
                "fn f() -> u64 { 18446744073709551616 }",
                Expect::ParseError(CODE_INVALID),
            ),
            (
                "fn f() -> u64 { 0x10000000000000000 }",
                Expect::ParseError(CODE_INVALID),
            ),
            ("fn f() -> u64 { 0x_ }", Expect::ParseError(CODE_INVALID)),
            (
                "fn f() -> String { \"\\q\" }",
                Expect::ParseError(CODE_INVALID),
            ),
            (
                "fn f() -> String { \"a\nb\" }",
                Expect::ParseError(CODE_INVALID),
            ),
            (
                "fn f() -> i64 { region r(0) { 1 } }",
                Expect::ParseError(CODE_INVALID),
            ),
            (
                "fn f() -> i64 { region r(1073741825) { 1 } }",
                Expect::ParseError(CODE_INVALID),
            ),
            // Syntax errors fail instead of recovering.
            (
                "fn f() -> i64 { let x = 1 x }",
                Expect::ParseError(CODE_EXPECTED),
            ),
            (
                "fn f() -> i64 { match 1 { 1 => 2 3 => 4 } }",
                Expect::ParseError(CODE_EXPECTED),
            ),
            (
                "fn f() { a::b @@ @ -> => == $ }",
                Expect::ParseError(CODE_EXPECTED),
            ),
            // Nesting the seed accepts, and nesting past its 128-level
            // descent limit, which must be a typed Limit here.
            (&deep("!", 40, "true"), Expect::Seed),
            (&deep("!", 200, "true"), Expect::ParseError(CODE_LIMIT)),
            (&chain("1", " + 1", 200), Expect::ParseError(CODE_LIMIT)),
            (&chain("x", ".y", 200), Expect::ParseError(CODE_LIMIT)),
            (&deep_parens(60), Expect::ParseError(CODE_LIMIT)),
            (&deep_parens(20), Expect::Seed),
            // Lexical errors carry the offending offset.
            ("fn f() { & }", Expect::LexError(9)),
            ("fn f() { \"open }", Expect::LexError(9)),
            ("fn f() {} /* open", Expect::LexError(10)),
            ("fn f() { \"\\\n\" }", Expect::LexError(9)),
            ("fn \u{e9}() {}", Expect::LexError(3)),
            // Syntax outside the supported subset.
            (
                "trait Score {\n    fn score(in self) -> i64;\n}",
                Expect::Unsupported(1),
            ),
            (
                "struct P {\n    x: i64\n}\nmodel ByX: Score for P {\n    fn score(in self) -> i64 {\n        self.x\n    }\n}",
                Expect::Unsupported(2),
            ),
            (
                "struct P {\n    x: i64\n}\nimpl Score for P {\n    fn score(in self) -> i64 {\n        self.x\n    }\n}",
                Expect::Unsupported(4),
            ),
            ("fn f() -> String { \"\\u0041\" }", Expect::Unsupported(7)),
            ("fn f[T](x: T) -> T::Out { x }", Expect::Unsupported(5)),
            (
                "fn f[e: eff](x: (i64, i64), i: i64) -> i64 eff [Fail[RuntimeError] | e] { x[i] }",
                Expect::Unsupported(6),
            ),
        ];
        for (source, expect) in cases {
            check_case(&front_end, source, *expect);
        }
    });
}
