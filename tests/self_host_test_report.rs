//! Self-hosting SH5 lane 14 (#1337), part 8: the text reports of `vesper
//! test` and `vesper test --doc` (src/test_report.rs) in Vesper
//! (`packages/vesper-test-runner/src/report.ves`) against Rust.
//!
//! Reports: every `.ves` file of the repository yields doctest, unit-test,
//! baseline and mutant reports whose names, messages, codes, pins and
//! capsules are the file's lines and identifiers (non-ASCII text, braces,
//! empty strings), under a seeded cycle of counts, positions up to
//! `u64::MAX`, pass/fail, engines, file errors and mutant outcomes; plus
//! the empty reports.
//!
//! `TEST_REPORT_SPEED=1` runs the ABBA timing of the ignored test: end to
//! end (encoding the reports into words, rendering and decoding the text,
//! as the hybrid swap does) and the rendering alone over encoded words
//! (`TEST_REPORT_SPEED=render`, what a Vesper-only compiler pays).

#[path = "common/flip_gate.rs"]
mod flip_gate;
#[path = "common/test_report_words.rs"]
mod words;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::check::World;
use vesper::test_report::{
    Diag, DocExample, Mutant, MutantCounts, MutantOutcome, UnitCase, UnitFile, rust_baseline_text,
    rust_doc_text, rust_mutants_text, rust_unit_text,
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
    doc: Entry,
    unit: Entry,
    baseline: Entry,
    mutants: Entry,
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
    let compile = |name: &str| {
        Entry(
            native::WordFunction::compile(w, name, optimize)
                .unwrap_or_else(|e| panic!("{name}: {e}")),
        )
    };
    Runner {
        doc: compile("report_doc_text_str"),
        unit: compile("report_unit_text_str"),
        baseline: compile("report_baseline_text_str"),
        mutants: compile("report_mutants_text_str"),
    }
}

fn optimized() -> &'static Runner {
    static RUNNER: OnceLock<Runner> = OnceLock::new();
    RUNNER.get_or_init(|| runner(true))
}

impl Runner {
    fn doc(&self, file: &str, base: &[Diag<'_>], examples: &[DocExample<'_>]) -> String {
        words::decode_text(&self.doc.words(&words::doc_input(file, base, examples)))
    }

    fn unit(&self, files: &[UnitFile<'_>]) -> String {
        words::decode_text(&self.unit.words(&words::unit_input(files)))
    }

    fn baseline(&self, files: &[UnitFile<'_>]) -> String {
        words::decode_text(&self.baseline.words(&words::unit_input(files)))
    }

    fn mutants(&self, mutants: &[Mutant<'_>], counts: MutantCounts) -> String {
        words::decode_text(&self.mutants.words(&words::mutants_input(mutants, counts)))
    }
}

// ------------------------------------------------------------------ corpus

struct File {
    name: String,
    /// The file's lines (at most 256) and identifiers (at most 256).
    texts: Vec<String>,
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "target" || name.starts_with("bazel-") {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|e| e == "ves") {
            out.push(p);
        }
    }
}

fn corpus() -> &'static [File] {
    static CORPUS: OnceLock<Vec<File>> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let root = repo();
        let mut paths = vec![];
        for d in ["examples", "packages", "src", "tests"] {
            walk(&root.join(d), &mut paths);
        }
        let mut out = vec![File {
            name: String::new(),
            texts: vec![String::new()],
        }];
        for p in paths {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let mut texts: Vec<String> = text.lines().take(256).map(str::to_string).collect();
            texts.extend(
                text.split(|c: char| !c.is_alphanumeric() && c != '_')
                    .filter(|w| !w.is_empty())
                    .take(256)
                    .map(str::to_string),
            );
            if texts.is_empty() {
                texts.push(String::new());
            }
            let name = p.strip_prefix(&root).unwrap_or(&p).display().to_string();
            out.push(File { name, texts });
        }
        out
    })
}

/// A seeded xorshift stream.
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

    fn number(&mut self) -> usize {
        match self.below(8) {
            0 => 0,
            1 => usize::MAX,
            2 => self.next() as usize,
            _ => self.below(5000) as usize,
        }
    }

    fn pick<'a>(&mut self, texts: &'a [String]) -> &'a str {
        &texts[self.below(texts.len() as u64) as usize]
    }

    fn maybe<'a>(&mut self, texts: &'a [String]) -> Option<&'a str> {
        (self.below(3) == 0).then(|| self.pick(texts))
    }
}

const ENGINES: [&str; 6] = [
    "jit",
    "evaluator",
    "checker",
    "crucible",
    "simpulse",
    "cache",
];

/// An example's index, line, column, passed, message and engine.
type ExampleRow<'a> = (usize, usize, usize, bool, &'a str, Option<&'a str>);

/// One file's reports, built from its texts.
struct Reports<'a> {
    base: Vec<Diag<'a>>,
    example_diags: Vec<Vec<Diag<'a>>>,
    examples: Vec<ExampleRow<'a>>,
    cases: Vec<Vec<UnitCase<'a>>>,
    files: Vec<(&'a str, Option<&'a str>)>,
    mutants: Vec<Mutant<'a>>,
    counts: MutantCounts,
}

fn diags<'a>(rng: &mut Rng, texts: &'a [String], n: u64) -> Vec<Diag<'a>> {
    (0..rng.below(n))
        .map(|_| Diag {
            line: rng.number(),
            column: rng.number(),
            code: rng.pick(texts),
            message: rng.pick(texts),
        })
        .collect()
}

fn reports<'a>(file: &'a File, seed: u64) -> Reports<'a> {
    let t = &file.texts;
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let base = diags(&mut rng, t, 4);
    let n = rng.below(24);
    let mut examples = vec![];
    let mut example_diags = vec![];
    for _ in 0..n {
        examples.push((
            rng.number(),
            rng.number(),
            rng.number(),
            rng.below(2) == 0,
            rng.pick(t),
            (rng.below(4) != 0).then(|| ENGINES[rng.below(3) as usize]),
        ));
        example_diags.push(diags(&mut rng, t, 4));
    }
    let nf = rng.below(6);
    let mut files = vec![];
    let mut cases = vec![];
    for _ in 0..nf {
        files.push((rng.pick(t), (rng.below(5) == 0).then(|| rng.pick(t))));
        let nt = rng.below(16);
        cases.push(
            (0..nt)
                .map(|_| UnitCase {
                    name: rng.pick(t),
                    line: rng.number(),
                    column: rng.number(),
                    passed: rng.below(3) != 0,
                    message: rng.pick(t),
                    engine: ENGINES[rng.below(6) as usize],
                    pin: rng.maybe(t),
                    capsule: rng.maybe(t),
                })
                .collect(),
        );
    }
    let mutants = (0..rng.below(24))
        .map(|_| Mutant {
            file: rng.pick(t),
            line: rng.number(),
            column: rng.number(),
            function: rng.pick(t),
            original: rng.pick(t),
            replacement: rng.pick(t),
            outcome: match rng.below(4) {
                0 => MutantOutcome::Survived,
                1 => MutantOutcome::IllTyped,
                _ => MutantOutcome::Killed(rng.maybe(t)),
            },
        })
        .collect();
    let counts = MutantCounts {
        killed: rng.number(),
        survived: rng.number(),
        ill_typed: rng.number(),
        tested: rng.number(),
        total: rng.number(),
    };
    Reports {
        base,
        example_diags,
        examples,
        cases,
        files,
        mutants,
        counts,
    }
}

impl<'a> Reports<'a> {
    fn doc_examples(&'a self) -> Vec<DocExample<'a>> {
        self.examples
            .iter()
            .zip(&self.example_diags)
            .map(
                |(&(index, line, column, passed, message, engine), ds)| DocExample {
                    index,
                    line,
                    column,
                    passed,
                    message,
                    engine,
                    diagnostics: ds,
                },
            )
            .collect()
    }

    fn unit_files(&'a self) -> Vec<UnitFile<'a>> {
        self.files
            .iter()
            .zip(&self.cases)
            .map(|(&(display, error), tests)| UnitFile {
                display,
                error,
                tests,
            })
            .collect()
    }
}

/// Each file's reports under two seeds.
fn cases(stride: usize, mut f: impl FnMut(&File, &Reports<'_>)) {
    for (i, file) in corpus().iter().enumerate() {
        if i % stride != 0 {
            continue;
        }
        for seed in [i as u64 * 2 + 1, i as u64 * 2 + 2] {
            f(file, &reports(file, seed));
        }
    }
}

#[derive(Debug, Default)]
struct Tally {
    reports: usize,
    lines: usize,
    differ: usize,
}

fn run_differential(r: &Runner, stride: usize) -> Tally {
    let mut t = Tally::default();
    let check = |t: &mut Tally, what: &str, file: &str, rust: String, vesper: String| {
        t.reports += 1;
        t.lines += rust.lines().count();
        if rust != vesper {
            t.differ += 1;
            if t.differ <= 5 {
                eprintln!("{what} differs on {file}:\nrust:\n{rust}\nvesper:\n{vesper}");
            }
        }
    };
    cases(stride, |file, rep| {
        let examples = rep.doc_examples();
        let units = rep.unit_files();
        check(
            &mut t,
            "doc_text",
            &file.name,
            rust_doc_text(&file.name, &rep.base, &examples),
            r.doc(&file.name, &rep.base, &examples),
        );
        check(
            &mut t,
            "unit_text",
            &file.name,
            rust_unit_text(&units),
            r.unit(&units),
        );
        check(
            &mut t,
            "baseline_text",
            &file.name,
            rust_baseline_text(&units),
            r.baseline(&units),
        );
        check(
            &mut t,
            "mutants_text",
            &file.name,
            rust_mutants_text(&rep.mutants, rep.counts),
            r.mutants(&rep.mutants, rep.counts),
        );
    });
    t
}

#[test]
fn test_report_matches_rust_on_the_corpus() {
    let t = run_differential(optimized(), 1);
    eprintln!("{t:?}");
    assert_eq!(t.differ, 0, "{t:?}");
    assert!(t.reports > 4000 && t.lines > t.reports, "{t:?}");
}

/// The unoptimized build renders the same, on every thirteenth file.
#[test]
fn test_report_matches_rust_unoptimized() {
    let t = run_differential(&runner(false), 13);
    assert_eq!(t.differ, 0, "{t:?}");
}

/// Every report, Rust against the optimized Vesper build, best of six ABBA
/// rounds.
#[test]
#[ignore = "timing; TEST_REPORT_SPEED=1 cargo test --release --test self_host_test_report -- --ignored"]
fn test_report_speed() {
    if std::env::var_os("TEST_REPORT_SPEED").is_none() && !flip_gate::on() {
        return;
    }
    let r = optimized();
    let built: Vec<(&File, Reports<'_>)> = {
        let mut v = vec![];
        for (i, file) in corpus().iter().enumerate() {
            v.push((file, reports(file, i as u64 + 1)));
        }
        v
    };
    let render_only = std::env::var("TEST_REPORT_SPEED").is_ok_and(|v| v == "render");
    let encoded: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = built
        .iter()
        .map(|(file, rep)| {
            (
                words::doc_input(&file.name, &rep.base, &rep.doc_examples()),
                words::unit_input(&rep.unit_files()),
                words::mutants_input(&rep.mutants, rep.counts),
            )
        })
        .collect();
    let run = |vesper: bool| {
        let t = Instant::now();
        if render_only && vesper {
            for (doc, unit, mutants) in &encoded {
                std::hint::black_box(r.doc.words(doc));
                std::hint::black_box(r.unit.words(unit));
                std::hint::black_box(r.baseline.words(unit));
                std::hint::black_box(r.mutants.words(mutants));
            }
            return t.elapsed();
        }
        for (file, rep) in &built {
            let examples = rep.doc_examples();
            let units = rep.unit_files();
            if vesper {
                std::hint::black_box(r.doc(&file.name, &rep.base, &examples));
                std::hint::black_box(r.unit(&units));
                std::hint::black_box(r.baseline(&units));
                std::hint::black_box(r.mutants(&rep.mutants, rep.counts));
            } else {
                std::hint::black_box(rust_doc_text(&file.name, &rep.base, &examples));
                std::hint::black_box(rust_unit_text(&units));
                std::hint::black_box(rust_baseline_text(&units));
                std::hint::black_box(rust_mutants_text(&rep.mutants, rep.counts));
            }
        }
        t.elapsed()
    };
    if flip_gate::on() {
        // The gate times the swap's path: encoding, rendering, decoding.
        flip_gate::pairs("test-report-end-to-end", &mut || run(false), &mut || {
            run(true)
        });
        return;
    }
    let mut rs: Vec<Duration> = vec![];
    let mut vs: Vec<Duration> = vec![];
    for _ in 0..3 {
        rs.push(run(false));
        vs.push(run(true));
        vs.push(run(true));
        rs.push(run(false));
    }
    let (rb, vb) = (*rs.iter().min().unwrap(), *vs.iter().min().unwrap());
    eprintln!(
        "ABBA test report{}: rust {rb:?} vesper {vb:?} ratio {:.2}",
        if render_only {
            " (rendering alone)"
        } else {
            ""
        },
        vb.as_secs_f64() / rb.as_secs_f64()
    );
}
