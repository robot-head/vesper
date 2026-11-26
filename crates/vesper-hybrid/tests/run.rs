//! The `run` swap (Stage 2 retirement, #2053): `vesper-hybrid run`
//! evaluates through `@vesper/run` (modules/run) and must print what it
//! prints with the swap off (`VESPER_HYBRID_RUN=0`, Rust's `Runtime`): the
//! same stdout, stderr and exit status for every single-file program of
//! `examples/` and `tests/fixtures/` with a `main`, at the default fuel
//! and at a small limit. The stats report must show programs the Vesper
//! machine evaluated, so the swap is proved installed.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn hybrid() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_vesper-hybrid"));
    if path.is_absolute() {
        return path;
    }
    // Bazel's launcher gives a bare name found on PATH.
    let suffixed = format!("{}{}", path.display(), std::env::consts::EXE_SUFFIX);
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .flat_map(|dir| [dir.join(&path), dir.join(&suffixed)])
                .find(|p| p.is_file())
        })
        .unwrap_or(path)
}

fn programs() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
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
            } else if p.extension().is_some_and(|e| e == "ves")
                && std::fs::read_to_string(&p).is_ok_and(|s| s.contains("fn main("))
            {
                out.push(p);
            }
        }
    }
    let mut out = vec![];
    for d in ["examples", "tests/fixtures"] {
        walk(&repository().join(d), &mut out);
    }
    out
}

fn run(file: &Path, fuel: Option<&str>, swap: bool) -> Output {
    let mut cmd = Command::new(hybrid());
    cmd.current_dir(repository())
        .env_remove("VESPER_HYBRID_SWAPS")
        .env_remove("VESPER_HYBRID_GATED")
        .env("VESPER_HYBRID_STATS", "1")
        .env("VESPER_HYBRID_RUN", if swap { "1" } else { "0" })
        .arg("run");
    if let Some(fuel) = fuel {
        cmd.args(["--fuel", fuel]);
    }
    cmd.arg(file).output().expect("run vesper-hybrid")
}

/// stderr without the stats report, which differs by design.
fn diagnostics(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr)
        .lines()
        .filter(|l| !l.starts_with("vesper-hybrid: "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `run evaluated N programs from Vesper` count.
fn evaluated(o: &Output) -> u64 {
    String::from_utf8_lossy(&o.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: run evaluated "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

#[test]
fn run_swap_prints_what_rust_prints() {
    let mut differences = vec![];
    let mut from_vesper = 0;
    let files = programs();
    for file in &files {
        for fuel in [None, Some("50")] {
            let ours = run(file, fuel, true);
            let rust = run(file, fuel, false);
            from_vesper += evaluated(&ours);
            assert_eq!(evaluated(&rust), 0, "{}: the swap is off", file.display());
            if ours.status.code() != rust.status.code()
                || ours.stdout != rust.stdout
                || diagnostics(&ours) != diagnostics(&rust)
            {
                differences.push(format!(
                    "{} (fuel {fuel:?}):\n  vesper: {:?} {:?} {:?}\n  rust:   {:?} {:?} {:?}",
                    file.display(),
                    ours.status.code(),
                    String::from_utf8_lossy(&ours.stdout),
                    diagnostics(&ours),
                    rust.status.code(),
                    String::from_utf8_lossy(&rust.stdout),
                    diagnostics(&rust),
                ));
            }
        }
    }
    eprintln!(
        "run swap: {} programs, {from_vesper} runs from Vesper, {} differences",
        files.len(),
        differences.len()
    );
    for d in differences.iter().take(10) {
        eprintln!("{d}");
    }
    assert!(differences.is_empty(), "{} differences", differences.len());
    assert!(from_vesper > 0, "no program ran in the Vesper evaluator");
}
