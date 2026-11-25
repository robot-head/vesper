//! Self-hosting SH5 lane 14 (#1337), part 7: what `vesper test --doc`
//! decides about a file's examples without running them
//! (src/doctest_verdict.rs): the private wrapper names, and per example
//! its line and column, its diagnostics placed in the documented file and
//! whether checking alone settles it (`compile_fail` matching, a positive
//! example that failed checking or does not define its entry), in Vesper
//! (`packages/vesper-test-runner/src/verdict.ves`) against Rust.
//!
//! Examples: the doc fences of every `.ves` file and line windows of every
//! `.ves` file as stand-in examples, each generated against its file
//! (`rust_generate`), with checker errors placed in the base, the wrapper,
//! on and between code lines and past the end, under a cycle of codes,
//! `compile_fail` codes and entry ownership. Wrapper names: every file's
//! examples against its function names, plus names built to collide.
//!
//! `DOCTEST_VERDICT_SPEED=1` runs the ABBA timing of the ignored test.

#[path = "common/doctest_verdict_words.rs"]
mod words;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::check::World;
use vesper::doctest_generate::{GenerateInput, rust_generate};
use vesper::doctest_verdict::{
    CheckError, ExampleNames, Verdict, VerdictInput, rust_verdict, rust_wrappers,
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
    wrappers: Entry,
    verdict: Entry,
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
        wrappers: compile("doctest_wrappers_str"),
        verdict: compile("doctest_verdict_str"),
    }
}

fn optimized() -> &'static Runner {
    static RUNNER: OnceLock<Runner> = OnceLock::new();
    RUNNER.get_or_init(|| runner(true))
}

impl Runner {
    fn wrappers(
        &self,
        functions: &[&str],
        examples: &[ExampleNames<'_>],
    ) -> Result<Vec<String>, String> {
        words::decode_wrappers(
            &self
                .wrappers
                .words(&words::wrappers_input(functions, examples)),
        )
    }

    fn verdict(&self, input: VerdictInput<'_>) -> Verdict {
        words::decode_verdict(&self.verdict.words(&words::verdict_input(input)))
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

/// An example of a file: its lines (text and offset), fence offset,
/// entry and `compile_fail` code.
struct Ex {
    offset: usize,
    lines: Vec<(String, usize)>,
    entry: Option<String>,
    expected: Option<String>,
}

struct File {
    label: String,
    text: String,
    functions: Vec<String>,
    examples: Vec<Ex>,
}

const CODES: [&str; 4] = ["E0001", "E0308", "E1521", "E0001"];

fn corpus() -> &'static [File] {
    static CORPUS: OnceLock<Vec<File>> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let root = repo();
        let mut paths = vec![];
        for d in ["examples", "packages", "crates", "src", "tests"] {
            walk(&root.join(d), &mut paths);
        }
        let limits = vesper::doctest_extract::ExtractLimits {
            max_snippet_bytes: 16 * 1024,
            max_examples: 64,
        };
        let mut out = vec![];
        for (n, p) in paths.iter().enumerate() {
            let Ok(text) = std::fs::read_to_string(p) else {
                continue;
            };
            if text.len() > MAX_SOURCE_BYTES {
                continue;
            }
            let label = p.strip_prefix(&root).unwrap_or(p).display().to_string();
            let functions: Vec<String> = vesper::parser::parse(&text)
                .module
                .items
                .iter()
                .filter_map(|i| match i {
                    vesper::ast::Item::Function(f) => Some(f.name.clone()),
                    _ => None,
                })
                .collect();
            let mut examples: Vec<Ex> = vesper::doctest_extract::extract(&text, limits)
                .unwrap_or_default()
                .into_iter()
                .map(|e| Ex {
                    offset: e.offset,
                    lines: e.lines.into_iter().map(|l| (l.text, l.offset)).collect(),
                    entry: e.entry,
                    expected: e.expected,
                })
                .collect();
            // Stand-in examples: windows of the file's lines.
            let mut offsets = vec![];
            let mut at = 0;
            for l in text.split_inclusive('\n') {
                offsets.push((at, l.trim_end_matches('\n').to_string()));
                at += l.len();
            }
            for (k, w) in offsets.chunks(4).enumerate().step_by(5 + n % 3) {
                examples.push(Ex {
                    offset: w[0].0.saturating_sub(1),
                    lines: w.iter().map(|(o, t)| (t.clone(), *o)).collect(),
                    entry: (k % 6 == 1).then(|| "run".to_string()),
                    expected: (k % 3 == 2).then(|| CODES[k % 4].to_string()),
                });
            }
            examples.truncate(64);
            out.push(File {
                label,
                text,
                functions,
                examples,
            });
        }
        // Names built to collide with the wrappers.
        let mut text = String::new();
        let mut functions = vec![];
        for i in 0..3 {
            for u in 0..4 {
                functions.push(format!("__vesper_doctest_{i}{}", "_".repeat(u)));
            }
        }
        functions.push(format!("__vesper_doctest_3{}", "_".repeat(100)));
        let lines = vec![(
            "let __vesper_doctest_4 = Upper::__vesper_doctest_5;".to_string(),
            0,
        )];
        text.push_str(&lines[0].0);
        let examples = (0..7)
            .map(|i| Ex {
                offset: 0,
                lines: lines.clone(),
                entry: (i == 6).then(|| "__vesper_doctest_6".to_string()),
                expected: None,
            })
            .collect();
        out.push(File {
            label: "collisions".into(),
            text,
            functions,
            examples,
        });
        // The 128-byte limit.
        let mut functions = vec![];
        for u in 0..=112 {
            functions.push(format!("__vesper_doctest_0{}", "_".repeat(u)));
        }
        out.push(File {
            label: "limit".into(),
            text: String::new(),
            functions,
            examples: vec![Ex {
                offset: 0,
                lines: vec![],
                entry: None,
                expected: None,
            }],
        });
        out
    })
}

/// The checker errors of case `k` of a generated module: none, or a few
/// at every kind of place.
fn errors(
    k: usize,
    len: usize,
    snippet: usize,
    segs: &[(usize, usize)],
) -> Vec<(String, usize, usize)> {
    let mut out = vec![];
    let n = k % 5;
    for j in 0..n {
        let code = CODES[(k + j) % 4].to_string();
        let at = match (k + j * 7) % 6 {
            0 => snippet / 2,
            1 => snippet + 1,
            2 => segs
                .get(j % segs.len().max(1))
                .map_or(snippet, |s| s.0 + (s.1 - s.0) / 2),
            3 => segs.last().map_or(snippet, |s| s.1),
            4 => segs.last().map_or(len, |s| s.1 + 1),
            _ => len,
        };
        let at = at.min(len);
        out.push((code, at, (at + 3).min(len)));
    }
    out
}

// ------------------------------------------------------------ differential

#[derive(Default, Debug)]
struct Tally {
    files: usize,
    wrapper_sets: usize,
    wrapper_errors: usize,
    examples: usize,
    diagnostics: usize,
    decided: usize,
    passed: usize,
    differ: usize,
}

/// Runs `f` on every (file, example, generated module, errors) case.
fn cases(stride: usize, mut f: impl FnMut(&File, usize, VerdictInput<'_>)) {
    for file in corpus().iter().step_by(stride) {
        for (k, e) in file.examples.iter().enumerate() {
            let texts: Vec<&str> = e.lines.iter().map(|l| l.0.as_str()).collect();
            let g = rust_generate(GenerateInput {
                base: &file.text,
                lines: &texts,
                wrapper: "__vesper_doctest_0",
                entry: e.entry.as_deref(),
                expected: e.expected.is_some(),
                sim_effects: None,
            });
            let lines: Vec<(usize, usize)> = e.lines.iter().map(|l| (l.1, l.0.len())).collect();
            let errs = errors(k, g.source.len(), g.snippet_start, &g.segments);
            let check: Vec<CheckError<'_>> = errs
                .iter()
                .map(|(code, start, end)| CheckError {
                    code,
                    message: "a checker message",
                    start: *start,
                    end: *end,
                })
                .collect();
            f(
                file,
                k,
                VerdictInput {
                    source: &file.text,
                    offset: e.offset,
                    lines: &lines,
                    snippet_start: g.snippet_start,
                    code_start: g.code_start,
                    segments: &g.segments,
                    errors: &check,
                    expected: e.expected.as_deref(),
                    own_entry: k % 4 != 3,
                },
            );
        }
    }
}

fn names(file: &File) -> (Vec<&str>, Vec<Vec<&str>>) {
    (
        file.functions.iter().map(String::as_str).collect(),
        file.examples
            .iter()
            .map(|e| e.lines.iter().map(|l| l.0.as_str()).collect())
            .collect(),
    )
}

fn run_differential(r: &Runner, stride: usize) -> Tally {
    let mut t = Tally::default();
    let mut shown = 0;
    for file in corpus().iter().step_by(stride) {
        t.files += 1;
        let (functions, lines) = names(file);
        let ex: Vec<ExampleNames<'_>> = file
            .examples
            .iter()
            .zip(&lines)
            .map(|(e, l)| ExampleNames {
                entry: e.entry.as_deref(),
                lines: l,
            })
            .collect();
        let rust = rust_wrappers(&functions, &ex);
        let vesper = r.wrappers(&functions, &ex);
        t.wrapper_sets += 1;
        if rust.is_err() {
            t.wrapper_errors += 1;
        }
        if rust != vesper {
            t.differ += 1;
            if shown < 8 {
                shown += 1;
                eprintln!(
                    "wrappers differ: {}: rust {rust:?} vesper {vesper:?}",
                    file.label
                );
            }
        }
    }
    cases(stride, |file, k, input| {
        t.examples += 1;
        let rust = rust_verdict(input);
        let vesper = r.verdict(input);
        if rust != vesper {
            t.differ += 1;
            if shown < 8 {
                shown += 1;
                eprintln!(
                    "verdicts differ: {} #{k}:\nrust {rust:?}\nvesper {vesper:?}",
                    file.label
                );
            }
            return;
        }
        t.diagnostics += rust.diagnostics.len();
        if let Some((passed, _)) = &rust.decided {
            t.decided += 1;
            t.passed += *passed as usize;
        }
    });
    eprintln!("{t:?}");
    t
}

#[test]
fn doctest_verdict_matches_rust_on_the_corpus() {
    let t = run_differential(optimized(), 1);
    assert_eq!(t.differ, 0, "{t:?}");
    assert!(t.wrapper_errors >= 1, "{t:?}");
    assert!(
        t.passed > 100 && t.decided > t.passed && t.examples > t.decided,
        "{t:?}"
    );
}

/// The unoptimized build answers the same, on every thirteenth file.
#[test]
fn doctest_verdict_matches_rust_unoptimized() {
    let t = run_differential(&runner(false), 13);
    assert_eq!(t.differ, 0, "{t:?}");
}

/// Every case, Rust against the optimized Vesper build, best of six ABBA
/// rounds.
#[test]
#[ignore = "timing; DOCTEST_VERDICT_SPEED=1 cargo test --release --test self_host_doctest_verdict -- --ignored"]
fn doctest_verdict_speed() {
    if std::env::var_os("DOCTEST_VERDICT_SPEED").is_none() {
        return;
    }
    let r = optimized();
    let time = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        t.elapsed()
    };
    let only = std::env::var("DOCTEST_VERDICT_SPEED").unwrap_or_default();
    let run = |vesper: bool| {
        time(&mut || {
            for file in corpus().iter().filter(|_| only != "verdict") {
                let (functions, lines) = names(file);
                let ex: Vec<ExampleNames<'_>> = file
                    .examples
                    .iter()
                    .zip(&lines)
                    .map(|(e, l)| ExampleNames {
                        entry: e.entry.as_deref(),
                        lines: l,
                    })
                    .collect();
                if vesper {
                    std::hint::black_box(r.wrappers(&functions, &ex)).ok();
                } else {
                    std::hint::black_box(rust_wrappers(&functions, &ex)).ok();
                }
            }
            cases(
                if only == "wrappers" { usize::MAX } else { 1 },
                |_, _, input| {
                    if vesper {
                        std::hint::black_box(r.verdict(input));
                    } else {
                        std::hint::black_box(rust_verdict(input));
                    }
                },
            );
        })
    };
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
        "ABBA doctest verdict: rust {rb:?} vesper {vb:?} ratio {:.2}",
        vb.as_secs_f64() / rb.as_secs_f64()
    );
}
