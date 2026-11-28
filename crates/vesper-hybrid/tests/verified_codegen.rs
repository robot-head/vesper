//! The verified code generator's differential (#1886, part 4d-2):
//! `vesper-hybrid`, whose `vesper::verified_codegen::build` comes from
//! `@vesper/verified-codegen`, and `vesper-hybrid` with that swap off
//! (`VESPER_HYBRID_VERIFIED=0`, Rust's build behind the same front end:
//! the hybrid's AST optimizer already reassociates some programs
//! differently from the default compiler's) run `vesper build --codegen
//! verified` over every `.ves` file of `examples/` and `tests/fixtures/`
//! for the three Linux targets (the macOS and Windows objects need their
//! linkers; the package's own
//! differential covers their bytes), and must agree on exit status,
//! stdout, stderr and the executable's bytes.
//!
//! `VESPER_VERIFIED_STRIDE=n` keeps every n-th file. Under the
//! `divergent-verified-codegen` feature every build fails (build.rs), and
//! the harness must report differences instead.
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

fn stride() -> usize {
    std::env::var("VESPER_VERIFIED_STRIDE")
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

const TARGETS: [&str; 3] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "riscv64gc-unknown-linux-gnu",
];

/// `vesper build --codegen verified` of `file` for `target` in a fresh
/// directory, writing `out` (`rust`: with the swap off): the run and the
/// executable's bytes.
fn build(
    compiler: &Path,
    rust: bool,
    root: &Path,
    file: &Path,
    target: &str,
    dir: &Path,
) -> (Output, Option<Vec<u8>>, Duration) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("create the build directory");
    let started = Instant::now();
    let mut command = Command::new(compiler);
    if rust {
        command.env("VESPER_HYBRID_VERIFIED", "0");
    }
    let output = command
        .current_dir(dir)
        .args([
            "build",
            "--codegen",
            "verified",
            "--target",
            target,
            "-o",
            "out",
        ])
        .arg(root.join(file))
        .output()
        .expect("run the compiler");
    let elapsed = started.elapsed();
    let bytes = std::fs::read(dir.join("out")).ok();
    (output, bytes, elapsed)
}

#[test]
fn hybrid_verified_builds_match_rust() {
    let root = repository();
    let files = corpus(&root, &["examples", "tests/fixtures"]);
    let hybrid = hybrid();
    let scratch =
        std::env::temp_dir().join(format!("vesper-hybrid-verified-{}", std::process::id()));
    let jobs: Vec<(PathBuf, &str)> = files
        .iter()
        .flat_map(|f| TARGETS.iter().map(move |t| (f.clone(), *t)))
        .collect();
    let next = Mutex::new(0usize);
    let mismatches = Mutex::new(Vec::new());
    let totals = Mutex::new((Duration::ZERO, Duration::ZERO, 0usize));
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
                    let Some((file, target)) = jobs.get(i) else {
                        return;
                    };
                    let (d, db, dt) = build(
                        &hybrid,
                        true,
                        &root,
                        file,
                        target,
                        &scratch.join(format!("{i}-d")),
                    );
                    let (h, hb, ht) = build(
                        &hybrid,
                        false,
                        &root,
                        file,
                        target,
                        &scratch.join(format!("{i}-h")),
                    );
                    {
                        let mut t = totals.lock().unwrap();
                        t.0 += dt;
                        t.1 += ht;
                        if d.status.success() {
                            t.2 += 1;
                        }
                    }
                    let mut diff = vec![];
                    if d.status.code() != h.status.code() {
                        diff.push(format!("status {:?} vs {:?}", d.status, h.status));
                    }
                    if d.stdout != h.stdout {
                        diff.push(format!(
                            "stdout {:?} vs {:?}",
                            String::from_utf8_lossy(&d.stdout),
                            String::from_utf8_lossy(&h.stdout)
                        ));
                    }
                    if d.stderr != h.stderr {
                        diff.push(format!(
                            "stderr {:?} vs {:?}",
                            String::from_utf8_lossy(&d.stderr),
                            String::from_utf8_lossy(&h.stderr)
                        ));
                    }
                    if db != hb {
                        diff.push("executable bytes".to_owned());
                    }
                    if !diff.is_empty() {
                        mismatches.lock().unwrap().push(format!(
                            "{} ({target}): {}",
                            file.display(),
                            diff.join(", ")
                        ));
                    }
                }
            });
        }
    });
    let _ = std::fs::remove_dir_all(&scratch);
    let (d, h, built) = *totals.lock().unwrap();
    let mut mismatches = mismatches.into_inner().unwrap();
    mismatches.sort();
    eprintln!(
        "verified codegen differential: {} builds ({built} built); Rust {:.2} s, Vesper {:.2} s \
         ({:.3}x); {} differ",
        jobs.len(),
        d.as_secs_f64(),
        h.as_secs_f64(),
        h.as_secs_f64() / d.as_secs_f64(),
        mismatches.len()
    );
    assert!(built * stride() > 20, "only {built} builds");
    if cfg!(feature = "divergent-verified-codegen") {
        assert!(
            !mismatches.is_empty(),
            "the divergent verified code generator agreed with Rust's build on every \
             build: the differential cannot fail"
        );
        eprintln!(
            "divergent verified code generator caught, first: {}",
            mismatches[0]
        );
    } else {
        assert!(
            mismatches.is_empty(),
            "the Vesper verified builds differ from Rust's on {} builds:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }
}

/// `vesper build --codegen verified` of `examples/fibonacci.ves` with the
/// hybrid's statistics on: the stats line that holds `what`.
fn reported(what: &str) -> String {
    let root = repository();
    let dir =
        std::env::temp_dir().join(format!("vesper-hybrid-verified-one-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let output = Command::new(hybrid())
        .current_dir(&dir)
        .env("VESPER_HYBRID_STATS", "1")
        .args([
            "build",
            "--codegen",
            "verified",
            "--target",
            "x86_64-unknown-linux-gnu",
            "-o",
            "out",
        ])
        .arg(root.join("examples/fibonacci.ves"))
        .output()
        .expect("run vesper-hybrid");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr
        .lines()
        .find(|l| l.contains(what))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("no {what:?} report in {stderr:?}"))
}

/// The Vesper driver builds the executable with @vesper/verified-codegen
/// and writes it itself, with no build left to Rust's code generator.
#[test]
fn verified_build_runs_in_the_vesper_driver() {
    let line = reported(" verified executables in Vesper ");
    assert_eq!(
        line,
        "vesper-hybrid: driver built 1 verified executables in Vesper (0 built by Rust's code generator)",
    );
}
