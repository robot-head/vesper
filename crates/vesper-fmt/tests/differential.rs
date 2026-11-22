//! The Vesper port of `format_source` against the frozen Rust seed
//! (self-hosting SH1 lane 11, #1154): every result, output or error, must be
//! the same. The corpus is every `.ves` file in the repository, the same
//! files with whitespace scrambled, edited copies from a fixed seed, and
//! sweeps of each limit across the point where it starts to refuse.
#![cfg(vesper_port)]
use std::path::{Path, PathBuf};
use vesper_fmt::{Limits, port_format_source};

fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    // Cargo: crates/vesper-fmt; Bazel runs the test from the execroot.
    if manifest.join("../../Cargo.toml").is_file() {
        manifest.join("../..")
    } else {
        PathBuf::from(".")
    }
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

fn under_bazel() -> bool {
    std::env::var_os("TEST_SRCDIR").is_some()
}

/// The printed result of the seed, the printed result of the port, or
/// `None` when the port hands the source to the Rust code.
fn both(source: &str, limits: Limits) -> (String, Option<String>) {
    let seed = match fmt_seed::format_source(source, seed_limits(limits)) {
        Ok(r) => format!("Ok(changed={}) {:?}", r.changed, r.output),
        Err(e) => format!("Err({e:?})"),
    };
    let describe = |r: Result<vesper_fmt::FormatResult, vesper_fmt::Error>| match r {
        Ok(r) => format!("Ok(changed={}) {:?}", r.changed, r.output),
        Err(e) => format!("Err({e:?})"),
    };
    let mut port = port_format_source(source, limits).map(describe);
    if let Some(raw) = &port
        && raw != &seed
        && has_surface_syntax(source)
    {
        // The standalone Vesper parser handles the executable grammar. Check
        // the actual port-backed entry point on the host surface profile.
        port = Some(describe(vesper_fmt::format_source(source, limits)));
    }
    (seed, port)
}

/// The two parsers intentionally differ on quoted operator delimiters, on
/// validation inside batch-skipped test blocks, and on unexpanded phrases.
fn has_surface_syntax(source: &str) -> bool {
    use lexer::TokenKind as K;
    use vesper_syntax::{lexer, parser};
    let Ok(lexed) = lexer::sugar_lex_with_limits(source, lexer::LexLimits::default()) else {
        return false;
    };
    let mut after_test = false;
    for t in lexed.tokens.iter().filter(|t| !t.kind.trivia()) {
        if t.kind == K::Backtick || (after_test && t.kind == K::LBrace) {
            return true;
        }
        after_test = t.kind == K::Lower && &source[t.span.clone()] == "test";
    }
    parser::parse_surface_with_limits(source, parser::ParseLimits::default()).is_ok_and(|p| {
        p.diagnostics.is_empty()
            && p.syntax()
                .descendants()
                .any(|n| parser::is_sugar_node(n.kind()))
    })
}

fn seed_limits(l: Limits) -> fmt_seed::Limits {
    fmt_seed::Limits {
        max_input_bytes: l.max_input_bytes,
        max_output_bytes: l.max_output_bytes,
        max_tokens: l.max_tokens,
        max_doc_nodes: l.max_doc_nodes,
        max_depth: l.max_depth,
        max_work: l.max_work,
    }
}

/// Compares one source; returns the difference, if any. `handed` counts
/// the sources the port gave back to the Rust code.
fn compare(source: &str, limits: Limits, handed: &mut usize) -> Option<String> {
    match both(source, limits) {
        (_, None) => {
            *handed += 1;
            None
        }
        (seed, Some(port)) if seed == port => None,
        (seed, Some(port)) => {
            let cut = |s: &str| s.chars().take(300).collect::<String>();
            Some(format!("seed {}\nport {}", cut(&seed), cut(&port)))
        }
    }
}

/// Whether the port hands this source to the Rust code: the macro profile
/// (`m!` after a name, or a quotation keyword), the only sources it does.
fn is_macro_source(source: &str) -> bool {
    use vesper_syntax::lexer::TokenKind as K;
    let (tokens, _) = vesper_syntax::lexer::lex(source);
    let mut previous = None;
    tokens.iter().any(|t| {
        if t.kind.trivia() {
            return false;
        }
        let text = &source[t.span.clone()];
        let opaque = (t.kind == K::Bang && matches!(previous, Some(K::Lower | K::Upper)))
            || matches!(text, "quote" | "quasiquote" | "syntax_quote");
        previous = Some(t.kind);
        opaque
    })
}

#[test]
fn repository_sources_format_byte_identically() {
    let files = repository_sources();
    let floor = if under_bazel() { 300 } else { 800 };
    assert!(files.len() >= floor, "{} sources", files.len());
    let mut failures = vec![];
    let mut handed = 0;
    let mut formatted = 0;
    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let name = path
            .strip_prefix(root())
            .unwrap_or(path)
            .display()
            .to_string();
        let before = handed;
        if let Some(d) = compare(&source, Limits::default(), &mut handed) {
            failures.push(format!("{name}: {d}"));
        }
        if handed > before && !is_macro_source(&source) {
            failures.push(format!("{name}: handed to Rust but not a macro source"));
        }
        // The canonical text of every file formats to itself on both builds.
        if let Ok(r) = fmt_seed::format_source(&source, fmt_seed::Limits::default()) {
            formatted += 1;
            let mut ignored = 0;
            if let Some(d) = compare(&r.output, Limits::default(), &mut ignored) {
                failures.push(format!("{name} (canonical): {d}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} sources differ:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
    eprintln!(
        "{} sources, {formatted} formatted, {handed} in the macro profile",
        files.len()
    );
}

/// The same files with every run of whitespace replaced, so the layout
/// decides every line break.
#[test]
fn scrambled_repository_sources_format_byte_identically() {
    let files = repository_sources();
    let mut failures = vec![];
    let mut handed = 0;
    for (i, path) in files.iter().enumerate() {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let (tokens, _) = vesper_syntax::lexer::lex(&source);
        let mut scrambled = String::with_capacity(source.len());
        for (j, t) in tokens.iter().enumerate() {
            let text = &source[t.span.clone()];
            if t.kind == vesper_syntax::lexer::TokenKind::Whitespace {
                // Keep line breaks after line comments; vary the rest.
                let lines = text.matches('\n').count();
                scrambled.push_str(match (lines, (i + j) % 3) {
                    (0, 0) => " ",
                    (0, _) => "  ",
                    (1, 0) => "\n",
                    (1, _) => " \n\t",
                    (_, 0) => "\n\n\n",
                    _ => "\n \n",
                });
            } else {
                scrambled.push_str(text);
            }
        }
        if let Some(d) = compare(&scrambled, Limits::default(), &mut handed) {
            failures.push(format!(
                "{}: {d}",
                path.strip_prefix(root()).unwrap_or(path).display()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const FRAGMENTS: &[&str] = &[
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    ",",
    ";",
    "\n",
    "\n\n",
    " ",
    "// c\n",
    "/* b */",
    "fn",
    "let",
    "x",
    "T",
    "1",
    "1.5",
    "\"s\"",
    "->",
    "=>",
    "::",
    ".",
    "..",
    "+",
    "-",
    "!",
    "@",
    "$",
    "?",
    "if",
    "else",
    "match",
    "eff",
    "[Fail[E]]",
    "quote",
    "m!",
    "é",
    "\r",
    "\t",
];

fn snap(source: &str, mut at: usize) -> usize {
    at = at.min(source.len());
    while !source.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn mutate(rng: &mut Rng, source: &str) -> String {
    let mut s = source.to_owned();
    for _ in 0..1 + rng.below(5) {
        let a = snap(&s, rng.below(s.len() + 1));
        let b = snap(&s, a + rng.below(24));
        match rng.below(4) {
            0 => s.replace_range(a..b, ""),
            1 => {
                let piece = s[a..b].to_owned();
                s.insert_str(b, &piece);
            }
            2 => s.truncate(b),
            _ => s.insert_str(a, FRAGMENTS[rng.below(FRAGMENTS.len())]),
        }
    }
    s
}

/// Edited repository sources from a fixed seed (`VESPER_FMT_FUZZ` sets the
/// count): errors as well as outputs must match.
#[test]
fn fuzzed_sources_give_the_same_result() {
    let files = repository_sources();
    let count = std::env::var("VESPER_FMT_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500);
    let sources: Vec<String> = files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter(|s| s.len() < 24 * 1024)
        .collect();
    let mut rng = Rng(0x5348_3131_666d_7400);
    let mut failures = vec![];
    let mut handed = 0;
    let mut errors = 0;
    for i in 0..count {
        let pick = rng.below(sources.len());
        let source = mutate(&mut rng, &sources[pick]);
        if let Some(d) = compare(&source, Limits::default(), &mut handed) {
            failures.push(format!("case {i}: {d}\n{source:?}"));
        }
        if fmt_seed::format_source(&source, fmt_seed::Limits::default()).is_err() {
            errors += 1;
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    eprintln!("{count} cases, {errors} refused, {handed} handed to Rust");
}

/// Every limit swept across the point where it starts to refuse: the port
/// fails with the same error at the same value.
#[test]
fn limit_sweeps_fail_at_the_same_point() {
    let sources = [
        "fn main( )->u64{let x=1;x}".to_owned(),
        include_str!("../../../examples/projections.ves").to_owned(),
        include_str!("../../../examples/traits/shapes.ves").to_owned(),
        "fn f(inout shard.{metrics}: WorkerShard, in raw: Slice[u64],) -> u32 { 0 }".to_owned(),
        format!(
            "fn call({})->u64{{0}}",
            (0..12)
                .map(|i| format!("in parameter_{i}: u64"))
                .collect::<Vec<_>>()
                .join(",")
        ),
    ];
    let base = Limits::default();
    let mut failures = vec![];
    let mut handed = 0;
    for source in &sources {
        let out = fmt_seed::format_source(source, fmt_seed::Limits::default())
            .unwrap()
            .output;
        let mut cases = vec![];
        // The work budget: every value around the smallest that admits the
        // source, and a spread below it.
        let mut low = 0usize;
        let mut high = 1usize << 31;
        while low + 1 < high {
            let mid = low + (high - low) / 2;
            if fmt_seed::format_source(
                source,
                seed_limits(Limits {
                    max_work: mid,
                    ..base
                }),
            )
            .is_ok()
            {
                high = mid;
            } else {
                low = mid;
            }
        }
        for w in (high.saturating_sub(40)..high + 40).chain((0..high).step_by(high / 97 + 1)) {
            cases.push(Limits {
                max_work: w,
                ..base
            });
        }
        for n in 0..out.len() + 8 {
            cases.push(Limits {
                max_output_bytes: n,
                ..base
            });
        }
        for n in (0..4000).step_by(7) {
            cases.push(Limits {
                max_doc_nodes: n,
                ..base
            });
        }
        for n in 0..400 {
            cases.push(Limits {
                max_tokens: n,
                ..base
            });
        }
        for n in 0..30 {
            cases.push(Limits {
                max_depth: n,
                ..base
            });
        }
        for n in 0..source.len() + 2 {
            cases.push(Limits {
                max_input_bytes: n,
                ..base
            });
        }
        for limits in cases {
            if let Some(d) = compare(source, limits, &mut handed) {
                failures.push(format!("{limits:?}: {d}"));
            }
        }
    }
    assert_eq!(handed, 0);
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Sources past the front end's default ceilings (65,536 tokens, 1 MiB),
/// which the port lexes with the hard ones as Rust does: the 131,072-token
/// cap refuses at the same point.
#[test]
fn large_sources_format_like_rust() {
    let item = |i: usize| format!("fn f{i}(x: u64) -> u64 {{\n    x + {i}\n}}\n\n");
    let mut failures = vec![];
    let mut handed = 0;
    // About 18 tokens per item: 80,000 tokens, then past the 131,072 cap.
    for items in [4_500, 7_500] {
        let source: String = (0..items).map(item).collect();
        if let Some(d) = compare(&source, Limits::default(), &mut handed) {
            failures.push(format!("{items} items: {d}"));
        }
    }
    // Over 1 MiB in one comment-heavy file.
    let long = format!(
        "// {}\nfn main() -> u64 {{\n    0\n}}\n",
        "c".repeat(1_100_000)
    );
    if let Some(d) = compare(&long, Limits::default(), &mut handed) {
        failures.push(format!("1.1 MB: {d}"));
    }
    assert_eq!(handed, 0);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
