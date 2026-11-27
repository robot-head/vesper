//! Self-hosting SH5 lane 14 (#1337), part 2: doctest fence extraction and
//! the token preflight of `vesper test --doc` in Vesper
//! (`packages/vesper-test-runner/src/doctest.ves`) against the Rust
//! implementation (`vesper::doctest_extract::{rust_extract,
//! rust_preflight}`), over every `.ves` file of the repository, mutants of
//! each that insert doc-comment blocks with every kind of fence and
//! attribute, and the code of every example extracted. Both limit sets
//! (the defaults and small ones that make the limits trip) must give equal
//! examples or equal error messages.
//!
//! `DOCTEST_EXTRACT_SPEED=1` runs the ABBA timing of the ignored test.

#[path = "common/flip_gate.rs"]
mod flip_gate;
#[path = "common/doctest_extract_words.rs"]
mod words;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::check::World;
use vesper::doctest_extract::{
    Example, ExtractLimits, PreflightLimits, rust_extract, rust_preflight,
};
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
    preflight: Entry,
    extract: Entry,
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
    let entry = |name: &str| {
        Entry(
            native::WordFunction::compile(w, name, optimize)
                .unwrap_or_else(|e| panic!("{name}: {e}")),
        )
    };
    Runner {
        preflight: entry("doctest_preflight_str"),
        extract: entry("doctest_extract_str"),
    }
}

fn optimized() -> &'static Runner {
    static RUNNER: OnceLock<Runner> = OnceLock::new();
    RUNNER.get_or_init(|| runner(true))
}

impl Runner {
    fn preflight(&self, source: &str, limits: PreflightLimits) -> Result<(), String> {
        words::decode_preflight(
            &self
                .preflight
                .words(&words::preflight_input(source, limits)),
        )
    }

    fn extract(&self, source: &str, limits: ExtractLimits) -> Result<Vec<Example>, String> {
        words::decode_extract(
            source,
            &self.extract.words(&words::extract_input(source, limits)),
        )
    }
}

// ------------------------------------------------------------------ limits

/// `DocTestLimits::default()`.
const PREFLIGHT: PreflightLimits = PreflightLimits {
    max_tokens: 8192,
    max_nesting: 32,
    max_segment_tokens: 128,
    max_declarations: 128,
};
const EXTRACT: ExtractLimits = ExtractLimits {
    max_snippet_bytes: 16 * 1024,
    max_examples: 64,
};
/// Small limits, so every limit trips somewhere in the corpus.
const SMALL_PREFLIGHT: PreflightLimits = PreflightLimits {
    max_tokens: 400,
    max_nesting: 3,
    max_segment_tokens: 12,
    max_declarations: 4,
};
const SMALL_EXTRACT: ExtractLimits = ExtractLimits {
    max_snippet_bytes: 40,
    max_examples: 1,
};

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

/// Doc-comment blocks the mutants insert before a function: fences of
/// every kind, good and bad attributes, `sim(...)` specs, unterminated and
/// nested fences, `//!` blocks and comments that are not doc comments.
const BLOCKS: &[&str] = &[
    "/// Adds.\n///\n/// ```vesper\n/// let x = 1 + 2;\n/// ```\n",
    "/// ```vesper,compile_fail(E0302)\n/// fn f() { g() }\n/// ```\n",
    "/// ```vesper, entry=run_it\n/// fn run_it() -> i64 { 3 }\n/// ```\n",
    "/// ```vesper,entry=9x\n/// ```\n",
    "/// ```vesper,entry=\n/// ```\n",
    "/// ```vesper,entry=a-b\n/// ```\n",
    "/// ```vesper,compile_fail(E03)\n/// x\n/// ```\n",
    "/// ```vesper,compile_fail(e0302)\n/// x\n/// ```\n",
    "/// ```vesper,compile_fail(E0302),compile_fail(E0303)\n/// x\n/// ```\n",
    "/// ```vesper,compile_fail(E0302),entry=main\n/// x\n/// ```\n",
    "/// ```vesper,compile_fail(E0302),sim(seed=1,clock)\n/// x\n/// ```\n",
    "/// ```vesper,compile_fail(\n/// x\n/// ```\n",
    "/// ```vesper,compile_fail)\n/// x\n/// ```\n",
    "/// ```vesper,sim(seed=42,clock,net)\n/// Clock::now()\n/// ```\n",
    "/// ```vesper,sim(seed=0x_FF, disk)\n/// Disk::flush()\n/// ```\n",
    "/// ```vesper,sim(seed=+7,clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=0x+a,net)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=18446744073709551615,clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=18446744073709551616,clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=0x1_0000_0000_0000_0000,clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=,clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=0x,clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=-1,clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=1é,clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=1,seed=2,clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=1,clock,clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=1)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim()\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=1, Clock)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=1,net),sim(seed=2,net)\n/// 1\n/// ```\n",
    "/// ```vesper,sim(seed=(1),net)\n/// 1\n/// ```\n",
    "/// ```vesper,sim\n/// 1\n/// ```\n",
    "/// ```vesper,should_panic\n/// 1\n/// ```\n",
    "/// ```vesper,\n/// 1\n/// ```\n",
    "/// ```vesper sim(seed=1,clock)\n/// 1\n/// ```\n",
    "/// ```vesper\u{a0}x\n/// 1\n/// ```\n",
    "/// ```vespers\n/// 1\n/// ```\n",
    "/// ``` vesper \n/// 1\n/// ```\n",
    "/// ```rust\n/// ```vesper\n/// ```\n/// ```vesper\n/// let y = 2;\n/// ```\n",
    "/// ~~~vesper\n/// let a = 1;\n/// ```\n/// ~~~~\n",
    "/// ````vesper\n/// ```\n/// let a = 1;\n/// ````\n",
    "/// ```vesper\n/// let a = 1;\n/// ```` \n",
    "/// ```vesper\n/// let a = 1;\n/// ``` x\n/// ```\n",
    "/// ```vesper\n/// never closed\n",
    "/// ```vesper\n/// split\n\n/// ```\n",
    "/// ```vesper\n/// a\n// plain\n/// ```\n",
    "/// ```vesper\n/// a\n//// four\n/// ```\n",
    "/// ```vesper\n/// a\n/* block */ /// ```\n",
    "//! ```vesper\n//! let m = 1;\n//! ```\n",
    "///```vesper\n///let tight = 1;\n///```\n",
    "///   ```vesper\n///   let deep = 1;\n///   ```\n",
    "/// ``vesper\n/// 1\n/// ``\n",
    "/// ```vesper\n/// fn a() { (((( 1 )))) }\n/// struct S { x: i64 }\n/// enum E { A }\n/// fn b() { let q = 1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 + 11 + 12 + 13; }\n/// ```\n",
    "/// ```vesper\n/// trait T { }\n/// pub fn c() { [1, 2, 3] }\n/// model M\n/// impl S { fn d() {} }\n/// ```\n/// ```vesper\n/// 1\n/// ```\n",
    "/// ```vesper\n/// a ) ] } ;\n/// ```\n",
    "/// ```vesper\n/// \"unterminated\n/// ```\n",
    "/// ```vesper,compile_fail(E0001)\n/// let s = \"é\u{301}\";\n/// ```\n",
];

/// Doc blocks each source's mutants take: source `i` takes blocks
/// `i * MUTANTS_PER_SOURCE..`, so the corpus covers every block many times.
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
            let block = BLOCKS[(i * MUTANTS_PER_SOURCE + k) % BLOCKS.len()];
            let at = starts[(i + k) % starts.len()];
            // The second mutant gets two blocks: one at the top too.
            if k == 1 {
                let other = BLOCKS[(i + 7) % BLOCKS.len()];
                format!("{other}{}{block}{}", &source[..at], &source[at..])
            } else {
                format!("{}{block}{}", &source[..at], &source[at..])
            }
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
        for (i, b) in BLOCKS.iter().enumerate() {
            out.push(Source {
                label: format!("block#{i}"),
                text: format!("{b}fn main() {{}}\n"),
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
    with_examples: usize,
    examples: usize,
    extract_errors: usize,
    snippets: usize,
    preflight_errors: usize,
    differ: usize,
    /// The distinct error messages both sides gave.
    messages: std::collections::BTreeSet<String>,
}

fn run_differential(r: &Runner, stride: usize) -> Tally {
    let mut t = Tally::default();
    let mut shown = 0;
    let mut report = |t: &mut Tally,
                      what: &str,
                      label: &str,
                      rust: &dyn std::fmt::Debug,
                      vesper: &dyn std::fmt::Debug| {
        t.differ += 1;
        if shown < 8 {
            shown += 1;
            eprintln!("{what} differs: {label}\n  rust:   {rust:?}\n  vesper: {vesper:?}");
        }
    };
    for s in corpus().iter().step_by(stride) {
        t.sources += 1;
        for limits in [PREFLIGHT, SMALL_PREFLIGHT] {
            let rust = rust_preflight(&s.text, limits);
            let vesper = r.preflight(&s.text, limits);
            if rust != vesper {
                report(&mut t, "preflight", &s.label, &rust, &vesper);
            } else if let Err(e) = rust {
                t.preflight_errors += 1;
                t.messages.insert(e);
            }
        }
        for limits in [EXTRACT, SMALL_EXTRACT] {
            let rust = rust_extract(&s.text, limits);
            let vesper = r.extract(&s.text, limits);
            if rust != vesper {
                report(&mut t, "extract", &s.label, &rust, &vesper);
                continue;
            }
            let examples = match rust {
                Ok(examples) => examples,
                Err(e) => {
                    t.extract_errors += 1;
                    t.messages.insert(e);
                    continue;
                }
            };
            if limits != EXTRACT {
                continue;
            }
            if !examples.is_empty() {
                t.with_examples += 1;
            }
            t.examples += examples.len();
            // The code of each example, as `generate` preflights it.
            for e in &examples {
                let raw: String = e.lines.iter().map(|l| format!("{}\n", l.text)).collect();
                t.snippets += 1;
                for limits in [PREFLIGHT, SMALL_PREFLIGHT] {
                    let rust = rust_preflight(&raw, limits);
                    let vesper = r.preflight(&raw, limits);
                    if rust != vesper {
                        report(&mut t, "snippet preflight", &s.label, &rust, &vesper);
                    } else if rust.is_err() {
                        t.preflight_errors += 1;
                    }
                }
            }
        }
    }
    eprintln!("{t:?}");
    t
}

#[test]
fn extraction_matches_rust_on_the_corpus() {
    let t = run_differential(optimized(), 1);
    assert_eq!(t.differ, 0, "{t:?}");
    // The corpus really exercises extraction and both limit sets.
    assert!(t.examples > 300, "{t:?}");
    assert!(t.extract_errors > 300, "{t:?}");
    assert!(t.preflight_errors > 300, "{t:?}");
    // Every error of both functions: the limits, the fence and attribute
    // errors and the `sim(...)` spec errors.
    assert!(t.messages.len() >= 25, "{t:?}");
}

/// The unoptimized build answers the same (the debug compiler's path), on
/// every thirteenth source.
#[test]
fn extraction_matches_rust_unoptimized() {
    let t = run_differential(&runner(false), 13);
    assert_eq!(t.differ, 0, "{t:?}");
}

/// Both functions over every corpus source with the default limits, Rust
/// against the optimized Vesper build, best of six ABBA rounds.
#[test]
#[ignore = "timing; DOCTEST_EXTRACT_SPEED=1 cargo test --release --test self_host_doctest_extract -- --ignored"]
fn extraction_speed() {
    if std::env::var_os("DOCTEST_EXTRACT_SPEED").is_none() {
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
                std::hint::black_box(rust_preflight(&s.text, PREFLIGHT).ok());
                std::hint::black_box(rust_extract(&s.text, EXTRACT).ok());
            }
        })
    };
    let vesper = || {
        time(&mut || {
            for s in inputs {
                std::hint::black_box(r.preflight(&s.text, PREFLIGHT).ok());
                std::hint::black_box(r.extract(&s.text, EXTRACT).ok());
            }
        })
    };
    // The flip gate (scripts/ci/flip-gate/doctest-extract.json, #2207).
    if flip_gate::on() {
        flip_gate::pairs("extract", &mut || rust(), &mut || vesper());
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
        "ABBA preflight + extract over {} sources: rust {rb:?} vesper {vb:?} ratio {:.2}",
        inputs.len(),
        vb.as_secs_f64() / rb.as_secs_f64()
    );
}
