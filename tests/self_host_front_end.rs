//! Corpus and compatibility acceptance for the self-hosted front end.
#[path = "fixtures/self_host/driver.rs"]
mod support;
use support::*;

#[derive(Default, Debug)]
struct Tally {
    tokens_matched: usize,
    tokens_rejected: usize,
    cst_matched: usize,
    cst_rejected: usize,
    native: usize,
}

/// Large sources use the JIT for lexing. Every source uses the evaluator
/// and bounded memory adapter for parsing and exact record comparison.
const EVALUATOR_MAX_BYTES: usize = 6 * 1024;

/// Compare every stored record; sample the lexer on the JIT as well.
fn compare(front_end: &str, name: &str, source: &str, native: bool, tally: &mut Tally) {
    let trace = std::env::var_os("SELF_HOST_TRACE").is_some();
    let started = std::time::Instant::now();
    let program = program(front_end, source.as_bytes());
    let checked = started.elapsed();
    let (parsed, cells) = transport(&program);
    let parsed = decode(&parsed);
    assert_flat(source, &cells);
    let lexed = if source.len() > EVALUATOR_MAX_BYTES {
        tally.native += 1;
        jit(&program, "lex_main")
    } else {
        let lexed = evaluate(&program, "lex_main");
        if native {
            assert_eq!(jit(&program, "lex_main"), lexed, "{name}: JIT lexer");
            tally.native += 1;
        }
        lexed
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
            panic!(
                "{name}: seed parses cleanly but the Vesper parser failed with code {code} at {at}"
            );
        }
        (None, Outcome::Error { code, .. }) => {
            let diagnostics = vesper::parser::parse(source).diagnostics;
            let diagnostic_code = match code {
                1 => "E0001",
                5 => "E0003",
                _ => "E0002",
            };
            if code == CODE_LIMIT {
                assert!(
                    diagnostics.iter().any(|d| d.code == diagnostic_code),
                    "{name}: {diagnostics:?}"
                );
            } else {
                assert_eq!(
                    diagnostics[0].code, diagnostic_code,
                    "{name}: diagnostic code"
                );
            }
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

/// Every repository source, including crate, bootstrap and editor fixtures
/// and all parser sources, on four workers. The lexer also runs on the JIT
/// for every fourth source and every source above EVALUATOR_MAX_BYTES.
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
                        compare(&front_end, &name, &source, i % 4 == 0, &mut tally);
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
        tally.cst_rejected += t.cst_rejected;
        tally.native += t.native;
    }
    eprintln!("{} sources: {tally:?}", files.len());
    assert!(files.len() >= 363, "{}", files.len());
    assert!(tally.tokens_matched >= 100, "{tally:?}");
    assert!(tally.cst_matched >= 50, "{tally:?}");
    assert!(tally.native >= 50, "{tally:?}");
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
            // Tensor shapes, dimension parameters and wire structs (§28.2).
            (
                "wire struct H { a: u32 }\nfn f[M: dim, N: dim](x: Tensor[f32, [M, N * 4, 128]]) -> Tensor[f32, [N, M]] { Tensor::reshape[[N, M]](x) }",
                Expect::Seed,
            ),
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
                Expect::Seed,
            ),
            (
                "struct P {\n    x: i64\n}\nmodel ByX: Score for P {\n    fn score(in self) -> i64 {\n        self.x\n    }\n}",
                Expect::Seed,
            ),
            (
                "struct P {\n    x: i64\n}\nimpl Score for P {\n    fn score(in self) -> i64 {\n        self.x\n    }\n}",
                Expect::Seed,
            ),
            ("fn f() -> String { \"\\u0041\" }", Expect::Seed),
            ("fn f[T](x: T) -> T::Out { x }", Expect::Seed),
            (
                "fn f[e: eff](x: (i64, i64), i: i64) -> i64 eff [Fail[RuntimeError] | e] { x[i] }",
                Expect::Seed,
            ),
        ];
        for (source, expect) in cases {
            check_case(&front_end, source, *expect);
        }
    });
}
