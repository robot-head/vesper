//! Self-hosting SH5 lane 14 (#1337), part 4: the Anvil function graph
//! (`FunctionGraph::from_module` of src/anvil_hash.rs: every top-level
//! function and impl method, its canonical span-insensitive BLAKE3 digest
//! and its direct callees and callers) in Vesper
//! (`packages/vesper-test-runner/src/graph.ves`, over `@vesper/ast` and
//! `@vesper-std/hash`) against Rust, over every `.ves` file of the
//! repository, the inline sources of the Anvil test suites and mutants
//! with every form the hash covers. The graphs must be equal.
//!
//! `ANVIL_GRAPH_SPEED=1` runs the ABBA timing of the ignored test.

#[path = "common/anvil_graph_words.rs"]
mod words;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::anvil_hash::FunctionGraph;
use vesper::check::World;
use vesper::{check, native, packages, parser};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

// ------------------------------------------------------------------ package

struct Entry(native::WordFunction);

impl Entry {
    /// Calls an entry taking `in Str` and returning `ChunkSeq[u64]`.
    fn words(&self, input: &[u8]) -> Vec<u64> {
        let s = [input.len() as u64, 0, input.as_ptr() as u64];
        // SAFETY: the entry takes one `in Str`, passed as the address of
        // its three words `{len, 0, data}`, which live across the call, as
        // does `input`.
        let handle = unsafe { self.0.call(&[s.as_ptr() as u64]) }.unwrap();
        // SAFETY: `handle` is the live sequence header the entry returned,
        // whose `data` holds `len` packed u64 elements.
        let words = unsafe {
            let header = handle as *const u64;
            let len = *header as usize;
            let data = *header.add(2) as *const u64;
            if len == 0 {
                vec![]
            } else {
                std::slice::from_raw_parts(data, len).to_vec()
            }
        };
        vesper::seq::native_runtime::vesper_seq_free(handle);
        words
    }
}

struct Runner {
    graph: Entry,
}

fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let assembled = packages::load(&repo().join("packages/vesper-test-runner")).unwrap();
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
        checked.world
    })
}

fn runner(optimize: bool) -> Runner {
    let w = world();
    Runner {
        graph: Entry(
            native::WordFunction::compile(w, "anvil_graph_str", optimize)
                .unwrap_or_else(|e| panic!("anvil_graph_str: {e}")),
        ),
    }
}

fn optimized() -> &'static Runner {
    static RUNNER: OnceLock<Runner> = OnceLock::new();
    RUNNER.get_or_init(|| runner(true))
}

impl Runner {
    fn graph(&self, source: &str) -> FunctionGraph {
        words::decode_graph(&self.graph.words(&words::graph_input(source)))
    }
}

fn rust_graph(source: &str) -> FunctionGraph {
    FunctionGraph::from_module(&parser::parse(source).module)
}

// ------------------------------------------------------------------ corpus

/// The runner's source limit (`MAX_SOURCE_BYTES` in src/test_runner.rs).
const MAX_SOURCE_BYTES: usize = 512 * 1024;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if matches!(&*name, "target" | ".git" | "node_modules") || name.starts_with("bazel-") {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|e| e == "ves") {
            out.push(p);
        }
    }
}

/// The raw string literals (`r#"..."#`) of a Rust test file: the inline
/// sources of the runner's test suites.
fn raw_strings(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(at) = rest.find("r#\"") {
        let body = &rest[at + 3..];
        let Some(end) = body.find("\"#") else {
            break;
        };
        out.push(body[..end].to_string());
        rest = &body[end + 2..];
    }
    out
}

/// Rows the mutants insert before a function: items with every
/// expression, pattern, literal, type and effect form the hash covers, and
/// calls between functions, methods and closures.
const ROWS: &[&str] = &[
    "fn lits() -> i64 { let a = 1; let b = 2.5; let c = true; let d = \"s\"; let e = 170141183460469231731687303715884105727; let f = -9223372036854775808; a }\n",
    "fn ops(a: i64, b: i64) -> bool { let mut c = a * b - 1; c += a; c -= b; a == b || !(a != c) && -a < b }\n",
    "fn ctl(a: i64) -> i64 { if a > 0 { match a { 1 => a + 1, (x, _) => x, P { x, .. } => x, _ => a - 1 } } else { loop { break a * 2; } } }\n",
    "fn uses(a: i64) -> i64 { lits() + ops(a, 1) + ctl(a).x + [a * 3][0] + (a, a).1 }\n",
    "struct P { x: i64 }\nimpl P { fn m(in self) -> i64 { self.x + helper(self.x) } fn helper(x: i64) -> i64 { x } }\nfn helper(x: i64) -> i64 { x.m() }\n",
    "impl Seq[i64] { fn m(in self) -> i64 { 0 } }\nfn call_m(in p: P) -> i64 { p.m() + P::m(p) }\n",
    "fn closure(a: i64) -> i64 { let f = |x: i64| x + a; for i in 0..a + 1 { continue; } f(a) }\n",
    "fn eff(a: i64) -> i64 / Fail, State[i64], Log { handle { a? } with { Fail::fail(e) => 0, } }\n",
    "fn gen[T, U](in x: T, inout y: U, sink z: Seq[T]) -> (T, U) / ..e { return (x, y); }\n",
    "fip fn fipped(a: u8) -> u8 { a }\n",
    "fn ret(a: i64) -> i64 { if !(a == 0) { return a - 1; } { let b = a; b } }\n",
    "fn spans(a: i64) -> i64 {\n    a\n        +\n    1\n}\n",
    "fn ret(a: i64) -> i64 { a }\n",
    "impl (Box[e], i64) { fn m() -> i64 { 0 } }\nimpl (Box[f], i64) { fn m() -> i64 { 1 } }\nimpl (Box[l: Fail | r], fn(in u8, sink (u8, _)) -> Box[State[i64] | s]) { fn m() -> i64 { 2 } }\nfn use_m() -> i64 { m() }\n",
    "fn broken(a: i64) -> i64 { a + }\n",
];

const MUTANTS_PER_SOURCE: usize = 3;

fn mutants(i: usize, source: &str) -> Vec<String> {
    let starts: Vec<usize> = source
        .match_indices("fn ")
        .map(|(i, _)| i)
        .filter(|&i| i == 0 || source.as_bytes()[i - 1] == b'\n')
        .collect();
    if starts.is_empty() {
        return vec![];
    }
    (0..MUTANTS_PER_SOURCE)
        .map(|k| {
            let row = ROWS[(i * MUTANTS_PER_SOURCE + k) % ROWS.len()];
            let at = starts[(i + k) % starts.len()];
            format!("{}{row}{}", &source[..at], &source[at..])
        })
        .collect()
}

struct Source {
    label: String,
    text: String,
}

fn corpus() -> &'static [Source] {
    static CORPUS: OnceLock<Vec<Source>> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let root = repo();
        let mut files = vec![];
        for d in ["examples", "packages", "crates", "src", "tests"] {
            walk(&root.join(d), &mut files);
        }
        let mut out = vec![];
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else {
                continue;
            };
            if text.len() > MAX_SOURCE_BYTES {
                continue;
            }
            let label = f
                .strip_prefix(&root)
                .unwrap_or(&f)
                .display()
                .to_string()
                .replace('\\', "/");
            out.push(Source { label, text });
        }
        for suite in [
            "tests/test_runner_cli.rs",
            "tests/anvil_cli.rs",
            "src/anvil_hash.rs",
        ] {
            let Ok(text) = std::fs::read_to_string(root.join(suite)) else {
                continue;
            };
            for (i, s) in raw_strings(&text).into_iter().enumerate() {
                out.push(Source {
                    label: format!("{suite}#{i}"),
                    text: s,
                });
            }
        }
        for (i, row) in ROWS.iter().enumerate() {
            out.push(Source {
                label: format!("row#{i}"),
                text: format!("{row}fn main() -> i64 {{ 1 + 2 }}\n"),
            });
        }
        let base = out.len();
        for i in 0..base {
            for (k, m) in mutants(i, &out[i].text).into_iter().enumerate() {
                let label = format!("{}~{k}", out[i].label);
                out.push(Source { label, text: m });
            }
        }
        out
    })
}

// ------------------------------------------------------------ differential

#[derive(Default, Debug)]
struct Tally {
    sources: usize,
    functions: usize,
    edges: usize,
    differ: usize,
}

fn first_difference(rust: &FunctionGraph, vesper: &FunctionGraph) -> String {
    for (k, h) in &rust.fn_blake3 {
        if vesper.fn_blake3.get(k) != Some(h) {
            return format!(
                "digest of {k}: rust {h} vesper {:?}",
                vesper.fn_blake3.get(k)
            );
        }
    }
    for k in vesper.fn_blake3.keys() {
        if !rust.fn_blake3.contains_key(k) {
            return format!("extra function {k}");
        }
    }
    for (k, c) in &rust.callees {
        if vesper.callees.get(k) != Some(c) {
            return format!(
                "callees of {k}: rust {c:?} vesper {:?}",
                vesper.callees.get(k)
            );
        }
    }
    "callers".to_string()
}

fn run_differential(r: &Runner, stride: usize) -> Tally {
    let mut t = Tally::default();
    let mut shown = 0;
    for s in corpus().iter().step_by(stride) {
        t.sources += 1;
        let rust = rust_graph(&s.text);
        let vesper = r.graph(&s.text);
        if rust != vesper {
            t.differ += 1;
            if shown < 6 {
                shown += 1;
                eprintln!(
                    "graphs differ: {}: {}",
                    s.label,
                    first_difference(&rust, &vesper)
                );
            }
            continue;
        }
        t.functions += rust.fn_blake3.len();
        t.edges += rust.callees.values().map(|c| c.len()).sum::<usize>();
    }
    eprintln!("{t:?}");
    t
}

#[test]
fn anvil_graph_matches_rust_on_the_corpus() {
    let t = run_differential(optimized(), 1);
    assert_eq!(t.differ, 0, "{t:?}");
    assert!(t.functions > 10_000, "{t:?}");
    assert!(t.edges > 10_000, "{t:?}");
}

/// The unoptimized build answers the same, on every thirteenth source.
#[test]
fn anvil_graph_matches_rust_unoptimized() {
    let t = run_differential(&runner(false), 13);
    assert_eq!(t.differ, 0, "{t:?}");
}

/// The graph of every corpus source, Rust (parse and `from_module`)
/// against the optimized Vesper build (lowering and graph), best of six
/// ABBA rounds.
#[test]
#[ignore = "timing; ANVIL_GRAPH_SPEED=1 cargo test --release --test self_host_anvil_graph -- --ignored"]
fn anvil_graph_speed() {
    if std::env::var_os("ANVIL_GRAPH_SPEED").is_none() {
        return;
    }
    let r = optimized();
    let inputs = corpus();
    let time = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        t.elapsed()
    };
    let rust = || {
        time(&mut || {
            for s in inputs {
                std::hint::black_box(rust_graph(&s.text));
            }
        })
    };
    let vesper = || {
        time(&mut || {
            for s in inputs {
                std::hint::black_box(r.graph(&s.text));
            }
        })
    };
    let mut rs: Vec<Duration> = vec![];
    let mut vs: Vec<Duration> = vec![];
    for _ in 0..3 {
        rs.push(rust());
        vs.push(vesper());
        vs.push(vesper());
        rs.push(rust());
    }
    let (rb, vb) = (*rs.iter().min().unwrap(), *vs.iter().min().unwrap());
    eprintln!(
        "ABBA anvil graph over {} sources: rust {rb:?} vesper {vb:?} ratio {:.2}",
        inputs.len(),
        vb.as_secs_f64() / rb.as_secs_f64()
    );
}
