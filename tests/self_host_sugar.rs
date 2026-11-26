//! Self-hosting SH1 lane 14 (#1157) part 4a: `@vesper/sugar`'s rule
//! matcher (packages/vesper-sugar/src/matcher.ves) against
//! `vesper_syntax::sugar_expand`'s (`matcher::match_rule`, through the
//! `match_rule_words` test hook): the same captures, values, extents,
//! refusals and remaining work budget on a rule corpus drawn from the
//! sugar test suites, and on a fuzzed corpus. Part 4b-1: its template
//! skeleton (template.ves, `template::render` before the flow analysis,
//! through the `template_skeleton_words` hook) on the rules of the same
//! suites and on fuzzed templates. Part 4b-4: the rendered template
//! (flow.ves, the flow analysis of `template::render`, through
//! `template_render_words`) on the same corpora. Part 4b-5: exit-aware
//! scope captures (scope.ves, `scope::lower`, through
//! `template_scope_words`) on scope rules over a corpus of callers and a
//! fuzzed corpus. The pattern template hygiene (hygiene.ves,
//! `hygiene::pattern`, through `template_pattern_words`) on pattern rules
//! and on every template corpus above.
use std::path::PathBuf;
use std::sync::OnceLock;
use vesper::{check, native, packages};
use vesper_syntax::{parser, sugar_expand};

const PACKAGE: &str = "packages/vesper-sugar";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Sugar {
    matcher: native::WordFunction,
    batch: native::WordFunction,
    skeleton: native::WordFunction,
    render: native::WordFunction,
    scope: native::WordFunction,
    pattern: native::WordFunction,
}

fn sugar() -> &'static Sugar {
    static F: OnceLock<Sugar> = OnceLock::new();
    F.get_or_init(|| {
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
        Sugar {
            matcher: compile("match_rule_words"),
            batch: compile("match_rule_batch"),
            skeleton: compile("template_skeleton_words"),
            render: compile("template_render_words"),
            scope: compile("template_scope_words"),
            pattern: compile("template_pattern_words"),
        }
    })
}

const JIT_RUNTIME: vesper_port_abi::SeqRuntime = {
    extern "C" fn new(cap: i64, stride: i64) -> i64 {
        vesper::seq::native_runtime::vesper_seq_new(cap as u64, stride as u64) as i64
    }
    extern "C" fn free(handle: i64) -> i64 {
        vesper::seq::native_runtime::vesper_seq_free(handle as u64) as i64
    }
    vesper_port_abi::SeqRuntime { new, free }
};

fn vesper_match(pattern: &str, invocation: &str, tail: Option<&str>, work: usize) -> Vec<u64> {
    // Native word functions take at most three words: the texts share one
    // string, their lengths one word.
    let text = format!("{pattern}{invocation}{}", tail.unwrap_or(""));
    let lens =
        pattern.len() as u64 | (invocation.len() as u64) << 32 | u64::from(tail.is_some()) << 63;
    let call = vesper_port_abi::HostCall::new()
        .str(&text)
        .word(lens)
        .word(work as u64);
    // SAFETY: the entry takes an `in Str` (passed as the address of its
    // three words) and two `u64` words, and returns a live sequence of the
    // JIT runtime holding `u64`s, which `HostCall` copies and frees.
    unsafe { call.seq(JIT_RUNTIME, |w| sugar().matcher.call(w).unwrap()) }
}

struct Case {
    pattern: String,
    invocation: String,
    tail: Option<String>,
    work: usize,
}

fn compare(c: &Case) -> Option<String> {
    let tail = c.tail.as_deref();
    let rust = sugar_expand::match_rule_words(&c.pattern, &c.invocation, tail, c.work);
    let vesper = vesper_match(&c.pattern, &c.invocation, tail, c.work);
    (rust != vesper).then(|| {
        format!(
            "pattern {:?} invocation {:?} tail {:?} work {}:\n  rust   {:?}\n  vesper {:?}",
            c.pattern, c.invocation, c.tail, c.work, rust, vesper
        )
    })
}

fn check_all(cases: &[Case]) {
    let failures: Vec<_> = cases.iter().filter_map(compare).collect();
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

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
    out
}

/// Files whose string literals hold sugar declarations and uses.
const SUGAR_FILES: &[&str] = &[
    "crates/vesper-syntax/tests/sugar_expand.rs",
    "crates/vesper-syntax/tests/sugar_surface.rs",
    "crates/vesper-syntax/src/sugar_expand.rs",
    "src/forge_frontend/sugar.rs",
    "src/packages/sugar.rs",
    "tests/forge_cli.rs",
    "tests/package_paths.rs",
    "tests/self_host_front_v2.rs",
];

fn significant_start(node: &parser::SyntaxNode) -> usize {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        // Kinds 0-2 are whitespace and comments.
        .find(|t| t.kind().0 > 2)
        .map_or(usize::from(node.text_range().start()), |t| {
            usize::from(t.text_range().start())
        })
}

/// Rule patterns and sugar uses of the sugar sources of the test suites:
/// every pattern against every use of its own source and against a
/// spread of uses from the others, with the rest of the use's source as
/// the block tail.
fn corpus_cases() -> Vec<Case> {
    let mut patterns = vec![];
    let mut uses = vec![];
    for (id, source) in SUGAR_FILES
        .iter()
        .filter_map(|file| std::fs::read_to_string(root().join(file)).ok())
        .flat_map(|text| rust_string_literals(&text))
        .filter(|s| s.contains("rule"))
        .enumerate()
    {
        let Ok(parse) = parser::parse_surface_cst(&source, Default::default(), true) else {
            continue;
        };
        for node in parse.syntax().descendants() {
            let r = node.text_range();
            let (s, e) = (usize::from(r.start()), usize::from(r.end()));
            if node.kind() == parser::SUGAR_PATTERN && e >= s + 2 {
                patterns.push((id, source[s + 1..e - 1].to_owned()));
            } else if matches!(
                node.kind(),
                parser::SUGAR_ITEM
                    | parser::SUGAR_PAT
                    | parser::SUGAR_STMT
                    | parser::SUGAR_EXPR
                    | parser::DOTTED_EXPR
                    | parser::PIPE_EXPR
                    | parser::TAGGED_LITERAL
            ) {
                let s = significant_start(&node);
                uses.push((id, source[s..e].to_owned(), source[e..].to_owned()));
            }
        }
    }
    let mut cases = vec![];
    for (n, (id, p)) in patterns.iter().enumerate() {
        let own = uses.iter().filter(|u| u.0 == *id);
        let spread = (0..8).map(|k| &uses[(n * 7 + k * 13) % uses.len().max(1)]);
        for (_, u, rest) in own.chain(spread.filter(|_| !uses.is_empty())) {
            let tail = p.contains("$..").then(|| rest.clone());
            cases.push(Case {
                pattern: p.clone(),
                invocation: u.clone(),
                tail,
                work: 1 << 20,
            });
        }
    }
    cases
}

/// The matcher's own examples (sugar_expand/matcher.rs tests) and the
/// sorts, repetitions, passing modes, adjacency and refusals around them.
const CASES: &[(&str, &str)] = &[
    (
        "while $cond:expr invariant $ok:expr $b:block",
        "while a < n invariant a <= n { step(); }",
    ),
    (
        "swap inout $a:place with inout $b:place ;",
        "swap inout a[i] with inout s.y;",
    ),
    ("swap inout $x:place ;", "swap inout get();"),
    (
        "swap in $x:place out $y:place sink $z:expr ;",
        "swap in a.b out c[d] sink 3;",
    ),
    (
        "rows { $( [ $( $v:expr ),* ] );* }",
        "rows { []; [f(1,2), 3] }",
    ),
    (
        "rows { $( [ $( $v:expr ),+ ] );+ }",
        "rows { [1]; [2, 3]; [4] }",
    ),
    (
        "$lhs:expr |?> $callee:ident ( $..args:args )",
        "frame |?> decode(x, in s)",
    ),
    ("tag{ x }", "tag { x }"),
    ("tag{ x }", "tag{x}"),
    ("tag\"x\"", "tag\"x\""),
    ("x .+ y", "x . + y"),
    ("x .+ y", "x .+ y"),
    ("$a:expr .<=> $b:expr", "p .<=> q"),
    ("unless $c:expr $body:block", "unless x { g(); }"),
    ("unless $c:expr $body:block", "unless x { g(); };"),
    ("unless $c:expr $body:block ;", "unless x { g(); };"),
    ("ghost twice $e:expr", "ghost twice 3"),
    ("twice $e:expr", "twice f(a, b) + 1"),
    ("n $x:ident", "n y"),
    ("n $x:ident", "n y z"),
    ("n $x:ident", "n 3"),
    ("let2 $p:pat = $e:expr", "let2 (a, b) = pair()"),
    ("let2 $( $p:pat ),* = $e:expr", "let2 a, Some(b), _ = x"),
    ("cast $e:expr as $t:ty", "cast x as Vec[u8]"),
    ("call $f:ident ( $..a:args )", "call f(1, in x, inout y)"),
    ("call $f:ident ( $..a:args )", "call f()"),
    ("pats ( $..ps:pats )", "pats (a, b, _)"),
    ("block $b:block", "block { 1 }"),
    ("block $b:block", "block (1)"),
    ("scoped $s:scope", "scoped { x(); y }"),
    ("defer $b:block $..rest:scope", "defer { tidy(); };"),
    ("each $..body:stmts", "each a(); b();"),
    ("items { $..i:items }", "items { fn a() {} struct B {} }"),
    ("odd $x:wat", "odd 1"),
    ("odd $..x:block", "odd { 1 }"),
    ("odd $..x:place", "odd a.b"),
    ("odd $..x:ident", "odd a"),
    ("bad $x", "bad 1"),
    ("bad $x:", "bad 1"),
    ("bad $x:expr $x:expr", "bad 1 2"),
    ("bad $( $x:expr )", "bad 1"),
    ("bad $( $x:expr ) ,", "bad 1"),
    ("bad $( $x:expr ) , ?", "bad 1"),
    ("bad ( $x:expr", "bad (1"),
    ("bad $x:expr )", "bad 1)"),
    ("bad [ $x:expr )", "bad [1)"),
    ("bad $x:expr", "bad `"),
    ("bad $x:expr", "bad /* open"),
    ("$( $x:expr )* z", "a a a a a a a a a a a a a z"),
    ("$( $( $x:expr ),* );*", "1, 2; ; 3"),
    ("sum $( $x:expr )++", "sum 1 + 2 + 3"),
    ("place $p:place", "place self.a[i].b"),
    ("place $p:place", "place a[]"),
    ("place $p:place", "place a.3"),
    ("e $x:expr ;", "e { a; b };"),
    ("e $x:expr", "e a, b"),
    ("e $x:expr", "e a; b"),
    ("e $..x:stmts", "e a; b"),
    ("pipe $x:expr |> $f:ident", "pipe a |> f"),
    ("$x:expr |> $f:ident ( $..a:args )", "a |> f(_, 2)"),
    ("with $x:expr", "with 1"),
    ("eff $x:expr", "eff 1"),
];

fn work_budgets() -> [usize; 4] {
    [1 << 20, 64, 300, 1500]
}

#[test]
fn matcher_matches_on_the_rule_examples() {
    let mut cases = vec![];
    for &(pattern, invocation) in CASES {
        for work in work_budgets() {
            for tail in [None, Some(" next(); value")] {
                cases.push(Case {
                    pattern: pattern.into(),
                    invocation: invocation.into(),
                    tail: tail.map(str::to_owned),
                    work,
                });
            }
        }
    }
    check_all(&cases);
}

#[test]
fn matcher_matches_on_the_sugar_test_suites() {
    let cases = corpus_cases();
    assert!(cases.len() > 200, "{} corpus cases", cases.len());
    check_all(&cases);
}

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

/// Fragments the fuzzer splices into patterns and invocations.
const FRAGMENTS: &[&str] = &[
    "$x:expr",
    "$y:ident",
    "$p:place",
    "$b:block",
    "$s:scope",
    "$t:ty",
    "$q:pat",
    "$..a:args",
    "$..ps:pats",
    "$..st:stmts",
    "$..sc:scope",
    "$..it:items",
    "$(",
    ")*",
    ")+",
    "),*",
    ");+",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ";",
    ",",
    ".",
    "..",
    ".+",
    "|>",
    "|?>",
    "in",
    "inout",
    "out",
    "sink",
    "ghost",
    "a",
    "b",
    "f",
    "1",
    "\"s\"",
    "x.y",
    "a[i]",
    "self",
    "{ }",
    "{ a; }",
    "_",
    "+",
    "<",
    "=>",
    "`",
    "/*",
    "é",
];

fn mutate(rng: &mut Rng, base: &str) -> String {
    let mut words: Vec<String> = base.split(' ').map(str::to_owned).collect();
    for _ in 0..=rng.below(3) {
        let at = rng.below(words.len() + 1);
        match rng.below(4) {
            0 if !words.is_empty() => {
                words.remove(at.min(words.len() - 1));
            }
            1 if !words.is_empty() => {
                let i = at.min(words.len() - 1);
                words[i] = FRAGMENTS[rng.below(FRAGMENTS.len())].to_owned();
            }
            _ => words.insert(at, FRAGMENTS[rng.below(FRAGMENTS.len())].to_owned()),
        }
    }
    let sep = if rng.below(4) == 0 { "" } else { " " };
    words.join(sep)
}

#[test]
fn matcher_matches_on_the_fuzzed_corpus() {
    let rounds: usize = std::env::var("VESPER_SUGAR_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4000);
    let mut rng = Rng(0x5eed_1157);
    let mut cases = vec![];
    for _ in 0..rounds {
        let (p, i) = CASES[rng.below(CASES.len())];
        let pattern = if rng.below(2) == 0 {
            mutate(&mut rng, p)
        } else {
            p.to_owned()
        };
        let invocation = if rng.below(2) == 0 {
            mutate(&mut rng, i)
        } else {
            i.to_owned()
        };
        let tail = (rng.below(3) == 0).then(|| " rest(); x".to_owned());
        let work = work_budgets()[rng.below(4)];
        cases.push(Case {
            pattern,
            invocation,
            tail,
            work,
        });
    }
    check_all(&cases);
}

#[test]
#[ignore = "speed measurement; run with --ignored --nocapture in release"]
fn matcher_speed() {
    let mut cases = corpus_cases();
    for &(p, i) in CASES {
        cases.push(Case {
            pattern: p.into(),
            invocation: i.into(),
            tail: Some(" next(); value".into()),
            work: 1 << 20,
        });
    }
    // One host call over all cases (`match_rule_batch`), so the time is
    // the matcher's and not the call boundary's.
    let mut text = String::new();
    let number = |text: &mut String, n: usize| {
        for k in 0..4 {
            text.push(char::from((n >> (7 * k)) as u8 & 127));
        }
    };
    for c in &cases {
        number(&mut text, c.pattern.len());
        number(&mut text, c.invocation.len());
        number(&mut text, c.tail.as_ref().map_or(0, |t| t.len() + 1));
        text.push_str(&c.pattern);
        text.push_str(&c.invocation);
    }
    let batch = || -> Vec<u64> {
        let call = vesper_port_abi::HostCall::new()
            .str(&text)
            .word(cases.len() as u64)
            .word(1 << 20);
        // SAFETY: as `vesper_match`.
        unsafe { call.seq(JIT_RUNTIME, |w| sugar().batch.call(w).unwrap()) }
    };
    let rust_words = || {
        let mut out: Vec<u64> = vec![];
        for c in &cases {
            out.extend(sugar_expand::match_rule_words(
                &c.pattern,
                &c.invocation,
                c.tail.as_deref(),
                c.work,
            ));
        }
        out
    };
    assert_eq!(batch(), rust_words(), "batch words");
    let time = |f: &dyn Fn()| {
        let mut best = std::time::Duration::MAX;
        for _ in 0..7 {
            let t = std::time::Instant::now();
            for _ in 0..20 {
                f();
            }
            best = best.min(t.elapsed());
        }
        best
    };
    let rust = time(&|| {
        std::hint::black_box(rust_words());
    });
    let vesper = time(&|| {
        std::hint::black_box(batch());
    });
    println!(
        "{} cases: rust {rust:?} vesper {vesper:?} ratio {:.2}x",
        cases.len(),
        vesper.as_secs_f64() / rust.as_secs_f64()
    );
}

struct Render {
    pattern: String,
    invocation: String,
    tail: Option<String>,
    template: String,
    max: usize,
}

const MARK: &str = "__sugar7_3_";

/// Which template stage a differential compares.
#[derive(Clone, Copy)]
enum Stage {
    Skeleton,
    Render,
    Pattern,
}

fn vesper_template(stage: Stage, r: &Render) -> Vec<u64> {
    let mut text = String::new();
    let number = |text: &mut String, n: usize| {
        for k in 0..4 {
            text.push(char::from((n >> (7 * k)) as u8 & 127));
        }
    };
    number(&mut text, r.pattern.len());
    number(&mut text, r.invocation.len());
    number(&mut text, r.tail.as_ref().map_or(0, |t| t.len() + 1));
    number(&mut text, r.template.len());
    number(&mut text, MARK.len());
    for part in [
        r.pattern.as_str(),
        &r.invocation,
        r.tail.as_deref().unwrap_or(""),
        &r.template,
        MARK,
    ] {
        text.push_str(part);
    }
    let call = vesper_port_abi::HostCall::new()
        .str(&text)
        .word(r.max as u64)
        .word(1 << 20);
    // SAFETY: as `vesper_match`.
    let f = match stage {
        Stage::Skeleton => &sugar().skeleton,
        Stage::Render => &sugar().render,
        Stage::Pattern => &sugar().pattern,
    };
    unsafe { call.seq(JIT_RUNTIME, |w| f.call(w).unwrap()) }
}

/// The cases that match, and the outcomes (skeleton or refusal) seen.
fn check_renders(stage: Stage, renders: &[Render]) -> (usize, usize) {
    let mut failures = vec![];
    let (mut matched, mut ok) = (0, 0);
    for r in renders {
        let rust_words = match stage {
            Stage::Skeleton => sugar_expand::template_skeleton_words,
            Stage::Render => sugar_expand::template_render_words,
            Stage::Pattern => sugar_expand::template_pattern_words,
        };
        let rust = rust_words(
            &r.pattern,
            &r.invocation,
            r.tail.as_deref(),
            &r.template,
            MARK,
            r.max,
            1 << 20,
        );
        let vesper = vesper_template(stage, r);
        matched += usize::from(rust[0] != 0);
        ok += usize::from(rust[0] == 1);
        if rust != vesper {
            failures.push(format!(
                "pattern {:?} invocation {:?} tail {:?} template {:?} max {}:\n  rust   {:?}\n  vesper {:?}",
                r.pattern, r.invocation, r.tail, r.template, r.max, rust, vesper
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} renders differ:\n{}",
        failures.len(),
        renders.len(),
        failures[..failures.len().min(20)].join("\n")
    );
    (matched, ok)
}

/// Rules (pattern and template) of the sugar sources, each against the
/// uses of its own source and a spread of the others.
fn corpus_renders() -> Vec<Render> {
    let mut rules = vec![];
    let mut uses = vec![];
    for (id, source) in SUGAR_FILES
        .iter()
        .filter_map(|file| std::fs::read_to_string(root().join(file)).ok())
        .flat_map(|text| rust_string_literals(&text))
        .filter(|s| s.contains("rule"))
        .enumerate()
    {
        let Ok(parse) = parser::parse_surface_cst(&source, Default::default(), true) else {
            continue;
        };
        let inner = |n: &parser::SyntaxNode| {
            let r = n.text_range();
            let (s, e) = (usize::from(r.start()), usize::from(r.end()));
            (e >= s + 2).then(|| source[s + 1..e - 1].to_owned())
        };
        for node in parse.syntax().descendants() {
            if node.kind() == parser::SUGAR_RULE {
                let part = |k| node.children().find(|n| n.kind() == k);
                if let (Some(Some(p)), Some(Some(t))) = (
                    part(parser::SUGAR_PATTERN).map(|n| inner(&n)),
                    part(parser::SUGAR_TEMPLATE).map(|n| inner(&n)),
                ) {
                    rules.push((id, p, t));
                }
            } else if matches!(
                node.kind(),
                parser::SUGAR_ITEM
                    | parser::SUGAR_PAT
                    | parser::SUGAR_STMT
                    | parser::SUGAR_EXPR
                    | parser::DOTTED_EXPR
                    | parser::PIPE_EXPR
                    | parser::TAGGED_LITERAL
            ) {
                let r = node.text_range();
                let s = significant_start(&node);
                let e = usize::from(r.end());
                uses.push((id, source[s..e].to_owned(), source[e..].to_owned()));
            }
        }
    }
    let mut renders = vec![];
    for (n, (id, p, t)) in rules.iter().enumerate() {
        let own = uses.iter().filter(|u| u.0 == *id);
        let spread = (0..8).map(|k| &uses[(n * 7 + k * 13) % uses.len().max(1)]);
        for (_, u, rest) in own.chain(spread.filter(|_| !uses.is_empty())) {
            renders.push(Render {
                pattern: p.clone(),
                invocation: u.clone(),
                tail: p.contains("$..").then(|| rest.clone()),
                template: t.clone(),
                max: 1 << 20,
            });
        }
    }
    renders
}

/// Templates over the rule examples: repetitions (nested, separated,
/// empty), splices, shared holes, grouping operands and refusals.
const TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "rows { $( [ $( $v:expr ),* ] );* }",
        "rows { []; [f(1,2), 3] }",
        "rows($(row($($v),*)),*)",
    ),
    (
        "rows { $( [ $( $v:expr ),* ] );* }",
        "rows { []; [f(1,2), 3] }",
        "$(row($y,$($v),*)),*",
    ),
    (
        "rows { $( [ $( $v:expr ),* ] );* }",
        "rows { [1]; [2, 3] }",
        "$( $( $v + $v )+* );*",
    ),
    (
        "rows { $( [ $( $v:expr ),+ ] );+ }",
        "rows { [1]; [2, 3]; [4] }",
        "$( $( $v )|+ )=>*",
    ),
    ("rows { $( [ $( $v:expr ),* ] );* }", "rows { [1] }", "$v"),
    (
        "rows { $( [ $( $v:expr ),* ] );* }",
        "rows { [1] }",
        "$($v)*",
    ),
    (
        "rows { $( [ $( $v:expr ),* ] );* }",
        "rows { [] }",
        "$($($v)+)*",
    ),
    ("twice $e:expr", "twice f(a, b) + 1", "{ let t=$e; t + $e }"),
    ("twice $e:expr", "twice ((x))", "inner($e, ($e))"),
    ("twice $e:expr", "twice (x, y)", "$e"),
    ("twice $e:expr", "twice ((a), (b))", "$e"),
    ("twice $e:expr", "twice (((a)))", "$e"),
    ("twice $e:expr", "twice ()", "$e"),
    ("twice $e:expr", "twice 1", "$..e"),
    ("twice $e:expr", "twice 1", "$ e $  e"),
    ("twice $e:expr", "twice 1", "$"),
    ("twice $e:expr", "twice 1", "$ 3"),
    ("twice $e:expr", "twice 1", "$( $e"),
    ("twice $e:expr", "twice 1", "$( $e )"),
    ("twice $e:expr", "twice 1", "$( $e ) ab *"),
    ("twice $e:expr", "twice 1", "$( $e ) , ?"),
    ("twice $e:expr", "twice 1", "$( 1 )*"),
    ("twice $e:expr", "twice 1", "$x"),
    ("twice $e:expr", "twice 1", "`"),
    ("twice $e:expr", "twice 1", "/* open"),
    ("twice $e:expr", "twice 1", "$( $( $( $e )* )* )*"),
    (
        "call $f:ident ( $..a:args )",
        "call f(1, in x, inout y)",
        "$f($..a)",
    ),
    ("call $f:ident ( $..a:args )", "call f()", "$f($..a, 2)"),
    ("call $f:ident ( $..a:args )", "call f(1)", "$f($a)"),
    ("call $f:ident ( $..a:args )", "call f(1)", "$..f($..a)"),
    ("scoped $s:scope", "scoped { x(); y }", "{ $..s }"),
    ("scoped $s:scope", "scoped { x(); y }", "$s"),
    (
        "defer $b:block $..rest:scope",
        "defer { tidy(); };",
        "{ $..rest; $b }",
    ),
    (
        "defer $b:block $..rest:scope",
        "defer { tidy(); };",
        "{ $rest; $b }",
    ),
    ("each $..body:stmts", "each a(); b();", "$..body $..body"),
    (
        "items { $..i:items }",
        "items { fn a() {} struct B {} }",
        "mod m { $..i }",
    ),
    ("place $p:place", "place ((obj.field))", "read(in $p)"),
    (
        "swap inout $a:place with inout $b:place ;",
        "swap inout a[i] with inout s.y;",
        "{ let t = $a; $a = $b; $b = t; }",
    ),
    (
        "let2 $( $p:pat ),* = $e:expr",
        "let2 a, Some(b), _ = x",
        "match $e { $( $p => 1 ),* }",
    ),
    (
        "let2 $( $p:pat ),* = $e:expr",
        "let2 a = x",
        "$( $p ),+ $( $p )|*",
    ),
    ("$( $x:expr )* z", "a a a z", "[$($x),*]"),
    ("$( $x:expr )* z", "z", "[$($x),+]"),
    ("$( $x:expr )* z", "z", "[$($x),*]"),
    (
        "$( $( $x:expr ),* );*",
        "1, 2; ; 3",
        "$( [ $( $x ),* ] )..*",
    ),
    (
        "cast $e:expr as $t:ty",
        "cast x as Vec[u8]",
        "($e : $t) with state linear eff",
    ),
    ("pats ( $..ps:pats )", "pats (a, b, _)", "fn f($..ps) {}"),
    ("pats ( $..ps:pats )", "pats ()", "f($..ps, 1)"),
];

fn max_bytes() -> [usize; 4] {
    [1 << 20, 4096, 24, 7]
}

fn example_renders() -> Vec<Render> {
    let mut renders = vec![];
    for &(pattern, invocation, template) in TEMPLATES {
        for max in max_bytes() {
            for tail in [None, Some(" next(); value")] {
                renders.push(Render {
                    pattern: pattern.into(),
                    invocation: invocation.into(),
                    tail: tail.filter(|_| pattern.contains("$..")).map(str::to_owned),
                    template: template.into(),
                    max,
                });
            }
        }
    }
    renders
}

#[test]
fn skeleton_matches_on_the_template_examples() {
    let renders = example_renders();
    let (matched, ok) = check_renders(Stage::Skeleton, &renders);
    assert!(
        matched * 2 > renders.len() && ok > 40,
        "{matched} matched, {ok} ok"
    );
}

#[test]
fn render_matches_on_the_template_examples() {
    let renders = example_renders();
    let (matched, ok) = check_renders(Stage::Render, &renders);
    assert!(
        matched * 2 > renders.len() && ok > 30,
        "{matched} matched, {ok} ok"
    );
}

#[test]
fn skeleton_matches_on_the_sugar_test_suites() {
    let renders = corpus_renders();
    let (matched, ok) = check_renders(Stage::Skeleton, &renders);
    assert!(
        ok > 50,
        "{} renders, {matched} matched, {ok} ok",
        renders.len()
    );
}

#[test]
fn render_matches_on_the_sugar_test_suites() {
    let renders = corpus_renders();
    let (matched, ok) = check_renders(Stage::Render, &renders);
    assert!(
        ok > 40,
        "{} renders, {matched} matched, {ok} ok",
        renders.len()
    );
}

const TEMPLATE_FRAGMENTS: &[&str] = &[
    "$x", "$e", "$v", "$p", "$..a", "$..s", "$..e", "$(", ")*", ")+", "),*", ");+", ")|+", "$",
    "(", ")", "{", "}", ";", ",", "let", "t", "=", "in", "inout", "..", "`", "\"s\"", "1",
];

fn fuzzed_renders(seed: u64) -> Vec<Render> {
    let rounds: usize = std::env::var("VESPER_SUGAR_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4000);
    let mut rng = Rng(seed);
    let mut renders = vec![];
    for _ in 0..rounds {
        let (p, i, t) = TEMPLATES[rng.below(TEMPLATES.len())];
        let mut template: Vec<String> = t.split(' ').map(str::to_owned).collect();
        for _ in 0..=rng.below(3) {
            let at = rng.below(template.len() + 1);
            let f = TEMPLATE_FRAGMENTS[rng.below(TEMPLATE_FRAGMENTS.len())].to_owned();
            match rng.below(3) {
                0 if !template.is_empty() => {
                    template.remove(at.min(template.len() - 1));
                }
                1 if !template.is_empty() => {
                    let k = at.min(template.len() - 1);
                    template[k] = f;
                }
                _ => template.insert(at, f),
            }
        }
        let invocation = if rng.below(3) == 0 {
            mutate(&mut rng, i)
        } else {
            i.to_owned()
        };
        renders.push(Render {
            pattern: p.into(),
            invocation,
            tail: p.contains("$..").then(|| " rest(); x".to_owned()),
            template: template.join(if rng.below(4) == 0 { "" } else { " " }),
            max: max_bytes()[rng.below(4)],
        });
    }
    renders
}

#[test]
fn skeleton_matches_on_the_fuzzed_corpus() {
    check_renders(Stage::Skeleton, &fuzzed_renders(0x5eed_4b01));
}

#[test]
fn render_matches_on_the_fuzzed_corpus() {
    check_renders(Stage::Render, &fuzzed_renders(0x5eed_4b04));
}

// ---------------------------------------------------------- scope capture

/// Scope rules: the supported protocol and its refusals (unbound, resumed
/// twice, resumed before cleanup, two scopes).
const SCOPE_RULES: &[(&str, &str)] = &[
    (
        "defer $cleanup:block $..rest:scope",
        "{\n let exit = $..rest;\n $cleanup;\n exit.resume()\n }",
    ),
    ("holding $body:scope", "{let result=$body;result.resume()}"),
    (
        "defer $cleanup:block $..rest:scope",
        "{ $cleanup; $..rest }",
    ),
    (
        "defer $cleanup:block $..rest:scope",
        "{ let exit = $..rest; exit.resume(); exit.resume() }",
    ),
    (
        "defer $cleanup:block $..rest:scope",
        "{ let exit = $..rest; exit.resume(); $cleanup }",
    ),
    (
        "defer $cleanup:block $..rest:scope",
        "{ let exit = $..rest; let again = $..rest; $cleanup; exit.resume() }",
    ),
    (
        "defer $cleanup:block $..rest:scope",
        "{ let exit = $..rest; $cleanup; { exit.resume() } }",
    ),
];

/// Callers of the scope rules: exits of every kind, loops, closures,
/// handlers, regions, Fail rows and Option `?`.
const SCOPE_PROGRAMS: &[&str] = &[
    "fn early(inout n:i64, flag: bool)->i64 {\n defer { n = wrapping_add(n,1); }\n if flag { return 3; } 4\n}",
    "fn failed(inout n:i64)->i64 eff [Fail[RuntimeError]] {\n defer { n = wrapping_add(n,1); }\n Fail::raise(RuntimeError::DivisionByZero)\n}",
    "fn main()->i64 {\n let mut n:i64=0;\n let v = loop {\n defer { n = wrapping_add(n,1); }\n if n < 4 {continue;} break 5;\n };\n v\n}",
    "fn main()->i64 {\n let mut n:i64=0;\n loop {\n defer { n = wrapping_add(n,1); }\n if n < 4 {continue;} break;\n }\n n\n}",
    "fn f()->i64 { holding { 41 } }",
    "fn f()->i64 { holding { if g() { return 1; } 2 } }",
    "fn f(xs: Vec[i64])->i64 {\n defer { log(); }\n for x in xs { if x > 3 { return x; } }\n 0\n}",
    "fn f()->i64 {\n defer { log(); }\n loop { if g() { return 7; } break; }\n 0\n}",
    "fn f()->Option[i64] {\n defer { log(); }\n let x = g()?;\n Option::Some(x)\n}",
    "fn f()->Option[i64] eff [Fail[RuntimeError]] {\n defer {}\n let x = Some(1)?;\n Some(x)\n}",
    "fn f()->Option[(i64, bool)] {\n defer { log(); }\n let x = g()?.h()?;\n Option::Some((x, true))\n}",
    "fn f()->i64 eff [Fail[RuntimeError], Fail[ParseError]] {\n defer { log(); }\n if g() { return 1; }\n parse()\n}",
    "fn f()->i64 eff [Fail[RuntimeError], Fail[RuntimeError]] {\n defer { log(); }\n 3\n}",
    "effect Stop {fn stop()->Never;}\nfn f()->i64 eff [Stop]{\n defer {}\n Stop::stop()\n}",
    "linear effect Ask { fn ask() -> i64; }\nfn f()->i64 eff [Ask] {\n defer { log(); }\n Ask::ask()\n}",
    "fn f()->i64 eff [ConstTime, DetFloat] {\n defer { log(); }\n 3\n}",
    "fn f()->i64 {\n handle stop:Fail[i64] in {\n defer {}\n stop.raise(3)\n } with {return v=>v,raise(e,_)=>e,}\n}",
    "fn f()->i64 {\n handle Fail[i64] in {\n defer { log(); }\n if g() { return 2; }\n 1\n } with {return v=>v,raise(e,_)=>e,}\n}",
    "fn f()->i64 {\n defer { log(); }\n let k = |x: i64| -> i64 { return x; };\n k(2)\n}",
    "fn f()->i64 {\n let k = || -> i64 eff [Fail[RuntimeError]] {\n defer { log(); }\n if g() { return 1; }\n 2\n };\n 3\n}",
    "fn f()->i64 {\n let k = || {\n defer { log(); }\n 2\n };\n 3\n}",
    "fn f()->i64 {\n defer { log(); }\n handle Fail[i64] in { if g() { return 1; } 2 } with {return v=>v,raise(e,_)=>e,}\n}",
    "fn f()->i64 {\n defer { log(); }\n handle Fail[i64] in { 2 } with {return v=>{ return v; },raise(e,_)=>e,}\n}",
    "fn f()->i64 {\n defer { log(); }\n region r {\n if g() { return 1; }\n }\n 2\n}",
    "fn f()->i64 {\n loop {\n defer { log(); }\n if a() { break; }\n if b() { continue; }\n if c() { return 4; }\n }\n 0\n}",
    "fn f()->i64 {\n let v = loop {\n defer { log(); }\n if a() { break 1; }\n if c() { return 4; }\n break 2;\n };\n v\n}",
    "fn f()->i64 {\n for i in 0..3 {\n defer { log(); }\n if i == 1 { continue; }\n loop { if b() { return 3; } break; }\n }\n 0\n}",
    "fn f()->fn() -> i64 {\n defer { log(); }\n for i in 0..3 { return g; }\n g\n}",
    "fn f()->dyn Show {\n defer { log(); }\n loop { return g(); }\n}",
    "impl Thing {\n fn m(self)->i64 {\n defer { log(); }\n if g() { return 1; }\n 2\n }\n}",
    "fn f[e]()->i64 eff [e] {\n defer { log(); }\n 2\n}",
    "fn f()->i64 {\n defer { log(); }\n let x = 1;\n defer { other(); }\n x\n}",
    "fn f()->i64 {\n defer { log(); }\n match g() { 1 => return 2, _ => 3 }\n}",
    "fn f()->Option[i64] {\n defer { log(); }\n let k = || -> Option[i64] { let y = g()?; Option::Some(y) };\n k()\n}",
    "fn f()->Option[i64] {\n defer { log(); }\n handle Fail[i64] in { let y = g()?; Option::Some(y) } with {return v=>v,raise(e,_)=>Option::None,}\n}",
    "fn f()->i64 {\n defer { log(); }\n return;\n}",
    "fn f()->i64 {\n defer { log() }\n while_true { 3 }\n}",
];

struct ScopeCase {
    render: Render,
    source: String,
    call: std::ops::Range<usize>,
}

fn vesper_scope(c: &ScopeCase) -> Vec<u64> {
    let r = &c.render;
    let mut text = String::new();
    let number = |text: &mut String, n: usize| {
        for k in 0..4 {
            text.push(char::from((n >> (7 * k)) as u8 & 127));
        }
    };
    number(&mut text, r.pattern.len());
    number(&mut text, r.invocation.len());
    number(&mut text, r.tail.as_ref().map_or(0, |t| t.len() + 1));
    number(&mut text, r.template.len());
    number(&mut text, MARK.len());
    number(&mut text, c.source.len());
    number(&mut text, c.call.start);
    number(&mut text, c.call.end);
    for part in [
        r.pattern.as_str(),
        &r.invocation,
        r.tail.as_deref().unwrap_or(""),
        &r.template,
        MARK,
        &c.source,
    ] {
        text.push_str(part);
    }
    let call = vesper_port_abi::HostCall::new()
        .str(&text)
        .word(r.max as u64)
        .word(1 << 20);
    // SAFETY: as `vesper_match`.
    unsafe { call.seq(JIT_RUNTIME, |w| sugar().scope.call(w).unwrap()) }
}

/// The source `program` under each scope rule's declaration, with a case
/// per statement or expression sugar use and rule.
fn scope_cases(program: &str, cases: &mut Vec<ScopeCase>) {
    for (k, (pattern, template)) in SCOPE_RULES.iter().enumerate() {
        let sort = if pattern.starts_with("holding") {
            "expr"
        } else {
            "stmt"
        };
        let name = pattern.split(' ').next().unwrap();
        let source =
            format!("sugar {name}:{sort} {{ rule ({pattern}) => {{{template}}} }}\n{program}");
        let Ok(parse) = parser::parse_surface_cst(&source, Default::default(), true) else {
            continue;
        };
        for node in parse.syntax().descendants() {
            if !matches!(node.kind(), parser::SUGAR_STMT | parser::SUGAR_EXPR) {
                continue;
            }
            let s = significant_start(&node);
            let e = usize::from(node.text_range().end());
            // `block_end`: the enclosing block's closing brace.
            let mut depth = 0usize;
            let mut end = None;
            for (i, b) in source.bytes().enumerate().skip(e) {
                if b == b'{' {
                    depth += 1;
                } else if b == b'}' {
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                    depth -= 1;
                }
            }
            let tail = pattern
                .contains("$..")
                .then(|| end.map(|end| source[e..end].to_owned()))
                .flatten();
            if pattern.contains("$..") && tail.is_none() {
                continue;
            }
            cases.push(ScopeCase {
                render: Render {
                    pattern: (*pattern).into(),
                    invocation: source[s..e].to_owned(),
                    tail,
                    template: (*template).into(),
                    max: match (k + cases.len()) % 4 {
                        0 => 200,
                        1 => 420,
                        _ => 1 << 20,
                    },
                },
                source: source.clone(),
                call: s..e,
            });
        }
    }
}

/// The cases that match, and those that lower.
fn check_scopes(cases: &[ScopeCase]) -> (usize, usize) {
    let mut failures = vec![];
    let (mut matched, mut ok) = (0, 0);
    let mut outcomes = std::collections::BTreeMap::<String, usize>::new();
    for c in cases {
        let r = &c.render;
        let rust = sugar_expand::template_scope_words(
            &r.pattern,
            &r.invocation,
            r.tail.as_deref(),
            &r.template,
            MARK,
            &c.source,
            c.call.clone(),
            r.max,
            1 << 20,
        );
        let vesper = vesper_scope(c);
        let outcome = match rust[0] {
            0 => "no match".to_owned(),
            1 if rust[1] != 0 => "lowered with a return effect".to_owned(),
            1 => "lowered".to_owned(),
            _ => rust[2..].iter().map(|&b| char::from(b as u8)).collect(),
        };
        *outcomes.entry(outcome).or_default() += 1;
        matched += usize::from(rust[0] != 0);
        ok += usize::from(rust[0] == 1);
        if rust != vesper {
            let text = |w: &[u64]| -> String {
                w.iter()
                    .map(|&b| if b < 128 { char::from(b as u8) } else { '?' })
                    .collect()
            };
            failures.push(format!(
                "source {:?} call {:?} template {:?} max {}:\n  rust   {:?}\n  vesper {:?}",
                c.source,
                c.call,
                r.template,
                r.max,
                text(&rust),
                text(&vesper)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} scope cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures[..failures.len().min(12)].join("\n")
    );
    println!("{outcomes:#?}");
    (matched, ok)
}

#[test]
fn scope_matches_on_the_scope_examples() {
    let mut cases = vec![];
    for program in SCOPE_PROGRAMS {
        scope_cases(program, &mut cases);
    }
    let (matched, ok) = check_scopes(&cases);
    assert!(
        matched > 60 && ok > 20,
        "{} cases, {matched} matched, {ok} ok",
        cases.len()
    );
}

/// Fragments the scope fuzzer splices into the callers.
const SCOPE_FRAGMENTS: &[&str] = &[
    "return 1;",
    "return;",
    "break;",
    "break 2;",
    "continue;",
    "g()?;",
    "x?",
    "loop {",
    "}",
    "{",
    "for i in xs {",
    "|| {",
    "|x: i64| -> i64 {",
    "handle Fail[i64] in {",
    "} with {return v=>v,raise(e,_)=>e,}",
    "region r {",
    "if a() {",
    "defer { log(); }",
    ";",
    "holding { 1 }",
    "eff [Fail[RuntimeError]]",
    "-> Option[i64]",
    "match x { _ => return 3 }",
];

#[test]
fn scope_matches_on_the_fuzzed_corpus() {
    let rounds: usize = std::env::var("VESPER_SUGAR_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4000)
        / 8;
    let mut rng = Rng(0x5eed_4b05);
    let mut cases = vec![];
    for _ in 0..rounds {
        let base = SCOPE_PROGRAMS[rng.below(SCOPE_PROGRAMS.len())];
        let mut parts: Vec<String> = base.split(' ').map(str::to_owned).collect();
        for _ in 0..=rng.below(3) {
            let at = rng.below(parts.len() + 1);
            let f = SCOPE_FRAGMENTS[rng.below(SCOPE_FRAGMENTS.len())].to_owned();
            match rng.below(3) {
                0 if !parts.is_empty() => {
                    parts.remove(at.min(parts.len() - 1));
                }
                1 if !parts.is_empty() => {
                    let k = at.min(parts.len() - 1);
                    parts[k] = f;
                }
                _ => parts.insert(at, f),
            }
        }
        scope_cases(&parts.join(" "), &mut cases);
    }
    check_scopes(&cases);
}

/// Pattern rules: literal binders, shorthand fields, binders spliced from
/// holes, repeated names and refusals.
const PATTERN_TEMPLATES: &[(&str, &str, &str)] = &[
    ("present $p:pat", "present value", "Option::Some($p)"),
    ("present $p:pat", "present (a, _)", "Option::Some($p)"),
    (
        "pair $a:pat $b:pat",
        "pair x y",
        "(x, $a, Point { x, y: $b })",
    ),
    ("pt $a:pat", "pt q", "Point { x, y: $a }"),
    ("pt $a:pat", "pt q", "Point { x: x, y }"),
    ("pt $a:pat", "pt q", "Point { x:\n  x, y: (y, y) }"),
    ("both $a:pat", "both z", "(w, w, Wrap { w }, $a)"),
    (
        "many $( $p:pat ),*",
        "many a, b, c",
        "($( Some($p) ),*, rest)",
    ),
    ("many $( $p:pat ),*", "many a, b", "[$( $p ),*]"),
    ("lit $e:expr", "lit 3", "Option::Some(x) | y"),
    ("lit $e:expr", "lit 3", "Num($e)"),
    ("ign {}", "ign {}", "shadow"),
    ("ign {}", "ign {}", "_"),
    ("ign {}", "ign {}", "(a, b) => b"),
    ("ign {}", "ign {}", "Ctor { a: Inner { b, c: d }, e }"),
];

fn pattern_renders() -> Vec<Render> {
    let mut renders = example_renders();
    for &(pattern, invocation, template) in PATTERN_TEMPLATES {
        for max in max_bytes() {
            renders.push(Render {
                pattern: pattern.into(),
                invocation: invocation.into(),
                tail: None,
                template: template.into(),
                max,
            });
        }
    }
    renders
}

#[test]
fn pattern_hygiene_matches_on_the_template_examples() {
    let (matched, ok) = check_renders(Stage::Pattern, &pattern_renders());
    assert!(matched > 100 && ok > 30, "{matched} matched, {ok} ok");
}

#[test]
fn pattern_hygiene_matches_on_the_sugar_test_suites() {
    check_renders(Stage::Pattern, &corpus_renders());
}

#[test]
fn pattern_hygiene_matches_on_the_fuzzed_corpus() {
    check_renders(Stage::Pattern, &fuzzed_renders(0x5eed_4b06));
}
