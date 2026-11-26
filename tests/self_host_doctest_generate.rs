//! Self-hosting SH5 lane 14 (#1337), part 6: the generated module of a
//! doctest example (`rust_generate` of src/doctest_generate.rs: whether
//! the snippet declares items, the wrapper function and its `sim` effect
//! row, the entry, and the code line segments) in Vesper
//! (`packages/vesper-test-runner/src/generate.ves`, over `@vesper/ast` and
//! `@vesper/front`) against Rust. Snippets: every `.ves` file whole, each
//! top-level function, each function's body lines, three-line windows
//! (malformed prefixes), the inline sources of the doctest suites and
//! rows for every declaration start, under a cycle of wrappers, entries,
//! `compile_fail` and `sim` fences. The modules must be equal.
//!
//! `DOCTEST_GENERATE_SPEED=1` runs the ABBA timing of the ignored test.

#[path = "common/doctest_generate_words.rs"]
mod words;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::check::World;
use vesper::doctest_generate::{GenerateInput, Generated, rust_generate};
use vesper::{check, native, packages};

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
    generate: Entry,
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
        generate: Entry(
            native::WordFunction::compile(w, "doctest_generate_str", optimize)
                .unwrap_or_else(|e| panic!("doctest_generate_str: {e}")),
        ),
    }
}

fn optimized() -> &'static Runner {
    static RUNNER: OnceLock<Runner> = OnceLock::new();
    RUNNER.get_or_init(|| runner(true))
}

impl Runner {
    fn generate(&self, input: GenerateInput<'_>) -> Generated {
        words::decode_generate(&self.generate.words(&words::generate_input(input)))
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

/// Snippets for every declaration start and body form.
const ROWS: &[&str] = &[
    "",
    "\n\n",
    "// only a comment",
    "/* block */ fn f() -> i64 { 1 }",
    "fn main() -> i64 { 1 }",
    "fn broken( -> i64 {",
    "copy struct P { x: i64 }",
    "affine struct A { x: i64 }",
    "linear struct L { x: i64 }",
    "struct S { x: i64 }\nfn main() -> i64 { S { x: 1 }.x }",
    "enum E { A, B }",
    "effect Log { fn log(x: i64) -> i64; }",
    "impl S { fn m(in self) -> i64 { 0 } }",
    "trait Show { fn show(in self) -> i64; }",
    "trait",
    "model ByKey: Ord for i64 { }",
    "model",
    "pub fn f() -> i64 { 1 }",
    "pub",
    "use model ByKey;",
    "use std::io;",
    "let x = 1;\nx + 1",
    "1 + 2",
    "assert(1 == 1);",
    "{ 1 }",
    "match 1 { _ => 2 }",
    "traitor(1)",
    "models + 1",
    "public",
    "fnord",
    "struct",
    "const N: i64 = 3;",
    "type T = i64;",
    "ünïcode(1)",
];

struct Snippet {
    label: String,
    lines: Vec<String>,
    base: String,
}

fn lines_of(s: &str) -> Vec<String> {
    s.lines().map(str::to_string).collect()
}

fn fn_starts(source: &str) -> Vec<usize> {
    source
        .match_indices("fn ")
        .map(|(i, _)| i)
        .filter(|&i| i == 0 || source.as_bytes()[i - 1] == b'\n')
        .collect()
}

const SMALL_BASE: &str = "fn helper() -> i64 { 1 }\n";

fn corpus() -> &'static [Snippet] {
    static CORPUS: OnceLock<Vec<Snippet>> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let root = repo();
        let mut files = vec![];
        for d in ["examples", "packages", "crates", "src", "tests"] {
            walk(&root.join(d), &mut files);
        }
        let mut out = vec![];
        let mut push = |label: String, text: &str, base: &str| {
            out.push(Snippet {
                label,
                lines: lines_of(text),
                base: base.to_string(),
            });
        };
        for (n, f) in files.iter().enumerate() {
            let Ok(text) = std::fs::read_to_string(f) else {
                continue;
            };
            if text.len() > MAX_SOURCE_BYTES {
                continue;
            }
            let label = f
                .strip_prefix(&root)
                .unwrap_or(f)
                .display()
                .to_string()
                .replace('\\', "/");
            // Every seventh file is also a base.
            let base = if n % 7 == 0 {
                text.as_str()
            } else {
                SMALL_BASE
            };
            if text.len() <= 16 * 1024 {
                push(label.clone(), &text, SMALL_BASE);
            }
            let mut starts = fn_starts(&text);
            starts.push(text.len());
            for (k, w) in starts.windows(2).enumerate() {
                let item = &text[w[0]..w[1]];
                push(format!("{label}@{k}"), item, base);
                // The body lines between the first `{` line and the last `}`.
                let ls = lines_of(item);
                if ls.len() > 2 {
                    let body = ls[1..ls.len() - 1].join("\n");
                    push(format!("{label}@{k}/body"), &body, SMALL_BASE);
                }
            }
            let ls = lines_of(&text);
            for (k, c) in ls.chunks(3).enumerate().step_by(11) {
                push(format!("{label}#{k}"), &c.join("\n"), SMALL_BASE);
            }
        }
        for suite in [
            "tests/doctest_cli.rs",
            "src/doctest.rs",
            "src/doctest_extract.rs",
        ] {
            let Ok(text) = std::fs::read_to_string(root.join(suite)) else {
                continue;
            };
            for (i, s) in raw_strings(&text).into_iter().enumerate() {
                push(format!("{suite}#{i}"), &s, SMALL_BASE);
            }
        }
        for (i, row) in ROWS.iter().enumerate() {
            push(format!("row#{i}"), row, SMALL_BASE);
            push(format!("row#{i}/indented"), &format!("    {row}"), "");
        }
        out
    })
}

/// The fence variants of snippet `i`: wrapper, entry, `compile_fail`, sim.
fn input<'a>(
    i: usize,
    s: &'a Snippet,
    lines: &'a [&'a str],
    sims: &'a [&'a str],
) -> GenerateInput<'a> {
    const WRAPPERS: [&str; 3] = ["__vesper_doctest_0", "__doc_7", "w"];
    GenerateInput {
        base: &s.base,
        lines,
        wrapper: WRAPPERS[i % 3],
        entry: match i % 5 {
            1 => Some("run"),
            3 => Some(""),
            _ => None,
        },
        expected: i % 7 == 4,
        sim_effects: match i % 4 {
            1 => Some(&sims[..0]),
            2 => Some(&sims[..1]),
            3 => Some(sims),
            _ => None,
        },
    }
}

const SIMS: [&str; 3] = ["Clock", "Net", "Disk"];

// ------------------------------------------------------------ differential

#[derive(Default, Debug)]
struct Tally {
    snippets: usize,
    declarations: usize,
    bodies: usize,
    differ: usize,
}

fn run_differential(r: &Runner, stride: usize) -> Tally {
    let mut t = Tally::default();
    let mut shown = 0;
    for (i, s) in corpus().iter().enumerate().step_by(stride) {
        t.snippets += 1;
        let lines: Vec<&str> = s.lines.iter().map(String::as_str).collect();
        let inp = input(i, s, &lines, &SIMS);
        let rust = rust_generate(inp);
        let vesper = r.generate(inp);
        if rust != vesper {
            t.differ += 1;
            if shown < 8 {
                shown += 1;
                eprintln!(
                    "modules differ: {}:\nrust {rust:?}\nvesper {vesper:?}",
                    s.label
                );
            }
            continue;
        }
        if rust.code_start == rust.snippet_start {
            t.declarations += 1;
        } else {
            t.bodies += 1;
        }
    }
    eprintln!("{t:?}");
    t
}

#[test]
fn doctest_generate_matches_rust_on_the_corpus() {
    let t = run_differential(optimized(), 1);
    assert_eq!(t.differ, 0, "{t:?}");
    assert!(t.declarations > 1_000 && t.bodies > 1_000, "{t:?}");
}

/// The unoptimized build answers the same, on every thirteenth snippet.
#[test]
fn doctest_generate_matches_rust_unoptimized() {
    let t = run_differential(&runner(false), 13);
    assert_eq!(t.differ, 0, "{t:?}");
}

/// Every snippet, Rust (`rust_generate`) against the optimized Vesper
/// build, best of six ABBA rounds.
#[test]
#[ignore = "timing; DOCTEST_GENERATE_SPEED=1 cargo test --release --test self_host_doctest_generate -- --ignored"]
fn doctest_generate_speed() {
    if std::env::var_os("DOCTEST_GENERATE_SPEED").is_none() {
        return;
    }
    let r = optimized();
    let inputs = corpus();
    let lines: Vec<Vec<&str>> = inputs
        .iter()
        .map(|s| s.lines.iter().map(String::as_str).collect())
        .collect();
    let time = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        t.elapsed()
    };
    let rust = || {
        time(&mut || {
            for (i, s) in inputs.iter().enumerate() {
                std::hint::black_box(rust_generate(input(i, s, &lines[i], &SIMS)));
            }
        })
    };
    let vesper = || {
        time(&mut || {
            for (i, s) in inputs.iter().enumerate() {
                std::hint::black_box(r.generate(input(i, s, &lines[i], &SIMS)));
            }
        })
    };
    // The flip gate (scripts/ci/flip-gate/doctest-generate.json, #2207):
    // `FLIP_GATE_EPOCHS` (default 12) ABBA rounds after one warm-up each.
    if std::env::var_os("FLIP_GATE").is_some() {
        let epochs: usize = std::env::var("FLIP_GATE_EPOCHS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(12);
        rust();
        vesper();
        let mut pairs = String::new();
        for _ in 0..epochs {
            let (r1, v1, v2, r2) = (rust(), vesper(), vesper(), rust());
            pairs += &format!(" {}:{}", (r1 + r2).as_nanos(), (v1 + v2).as_nanos());
        }
        println!("flip-gate pairs generate{pairs}");
        return;
    }
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
        "ABBA doctest generate over {} snippets: rust {rb:?} vesper {vb:?} ratio {:.2}",
        inputs.len(),
        vb.as_secs_f64() / rb.as_secs_f64()
    );
}
