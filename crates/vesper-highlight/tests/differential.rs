//! The Vesper port of Tier 0 (`vesper_highlight` with `vesper-port`)
//! against the frozen Rust seed (self-hosting SH1 lane 11, #1154): in both
//! modes every span, class and flag must be the same. The corpus is every
//! `.ves` file in the repository, every prefix of a sample, the chroma test
//! snippets' shapes, and edited copies of repository files from a fixed
//! seed.
#![cfg(vesper_port)]
use std::path::{Path, PathBuf};

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

type Flat = Vec<(usize, usize, u16, u16)>;

fn seed(src: &str, tokens: bool) -> Result<Flat, String> {
    let r = if tokens {
        highlight_seed::highlight_tier0_tokens(src)
    } else {
        highlight_seed::highlight_tier0(src)
    };
    r.map(|s| {
        s.iter()
            .map(|p| (p.start, p.end, p.class as u16, p.flags))
            .collect()
    })
    .map_err(|e| format!("{e:?}"))
}

/// The difference between the builds on one source, if any. `refused`
/// counts the sources the port hands to the Rust classifier.
fn compare(src: &str, refused: &mut usize) -> Option<String> {
    for tokens in [true, false] {
        let Some(port) = vesper_highlight::port_tier0(src, tokens) else {
            *refused += 1;
            continue;
        };
        let port: Flat = port
            .iter()
            .map(|p| (p.start, p.end, p.class as u16, p.flags))
            .collect();
        if vesper_highlight::verify_lossless_spans(
            src.len(),
            &vesper_highlight::port_tier0(src, tokens).unwrap(),
        )
        .is_err()
        {
            return Some(format!("port spans are not lossless (tokens={tokens})"));
        }
        match seed(src, tokens) {
            Ok(rust) if rust == port => {}
            Ok(rust) => {
                let at = rust
                    .iter()
                    .zip(&port)
                    .position(|(a, b)| a != b)
                    .unwrap_or(rust.len().min(port.len()));
                return Some(format!(
                    "tokens={tokens}: first difference at span {at}: rust {:?} port {:?} ({} vs {} spans)",
                    rust.get(at),
                    port.get(at),
                    rust.len(),
                    port.len()
                ));
            }
            Err(e) => {
                return Some(format!(
                    "tokens={tokens}: rust failed with {e}, the port did not"
                ));
            }
        }
    }
    None
}

#[test]
fn repository_sources_classify_identically() {
    let files = repository_sources();
    let floor = if under_bazel() { 300 } else { 800 };
    assert!(files.len() >= floor, "{} sources", files.len());
    let mut failures = vec![];
    let mut refused = 0;
    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        if let Some(d) = compare(&source, &mut refused) {
            failures.push(format!(
                "{}: {d}",
                path.strip_prefix(root()).unwrap_or(path).display()
            ));
        }
    }
    assert_eq!(refused, 0, "the port refused repository sources");
    assert!(
        failures.is_empty(),
        "{} of {} sources differ:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}

const SNIPPETS: &[&str] = &[
    "fn f() -> u64 { ~1 }",
    "fn f() { x |= 1; y &= 2; }",
    "fn # x",
    "linear effect Storage { fn begin_txn(in n: u64) -> WriteTxn; }\nwire struct Header { len: u32be, tag: u16le }",
    "fn append(inout broker.{index, metrics.requests}: Broker, in config: Config, sink batch: Batch) -> u64 eff [Storage, Fail[IoError]] { 0 }",
    "/* block /* nested */ comment */ @proof fn f() -> mod linear Unit { \"str\\\"ing\" }",
    "@fip fn g() {} tiled(4) struct S { hot a: u64, cold b: u64 } fn h(x: [u8; 4]) -> (u32, i64) { 0 }",
    "fn f() { let mut i = 0; loop { break; } for j in 0..3 { continue; } let loop = 1; }",
    "impl Show for Point { } use @scope/name::{a, b}; pub trait T { type A; } model M for T {}",
    "handle Counter in { 1 } with { return (v) => v, } effect E { } linear effect L {} fn f() eff [E, L, Fail[X], Cancel] {}",
    "fn f() eff [A, B: C, D[E, F]] { G }",
    "\"unterminated",
    "/* unterminated",
    "",
    "   \n\t",
    "fn f(x: Slice[u8], y: ChunkSeq[f64]) -> Option[Result[u8, E]] { x as u64 }",
];

#[test]
fn snippets_and_every_prefix_classify_identically() {
    let mut failures = vec![];
    let mut refused = 0;
    for snippet in SNIPPETS {
        for end in 0..=snippet.len() {
            if !snippet.is_char_boundary(end) {
                continue;
            }
            let src = &snippet[..end];
            if let Some(d) = compare(src, &mut refused) {
                failures.push(format!("{src:?}: {d}"));
            }
        }
    }
    let sample = include_str!("../../../examples/projections.ves");
    for end in (0..=sample.len()).step_by(7) {
        if !sample.is_char_boundary(end) {
            continue;
        }
        if let Some(d) = compare(&sample[..end], &mut refused) {
            failures.push(format!("projections.ves[..{end}]: {d}"));
        }
    }
    assert_eq!(refused, 0);
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

/// Edited repository sources from a fixed seed (`VESPER_HIGHLIGHT_FUZZ`
/// sets the count).
#[test]
fn fuzzed_sources_classify_identically() {
    let files = repository_sources();
    let count = std::env::var("VESPER_HIGHLIGHT_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500);
    let sources: Vec<String> = files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter(|s| s.len() < 24 * 1024)
        .collect();
    let mut rng = Rng(0x5348_3131_6869_6768);
    let mut failures = vec![];
    let mut refused = 0;
    for i in 0..count {
        let pick = rng.below(sources.len());
        let source = mutate(&mut rng, &sources[pick]);
        if let Some(d) = compare(&source, &mut refused) {
            failures.push(format!("case {i}: {d}\n{source:?}"));
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
}

/// Above the front end's default ceilings (65,536 tokens, 128
/// diagnostics) the port lexes with the hard ones, as Rust does; past a
/// hard ceiling the lexer stops early, the port refuses and the public API
/// gives the Rust result.
#[test]
fn sources_past_the_front_end_ceilings_are_classified_by_rust() {
    let flat = |spans: Vec<vesper_highlight::HighlightSpan>| -> Flat {
        spans
            .iter()
            .map(|p| (p.start, p.end, p.class as u16, p.flags))
            .collect()
    };
    let many = "x ".repeat(40_000);
    let errors = "# ".repeat(200);
    for src in [many.as_str(), errors.as_str()] {
        let port = vesper_highlight::port_tier0(src, true).map(flat);
        assert_eq!(port.map(Ok), Some(seed(src, true)));
    }
    let too_many = "x ".repeat(140_000);
    let too_many_errors = "# ".repeat(1_100);
    for src in [too_many.as_str(), too_many_errors.as_str()] {
        assert!(vesper_highlight::port_tier0(src, true).is_none());
        let public = flat(vesper_highlight::highlight_tier0_tokens(src).unwrap());
        assert_eq!(Ok(public), seed(src, true));
    }
    // CST mode keeps the parser's default 128 diagnostics.
    assert!(vesper_highlight::port_tier0(&errors, false).is_none());
    let public = flat(vesper_highlight::highlight_tier0(&errors).unwrap());
    assert_eq!(Ok(public), seed(&errors, false));
}
