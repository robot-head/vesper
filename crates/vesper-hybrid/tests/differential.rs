//! The differential harness of the hybrid Stage 2 build (self-hosting SH0
//! lane 1, #1123): the default `vesper` and `vesper-hybrid` run the same
//! commands over the bootstrap corpus, `examples/` and every `.ves` file
//! under `tests/`, and must agree on every output: exit status, stdout,
//! stderr (diagnostics, `--json` too) and the bytes of each object file.
//!
//! The default compiler is `VESPER_DEFAULT_COMPILER`, else
//! `VESPER_PORT_COMPILER` (CI builds it once), else the in-tree one built by
//! `cargo build -p vesper --bin vesper` into this build's target directory.
//!
//! Under the `divergent` (or `divergent-parse`, `divergent-ir-check`,
//! `divergent-ir-inline`, `divergent-ir-opt`, `divergent-optimize`,
//! `divergent-pact`) feature the hybrid's layout (or parse, IR-check,
//! inliner, IR optimizer, AST optimizer, trait declaration pass) module is
//! deliberately wrong (build.rs), and the harness must
//! report differences instead: that is the proof it can fail.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn hybrid() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_vesper-hybrid"));
    // Bazel's launcher gives a bare name found on PATH.
    if path.is_absolute() {
        path
    } else {
        which(&path).unwrap_or(path)
    }
}

fn which(name: &Path) -> Option<PathBuf> {
    let suffixed = format!("{}{}", name.display(), std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| [dir.join(name), dir.join(&suffixed)])
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
}

fn default_compiler() -> PathBuf {
    for var in ["VESPER_DEFAULT_COMPILER", "VESPER_PORT_COMPILER"] {
        if let Some(path) = std::env::var_os(var) {
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                which(&path).unwrap_or(path)
            };
            assert!(path.is_file(), "{var}={} is not a file", path.display());
            return path;
        }
    }
    // target/<profile>/vesper-hybrid -> target/
    let hybrid = hybrid();
    let target = hybrid
        .parent()
        .and_then(Path::parent)
        .expect("target directory");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(repository())
        .args(["build", "--locked", "-p", "vesper", "--bin", "vesper"])
        .arg("--target-dir")
        .arg(target)
        .status()
        .expect("run cargo build --bin vesper");
    assert!(status.success(), "cargo build --bin vesper failed");
    target
        .join("debug")
        .join(format!("vesper{}", std::env::consts::EXE_SUFFIX))
}

/// Every corpus file, relative to the repository root, sorted.
fn corpus(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "ves") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    for dir in ["bootstrap/corpus", "examples", "tests"] {
        walk(&root.join(dir), &mut files);
    }
    files
        .into_iter()
        .map(|p| p.strip_prefix(root).unwrap().to_path_buf())
        .collect()
}

/// Files where the Vesper AST optimizer (#1249) settles an equal-cost tie
/// differently from egg: egg's extraction, in hash order, picks a
/// reordered tree of the source's size and rewrites the island, where the
/// Vesper e-graph keeps the source. The owner accepted equal-cost
/// extraction differences without egg hash-order emulation
/// (docs/parallel-results/self-host-optimize.md, part 2). Their `emit` runs
/// count as ties only when the hybrid with the Rust producer
/// (`VESPER_HYBRID_RUST_PRODUCER=1`; the Vesper gate stays) agrees with the
/// default compiler on them.
const OPTIMIZE_EQUAL_COST_TIES: &[&str] = &[
    "examples/traits/shapes.ves",
    "tests/fixtures/bounded_generics/nested.ves",
    "tests/fixtures/native_handlers/shared_locals.ves",
    "tests/fixtures/wide_literals.ves",
];

/// The commands run on each file. `{out}` is an output file.
const COMMANDS: &[&[&str]] = &[
    &["check", "--json"],
    &["emit", "--emit", "ir"],
    &["emit", "--emit", "object", "-o", "{out}"],
    // The wasm backend swap (#1289): the component bytes. `build` prints the
    // output path, so both compilers write the same one, in turn.
    &["build", "--target", "wasm32-wasip2", "-o", "{out}"],
];

struct Run {
    output: Output,
    written: Option<Vec<u8>>,
    time: Duration,
}

fn run(compiler: &Path, root: &Path, command: &[&str], file: &Path, out: &Path) -> Run {
    run_with(compiler, root, command, file, out, &[])
}

fn run_with(
    compiler: &Path,
    root: &Path,
    command: &[&str],
    file: &Path,
    out: &Path,
    env: &[(&str, &str)],
) -> Run {
    let _ = std::fs::remove_file(out);
    let mut cmd = Command::new(compiler);
    cmd.current_dir(root).env_remove("VESPER_HYBRID_STATS");
    cmd.envs(env.iter().copied());
    let mut wrote = false;
    for arg in command {
        if *arg == "{out}" {
            cmd.arg(out);
            wrote = true;
        } else {
            cmd.arg(arg);
        }
    }
    cmd.arg(file);
    let start = Instant::now();
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", compiler.display()));
    let time = start.elapsed();
    let written = if wrote { std::fs::read(out).ok() } else { None };
    Run {
        output,
        written,
        time,
    }
}

/// A wasm build the default compiler refuses for loops (Rust's wasm backend
/// does not lower them) and the hybrid's Vesper backend compiles: the port
/// goes beyond Rust here, so the runs cannot agree (the port's loop
/// lowering is checked against the evaluator in
/// tests/self_host_wasm_backend.rs).
fn loop_extension(command: &[&str], default: &Run, hybrid: &Run) -> bool {
    let refusal = vesper::loops::WASM_REFUSAL
        .split(';')
        .next()
        .unwrap_or_default();
    command.contains(&"wasm32-wasip2")
        && !default.output.status.success()
        && String::from_utf8_lossy(&default.output.stderr).contains(refusal)
        && hybrid.output.status.success()
}

fn differences(default: &Run, hybrid: &Run) -> Vec<&'static str> {
    let mut out = Vec::new();
    if default.output.status.code() != hybrid.output.status.code() {
        out.push("exit status");
    }
    if default.output.stdout != hybrid.output.stdout {
        out.push("stdout");
    }
    if default.output.stderr != hybrid.output.stderr {
        out.push("stderr");
    }
    if default.written != hybrid.written {
        out.push("object bytes");
    }
    out
}

#[test]
fn hybrid_matches_the_default_compiler() {
    let root = repository();
    let default = default_compiler();
    let hybrid = hybrid();
    let files = corpus(&root);
    assert!(files.len() > 500, "corpus has only {} files", files.len());
    let scratch = std::env::temp_dir().join(format!("vesper-hybrid-diff-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();

    let jobs: Mutex<Vec<(usize, &PathBuf)>> = Mutex::new(files.iter().enumerate().rev().collect());
    let mismatches = Mutex::new(Vec::new());
    let ties = Mutex::new(Vec::new());
    let totals = Mutex::new((Duration::ZERO, Duration::ZERO, 0usize));
    let extended = Mutex::new(0usize);
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(8));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let Some((i, file)) = jobs.lock().unwrap().pop() else {
                        break;
                    };
                    for (c, command) in COMMANDS.iter().enumerate() {
                        // One output path for both: each run reads its
                        // bytes back before the other starts.
                        let a = scratch.join(format!("{i}-{c}.o"));
                        let b = a.clone();
                        let d = run(&default, &root, command, file, &a);
                        let h = run(&hybrid, &root, command, file, &b);
                        if loop_extension(command, &d, &h) {
                            *extended.lock().unwrap() += 1;
                            continue;
                        }
                        let mut diff = differences(&d, &h);
                        let tie_file = OPTIMIZE_EQUAL_COST_TIES
                            .iter()
                            .any(|t| Path::new(t) == file.as_path());
                        // A tie only changes the emitted IR, object or wasm
                        // component: both runs succeed with the same
                        // diagnostics.
                        let wasm = command.contains(&"wasm32-wasip2");
                        let artifact_only = diff
                            .iter()
                            .all(|d| *d == "stdout" || *d == "object bytes")
                            && d.output.status.success()
                            && (command.get(2) == Some(&"object") || wasm || diff == ["stdout"]);
                        if !diff.is_empty()
                            && tie_file
                            && (command[0] == "emit" || wasm)
                            && artifact_only
                        {
                            let r = run_with(
                                &hybrid,
                                &root,
                                command,
                                file,
                                &b,
                                &[("VESPER_HYBRID_RUST_PRODUCER", "1")],
                            );
                            if differences(&d, &r).is_empty() {
                                ties.lock().unwrap().push(format!(
                                    "{} {}",
                                    command.join(" "),
                                    file.display()
                                ));
                                diff.clear();
                            }
                        }
                        {
                            let mut t = totals.lock().unwrap();
                            t.0 += d.time;
                            t.1 += h.time;
                            t.2 += 1;
                        }
                        if !diff.is_empty() {
                            mismatches.lock().unwrap().push(format!(
                                "{} {}: {}",
                                command.join(" "),
                                file.display(),
                                diff.join(", ")
                            ));
                        }
                    }
                }
            });
        }
    });
    let _ = std::fs::remove_dir_all(&scratch);
    let (d, h, runs) = *totals.lock().unwrap();
    let mut mismatches = mismatches.into_inner().unwrap();
    mismatches.sort();
    let ties = ties.into_inner().unwrap();
    eprintln!(
        "differential: {} runs differ only by an accepted optimizer equal-cost tie",
        ties.len()
    );
    eprintln!(
        "differential: {} files, {runs} runs per compiler; default {:.2} s, hybrid {:.2} s ({:.3}x); \
         {} differ; {} wasm builds with loops only the hybrid compiles",
        files.len(),
        d.as_secs_f64(),
        h.as_secs_f64(),
        h.as_secs_f64() / d.as_secs_f64(),
        mismatches.len(),
        extended.into_inner().unwrap()
    );
    if cfg!(any(
        feature = "divergent",
        feature = "divergent-parse",
        feature = "divergent-ir-check",
        feature = "divergent-ir-inline",
        feature = "divergent-ir-opt",
        feature = "divergent-optimize",
        feature = "divergent-pact"
    )) {
        assert!(
            !mismatches.is_empty(),
            "the divergent hybrid build agreed with the default compiler on every run: \
             the differential harness cannot fail"
        );
        eprintln!("divergent build caught, first: {}", mismatches[0]);
    } else {
        assert!(
            mismatches.is_empty(),
            "the hybrid compiler differs from the default on {} runs:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }
}

/// The pilot module really runs from Vesper in the hybrid build, and only
/// there.
#[test]
fn pilot_module_runs_from_vesper() {
    let root = repository();
    let out = std::env::temp_dir().join(format!("vesper-hybrid-stats-{}.o", std::process::id()));
    let output = Command::new(hybrid())
        .current_dir(&root)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["emit", "--emit", "object", "-o"])
        .arg(&out)
        .arg("examples/counter.ves")
        .output()
        .expect("run vesper-hybrid");
    let _ = std::fs::remove_file(&out);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let calls: Option<u64> = stderr
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: layout answered "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok());
    assert!(calls.is_some(), "no layout report in {stderr:?}");
    let parses: u64 = stderr
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: parse answered "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no parse report in {stderr:?}"));
    assert!(parses > 0, "the Vesper parse module answered no calls");
    // SH2 lane 5 (#1177): the whole layout and the IR check. With the whole
    // layout swapped, the pilot's word arithmetic runs only under the Rust
    // fallback, so its count may be 0 here (tests/kernel.rs still checks it).
    // SH3 lane 11 (#1250): the inliner, on the optimized object. SH3 lane 9
    // (#1248): the IR optimizer, on the same object.
    for module in ["check-layout", "ir-check", "ir-inline", "ir-opt"] {
        let prefix = format!("vesper-hybrid: {module} answered ");
        let calls: u64 = stderr
            .lines()
            .find_map(|l| l.strip_prefix(prefix.as_str()))
            .and_then(|l| l.split(' ').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no {module} report in {stderr:?}"));
        assert!(calls > 0, "the Vesper {module} module answered no calls");
    }
    // SH3 lane 10 (#1249): the AST optimizer's producer runs on every
    // visited expression of the optimized module.
    let proposed: u64 = stderr
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: optimize proposed for "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no optimize report in {stderr:?}"));
    assert!(
        proposed > 0,
        "the Vesper optimize module proposed for nothing"
    );
    // SH3 lane 12 (#1251): the `hoist` hook hoists every native program.
    let hoisted: u64 = stderr
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: bounds-hoist hoisted "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no bounds-hoist report in {stderr:?}"));
    assert!(
        hoisted > 0,
        "the Vesper bounds-hoist module hoisted no program"
    );
}

/// The trait declaration pass (#1185) really runs from Vesper in the hybrid
/// build: it answers the accepted shapes example, and it rejects an impl
/// that leaves out an associated type. The stats report is read from the
/// command that succeeds: a failing command exits before the report on
/// Windows (#1486).
#[test]
fn pact_module_runs_from_vesper() {
    let root = repository();
    let output = Command::new(hybrid())
        .current_dir(&root)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["check", "examples/traits/shapes.ves"])
        .output()
        .expect("run vesper-hybrid");
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let calls: u64 = stderr
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: pact answered "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no pact report in {stderr:?}"));
    assert!(calls > 0, "the Vesper pact module answered no calls");
    let bad = std::env::temp_dir().join(format!("vesper-hybrid-pact-{}.ves", std::process::id()));
    std::fs::write(
        &bad,
        "struct Point { x: i64, y: i64 }\n\
         trait Show { type Out: mod copy; fn show(in self) -> Self::Out; }\n\
         impl Show for Point { fn show(in self) -> i64 { 1 } }\n\
         fn main() -> i64 { 0 }\n",
    )
    .expect("write the rejected source");
    let output = Command::new(hybrid())
        .current_dir(&root)
        .arg("check")
        .arg(&bad)
        .output()
        .expect("run vesper-hybrid");
    let _ = std::fs::remove_file(&bad);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr.contains("missing associated type 'Out'"),
        "{stderr:?}"
    );
}

/// The wasm backend module (#1289) really compiles from Vesper in the
/// hybrid build, and its component is wrapped from Vesper (#1334):
/// `transaction.ves` has no handler and no host capability.
#[test]
fn wasm_module_runs_from_vesper() {
    let root = repository();
    let out = std::env::temp_dir().join(format!("vesper-hybrid-wasm-{}.wasm", std::process::id()));
    let output = Command::new(hybrid())
        .current_dir(&root)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["build", "--target", "wasm32-wasip2", "-o"])
        .arg(&out)
        .arg("examples/transaction.ves")
        .output()
        .expect("run vesper-hybrid");
    let _ = std::fs::remove_file(&out);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let compiled: Option<u64> = stderr
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: wasm backend compiled "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok());
    assert_eq!(compiled, Some(1), "{stderr}");
    // ... and @vesper/wit-component wraps its component (#1334).
    let wrapped: Option<u64> = stderr
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: wit-component wrapped "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok());
    assert_eq!(wrapped, Some(1), "{stderr}");
}
