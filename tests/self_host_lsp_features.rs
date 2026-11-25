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
use std::time::Duration;
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
            "lsp_document_symbols_kept",
            "lsp_hovers",
            "lsp_proof_models",
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

/// `lsp_document_symbols_kept`: one parse, `n` requests, an index rebuilt
/// per request.
fn vesper_symbols_kept(uri: &str, source: &str, hierarchical: bool, n: usize) -> Vec<String> {
    let mut input = vec![];
    index_words(&mut input, index_limits());
    push_word(&mut input, hierarchical as u64);
    push_word(&mut input, n as u64);
    push_word(&mut input, uri.len() as u64);
    push_word(&mut input, source.len() as u64);
    input.extend_from_slice(uri.as_bytes());
    input.extend_from_slice(source.as_bytes());
    let text = entry("lsp_document_symbols_kept").text(&input);
    if n == 0 {
        return vec![];
    }
    text.split('\n').map(str::to_string).collect()
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

fn push_text(out: &mut Vec<u8>, s: &str) {
    push_word(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
}

/// The checker's facts the documentation-link inventory reads
/// (doc_links.ves `read_doc_facts`): every function, generated function,
/// ADT, effect with its operations, and trait name of the world.
fn doc_facts(out: &mut Vec<u8>, w: &vesper::check::World) {
    push_word(out, w.functions.len() as u64);
    for (name, f) in &w.functions {
        push_text(out, name);
        push_word(out, f.span.start as u64);
        push_word(out, f.span.end as u64);
    }
    push_word(out, w.elaboration.generated.len() as u64);
    for name in w.elaboration.generated.keys() {
        push_text(out, name);
    }
    push_word(out, w.adts.len() as u64);
    for (name, a) in &w.adts {
        push_text(out, name);
        push_word(out, a.span.start as u64);
        push_word(out, a.span.end as u64);
    }
    push_word(out, w.effects.len() as u64);
    for (name, e) in &w.effects {
        push_text(out, name);
        push_word(out, e.span.start as u64);
        push_word(out, e.span.end as u64);
        push_word(out, e.ops.len() as u64);
        for op in &e.ops {
            push_text(out, &op.name);
            push_word(out, op.span.start as u64);
            push_word(out, op.span.end as u64);
        }
    }
    push_word(out, w.elaboration.traits.len() as u64);
    for name in &w.elaboration.traits {
        push_text(out, name);
    }
}

/// `lsp_definitions`: one parse, an index rebuilt per cursor. With the
/// world, the documentation-link fallback runs too; without it those
/// cursors answer `D`.
fn vesper_definitions(
    uri: &str,
    source: &str,
    world: Option<&vesper::check::World>,
    cursors: &[Position],
) -> Vec<String> {
    let mut input = vec![];
    index_words(&mut input, index_limits());
    push_word(&mut input, uri.len() as u64);
    push_word(&mut input, source.len() as u64);
    push_word(&mut input, cursors.len() as u64);
    for p in cursors {
        push_word(&mut input, p.line as u64);
        push_word(&mut input, p.character as u64);
    }
    push_word(&mut input, world.is_some() as u64);
    if let Some(w) = world {
        doc_facts(&mut input, w);
    }
    input.extend_from_slice(uri.as_bytes());
    input.extend_from_slice(source.as_bytes());
    let text = entry("lsp_definitions").text(&input);
    if cursors.is_empty() {
        return vec![];
    }
    text.split('\n').map(str::to_string).collect()
}

/// `lsp_hovers`: the checker's facts in the check-data harness layout,
/// then one parse and an index rebuilt per cursor.
fn vesper_hovers(s: &CheckedSource, cursors: &[Position]) -> Vec<String> {
    let checked = s.checked();
    let mut world = checked.world.clone();
    world.types = checked.expression_types.clone();
    let source_functions: std::collections::BTreeSet<&str> = s
        .parsed()
        .module
        .items
        .iter()
        .filter_map(|item| match item {
            vesper::ast::Item::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    let input = encode(&world, |e, tail| {
        let mut words = vec![];
        index_words(&mut words, index_limits());
        for chunk in words.chunks(8) {
            tail.push(u64::from_le_bytes(chunk.try_into().unwrap()));
        }
        let functions: Vec<_> = world
            .functions
            .iter()
            .filter(|(name, _)| source_functions.contains(name.as_str()))
            .collect();
        tail.push(functions.len() as u64);
        for (name, f) in functions {
            tail.push(e.name(name));
            tail.extend([f.span.start as u64, f.span.end as u64]);
            match world.function_results.get(name) {
                Some(t) => {
                    let t = e.ty(t);
                    tail.extend([1, t]);
                }
                None => tail.extend([0, 0]),
            }
            tail.push(f.params.len() as u64);
            for p in &f.params {
                tail.extend([p.span.start as u64, p.span.end as u64]);
                let n = e.name(&p.name);
                let t = e.ty(&p.ty);
                tail.extend([n, t]);
            }
        }
        tail.push(cursors.len() as u64);
        for p in cursors {
            tail.extend([p.line as u64, p.character as u64]);
        }
        tail.push(s.source().len() as u64);
    });
    let mut input = input;
    input.extend_from_slice(s.source().as_bytes());
    let text = entry("lsp_hovers").text(&input);
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

fn rust_hover(s: &CheckedSource, index: &LineIndex, at: Position) -> String {
    answer(
        navigation::hover(s, index, at, navigation::Limits::default())
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
const PROOF_SCALAR: &str = "fn inc(x: i64) -> i64 eff [Fail[ArithOverflow]] {
    if x < 100 {
        x + 1
    } else {
        0
    }
}

fn add(a: i64, b: i64) -> i64 eff [Fail[ArithOverflow]] {
    a + b
}

fn half(x: i64) -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    let q = x / 2;
    let r = x % 3;
    wrapping_add(q, r)
}

fn ratio(x: u64, y: u64) -> u64 eff [Fail[RuntimeError]] {
    x / y
}

fn both(a: bool, b: bool, x: u8) -> bool eff [Fail[ArithOverflow]] {
    a && (x < 200) && (wrapping_add(x, 100) > 3)
}

fn negate(x: i64) -> i64 eff [Fail[ArithOverflow]] {
    -x
}

fn counted(x: u32) -> u32 eff [Fail[ArithOverflow]] {
    let mut total = x;
    total += 1;
    if total > 10 {
        total - 10
    } else {
        total
    }
}

fn main() -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    add(inc(3), half(4))
}

fn answer() -> i64 {
    42
}
";

const PROOF_OPS: &str =
    "fn signed(x: i64, y: i64) -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    let a = x / -3;
    let b = x % -3;
    let c = x / y;
    let d = x % y;
    let e = wrapping_sub(a, b);
    let f = wrapping_mul(c, d);
    let mut g = e * 2;
    g -= 1;
    g *= 3;
    g /= 5;
    g %= 7;
    g = g - f;
    if x >= y || x <= -5 {
        -g
    } else {
        g
    }
}

fn unsigned(x: u8, y: u32, z: u64) -> bool eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    let a = wrapping_mul(x, 3);
    let b = wrapping_sub(y, 9) / 7;
    let c = z % 10;
    let d = z * 2 - 1;
    let ok = a != 0 && b > 1 || c == d;
    let flag = !ok;
    flag == true
}

fn units(x: u64) eff [Fail[ArithOverflow]] {
    let y = x + 1;
    if y > 3 {
        ()
    }
}

fn nothing() {
}

fn small(x: u8) -> u8 eff [Fail[ArithOverflow]] {
    let y: u8 = 7;
    x + y + 255
}

fn wide_literals(x: u64, y: i64) -> bool eff [Fail[ArithOverflow]] {
    x + 18446744073709551000 > 5 && y - 9223372036854775807 < 0
}

fn calls(x: u32) -> u32 eff [Fail[ArithOverflow]] {
    helper(x, true) + helper(1, false)
}

fn helper(v: u32, on: bool) -> u32 eff [Fail[ArithOverflow]] {
    if on {
        v * 2
    } else {
        v
    }
}
";

const PROOF_REASONS: &str = "linear struct Tx {
    slot: u64
}

enum Shape { Dot, Square(u64) }

fn make(x: u64) -> Tx {
    Tx {
        slot: x
    }
}

fn loop_forever(x: u64) -> u64 {
    loop_forever(x)
}

fn ping(x: u64) -> u64 {
    pong(x)
}

fn pong(x: u64) -> u64 {
    ping(x)
}

fn pick[T](sink x: T) -> T {
    x
}

fn bump(inout x: u64) {
    x = wrapping_add(x, 1);
}

fn floaty(x: f64) -> f64 {
    x
}

fn helper(x: u64) -> u64 {
    x
}

fn opt(x: Option[u64]) -> u64 {
    0
}

fn matched(x: u64) -> u64 {
    match x {
        0 => 1,
        _ => 2,
    }
}

fn looped(x: u64) -> u64 {
    let mut i = 0;
    loop {
        if i > x {
            break;
        }
        i = wrapping_add(i, 1);
    }
    i
}

fn early(x: u64) -> u64 {
    return x;
}

fn typed_let(x: Option[u64]) -> u64 {
    let s: Option[u64] = x;
    0
}

fn shapes(s: Shape) -> u64 {
    0
}

fn outer(x: u64, c: bool) -> u64 {
    let mut t = x;
    if c {
        t = 1;
    }
    t
}

fn none() {
}

fn unit_cmp(a: u64) -> bool {
    none() == none()
}

fn text() -> u64 {
    let s = \"hi\";
    1
}

effect Log { fn note(x: u64) -> Unit; }

fn many(p0: u64, p1: u64, p2: u64, p3: u64, p4: u64, p5: u64, p6: u64, p7: u64, p8: u64, p9: u64, p10: u64, p11: u64, p12: u64, p13: u64, p14: u64, p15: u64, p16: u64) -> u64 {
    p0
}

fn logged(x: u64) -> u64 eff [Log] {
    x
}

fn shape_let(x: u64) -> u64 {
    let s: Shape = Shape::Dot;
    x
}

fn shape_value(x: u64) -> u64 {
    let s = Shape::Square(x);
    x
}

struct Pt { x: u64 }

fn built(x: u64) -> u64 {
    let t = Pt { x: x };
    0
}

fn calls_unknown(x: u64) -> u64 eff [Fail[ArithOverflow]] {
    helper(x) + 1
}
";

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
    // The link scanner's budgets: one group of 1,300 comment lines passes
    // `max_work` through the exact-growth line vector (24 bytes a push;
    // 1,000 lines stay under it),
    // and 6,000 lines pass its token envelope (10,922 tokens).
    for (name, lines) in [
        ("doc_work_under", 1000),
        ("doc_work", 1300),
        ("doc_tokens", 6000),
    ] {
        let mut s = String::new();
        for _ in 0..lines {
            s.push_str("/// [f]\n");
        }
        s.push_str("fn f() -> u64 { 1 }\n");
        out.push((name.into(), s));
    }
    // Proof-lens models: the scalar shapes the model covers, and each
    // reason a function has none.
    out.push(("proof_scalar".into(), PROOF_SCALAR.into()));
    out.push(("proof_reasons".into(), PROOF_REASONS.into()));
    out.push(("proof_ops".into(), PROOF_OPS.into()));
    // A call chain deeper than the inlining limit (8), and a body past the
    // node budget (4,096).
    let mut chain = String::new();
    for i in 0..11 {
        if i == 10 {
            chain.push_str("fn c10(x: u64) -> u64 { x }\n");
        } else {
            chain.push_str(&format!("fn c{i}(x: u64) -> u64 {{ c{}(x) }}\n", i + 1));
        }
    }
    out.push(("proof_chain".into(), chain));
    let mut wide = String::from("fn wide(x: u64) -> u64 eff [Fail[ArithOverflow]] {\n");
    for i in 0..460 {
        wide.push_str(&format!("    let a{i} = x + x + x + x + {i};\n"));
    }
    out.push(("proof_budget".into(), format!("{wide}    x\n}}\n")));
    // Documentation links of every target kind and the Markdown shapes
    // the link scanner skips: code spans and fences, inline and
    // reference-style links, images, definitions with titles, escapes,
    // prose brackets, unknown, unsupported and `Self` paths.
    out.push((
        "doc_links".into(),
        r#"/// A shape. See [area], [`Shape::grow`] and [Log::note].
/// Code: `[not_a_link]` and ``[also `not`]``. An image ![alt][area].
/// Inline [text](area) and [ref][area], then [ref2][area](x "t").
/// [def]: area "title"
///   'continued'
/// Escaped \[area] and prose [a b c], unknown [nope], [@pkg::x],
/// unsupported [Shape::grow[u64]] and [Self::grow], [Log], [Pair].
enum Shape { Dot, Square(u64) }
/// Uses [area] twice: [area].
///
/// ```
/// [area] in a fence
/// ```
/// After the fence [Shape].
fn area(s: Shape) -> u64 { match s { Shape::Dot => 0, Shape::Square(n) => n } }
struct Pair { a: u64 }
impl Shape {
    /// Grows [Shape] by [Pair] (see [`area`] and [Shape::grow]).
    fn grow(self) -> u64 { area(Shape::Dot) }
}
//! Module note: [area], [Log::note], [missing::path].
effect Log { fn note(x: u64) -> Unit; }
/// Unbalanced [area
fn main() -> u64 { area(Shape::Dot) }
"#
        .into(),
    ));
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
    doc_targets: usize,
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
            let rust = rust_symbols(&d.uri, &d.checked, &d.index, hierarchical);
            let kept = vesper_symbols_kept(&d.uri, d.checked.source(), hierarchical, 3);
            assert_eq!(kept.len(), 3, "{}", d.name);
            for (k, answer) in kept.into_iter().enumerate() {
                compare(
                    &mut differ,
                    &mut c,
                    format!("{} symbols kept #{k} hierarchical={hierarchical}", d.name),
                    rust.clone(),
                    answer,
                );
            }
            compare(
                &mut differ,
                &mut c,
                format!("{} symbols hierarchical={hierarchical}", d.name),
                rust,
                vesper_symbols(&d.uri, d.checked.source(), hierarchical),
            );
        }
        let cursors = positions(d);
        let batch = vesper_definitions(
            &d.uri,
            d.checked.source(),
            Some(d.checked.world()),
            &cursors,
        );
        assert_eq!(batch.len(), cursors.len(), "{}", d.name);
        for (k, at) in cursors.into_iter().enumerate() {
            let rust = rust_definition(&d.uri, &d.checked, &d.index, at);
            let vesper = vesper_definition(&d.uri, d.checked.source(), at);
            c.definitions += 1;
            if rust.starts_with("R{") {
                c.targets += 1;
            }
            if vesper == "D" {
                // The single entry has no world, so it leaves the
                // documentation-link fallback to the batch entry.
                c.deferred += 1;
                if rust.starts_with("R{") {
                    c.doc_targets += 1;
                }
            } else if batch[k] != vesper && differ.len() < 20 {
                differ.push(format!(
                    "{} definition {}:{}: batch {} single {}",
                    d.name, at.line, at.character, batch[k], vesper
                ));
            }
            compare(
                &mut differ,
                &mut c,
                format!("{} definition {}:{}", d.name, at.line, at.character),
                rust,
                batch[k].clone(),
            );
        }
    }
    // Every cursor on every documentation-comment line: the
    // documentation-link fallback answers inside the comment text.
    let mut comment_cursors = 0usize;
    let mut comment_targets = 0usize;
    for d in docs {
        let source = d.checked.source();
        let mut cursors = vec![];
        for (line, text) in source.split('\n').enumerate() {
            if !(text.contains("///") || text.contains("//!")) {
                continue;
            }
            let units: u32 = text.encode_utf16().count() as u32;
            for character in 0..=units {
                cursors.push(Position {
                    line: line as u32,
                    character,
                });
            }
        }
        if cursors.is_empty() {
            continue;
        }
        let batch = vesper_definitions(&d.uri, source, Some(d.checked.world()), &cursors);
        assert_eq!(batch.len(), cursors.len(), "{}", d.name);
        for (k, at) in cursors.into_iter().enumerate() {
            let rust = rust_definition(&d.uri, &d.checked, &d.index, at);
            comment_cursors += 1;
            if rust.starts_with("R{") {
                comment_targets += 1;
            }
            compare(
                &mut differ,
                &mut c,
                format!("{} comment definition {}:{}", d.name, at.line, at.character),
                rust,
                batch[k].clone(),
            );
        }
    }
    eprintln!("documentation comments: {comment_cursors} cursors, {comment_targets} with a target");
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
        "navigation: {} documents, {} symbol answers, {} definitions ({} targets, {} in doc \
         comments, {} of them documentation-link targets), {} workspace queries; errors {:?}; \
         {} differ",
        docs.len(),
        c.symbols,
        c.definitions,
        c.targets,
        c.deferred,
        c.doc_targets,
        c.workspace,
        c.errors,
        differ.len()
    );
    assert!(differ.is_empty(), "{}", differ.join("\n"));
    assert!(c.targets > 1000, "only {} definition targets", c.targets);
}

/// The Rust `size_of` values doc_links.ves charges its budgets with.
#[test]
fn doc_link_budget_sizes() {
    use std::mem::size_of;
    use vesper_doc::links::{LinkDiagnostic, LinkReference, LinkTarget};
    assert_eq!(size_of::<LinkTarget>(), 32, "SIZE_LINK_TARGET");
    assert_eq!(size_of::<std::ops::Range<usize>>(), 16, "SIZE_SPAN");
    assert_eq!(size_of::<(&str, usize)>(), 24, "SIZE_LINE");
    assert_eq!(size_of::<LinkReference>(), 96, "SIZE_REFERENCE");
    assert_eq!(size_of::<LinkDiagnostic>(), 64, "SIZE_DIAGNOSTIC");
    assert_eq!(size_of::<vesper_syntax::lexer::Token>(), 24, "SIZE_TOKEN");
}

/// `navigation::hover` at every cursor of every corpus document.
#[test]
fn hover_matches_rust() {
    let docs = corpus_docs();
    let mut differ = vec![];
    let mut c = Counts::default();
    let (mut hovers, mut values) = (0usize, 0usize);
    // Answers that reach each part of the hover text.
    let parts = [
        "type mode: ",
        "\\nparameter: ",
        "\\ndeclared mode: ",
        "\\nfn ",
        "\"value\":\"fn ",
        "integer literal",
        " fn(",
        "eff [",
    ];
    let mut reached = [0usize; 8];
    for d in docs {
        let cursors = positions(d);
        let batch = vesper_hovers(&d.checked, &cursors);
        assert_eq!(batch.len(), cursors.len(), "{}", d.name);
        for (k, at) in cursors.into_iter().enumerate() {
            let rust = rust_hover(&d.checked, &d.index, at);
            hovers += 1;
            if rust.starts_with("R{") {
                values += 1;
                for (n, part) in reached.iter_mut().zip(parts) {
                    *n += rust.contains(part) as usize;
                }
            }
            compare(
                &mut differ,
                &mut c,
                format!("{} hover {}:{}", d.name, at.line, at.character),
                rust,
                batch[k].clone(),
            );
        }
    }
    eprintln!(
        "hover: {} documents, {hovers} hovers ({values} with contents); reached {:?}; errors \
         {:?}; {} differ",
        docs.len(),
        parts.iter().zip(reached).collect::<Vec<_>>(),
        c.errors,
        differ.len()
    );
    assert!(differ.is_empty(), "{}", differ.join("\n"));
    assert!(values > 1000, "only {values} hovers with contents");
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
    // The generated budget docs (thousands of `/// [f]` lines) measure the
    // doc-link limits, not editor speed, so they stay out of the timings.
    let docs: Vec<&Doc> = corpus_docs()
        .iter()
        .filter(|d| !d.uri.starts_with("untitled:doc_"))
        .collect();
    let docs = &docs[..];
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
    // The server shape: the parse is kept in the snapshot and the index is
    // built per request (Rust's `Index::new` per request too). The kept
    // entry answers `KEPT` requests on one parse.
    const KEPT: usize = 32;
    let (r, v) = abba(
        "documentSymbol, parse kept per document",
        &mut || {
            time(|| {
                for d in docs {
                    for _ in 0..KEPT {
                        std::hint::black_box(rust_symbols(&d.uri, &d.checked, &d.index, true));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for d in docs {
                    std::hint::black_box(vesper_symbols_kept(
                        &d.uri,
                        d.checked.source(),
                        true,
                        KEPT,
                    ));
                }
            })
        },
    );
    let requests = (docs.len() * KEPT) as u32;
    eprintln!(
        "documentSymbol per request, parse kept ({} x {KEPT}): rust {:?} vesper {:?}",
        docs.len(),
        r / requests,
        v / requests
    );
    let hover_cursors: Vec<Vec<Position>> = docs
        .iter()
        .map(|d| positions(d).into_iter().step_by(8).collect())
        .collect();
    let (r, v) = abba(
        "hover, parse and facts kept per document",
        &mut || {
            time(|| {
                for (d, ps) in docs.iter().zip(&hover_cursors) {
                    for p in ps {
                        std::hint::black_box(rust_hover(&d.checked, &d.index, *p));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for (d, ps) in docs.iter().zip(&hover_cursors) {
                    std::hint::black_box(vesper_hovers(&d.checked, ps));
                }
            })
        },
    );
    let requests: usize = hover_cursors.iter().map(Vec::len).sum();
    eprintln!(
        "hover per request ({requests}): rust {:?} vesper {:?} (vesper includes encoding \
         and decoding the facts once per document)",
        r / requests as u32,
        v / requests as u32
    );
    // The session builds every model once per snapshot from the parse it
    // keeps. Vesper's one-time cost per document (the harness encoding the
    // facts, decoding them, the parse) is timed with 0 rounds and taken off
    // 64 rounds; the best of five runs each, interleaved.
    const ROUNDS: u64 = 64;
    let (mut rust, mut fixed, mut full) = (Duration::MAX, Duration::MAX, Duration::MAX);
    let mut hosted = Duration::MAX;
    for _ in 0..5 {
        hosted = hosted.min(time(|| {
            for d in docs {
                std::hint::black_box(vesper_proof_models_kept(&d.checked, ROUNDS, false));
            }
        }));
        rust = rust.min(time(|| {
            for d in docs {
                for _ in 0..ROUNDS {
                    std::hint::black_box(vesper::lsp::proof_models_for_port(&d.checked));
                }
            }
        }));
        fixed = fixed.min(time(|| {
            for d in docs {
                std::hint::black_box(vesper_proof_models_kept(&d.checked, 0, false));
            }
        }));
        full = full.min(time(|| {
            for d in docs {
                std::hint::black_box(vesper_proof_models_kept(&d.checked, ROUNDS, true));
            }
        }));
    }
    let rounds = docs.len() as u32 * ROUNDS as u32;
    // The host hashes once per answer, not per round: its hashing is in
    // `fixed` and `hosted` alike, so the difference is the port's rounds.
    // That hashing is charged per round too: the host's C BLAKE3 over every
    // model's renderings, best of five.
    let renderings: Vec<Vec<[String; 3]>> = docs
        .iter()
        .map(|d| {
            vesper::lsp::proof_models_for_port(&d.checked)
                .into_iter()
                .filter_map(|(_, m)| m.ok())
                .collect()
        })
        .collect();
    let mut hashing = Duration::MAX;
    for _ in 0..5 {
        hashing = hashing.min(time(|| {
            for ms in &renderings {
                for m in ms {
                    let mut h = blake3::Hasher::new();
                    h.update(b"vesper.lens.model/1\0");
                    h.update(m[0].as_bytes());
                    h.update(b"\0");
                    h.update(m[1].as_bytes());
                    std::hint::black_box(h.finalize().to_hex());
                }
            }
        }));
    }
    let h = hosted.saturating_sub(fixed) / rounds + hashing / docs.len() as u32;
    let r = rust / rounds;
    eprintln!(
        "proof-lens models per document round, key hashed by the host's C BLAKE3: rust {r:?} \
         vesper {h:?} ratio {:.2}",
        h.as_secs_f64() / r.as_secs_f64()
    );
    let v = full.saturating_sub(fixed) / rounds;
    eprintln!(
        "proof-lens models per document round ({} x {ROUNDS}), key hashed by the portable \
         Vesper BLAKE3: rust {r:?} vesper {v:?} ratio {:.2} (vesper's one-time encode, decode and parse: {:?} per document)",
        docs.len(),
        v.as_secs_f64() / r.as_secs_f64(),
        fixed / docs.len() as u32
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
                    std::hint::black_box(vesper_definitions(
                        &d.uri,
                        d.checked.source(),
                        Some(d.checked.world()),
                        ps,
                    ));
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

// ------------------------------------------------------------ proof models

/// The preludes the checker can add to a module (their functions reach
/// `World::functions` with spans into the prelude's own text).
fn preludes() -> [&'static str; 5] {
    [
        vesper::strings::PRELUDE,
        vesper::atomics::PRELUDE,
        vesper::atomics::WORKERS_PRELUDE,
        vesper::trace_effects::PRELUDE,
        vesper::trace_effects::TELEMETRABLE_PRELUDE,
    ]
}

/// The function trees of a parsed text, as the checker names them.
fn trees(module: &vesper::ast::Module) -> Vec<Tree> {
    let mut out = vec![];
    for item in &module.items {
        match item {
            vesper::ast::Item::Function(f) => out.push((f.name.clone(), f.span.clone())),
            vesper::ast::Item::Impl { ty, methods, .. } => {
                for f in methods {
                    out.push((format!("{}::{}", ty.name(), f.name), f.span.clone()));
                }
            }
            _ => {}
        }
    }
    out
}

/// `lsp_proof_models`: the checker's facts, the function table and one text
/// (the source, then each prelude a function comes from). `None` when a
/// function's tree is in no known text.
/// One function's proof-lens model from the port: its renderings and key,
/// or the reason it has none.
type PortModel = (String, Result<[String; 3], String>);

/// A function's tree, as the checker names it, and its span.
type Tree = (String, std::ops::Range<usize>);

fn vesper_proof_models(s: &CheckedSource) -> Option<Vec<PortModel>> {
    vesper_proof_models_kept(s, 1, true)
}

/// `lsp_proof_models` with `reps` rounds of every model on one parse. With
/// `hash` false the port leaves the key to the host, which hashes the
/// renderings with the C BLAKE3 (the `blake3` crate), as the hosted build
/// does through `@vesper-std/hash-sys`.
fn vesper_proof_models_kept(s: &CheckedSource, reps: u64, hash: bool) -> Option<Vec<PortModel>> {
    let checked = s.checked();
    let mut world = checked.world.clone();
    world.types = checked.expression_types.clone();
    let mut texts: Vec<(&str, Vec<Tree>)> = vec![(s.source(), trees(&s.parsed().module))];
    for p in preludes() {
        texts.push((p, trees(&vesper::parser::parse(p).module)));
    }
    // A prelude is parsed at an offset (after the source), so its spans in
    // the world are its own spans plus that offset.
    let mut offset: Vec<Option<usize>> = vec![None; texts.len()];
    offset[0] = Some(0);
    let mut rows = vec![];
    for (name, f) in &world.functions {
        let mut placed = None;
        for (t, (_, fs)) in texts.iter().enumerate() {
            for (n, sp) in fs {
                if n != name
                    || sp.end - sp.start != f.span.end - f.span.start
                    || f.span.start < sp.start
                {
                    continue;
                }
                let o = f.span.start - sp.start;
                if offset[t].is_none_or(|x| x == o) {
                    offset[t] = Some(o);
                    placed = Some(t);
                    break;
                }
            }
            if placed.is_some() {
                break;
            }
        }
        let Some(t) = placed else {
            eprintln!("  unplaced {name} {:?}", f.span);
            return None;
        };
        rows.push((name.clone(), t, f.span.clone(), f.body.is_some()));
    }
    // The one text: the source, then each prelude used; `delta` is a
    // text's place in it less its offset in the world.
    let mut text = String::new();
    let mut delta = vec![0usize; texts.len()];
    for (t, (src, _)) in texts.iter().enumerate() {
        if let Some(o) = offset[t] {
            if t > 0 {
                text.push('\n');
            }
            delta[t] = text.len().wrapping_sub(o);
            text.push_str(src);
        }
    }
    let mut names = String::new();
    let input = encode(&world, |_, tail| {
        tail.push(rows.len() as u64);
        for (name, t, span, body) in &rows {
            tail.extend([names.len() as u64, name.len() as u64]);
            names.push_str(name);
            let d = delta[*t];
            tail.extend([
                span.start.wrapping_add(d) as u64,
                span.end.wrapping_add(d) as u64,
                d as u64,
                *body as u64,
            ]);
        }
        tail.push(text.len() as u64);
        tail.push(names.len() as u64);
        tail.push(reps);
        tail.push(hash as u64);
    });
    let mut input = input;
    input.extend_from_slice(text.as_bytes());
    input.extend_from_slice(names.as_bytes());
    let out = entry("lsp_proof_models").text(&input);
    if out.is_empty() {
        return Some(vec![]);
    }
    Some(
        out.split('\u{1e}')
            .map(|record| {
                let (name, model) = record.split_once('\u{1f}').expect("record name");
                let model = match model.strip_prefix('O') {
                    Some(rest) => {
                        let parts: Vec<&str> = rest.split('\u{1f}').collect();
                        let key = if hash {
                            parts[2].to_string()
                        } else {
                            let mut h = blake3::Hasher::new();
                            h.update(b"vesper.lens.model/1\0");
                            h.update(parts[0].as_bytes());
                            h.update(b"\0");
                            h.update(parts[1].as_bytes());
                            h.finalize().to_hex().to_string()
                        };
                        Ok([parts[0].to_string(), parts[1].to_string(), key])
                    }
                    None => Err(model.strip_prefix('E').unwrap_or(model).to_string()),
                };
                (name.to_string(), model)
            })
            .collect(),
    )
}

/// Every proof-lens model of every corpus document (Rust
/// `proof_model::all`) against the Vesper port: renderings, keys and
/// reasons.
#[test]
fn proof_models_match_rust() {
    let (mut docs, mut models, mut reasons, mut unplaced) = (0, 0, 0, 0);
    let mut diffs = 0;
    for d in corpus_docs() {
        let rust = vesper::lsp::proof_models_for_port(&d.checked);
        assert_eq!(
            vesper_proof_models_kept(&d.checked, 1, false),
            vesper_proof_models(&d.checked),
            "{}: host-hashed keys",
            d.name
        );
        let Some(port) = vesper_proof_models(&d.checked) else {
            unplaced += 1;
            eprintln!("{}: a function's tree is in no known text", d.name);
            continue;
        };
        docs += 1;
        assert_eq!(rust.len(), port.len(), "{}", d.name);
        for (r, v) in rust.iter().zip(&port) {
            match &r.1 {
                Ok(_) => models += 1,
                Err(_) => reasons += 1,
            }
            if std::env::var("PROOF_SHOW").is_ok() && d.uri.starts_with("untitled:proof") {
                eprintln!(
                    "  {} {}: {:?}",
                    d.name,
                    r.0,
                    r.1.as_ref().map(|m| m[2].clone())
                );
            }
            if r != v {
                diffs += 1;
                if diffs <= 12 {
                    eprintln!(
                        "{} {}:\n  rust   {:?}\n  vesper {:?}",
                        d.name, r.0, r.1, v.1
                    );
                }
            }
        }
    }
    eprintln!(
        "proof models: {docs} documents, {models} models, {reasons} reasons, {unplaced} unplaced, {diffs} differ"
    );
    assert_eq!(diffs, 0);
    assert_eq!(unplaced, 0);
}
