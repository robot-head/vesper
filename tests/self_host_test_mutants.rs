//! Self-hosting SH5 lane 14 (#1337), part 3: the mutation sites of
//! `vesper test --mutate` in Vesper (`packages/vesper-test-runner/src/mutants.ves`,
//! over `@vesper/ast`) against the Rust implementation
//! (`vesper::test_mutants::rust_collect`), over every `.ves` file of the
//! repository, the inline sources of the runner's test suites and mutants
//! that add test markers, `test_` names, impls, conformances and every
//! operator. Each source runs with no test names and with the names of
//! some of its functions; the sites (function, span, original and
//! replacement, in order) must be equal.
//!
//! `TEST_MUTANTS_SPEED=1` runs the ABBA timing of the ignored test.

#[path = "common/test_mutants_words.rs"]
mod words;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::ast::Item;
use vesper::check::World;
use vesper::test_mutants::{MutantSite, rust_collect};
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
    collect: Entry,
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
        collect: Entry(
            native::WordFunction::compile(w, "mutants_str", optimize)
                .unwrap_or_else(|e| panic!("mutants_str: {e}")),
        ),
    }
}

fn optimized() -> &'static Runner {
    static RUNNER: OnceLock<Runner> = OnceLock::new();
    RUNNER.get_or_init(|| runner(true))
}

impl Runner {
    fn collect(&self, source: &str, names: &BTreeSet<&str>) -> Vec<MutantSite> {
        words::decode_collect(&self.collect.words(&words::collect_input(source, names)))
    }
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

/// Rows the mutants insert before a function: test markers that exclude
/// it, comments that do not, and items with every mutable operator.
const ROWS: &[&str] = &[
    "// @test\n",
    "// @case one()\n",
    "//@test(cases = 2)\n",
    "// @testing\n",
    "// @casefoo()\n",
    "/// docs\n",
    "fn test_extra(x: i64) -> bool { x + 1 > 2 && !true }\n",
    "fn ops(a: i64, b: i64) -> bool { let mut c = a * b - 1; c += a; c -= b; a == b || a != c && a < b || a <= c || a > b || a >= c || false }\n",
    "fn spaced(a: i64) -> bool { ! ( a  <  1 ) && (  true  ) }\n",
    "fn nested(a: i64) -> i64 { if a > 0 { match a { 1 => a + 1, _ => a - 1 } } else { loop { break a * 2; } } }\n",
    "fn calls(a: i64) -> i64 { f(a + 1, g(a - 2)).x + [a * 3][0] }\n",
    "struct P { x: i64 }\nimpl P { fn m(in self) -> bool { self.x > 0 && true } }\n",
    "fn closure(a: i64) -> i64 { let f = |x: i64| x + a; for i in 0..a + 1 { } f(a) }\n",
    "fn ret(a: i64) -> i64 { if !(a == 0) { return a - 1; } (a + 1, a * 2).0 }\n",
    "fn cmt(a: i64) -> i64 { a /* x */ + /* y */ 1 }\n",
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
            "src/test_mutants.rs",
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

/// Every other top-level function name of `source`: a test-name set.
fn some_names(source: &str) -> Vec<String> {
    parser::parse(source)
        .module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(f) => Some(f.name.clone()),
            _ => None,
        })
        .step_by(2)
        .collect()
}

// ------------------------------------------------------------ differential

#[derive(Default, Debug)]
struct Tally {
    sources: usize,
    runs: usize,
    sites: usize,
    differ: usize,
}

fn run_differential(r: &Runner, stride: usize) -> Tally {
    let mut t = Tally::default();
    let mut shown = 0;
    for s in corpus().iter().step_by(stride) {
        t.sources += 1;
        let names = some_names(&s.text);
        let some: BTreeSet<&str> = names.iter().map(String::as_str).collect();
        for set in [BTreeSet::new(), some] {
            t.runs += 1;
            let rust = rust_collect(&s.text, &set);
            let vesper = r.collect(&s.text, &set);
            if rust != vesper {
                t.differ += 1;
                if shown < 6 {
                    shown += 1;
                    let at = rust.iter().zip(&vesper).position(|(a, b)| a != b);
                    eprintln!(
                        "sites differ: {} ({} vs {} sites, first difference at {at:?})\n  rust:   {:?}\n  vesper: {:?}",
                        s.label,
                        rust.len(),
                        vesper.len(),
                        at.and_then(|i| rust.get(i)),
                        at.and_then(|i| vesper.get(i)),
                    );
                }
                continue;
            }
            t.sites += rust.len();
        }
    }
    eprintln!("{t:?}");
    t
}

#[test]
fn mutation_sites_match_rust_on_the_corpus() {
    let t = run_differential(optimized(), 1);
    assert_eq!(t.differ, 0, "{t:?}");
    assert!(t.sites > 10_000, "{t:?}");
}

/// The unoptimized build answers the same, on every thirteenth source.
#[test]
fn mutation_sites_match_rust_unoptimized() {
    let t = run_differential(&runner(false), 13);
    assert_eq!(t.differ, 0, "{t:?}");
}

/// `collect` over every corpus source with no test names, Rust against the
/// optimized Vesper build, best of six ABBA rounds.
#[test]
#[ignore = "timing; TEST_MUTANTS_SPEED=1 cargo test --release --test self_host_test_mutants -- --ignored"]
fn mutation_sites_speed() {
    if std::env::var_os("TEST_MUTANTS_SPEED").is_none() {
        return;
    }
    let r = optimized();
    let inputs = corpus();
    let none = BTreeSet::new();
    let time = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        t.elapsed()
    };
    let rust = || {
        time(&mut || {
            for s in inputs {
                std::hint::black_box(rust_collect(&s.text, &none));
            }
        })
    };
    let vesper = || {
        time(&mut || {
            for s in inputs {
                std::hint::black_box(r.collect(&s.text, &none));
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
        "ABBA mutation sites over {} sources: rust {rb:?} vesper {vb:?} ratio {:.2}",
        inputs.len(),
        vb.as_secs_f64() / rb.as_secs_f64()
    );
}
