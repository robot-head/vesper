//! Self-hosting SH5 lane 17 (#1340), part 1: navigation in Vesper.
//! `@vesper/lsp-features` (packages/vesper-lsp-features) against
//! src/lsp/navigation.rs and the live `vesper lsp` server.
//!
//! - `navigation_matches_rust`: `document_symbols` (both shapes),
//!   `definition` at every token boundary and inside every token, invalid
//!   cursors, and `workspace_symbols` over windows of open documents and
//!   several queries, on every corpus program the editor can check and on
//!   generated sources that reach the navigation limits. Each answer is the
//!   JSON-RPC result or error object the session sends, as text.
//! - `sessions_match_server`: recorded `vesper lsp` sessions (the
//!   navigation scenario of tests/lsp_cli.rs, both symbol shapes, editing
//!   sessions over examples, and workspace queries over several open
//!   documents). Every navigation response the server wrote is compared,
//!   byte for byte after `serde_json` normalization, with the Vesper
//!   answer computed from the same document text.
//!
//! A definition request whose cursor is inside a `///` or `//!` comment
//! with no lexical target goes to the documentation-link fallback
//! (src/lsp/navigation/doc_links.rs), which this part does not port. The
//! Vesper entry reports it as deferred (`D`); the tests count those cases
//! and compare all others.

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use vesper::lsp::navigation::{self, OpenDocument};
use vesper::tooling::frontend::{AnalysisFailure, AnalysisLimits, CheckedSource, check_source};
use vesper_lsp::{ContentChange, DocumentStore, StoreLimits};
use vesper_span::{LineIndex, Position};

fn features() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-lsp-features"))
}

fn optimize() -> bool {
    std::env::var_os("LSP_FEATURES_NO_OPT").is_none()
}

fn entry(name: &'static str) -> &'static Entry {
    static ENTRIES: OnceLock<BTreeMap<&'static str, Entry>> = OnceLock::new();
    let entries = ENTRIES.get_or_init(|| {
        [
            "lsp_document_symbols",
            "lsp_definition",
            "lsp_workspace_symbols",
            "lsp_definitions",
        ]
        .into_iter()
        .map(|n| (n, features().entry(n, optimize())))
        .collect()
    });
    &entries[name]
}

// ------------------------------------------------------------- inputs

fn push_word(out: &mut Vec<u8>, w: u64) {
    out.extend_from_slice(&w.to_le_bytes());
}

/// `vesper_span::Limits` as the ten words of `read_index` (lib.ves).
fn index_words(out: &mut Vec<u8>, l: vesper_span::Limits) {
    for w in [
        l.max_source_bytes,
        l.max_lines,
        l.max_metadata_bytes,
        l.max_work,
        l.max_edits,
        l.max_output_bytes,
        l.max_diagnostics,
        l.max_labels,
        l.max_fixes,
        l.max_text_bytes,
    ] {
        push_word(out, w as u64);
    }
}

fn index_limits() -> vesper_span::Limits {
    StoreLimits::default().index_limits
}

fn vesper_symbols(uri: &str, source: &str, hierarchical: bool) -> String {
    let mut input = vec![];
    index_words(&mut input, index_limits());
    push_word(&mut input, hierarchical as u64);
    push_word(&mut input, uri.len() as u64);
    push_word(&mut input, source.len() as u64);
    input.extend_from_slice(uri.as_bytes());
    input.extend_from_slice(source.as_bytes());
    entry("lsp_document_symbols").text(&input)
}

fn vesper_definition(uri: &str, source: &str, at: Position) -> String {
    let mut input = vec![];
    index_words(&mut input, index_limits());
    push_word(&mut input, at.line as u64);
    push_word(&mut input, at.character as u64);
    push_word(&mut input, uri.len() as u64);
    push_word(&mut input, source.len() as u64);
    input.extend_from_slice(uri.as_bytes());
    input.extend_from_slice(source.as_bytes());
    entry("lsp_definition").text(&input)
}

/// `lsp_definitions`: one parse, an index rebuilt per cursor.
fn vesper_definitions(uri: &str, source: &str, cursors: &[Position]) -> Vec<String> {
    let mut input = vec![];
    index_words(&mut input, index_limits());
    push_word(&mut input, uri.len() as u64);
    push_word(&mut input, source.len() as u64);
    push_word(&mut input, cursors.len() as u64);
    for p in cursors {
        push_word(&mut input, p.line as u64);
        push_word(&mut input, p.character as u64);
    }
    input.extend_from_slice(uri.as_bytes());
    input.extend_from_slice(source.as_bytes());
    let text = entry("lsp_definitions").text(&input);
    if cursors.is_empty() {
        return vec![];
    }
    text.split('\n').map(str::to_string).collect()
}

fn vesper_workspace(docs: &[(&str, &str)], query: &str) -> String {
    let mut input = vec![];
    index_words(&mut input, index_limits());
    push_word(&mut input, query.len() as u64);
    push_word(&mut input, docs.len() as u64);
    for (uri, source) in docs {
        push_word(&mut input, uri.len() as u64);
        push_word(&mut input, source.len() as u64);
    }
    input.extend_from_slice(query.as_bytes());
    for (uri, source) in docs {
        input.extend_from_slice(uri.as_bytes());
        input.extend_from_slice(source.as_bytes());
    }
    entry("lsp_workspace_symbols").text(&input)
}

// --------------------------------------------------------------- Rust

/// session.rs `nav_error`, as the error object the session sends.
fn nav_error(error: navigation::Error) -> Value {
    match error {
        navigation::Error::InvalidPosition(reason) => json!({"code":-32602,"message":reason}),
        navigation::Error::SourceMismatch
        | navigation::Error::InvalidSpan
        | navigation::Error::InvalidLimits => {
            json!({"code":-32603,"message":error.to_string()})
        }
        navigation::Error::Resource(_) => json!({"code":-32003,"message":error.to_string(),
            "data":{"status":"analysis_unavailable","kind":"resource"}}),
    }
}

fn answer(result: Result<Value, navigation::Error>) -> String {
    match result {
        Ok(v) => format!("R{}", serde_json::to_string(&v).unwrap()),
        Err(e) => format!("E{}", serde_json::to_string(&nav_error(e)).unwrap()),
    }
}

/// The session's `textDocument/documentSymbol` result.
fn rust_symbols(uri: &str, s: &CheckedSource, index: &LineIndex, hierarchical: bool) -> String {
    answer(
        navigation::document_symbols(s, index, navigation::Limits::default()).map(|symbols| {
            Value::Array(if hierarchical {
                symbols
            } else {
                symbols
                    .into_iter()
                    .map(|symbol| {
                        json!({"name":symbol["name"],"kind":symbol["kind"],
                            "location":{"uri":uri,"range":symbol["range"]}})
                    })
                    .collect()
            })
        }),
    )
}

fn rust_definition(uri: &str, s: &CheckedSource, index: &LineIndex, at: Position) -> String {
    answer(
        navigation::definition(uri, s, index, at, navigation::Limits::default())
            .map(|v| v.unwrap_or(Value::Null)),
    )
}

fn rust_workspace(docs: &[OpenDocument<'_>], query: &str) -> String {
    answer(
        navigation::workspace_symbols(docs, query, navigation::Limits::default()).map(Value::Array),
    )
}

/// lens_requests.rs `lens_limits`: the raised profile `Session::snapshot`
/// falls back to when the editor profile refuses a buffer for its budgets.
fn lens_limits() -> AnalysisLimits {
    AnalysisLimits {
        max_ast_nodes: 8192,
        max_type_nodes: 16_384,
        max_depth: 64,
        max_diagnostics: 256,
        max_diagnostic_bytes: 262_144,
        max_metadata_bytes: 2_097_152,
        ..AnalysisLimits::default()
    }
}

fn hard_limits() -> AnalysisLimits {
    AnalysisLimits {
        max_source_bytes: 262_144,
        max_ast_nodes: 8192,
        max_type_nodes: 32_768,
        max_depth: 64,
        max_identifier_bytes: 1024,
        max_printed_type_bytes: 8192,
        max_diagnostics: 256,
        max_diagnostic_bytes: 262_144,
        max_metadata_bytes: 2_097_152,
    }
}

/// `Session::snapshot`: the editor profile, then the raised one for a
/// resource refusal (with the editor's diagnostic budgets).
fn snapshot(source: &str) -> Option<Arc<CheckedSource>> {
    match check_source(source, AnalysisLimits::default()) {
        Ok(s) => Some(Arc::new(s)),
        Err(AnalysisFailure::Resource { .. }) => {
            let editor = AnalysisLimits::default();
            check_source(
                source,
                AnalysisLimits {
                    max_diagnostics: editor.max_diagnostics,
                    max_diagnostic_bytes: editor.max_diagnostic_bytes,
                    ..lens_limits()
                },
            )
            .ok()
            .map(Arc::new)
        }
        Err(_) => None,
    }
}

// ------------------------------------------------------------- corpus

/// A checked source with its line index.
struct Doc {
    name: String,
    uri: String,
    index: LineIndex,
    checked: Arc<CheckedSource>,
}

fn doc(name: &str, uri: &str, source: &str, hard: bool) -> Option<Doc> {
    let index = LineIndex::new(Arc::from(source), index_limits()).ok()?;
    let checked = if hard {
        // The largest profile `check_source` accepts: navigation is a pure
        // function of the checked source, and these reach its own limits.
        match check_source(source, hard_limits()) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                eprintln!("generated {name} does not check: {e}");
                return None;
            }
        }
    } else {
        snapshot(source)?
    };
    Some(Doc {
        name: name.into(),
        uri: uri.into(),
        index,
        checked,
    })
}

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|e| e == "ves") {
            out.push(p);
        }
    }
}

/// Generated sources at the navigation limits and with the shapes the
/// index treats specially.
fn generated() -> Vec<(String, String)> {
    let mut out = vec![];
    // Long names: three of these overflow `max_output_bytes` (256 KiB)
    // together in one workspace answer.
    for k in 0..3 {
        let mut s = String::new();
        for i in 0..110 {
            s.push_str(&format!(
                "fn {}{k}_{i}() -> u64 {{ {i} }}\n",
                "n".repeat(1000)
            ));
        }
        out.push((format!("long_names_{k}"), s));
    }
    // Nested blocks (the traversal depth is 64; the editor's parser stops
    // well before it).
    for depth in [4, 8] {
        let mut blocks = String::from("x");
        for _ in 0..depth {
            blocks = format!("{{ {blocks} }}");
        }
        out.push((
            format!("blocks_{depth}"),
            format!("fn g(x: u64) -> u64 {blocks}\n"),
        ));
    }
    // Shadowing, closures, matches, loops, handlers, effect receivers,
    // impls, traits, effects and conformance models.
    out.push((
        "scopes".into(),
        r#"/// Adds one.
/// See [`twice`].
fn inc(x: u64) -> u64 { wrapping_add(x, 1) }
//! module note
fn twice(x: u64) -> u64 {
    let y = inc(x);
    let x = inc(y);
    x
}
copy struct Pair { a: u64, b: u64 }
copy enum Shape { Dot, Line(u64) }
fn area(s: Shape) -> u64 {
    match s { Shape::Dot => 0, Shape::Line(n) => twice(n) }
}
fn sum(n: u64) -> u64 {
    let mut t = 0;
    for i in 0..n { t = wrapping_add(t, i); }
    let p = Pair { a: t, b: n };
    wrapping_add(p.a, p.b)
}
effect Log { fn note(x: u64) -> Unit; }
fn noisy(x: u64) -> u64 eff [Log] { Log::note(x); x }
fn quiet(x: u64) -> u64 {
    handle Log in { noisy(x) } with {
        return v => v,
        note(y, k) => k(()),
    }
}
impl Pair { fn first(self) -> u64 { self.a } }
fn main() -> u64 { wrapping_add(sum(3), wrapping_add(area(Shape::Dot), quiet(1))) }
"#
        .into(),
    ));
    out.push((
        "unicode".into(),
        "// 😀 ünïcödé 𝄞\r\nfn first()->u64{1}\r\nfn main()->u64{first()}\r\n".into(),
    ));
    out
}

fn corpus_docs() -> &'static [Doc] {
    static DOCS: OnceLock<Vec<Doc>> = OnceLock::new();
    DOCS.get_or_init(|| {
        let mut files = vec![];
        for d in ["examples", "tests/fixtures", "src/lsp/fixtures"] {
            walk(&repo().join(d), &mut files);
        }
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let mut docs = vec![];
        for f in files {
            let Ok(source) = std::fs::read_to_string(&f) else {
                continue;
            };
            if source.len() > 131_072 {
                continue;
            }
            let name = f
                .strip_prefix(repo())
                .unwrap_or(&f)
                .display()
                .to_string()
                .replace('\\', "/");
            let uri = format!("file:///repo/{name}");
            if let Ok(Some(d)) = std::panic::catch_unwind(|| doc(&name, &uri, &source, false)) {
                docs.push(d);
            }
        }
        for (name, source) in generated() {
            let uri = format!("untitled:{name}");
            match std::panic::catch_unwind(|| doc(&name, &uri, &source, true)) {
                Ok(Some(d)) => docs.push(d),
                Ok(None) => {}
                Err(_) => eprintln!("generated {name} panicked"),
            }
        }
        std::panic::set_hook(quiet);
        docs
    })
}

/// Cursor positions: every token start, the middle of every token, the
/// end of every line, and invalid ones (past the last line, past a line's
/// end, inside a surrogate pair).
fn positions(d: &Doc) -> Vec<Position> {
    let source = d.checked.source();
    let mut offsets = vec![];
    if let Ok(lexed) = vesper::lexer::lex_with_limits(
        source,
        vesper::lexer::LexLimits {
            max_source_bytes: 1 << 20,
            max_tokens: 1 << 18,
            max_diagnostics: 1,
        },
    ) {
        for t in &lexed.tokens {
            offsets.push(t.span.start);
            let mid = t.span.start + t.span.len() / 2;
            if source.is_char_boundary(mid) {
                offsets.push(mid);
            }
        }
    }
    offsets.push(source.len());
    offsets.sort();
    offsets.dedup();
    let stride = (offsets.len() / 600).max(1);
    let mut out: Vec<Position> = offsets
        .into_iter()
        .step_by(stride)
        .filter_map(|o| d.index.position(o).ok())
        .collect();
    let lines = source.split('\n').count() as u32;
    out.push(Position {
        line: lines + 3,
        character: 0,
    });
    out.push(Position {
        line: 0,
        character: 100_000,
    });
    if let Some(at) = source.find('𝄞').or_else(|| source.find('😀'))
        && let Ok(p) = d.index.position(at)
    {
        out.push(Position {
            line: p.line,
            character: p.character + 1,
        });
    }
    out
}

/// The start and the middle of every identifier token.
fn name_positions(d: &Doc) -> Vec<Position> {
    let source = d.checked.source();
    let Ok(lexed) = vesper::lexer::lex_with_limits(
        source,
        vesper::lexer::LexLimits {
            max_source_bytes: 1 << 20,
            max_tokens: 1 << 18,
            max_diagnostics: 1,
        },
    ) else {
        return vec![];
    };
    lexed
        .tokens
        .iter()
        .filter(|t| {
            matches!(
                t.kind,
                vesper::lexer::TokenKind::Lower | vesper::lexer::TokenKind::Upper
            )
        })
        .flat_map(|t| [t.span.start, t.span.start + t.span.len() / 2])
        .filter_map(|o| d.index.position(o).ok())
        .collect()
}

const QUERIES: [&str; 6] = ["", "main", "f", "x", "new", "zzz_no_match"];

#[derive(Default)]
struct Counts {
    symbols: usize,
    definitions: usize,
    targets: usize,
    deferred: usize,
    workspace: usize,
    errors: BTreeMap<String, usize>,
}

impl Counts {
    fn note(&mut self, answer: &str) {
        if let Some(e) = answer.strip_prefix('E') {
            let v: Value = serde_json::from_str(e).unwrap();
            *self
                .errors
                .entry(v["message"].as_str().unwrap_or("").to_string())
                .or_default() += 1;
        }
    }
}

fn compare(
    differ: &mut Vec<String>,
    counts: &mut Counts,
    what: String,
    rust: String,
    vesper: String,
) {
    counts.note(&rust);
    if rust != vesper && differ.len() < 20 {
        differ.push(format!(
            "{what}\n  rust   {rust:.400}\n  vesper {vesper:.400}"
        ));
    }
}

#[test]
fn navigation_matches_rust() {
    let docs = corpus_docs();
    assert!(docs.len() >= 40, "only {} corpus documents", docs.len());
    let mut differ = vec![];
    let mut c = Counts::default();
    for d in docs {
        for hierarchical in [true, false] {
            c.symbols += 1;
            compare(
                &mut differ,
                &mut c,
                format!("{} symbols hierarchical={hierarchical}", d.name),
                rust_symbols(&d.uri, &d.checked, &d.index, hierarchical),
                vesper_symbols(&d.uri, d.checked.source(), hierarchical),
            );
        }
        let cursors = positions(d);
        let batch = vesper_definitions(&d.uri, d.checked.source(), &cursors);
        assert_eq!(batch.len(), cursors.len(), "{}", d.name);
        for (k, at) in cursors.into_iter().enumerate() {
            let rust = rust_definition(&d.uri, &d.checked, &d.index, at);
            let vesper = vesper_definition(&d.uri, d.checked.source(), at);
            if batch[k] != vesper && differ.len() < 20 {
                differ.push(format!(
                    "{} definition {}:{}: batch {} single {}",
                    d.name, at.line, at.character, batch[k], vesper
                ));
            }
            c.definitions += 1;
            if vesper == "D" {
                // The doc-link fallback: Rust answers from vesper_doc::links.
                c.deferred += 1;
                continue;
            }
            if rust.starts_with("R{") {
                c.targets += 1;
            }
            compare(
                &mut differ,
                &mut c,
                format!("{} definition {}:{}", d.name, at.line, at.character),
                rust,
                vesper,
            );
        }
    }
    // URIs the encoder must escape, and the URI limit.
    let first = &docs[0];
    for uri in [
        "",
        "untitled:a\"b\\c",
        "untitled:\u{1}\t\n\u{7f}é😀",
        &"u".repeat(4096),
        &"u".repeat(4097),
    ] {
        let at = Position {
            line: 0,
            character: 0,
        };
        compare(
            &mut differ,
            &mut c,
            format!("uri {uri:.20?} definition"),
            rust_definition(uri, &first.checked, &first.index, at),
            vesper_definition(uri, first.checked.source(), at),
        );
        compare(
            &mut differ,
            &mut c,
            format!("uri {uri:.20?} legacy symbols"),
            rust_symbols(uri, &first.checked, &first.index, false),
            vesper_symbols(uri, first.checked.source(), false),
        );
        let open = [OpenDocument {
            uri,
            index: &first.index,
            snapshot: &first.checked,
        }];
        compare(
            &mut differ,
            &mut c,
            format!("uri {uri:.20?} workspace"),
            rust_workspace(&open, ""),
            vesper_workspace(&[(uri, first.checked.source())], ""),
        );
    }
    // Workspace queries over windows of open documents, up to and past
    // `max_documents` (32), and queries past `max_query_bytes` (256).
    let mut windows: Vec<&[Doc]> = docs.chunks(8).collect();
    windows.push(&docs[..docs.len().min(32)]);
    windows.push(&docs[..docs.len().min(33)]);
    windows.push(&docs[..docs.len().min(48)]);
    let long: Vec<&Doc> = docs
        .iter()
        .filter(|d| d.name.starts_with("long_names_"))
        .collect();
    assert_eq!(long.len(), 3, "the long-name documents check");
    let long_window: Vec<Doc> = long
        .iter()
        .map(|d| Doc {
            name: d.name.clone(),
            uri: d.uri.clone(),
            index: LineIndex::new(Arc::from(d.checked.source()), index_limits()).unwrap(),
            checked: Arc::clone(&d.checked),
        })
        .collect();
    windows.push(&long_window);
    let long = "q".repeat(257);
    let edge = "q".repeat(256);
    for w in windows {
        let open: Vec<_> = w
            .iter()
            .map(|d| OpenDocument {
                uri: &d.uri,
                index: &d.index,
                snapshot: &d.checked,
            })
            .collect();
        let texts: Vec<(&str, &str)> = w
            .iter()
            .map(|d| (d.uri.as_str(), d.checked.source()))
            .collect();
        for q in QUERIES
            .iter()
            .copied()
            .chain([long.as_str(), edge.as_str()])
        {
            c.workspace += 1;
            compare(
                &mut differ,
                &mut c,
                format!("workspace {:?}.. ({}) {q:.12?}", w[0].name, w.len()),
                rust_workspace(&open, q),
                vesper_workspace(&texts, q),
            );
        }
    }
    eprintln!(
        "navigation: {} documents, {} symbol answers, {} definitions ({} targets, {} doc-link \
         fallbacks deferred), {} workspace queries; errors {:?}; {} differ",
        docs.len(),
        c.symbols,
        c.definitions,
        c.targets,
        c.deferred,
        c.workspace,
        c.errors,
        differ.len()
    );
    assert!(differ.is_empty(), "{}", differ.join("\n"));
    assert!(c.targets > 1000, "only {} definition targets", c.targets);
}

// ----------------------------------------------------------- sessions

fn frame(body: &[u8]) -> Vec<u8> {
    let mut v = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    v.extend(body);
    v
}

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

fn notice(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}

fn initialize(hierarchical: bool) -> Value {
    request(
        1,
        "initialize",
        json!({"capabilities":{"general":{"positionEncodings":["utf-16"]},"textDocument":{"documentSymbol":{"hierarchicalDocumentSymbolSupport":hierarchical}}}}),
    )
}

fn text_doc(uri: &str) -> Value {
    json!({"textDocument":{"uri":uri}})
}

fn at(uri: &str, line: u32, character: u32) -> Value {
    json!({"textDocument":{"uri":uri},"position":{"line":line,"character":character}})
}

fn open(uri: &str, version: i32, text: &str) -> Value {
    notice(
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"languageId":"vesper","version":version,"text":text}}),
    )
}

/// Runs the actual `vesper lsp` server on `input` and returns its stdout.
fn serve(input: &[u8]) -> Vec<u8> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let mut out = vec![];
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    writer.join().unwrap();
    child.wait().unwrap();
    out
}

/// The session scripts: client messages, in order.
fn scripts() -> Vec<(String, Vec<Value>)> {
    let mut out = vec![];
    // tests/lsp_cli.rs's unicode navigation scenario, in both shapes.
    for hierarchical in [true, false] {
        let uri = "untitled:stdio/🦀";
        let source = "// 😀\r\nfn first()->u64{1}\r\nfn main()->u64{first()}\r\n";
        out.push((
            format!("unicode_navigation_{hierarchical}"),
            vec![
                initialize(hierarchical),
                notice("initialized", json!({})),
                open(uri, 1, source),
                request(2, "textDocument/documentSymbol", text_doc(uri)),
                request(4, "textDocument/definition", at(uri, 2, 15)),
                notice(
                    "textDocument/didChange",
                    json!({"textDocument":{"uri":uri,"version":2},"contentChanges":[{"range":{"start":{"line":1,"character":16},"end":{"line":1,"character":17}},"rangeLength":1,"text":"\r\n1\r\n"}]}),
                ),
                request(5, "textDocument/documentSymbol", text_doc(uri)),
                request(6, "textDocument/definition", at(uri, 4, 15)),
                request(7, "textDocument/definition", at(uri, 40, 0)),
                request(8, "textDocument/definition", at(uri, 0, 4)),
                notice(
                    "textDocument/didChange",
                    json!({"textDocument":{"uri":uri,"version":3},"contentChanges":[{"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":5}},"rangeLength":2,"text":"🦀"}]}),
                ),
                request(9, "textDocument/documentSymbol", text_doc(uri)),
                request(10, "workspace/symbol", json!({"query":"main"})),
                request(11, "workspace/symbol", json!({"query":""})),
            ],
        ));
    }
    // Every example that checks, opened and navigated: symbols, a
    // definition at each of up to 40 token starts, an edit, and again.
    let docs = corpus_docs();
    let examples: Vec<&Doc> = docs
        .iter()
        .filter(|d| d.name.starts_with("examples/"))
        .take(24)
        .collect();
    for d in &examples {
        let source = d.checked.source();
        let mut m = vec![
            initialize(true),
            notice("initialized", json!({})),
            open(&d.uri, 1, source),
            request(2, "textDocument/documentSymbol", text_doc(&d.uri)),
        ];
        let mut id = 10;
        let ps = name_positions(d);
        let stride = (ps.len() / 40).max(1);
        for p in ps.iter().step_by(stride) {
            m.push(request(
                id,
                "textDocument/definition",
                at(&d.uri, p.line, p.character),
            ));
            id += 1;
        }
        // Prepend a declaration, which moves every range, then ask again.
        m.push(notice(
            "textDocument/didChange",
            json!({"textDocument":{"uri":d.uri,"version":2},"contentChanges":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"text":"fn added_first() -> u64 { 7 }\n"}]}),
        ));
        m.push(request(3, "textDocument/documentSymbol", text_doc(&d.uri)));
        for p in ps.iter().step_by(stride).take(10) {
            m.push(request(
                id,
                "textDocument/definition",
                at(&d.uri, p.line + 1, p.character),
            ));
            id += 1;
        }
        m.push(request(4, "workspace/symbol", json!({"query":"added"})));
        out.push((format!("editing_{}", d.name), m));
    }
    // Several open documents and workspace queries over them.
    let mut m = vec![initialize(false), notice("initialized", json!({}))];
    for d in examples.iter().take(12) {
        m.push(open(&d.uri, 1, d.checked.source()));
    }
    let mut id = 2;
    for q in QUERIES {
        m.push(request(id, "workspace/symbol", json!({ "query": q })));
        id += 1;
    }
    for d in examples.iter().take(12) {
        m.push(request(id, "textDocument/documentSymbol", text_doc(&d.uri)));
        id += 1;
    }
    out.push(("workspace".into(), m));
    out
}

/// The server's response to each request id.
fn responses(output: &[u8]) -> BTreeMap<i64, Value> {
    let mut out = BTreeMap::new();
    let mut cursor = std::io::Cursor::new(output);
    while let Ok(Some(body)) =
        vesper_lsp::read_frame(&mut cursor, vesper_lsp::ProtocolLimits::default())
    {
        let v: Value = serde_json::from_slice(&body).unwrap();
        if let Some(id) = v.get("id").and_then(Value::as_i64)
            && v.get("method").is_none()
        {
            out.insert(id, v);
        }
    }
    out
}

/// The server's answer in the entries' form: `R` and the result, or `E`
/// and the error object.
fn server_answer(v: &Value) -> String {
    if let Some(e) = v.get("error") {
        format!("E{}", serde_json::to_string(e).unwrap())
    } else {
        format!("R{}", serde_json::to_string(&v["result"]).unwrap())
    }
}

#[test]
fn sessions_match_server() {
    let mut differ = vec![];
    let mut c = Counts::default();
    let mut session_level = 0;
    for (name, messages) in scripts() {
        let input: Vec<u8> = messages
            .iter()
            .chain(&[
                request(900, "shutdown", json!({})),
                notice("exit", json!({})),
            ])
            .flat_map(|m| frame(&serde_json::to_vec(m).unwrap()))
            .collect();
        let output = serve(&input);
        let replies = responses(&output);
        // Mirror the session's documents with the store it uses.
        let mut store = DocumentStore::new(StoreLimits::default()).unwrap();
        let mut hierarchical = false;
        for m in &messages {
            let method = m["method"].as_str().unwrap_or("");
            let p = &m["params"];
            match method {
                "initialize" => {
                    hierarchical = p["capabilities"]["textDocument"]["documentSymbol"]
                        ["hierarchicalDocumentSymbolSupport"]
                        .as_bool()
                        .unwrap_or(false);
                }
                "textDocument/didOpen" => {
                    let d = &p["textDocument"];
                    let _ = store.open(
                        d["uri"].as_str().unwrap(),
                        d["version"].as_i64().unwrap() as i32,
                        d["text"].as_str().unwrap(),
                    );
                }
                "textDocument/didChange" => {
                    let changes: Vec<ContentChange<'_>> = p["contentChanges"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|ch| ContentChange {
                            range: ch.get("range").map(|r| {
                                let p = |v: &Value| Position {
                                    line: v["line"].as_u64().unwrap() as u32,
                                    character: v["character"].as_u64().unwrap() as u32,
                                };
                                vesper_lsp::Range {
                                    start: p(&r["start"]),
                                    end: p(&r["end"]),
                                }
                            }),
                            range_length: ch
                                .get("rangeLength")
                                .and_then(Value::as_u64)
                                .map(|n| n as u32),
                            text: ch["text"].as_str().unwrap(),
                        })
                        .collect();
                    let _ = store.change(
                        p["textDocument"]["uri"].as_str().unwrap(),
                        p["textDocument"]["version"].as_i64().unwrap() as i32,
                        &changes,
                    );
                }
                "textDocument/didClose" => {
                    let _ = store.close(p["textDocument"]["uri"].as_str().unwrap());
                }
                "textDocument/documentSymbol" | "textDocument/definition" => {
                    let id = m["id"].as_i64().unwrap();
                    let uri = p["textDocument"]["uri"].as_str().unwrap();
                    let Some(document) = store.get(uri) else {
                        session_level += 1;
                        continue;
                    };
                    let text = document.text();
                    if snapshot(text).is_none() {
                        session_level += 1;
                        continue;
                    }
                    let vesper = if method == "textDocument/documentSymbol" {
                        c.symbols += 1;
                        vesper_symbols(uri, text, hierarchical)
                    } else {
                        c.definitions += 1;
                        let position = Position {
                            line: p["position"]["line"].as_u64().unwrap() as u32,
                            character: p["position"]["character"].as_u64().unwrap() as u32,
                        };
                        vesper_definition(uri, text, position)
                    };
                    if vesper == "D" {
                        c.deferred += 1;
                        continue;
                    }
                    let server = server_answer(&replies[&id]);
                    if server.starts_with("R{") && method == "textDocument/definition" {
                        c.targets += 1;
                    }
                    compare(
                        &mut differ,
                        &mut c,
                        format!("{name} id {id}"),
                        server,
                        vesper,
                    );
                }
                "workspace/symbol" => {
                    let id = m["id"].as_i64().unwrap();
                    let docs: Vec<(String, String)> = store
                        .iter()
                        .map(|d| (d.uri().to_string(), d.text().to_string()))
                        .collect();
                    if docs.iter().any(|(_, t)| snapshot(t).is_none()) {
                        session_level += 1;
                        continue;
                    }
                    let texts: Vec<(&str, &str)> =
                        docs.iter().map(|(u, t)| (u.as_str(), t.as_str())).collect();
                    c.workspace += 1;
                    let vesper = vesper_workspace(&texts, p["query"].as_str().unwrap());
                    compare(
                        &mut differ,
                        &mut c,
                        format!("{name} id {id}"),
                        server_answer(&replies[&id]),
                        vesper,
                    );
                }
                _ => {}
            }
        }
    }
    eprintln!(
        "sessions: {} symbol, {} definition ({} targets, {} deferred) and {} workspace \
         responses compared, {} left to the session (no snapshot); errors {:?}; {} differ",
        c.symbols,
        c.definitions,
        c.targets,
        c.deferred,
        c.workspace,
        session_level,
        c.errors,
        differ.len()
    );
    assert!(differ.is_empty(), "{}", differ.join("\n"));
    assert!(
        c.symbols >= 20 && c.targets >= 50,
        "too few responses compared"
    );
}

// -------------------------------------------------------------- speed

/// ABBA timings of the three requests on the corpus
/// (`CHECK_HARNESS_SPEED=1 cargo test --release --test
/// self_host_lsp_features lsp_features_speed -- --ignored`).
#[test]
#[ignore]
fn lsp_features_speed() {
    if !speed() {
        return;
    }
    let docs = corpus_docs();
    let cursors: Vec<(usize, Position)> = docs
        .iter()
        .enumerate()
        .flat_map(|(i, d)| positions(d).into_iter().step_by(8).map(move |p| (i, p)))
        .collect();
    let (r, v) = abba(
        "documentSymbol, whole corpus",
        &mut || {
            time(|| {
                for d in docs {
                    std::hint::black_box(rust_symbols(&d.uri, &d.checked, &d.index, true));
                }
            })
        },
        &mut || {
            time(|| {
                for d in docs {
                    std::hint::black_box(vesper_symbols(&d.uri, d.checked.source(), true));
                }
            })
        },
    );
    eprintln!(
        "documentSymbol per document ({}): rust {:?} vesper {:?}",
        docs.len(),
        r / docs.len() as u32,
        v / docs.len() as u32
    );
    let per_doc: Vec<Vec<Position>> = docs
        .iter()
        .map(|d| positions(d).into_iter().step_by(8).collect())
        .collect();
    let (r, v) = abba(
        "definition, sampled cursors",
        &mut || {
            time(|| {
                for (i, p) in &cursors {
                    let d = &docs[*i];
                    std::hint::black_box(rust_definition(&d.uri, &d.checked, &d.index, *p));
                }
            })
        },
        &mut || {
            time(|| {
                for (i, p) in &cursors {
                    let d = &docs[*i];
                    std::hint::black_box(vesper_definition(&d.uri, d.checked.source(), *p));
                }
            })
        },
    );
    eprintln!(
        "definition per request, parsing each time ({}): rust {:?} vesper {:?}",
        cursors.len(),
        r / cursors.len() as u32,
        v / cursors.len() as u32
    );
    let (r, v) = abba(
        "definition, parse kept per document",
        &mut || {
            time(|| {
                for (d, ps) in docs.iter().zip(&per_doc) {
                    for p in ps {
                        std::hint::black_box(rust_definition(&d.uri, &d.checked, &d.index, *p));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for (d, ps) in docs.iter().zip(&per_doc) {
                    std::hint::black_box(vesper_definitions(&d.uri, d.checked.source(), ps));
                }
            })
        },
    );
    eprintln!(
        "definition per request ({}): rust {:?} vesper {:?}",
        cursors.len(),
        r / cursors.len() as u32,
        v / cursors.len() as u32
    );
}
