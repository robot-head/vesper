//! The front-end passes the Vesper driver runs itself (hd_step group 2,
//! #1893, modules/driver/src/front.ves): `--emit cst` and the parse the
//! Rust session's later passes read, for a standalone source. The hybrid
//! and the default compiler run `emit --emit cst` (to stdout and to a
//! file) and `check` over every `.ves` file of the repository's corpus
//! (examples/, tests/, the Vesper packages and the crates' Vesper ports)
//! plus a few sources written for the dump's edge cases, and must agree
//! on exit status, stdout, stderr and the file written.
//!
//! `VESPER_DRIVER_FRONT_STRIDE=<n>` runs every n-th file (default 1).
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

/// Sources for the dump's edge cases: long tokens cut at a char boundary,
/// escapes in the token text, non-ASCII, a syntax error (the session's
/// pass) and source sugar (the session's expansion).
const PROGRAMS: &[(&str, &str)] = &[
    (
        "long_tokens.ves",
        "fn main() -> i64 {\n    let a_very_long_identifier_name_here = 1;\n    let s = \"tab\\there \\\"quoted\\\" and 'single' and more\";\n    a_very_long_identifier_name_here\n}\n",
    ),
    (
        "unicode.ves",
        "// héllo wörld ✓ — a comment with multibyte chars at the cut point\n// 12345678901234567890é✓✓✓✓\nfn main() -> i64 {\n    let s = \"ünïcödé ✓ \\u{7f} text that is long enough\";\n    0\n}\n",
    ),
    (
        "controls.ves",
        "fn main() -> i64 {\n\t// a tab-indented comment\r\n    0\r\n}\r\n",
    ),
    ("empty.ves", ""),
    ("syntax.ves", "fn main( -> i64 {\n"),
    ("semantic.ves", "fn main() -> i64 {\n    nope\n}\n"),
    (
        "sugar.ves",
        "sugar take_right: infix(`.+`, additive, left) { rule ($a:expr .+ $b:expr) => { $b } }\nfn main() -> i64 {\n    1 .+ 42\n}\n",
    ),
];

fn ves_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') {
                ves_files(&path, out);
            }
        } else if name.ends_with(".ves") {
            out.push(path);
        }
    }
}

fn corpus(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![];
    for dir in ["examples", "tests", "packages"] {
        ves_files(&root.join(dir), &mut files);
    }
    if let Ok(crates) = std::fs::read_dir(root.join("crates")) {
        let mut crates: Vec<_> = crates.flatten().map(|e| e.path()).collect();
        crates.sort();
        for c in crates {
            ves_files(&c.join("vesper"), &mut files);
        }
    }
    files
}

fn run(compiler: &Path, cwd: &Path, args: &[String], out: &Path) -> (Output, Option<Vec<u8>>) {
    let _ = std::fs::remove_file(out);
    let output = Command::new(compiler)
        .current_dir(cwd)
        .env_remove("VESPER_HYBRID_STATS")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", compiler.display()));
    let written = std::fs::read(out).ok();
    let _ = std::fs::remove_file(out);
    (output, written)
}

#[test]
fn front_passes_match_the_default_compiler() {
    let root = repository();
    let default = default_compiler();
    let hybrid = hybrid();
    let scratch = std::env::temp_dir().join(format!("vesper-driver-front-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let mut inputs = vec![];
    for (name, text) in PROGRAMS {
        let path = scratch.join(name);
        std::fs::write(&path, text).unwrap();
        inputs.push((path, true));
    }
    let stride: usize = std::env::var("VESPER_DRIVER_FRONT_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    for (i, path) in corpus(&root).into_iter().enumerate() {
        if i % stride == 0 {
            // `check` over examples/ and tests/ only: the packages' files
            // need their package to check.
            let check =
                path.starts_with(root.join("examples")) || path.starts_with(root.join("tests"));
            inputs.push((path, check));
        }
    }
    let out = scratch.join("out.txt");
    let missing = scratch.join("missing").join("out.txt");
    let mut cases = 0usize;
    let mut differences = vec![];
    for (input, check) in &inputs {
        let input = input.display().to_string();
        let mut sets: Vec<Vec<String>> = vec![
            vec!["emit".into(), "--emit".into(), "cst".into(), input.clone()],
            vec![
                "emit".into(),
                "--emit".into(),
                "cst".into(),
                "-o".into(),
                out.display().to_string(),
                input.clone(),
            ],
        ];
        if *check {
            sets.push(vec!["check".into(), input.clone()]);
            sets.push(vec!["check".into(), "--json".into(), input.clone()]);
            // `--no-opt`: the AST optimizer swap folds some programs
            // differently from Rust's on main too (tests/fixtures/
            // wide_literals.ves), which is not the parse this checks.
            sets.push(vec![
                "emit".into(),
                "--emit".into(),
                "hir".into(),
                "--no-opt".into(),
                input.clone(),
            ]);
        }
        if inputs.len() < 64 || cases == 0 {
            sets.push(vec![
                "emit".into(),
                "--emit".into(),
                "cst".into(),
                "-o".into(),
                missing.display().to_string(),
                input.clone(),
            ]);
        }
        for args in sets {
            cases += 1;
            let (d, dw) = run(&default, &root, &args, &out);
            let (h, hw) = run(&hybrid, &root, &args, &out);
            if d.status.code() != h.status.code()
                || d.stdout != h.stdout
                || d.stderr != h.stderr
                || dw != hw
            {
                differences.push(format!(
                    "{args:?}: status {:?}/{:?}\n--- default stderr\n{}\n--- hybrid stderr\n{}",
                    d.status.code(),
                    h.status.code(),
                    String::from_utf8_lossy(&d.stderr),
                    String::from_utf8_lossy(&h.stderr),
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    println!(
        "flip-gate differential differences={} cases={cases}",
        differences.len()
    );
    assert!(
        differences.is_empty(),
        "{} of {cases} cases differ:\n{}",
        differences.len(),
        differences.join("\n\n")
    );
}

/// The passes the session ran (`VESPER_HYBRID_STATS=1`).
fn passes(args: &[&str]) -> u64 {
    passes_with(args, "all")
}

fn passes_with(args: &[&str], swaps: &str) -> u64 {
    let output = Command::new(hybrid())
        .current_dir(repository())
        .env("VESPER_HYBRID_STATS", "1")
        .env("VESPER_HYBRID_SWAPS", swaps)
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
}

/// `--emit cst` of a standalone source runs no session pass after the
/// load (IsPackage, Load); a package still runs Parse and EmitCst.
#[test]
fn emit_cst_runs_in_the_vesper_driver() {
    assert_eq!(
        passes(&["emit", "--emit", "cst", "examples/counter.ves"]),
        2
    );
    // A syntax error goes to the session's passes (IsPackage, Load, Parse,
    // EmitCst).
    let scratch =
        std::env::temp_dir().join(format!("vesper-driver-front-cst-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let bad = scratch.join("bad.ves");
    std::fs::write(&bad, "fn main( -> i64 {\n").unwrap();
    assert_eq!(passes(&["emit", "--emit", "cst", bad.to_str().unwrap()]), 4);
    let _ = std::fs::remove_dir_all(&scratch);
    // The driver's parse is the parse swap's: with no swaps, the session
    // parses and dumps.
    assert_eq!(
        passes_with(&["emit", "--emit", "cst", "examples/counter.ves"], "none"),
        4
    );
}

/// A failing stdout fails `--emit cst` with the write error, as in `cli()`.
#[cfg(target_os = "linux")]
#[test]
fn emit_cst_reports_stdout_write_errors() {
    let full = || std::fs::File::create("/dev/full").expect("open /dev/full");
    let run = |compiler: PathBuf| {
        Command::new(compiler)
            .current_dir(repository())
            .env_remove("VESPER_HYBRID_STATS")
            .args(["emit", "--emit", "cst", "examples/counter.ves"])
            .stdout(full())
            .output()
            .expect("run the compiler")
    };
    let d = run(default_compiler());
    let h = run(hybrid());
    assert_eq!(d.status.code(), Some(1));
    assert_eq!(d.status.code(), h.status.code());
    assert_eq!(d.stderr, h.stderr);
}
