//! The Lean extraction module's differential (self-hosting SH5 lane 18,
//! #1341): the default `vesper` and `vesper-hybrid`, whose
//! `vesper::lean::extract` comes from `@vesper/lean-extract`, run
//! `vesper emit --emit lean` over every `.ves` file of `examples/` and
//! `tests/fixtures/`, and must agree byte for byte on exit status, stdout
//! and stderr. A file whose `--emit hir` already differs (the AST
//! optimizer runs before extraction, and the hybrid swaps it too) is
//! compared again with `--no-ast-opt`, so the extraction itself is still
//! checked on it.
//!
//! `VESPER_LEAN_STRIDE=n` keeps every n-th file. Under the
//! `divergent-lean-extract` feature every module text comes back empty
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

fn resolve(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        which(&path).unwrap_or(path)
    }
}

fn hybrid() -> PathBuf {
    resolve(PathBuf::from(env!("CARGO_BIN_EXE_vesper-hybrid")))
}

/// `VESPER_DEFAULT_COMPILER`, else `VESPER_PORT_COMPILER`, else the
/// in-tree `vesper` built into this build's target directory (as in
/// tests/doc.rs).
fn default_compiler() -> PathBuf {
    for var in ["VESPER_DEFAULT_COMPILER", "VESPER_PORT_COMPILER"] {
        if let Some(path) = std::env::var_os(var) {
            let path = resolve(PathBuf::from(path));
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
    std::env::var("VESPER_LEAN_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1)
}

/// Every `.ves` file under `dirs` of the repository, relative to it and
/// sorted; hidden and `target` directories are skipped.
fn corpus(root: &Path, dirs: &[&str]) -> Vec<PathBuf> {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                if !name.starts_with('.') && name != "target" {
                    walk(&path, files);
                }
            } else if path.extension().is_some_and(|e| e == "ves") {
                files.push(path);
            }
        }
    }
    let mut files = vec![];
    for d in dirs {
        walk(&root.join(d), &mut files);
    }
    files
        .into_iter()
        .map(|p| p.strip_prefix(root).unwrap().to_path_buf())
        .step_by(stride())
        .collect()
}

fn emit(compiler: &Path, root: &Path, file: &Path, args: &[&str]) -> (Output, Duration) {
    let started = Instant::now();
    let output = Command::new(compiler)
        .current_dir(root)
        .arg("emit")
        .args(args)
        .arg(file)
        .output()
        .expect("run the compiler");
    (output, started.elapsed())
}

/// What differs between two runs: the status, stdout and stderr.
fn differences(d: &Output, h: &Output) -> Vec<String> {
    let mut diff = vec![];
    if d.status.code() != h.status.code() {
        diff.push(format!("status {:?} vs {:?}", d.status, h.status));
    }
    if d.stdout != h.stdout {
        diff.push("stdout".to_owned());
    }
    if d.stderr != h.stderr {
        diff.push(format!(
            "stderr {:?} vs {:?}",
            String::from_utf8_lossy(&d.stderr),
            String::from_utf8_lossy(&h.stderr)
        ));
    }
    diff
}

#[test]
fn hybrid_lean_matches_the_default_compiler() {
    let root = repository();
    let files = corpus(&root, &["examples", "tests/fixtures"]);
    let default = default_compiler();
    let hybrid = hybrid();
    let next = Mutex::new(0usize);
    let mismatches = Mutex::new(Vec::new());
    let optimized = Mutex::new(Vec::new());
    let totals = Mutex::new((Duration::ZERO, Duration::ZERO, 0usize, 0usize));
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = {
                        let mut n = next.lock().unwrap();
                        let i = *n;
                        *n += 1;
                        i
                    };
                    let Some(file) = files.get(i) else {
                        return;
                    };
                    let lean = ["--emit", "lean"];
                    let (d, dt) = emit(&default, &root, file, &lean);
                    let (h, ht) = emit(&hybrid, &root, file, &lean);
                    {
                        let mut t = totals.lock().unwrap();
                        t.0 += dt;
                        t.1 += ht;
                        if d.status.success() {
                            t.2 += 1;
                            t.3 += d.stdout.len();
                        }
                    }
                    let mut diff = differences(&d, &h);
                    if !diff.is_empty() {
                        let hir = ["--emit", "hir"];
                        let (dh, _) = emit(&default, &root, file, &hir);
                        let (hh, _) = emit(&hybrid, &root, file, &hir);
                        if !differences(&dh, &hh).is_empty() {
                            let plain = ["--emit", "lean", "--no-ast-opt"];
                            let (d, _) = emit(&default, &root, file, &plain);
                            let (h, _) = emit(&hybrid, &root, file, &plain);
                            diff = differences(&d, &h);
                            optimized.lock().unwrap().push(file.display().to_string());
                        }
                    }
                    if !diff.is_empty() {
                        mismatches.lock().unwrap().push(format!(
                            "{}: {}",
                            file.display(),
                            diff.join(", ")
                        ));
                    }
                }
            });
        }
    });
    let (d, h, ok, bytes) = *totals.lock().unwrap();
    let mut mismatches = mismatches.into_inner().unwrap();
    mismatches.sort();
    let mut optimized = optimized.into_inner().unwrap();
    optimized.sort();
    eprintln!(
        "lean differential: {} files ({ok} extracted, {bytes} bytes); default {:.2} s, \
         hybrid {:.2} s ({:.3}x); {} differ; {} compared without the AST optimizer, whose \
         HIR differs: {optimized:?}",
        files.len(),
        d.as_secs_f64(),
        h.as_secs_f64(),
        h.as_secs_f64() / d.as_secs_f64(),
        mismatches.len(),
        optimized.len()
    );
    assert!(ok * stride() > 20, "only {ok} files extracted");
    if cfg!(feature = "divergent-lean-extract") {
        assert!(
            !mismatches.is_empty(),
            "the divergent Lean extraction module agreed with the default compiler on every \
             file: the Lean differential cannot fail"
        );
        eprintln!(
            "divergent Lean extraction module caught, first: {}",
            mismatches[0]
        );
    } else {
        assert!(
            mismatches.is_empty(),
            "the hybrid's Lean differs from the default on {} files:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }
}

/// The Lean extraction module really runs from Vesper in the hybrid build.
#[test]
fn lean_extract_runs_from_vesper() {
    let root = repository();
    let output = Command::new(hybrid())
        .current_dir(&root)
        .env("VESPER_HYBRID_STATS", "1")
        .args(["emit", "--emit", "lean", "examples/counter.ves"])
        .output()
        .expect("run vesper-hybrid");
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let calls: u64 = stderr
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: lean_extract built "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no lean_extract report in {stderr:?}"));
    assert!(
        calls > 0,
        "the Vesper Lean extraction module built no module"
    );
}
