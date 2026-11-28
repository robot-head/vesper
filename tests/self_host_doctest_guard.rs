//! Self-hosting SH5 lane 14 (#1337), part 5: the fail-closed source
//! profile of `vesper test --doc` (`rust_guard` of src/doctest_guard.rs:
//! the item, AST volume and depth limits, the constructs whose checking
//! grows inferred composites, and the nominal expansion budget) in Vesper
//! (`packages/vesper-test-runner/src/guard.ves`, over `@vesper/ast`)
//! against Rust, over every `.ves` file of the repository, each of their
//! top-level functions on its own, the inline sources of the doctest
//! suites, rows that reach every error and mutants, under four limit
//! sets. Only sources that parse without diagnostics are compared: the
//! guard runs only on those. The results must be equal.
//!
//! `DOCTEST_GUARD_SPEED=1` runs the ABBA timing of the ignored test.

#[path = "common/flip_gate.rs"]
mod flip_gate;
#[path = "common/doctest_guard_words.rs"]
mod words;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::check::World;
use vesper::doctest_guard::{GuardLimits, rust_guard};
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
    guard: Entry,
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
        guard: Entry(
            native::WordFunction::compile(w, "doctest_guard_str", optimize)
                .unwrap_or_else(|e| panic!("doctest_guard_str: {e}")),
        ),
    }
}

fn optimized() -> &'static Runner {
    static RUNNER: OnceLock<Runner> = OnceLock::new();
    RUNNER.get_or_init(|| runner(true))
}

impl Runner {
    fn guard(&self, source: &str, limits: GuardLimits) -> Result<(), String> {
        words::decode_guard(&self.guard.words(&words::guard_input(source, limits)))
    }
}

/// The limit sets: the widest `DocTestLimits::validate` allows, the
/// defaults, and two small ones that trip the item and nominal limits.
const LIMITS: [GuardLimits; 4] = [
    GuardLimits {
        max_declarations: 256,
        max_nominal_expansion: 32768,
    },
    GuardLimits {
        max_declarations: 128,
        max_nominal_expansion: 8192,
    },
    GuardLimits {
        max_declarations: 4,
        max_nominal_expansion: 6,
    },
    GuardLimits {
        max_declarations: 1,
        max_nominal_expansion: 1,
    },
];

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

/// Rows the mutants insert before a function: sources that pass, and one
/// that reaches each error of the guard.
const ROWS: &[&str] = &[
    "struct P { x: i64, y: bool }\nenum E { A, B(i64, P), C { p: P } }\nfn f(in p: P) -> i64 { let a = 1; let b: i64 = a + p.x; if p.y { b } else { -b } }\n",
    "struct P { x: i64 }\nimpl P { fn m(in self) -> i64 { self.x } fn n(self) -> i64 { 0 } }\nfn g(in p: P) -> i64 { match p.x { 1 => p.m(), _ => P { x: 2 }.x } }\n",
    "fn h(a: i64) -> i64 / Fail { let mut s = a; s += 1; let v = [s, a]; v[0] + h2(a)? }\nfn h2(a: i64) -> i64 / Fail { a }\n",
    "fn k(a: i64) -> i64 { loop { break (a, a); } for i in 0..a { return (i, i).0; } a }\n",
    "fn ff(f: fn(in i64) -> i64 / Log) -> i64 { 0 }\n",
    "fn fr(f: fn(in i64) -> i64 / State[(i64, i64)]) -> i64 { 0 }\n",
    "struct G[T] { x: T }\n",
    "effect Ask[T] { fn ask() -> T; }\n",
    "effect Tell { fn tell(x: i64) -> i64; fn told(x: (i64, i64)) -> i64; }\n",
    "struct Q { x: i64 }\nimpl[T] Q { fn m(in self) -> i64 { 0 } }\n",
    "impl (i64, i64) { fn m() -> i64 { 0 } }\n",
    "trait Show { fn show(in self) -> i64; }\n",
    "fn tt(x: (i64, i64)) -> i64 { 0 }\n",
    "fn ta(x: Seq[i64]) -> i64 { 0 }\n",
    "fn to(x: Option) -> i64 { 0 }\n",
    "fn tr() -> Result { 0 }\n",
    "fn ts(self) -> i64 { 0 }\n",
    "fn tp(a: i64) -> i64 { match a { (x, y) => x, _ => 0 } }\n",
    "fn tb(a: i64) -> i64 { match a { Some(x) => x, Option::None => 0, _ => 1 } }\n",
    "fn tc(a: i64) -> i64 { match a { P { x, .. } => x, Q(z) => z, _ => 1 } }\n",
    "fn te(a: i64) -> i64 { let t = (a, 1); 0 }\n",
    "fn tu(a: i64) -> i64 { let t = (); a }\n",
    "fn tl(a: i64) -> i64 { let f = |x: i64| x + a; 0 }\n",
    "fn th(a: i64) -> i64 / Fail { handle { a } with { Fail::fail(e) => 0, } }\n",
    "fn tn(a: i64) -> i64 { let x = None; 0 }\n",
    "fn tm(a: i64) -> i64 { let x = std::Ok; 0 }\n",
    "fn tsl(a: i64) -> i64 { let x = Some { v: 1 }; 0 }\n",
    "fn tg(a: i64) -> i64 { id[i64](a) }\n",
    "fn tf(a: i64) -> i64 { a.Err }\n",
    "fn tq(a: i64) -> i64 { let (x, y) = a; x }\n",
    "fn gen[T](x: T) -> T { x }\n",
    "struct R { x: R }\n",
    "struct R1 { x: R2 }\nstruct R2 { x: (i64, fn(in R1) -> i64) }\n",
    "enum L { Nil, Cons(i64, L) }\n",
    "struct D1 { a: D2, b: D2 }\nstruct D2 { a: D3, b: D3 }\nstruct D3 { a: i64, b: i64 }\nstruct D1 { z: i64 }\n",
];

/// Generated rows: AST depth and volume, nominal width and depth.
fn generated_rows() -> Vec<String> {
    let mut rows = vec![];
    for n in [30, 62, 63, 64, 66] {
        let mut e = String::from("a");
        for _ in 0..n {
            e = format!("-({e})");
        }
        rows.push(format!("fn deep(a: i64) -> i64 {{ {e} }}\n"));
        let chain = vec!["a"; n + 1].join(" + ");
        rows.push(format!("fn chain(a: i64) -> i64 {{ {chain} }}\n"));
    }
    for n in [5000, 5460, 5461, 5462, 6000] {
        let mut s = String::from("fn wide(a: i64) -> i64 {\n");
        for i in 0..n {
            s.push_str(&format!("    let a{i} = a;\n"));
        }
        s.push_str("    a\n}\n");
        rows.push(s);
    }
    for n in [8, 12, 14, 16] {
        let mut s = String::new();
        for i in 0..n {
            s.push_str(&format!("struct W{i} {{ a: W{}, b: W{} }}\n", i + 1, i + 1));
        }
        s.push_str(&format!("struct W{n} {{ a: i64 }}\n"));
        rows.push(s);
    }
    for n in [40, 47, 48, 49, 60] {
        let mut s = String::new();
        for i in 0..n {
            s.push_str(&format!("struct C{i} {{ a: C{} }}\n", i + 1));
        }
        s.push_str(&format!("struct C{n} {{ a: i64 }}\n"));
        rows.push(s);
    }
    rows
}

const MUTANTS_PER_SOURCE: usize = 3;

fn mutants(i: usize, source: &str) -> Vec<String> {
    let starts = fn_starts(source);
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

fn fn_starts(source: &str) -> Vec<usize> {
    source
        .match_indices("fn ")
        .map(|(i, _)| i)
        .filter(|&i| i == 0 || source.as_bytes()[i - 1] == b'\n')
        .collect()
}

/// Each top-level function of a source on its own (the text from one
/// line-initial `fn ` to the next), so a generic function early in a file
/// does not hide the rest from the walk.
fn functions(source: &str) -> Vec<String> {
    let mut starts = fn_starts(source);
    starts.push(source.len());
    starts
        .windows(2)
        .map(|w| source[w[0]..w[1]].to_string())
        .collect()
}

struct Source {
    label: String,
    text: String,
}

fn clean(text: &str) -> bool {
    parser::parse(text).diagnostics.is_empty()
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
            for (k, s) in functions(&text).into_iter().enumerate() {
                out.push(Source {
                    label: format!("{label}@{k}"),
                    text: s,
                });
            }
            out.push(Source { label, text });
        }
        for suite in [
            "tests/doctest_cli.rs",
            "crates/vesper-cli-tests/tests/doctest_cli.rs",
            "src/doctest.rs",
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
        for (i, row) in generated_rows().into_iter().enumerate() {
            out.push(Source {
                label: format!("generated#{i}"),
                text: row,
            });
        }
        let base = out.len();
        for i in 0..base {
            if out[i].label.contains('@') {
                continue;
            }
            for (k, m) in mutants(i, &out[i].text).into_iter().enumerate() {
                let label = format!("{}~{k}", out[i].label);
                out.push(Source { label, text: m });
            }
        }
        out.retain(|s| clean(&s.text));
        out
    })
}

fn rust_result(source: &str, limits: GuardLimits) -> Result<(), String> {
    rust_guard(&parser::parse(source).module, limits)
}

// ------------------------------------------------------------ differential

#[derive(Default, Debug)]
struct Tally {
    sources: usize,
    runs: usize,
    passed: usize,
    differ: usize,
    errors: BTreeMap<String, usize>,
}

fn run_differential(r: &Runner, stride: usize) -> Tally {
    let mut t = Tally::default();
    let mut shown = 0;
    for s in corpus().iter().step_by(stride) {
        t.sources += 1;
        let module = parser::parse(&s.text).module;
        for limits in LIMITS {
            t.runs += 1;
            let rust = rust_guard(&module, limits);
            let vesper = r.guard(&s.text, limits);
            if rust != vesper {
                t.differ += 1;
                if shown < 8 {
                    shown += 1;
                    eprintln!(
                        "guards differ: {} {limits:?}: rust {rust:?} vesper {vesper:?}",
                        s.label
                    );
                }
                continue;
            }
            match rust {
                Ok(()) => t.passed += 1,
                Err(e) => *t.errors.entry(e).or_default() += 1,
            }
        }
    }
    eprintln!("{t:#?}");
    t
}

/// Every error `rust_guard` can return (the overflow ones cannot happen).
const ERRORS: &[&str] = &[
    "doctest parsed module item limit",
    "doctest inferred composite growth: generic nominal types unsupported before checking",
    "doctest generic effect declarations unsupported before checking",
    "doctest generic implementations unsupported before checking",
    "doctest trait/model inference unsupported before checking",
    "doctest parsed AST volume limit",
    "doctest inferred composite growth: generic functions unsupported before checking",
    "doctest parsed AST depth limit",
    "doctest inferred composite growth: nonempty tuple types unsupported before checking",
    "doctest inferred composite growth: generic type applications unsupported before checking",
    "doctest inferred function/field signatures unsupported before checking",
    "doctest inferred composite growth: tuple patterns unsupported before checking",
    "doctest inferred composite growth: builtin generic constructors unsupported before checking",
    "doctest inferred composite growth: nonempty tuples unsupported before checking",
    "doctest inferred composite growth: closures/handlers unsupported before checking",
    "doctest builtin generic constructors unsupported before checking",
    "doctest inferred composite growth: generic calls unsupported before checking",
    "doctest nominal expansion limit",
    "doctest nominal expansion depth limit",
    "recursive nominal doctest types are unsupported before checking",
];

#[test]
fn doctest_guard_matches_rust_on_the_corpus() {
    let t = run_differential(optimized(), 1);
    assert_eq!(t.differ, 0, "{t:?}");
    assert!(t.passed > 1_000, "{t:?}");
    for e in ERRORS {
        assert!(t.errors.contains_key(*e), "no source reaches {e:?}: {t:#?}");
    }
}

/// The unoptimized build answers the same, on every thirteenth source.
#[test]
fn doctest_guard_matches_rust_unoptimized() {
    let t = run_differential(&runner(false), 13);
    assert_eq!(t.differ, 0, "{t:?}");
}

/// The guard of every corpus source under the default limits, Rust (parse
/// and `rust_guard`) against the optimized Vesper build (lowering and
/// guard), best of six ABBA rounds.
#[test]
#[ignore = "timing; DOCTEST_GUARD_SPEED=1 cargo test --release --test self_host_doctest_guard -- --ignored"]
fn doctest_guard_speed() {
    if std::env::var_os("DOCTEST_GUARD_SPEED").is_none() {
        return;
    }
    let r = optimized();
    let inputs = corpus();
    let limits = LIMITS[1];
    let time = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        t.elapsed()
    };
    let rust = || {
        time(&mut || {
            for s in inputs {
                std::hint::black_box(rust_result(&s.text, limits)).ok();
            }
        })
    };
    let vesper = || {
        time(&mut || {
            for s in inputs {
                std::hint::black_box(r.guard(&s.text, limits)).ok();
            }
        })
    };
    // The flip gate (scripts/ci/flip-gate/doctest-guard.json, #2207).
    if flip_gate::on() {
        flip_gate::pairs("guard", &mut || rust(), &mut || vesper());
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
        "ABBA doctest guard over {} sources: rust {rb:?} vesper {vb:?} ratio {:.2}",
        inputs.len(),
        vb.as_secs_f64() / rb.as_secs_f64()
    );
}
