//! The Vesper driver end to end (self-hosting SH5 lane 12, #1335): the
//! hybrid's `main` is `@vesper/hybrid-driver` (modules/driver), which parses
//! the arguments with `@vesper/driver`, runs each compiler pass through the
//! hybrid's Rust and links through `@vesper-std/process`. Here the hybrid
//! and the default compiler run every option set the CLI suites use for
//! `check`, `run`, `jit`, `build` and `emit` over `examples/`, the package
//! fixtures and a few programs written for the build and error paths, and
//! must agree on exit status, stdout, stderr and every file written.
//!
//! The corpus-wide `check`/`emit` differential is tests/differential.rs;
//! the parser's own differential is the root package's
//! tests/self_host_driver.rs.
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

/// The default compiler, as in tests/differential.rs.
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

/// Programs for the paths the corpus does not reach.
const PROGRAMS: &[(&str, &str)] = &[
    ("ok.ves", "fn main() -> i64 {\n    40 + 2\n}\n"),
    ("unit.ves", "fn main() {\n}\n"),
    ("args.ves", "fn main(x: i64) -> i64 {\n    x\n}\n"),
    ("entry.ves", "fn other() -> i64 {\n    7\n}\n"),
    ("syntax.ves", "fn main( -> i64 {\n"),
    ("semantic.ves", "fn main() -> i64 {\n    nope\n}\n"),
    (
        "lib.ves",
        "pub fn add(a: u64, b: u64) -> u64 {\n    wrapping_add(a, b)\n}\n",
    ),
];

/// Option sets, each run on every input. `{out}` is a fresh output path
/// in the run's directory, `{dir}` a missing directory.
const OPTION_SETS: &[&[&str]] = &[
    &["check"],
    &["check", "--json"],
    &["check", "--include-tests"],
    &["run"],
    &["run", "--no-opt"],
    &["run", "--fuel", "3"],
    &["run", "--entry", "other"],
    &["jit"],
    &["jit", "--no-opt"],
    &["emit", "--emit", "hir"],
    &["emit", "--emit", "ir"],
    &["emit", "--emit", "ir", "--no-opt"],
    &["emit", "--emit", "cst"],
    &["emit", "--emit", "lean"],
    &["emit", "--emit", "clif"],
    &["emit", "--emit", "object", "-o", "{out}"],
    &["emit", "--emit", "object"],
    &[
        "emit",
        "--emit",
        "interface",
        "--package",
        "@t/x",
        "-o",
        "{out}",
    ],
    &["emit", "--emit", "interface"],
    &["emit", "--emit", "bogus"],
    &["build", "-o", "{out}"],
    &["build", "-o", "{out}", "--no-opt"],
    &["build", "-o", "{out}", "--entry", "other"],
    &["build", "-o", "{dir}/a.out"],
    &["build", "-o", "{out}", "--linker", "vesper-no-such-linker"],
    &["build", "-o", "{out}", "--codegen", "other"],
    &["build", "-o", "{out}", "--codegen", "verified", "--tee"],
    &[
        "build",
        "-o",
        "{out}",
        "--target",
        "wasm32-wasip2",
        "--verify-stack",
    ],
    &["build", "--lib", "-o", "{out}"],
    &["build", "--lib"],
    &["build", "--lib-name", "x", "-o", "{out}"],
    &["check", "--lib"],
    &["emit", "--emit", "clif", "--verify-stack"],
    &["build", "-o", "{out}", "--verify=stack,realtime"],
];

#[cfg(unix)]
const FALSE_LINKER: &[&str] = &["build", "-o", "{out}", "--linker", "false"];

struct Run {
    output: Output,
    written: Option<Vec<u8>>,
    time: Duration,
}

fn run(compiler: &Path, cwd: &Path, args: &[String], out: &Path) -> Run {
    let _ = std::fs::remove_file(out);
    let start = Instant::now();
    let output = Command::new(compiler)
        .current_dir(cwd)
        .env_remove("VESPER_HYBRID_STATS")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", compiler.display()));
    let time = start.elapsed();
    let written = std::fs::read(out).ok();
    let _ = std::fs::remove_file(out);
    Run {
        output,
        written,
        time,
    }
}

/// Replace the run's own output paths so both compilers' texts compare.
fn normalized(bytes: &[u8], out: &Path, tag: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let name = out.file_name().unwrap().to_string_lossy().into_owned();
    let stem = name.replace(tag, "{tag}");
    text.replace(&name, &stem).into_bytes()
}

#[test]
fn driver_matches_the_default_compiler() {
    let root = repository();
    let default = default_compiler();
    let hybrid = hybrid();
    let scratch = std::env::temp_dir().join(format!("vesper-hybrid-driver-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let mut inputs: Vec<PathBuf> = vec![];
    for (name, text) in PROGRAMS {
        let path = scratch.join(name);
        std::fs::write(&path, text).unwrap();
        inputs.push(path);
    }
    let mut examples: Vec<PathBuf> = std::fs::read_dir(root.join("examples"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ves"))
        .collect();
    examples.sort();
    inputs.extend(examples);
    inputs.push(root.join("examples/loom"));
    inputs.push(root.join("tests/fixtures/packages/workspace/app"));
    inputs.push(root.join("tests/fixtures/packages/reject/unknown_item"));
    inputs.push(scratch.join("missing.ves"));

    let mut sets: Vec<&[&str]> = OPTION_SETS.to_vec();
    #[cfg(unix)]
    sets.push(FALSE_LINKER);
    let mut mismatches = vec![];
    let (mut d_time, mut h_time, mut runs) = (Duration::ZERO, Duration::ZERO, 0);
    for (i, input) in inputs.iter().enumerate() {
        for (k, set) in sets.iter().enumerate() {
            let mut both = vec![];
            for (tag, compiler) in [("default", &default), ("hybrid", &hybrid)] {
                let out = scratch.join(format!("out-{i}-{k}-{tag}"));
                let mut args: Vec<String> = set
                    .iter()
                    .map(|a| match *a {
                        "{out}" => out.display().to_string(),
                        "{dir}/a.out" => scratch.join("no-such-dir/a.out").display().to_string(),
                        a => a.to_owned(),
                    })
                    .collect();
                args.push(input.display().to_string());
                let r = run(compiler, &scratch, &args, &out);
                both.push((r, out, tag));
            }
            let (d, h) = (&both[0], &both[1]);
            d_time += d.0.time;
            h_time += h.0.time;
            runs += 1;
            let mut diff = vec![];
            if d.0.output.status.code() != h.0.output.status.code() {
                diff.push("exit status");
            }
            if normalized(&d.0.output.stdout, &d.1, d.2)
                != normalized(&h.0.output.stdout, &h.1, h.2)
            {
                diff.push("stdout");
            }
            if normalized(&d.0.output.stderr, &d.1, d.2)
                != normalized(&h.0.output.stderr, &h.1, h.2)
            {
                diff.push("stderr");
            }
            // Executables embed nothing of their path; objects and texts
            // must be byte-identical.
            if d.0.written != h.0.written {
                diff.push("output bytes");
            }
            if !diff.is_empty() {
                mismatches.push(format!(
                    "{} {}: {} (default {:?}, hybrid {:?})",
                    set.join(" "),
                    input.display(),
                    diff.join(", "),
                    String::from_utf8_lossy(&d.0.output.stderr),
                    String::from_utf8_lossy(&h.0.output.stderr),
                ));
            }
        }
    }
    let leftovers: Vec<_> = std::fs::read_dir(&scratch)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".vesper.o"))
        .collect();
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!(
        "driver differential: {} inputs x {} option sets = {runs} runs; default {:.2} s, hybrid {:.2} s ({:.3}x); {} differ",
        inputs.len(),
        sets.len(),
        d_time.as_secs_f64(),
        h_time.as_secs_f64(),
        h_time.as_secs_f64() / d_time.as_secs_f64(),
        mismatches.len()
    );
    // A failed link leaves the object file behind in the Rust CLI; the
    // driver must leave exactly the same ones.
    let mut left: Vec<String> = leftovers
        .iter()
        .filter(|n| n.contains("-default"))
        .map(|n| n.replace("-default", "-{tag}"))
        .collect();
    let mut right: Vec<String> = leftovers
        .iter()
        .filter(|n| n.contains("-hybrid"))
        .map(|n| n.replace("-hybrid", "-{tag}"))
        .collect();
    left.sort();
    right.sort();
    assert_eq!(left, right, "object files left behind differ");
    assert!(
        mismatches.is_empty(),
        "the Vesper driver differs from the default CLI on {} runs:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// The compile commands really run in the Vesper driver, and the others
/// in the Rust CLI.
#[test]
fn compile_commands_run_in_the_vesper_driver() {
    let root = repository();
    let passes = |args: &[&str]| -> u64 {
        let output = Command::new(hybrid())
            .current_dir(&root)
            .env("VESPER_HYBRID_STATS", "1")
            .args(args)
            .output()
            .expect("run vesper-hybrid");
        let stderr = String::from_utf8_lossy(&output.stderr);
        stderr
            .lines()
            .find_map(|l| l.strip_prefix("vesper-hybrid: the driver ran "))
            .and_then(|l| l.split(' ').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no driver report in {stderr:?}"))
    };
    assert!(passes(&["check", "examples/counter.ves"]) >= 4);
    assert!(passes(&["emit", "--emit", "ir", "examples/counter.ves"]) >= 6);
    // Argument errors stop before the first pass.
    assert_eq!(passes(&["check", "--fuel"]), 0);
    // Other commands are the Rust CLI's.
    assert_eq!(passes(&["fmt", "--help"]), 0);
}

/// Help, the version banner, no arguments and an unknown command: the
/// driver's help path and the commands it hands back to the Rust CLI.
#[test]
fn help_and_version_match() {
    let default = default_compiler();
    let hybrid = hybrid();
    for args in [
        &["--help"][..],
        &["--version"],
        &[],
        &["check", "--help"],
        &["build", "-h", "--fuel"],
        &["nope"],
    ] {
        let d = Command::new(&default).args(args).output().unwrap();
        let h = Command::new(&hybrid).args(args).output().unwrap();
        assert_eq!(d.status.code(), h.status.code(), "{args:?}");
        assert_eq!(d.stdout, h.stdout, "{args:?}");
        assert_eq!(d.stderr, h.stderr, "{args:?}");
    }
}
