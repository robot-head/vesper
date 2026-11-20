//! The Vesper port (`vesper_diagnostics`, this package) against the frozen
//! Rust seed (`diagnostics_seed`) on generated reports: every `prepare`
//! result, every terminal report (colors off and on) byte for byte, and
//! every LSP array (both outputs parsed with serde_json) must be the same,
//! errors included.
#![cfg(vesper_port)]
use diagnostics_seed as seed;
use std::sync::Arc;
use vesper_core::ast::Diagnostic;
use vesper_span::LineIndex;

thread_local! {
    /// Terminal reports both builds rendered, and their total bytes.
    static RENDERED: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
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
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const PIECES: &[&str] = &[
    "let",
    " ",
    "x",
    "=",
    "1;",
    "foo(bar)",
    "\t",
    "\t\t",
    "名前",
    "😀",
    "e\u{301}",
    "\r\n",
    "\n",
    "\n",
    "\r",
    "  ",
    "\u{3000}",
    "\u{a0}",
    "\u{200b}",
    "ｗｉｄｅ",
    "\u{7}",
    "\u{1f}",
    "\u{9f}",
    "é",
    "x\ty",
    "{",
    "}",
    "//",
    "\u{fe0f}",
    "가",
    "\u{2003}",
    "}\n",
];

const TEXTS: &[&str] = &[
    "",
    "m",
    "expected `;`",
    "two\nlines",
    "crlf\r\nhelp",
    "trailing\n",
    "bare\r",
    "\n",
    "tab\there",
    "名前 😀",
    "\u{1b}[1mraw escape\u{1b}[0m",
    "quote \" and \\ slash",
    "\u{0}\u{1}\u{8}\u{c}\u{1f}\u{7f}",
];

fn source(rng: &mut Rng) -> String {
    let mut s = String::new();
    let pieces = rng.below(40);
    for _ in 0..pieces {
        s.push_str(rng.pick(PIECES));
    }
    if rng.chance(20) {
        // A long line.
        for _ in 0..rng.below(120) {
            s.push_str(rng.pick(&["a", "b", " ", "名", "\t"]));
        }
    }
    s
}

/// The byte offsets a span may start or end at: char boundaries outside a
/// CRLF.
fn boundaries(s: &str) -> Vec<usize> {
    (0..=s.len())
        .filter(|&i| {
            s.is_char_boundary(i)
                && !(i > 0 && s.as_bytes()[i - 1] == b'\r' && s.as_bytes().get(i) == Some(&b'\n'))
        })
        .collect()
}

fn span(rng: &mut Rng, s: &str, valid: &[usize]) -> std::ops::Range<usize> {
    if rng.chance(2) {
        // Any offsets, including reversed, out of bounds and interior ones.
        let a = rng.below(s.len() + 3);
        let b = rng.below(s.len() + 3);
        return a..b;
    }
    let a = valid[rng.below(valid.len())];
    let b = if rng.chance(25) {
        a
    } else {
        let later: Vec<_> = valid.iter().copied().filter(|&b| b >= a).collect();
        // Mostly short spans, sometimes to anywhere later.
        if rng.chance(60) {
            later[rng.below(later.len().min(4))]
        } else {
            later[rng.below(later.len())]
        }
    };
    a..b
}

fn text(rng: &mut Rng) -> String {
    let mut t = String::from(rng.pick(TEXTS));
    if rng.chance(20) {
        t.push_str(rng.pick(TEXTS));
    }
    t
}

fn diagnostics(rng: &mut Rng, s: &str, valid: &[usize]) -> Vec<Diagnostic> {
    let count = if rng.chance(5) { 0 } else { 1 + rng.below(3) };
    (0..count)
        .map(|_| {
            let code = rng.pick(&["E", "E0300", "W1", "名", "😀", "\"", "E01"]);
            let message = if rng.chance(3) {
                String::new()
            } else {
                text(rng)
            };
            let message = if message.is_empty() && rng.chance(80) {
                "message".to_string()
            } else {
                message
            };
            let mut d = Diagnostic::new(code, message, span(rng, s, valid));
            for _ in 0..rng.below(7) {
                let message = text(rng);
                d = d.with_related(span(rng, s, valid), message);
            }
            if rng.chance(50) {
                d = d.with_help(text(rng));
            }
            d
        })
        .collect()
}

fn limits(rng: &mut Rng) -> seed::Limits {
    let mut l = seed::Limits::default();
    if rng.chance(15) {
        match rng.below(7) {
            0 => l.span.max_output_bytes = rng.below(3000),
            1 => l.span.max_work = rng.below(200_000),
            2 => l.span.max_labels = rng.below(6),
            3 => l.span.max_text_bytes = rng.below(60),
            4 => l.span.max_metadata_bytes = rng.below(4000),
            5 => l.span.max_lines = rng.below(8),
            _ => l.max_name_bytes = rng.below(12),
        }
    }
    l
}

fn port_limits(l: seed::Limits) -> vesper_diagnostics::Limits {
    vesper_diagnostics::Limits {
        span: l.span,
        max_name_bytes: l.max_name_bytes,
    }
}

fn error(e: seed::Error) -> String {
    format!("{e:?}")
}

/// Compares every output of one input, and returns whether `prepare`
/// accepted it.
fn compare(
    case: &str,
    source: &str,
    data: &[Diagnostic],
    limits: seed::Limits,
    names: &[&str],
) -> bool {
    let Ok(index) = LineIndex::new(Arc::from(source), vesper_span::Limits::default()) else {
        return false;
    };
    let rust = seed::prepare(&index, data, limits);
    let port = vesper_diagnostics::prepare(&index, data, port_limits(limits));
    let (rust, port) = match (rust, port) {
        (Ok(r), Ok(p)) => (r, p),
        (r, p) => {
            assert_eq!(
                r.map(|_| ()).map_err(error),
                p.map(|_| ()).map_err(|e| format!("{e:?}")),
                "{case}: prepare of {source:?} {data:?}"
            );
            return false;
        }
    };
    assert_eq!(rust.data(), port.data(), "{case}: data");
    for name in names {
        let r = rust.terminal(name);
        let p = port.terminal(name);
        match (&r, &p) {
            (Ok(r), Ok(p)) => assert!(
                {
                    RENDERED.with(|c| c.set((c.get().0 + 1, c.get().1 + r.len())));
                    r == p
                },
                "{case}: terminal of {source:?} {data:?}\n--- rust\n{}\n--- vesper\n{}",
                String::from_utf8_lossy(r),
                String::from_utf8_lossy(p)
            ),
            _ => assert_eq!(
                r.map_err(error),
                p.map_err(|e| format!("{e:?}")),
                "{case}: terminal of {source:?} {data:?}"
            ),
        }
        let r = rust.terminal_colored(name).map_err(error);
        let p = port.terminal_colored(name).map_err(|e| format!("{e:?}"));
        assert!(
            r == p,
            "{case}: colored terminal of {source:?} {data:?}\n{r:?}\n{p:?}"
        );
        let r = rust.lsp_json(name).map_err(error);
        let p = port.lsp_json(name).map_err(|e| format!("{e:?}"));
        match (&r, &p) {
            (Ok(r), Ok(p)) => {
                let r: serde_json::Value = serde_json::from_slice(r).unwrap();
                let p: serde_json::Value = serde_json::from_slice(p).unwrap();
                assert_eq!(r, p, "{case}: LSP of {source:?} {data:?}");
            }
            _ => assert_eq!(r, p, "{case}: LSP of {source:?} {data:?}"),
        }
        assert_eq!(
            rust.to_lsp(name).map_err(error),
            port.to_lsp(name).map_err(|e| format!("{e:?}")),
            "{case}: to_lsp"
        );
    }
    true
}

const NAMES: &[&str] = &[
    "main.vsp",
    "x",
    "名/😀.vsp",
    "untitled:a\"b\\c",
    "bad\nname",
    "",
];

#[test]
fn the_builds_differ() {
    assert_eq!(vesper_diagnostics::IMPLEMENTATION, "vesper");
    assert_eq!(seed::IMPLEMENTATION, "rust");
}

#[test]
fn generated_reports_render_identically() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut accepted = 0;
    let mut cases = 0;
    while accepted < 600 {
        let s = source(&mut rng);
        let valid = boundaries(&s);
        let data = diagnostics(&mut rng, &s, &valid);
        let l = limits(&mut rng);
        let names = [NAMES[rng.below(NAMES.len())], "main.vsp"];
        if compare(&format!("case {cases}"), &s, &data, l, &names) {
            accepted += 1;
        }
        cases += 1;
    }
    assert!(
        cases < 2000,
        "too few generated inputs pass prepare: {cases}"
    );
    let (reports, bytes) = RENDERED.with(|c| c.get());
    eprintln!(
        "{accepted} of {cases} generated inputs passed prepare; {reports} reports ({bytes} bytes) compared"
    );
    assert!(reports >= 500, "only {reports} generated reports rendered");
}

/// Many labels on one line and nested multi-line labels: the shapes where
/// ariadne's margin and arrow rules interact most.
#[test]
fn dense_and_nested_reports_render_identically() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for case in 0..80 {
        let lines = 1 + rng.below(6);
        let mut s = String::new();
        for _ in 0..lines {
            for _ in 0..rng.below(30) {
                s.push_str(rng.pick(&["a", "b", " ", "名", "\t", "😀", "e\u{301}"]));
            }
            s.push_str(rng.pick(&["\n", "\r\n", "\r"]));
        }
        let valid = boundaries(&s);
        let mut d = Diagnostic::new("E1", "dense", span(&mut rng, &s, &valid));
        for _ in 0..rng.below(24) {
            let a = valid[rng.below(valid.len())];
            let b = valid[rng.below(valid.len())];
            d = d.with_related(a.min(b)..a.max(b), text(&mut rng));
        }
        if rng.chance(50) {
            d = d.with_help(text(&mut rng));
        }
        compare(
            &format!("dense {case}"),
            &s,
            &[d],
            seed::Limits::default(),
            &["dense.vsp"],
        );
    }
}

/// The reports of the crate's own tests, by construction the same inputs.
#[test]
fn crate_test_inputs_render_identically() {
    let cases: Vec<(&str, Vec<Diagnostic>)> = vec![
        (
            "😀let 名 = 1;\r\nuse(名);\n",
            vec![
                Diagnostic::new("E0300", "already consumed", 22..25)
                    .with_related(8..11, "binding declared here")
                    .with_related(0..4, "actual earlier event")
                    .with_help("Consume exactly once."),
            ],
        ),
        (
            "a😀b",
            vec![
                Diagnostic::new("E1", "oops", 1..5)
                    .with_related(0..1, "origin")
                    .with_help("help"),
            ],
        ),
        (
            "😀e\u{301}名",
            vec![Diagnostic::new("E", "combining mark", 5..7).with_related(4..7, "base and mark")],
        ),
        (
            "",
            vec![
                Diagnostic::new("E", "primary", 0..0)
                    .with_related(0..0, "actual EOF cause")
                    .with_help("EOF help"),
            ],
        ),
        (
            "x\r\n",
            vec![
                Diagnostic::new("E", "primary", 3..3)
                    .with_related(3..3, "actual EOF cause")
                    .with_help("EOF help"),
            ],
        ),
        ("a\n\n", vec![Diagnostic::new("E0", "expected token", 3..3)]),
        ("a\r", vec![Diagnostic::new("E0", "expected token", 2..2)]),
        ("abc", vec![Diagnostic::new("E1", "message", 0..1)]),
    ];
    for (i, (s, d)) in cases.iter().enumerate() {
        assert!(
            compare(
                &format!("crate case {i}"),
                s,
                d,
                seed::Limits::default(),
                &["unicode.vsp"]
            ),
            "crate case {i} must prepare"
        );
    }
}
