//! The Vesper driver's package load (#1986): the Rust loader reads the
//! package graph and the driver assembles it with
//! `@vesper/check-names-packages` (modules/driver/src/assemble.ves). For
//! every Loom package of the corpus (the loader's accept and reject
//! fixtures included), `vesper check` must print what the default compiler
//! prints, and at `VESPER_HYBRID_STATS=2` the assembled package must equal
//! `packages::load_for_target`'s (module, qualified names, diagnostics).
//!
//! `VESPER_DRIVER_PACKAGES_STRIDE=n` checks every nth package (Bazel);
//! Cargo checks them all. `DRIVER_PACKAGES_SPEED=1` runs the ignored ABBA
//! timing of the load against the Rust assembler.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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

/// The default compiler, as in tests/driver.rs.
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

/// Every Loom package directory under `examples/`, `packages/`, `crates/`
/// and `tests/fixtures/`, in path order.
fn corpus_packages(root: &Path) -> Vec<PathBuf> {
    fn find(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        if dir.join("Loom.toml").is_file() {
            out.push(dir.to_path_buf());
        }
        for path in entries {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if path.is_dir() && !name.starts_with('.') && name != "target" {
                find(&path, out);
            }
        }
    }
    let mut out = vec![];
    for dir in ["examples", "packages", "crates", "tests/fixtures"] {
        find(&root.join(dir), &mut out);
    }
    out
}

fn run(compiler: &Path, cwd: &Path, args: &[&str], stats: Option<&str>) -> Output {
    let mut c = Command::new(compiler);
    c.current_dir(cwd)
        .args(args)
        .env_remove("VESPER_HYBRID_STATS");
    if let Some(level) = stats {
        c.env("VESPER_HYBRID_STATS", level);
    }
    c.output()
        .unwrap_or_else(|e| panic!("{}: {e}", compiler.display()))
}

/// The hybrid's `VESPER_HYBRID_STATS` report lines and the rest of stderr.
fn split_report(stderr: &[u8]) -> (String, Vec<u8>) {
    let text = String::from_utf8_lossy(stderr);
    let (mut report, mut rest) = (String::new(), String::new());
    for line in text.split_inclusive('\n') {
        if line.starts_with("vesper-hybrid: ") {
            report.push_str(line);
        } else {
            rest.push_str(line);
        }
    }
    (report, rest.into_bytes())
}

/// `assemble answered <n> package loads from Vesper (<m> assembled in
/// Rust)`: `(n, m)`.
fn answered(report: &str) -> (u64, u64) {
    let Some(tail) = report
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: assemble answered "))
    else {
        return (0, 0);
    };
    let n = tail.split(' ').next().and_then(|n| n.parse().ok());
    let m = tail
        .split('(')
        .nth(1)
        .and_then(|t| t.split(' ').next())
        .and_then(|n| n.parse().ok());
    (n.unwrap_or(0), m.unwrap_or(0))
}

/// `cost assemble`'s `<n> differ` field, when the assembly ran.
fn differ(report: &str) -> Option<u64> {
    let line = report
        .lines()
        .find(|l| l.starts_with("vesper-hybrid: cost assemble: "))?;
    if line.contains(": 0 calls") {
        return None;
    }
    let before = line.split(" differ").next()?;
    before.rsplit(' ').next()?.parse().ok()
}

/// `assemble: package load <ms> ms`.
fn load_ms(report: &str) -> f64 {
    report
        .lines()
        .find_map(|l| l.strip_prefix("vesper-hybrid: assemble: package load "))
        .and_then(|t| t.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0.0)
}

#[test]
fn package_load_matches_the_default_compiler() {
    let root = repository();
    let default = default_compiler();
    let hybrid = hybrid();
    let stride: usize = std::env::var("VESPER_DRIVER_PACKAGES_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let packages: Vec<PathBuf> = corpus_packages(&root).into_iter().step_by(stride).collect();
    let mut mismatches = vec![];
    let (mut vesper, mut rust) = (0, 0);
    let (mut d_time, mut h_time) = (Duration::ZERO, Duration::ZERO);
    for package in &packages {
        let shown = package.strip_prefix(&root).unwrap_or(package);
        let shown = shown.to_string_lossy().replace('\\', "/");
        for args in [&["check"][..], &["check", "--json"][..]] {
            if args.len() > 1 && !shown.starts_with("tests/fixtures/packages") {
                continue;
            }
            let mut argv = args.to_vec();
            argv.push(&shown);
            let t = Instant::now();
            let d = run(&default, &root, &argv, None);
            d_time += t.elapsed();
            // `VESPER_HYBRID_STATS=time` also loads the package with the
            // Rust loader and compares (`cost assemble`'s `differ`); the
            // report lines are left out of the compared stderr.
            let t = Instant::now();
            let h = run(&hybrid, &root, &argv, Some("time"));
            h_time += t.elapsed();
            let (report, stderr) = split_report(&h.stderr);
            let mut diff = vec![];
            if d.status.code() != h.status.code() {
                diff.push("exit status");
            }
            if d.stdout != h.stdout {
                diff.push("stdout");
            }
            if d.stderr != stderr {
                diff.push("stderr");
            }
            if !diff.is_empty() {
                mismatches.push(format!(
                    "{}: {} (default {:?}, hybrid {:?})",
                    argv.join(" "),
                    diff.join(", "),
                    String::from_utf8_lossy(&d.stderr),
                    String::from_utf8_lossy(&stderr),
                ));
            }
            if args.len() > 1 {
                continue;
            }
            let (assembled, fallback) = answered(&report);
            vesper += assembled;
            rust += fallback;
            match differ(&report) {
                Some(0) => {}
                Some(n) => mismatches.push(format!(
                    "{shown}: the Vesper assembly differs from packages::load_for_target ({n})"
                )),
                // A package the Rust loader assembled (sugar, manifest
                // errors) or a command that stopped before loading.
                None if assembled == 0 => {}
                None => mismatches.push(format!("{shown}: no assemble cost line in {report:?}")),
            }
        }
    }
    eprintln!(
        "driver package load: {} packages ({vesper} assembled in Vesper, {rust} in Rust); \
         check default {:.2} s, hybrid {:.2} s ({:.3}x); {} differ",
        packages.len(),
        d_time.as_secs_f64(),
        h_time.as_secs_f64(),
        h_time.as_secs_f64() / d_time.as_secs_f64().max(1e-9),
        mismatches.len()
    );
    assert!(
        vesper > 0,
        "no package was assembled in Vesper (stride {stride})"
    );
    assert!(
        mismatches.is_empty(),
        "the Vesper package load differs on {} runs:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// ABBA timing of the driver's package load (the hybrid's
/// `assemble: package load` stats line: the Rust read plus the Vesper
/// assembly and read-back, or plus the Rust assembler) with the Vesper
/// assembly on and off (`VESPER_HYBRID_ASSEMBLE=0`), over every package the
/// Vesper driver assembles (or every nth), through `vesper check`: rounds of
/// on/off/off/on, the ratio's mean and 95% confidence interval.
#[test]
#[ignore = "timing; DRIVER_PACKAGES_SPEED=1"]
fn package_load_speed() {
    if std::env::var_os("DRIVER_PACKAGES_SPEED").is_none() {
        return;
    }
    let root = repository();
    let hybrid = hybrid();
    let rounds: usize = std::env::var("DRIVER_PACKAGES_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
        .max(2);
    // `vesper check` checks each package too, so a full pass takes minutes:
    // `DRIVER_PACKAGES_SPEED_STRIDE=n` times every nth package.
    let stride: usize = std::env::var("DRIVER_PACKAGES_SPEED_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let packages: Vec<String> = corpus_packages(&root)
        .iter()
        .step_by(stride)
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .filter(|p| {
            let h = run(&hybrid, &root, &["check", p], Some("1"));
            answered(&split_report(&h.stderr).0).0 == 1
        })
        .collect();
    let time = |on: bool| -> f64 {
        let mut ms = 0.0;
        for p in &packages {
            let mut c = Command::new(&hybrid);
            c.current_dir(&root)
                .args(["check", p])
                .env("VESPER_HYBRID_STATS", "1");
            if !on {
                c.env("VESPER_HYBRID_ASSEMBLE", "0");
            }
            let h = c.output().expect("run the hybrid");
            ms += load_ms(&split_report(&h.stderr).0);
        }
        ms
    };
    let _ = (time(true), time(false));
    let (mut ratios, mut on_ms, mut off_ms) = (vec![], 0.0, 0.0);
    for _ in 0..rounds {
        let a1 = time(true);
        let b1 = time(false);
        let b2 = time(false);
        let a2 = time(true);
        on_ms += a1 + a2;
        off_ms += b1 + b2;
        ratios.push((a1 + a2) / (b1 + b2));
    }
    let n = ratios.len() as f64;
    let mean = ratios.iter().sum::<f64>() / n;
    let var = ratios.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    // Student's t at 97.5% for n - 1 degrees of freedom (normal past 30).
    let t = [
        12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160,
        2.145, 2.131, 2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056,
        2.052, 2.048, 2.045,
    ]
    .get(ratios.len() - 2)
    .copied()
    .unwrap_or(1.96);
    let half = t * (var / n).sqrt();
    eprintln!(
        "driver package load speed: {} packages, {rounds} ABBA rounds: load on {:.1} ms, off \
         {:.1} ms per pass; Vesper/Rust {mean:.3}x (95% CI {:.3}..{:.3})",
        packages.len(),
        on_ms / (2.0 * rounds as f64),
        off_ms / (2.0 * rounds as f64),
        mean - half,
        mean + half
    );
}
