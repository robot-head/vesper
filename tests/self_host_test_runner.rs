//! Self-hosting SH5 lane 14 (#1337): test discovery of `vesper test` in
//! Vesper (`packages/vesper-test-runner`) against the Rust implementation
//! (`vesper::test_discovery::{rust_case_rows, rust_discover}`), over every
//! `.ves` file of the repository, the inline sources of the runner's test
//! suites and mutants of both that add `@test`, `@test_case`, `// @test` and
//! `// @case` attributes. The `@case` rows (with the expanded source) and
//! every discovered test (name, span, harness plan and raw pins) or error
//! message must be equal.
//!
//! `TEST_RUNNER_SPEED=1` runs the ABBA timing of the ignored test.

#[path = "common/test_runner_words.rs"]
mod words;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::ast::Item;
use vesper::check::World;
use vesper::test_discovery::{CaseRow, CaseRows, Discovered, rust_case_rows, rust_discover};
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
    case_rows: Entry,
    discover: Entry,
    format: u64,
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
    let format = native::WordFunction::compile(w, "format", optimize).unwrap();
    // SAFETY: `format` takes nothing and returns a word.
    let format = unsafe { format.call(&[]) }.unwrap();
    Runner {
        case_rows: entry("case_rows_str"),
        discover: entry("discover_str"),
        format,
    }
}

fn optimized() -> &'static Runner {
    static RUNNER: OnceLock<Runner> = OnceLock::new();
    RUNNER.get_or_init(|| runner(true))
}

impl Runner {
    fn case_rows(&self, source: &str, module: &vesper::ast::Module) -> Result<CaseRows, String> {
        let input = words::case_rows_input(source, module);
        words::decode_case_rows(source, &self.case_rows.words(&input))
    }

    fn discover(
        &self,
        source: &str,
        items: &[Item],
        cases: &[CaseRow],
    ) -> Result<Vec<Discovered>, String> {
        let input = words::discover_input(source, items, cases);
        words::decode_discover(&self.discover.words(&input))
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

/// Attribute rows the mutants insert before a function: every key of
/// `@test(...)` with good and bad values, `@test_case` pins of every value
/// kind, `// @test` markers and `// @case` rows (good, malformed and
/// duplicated).
const ATTRS: &[&str] = &[
    "@test\n",
    "@test(cases = 3)\n",
    "@test(expect_error = E0302, primary = \"linear\", related = \"moved here\")\n",
    "@test(expect_error = \"E0401\", span = \"x\")\n",
    "@test(sim_schedules = 4, fault_profile = TornWalSector, seed = 9)\n",
    "@test(fault_profile = \"Custom\\tProfile\")\n",
    "@test(fault_profile = ClockSkew, max_stack = 384)\n",
    "@test(fault_profile = NetPartition)\n",
    "@test(max_cycles = 0x1_000, zero_alloc = true)\n",
    "@test(zero_alloc = 1, cases = -1, max_stack = \"no\")\n",
    "@test(cases = 4294967301, sim_schedules = 18446744073709551615)\n",
    "@test(cases = 340282366920938463463374607431768211455)\n",
    "@test(= 3, cases 4, , primary = (1, 2), 5 = x)\n",
    "@test(expect_error = E0302\n",
    "@test_case(1, 2)\n",
    "@test_case(-7, 0xFF, 1_000, -0)\n",
    "@test_case(a = 1, b = true, c = -2.5, d = 1.0e3)\n",
    "@test_case(\"s\\\"q\\n\\\\\\0\\x\", 'c', Foo::Bar, (1, [2, 3]), {})\n",
    "@test_case(170141183460469231731687303715884105727, -170141183460469231731687303715884105727)\n",
    "@test_case(170141183460469231731687303715884105728, 99999999999999999999999999999999999999999)\n",
    "@test_case(\"\\u{1b}\u{7f}\u{85}é\u{301}\")\n",
    "@test_case\n",
    "@test_case(x = )\n",
    "@test_case(1, 2\n",
    "@fip\n@test\n",
    "pub @test\n",
    "@test\n@test_case(1)\n@test_case(2)\n",
    "// @test\n",
    "//@test(cases = 2)\n",
    "/// @test: documented\n",
    "// @testing\n",
    "// @case one()\n",
    "// @case one(1, 2)\n// @case two(3)\n",
    "// @case one()\n// @case one()\n",
    "// @case 9bad()\n",
    "// @case nobrackets\n",
    "// @case open(\n",
    "// @case\u{a0}spaced()\n",
    "// @casefoo()\n",
    "// @case\n",
    "// @case la bel()\n",
    "// @case _ok_2(x, y)   \n",
];

/// Attribute rows each source's mutants take: source `i` takes rows
/// `i * MUTANTS_PER_SOURCE..`, so the corpus covers every row many times.
const MUTANTS_PER_SOURCE: usize = 2;

/// Mutants of the `i`-th source: attribute rows inserted at the line start
/// of its functions, cycling through the functions.
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
            let attr = ATTRS[(i * MUTANTS_PER_SOURCE + k) % ATTRS.len()];
            let at = starts[(i + k) % starts.len()];
            format!("{}{attr}{}", &source[..at], &source[at..])
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
            "tests/imported_source_tests.rs",
            "crates/vesper-syntax/src/test_attrs.rs",
            "src/test_runner.rs",
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

/// What the runner gives discovery for one source: the original source, the
/// case rows, and (when the rows' wrappers parse) the expanded items with
/// each wrapper's effects taken from its target (`prepare_tests`).
fn expanded_items(source: &str, rows: &CaseRows) -> Option<Vec<Item>> {
    let parsed = parser::parse_for_tests(source);
    let Some(expanded) = &rows.expanded else {
        return Some(parsed.module.items);
    };
    let mut lowered = parser::parse_for_tests(expanded);
    if !lowered.diagnostics.is_empty() {
        return None;
    }
    let first = parsed.module.items.len();
    for (row, index) in rows.rows.iter().zip(first..) {
        let Item::Function(target) = &parsed.module.items[row.function_index] else {
            return None;
        };
        let Some(Item::Function(f)) = lowered.module.items.get_mut(index) else {
            return None;
        };
        if f.span != row.generated
            || f.name != row.entry
            || !vesper::test_discovery::is_case_wrapper_body(f, &target.name)
        {
            return None;
        }
        f.result = target.result.clone();
        f.effects = target.effects.clone();
    }
    Some(lowered.module.items)
}

#[derive(Default, Debug)]
struct Tally {
    sources: usize,
    parsed: usize,
    with_rows: usize,
    discovered: usize,
    tests: usize,
    errors: usize,
    differ: usize,
}

fn run_differential(r: &Runner, stride: usize) -> Tally {
    let mut t = Tally::default();
    let mut shown = 0;
    for s in corpus().iter().step_by(stride) {
        t.sources += 1;
        let parsed = parser::parse_for_tests(&s.text);
        if !parsed.diagnostics.is_empty() {
            continue;
        }
        t.parsed += 1;
        let rust = rust_case_rows(&s.text, &parsed.module);
        let vesper = r.case_rows(&s.text, &parsed.module);
        if rust != vesper {
            t.differ += 1;
            if shown < 8 {
                shown += 1;
                eprintln!(
                    "case rows differ: {}\n  rust:   {rust:?}\n  vesper: {vesper:?}",
                    s.label
                );
            }
            continue;
        }
        let Ok(rows) = rust else {
            t.errors += 1;
            continue;
        };
        if !rows.rows.is_empty() {
            t.with_rows += 1;
        }
        let Some(items) = expanded_items(&s.text, &rows) else {
            continue;
        };
        t.discovered += 1;
        let rust = rust_discover(&s.text, &items, &rows.rows);
        let vesper = r.discover(&s.text, &items, &rows.rows);
        if rust != vesper {
            t.differ += 1;
            if shown < 8 {
                shown += 1;
                eprintln!(
                    "discovery differs: {}\n  rust:   {rust:?}\n  vesper: {vesper:?}",
                    s.label
                );
            }
            continue;
        }
        match rust {
            Ok(tests) => t.tests += tests.len(),
            Err(_) => t.errors += 1,
        }
    }
    eprintln!("{t:?}");
    t
}

#[test]
fn word_format_matches() {
    assert_eq!(optimized().format, words::FORMAT);
}

#[test]
fn discovery_matches_rust_on_the_corpus() {
    let t = run_differential(optimized(), 1);
    assert_eq!(t.differ, 0, "{t:?}");
    // The corpus really exercises discovery: tests, rows and errors.
    assert!(t.tests > 300, "{t:?}");
    assert!(t.with_rows > 30, "{t:?}");
    assert!(t.errors > 30, "{t:?}");
}

/// The unoptimized build answers the same (the debug compiler's path), on
/// every thirteenth source.
#[test]
fn discovery_matches_rust_unoptimized() {
    let t = run_differential(&runner(false), 13);
    assert_eq!(t.differ, 0, "{t:?}");
}

/// The runner's own time: discovery of every corpus source, Rust against
/// the optimized Vesper build, best of six ABBA rounds.
#[test]
#[ignore = "timing; TEST_RUNNER_SPEED=1 cargo test --release --test self_host_test_runner -- --ignored"]
fn discovery_speed() {
    if std::env::var_os("TEST_RUNNER_SPEED").is_none() {
        return;
    }
    let r = optimized();
    let inputs: Vec<_> = corpus()
        .iter()
        .filter_map(|s| {
            let parsed = parser::parse_for_tests(&s.text);
            if !parsed.diagnostics.is_empty() {
                return None;
            }
            let rows = rust_case_rows(&s.text, &parsed.module).ok()?;
            let items = expanded_items(&s.text, &rows)?;
            Some((s, parsed.module, rows, items))
        })
        .collect();
    let time = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        t.elapsed()
    };
    let rust = || {
        time(&mut || {
            for (s, module, rows, items) in &inputs {
                std::hint::black_box(rust_case_rows(&s.text, module).unwrap());
                std::hint::black_box(rust_discover(&s.text, items, &rows.rows).ok());
            }
        })
    };
    let vesper = || {
        time(&mut || {
            for (s, module, rows, items) in &inputs {
                std::hint::black_box(r.case_rows(&s.text, module).unwrap());
                std::hint::black_box(r.discover(&s.text, items, &rows.rows).ok());
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
    // The crossing alone: encoding every input (the result decode is a
    // copy of the words).
    let crossing = (0..3)
        .map(|_| {
            time(&mut || {
                for (s, module, rows, items) in &inputs {
                    std::hint::black_box(words::case_rows_input(&s.text, module));
                    std::hint::black_box(words::discover_input(&s.text, items, &rows.rows));
                }
            })
        })
        .min()
        .unwrap();
    eprintln!("encoding the inputs: {crossing:?}");
    let (rb, vb) = (*rs.iter().min().unwrap(), *vs.iter().min().unwrap());
    eprintln!(
        "ABBA discovery over {} sources: rust {rb:?} vesper {vb:?} ratio {:.2}",
        inputs.len(),
        vb.as_secs_f64() / rb.as_secs_f64()
    );
}
