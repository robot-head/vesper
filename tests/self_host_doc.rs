//! Self-hosting SH5 lane 15 (#1338), part 1: `@vesper/doc`
//! (packages/vesper-doc) against the Rust renderers of `crates/vesper-doc`.
//!
//! The Vesper renderers are JIT-compiled from the package and installed as
//! `vesper_doc::swap`'s render table, the same way the hybrid build installs
//! its objects. Each test first renders with Rust (no table installed in
//! that phase), then with the table, through the same public functions, and
//! requires the same bytes, or the same error text:
//!
//! - `repository_documents_render_identically`: every repository `.ves` file
//!   and doc fixture that `vesper::tooling::doc::document_source` accepts:
//!   `to_json`, `render_html` and `render_document_html` (the linked page)
//!   of its `Document`, and `to_json` and `render_html` of the full
//!   `vesper doc --json` report of the file, replayed in key order.
//! - fuzzed JSON values (`VESPER_DOC_FUZZ`, default 3,000): strings with
//!   escapes, markup and non-ASCII text, numbers, the overview's keys in
//!   every shape, and nesting on both sides of serde_json's reader limit.
//! - budget sweeps: every renderer at budgets around each output's length.
//! - `speed` (ignored; `VESPER_DOC_SPEED=1`): ABBA timing against Rust.
//!
//! Every phase runs inside one `#[test]`, because the table, once installed,
//! stays for the whole process.

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::{Package, repo};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;
use vesper::native::WordFunction;
use vesper_doc::{Document, Limits, swap};

// ------------------------------------------------------------ the port

struct Port {
    pretty: WordFunction,
    page: WordFunction,
    linked: WordFunction,
}

static PORT: OnceLock<Port> = OnceLock::new();

fn port() -> &'static Port {
    PORT.get_or_init(|| {
        let package = Package::load("packages/vesper-doc");
        let compile = |name: &str| {
            WordFunction::compile(&package.world, name, true)
                .unwrap_or_else(|e| panic!("{name}: {e}"))
        };
        let format = compile("format");
        // SAFETY: `format` takes no parameters and returns a u64.
        assert_eq!(unsafe { format.call(&[]) }.unwrap(), swap::FORMAT);
        Port {
            pretty: compile("pretty_json"),
            page: compile("static_page"),
            linked: compile("linked_page"),
        }
    })
}

/// Calls an entry `fn(in Slice[u8]) -> ChunkSeq[u8]` on `input`.
fn call_bytes(f: &WordFunction, input: &[u8]) -> Vec<u8> {
    // A borrowed sequence header `{len, cap 0, data}` and the slice view
    // `{header, start, len}` over it.
    let header = [input.len() as u64, 0, input.as_ptr() as u64];
    let slice = [header.as_ptr() as u64, 0, input.len() as u64];
    // SAFETY: the entry takes one `in Slice[u8]`, passed as the address of
    // its three words, which live across the call, as do `header` and
    // `input`. It returns a live `ChunkSeq[u8]` header whose `data` holds
    // `len` bytes, which this copies and then frees.
    unsafe {
        let handle = f.call(&[slice.as_ptr() as u64]).unwrap();
        let h = handle as *const u64;
        let len = *h as usize;
        let data = *h.add(2) as *const u8;
        let out = if len == 0 {
            vec![]
        } else {
            std::slice::from_raw_parts(data, len).to_vec()
        };
        vesper::seq::native_runtime::vesper_seq_free(handle);
        out
    }
}

fn pretty_fn(request: &[u8]) -> Vec<u8> {
    call_bytes(&port().pretty, request)
}
fn page_fn(request: &[u8]) -> Vec<u8> {
    call_bytes(&port().page, request)
}
fn linked_fn(request: &[u8]) -> Vec<u8> {
    call_bytes(&port().linked, request)
}

const TABLE: swap::RenderTable = swap::RenderTable {
    pretty_json: pretty_fn,
    static_page: page_fn,
    linked_page: linked_fn,
};

// ------------------------------------------------ JSON replayed in order

/// A JSON value that serializes back to the exact text it was read from:
/// object keys keep their order, and numbers their spelling.
#[derive(Clone, Debug)]
enum J {
    Null,
    Bool(bool),
    Num(serde_json::Number),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl Serialize for J {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            J::Null => s.serialize_unit(),
            J::Bool(b) => s.serialize_bool(*b),
            J::Num(n) => n.serialize(s),
            J::Str(t) => s.serialize_str(t),
            J::Arr(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for i in items {
                    seq.serialize_element(i)?;
                }
                seq.end()
            }
            J::Obj(fields) => {
                let mut map = s.serialize_map(Some(fields.len()))?;
                for (k, v) in fields {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

/// Reads a JSON text (from serde_json) into a `J`, without a depth limit.
fn read_j(text: &[u8]) -> J {
    struct R<'a> {
        b: &'a [u8],
        i: usize,
    }
    impl R<'_> {
        fn ws(&mut self) {
            while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
                self.i += 1;
            }
        }
        fn string(&mut self) -> String {
            let start = self.i;
            self.i += 1;
            while self.b[self.i] != b'"' {
                if self.b[self.i] == b'\\' {
                    self.i += 1;
                }
                self.i += 1;
            }
            self.i += 1;
            serde_json::from_slice(&self.b[start..self.i]).unwrap()
        }
        fn value(&mut self) -> J {
            self.ws();
            let v = match self.b[self.i] {
                b'n' => {
                    self.i += 4;
                    J::Null
                }
                b't' => {
                    self.i += 4;
                    J::Bool(true)
                }
                b'f' => {
                    self.i += 5;
                    J::Bool(false)
                }
                b'"' => J::Str(self.string()),
                b'[' => {
                    self.i += 1;
                    let mut items = vec![];
                    self.ws();
                    if self.b[self.i] == b']' {
                        self.i += 1;
                    } else {
                        loop {
                            items.push(self.value());
                            self.ws();
                            let c = self.b[self.i];
                            self.i += 1;
                            if c == b']' {
                                break;
                            }
                        }
                    }
                    J::Arr(items)
                }
                b'{' => {
                    self.i += 1;
                    let mut fields = vec![];
                    self.ws();
                    if self.b[self.i] == b'}' {
                        self.i += 1;
                    } else {
                        loop {
                            self.ws();
                            let k = self.string();
                            self.ws();
                            self.i += 1; // ':'
                            let v = self.value();
                            fields.push((k, v));
                            self.ws();
                            let c = self.b[self.i];
                            self.i += 1;
                            if c == b'}' {
                                break;
                            }
                        }
                    }
                    J::Obj(fields)
                }
                _ => {
                    let start = self.i;
                    while self.i < self.b.len()
                        && matches!(
                            self.b[self.i],
                            b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                        )
                    {
                        self.i += 1;
                    }
                    J::Num(serde_json::from_slice(&self.b[start..self.i]).unwrap())
                }
            };
            self.ws();
            v
        }
    }
    R { b: text, i: 0 }.value()
}

// ------------------------------------------------------------- the cases

/// Repository `.ves` files and the doc fixtures, in path order: every
/// `.ves` file under the repository's source directories (hidden, `target`
/// and `bazel-*` directories skipped). Bazel's sandbox holds the root
/// package's files and @vesper/serde; Cargo sees every crate too.
fn sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !(name.starts_with('.') || name == "target" || name.starts_with("bazel-")) {
                    walk(&path, out);
                }
            } else if name.ends_with(".ves") {
                out.push(path);
            }
        }
    }
    let mut files = vec![];
    for dir in ["bootstrap", "crates", "examples", "packages", "tests"] {
        walk(&repo().join(dir), &mut files);
    }
    files.sort();
    let stride: usize = std::env::var("VESPER_DOC_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    files.into_iter().step_by(stride.max(1)).collect()
}

/// One rendered case: what Rust and then the port produced.
struct Outputs(Vec<Result<Vec<u8>, String>>);

fn document_outputs(doc: &Document, limits: Limits) -> Outputs {
    Outputs(vec![
        vesper_doc::to_json(doc, limits.max_output_bytes),
        vesper_doc::render_html(doc, limits),
        vesper_doc::render_document_html(doc, doc, limits),
    ])
}

fn value_outputs(v: &J, limits: Limits) -> Outputs {
    Outputs(vec![
        vesper_doc::to_json(v, limits.max_output_bytes),
        vesper_doc::render_html(v, limits),
    ])
}

/// Budgets around every length the outputs have, plus a few small ones.
fn budgets(o: &Outputs) -> Vec<usize> {
    let mut b = vec![1, 64, 1024];
    for bytes in o.0.iter().flatten() {
        let n = bytes.len();
        b.extend([n.saturating_sub(1), n, n + 1].iter().filter(|&&x| x > 0));
    }
    b.sort();
    b.dedup();
    b
}

fn limits_with(max: usize) -> Limits {
    Limits {
        max_output_bytes: max.min(8_388_608),
        ..Limits::default()
    }
}

struct Case {
    name: String,
    kind: CaseKind,
    limits: Limits,
    rust: Option<Outputs>,
}

enum CaseKind {
    Doc(Box<Document>),
    Value(J),
}

impl Case {
    fn render(&self) -> Outputs {
        match &self.kind {
            CaseKind::Doc(d) => document_outputs(d, self.limits),
            CaseKind::Value(v) => value_outputs(v, self.limits),
        }
    }
}

fn vesper_bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_vesper"))
}

/// The `vesper doc --json` report of `path`, if the CLI documents it.
fn cli_report(path: &Path) -> Option<Vec<u8>> {
    let out = Command::new(vesper_bin())
        .args(["doc", "--json"])
        .arg(path)
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

// ------------------------------------------------------------- the fuzzer

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const KEYS: &[&str] = &[
    "document",
    "functions",
    "types",
    "effects",
    "traits",
    "name",
    "declaration",
    "docs",
    "",
    "na\"me",
    "x",
];

fn fuzz_string(r: &mut Rng) -> String {
    const PIECES: &[&str] = &[
        "a", "Z", "&", "<", ">", "\"", "'", "\\", "\n", "\t", "\u{1}", "\u{1f}", "\u{7f}", "é",
        "水", "😀", "</pre>", "&amp;", " ", "/", "\u{2028}",
    ];
    let n = r.below(12);
    (0..n)
        .map(|_| PIECES[r.below(PIECES.len() as u64) as usize])
        .collect()
}

fn fuzz_value(r: &mut Rng, depth: usize, budget: &mut usize) -> J {
    if *budget == 0 {
        return J::Null;
    }
    *budget -= 1;
    let pick = if depth > 6 { r.below(5) } else { r.below(8) };
    match pick {
        0 => J::Null,
        1 => J::Bool(r.below(2) == 0),
        2 => J::Num(match r.below(3) {
            0 => serde_json::Number::from(r.next()),
            1 => serde_json::Number::from(r.next() as i64),
            _ => serde_json::Number::from_f64(f64::from_bits(r.next()) % 1e9).unwrap_or(0.into()),
        }),
        3 | 4 => J::Str(fuzz_string(r)),
        5 => J::Arr(
            (0..r.below(5))
                .map(|_| fuzz_value(r, depth + 1, budget))
                .collect(),
        ),
        _ => {
            let mut keys: Vec<&str> = KEYS.to_vec();
            let n = r.below(6) as usize;
            let mut fields = vec![];
            for _ in 0..n {
                if keys.is_empty() {
                    break;
                }
                let k = keys.remove(r.below(keys.len() as u64) as usize);
                fields.push((k.to_string(), fuzz_value(r, depth + 1, budget)));
            }
            J::Obj(fields)
        }
    }
}

/// A documented-looking value: the overview's sections, with items of
/// every shape.
fn fuzz_report(r: &mut Rng) -> J {
    let mut budget = 400;
    let mut item = |r: &mut Rng| -> J {
        let mut fields = vec![];
        match r.below(4) {
            0 => fields.push(("name".into(), J::Str(fuzz_string(r)))),
            1 => fields.push((
                "declaration".into(),
                J::Obj(vec![("name".into(), fuzz_value(r, 5, &mut budget))]),
            )),
            2 => fields.push(("name".into(), fuzz_value(r, 5, &mut budget))),
            _ => {}
        }
        if r.below(3) != 0 {
            fields.push(("docs".into(), J::Str(fuzz_string(r))));
        }
        if r.below(2) == 0 {
            fields.push(("other".into(), fuzz_value(r, 3, &mut budget)));
        }
        if r.below(10) == 0 {
            J::Str(fuzz_string(r))
        } else {
            J::Obj(fields)
        }
    };
    let mut sections = vec![];
    for key in ["functions", "types", "effects", "traits"] {
        if r.below(4) != 0 {
            let value = if r.below(8) == 0 {
                J::Str("not an array".into())
            } else {
                J::Arr((0..r.below(4)).map(|_| item(r)).collect())
            };
            sections.push((key.to_string(), value));
        }
    }
    let doc = J::Obj(sections);
    match r.below(3) {
        0 => doc,
        1 => J::Obj(vec![
            ("schema_version".into(), J::Num(1.into())),
            ("document".into(), doc),
        ]),
        _ => J::Arr(vec![doc]),
    }
}

/// `inner` wrapped in `depth` nested containers.
fn nest(inner: J, depth: usize, r: &mut Rng) -> J {
    let mut v = inner;
    for _ in 0..depth {
        v = if r.below(2) == 0 {
            J::Arr(vec![v])
        } else {
            J::Obj(vec![("functions".into(), v)])
        };
    }
    v
}

fn fuzz_cases() -> Vec<Case> {
    let n: u64 = std::env::var("VESPER_DOC_FUZZ")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let mut cases = vec![];
    for i in 0..n {
        let v = match i % 4 {
            0 => fuzz_value(&mut r, 0, &mut 200),
            1 | 2 => fuzz_report(&mut r),
            _ => {
                // Around serde_json's reader limit (127 nested containers
                // parse): the overview appears or is left out.
                let depth = 120 + r.below(16) as usize;
                let inner = fuzz_report(&mut r);
                let wrapped = nest(inner, depth, &mut r);
                J::Obj(vec![("document".into(), wrapped)])
            }
        };
        cases.push(Case {
            name: format!("fuzz #{i}"),
            kind: CaseKind::Value(v),
            limits: Limits::default(),
            rust: None,
        });
    }
    // Exact depths at the limit, with a real overview inside.
    for depth in 124..=130 {
        let inner = J::Obj(vec![(
            "functions".into(),
            J::Arr(vec![J::Obj(vec![
                ("name".into(), J::Str("f<&>".into())),
                ("docs".into(), J::Str("d".into())),
            ])]),
        )]);
        let mut v = inner;
        for _ in 0..depth {
            v = J::Arr(vec![v]);
        }
        cases.push(Case {
            name: format!("nesting {depth}"),
            kind: CaseKind::Value(J::Obj(vec![("document".into(), J::Arr(vec![v]))])),
            limits: Limits::default(),
            rust: None,
        });
        // A top-level overview whose metadata is too deep for the reader.
        let mut deep = J::Null;
        for _ in 0..depth {
            deep = J::Arr(vec![deep]);
        }
        cases.push(Case {
            name: format!("deep sibling {depth}"),
            kind: CaseKind::Value(J::Obj(vec![
                (
                    "functions".into(),
                    J::Arr(vec![J::Obj(vec![("name".into(), J::Str("g".into()))])]),
                ),
                ("deep".into(), deep),
            ])),
            limits: Limits::default(),
            rust: None,
        });
    }
    cases
}

// --------------------------------------------------------------- the test

fn show(r: &Result<Vec<u8>, String>) -> String {
    match r {
        Ok(b) => format!("Ok({} bytes)", b.len()),
        Err(e) => format!("Err({e})"),
    }
}

fn first_byte_difference(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

#[test]
fn renderers_match_rust() {
    let limits = Limits::default();
    let mut cases = vec![];
    let mut documented = 0usize;
    let mut reports = 0usize;
    let mut linked = 0usize;
    for path in sources() {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let name = path.strip_prefix(repo()).unwrap().display().to_string();
        if let Ok(doc) = vesper::tooling::doc::document_source(&source, limits) {
            documented += 1;
            if !doc.intra_doc_links.references.is_empty() {
                linked += 1;
            }
            cases.push(Case {
                name: name.clone(),
                kind: CaseKind::Doc(Box::new(doc)),
                limits,
                rust: None,
            });
        }
        if let Some(report) = cli_report(&path) {
            reports += 1;
            let value = read_j(&report);
            // The replayed value serializes to the CLI's exact bytes.
            assert_eq!(
                vesper_doc::to_json(&value, 4 * limits.max_output_bytes).unwrap(),
                report,
                "{name}: report replay"
            );
            cases.push(Case {
                name: format!("{name} (report)"),
                kind: CaseKind::Value(value),
                limits,
                rust: None,
            });
        }
    }
    eprintln!("{documented} documents ({linked} with intra-doc links), {reports} CLI reports");
    if std::env::var_os("VESPER_DOC_STRIDE").is_none() {
        assert!(documented > 50 && reports > 50, "corpus too small");
        assert!(linked > 0, "no document has an intra-doc link");
    }
    let corpus = cases.len();
    cases.extend(fuzz_cases());

    // Phase 1: Rust, and the budget sweeps' cases.
    let mut swept = vec![];
    for (i, c) in cases.iter_mut().enumerate() {
        let out = c.render();
        // Sweep the budgets of every 16th corpus case and every 64th
        // fuzzed one.
        if (i < corpus && i % 16 == 0) || (i >= corpus && i % 64 == 0) {
            for max in budgets(&out) {
                swept.push(Case {
                    name: format!("{} at budget {max}", c.name),
                    kind: match &c.kind {
                        CaseKind::Doc(d) => CaseKind::Doc(d.clone()),
                        CaseKind::Value(v) => CaseKind::Value(v.clone()),
                    },
                    limits: limits_with(max),
                    rust: None,
                });
            }
        }
        c.rust = Some(out);
    }
    for c in &mut swept {
        c.rust = Some(c.render());
    }
    cases.extend(swept);

    // Phase 2: the port.
    port();
    swap::install_render(TABLE).unwrap();
    let mut bad = 0;
    let mut overviews = 0;
    let mut errors = 0;
    for c in &cases {
        let vesper = c.render();
        let rust = c.rust.as_ref().unwrap();
        for (k, (r, v)) in rust.0.iter().zip(&vesper.0).enumerate() {
            if let Ok(b) = r {
                if k == 1 && b.windows(10).any(|w| w == b"<section><") {
                    overviews += 1;
                }
            } else {
                errors += 1;
            }
            if r != v {
                bad += 1;
                if bad <= 8 {
                    let at = match (r, v) {
                        (Ok(a), Ok(b)) => {
                            let at = first_byte_difference(a, b);
                            let ctx = |x: &[u8]| {
                                String::from_utf8_lossy(
                                    &x[at.saturating_sub(40)..(at + 40).min(x.len())],
                                )
                                .into_owned()
                            };
                            format!(
                                " at byte {at}:\n  rust:   {:?}\n  vesper: {:?}",
                                ctx(a),
                                ctx(b)
                            )
                        }
                        _ => String::new(),
                    };
                    eprintln!(
                        "{} renderer {k}: rust {} vesper {}{at}",
                        c.name,
                        show(r),
                        show(v)
                    );
                }
            }
        }
    }
    eprintln!(
        "{} cases ({corpus} from the repository), {overviews} pages with an overview, {errors} errors, {bad} differ",
        cases.len()
    );
    assert!(overviews > 50, "the overview never ran");
    assert!(errors > 50, "the budget sweeps never failed");
    assert_eq!(bad, 0);
}

/// ABBA timing of the renderers against Rust (`VESPER_DOC_SPEED=1`): the
/// `to_json` and `render_html` of every 4th file's CLI report. The port
/// side is what the swap runs: the compact serialization, the request and
/// the Vesper call.
#[test]
#[ignore]
fn speed() {
    if std::env::var("VESPER_DOC_SPEED").as_deref() != Ok("1") {
        return;
    }
    let limits = Limits::default();
    let mut values = vec![];
    for path in sources().into_iter().step_by(4) {
        if let Some(report) = cli_report(&path) {
            values.push(read_j(&report));
        }
    }
    let bytes: usize = values
        .iter()
        .map(|v| serde_json::to_vec(v).unwrap().len())
        .sum();
    let max = 4 * limits.max_output_bytes;
    let rust = |values: &[J]| {
        let t = Instant::now();
        for v in values {
            std::hint::black_box(vesper_doc::to_json(v, max).unwrap());
            std::hint::black_box(vesper_doc::render_html(v, limits).ok());
        }
        t.elapsed()
    };
    let vesper = |values: &[J]| {
        let t = Instant::now();
        for v in values {
            let compact = swap::compact(v, max).unwrap();
            std::hint::black_box(
                swap::decode(pretty_fn(&swap::pretty_request(&compact, max))).unwrap(),
            );
            if let Ok(compact) = swap::compact(v, limits.max_output_bytes) {
                let request = swap::pretty_request(&compact, limits.max_output_bytes);
                std::hint::black_box(swap::decode(page_fn(&request)).ok());
            }
        }
        t.elapsed()
    };
    port();
    let (a1, b1, b2, a2) = (
        rust(&values),
        vesper(&values),
        vesper(&values),
        rust(&values),
    );
    let (a, b) = (a1 + a2, b1 + b2);
    eprintln!(
        "{} reports, {bytes} compact JSON bytes: rust {a1:?} {a2:?}, vesper {b1:?} {b2:?}: {:.2}x of Rust",
        values.len(),
        b.as_secs_f64() / a.as_secs_f64()
    );
}
