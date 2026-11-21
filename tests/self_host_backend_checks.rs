//! SH4 lane 17 (#1291): `@vesper/backend-checks` against
//! `src/native_verifiers.rs`.
//!
//! Every case emits a program with the real native backend, encodes the
//! emitted Cranelift functions (tests/common/backend_checks_input.rs), runs
//! the Vesper verifiers on them and compares the dump of the report with
//! Rust's byte for byte. The cases are the verifier CLI fixture under every
//! policy the CLI tests use (plus an all-verifier policy), the capability
//! fixtures through the capability policies, and every corpus program with a
//! `main` under a synthesized policy, on both object ISAs, optimized and not.
//!
//! - `BACKEND_CHECKS_STRIDE=n` keeps every n-th corpus program (callgrind).
//! - `CHECK_HARNESS_SPEED=1` runs the ABBA timing of the ignored test.

#[path = "common/check_harness.rs"]
mod check_harness;

#[path = "common/backend_checks_input.rs"]
mod backend_checks_input;

use backend_checks_input::{Mode, encode, rust_text};
use check_harness::{Entry, Package, abba, first_difference, repo, speed, time};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;
use vesper::check::World;
use vesper::native::EmittedFunction;
use vesper::native_verifiers::{self as nv, NativeVerifier, NativeVerifierPolicy};

/// The differential runs the unoptimized JIT build of the package: the IR
/// optimizer takes about 27 minutes on it in a debug compiler (see the
/// report's compiler gaps). `optimized_package_agrees` and the timing run
/// the optimized build.
const JIT_OPT: bool = false;

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| {
        let t = std::time::Instant::now();
        let p = Package::load("packages/vesper-backend-checks");
        eprintln!("backend-checks checked in {:?}", t.elapsed());
        p
    })
}

fn entry(optimize: bool) -> &'static Entry {
    static OPT: OnceLock<Entry> = OnceLock::new();
    static NOOPT: OnceLock<Entry> = OnceLock::new();
    let cell = if optimize { &OPT } else { &NOOPT };
    cell.get_or_init(|| {
        let t = std::time::Instant::now();
        let e = package().entry("backend_verify", optimize);
        eprintln!(
            "backend_verify compiled (optimize={optimize}) in {:?}",
            t.elapsed()
        );
        e
    })
}

fn checked(source: &str) -> Option<World> {
    let parsed = vesper::parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let checked = vesper::check::check(&parsed.module);
    checked.diagnostics.is_empty().then_some(checked.world)
}

fn all_verifiers() -> BTreeSet<NativeVerifier> {
    [
        NativeVerifier::ConstTime,
        NativeVerifier::Stack,
        NativeVerifier::Realtime,
        NativeVerifier::DetFloat,
    ]
    .into_iter()
    .collect()
}

fn policy(json: &str) -> NativeVerifierPolicy {
    NativeVerifierPolicy::parse(&format!(
        r#"{{"schema":"vesper.native-verifiers/1"{}{json}}}"#,
        if json.is_empty() { "" } else { "," }
    ))
    .unwrap()
}

/// One comparison: `Ok(())` or the first differing line.
struct Run<'a> {
    label: String,
    world: &'a World,
    functions: &'a [EmittedFunction],
    target: &'a str,
    file: &'a str,
    source: &'a str,
}

impl Run<'_> {
    fn compare(&self, mode: &Mode, optimize_jit: bool) -> Result<(), String> {
        let rust = rust_text(
            self.world,
            self.functions,
            Some(self.target),
            mode,
            self.file,
            self.source,
        );
        let input = encode(
            self.world,
            self.functions,
            Some(self.target),
            mode,
            self.file,
            self.source,
        );
        if std::env::var("BACKEND_CHECKS_ONLY").is_ok_and(|o| !self.label.contains(&o)) {
            return Ok(());
        }
        if std::env::var_os("BACKEND_CHECKS_TRACE").is_some() {
            eprintln!("run {} ({} input bytes)", self.label, input.len());
        }
        let ves = entry(optimize_jit).text(&input);
        match first_difference(&rust, &ves) {
            None => Ok(()),
            Some(d) => Err(format!(
                "{}: {d}\n--- rust\n{rust}--- vesper\n{ves}",
                self.label
            )),
        }
    }
}

fn emit(world: &World, main: &str, target: &str, optimize: bool) -> Option<Vec<EmittedFunction>> {
    nv::emit_and_collect(world, main, Some(target), optimize)
        .ok()
        .map(|(_, _, f)| f)
}

fn report(failures: &[String], runs: usize) {
    eprintln!("backend-checks: {runs} runs, {} differ", failures.len());
    assert!(
        failures.is_empty(),
        "{} of {runs} runs differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The policies of tests/native_verifiers_cli.rs, a default one and an
/// all-verifier one over every entry.
fn cli_policies() -> Vec<(&'static str, BTreeSet<NativeVerifier>, NativeVerifierPolicy)> {
    use NativeVerifier::*;
    let one = |v: NativeVerifier| [v].into_iter().collect::<BTreeSet<_>>();
    vec![
        (
            "ct",
            one(ConstTime),
            policy(r#""const_time":{"select":{"secret":["secret"]}}"#),
        ),
        (
            "ct-clean",
            one(ConstTime),
            policy(r#""const_time":{"forward":{"secret":["secret"]}}"#),
        ),
        ("stack", one(Stack), policy("")),
        (
            "stack-tight",
            one(Stack),
            policy(r#""stack":{"roots":{"ct_main":8}}"#),
        ),
        (
            "stack-roomy",
            one(Stack),
            policy(r#""stack":{"roots":{"ct_main":4096}}"#),
        ),
        (
            "rt-rec",
            one(Realtime),
            policy(r#""realtime":{"roots":{"rec_main":null}}"#),
        ),
        (
            "rt-float",
            one(Realtime),
            policy(r#""realtime":{"roots":{"float_main":null}}"#),
        ),
        (
            "rt-declared",
            one(Realtime),
            policy(
                r#""realtime":{"roots":{"float_main":null},"symbols":{"fmod":{"effects":[],"frame_bytes":64}}}"#,
            ),
        ),
        (
            "rt-clean",
            one(Realtime),
            policy(r#""realtime":{"roots":{"clean_main":null}}"#),
        ),
        (
            "rt-alloc",
            one(Realtime),
            policy(r#""realtime":{"roots":{"clean_main":null},"effects":{"forward":["Alloc"]}}"#),
        ),
        (
            "rt-bounded",
            one(Realtime),
            policy(
                r#""realtime":{"roots":{"rec_main":4096,"clean_main":16},"reject_unbounded_loops":true,"max_loop_iterations":8}"#,
            ),
        ),
        ("df", one(DetFloat), policy("")),
        (
            "df-fma",
            one(DetFloat),
            policy(
                r#""det_float":{"functions":["wrap","float_main"],"allow_fma":true,"allowed_float_calls":["fmod"],"reduction_order":"sequential"}"#,
            ),
        ),
        (
            "all",
            all_verifiers(),
            policy(
                r#""const_time":{"select":{"secret":["secret"],"declassify":["a"]},"forward":{"secret":["secret"],"constant_time_callees":["select"]}},"stack":{"roots":{"ct_main":64,"rec_main":64,"float_main":256},"external_bounds":{"fmod":128}},"realtime":{"roots":{"ct_main":null,"float_main":512},"allowed_effects":["Alloc"]}"#,
            ),
        ),
    ]
}

#[test]
fn verifier_fixture_reports_match_rust() {
    let path = repo().join("tests/fixtures/integration_b/verifiers.ves");
    let source = std::fs::read_to_string(&path).unwrap();
    let world = checked(&source).unwrap();
    let file = "verifiers.ves";
    let mut failures = vec![];
    let mut runs = 0;
    for main in ["ct_main", "clean_main", "rec_main", "float_main"] {
        for target in TARGETS {
            for optimize in [true, false] {
                let functions = emit(&world, main, target, optimize).unwrap();
                for (name, verifiers, p) in cli_policies() {
                    let run = Run {
                        label: format!("{main} {target} opt={optimize} {name}"),
                        world: &world,
                        functions: &functions,
                        target,
                        file,
                        source: &source,
                    };
                    runs += 1;
                    if let Err(e) = run.compare(&Mode::Emitted(&verifiers, &p), JIT_OPT) {
                        failures.push(e);
                    }
                }
            }
        }
    }
    report(&failures, runs);
}

fn capability_files() -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for d in [
        "tests/fixtures/capability_verifiers/accept",
        "tests/fixtures/capability_verifiers/reject",
        "tests/fixtures/capability_effects/accept",
    ] {
        let Ok(entries) = std::fs::read_dir(repo().join(d)) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "ves") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn rel(p: &Path) -> String {
    p.strip_prefix(repo()).unwrap_or(p).display().to_string()
}

#[test]
fn capability_reports_match_rust() {
    let none = BTreeSet::new();
    let df: BTreeSet<_> = [NativeVerifier::DetFloat].into_iter().collect();
    let base = policy("");
    let narrowed = policy(r#""det_float":{"functions":["main"]},"stack":{"roots":{"main":4096}}"#);
    let mut failures = vec![];
    let mut runs = 0;
    for path in capability_files() {
        let source = std::fs::read_to_string(&path).unwrap();
        let Some(world) = checked(&source) else {
            continue;
        };
        let file = rel(&path);
        for target in TARGETS {
            for optimize in [true, false] {
                let Some(functions) = emit(&world, "main", target, optimize) else {
                    continue;
                };
                for (name, requested, p) in [
                    ("caps", &none, &base),
                    ("caps+df", &df, &base),
                    ("caps+df narrowed", &df, &narrowed),
                ] {
                    let run = Run {
                        label: format!("{file} {target} opt={optimize} {name}"),
                        world: &world,
                        functions: &functions,
                        target,
                        file: &file,
                        source: &source,
                    };
                    runs += 1;
                    if let Err(e) = run.compare(&Mode::Capabilities(requested, p), JIT_OPT) {
                        failures.push(e);
                    }
                }
            }
        }
    }
    assert!(runs > 0);
    report(&failures, runs);
}

fn stride() -> usize {
    std::env::var("BACKEND_CHECKS_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1)
}

/// Single-file corpus programs with a `main`, checked.
fn corpus() -> Vec<(String, String, World)> {
    let mut files = vec![];
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                if !p.join("Loom.toml").exists() {
                    walk(&p, out);
                }
            } else if p.extension().is_some_and(|e| e == "ves") {
                out.push(p);
            }
        }
    }
    walk(&repo().join("tests/fixtures"), &mut files);
    walk(&repo().join("examples"), &mut files);
    let mut out = vec![];
    for p in files.into_iter().step_by(stride()) {
        let Ok(source) = std::fs::read_to_string(&p) else {
            continue;
        };
        if !source.contains("fn main(") {
            continue;
        }
        let Some(world) = checked(&source) else {
            continue;
        };
        out.push((rel(&p), source, world));
    }
    out
}

/// Every verifier on, with `main` as a stack and realtime root and every
/// function's first parameter secret.
fn synthesized(world: &World) -> NativeVerifierPolicy {
    let mut ct = vec![];
    for f in world.functions.values() {
        if let Some(param) = f.params.first() {
            ct.push(format!(r#""{}":{{"secret":["{}"]}}"#, f.name, param.name));
        }
    }
    policy(&format!(
        r#""const_time":{{{}}},"stack":{{"roots":{{"main":256}}}},"realtime":{{"roots":{{"main":null}}}}"#,
        ct.join(",")
    ))
}

/// Runs `f` on a thread with a 512 MiB stack: debug builds of native
/// lowering recurse deeply on some corpus programs.
fn big_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(512 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn corpus_reports_match_rust() {
    big_stack(corpus_reports);
}

fn corpus_reports() {
    let all = all_verifiers();
    let none = BTreeSet::new();
    let base = policy("");
    let mut failures = vec![];
    let mut runs = 0;
    let mut programs = 0;
    for (file, source, world) in corpus() {
        let p = synthesized(&world);
        let mut any = false;
        for target in TARGETS {
            for optimize in [true, false] {
                let Some(functions) = emit(&world, "main", target, optimize) else {
                    continue;
                };
                any = true;
                let run = Run {
                    label: format!("{file} {target} opt={optimize}"),
                    world: &world,
                    functions: &functions,
                    target,
                    file: &file,
                    source: &source,
                };
                runs += 2;
                if let Err(e) = run.compare(&Mode::Emitted(&all, &p), JIT_OPT) {
                    failures.push(e);
                }
                if let Err(e) = run.compare(&Mode::Capabilities(&none, &base), JIT_OPT) {
                    failures.push(e);
                }
            }
        }
        programs += usize::from(any);
    }
    eprintln!("backend-checks corpus: {programs} programs");
    assert!(programs > 0);
    report(&failures, runs);
}

/// The optimized JIT build of the package agrees with Rust too. Ignored in
/// debug builds of the compiler, where the IR optimizer takes about 27
/// minutes on the package; run it with `--profile port-compiler`.
#[test]
#[ignore = "optimizes the package; run under the port-compiler profile"]
fn optimized_package_agrees() {
    let path = repo().join("tests/fixtures/integration_b/verifiers.ves");
    let source = std::fs::read_to_string(&path).unwrap();
    let world = checked(&source).unwrap();
    let mut failures = vec![];
    let mut runs = 0;
    for target in TARGETS {
        let functions = emit(&world, "float_main", target, false).unwrap();
        for (name, verifiers, p) in cli_policies() {
            let run = Run {
                label: format!("float_main {target} {name} (optimized JIT)"),
                world: &world,
                functions: &functions,
                target,
                file: "verifiers.ves",
                source: &source,
            };
            runs += 1;
            if let Err(e) = run.compare(&Mode::Emitted(&verifiers, &p), true) {
                failures.push(e);
            }
        }
    }
    report(&failures, runs);
}

/// ABBA timing of the verification (decoding subtracted on the Vesper side),
/// over every verifier and without the stack verifier. Rust's stack verifier
/// compiles each function to machine code, which the Vesper input carries
/// precomputed, so only the run without it compares like with like.
#[test]
#[ignore = "timing; run with CHECK_HARNESS_SPEED=1"]
fn backend_checks_speed() {
    if !speed() {
        return;
    }
    let count = package().entry("backend_verify_count", true);
    let decode = package().entry("backend_verify_decode", true);
    let all = all_verifiers();
    let mut no_stack = all.clone();
    no_stack.remove(&NativeVerifier::Stack);
    let programs: Vec<_> = corpus()
        .into_iter()
        .filter_map(|(file, source, world)| {
            let functions = emit(&world, "main", TARGETS[0], true)?;
            let p = synthesized(&world);
            Some((file, source, world, functions, p))
        })
        .collect();
    for (label, verifiers) in [("all verifiers", &all), ("without stack", &no_stack)] {
        let inputs: Vec<Vec<u8>> = programs
            .iter()
            .map(|(file, source, world, functions, p)| {
                encode(
                    world,
                    functions,
                    Some(TARGETS[0]),
                    &Mode::Emitted(verifiers, p),
                    file,
                    source,
                )
            })
            .collect();
        let rounds = 5;
        let mut rust = || {
            time(|| {
                for _ in 0..rounds {
                    for (_, _, world, functions, p) in &programs {
                        let r =
                            nv::verify_emitted(world, functions, Some(TARGETS[0]), verifiers, p);
                        std::hint::black_box(r.map(|r| r.findings.len()).ok());
                    }
                }
            })
        };
        let mut dec_time = std::time::Duration::MAX;
        let mut ves = || {
            let full = time(|| {
                for _ in 0..rounds {
                    for input in &inputs {
                        std::hint::black_box(count.word(input));
                    }
                }
            });
            let dec = time(|| {
                for _ in 0..rounds {
                    for input in &inputs {
                        std::hint::black_box(decode.word(input));
                    }
                }
            });
            dec_time = dec_time.min(dec);
            full.saturating_sub(dec)
        };
        abba(
            &format!("backend checks, {label} ({} programs)", programs.len()),
            &mut rust,
            &mut ves,
        );
        eprintln!("decode (subtracted): {dec_time:?}");
    }
}
