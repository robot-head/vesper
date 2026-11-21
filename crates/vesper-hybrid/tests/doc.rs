//! The doc module's differential (self-hosting SH5 lane 15, #1338): the
//! default `vesper` and `vesper-hybrid`, whose `vesper_doc` renderers come
//! from `@vesper/doc`, run `vesper doc` over every repository `.ves` file
//! and every Loom package, and must agree byte for byte on every output:
//! exit status, stdout (the `--json` report), stderr, the HTML page written
//! with `-o`, and every file of a `--bundle` (file pages, the package
//! overview, the search page and the search index).
//!
//! `VESPER_DOC_STRIDE=n` keeps every n-th file and package. Under the
//! `divergent-doc` feature every linked page gains a trailing space
//! (build.rs), and the harness must report differences instead.
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

fn which(name: &Path) -> Option<PathBuf> {
    let suffixed = format!("{}{}", name.display(), std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| [dir.join(name), dir.join(&suffixed)])
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
}

fn hybrid() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_vesper-hybrid"));
    if path.is_absolute() {
        path
    } else {
        which(&path).unwrap_or(path)
    }
}

/// `VESPER_DEFAULT_COMPILER`, else `VESPER_PORT_COMPILER`, else the
/// in-tree `vesper` built into this build's target directory (as in
/// tests/differential.rs).
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
    let hybrid = hybrid();
    let target = hybrid
        .parent()
        .and_then(Path::parent)
        .expect("target directory");
    let profile = hybrid
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .unwrap_or("debug")
        .to_string();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = Command::new(cargo);
    build
        .current_dir(repository())
        .args(["build", "--locked", "-p", "vesper", "--bin", "vesper"])
        .arg("--target-dir")
        .arg(target);
    if profile == "release" {
        build.arg("--release");
    }
    assert!(
        build
            .status()
            .expect("run cargo build --bin vesper")
            .success(),
        "cargo build --bin vesper failed"
    );
    target
        .join(profile)
        .join(format!("vesper{}", std::env::consts::EXE_SUFFIX))
}

fn stride() -> usize {
    std::env::var("VESPER_DOC_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1)
}

/// Every `.ves` file and every directory holding a `Loom.toml`, relative to
/// the repository root, sorted; hidden, `target` and `bazel-*` directories
/// are skipped.
fn corpus(root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>, packages: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                if name.starts_with('.') || name == "target" || name.starts_with("bazel-") {
                    continue;
                }
                if path.join("Loom.toml").is_file() {
                    packages.push(path.clone());
                }
                walk(&path, files, packages);
            } else if path.extension().is_some_and(|e| e == "ves") {
                files.push(path);
            }
        }
    }
    let (mut files, mut packages) = (Vec::new(), Vec::new());
    walk(root, &mut files, &mut packages);
    let rel = |v: Vec<PathBuf>| -> Vec<PathBuf> {
        v.into_iter()
            .map(|p| p.strip_prefix(root).unwrap().to_path_buf())
            .step_by(stride())
            .collect()
    };
    (rel(files), rel(packages))
}

/// One `vesper doc` run: its output and every file it wrote, in path order.
struct Run {
    output: Output,
    written: Vec<(PathBuf, Vec<u8>)>,
    time: Duration,
}

fn read_tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.push((
                    p.strip_prefix(base).unwrap().to_path_buf(),
                    std::fs::read(&p).unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Runs `vesper doc <args> <input>` with `{out}` replaced by a fresh
/// scratch path under `scratch`.
fn run(compiler: &Path, root: &Path, args: &[&str], input: &Path, scratch: &Path) -> Run {
    let _ = std::fs::remove_dir_all(scratch);
    std::fs::create_dir_all(scratch).unwrap();
    let out = scratch.join("out");
    let mut cmd = Command::new(compiler);
    cmd.current_dir(root)
        .env_remove("VESPER_HYBRID_STATS")
        .arg("doc");
    for arg in args {
        if *arg == "{out}" {
            cmd.arg(&out);
        } else {
            cmd.arg(arg);
        }
    }
    cmd.arg(input);
    let start = Instant::now();
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", compiler.display()));
    let time = start.elapsed();
    let written = if out.is_dir() {
        read_tree(&out)
    } else {
        std::fs::read(&out)
            .map(|b| vec![(PathBuf::from("out"), b)])
            .unwrap_or_default()
    };
    Run {
        output,
        written,
        time,
    }
}

fn differences(default: &Run, hybrid: &Run) -> Vec<String> {
    let mut out = Vec::new();
    if default.output.status.code() != hybrid.output.status.code() {
        out.push("exit status".into());
    }
    if default.output.stdout != hybrid.output.stdout {
        out.push("stdout".into());
    }
    if default.output.stderr != hybrid.output.stderr {
        out.push("stderr".into());
    }
    let names = |r: &Run| r.written.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>();
    if names(default) != names(hybrid) {
        out.push("written file names".into());
    } else {
        for ((p, a), (_, b)) in default.written.iter().zip(&hybrid.written) {
            if a != b {
                out.push(format!("{}", p.display()));
            }
        }
    }
    out
}

/// The commands run on each file and each package.
const FILE_COMMANDS: &[&[&str]] = &[&["--json"], &["-o", "{out}"], &["--bundle", "{out}"]];
const PACKAGE_COMMANDS: &[&[&str]] = &[&["--json"], &["--bundle", "{out}"]];

#[test]
fn hybrid_docs_match_the_default_compiler() {
    let root = repository();
    let default = default_compiler();
    let hybrid = hybrid();
    let (files, packages) = corpus(&root);
    assert!(
        files.len() * stride() > 500,
        "corpus has only {} files",
        files.len()
    );
    let scratch = std::env::temp_dir().join(format!("vesper-hybrid-doc-{}", std::process::id()));
    let mut jobs: Vec<(PathBuf, &[&str])> = Vec::new();
    for f in &files {
        for c in FILE_COMMANDS {
            jobs.push((f.clone(), c));
        }
    }
    for p in &packages {
        for c in PACKAGE_COMMANDS {
            jobs.push((p.clone(), c));
        }
    }
    let total = jobs.len();
    let jobs = Mutex::new(jobs.into_iter().enumerate().rev().collect::<Vec<_>>());
    let mismatches = Mutex::new(Vec::new());
    // (default time, hybrid time, successful runs, written bytes)
    let totals = Mutex::new((Duration::ZERO, Duration::ZERO, 0usize, 0usize));
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(8));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let Some((i, (input, args))) = jobs.lock().unwrap().pop() else {
                        break;
                    };
                    let d = run(
                        &default,
                        &root,
                        args,
                        &input,
                        &scratch.join(format!("{i}-d")),
                    );
                    let h = run(
                        &hybrid,
                        &root,
                        args,
                        &input,
                        &scratch.join(format!("{i}-h")),
                    );
                    let _ = std::fs::remove_dir_all(scratch.join(format!("{i}-d")));
                    let _ = std::fs::remove_dir_all(scratch.join(format!("{i}-h")));
                    let diff = differences(&d, &h);
                    {
                        let mut t = totals.lock().unwrap();
                        t.0 += d.time;
                        t.1 += h.time;
                        if d.output.status.success() {
                            t.2 += 1;
                            t.3 += d.output.stdout.len()
                                + d.written.iter().map(|(_, b)| b.len()).sum::<usize>();
                        }
                    }
                    if !diff.is_empty() {
                        mismatches.lock().unwrap().push(format!(
                            "doc {} {}: {}",
                            args.join(" "),
                            input.display(),
                            diff.join(", ")
                        ));
                    }
                }
            });
        }
    });
    let _ = std::fs::remove_dir_all(&scratch);
    let (d, h, ok, bytes) = *totals.lock().unwrap();
    let mut mismatches = mismatches.into_inner().unwrap();
    mismatches.sort();
    eprintln!(
        "doc differential: {} files and {} packages, {total} runs per compiler ({ok} documented, \
         {bytes} bytes written); default {:.2} s, hybrid {:.2} s ({:.3}x); {} differ",
        files.len(),
        packages.len(),
        d.as_secs_f64(),
        h.as_secs_f64(),
        h.as_secs_f64() / d.as_secs_f64(),
        mismatches.len()
    );
    assert!(ok * stride() > 100, "only {ok} runs documented anything");
    if cfg!(feature = "divergent-doc") {
        assert!(
            !mismatches.is_empty(),
            "the divergent doc module agreed with the default compiler on every run: \
             the doc differential cannot fail"
        );
        eprintln!("divergent doc module caught, first: {}", mismatches[0]);
    } else {
        assert!(
            mismatches.is_empty(),
            "the hybrid docs differ from the default on {} runs:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }
}

/// The doc module really runs from Vesper in the hybrid build.
#[test]
fn doc_module_runs_from_vesper() {
    let root = repository();
    let output = Command::new(hybrid())
        .current_dir(&root)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["doc", "--json", "examples/counter.ves"])
        .output()
        .expect("run vesper-hybrid");
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let calls: u64 = stderr
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: doc answered "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no doc report in {stderr:?}"));
    assert!(calls > 0, "the Vesper doc module answered no calls");
}
