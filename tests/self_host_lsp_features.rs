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
            "lsp_lens_facts",
            "lsp_lens_repair",
            "lsp_lens_view",
            "lsp_lens_requests",
            "lsp_proof_lenses",
            "lsp_lens_state",
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

/// The linear transaction of src/lsp/lens_tests.rs, for the Lens
/// ownership-facts sources.
const LENS_PREFIX: &str = "effect Storage {
    fn write(slot: u64) -> Unit;
}

linear struct WriteTxn {
    wal_slot: u64
}

impl WriteTxn {
    fn commit(sink self) -> u64 {
        let WriteTxn {
            wal_slot
        }
        = self;
        wal_slot
    }
    fn abort(sink self) {
        let WriteTxn {
            wal_slot
        }
        = self;
    }
    fn slot(self) -> u64 {
        self.wal_slot
    }
    fn persist(self) -> u64 eff [Storage] {
        Storage::write(self.wal_slot);
        self.wal_slot
    }
}

fn open_txn(slot: u64) -> WriteTxn {
    WriteTxn {
        wal_slot: slot
    }
}
";

/// `main` bodies over `LENS_PREFIX`.
const LENS_BODIES: [(&str, &str); 8] = [
    (
        "plain",
        "    let tx = open_txn(1);\n    let other = open_txn(2);\n    other.abort();\n    tx.commit()\n",
    ),
    (
        "branch",
        "    let tx = open_txn(1);\n    let n = if tx.slot() > 0 { tx.commit() } else { tx.commit() };\n    n\n",
    ),
    (
        "closure",
        "    let f: fn @copy (sink WriteTxn) -> u64 = | sink t: WriteTxn | t.commit();\n    let tx = open_txn(1);\n    f(tx)\n",
    ),
    (
        "destructure",
        "    let WriteTxn { wal_slot } = open_txn(4);\n    wal_slot\n",
    ),
    (
        "shadow",
        "    let tx = open_txn(1);\n    let kept = tx;\n    let tx = open_txn(2);\n    kept.abort();\n    tx.commit()\n",
    ),
    (
        "handled",
        "    handle Storage in {\n        stored(5)\n    } with {\n        return v => v,\n        write(s, k) => k(()),\n    }\n}\n\nfn stored(slot: u64) -> u64 eff [Storage] {\n    Storage::write(slot);\n    let tx = open_txn(slot);\n    tx.commit()\n",
    ),
    (
        "unconsumed_branch",
        "    let tx = open_txn(1);\n    let n = tx.slot();\n    if n > 3 {\n        tx.abort();\n        0\n    } else {\n        tx.commit()\n    }\n",
    ),
    (
        "early",
        "    let tx = open_txn(1);\n    if tx.slot() > 9 {\n        return tx.commit();\n    }\n    tx.commit()\n",
    ),
];

/// Generic payloads (`Option`, a generic linear enum), match arms,
/// `in` and `sink` parameters and struct fields. Generic constructors send
/// the editor's guard through tuple admission, which refuses inherent
/// `self` receivers, so these sources use free functions.
const LENS_SHAPES: &str = "linear struct Txn {
    slot: u64
}

linear enum Slot[T] {
    Empty,
    Full(T)
}

linear struct Pair {
    left: Txn,
    right: Txn
}

fn open(slot: u64) -> Txn {
    Txn {
        slot: slot
    }
}

fn close(sink t: Txn) -> u64 {
    let Txn {
        slot
    }
    = t;
    slot
}

fn look(t: Txn) -> u64 {
    t.slot
}

fn fill(sink t: Txn) -> Slot[Txn] {
    Slot::Full(t)
}

fn drain(sink s: Slot[Txn]) -> u64 {
    match s {
        Slot::Empty => 0,
        Slot::Full(t) => close(t),
    }
}

fn split(sink p: Pair) -> u64 {
    let Pair {
        left,
        right
    }
    = p;
    let n = look(left);
    let m = close(right);
    wrapping_add(wrapping_add(n, m), close(left))
}

fn maybe(sink o: Option[Txn]) -> u64 {
    match o {
        Option::Some(t) => close(t),
        Option::None => 0,
    }
}

fn nested(c: bool) -> u64 {
    let a = open(1);
    let b = open(2);
    let n = if c {
        maybe(Option::Some(a))
    } else {
        close(a)
    };
    wrapping_add(n, close(b))
}

fn main() -> u64 {
    let p = Pair {
        left: open(1),
        right: open(2)
    };
    let tx = open(3);
    let n = look(tx);
    let o = Option::Some(open(4));
    let total = wrapping_add(wrapping_add(drain(fill(tx)), split(p)), n);
    wrapping_add(wrapping_add(total, maybe(o)), nested(true))
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
    // Lens ownership facts: linear obligations through lets, branches,
    // matches with payloads, destructuring, closures, function values,
    // handlers, method receivers and effect operations, and an early exit
    // (no facts).
    for (name, body) in LENS_BODIES {
        out.push((
            format!("lens_{name}"),
            format!("{LENS_PREFIX}\nfn main() -> u64 {{\n{body}}}\n"),
        ));
    }
    out.push(("lens_shapes".into(), LENS_SHAPES.into()));
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
        let quiet = quiet_panics();
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
        drop(quiet);
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
    // Lens facts: one view per snapshot and its queries (`View::checked`,
    // then `View::outstanding` at each cursor), on the parse the session
    // keeps; Vesper's one-time cost is taken off as for the proof models.
    let lens: Vec<(&Doc, Vec<usize>)> = docs
        .iter()
        .map(|d| {
            let facts = vesper::lsp::lens_facts_for_port(d.checked.clone(), &[]);
            (*d, lens_cursors(d.checked.source(), &facts))
        })
        .collect();
    let (mut rust, mut fixed, mut full) = (Duration::MAX, Duration::MAX, Duration::MAX);
    for _ in 0..5 {
        rust = rust.min(time(|| {
            for (d, at) in &lens {
                for _ in 0..ROUNDS {
                    std::hint::black_box(vesper::lsp::lens_facts_for_port(d.checked.clone(), at));
                }
            }
        }));
        fixed = fixed.min(time(|| {
            for (d, at) in &lens {
                std::hint::black_box(vesper_lens_facts(&d.checked, at, 0));
            }
        }));
        full = full.min(time(|| {
            for (d, at) in &lens {
                std::hint::black_box(vesper_lens_facts(&d.checked, at, ROUNDS));
            }
        }));
    }
    let r = rust / rounds;
    let v = full.saturating_sub(fixed) / rounds;
    eprintln!(
        "lens facts and outstanding obligations per document round ({} x {ROUNDS}): rust {r:?} \
         vesper {v:?} ratio {:.2} (vesper's one-time encode, decode and parse: {:?} per document)",
        docs.len(),
        v.as_secs_f64() / r.as_secs_f64(),
        fixed / docs.len() as u32
    );
    // Lens repair: one repaired view per damaged snapshot (its validating
    // parses included); Vesper's one-time cost is taken off as above.
    let damaged: Vec<_> = damaged_docs()
        .into_iter()
        .filter_map(|(_, source)| {
            vesper_syntax::parser::parse_recovery(
                &source,
                vesper_syntax::recovery::Limits::default(),
            )
            .ok()
        })
        .collect();
    const REPAIRS: u64 = 8;
    let limits = lens_limits();
    let (mut rust, mut fixed, mut full) = (Duration::MAX, Duration::MAX, Duration::MAX);
    for _ in 0..3 {
        rust = rust.min(time(|| {
            for syntax in &damaged {
                for _ in 0..REPAIRS {
                    std::hint::black_box(vesper::lsp::lens_repair_for_port(syntax, limits));
                }
            }
        }));
        fixed = fixed.min(time(|| {
            for syntax in &damaged {
                std::hint::black_box(vesper_lens_repair(syntax, limits, 0));
            }
        }));
        full = full.min(time(|| {
            for syntax in &damaged {
                std::hint::black_box(vesper_lens_repair(syntax, limits, REPAIRS));
            }
        }));
    }
    let repairs = damaged.len() as u32 * REPAIRS as u32;
    let (r, v) = (rust / repairs, full.saturating_sub(fixed) / repairs);
    eprintln!(
        "lens repair per damaged document ({} x {REPAIRS}): rust {r:?} vesper {v:?} ratio {:.2}",
        damaged.len(),
        v.as_secs_f64() / r.as_secs_f64()
    );
    // Lens views of semantic-error and repaired text with their queries:
    // Rust's view and answers on its checked parse (`Prepared`: the editor
    // guard, parse and check the Vesper port takes as input, done once),
    // against Vesper's rounds on one parse less its one-time cost.
    let views: Vec<_> = lens_views()
        .into_iter()
        .map(|(_, text, regions)| {
            let bare = ViewRequests {
                cursors: vec![],
                bindings: vec![],
                members: vec![],
                ceiling: None,
            };
            let facts = rust_lens_view(&text, &regions, limits, &bare);
            let q = view_requests(&text, &regions, &facts, true);
            (text, regions, q)
        })
        .collect();
    const VIEWS: u64 = 32;
    let rounds = |text: &str, regions: &str, q: &ViewRequests, n: u64| {
        vesper::lsp::lens_view_rounds_for_port(
            text,
            regions,
            limits,
            &q.cursors,
            &q.bindings,
            &q.members,
            q.ceiling.as_ref(),
            n,
        )
    };
    let inputs: Vec<[Vec<u8>; 2]> = views
        .iter()
        .map(|(text, regions, q)| {
            [0, VIEWS].map(|reps| lens_view_input(text, regions, limits, q, reps))
        })
        .collect();
    let view_entry = entry("lsp_lens_view");
    let (mut rust, mut basis, mut fixed, mut full) =
        (Duration::MAX, Duration::MAX, Duration::MAX, Duration::MAX);
    for _ in 0..3 {
        rust = rust.min(time(|| {
            for (text, regions, q) in &views {
                std::hint::black_box(rounds(text, regions, q, VIEWS));
            }
        }));
        basis = basis.min(time(|| {
            for (text, regions, q) in &views {
                std::hint::black_box(rounds(text, regions, q, 0));
            }
        }));
        fixed = fixed.min(time(|| {
            for [none, _] in &inputs {
                std::hint::black_box(view_entry.text(none));
            }
        }));
        full = full.min(time(|| {
            for [_, all] in &inputs {
                std::hint::black_box(view_entry.text(all));
            }
        }));
    }
    let n = views.len() as u32 * VIEWS as u32;
    let (r, v) = (
        rust.saturating_sub(basis) / n,
        full.saturating_sub(fixed) / n,
    );
    eprintln!(
        "lens view and queries per document ({} x {VIEWS}), on the checked parse: rust {r:?} \
         vesper {v:?} ratio {:.2} (rust's one-time guard, parse and check: {:?} per document)",
        views.len(),
        v.as_secs_f64() / r.as_secs_f64(),
        basis / views.len() as u32
    );
    // The Lens request layer (poison notices, completion, obligation
    // hovers, inlay hints) on one view per document: rounds of answers
    // less the one-time view on both sides.
    let lens_docs: Vec<_> = lens_documents()
        .into_iter()
        .map(|d| {
            let r = lens_requests(&d.doc, true);
            (d, r)
        })
        .collect();
    const ANSWERS: u64 = 16;
    let request_inputs: Vec<[Vec<u8>; 2]> = lens_docs
        .iter()
        .map(|(d, r)| [0, ANSWERS].map(|reps| lens_requests_input(d, r, reps)))
        .collect();
    let request_entry = entry("lsp_lens_requests");
    let (mut rust, mut basis, mut fixed, mut full) =
        (Duration::MAX, Duration::MAX, Duration::MAX, Duration::MAX);
    for _ in 0..3 {
        rust = rust.min(time(|| {
            for (d, r) in &lens_docs {
                std::hint::black_box(rust_lens_requests(d, r, ANSWERS));
            }
        }));
        basis = basis.min(time(|| {
            for (d, r) in &lens_docs {
                std::hint::black_box(rust_lens_requests(d, r, 0));
            }
        }));
        fixed = fixed.min(time(|| {
            for [none, _] in &request_inputs {
                std::hint::black_box(request_entry.text(none));
            }
        }));
        full = full.min(time(|| {
            for [_, all] in &request_inputs {
                std::hint::black_box(request_entry.text(all));
            }
        }));
    }
    let n = lens_docs.len() as u32 * ANSWERS as u32;
    let (r, v) = (
        rust.saturating_sub(basis) / n,
        full.saturating_sub(fixed) / n,
    );
    eprintln!(
        "lens requests per document ({} x {ANSWERS}), on one view: rust {r:?} vesper {v:?} \
         ratio {:.2}",
        lens_docs.len(),
        v.as_secs_f64() / r.as_secs_f64()
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
fn preludes() -> [&'static str; 6] {
    [
        vesper::strings::PRELUDE,
        vesper::atomics::PRELUDE,
        vesper::atomics::WORKERS_PRELUDE,
        vesper::trace_effects::PRELUDE,
        vesper::trace_effects::TELEMETRABLE_PRELUDE,
        vesper::option_methods::PRELUDE,
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

// ------------------------------------------------------------ lens facts

/// `lsp_lens_facts`: the checker's facts, the callee table
/// (`World::functions` and `World::effects` with their parameter passes),
/// the cursors and the source text.
fn vesper_lens_facts(s: &CheckedSource, cursors: &[usize], reps: u64) -> String {
    let checked = s.checked();
    let mut world = checked.world.clone();
    world.types = checked.expression_types.clone();
    let pass = |p: vesper::ast::Pass| match p {
        vesper::ast::Pass::In => 0u64,
        vesper::ast::Pass::Inout => 1,
        vesper::ast::Pass::Sink => 2,
    };
    let mut names = String::new();
    let input = encode(&world, |_, tail| {
        tail.push(world.functions.len() as u64);
        for (name, f) in &world.functions {
            tail.extend([names.len() as u64, name.len() as u64]);
            names.push_str(name);
            tail.push(f.params.len() as u64);
            tail.push(f.params.first().is_some_and(|p| p.name == "self") as u64);
            tail.extend(f.params.iter().map(|p| pass(p.pass)));
        }
        tail.push(world.effects.len() as u64);
        for (name, e) in &world.effects {
            tail.extend([names.len() as u64, name.len() as u64]);
            names.push_str(name);
            tail.push(e.ops.len() as u64);
            for op in &e.ops {
                tail.extend([names.len() as u64, op.name.len() as u64]);
                names.push_str(&op.name);
                tail.push(op.params.len() as u64);
                tail.extend(op.params.iter().map(|p| pass(p.pass)));
            }
        }
        tail.push(cursors.len() as u64);
        tail.extend(cursors.iter().map(|&c| c as u64));
        tail.push(s.source().len() as u64);
        tail.push(names.len() as u64);
        tail.push(reps);
    });
    let mut input = input;
    input.extend_from_slice(s.source().as_bytes());
    input.extend_from_slice(names.as_bytes());
    entry("lsp_lens_facts").text(&input)
}

/// Cursors for `View::outstanding`: every offset a fact names and its
/// neighbours, and a stride over the whole source.
fn lens_cursors(source: &str, facts: &str) -> Vec<usize> {
    let mut at = std::collections::BTreeSet::new();
    for line in facts.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let numbers: &[&str] = match fields.first() {
            Some(&"F") | Some(&"E") => &fields[1..],
            Some(&"B") if fields.len() > 7 => &fields[2..8],
            _ => &[],
        };
        for n in numbers {
            if let Ok(v) = n.parse::<usize>() {
                at.extend([v.saturating_sub(1), v, v + 1]);
            }
        }
    }
    at.extend((0..=source.len()).step_by(29));
    at.into_iter().collect()
}

/// The ownership facts behind every Lens view (Rust `lens::View::checked`
/// and `View::outstanding`) against the Vesper port, on every corpus
/// document.
#[test]
fn lens_facts_match_rust() {
    let (mut docs, mut functions, mut bindings, mut linear, mut events, mut cursors) =
        (0, 0, 0, 0, 0, 0);
    let (mut unavailable, mut diffs) = (0, 0);
    for d in corpus_docs() {
        let facts = vesper::lsp::lens_facts_for_port(d.checked.clone(), &[]);
        let at = lens_cursors(d.checked.source(), &facts);
        let rust = vesper::lsp::lens_facts_for_port(d.checked.clone(), &at);
        let port = vesper_lens_facts(&d.checked, &at, 1);
        docs += 1;
        for line in rust.lines() {
            match line.split('\t').next() {
                Some("F") => functions += 1,
                Some("B") => {
                    bindings += 1;
                    if line.split('\t').nth(4) == Some("1") {
                        linear += 1;
                    }
                }
                Some("E") => events += 1,
                Some("O") => cursors += 1,
                Some("U") => unavailable += 1,
                _ => {}
            }
        }
        if std::env::var("LENS_SHOW").is_ok() && d.uri.starts_with("untitled:lens") {
            eprintln!("{}:\n{rust}", d.name);
        }
        if rust != port {
            diffs += 1;
            if diffs <= 6 {
                let (r, v): (Vec<_>, Vec<_>) = (rust.lines().collect(), port.lines().collect());
                let i = r
                    .iter()
                    .zip(&v)
                    .position(|(a, b)| a != b)
                    .unwrap_or(r.len().min(v.len()));
                eprintln!(
                    "{} line {i}:\n  rust   {:?}\n  vesper {:?}",
                    d.name,
                    r.get(i),
                    v.get(i)
                );
            }
        }
    }
    eprintln!(
        "lens facts: {docs} documents ({unavailable} without facts), {functions} functions, \
         {bindings} bindings ({linear} linear), {events} consumptions, {cursors} cursors, {diffs} differ"
    );
    assert_eq!(diffs, 0);
}

// ------------------------------------------------------------ lens repair

/// `lsp_lens_repair`: the recovery parse's declarations, sites and
/// diagnostics, the limits and the source.
fn vesper_lens_repair(
    syntax: &vesper_syntax::recovery::RecoverySyntax,
    limits: AnalysisLimits,
    reps: u64,
) -> String {
    use vesper_syntax::recovery::{DeclarationKind as D, DeclarationStatus as S};
    let none = u64::MAX;
    let mut words: Vec<u64> = vec![syntax.declarations().len() as u64];
    for d in syntax.declarations() {
        words.push(match d.kind {
            D::Function => 1,
            D::Impl => 2,
            D::Struct => 3,
            D::Enum => 4,
            D::Effect => 5,
            _ => 0,
        });
        words.push(match d.status {
            S::Complete => 0,
            S::Recovered => 1,
            S::Opaque => 2,
        });
        words.extend([d.span.start as u64, d.span.end as u64]);
        match &d.name_span {
            Some(n) => words.extend([n.start as u64, n.end as u64]),
            None => words.extend([none, none]),
        }
    }
    words.push(syntax.sites().len() as u64);
    for site in syntax.sites() {
        words.push(site.owner.map_or(none, |o| o as u64));
        words.extend([site.span.start as u64, site.span.end as u64]);
    }
    words.push(syntax.diagnostics().len() as u64);
    for d in syntax.diagnostics() {
        words.extend([d.span.start as u64, d.span.end as u64]);
    }
    words.extend([
        limits.max_source_bytes as u64,
        limits.max_diagnostics as u64,
        limits.max_depth as u64,
        reps,
        syntax.source().len() as u64,
    ]);
    let mut input: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    input.extend_from_slice(syntax.source().as_bytes());
    entry("lsp_lens_repair").text(&input)
}

/// Syntax-damaged variants of a source: each mutation at a few statement
/// ends inside its bodies (an unfinished `let`, a member access, an open
/// call), a truncated prefix, a broken parameter list and an unfinished
/// struct member.
fn damaged_variants(source: &str) -> Vec<String> {
    let mut out = vec![];
    let ends: Vec<usize> = source.match_indices(";\n").map(|(i, _)| i + 2).collect();
    let step = (ends.len() / 3).max(1);
    for (k, &at) in ends.iter().step_by(step).take(4).enumerate() {
        let insert = [
            "    let broken = \n",
            "    x.\n",
            "    call(1, \n",
            "    if x {\n",
        ][k % 4];
        out.push(format!("{}{insert}{}", &source[..at], &source[at..]));
        out.push(source[..at].to_string());
    }
    if let Some(f) = source.find("fn ")
        && let Some(close) = source[f..].find(')')
    {
        let at = f + close;
        out.push(format!("{}{}", &source[..at], &source[at + 1..]));
    }
    if let Some(s) = source.find("struct ")
        && let Some(close) = source[s..].find('}')
    {
        let at = s + close;
        out.push(format!("{} oops: {}", &source[..at], &source[at..]));
    }
    out
}

/// The damaged sources: Lens's own scenarios and variants of every corpus
/// document.
fn damaged_docs() -> Vec<(String, String)> {
    let mut out = vec![];
    for (name, body) in [
        ("member", "    let tx = open_txn(1);\n    tx.\n"),
        (
            "let",
            "    let tx = open_txn(1);\n    let other = open_txn(2);\n    let n = \n",
        ),
        (
            "nested",
            "    let tx = open_txn(1);\n    if tx.slot() > 0 {\n        let y = tx.\n",
        ),
        (
            "arm",
            "    let tx = open_txn(1);\n    match 1 {\n        0 => tx.commit(),\n        _ => tx.\n",
        ),
    ] {
        out.push((
            format!("lens_damaged_{name}"),
            format!("{LENS_PREFIX}\nfn main() -> u64 {{\n{body}}}\n"),
        ));
    }
    out.push((
        "lens_damaged_impl".into(),
        LENS_PREFIX.replace(
            "fn slot(self) -> u64 {\n        self.wal_slot",
            "fn slot(self) -> u64 {\n        self.",
        ),
    ));
    out.push((
        "lens_damaged_header".into(),
        LENS_PREFIX.replace(
            "fn open_txn(slot: u64) -> WriteTxn {",
            "fn open_txn(slot: u64, -> WriteTxn {",
        ),
    ));
    out.push((
        "lens_damaged_struct".into(),
        LENS_PREFIX.replace("    wal_slot: u64\n}", "    wal_slot: u64,\n    extra:\n}"),
    ));
    for d in corpus_docs() {
        for (i, v) in damaged_variants(d.checked.source()).into_iter().enumerate() {
            out.push((format!("{}#{i}", d.name), v));
        }
    }
    out
}

/// The checker's facts on a Lens view's text, as `View::from_text` gets
/// them: the editor guard's outcome (0 clean, 1 semantic errors, 2 the
/// text does not parse, 3 a resource limit, 4 another guard), then the
/// module's `check::check`.
fn view_basis(text: &str, limits: AnalysisLimits) -> (u64, Option<vesper::check::Checked>) {
    let status = match check_source(text, limits) {
        Ok(_) => 0,
        Err(AnalysisFailure::Semantic { .. }) => 1,
        Err(AnalysisFailure::Parse { .. }) => return (2, None),
        Err(AnalysisFailure::Resource { .. }) => return (3, None),
        Err(_) => return (4, None),
    };
    let parsed = match vesper::parser::parse_with_limits(
        text,
        vesper::parser::ParseLimits {
            max_source_bytes: limits.max_source_bytes,
            max_tokens: 16_384,
            max_diagnostics: limits.max_diagnostics,
            max_depth: limits.max_depth,
        },
    ) {
        Ok(parsed) => parsed,
        Err(_) => return (3, None),
    };
    if !parsed.diagnostics.is_empty() {
        return (2, None);
    }
    (status, Some(vesper::check::check(&parsed.module)))
}

/// Requests against one Lens view.
struct ViewRequests {
    cursors: Vec<usize>,
    bindings: Vec<(String, usize)>,
    members: Vec<(String, usize)>,
    ceiling: Option<std::collections::BTreeSet<String>>,
}

/// The Vesper view (`lsp_lens_view`) of `text`, whose regions are the `G`
/// rows of a repair dump.
fn vesper_lens_view(
    text: &str,
    regions: &str,
    limits: AnalysisLimits,
    q: &ViewRequests,
    reps: u64,
) -> String {
    entry("lsp_lens_view").text(&lens_view_input(text, regions, limits, q, reps))
}

/// The input of `lsp_lens_view`.
fn lens_view_input(
    text: &str,
    regions: &str,
    limits: AnalysisLimits,
    q: &ViewRequests,
    reps: u64,
) -> Vec<u8> {
    lens_view_input_basis(text, regions, limits, q, reps, false)
}

/// The input of `lsp_lens_view`; with `snapshot`, of the view of the
/// text's checked snapshot (`View::checked`: status 5, or the editor
/// guard's refusal when the text does not check).
fn lens_view_input_basis(
    text: &str,
    regions: &str,
    limits: AnalysisLimits,
    q: &ViewRequests,
    reps: u64,
    snapshot: bool,
) -> Vec<u8> {
    let (status, checked) = view_basis(text, limits);
    let status = match (snapshot, status) {
        (false, status) => status,
        (true, 0) => 5,
        (true, _) => 4,
    };
    let mut world = vesper::check::World::default();
    let mut diags = vec![];
    if let Some(checked) = &checked {
        world = checked.world.clone();
        world.types = checked.expression_types.clone();
        diags = checked.diagnostics.iter().map(|d| d.span.clone()).collect();
    }
    let pass = |p: vesper::ast::Pass| match p {
        vesper::ast::Pass::In => 0u64,
        vesper::ast::Pass::Inout => 1,
        vesper::ast::Pass::Sink => 2,
    };
    let mut names = String::new();
    let blob = |names: &mut String, text: &str| -> [u64; 2] {
        let at = names.len() as u64;
        names.push_str(text);
        [at, text.len() as u64]
    };
    let input = encode(&world, |e, tail| {
        tail.push(world.functions.len() as u64);
        for (name, f) in &world.functions {
            tail.extend(blob(&mut names, name));
            tail.push(f.params.len() as u64);
            tail.push(f.params.first().is_some_and(|p| p.name == "self") as u64);
            tail.extend(f.params.iter().map(|p| pass(p.pass)));
        }
        tail.push(world.effects.len() as u64);
        for (name, d) in &world.effects {
            tail.extend(blob(&mut names, name));
            tail.push(d.ops.len() as u64);
            for op in &d.ops {
                tail.extend(blob(&mut names, &op.name));
                tail.push(op.params.len() as u64);
                tail.extend(op.params.iter().map(|p| pass(p.pass)));
            }
        }
        for f in world.functions.values() {
            for p in &f.params {
                tail.extend(blob(&mut names, &p.name));
                tail.push(e.ty(&p.ty));
            }
            tail.push(e.ty(&f.result));
            tail.push(f.effects.instances.len() as u64);
            for i in &f.effects.instances {
                tail.extend(blob(&mut names, &i.name));
                tail.push(i.args.len() as u64);
                for a in &i.args {
                    tail.push(e.ty(a));
                }
            }
        }
        tail.extend(world.effects.values().map(|d| d.linear as u64));
        tail.push(status);
        let rows: Vec<Vec<&str>> = regions
            .lines()
            .take_while(|l| *l != "T")
            .filter(|l| l.starts_with("G\t"))
            .map(|l| l.split('\t').collect())
            .collect();
        tail.push(rows.len() as u64);
        for r in &rows {
            let n = |i: usize| r[i].parse::<u64>().unwrap();
            tail.push(match r[2] {
                "body" => 0,
                "header" => 1,
                _ => 2,
            });
            tail.extend([n(3), n(5), n(6), n(7)]);
        }
        tail.push(diags.len() as u64);
        for d in &diags {
            tail.extend([d.start as u64, d.end as u64]);
        }
        tail.push(q.cursors.len() as u64);
        tail.extend(q.cursors.iter().map(|&c| c as u64));
        for list in [&q.bindings, &q.members] {
            tail.push(list.len() as u64);
            for (name, at) in list {
                tail.extend(blob(&mut names, name));
                tail.push(*at as u64);
            }
        }
        tail.push(q.ceiling.is_some() as u64);
        let ceiling: Vec<&String> = q.ceiling.iter().flatten().collect();
        tail.push(ceiling.len() as u64);
        for c in ceiling {
            tail.extend(blob(&mut names, c));
        }
        tail.push(text.len() as u64);
        tail.push(names.len() as u64);
        tail.push(reps);
    });
    let mut input = input;
    input.extend_from_slice(text.as_bytes());
    input.extend_from_slice(names.as_bytes());
    input
}

/// The Rust view (`lens::View::from_text` and its queries).
fn rust_lens_view(text: &str, regions: &str, limits: AnalysisLimits, q: &ViewRequests) -> String {
    vesper::lsp::lens_view_for_port(
        text,
        regions,
        limits,
        &q.cursors,
        &q.bindings,
        &q.members,
        q.ceiling.as_ref(),
    )
}

/// Requests for a view: cursors as for `lens_facts`, and at each binding's
/// origin end, scope end and the cuts, its name for `binding` and
/// `complete_member` (with a name nothing binds); the capability ceiling
/// grants every other effect the text declares (none when it declares one),
/// or is absent.
fn view_requests(text: &str, regions: &str, facts: &str, ceiling: bool) -> ViewRequests {
    let mut cursors = lens_cursors(text, facts);
    let cuts: Vec<usize> = regions
        .lines()
        .filter(|l| l.starts_with("G\t"))
        .filter_map(|l| l.split('\t').nth(7)?.parse().ok())
        .collect();
    for &c in &cuts {
        cursors.extend([c.saturating_sub(1), c, c + 1, c + 5]);
    }
    cursors.sort_unstable();
    cursors.dedup();
    let mut bindings = vec![];
    for line in facts.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.first() != Some(&"B") || f.len() < 8 {
            continue;
        }
        let n = |i: usize| f[i].parse::<usize>().unwrap_or(0);
        for at in [n(3), n(7), n(7) + 1] {
            bindings.push((f[1].to_string(), at));
        }
        for &c in &cuts {
            bindings.push((f[1].to_string(), c + 3));
        }
    }
    bindings.truncate(400);
    bindings.push(("lens_unbound".into(), text.len() / 2));
    let members = bindings.clone();
    let ceiling = ceiling.then(|| {
        let mut set = std::collections::BTreeSet::new();
        for (i, d) in text.match_indices("effect ").enumerate() {
            let name: String = text[d.0 + 7..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if i % 2 == 1 && !name.is_empty() {
                set.insert(name);
            }
        }
        set
    });
    ViewRequests {
        cursors,
        bindings,
        members,
        ceiling,
    }
}

/// Sources with semantic errors: each corpus document without one of a
/// few of its statements, and Lens scenarios with an unconsumed or
/// double-consumed transaction.
fn semantic_docs() -> Vec<(String, String)> {
    let mut out = vec![];
    for (name, body) in [
        ("leak", "    let tx = open_txn(1);\n    tx.slot()\n"),
        (
            "twice",
            "    let tx = open_txn(1);\n    let a = tx.commit();\n    tx.commit()\n",
        ),
        (
            "blocked",
            "    let tx = open_txn(1);\n    let other = open_txn(2);\n    let s = tx.persist();\n    other.commit() + tx.commit()\n",
        ),
        ("unknown", "    let tx = open_txn(1);\n    missing(tx)\n"),
    ] {
        out.push((
            format!("lens_semantic_{name}"),
            format!("{LENS_PREFIX}\nfn main() -> u64 eff [Storage] {{\n{body}}}\n"),
        ));
    }
    for d in corpus_docs() {
        let source = d.checked.source();
        let ends: Vec<usize> = source.match_indices(";\n").map(|(i, _)| i + 2).collect();
        let step = (ends.len() / 3).max(1);
        for (k, w) in ends.windows(2).step_by(step).take(3).enumerate() {
            out.push((
                format!("{}~{k}", d.name),
                format!("{}{}", &source[..w[0]], &source[w[1]..]),
            ));
        }
    }
    out
}

/// The Lens views the tests ask for: every corpus document, the
/// semantic-error sources, and each damaged source's repaired prefix (text
/// and repair dump `G` rows).
fn lens_views() -> Vec<(String, String, String)> {
    let limits = lens_limits();
    let mut views: Vec<(String, String, String)> = vec![];
    for d in corpus_docs() {
        views.push((
            d.name.clone(),
            d.checked.source().to_string(),
            String::new(),
        ));
    }
    for (name, source) in semantic_docs() {
        views.push((name, source, String::new()));
    }
    for (name, source) in damaged_docs() {
        let Ok(syntax) = vesper_syntax::parser::parse_recovery(
            &source,
            vesper_syntax::recovery::Limits::default(),
        ) else {
            continue;
        };
        let dump = vesper::lsp::lens_repair_for_port(&syntax, limits);
        if let Some((regions, text)) = dump.split_once("T\n") {
            views.push((name, text.to_string(), regions.to_string()));
        }
    }
    views
}

/// Every Lens view (Rust `View::from_text`: semantic-error sources and the
/// repaired prefixes of damaged ones) against the Vesper port: scope,
/// absorbed diagnostics, facts, live obligations read at the cut, the
/// poisoned bindings, resolved names and ranked member completions, with
/// and without a capability ceiling.
#[test]
fn lens_view_matches_rust() {
    let limits = lens_limits();
    let views = lens_views();
    let mut scopes: BTreeMap<String, usize> = BTreeMap::new();
    let mut tiers: BTreeMap<String, usize> = BTreeMap::new();
    let (mut rows, mut poisoned, mut completions, mut diffs) = (0, 0, 0, 0);
    for (name, text, regions) in &views {
        let bare = ViewRequests {
            cursors: vec![],
            bindings: vec![],
            members: vec![],
            ceiling: None,
        };
        let facts = rust_lens_view(text, regions, limits, &bare);
        for ceiling in [false, true] {
            let q = view_requests(text, regions, &facts, ceiling);
            let rust = rust_lens_view(text, regions, limits, &q);
            let port = vesper_lens_view(text, regions, limits, &q, 1);
            if !ceiling {
                let scope = rust.lines().next().unwrap_or("").to_string();
                *scopes.entry(scope).or_default() += 1;
            }
            for line in rust.lines() {
                rows += 1;
                match line.split('\t').next() {
                    Some("P") => poisoned += 1,
                    Some("M") => {
                        completions += 1;
                        let tier = line.split('\t').nth(1).unwrap_or("");
                        *tiers.entry(tier.to_string()).or_default() += 1;
                    }
                    _ => {}
                }
            }
            if rust != port {
                diffs += 1;
                if diffs <= 6 {
                    let (r, v): (Vec<_>, Vec<_>) = (rust.lines().collect(), port.lines().collect());
                    let i = r
                        .iter()
                        .zip(&v)
                        .position(|(a, b)| a != b)
                        .unwrap_or(r.len().min(v.len()));
                    eprintln!(
                        "{name} (ceiling {ceiling}) line {i}:\n  rust   {:?}\n  vesper {:?}",
                        r.get(i),
                        v.get(i)
                    );
                }
            }
        }
    }
    eprintln!(
        "lens views: {} views, {rows} rows ({poisoned} poisoned sets, {completions} ranked \
         completions: {tiers:?}), scopes {scopes:?}, {diffs} differ",
        views.len()
    );
    assert_eq!(diffs, 0);
}

/// A document the Lens request layer answers on: its text, the text and
/// repair-dump regions of its view, and whether that view is the text's
/// checked snapshot's (`View::checked`).
struct LensDoc {
    name: String,
    doc: String,
    view_text: String,
    regions: String,
    checked: bool,
}

const LENS_URI: &str = "file:///lens/main.ves";

/// Every corpus document (checked), the semantic-error sources, and each
/// damaged source with its repaired prefix.
fn lens_documents() -> Vec<LensDoc> {
    let mut out = vec![];
    for d in corpus_docs() {
        let doc = d.checked.source().to_string();
        out.push(LensDoc {
            name: d.name.clone(),
            view_text: doc.clone(),
            doc,
            regions: String::new(),
            checked: true,
        });
    }
    for (name, doc) in semantic_docs() {
        out.push(LensDoc {
            name,
            view_text: doc.clone(),
            doc,
            regions: String::new(),
            checked: false,
        });
    }
    for (name, doc) in damaged_docs() {
        let Ok(syntax) =
            vesper_syntax::parser::parse_recovery(&doc, vesper_syntax::recovery::Limits::default())
        else {
            continue;
        };
        let dump = vesper::lsp::lens_repair_for_port(&syntax, lens_limits());
        if let Some((regions, text)) = dump.split_once("T\n") {
            out.push(LensDoc {
                name,
                doc,
                view_text: text.to_string(),
                regions: regions.to_string(),
                checked: false,
            });
        }
    }
    out
}

/// Requests on one document.
struct LensRequests {
    completions: Vec<Position>,
    hovers: Vec<Position>,
    inlays: Vec<vesper_span::Range>,
    ceiling: Option<std::collections::BTreeSet<String>>,
}

/// Completion just after each `.` and one character later, hovers at
/// evenly spaced characters, inlay hints over the whole document (and past
/// its end), its first half and its second half; each list also has a
/// position past the last line.
fn lens_requests(doc: &str, ceiling: bool) -> LensRequests {
    let index = LineIndex::new(Arc::from(doc), index_limits()).expect("line index");
    let at = |offset: usize| index.position(offset).expect("char boundary");
    // Offsets with a position (not inside a CRLF pair).
    let bounds: Vec<usize> = doc
        .char_indices()
        .map(|(i, _)| i)
        .chain([doc.len()])
        .filter(|&i| index.position(i).is_ok())
        .collect();
    let far = Position {
        line: 10_000,
        character: 0,
    };
    let mut completions = vec![];
    for (i, _) in doc.match_indices('.') {
        if !bounds.contains(&(i + 1)) {
            continue;
        }
        completions.push(at(i + 1));
        if let Some(&next) = bounds.iter().find(|&&b| b > i + 1) {
            completions.push(at(next));
        }
    }
    completions.truncate(96);
    completions.push(far);
    let step = (bounds.len() / 48).max(1);
    let mut hovers: Vec<Position> = bounds.iter().step_by(step).map(|&b| at(b)).collect();
    hovers.push(far);
    let lines = doc.lines().count() as u32;
    let half = bounds[bounds.len() / 2];
    let start = Position {
        line: 0,
        character: 0,
    };
    let end = Position {
        line: lines + 3,
        character: 0,
    };
    let inlays = vec![
        vesper_span::Range { start, end },
        vesper_span::Range {
            start,
            end: at(half),
        },
        vesper_span::Range {
            start: at(half),
            end: at(doc.len()),
        },
        vesper_span::Range { start: far, end },
    ];
    let ceiling = ceiling.then(|| {
        let mut set = std::collections::BTreeSet::new();
        for (i, d) in doc.match_indices("effect ").enumerate() {
            let name: String = doc[d.0 + 7..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if i % 2 == 1 && !name.is_empty() {
                set.insert(name);
            }
        }
        set
    });
    LensRequests {
        completions,
        hovers,
        inlays,
        ceiling,
    }
}

/// The Rust request layer (`requests_dump`) on one view, `rounds` times.
fn rust_lens_requests(d: &LensDoc, r: &LensRequests, rounds: u64) -> String {
    vesper::lsp::lens_requests_for_port(
        LENS_URI,
        &d.doc,
        &d.view_text,
        &d.regions,
        d.checked,
        lens_limits(),
        index_limits(),
        &r.completions,
        &r.hovers,
        &r.inlays,
        r.ceiling.as_ref(),
        rounds,
    )
}

/// The input of `lsp_lens_requests`.
fn lens_requests_input(d: &LensDoc, r: &LensRequests, reps: u64) -> Vec<u8> {
    let q = ViewRequests {
        cursors: vec![],
        bindings: vec![],
        members: vec![],
        ceiling: r.ceiling.clone(),
    };
    let mut input =
        lens_view_input_basis(&d.view_text, &d.regions, lens_limits(), &q, reps, d.checked);
    index_words(&mut input, index_limits());
    push_word(&mut input, LENS_URI.len() as u64);
    push_word(&mut input, d.doc.len() as u64);
    let rows: Vec<Vec<&str>> = d
        .regions
        .lines()
        .take_while(|l| *l != "T")
        .filter(|l| l.starts_with("G\t"))
        .map(|l| l.split('\t').collect())
        .collect();
    let mut names = String::new();
    for row in &rows {
        let n = |i: usize| row[i].parse::<u64>().unwrap();
        names.push_str(row[1]);
        for w in [row[1].len() as u64, n(8), n(4), n(9), n(10)] {
            push_word(&mut input, w);
        }
    }
    for list in [&r.completions, &r.hovers] {
        push_word(&mut input, list.len() as u64);
        for p in list.iter() {
            push_word(&mut input, p.line as u64);
            push_word(&mut input, p.character as u64);
        }
    }
    push_word(&mut input, r.inlays.len() as u64);
    for range in &r.inlays {
        for w in [
            range.start.line,
            range.start.character,
            range.end.line,
            range.end.character,
        ] {
            push_word(&mut input, w as u64);
        }
    }
    input.extend_from_slice(LENS_URI.as_bytes());
    input.extend_from_slice(d.doc.as_bytes());
    input.extend_from_slice(names.as_bytes());
    input
}

/// The Lens request layer on every Lens document (checked snapshots,
/// semantic-error sources and repaired prefixes of damaged ones), with and
/// without a capability ceiling, against the Vesper port: poison notices
/// and Lens metadata, completion items, obligation hover text and inlay
/// hints, as JSON.
#[test]
fn lens_requests_match_rust() {
    let docs = lens_documents();
    let entry = entry("lsp_lens_requests");
    let (mut rows, mut items, mut hovers, mut hints, mut notices, mut errors, mut diffs) =
        (0, 0, 0, 0, 0, 0, 0);
    for d in &docs {
        for ceiling in [false, true] {
            let r = lens_requests(&d.doc, ceiling);
            let rust = rust_lens_requests(d, &r, 1);
            let port = entry.text(&lens_requests_input(d, &r, 1));
            for line in rust.lines() {
                rows += 1;
                let (kind, body) = line.split_once('\t').unwrap_or((line, ""));
                if body.starts_with("{\"error\"") {
                    errors += 1;
                    continue;
                }
                match kind {
                    "D" => notices += body.matches("VESPER_POISONED_RECOVERY").count(),
                    "C" => items += body.matches("\"sortText\"").count(),
                    "H" if body != "null" => hovers += 1,
                    "I" => hints += body.matches("\"paddingLeft\"").count(),
                    _ => {}
                }
            }
            if rust != port {
                diffs += 1;
                if diffs <= 6 {
                    let (a, b): (Vec<_>, Vec<_>) = (rust.lines().collect(), port.lines().collect());
                    let i = a
                        .iter()
                        .zip(&b)
                        .position(|(x, y)| x != y)
                        .unwrap_or(a.len().min(b.len()));
                    eprintln!(
                        "{} (ceiling {ceiling}) line {i}:\n  rust   {:?}\n  vesper {:?}",
                        d.name,
                        a.get(i),
                        b.get(i)
                    );
                }
            }
        }
    }
    eprintln!(
        "lens requests: {} documents, {rows} answers ({notices} poison notices, {items} \
         completion items, {hovers} obligation hovers, {hints} inlay hints, {errors} errors), \
         {diffs} differ",
        docs.len()
    );
    assert_eq!(diffs, 0);
}

/// The repaired view of every damaged source (Rust `lens::repair`) against
/// the Vesper port: regions and repaired text, or the reason there is none.
#[test]
fn lens_repair_matches_rust() {
    let limits = lens_limits();
    let (mut docs, mut repaired, mut regions, mut unavailable, mut failed) = (0, 0, 0, 0, 0);
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut diffs = 0;
    for (name, source) in damaged_docs() {
        let Ok(syntax) = vesper_syntax::parser::parse_recovery(
            &source,
            vesper_syntax::recovery::Limits::default(),
        ) else {
            failed += 1;
            continue;
        };
        docs += 1;
        let rust = vesper::lsp::lens_repair_for_port(&syntax, limits);
        let port = vesper_lens_repair(&syntax, limits, 1);
        if let Some(reason) = rust.strip_prefix("U\t") {
            unavailable += 1;
            *reasons.entry(reason.trim().to_string()).or_default() += 1;
        } else {
            repaired += 1;
            regions += rust.lines().take_while(|l| l.starts_with("G\t")).count();
        }
        if std::env::var("LENS_SHOW").is_ok() && name.starts_with("lens_damaged") {
            eprintln!("{name}:\n{rust}");
        }
        if rust != port {
            diffs += 1;
            if diffs <= 6 {
                let (r, v): (Vec<_>, Vec<_>) = (rust.lines().collect(), port.lines().collect());
                let i = r
                    .iter()
                    .zip(&v)
                    .position(|(a, b)| a != b)
                    .unwrap_or(r.len().min(v.len()));
                eprintln!(
                    "{name} line {i}:\n  rust   {:?}\n  vesper {:?}",
                    r.get(i),
                    v.get(i)
                );
            }
        }
    }
    eprintln!(
        "lens repair: {docs} sources ({failed} without a recovery parse), {repaired} repaired \
         ({regions} regions), {unavailable} unavailable {reasons:?}, {diffs} differ"
    );
    assert_eq!(diffs, 0);
}

// ---------------------------------------------------- proof code lenses

const PROOF_URI: &str = "file:///proof/lens \"main\".ves";

/// One proof-lens session: a corpus document's lens functions and models
/// (as the session reads them) and finished jobs against ledgers of the
/// given capacities.
struct ProofLensCase {
    name: String,
    text: String,
    version: i32,
    functions: Vec<(String, usize)>,
    models: Vec<(String, Result<[u8; 32], String>)>,
    capacities: (usize, usize),
    ops: Vec<vesper::lsp::ProofLensOp>,
}

fn proof_op(kind: u8, source: [u8; 32]) -> vesper::lsp::ProofLensOp {
    vesper::lsp::ProofLensOp {
        kind,
        source,
        current: source,
        generation: 0,
        requested: None,
        extraction: vec![],
        semantic: vec![],
        reused: 0,
        compiled: 0,
        elapsed_ms: 0,
    }
}

/// Outcomes of every kind: kernel-checked, native-checked, refuted,
/// failed at the prover or kernel (kept for models) and at the toolchain
/// (dropped for models), and not attempted, with texts that need escapes.
fn proof_status(i: usize) -> vesper::lsp::PortProofStatus {
    let s = |code: u8, a: &str, b: &str| (code, a.to_string(), b.to_string());
    match i % 8 {
        0 => s(1, "kernel replay: 0 axioms", ""),
        1 => s(2, "bv_decide \"native\" (Lean.ofReduceBool)", ""),
        2 => s(3, "a = 255, b = 1 \u{27c2} \\ \t", ""),
        3 => s(4, "prove", "omega could not prove the goal:\n  a + b < 256"),
        4 => s(4, "lean", "lean exited with signal 9 (SIGKILL)"),
        5 => s(4, "kernel", "uses axiom sorryAx \u{1}"),
        6 => s(0, "", ""),
        _ => s(4, "cache", "permission denied"),
    }
}

fn proof_lens_cases() -> Vec<ProofLensCase> {
    let other = *blake3::hash(b"an edited buffer").as_bytes();
    let stale = *blake3::hash(b"a model of an older body").as_bytes();
    let mut cases = vec![];
    for (i, d) in corpus_docs().iter().enumerate() {
        let (mut functions, mut models) = vesper::lsp::proof_lens_inputs_for_port(&d.checked);
        if functions.is_empty() {
            continue;
        }
        let text = d.checked.source().to_string();
        let source = d.checked.source_hash();
        let capacities = match i % 3 {
            0 => (1024, 4096),
            1 => (3, 2),
            _ => (1, 1),
        };
        if i % 3 == 2 {
            // A reason past the title's 80 characters, multi-byte at the cut.
            let reason = format!("{}\u{e9}\u{1f600} model unavailable", "x".repeat(78));
            let first = functions[0].0.clone();
            match models.iter_mut().find(|(name, _)| *name == first) {
                Some(m) => m.1 = Err(reason),
                None => {
                    models.push((first, Err(reason)));
                    models.sort_by(|a, b| a.0.cmp(&b.0));
                }
            }
        }
        if i % 7 == 0 {
            functions.push(("past_the_end".into(), text.len() + 1));
        }
        let keyed: Vec<(String, [u8; 32])> = models
            .iter()
            .filter_map(|(name, m)| m.as_ref().ok().map(|k| (name.clone(), *k)))
            .collect();
        let first = functions[0].0.clone();
        let second = functions.get(1).unwrap_or(&functions[0]).0.clone();
        let job = |kind: u8, shift: usize, requested: Option<(String, bool)>| {
            let mut op = proof_op(kind, source);
            op.requested = requested;
            op.extraction = functions
                .iter()
                .take(6)
                .enumerate()
                .map(|(k, (f, _))| (f.clone(), proof_status(i + k + shift)))
                .collect();
            op.semantic = keyed
                .iter()
                .take(6)
                .enumerate()
                .map(|(k, (f, key))| (f.clone(), *key, proof_status(i + k + shift + 2)))
                .collect();
            if let Some((f, _)) = keyed.first() {
                op.semantic.push((f.clone(), stale, proof_status(shift)));
            }
            op.reused = i % 5;
            op.compiled = shift;
            op.elapsed_ms = 1234 + (i as u64) * 17;
            op
        };
        let mut ops = vec![proof_op(b'L', source)];
        ops.push(job(b'P', 0, Some((first.clone(), false))));
        ops.push(proof_op(b'L', source));
        ops.push(job(b'P', 3, Some((second.clone(), true))));
        ops.push(job(b'P', 5, None));
        ops.push(job(b'P', 1, Some(("missing".into(), true))));
        ops.push(job(b'P', 6, Some(("missing".into(), false))));
        ops.push(proof_op(b'L', source));
        let mut background = job(b'B', 2, None);
        background.generation = 7;
        ops.push(background);
        let mut late = job(b'B', 4, None);
        late.generation = 8;
        late.current = other;
        ops.push(late);
        ops.push(proof_op(b'L', source));
        ops.push(proof_op(b'L', other));
        cases.push(ProofLensCase {
            name: d.name.clone(),
            text,
            version: if i % 2 == 0 { i as i32 } else { -(i as i32) },
            functions,
            models,
            capacities,
            ops,
        });
    }
    cases
}

fn rust_proof_lenses(c: &ProofLensCase, rounds: u64) -> String {
    vesper::lsp::proof_lenses_for_port(
        PROOF_URI,
        c.version,
        &c.text,
        index_limits(),
        &c.functions,
        &c.models,
        c.capacities,
        &c.ops,
        rounds,
    )
}

/// The input of `lsp_proof_lenses` (proof_lenses.ves `proof_lenses`).
fn proof_lenses_input(c: &ProofLensCase, rounds: u64) -> Vec<u8> {
    let mut w: Vec<u64> = vec![0];
    let mut blob: Vec<u8> = vec![];
    let mut limits = vec![];
    index_words(&mut limits, index_limits());
    w.extend(
        limits
            .chunks(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap())),
    );
    w.extend([
        PROOF_URI.len() as u64,
        c.text.len() as u64,
        c.version as i64 as u64,
        c.capacities.0 as u64,
        c.capacities.1 as u64,
        rounds,
    ]);
    blob.extend_from_slice(PROOF_URI.as_bytes());
    blob.extend_from_slice(c.text.as_bytes());
    w.push(c.functions.len() as u64);
    for (name, start) in &c.functions {
        w.extend([name.len() as u64, *start as u64]);
        blob.extend_from_slice(name.as_bytes());
    }
    w.push(c.models.len() as u64);
    for (name, model) in &c.models {
        blob.extend_from_slice(name.as_bytes());
        match model {
            Ok(key) => {
                w.extend([name.len() as u64, 1, 0]);
                blob.extend_from_slice(key);
            }
            Err(reason) => {
                w.extend([name.len() as u64, 0, reason.len() as u64]);
                blob.extend_from_slice(reason.as_bytes());
            }
        }
    }
    w.push(c.ops.len() as u64);
    for op in &c.ops {
        let (requested, name) = match &op.requested {
            None => (0, ""),
            Some((name, semantic)) => (1 + *semantic as u64, name.as_str()),
        };
        w.extend([
            op.kind as u64,
            op.generation,
            requested,
            name.len() as u64,
            op.extraction.len() as u64,
            op.semantic.len() as u64,
            op.reused as u64,
            op.compiled as u64,
            op.elapsed_ms,
        ]);
        blob.extend_from_slice(&op.source);
        blob.extend_from_slice(&op.current);
        blob.extend_from_slice(name.as_bytes());
        let status = |w: &mut Vec<u64>, blob: &mut Vec<u8>, (code, a, b): &(u8, String, String)| {
            w.extend([*code as u64, a.len() as u64, b.len() as u64]);
            blob.extend_from_slice(a.as_bytes());
            blob.extend_from_slice(b.as_bytes());
        };
        for (f, s) in &op.extraction {
            w.push(f.len() as u64);
            blob.extend_from_slice(f.as_bytes());
            status(&mut w, &mut blob, s);
        }
        for (f, key, s) in &op.semantic {
            w.push(f.len() as u64);
            blob.extend_from_slice(f.as_bytes());
            blob.extend_from_slice(key);
            status(&mut w, &mut blob, s);
        }
    }
    w[0] = w.len() as u64;
    let mut input: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
    input.extend_from_slice(&blob);
    input
}

/// Proof code lenses, `vesper.proofAudit` results and background
/// `vesper/proofStatus` notifications (Rust `code_lenses_of`, `record_of`,
/// `proof_result_of`, `proof_status_of` over `ProofLedger`s) against the
/// Vesper port, on every corpus document with proof-lens functions: empty,
/// roomy and evicting ledgers, every outcome kind, unrequested and
/// unmatched commands, stale model keys, background runs for the current
/// and an edited buffer, long multi-byte model reasons and a span past the
/// end of the document.
#[test]
fn proof_lenses_match_rust() {
    let cases = proof_lens_cases();
    let entry = entry("lsp_proof_lenses");
    let (mut lenses, mut results, mut notices, mut errors, mut diffs) = (0, 0, 0, 0, 0);
    for c in &cases {
        let rust = rust_proof_lenses(c, 1);
        let port = entry.text(&proof_lenses_input(c, 1));
        for line in rust.lines() {
            match line.split_once('\t') {
                Some((_, body)) if body.starts_with("{\"error\"") => errors += 1,
                Some(("L", body)) => {
                    lenses += body.matches("\"command\":\"vesper.proofAudit\"").count()
                }
                Some(("P", _)) => results += 1,
                Some(("B", body)) if body != "-" => notices += 1,
                _ => {}
            }
        }
        if rust != port {
            diffs += 1;
            if diffs <= 6 {
                let (a, b): (Vec<_>, Vec<_>) = (rust.lines().collect(), port.lines().collect());
                let i = a
                    .iter()
                    .zip(&b)
                    .position(|(x, y)| x != y)
                    .unwrap_or(a.len().min(b.len()));
                eprintln!(
                    "{} line {i}:\n  rust   {:?}\n  vesper {:?}",
                    c.name,
                    a.get(i),
                    b.get(i)
                );
            }
        }
    }
    eprintln!(
        "proof lenses: {} documents, {lenses} lenses, {results} command results, {notices} \
         background notices, {errors} span errors, {diffs} differ",
        cases.len()
    );
    assert!(lenses > 0 && results > 0 && notices > 0 && errors > 0);
    assert_eq!(diffs, 0);
}

/// Proof-lens sessions per document, `rounds` of every case less one
/// round's setup on both sides (`CHECK_HARNESS_SPEED=1 cargo test
/// --release --test self_host_lsp_features proof_lenses_speed --
/// --ignored`).
#[test]
#[ignore]
fn proof_lenses_speed() {
    if !speed() {
        return;
    }
    const ROUNDS: u64 = 32;
    let cases = proof_lens_cases();
    let entry = entry("lsp_proof_lenses");
    let inputs: Vec<[Vec<u8>; 2]> = cases
        .iter()
        .map(|c| [0, ROUNDS].map(|n| proof_lenses_input(c, n)))
        .collect();
    let (mut rust, mut basis, mut fixed, mut full) =
        (Duration::MAX, Duration::MAX, Duration::MAX, Duration::MAX);
    for _ in 0..3 {
        rust = rust.min(time(|| {
            for c in &cases {
                std::hint::black_box(rust_proof_lenses(c, ROUNDS));
            }
        }));
        basis = basis.min(time(|| {
            for c in &cases {
                std::hint::black_box(rust_proof_lenses(c, 0));
            }
        }));
        fixed = fixed.min(time(|| {
            for [none, _] in &inputs {
                std::hint::black_box(entry.text(none));
            }
        }));
        full = full.min(time(|| {
            for [_, all] in &inputs {
                std::hint::black_box(entry.text(all));
            }
        }));
    }
    let n = cases.len() as u32 * ROUNDS as u32;
    let (r, v) = (
        rust.saturating_sub(basis) / n,
        full.saturating_sub(fixed) / n,
    );
    eprintln!(
        "proof-lens session per document ({} x {ROUNDS}): rust {r:?} vesper {v:?} ratio {:.2}",
        cases.len(),
        v.as_secs_f64() / r.as_secs_f64()
    );
}

// ------------------------------------------- debounce and pending requests

/// One step of a Lens bookkeeping session (lens_state.ves): `kind`, three
/// arguments and a text (a URI or a string request ID). A token argument
/// past the tokens made names none; `u64::MAX` for `D_STARTED` is the token
/// made last.
#[derive(Clone)]
struct LensStateOp {
    kind: u64,
    x: u64,
    y: u64,
    z: u64,
    s: String,
}

/// The ops of lens_state.ves.
mod lens_op {
    pub const D_NEW: u64 = 1;
    pub const D_TOUCH: u64 = 2;
    pub const D_TIMER: u64 = 3;
    pub const D_STARTED: u64 = 4;
    pub const D_IS_CURRENT: u64 = 5;
    pub const D_FINISHED: u64 = 6;
    pub const D_FORGET: u64 = 7;
    pub const D_CANCEL_ALL: u64 = 8;
    pub const D_LEN: u64 = 9;
    pub const P_NEW: u64 = 10;
    pub const P_BEGIN: u64 = 11;
    pub const P_CANCEL: u64 = 12;
    pub const P_FINISH: u64 = 13;
    pub const P_IS_PENDING: u64 = 14;
    pub const P_CANCEL_ALL: u64 = 15;
    pub const P_LEN: u64 = 16;
    pub const T_NEW: u64 = 17;
    pub const T_CANCEL: u64 = 18;
}

/// `Debouncer`, `PendingRequests` and `CancelToken` over `ops`, `rounds`
/// times (the last answers, or "" for no rounds): a row per op, then the
/// cancelled flag of every token.
fn rust_lens_state(ops: &[LensStateOp], rounds: u64) -> String {
    use lens_op::*;
    use std::fmt::Write as _;
    use vesper_lsp::lens::{CancelToken, Debouncer, PendingRequests, TimerAction};
    let base = std::time::Instant::now();
    let mut out = String::new();
    for _ in 0..rounds {
        out.clear();
        let mut tokens: Vec<CancelToken> = vec![];
        let mut d: Option<Debouncer> = None;
        let mut p: Option<PendingRequests> = None;
        let action = |a: TimerAction| match a {
            TimerAction::Arm(delay) => format!("A {}", delay.as_nanos()),
            TimerAction::Idle => "I".to_string(),
            TimerAction::Fire { generation } => format!("F {generation}"),
        };
        let bit = |b: bool| if b { "1" } else { "0" };
        for op in ops {
            let at = base + Duration::from_nanos(op.x);
            let id = || {
                if op.y == 0 {
                    vesper_lsp::Id::Integer(op.x as i64 as i32)
                } else {
                    vesper_lsp::Id::String(op.s.clone())
                }
            };
            let row = match (op.kind, d.as_mut(), p.as_mut()) {
                (D_NEW, ..) => match Debouncer::new(Duration::from_nanos(op.x), op.y as usize) {
                    Ok(new) => {
                        d = Some(new);
                        ".".to_string()
                    }
                    Err(e) => {
                        d = None;
                        format!("E {e}")
                    }
                },
                (P_NEW, ..) => match PendingRequests::new(op.x as usize) {
                    Ok(new) => {
                        p = Some(new);
                        ".".to_string()
                    }
                    Err(e) => {
                        p = None;
                        format!("E {e}")
                    }
                },
                (T_NEW, ..) => {
                    tokens.push(CancelToken::new());
                    (tokens.len() - 1).to_string()
                }
                (T_CANCEL, ..) => {
                    if let Some(token) = tokens.get(op.z as usize) {
                        token.cancel();
                    }
                    ".".to_string()
                }
                (D_TOUCH..=D_LEN, None, _) | (P_BEGIN..=P_LEN, _, None) => "-".to_string(),
                (D_TOUCH, Some(d), _) => action(d.touch(&op.s, at)),
                (D_TIMER, Some(d), _) => action(d.timer(&op.s, at)),
                (D_STARTED, Some(d), _) => {
                    let token = tokens.last().cloned().unwrap_or_default();
                    d.started(&op.s, op.x, token);
                    ".".to_string()
                }
                (D_IS_CURRENT, Some(d), _) => bit(d.is_current(&op.s, op.x)).to_string(),
                (D_FINISHED, Some(d), _) => {
                    d.finished(&op.s, op.x);
                    ".".to_string()
                }
                (D_FORGET, Some(d), _) => {
                    d.forget(&op.s);
                    ".".to_string()
                }
                (D_CANCEL_ALL, Some(d), _) => {
                    d.cancel_all();
                    ".".to_string()
                }
                (D_LEN, Some(d), _) => d.len().to_string(),
                (P_BEGIN, _, Some(p)) => match p.begin(id()) {
                    Ok(token) => {
                        tokens.push(token);
                        format!("K {}", tokens.len() - 1)
                    }
                    Err(e) => format!("E {e}"),
                },
                (P_CANCEL, _, Some(p)) => bit(p.cancel(&id())).to_string(),
                (P_FINISH, _, Some(p)) => {
                    let job = tokens.get(op.z as usize).cloned().unwrap_or_default();
                    bit(p.finish(&id(), &job)).to_string()
                }
                (P_IS_PENDING, _, Some(p)) => {
                    let job = tokens.get(op.z as usize).cloned().unwrap_or_default();
                    bit(p.is_pending(&id(), &job)).to_string()
                }
                (P_CANCEL_ALL, _, Some(p)) => p
                    .cancel_all()
                    .into_iter()
                    .map(|id| match id {
                        vesper_lsp::Id::Integer(n) => format!("i{n}"),
                        vesper_lsp::Id::String(s) => format!("s{}:{s}", s.len()),
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                (P_LEN, _, Some(p)) => p.len().to_string(),
                (kind, ..) => panic!("unknown op {kind}"),
            };
            let _ = writeln!(out, "{row}");
        }
        let flags: String = tokens
            .iter()
            .map(|t| if t.is_cancelled() { '1' } else { '0' })
            .collect();
        let _ = writeln!(out, "T {flags}");
    }
    out
}

/// The input of `lsp_lens_state`.
fn lens_state_input(ops: &[LensStateOp], rounds: u64) -> Vec<u8> {
    let mut w: Vec<u64> = vec![0, rounds, ops.len() as u64];
    let mut blob = vec![];
    for op in ops {
        w.extend([op.kind, op.x, op.y, op.z, op.s.len() as u64]);
        blob.extend_from_slice(op.s.as_bytes());
    }
    w[0] = w.len() as u64;
    let mut input: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
    input.extend_from_slice(&blob);
    input
}

/// Seeded sessions: each sets its limits (some invalid), then mixes edits,
/// timers that fire early, on time and late, runs that start for current
/// and stale generations, closes, requests that begin, are cancelled, are
/// answered (by the right job and a stale one) and exit cancellation, with
/// integer and string IDs that repeat.
fn lens_state_sessions() -> Vec<Vec<LensStateOp>> {
    use lens_op::*;
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n.max(1)
    };
    let op = |kind, x, y, z, s: &str| LensStateOp {
        kind,
        x,
        y,
        z,
        s: s.to_string(),
    };
    let uris = [
        "file:///a.ves",
        "file:///b.ves",
        "file:///c%20d.ves",
        "file:///\u{e9}.ves",
        "untitled:1",
        "file:///a.ves#x",
    ];
    let ids = ["1", "", "a,b", "s3:x", "\u{1f600}", "vesper-lens-refresh-1"];
    let ms = 1_000_000u64;
    let limits = [
        (150 * ms, 4096, 1024),
        (0, 1, 1),
        (60_000 * ms, 3, 3),
        (60_000 * ms + 1, 3, 3),
        (150 * ms, 0, 0),
        (150 * ms, 4097, 1025),
        (5 * ms, 2, 2),
    ];
    let mut sessions = vec![];
    for s in 0..140 {
        let (delay, max_d, max_p) = limits[s % limits.len()];
        let mut ops = vec![op(D_NEW, delay, max_d, 0, ""), op(P_NEW, max_p, 0, 0, "")];
        let mut now = 0u64;
        let mut tokens = 0u64;
        let mut generation = [0u64; 6];
        for _ in 0..(40 + next(400)) {
            now += next(4) * next(100) * ms / 2;
            let u = next(uris.len() as u64) as usize;
            let uri = uris[u];
            let (y, x, text) = if next(2) == 0 {
                let n = [0i64, 1, -1, 7, i32::MAX as i64, i32::MIN as i64][next(6) as usize];
                (0, n as u64, "")
            } else {
                (1, 0, ids[next(ids.len() as u64) as usize])
            };
            let tok = if tokens == 0 {
                None
            } else {
                Some(next(tokens))
            };
            match next(20) {
                0..=4 => {
                    generation[u] += 1;
                    ops.push(op(D_TOUCH, now, 0, 0, uri));
                }
                5..=7 => ops.push(op(D_TIMER, now, 0, 0, uri)),
                8 => {
                    ops.push(op(T_NEW, 0, 0, 0, ""));
                    tokens += 1;
                    let g = generation[u].saturating_sub(next(2));
                    // `u64::MAX`: the token just made.
                    ops.push(op(D_STARTED, g, 0, u64::MAX, uri));
                }
                9 => ops.push(op(
                    D_IS_CURRENT,
                    generation[u].saturating_sub(next(2)),
                    0,
                    0,
                    uri,
                )),
                10 => ops.push(op(
                    D_FINISHED,
                    generation[u].saturating_sub(next(2)),
                    0,
                    0,
                    uri,
                )),
                11 => {
                    if next(4) == 0 {
                        ops.push(op(D_FORGET, 0, 0, 0, uri));
                    } else {
                        ops.push(op(D_LEN, 0, 0, 0, ""));
                    }
                }
                12..=14 => {
                    ops.push(op(P_BEGIN, x, y, 0, text));
                    tokens += 1;
                }
                15 => ops.push(op(P_CANCEL, x, y, 0, text)),
                16 => {
                    if let Some(t) = tok {
                        ops.push(op(P_FINISH, x, y, t, text));
                    }
                }
                17 => {
                    if let Some(t) = tok {
                        ops.push(op(P_IS_PENDING, x, y, t, text));
                    }
                }
                18 => match next(6) {
                    0 => ops.push(op(D_CANCEL_ALL, 0, 0, 0, "")),
                    1 => ops.push(op(P_CANCEL_ALL, 0, 0, 0, "")),
                    2 => {
                        if let Some(t) = tok {
                            ops.push(op(T_CANCEL, 0, 0, t, ""));
                        }
                    }
                    _ => ops.push(op(P_LEN, 0, 0, 0, "")),
                },
                _ => {
                    // Wait out the delay, then fire.
                    now += delay + next(3);
                    ops.push(op(D_TIMER, now, 0, 0, uri));
                }
            }
        }
        sessions.push(ops);
    }
    sessions
}

/// `Debouncer`, `PendingRequests` and `CancelToken` (crates/vesper-lsp
/// lens.rs) against the Vesper port on seeded sessions: every answer and
/// the final state of every cancel token.
#[test]
fn lens_state_matches_rust() {
    let entry = entry("lsp_lens_state");
    let sessions = lens_state_sessions();
    let (mut ops, mut fires, mut arms, mut errors, mut cancelled, mut diffs) = (0, 0, 0, 0, 0, 0);
    for (i, session) in sessions.iter().enumerate() {
        let rust = rust_lens_state(session, 1);
        let port = entry.text(&lens_state_input(session, 1));
        ops += session.len();
        fires += rust.lines().filter(|l| l.starts_with("F ")).count();
        arms += rust.lines().filter(|l| l.starts_with("A ")).count();
        errors += rust.lines().filter(|l| l.starts_with("E ")).count();
        cancelled += rust.lines().last().map_or(0, |l| l.matches('1').count());
        if rust != port {
            diffs += 1;
            if diffs <= 6 {
                let (a, b): (Vec<_>, Vec<_>) = (rust.lines().collect(), port.lines().collect());
                let k = a
                    .iter()
                    .zip(&b)
                    .position(|(x, y)| x != y)
                    .unwrap_or(a.len().min(b.len()));
                eprintln!(
                    "session {i} line {k}:\n  rust   {:?}\n  vesper {:?}",
                    a.get(k),
                    b.get(k)
                );
            }
        }
    }
    eprintln!(
        "lens state: {} sessions, {ops} ops ({arms} timers armed, {fires} fired, {errors} \
         errors, {cancelled} tokens cancelled), {diffs} differ",
        sessions.len()
    );
    assert!(fires > 0 && arms > 0 && errors > 0 && cancelled > 0);
    assert_eq!(diffs, 0);
}

/// Bookkeeping sessions, `rounds` of every one less the setup on both
/// sides (`CHECK_HARNESS_SPEED=1 cargo test --release --test
/// self_host_lsp_features lens_state_speed -- --ignored`).
#[test]
#[ignore]
fn lens_state_speed() {
    if !speed() {
        return;
    }
    const ROUNDS: u64 = 64;
    let sessions = lens_state_sessions();
    let entry = entry("lsp_lens_state");
    let inputs: Vec<[Vec<u8>; 2]> = sessions
        .iter()
        .map(|s| [0, ROUNDS].map(|n| lens_state_input(s, n)))
        .collect();
    let (mut rust, mut basis, mut fixed, mut full) =
        (Duration::MAX, Duration::MAX, Duration::MAX, Duration::MAX);
    for _ in 0..3 {
        rust = rust.min(time(|| {
            for s in &sessions {
                std::hint::black_box(rust_lens_state(s, ROUNDS));
            }
        }));
        basis = basis.min(time(|| {
            for s in &sessions {
                std::hint::black_box(rust_lens_state(s, 0));
            }
        }));
        fixed = fixed.min(time(|| {
            for [none, _] in &inputs {
                std::hint::black_box(entry.text(none));
            }
        }));
        full = full.min(time(|| {
            for [_, all] in &inputs {
                std::hint::black_box(entry.text(all));
            }
        }));
    }
    let n = sessions.len() as u32 * ROUNDS as u32;
    let (r, v) = (
        rust.saturating_sub(basis) / n,
        full.saturating_sub(fixed) / n,
    );
    eprintln!(
        "lens bookkeeping session ({} x {ROUNDS}): rust {r:?} vesper {v:?} ratio {:.2}",
        sessions.len(),
        v.as_secs_f64() / r.as_secs_f64()
    );
}
