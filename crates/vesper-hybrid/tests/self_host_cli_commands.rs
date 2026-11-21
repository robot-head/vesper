//! The Vesper command layers end to end (self-hosting SH5 lane 13, #1336):
//! the hybrid's driver hands every command off the compile path to
//! `@vesper/driver-commands` (packages/vesper-driver-commands), which runs
//! the ported ones and hands the rest back to the Rust CLI. Here the hybrid
//! and the default compiler run every argument vector of each ported
//! command's CLI tests, plus the edge cases of its argument loop, each in a
//! fresh directory with the same fixture files, and must agree on exit
//! status, stdout, stderr and every file in the directory afterwards.
//!
//! Ported: no arguments, `--help`/`-h`, `--version`/`-V`, `traceparent`
//! (tests/foundation_cli.rs) and `simd` (tests/simd_cli.rs).
use std::collections::BTreeMap;
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

const VALID: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-ff";

/// Headers for `traceparent`: every error of the parser, in its order.
fn headers() -> Vec<String> {
    let mut h: Vec<String> = [
        VALID,
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-fe",
        "00-00000000000000000000000000000001-0000000000000001-01",
        "00-00000000000000010000000000000000-0000000000000001-01",
        "",
        "00",
        "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
        "00-00000000000000000000000000000000-0000000000000000-01",
        "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "0g-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e473G-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902bZ-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-0x",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
        "00_4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736_00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7_01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-1",
        " 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-1",
        "00-4bf92f3577b34da6a3ce929d0e0é4736-00f067aa0ba902b7-1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // An invalid digit at every hex offset.
    for at in [0usize, 1, 3, 10, 18, 19, 34, 36, 44, 51, 53, 54] {
        let mut b = VALID.as_bytes().to_vec();
        b[at] = b'x';
        h.push(String::from_utf8(b).unwrap());
    }
    h
}

/// Argument vectors (after the program name). `{in}` is the fixture
/// `input`, written before each run.
fn vectors() -> Vec<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let mut v: Vec<Vec<String>> = vec![
        // Top level.
        s(&[]),
        s(&["--help"]),
        s(&["-h"]),
        s(&["-h", "junk"]),
        s(&["--version"]),
        s(&["-V"]),
        s(&["--version", "check"]),
        // traceparent: tests/foundation_cli.rs and the argument loop.
        s(&["traceparent"]),
        s(&["traceparent", "--json"]),
        s(&["traceparent", "--bad"]),
        s(&["traceparent", "--json", "--bad"]),
        s(&["traceparent", "--bad", "--json"]),
        s(&["traceparent", "first", "second"]),
        s(&["traceparent", "one", "two", "--json"]),
        s(&["traceparent", "--help"]),
        s(&["traceparent", "-h"]),
        s(&["traceparent", "--json", "--help"]),
        s(&["traceparent", "--bad", "--help"]),
        s(&["traceparent", "--help", "--bad"]),
        s(&["traceparent", "one", "--help"]),
        s(&["traceparent", "one", "two", "--help"]),
        s(&["traceparent", "-"]),
        s(&["traceparent", "-x", "--json"]),
        s(&["traceparent", "--é", "--json"]),
        s(&["traceparent", "--a\tb\nc\"d\\e\u{1}\u{1f}\u{7f}", "--json"]),
        s(&["traceparent", "--json", "--json", VALID]),
        s(&["traceparent", VALID, "--json", "extra"]),
        // simd: tests/simd_cli.rs and the argument loop.
        s(&["simd"]),
        s(&["simd", "unknown"]),
        s(&["simd", "--help"]),
        s(&["simd", "-h"]),
        s(&["simd", "scan"]),
        s(&["simd", "scan", "input"]),
        s(&["simd", "scan", "input", "--delimiter"]),
        s(&["simd", "scan", "input", "--delimiter", "256"]),
        s(&["simd", "scan", "input", "--delimiter", "-1"]),
        s(&["simd", "scan", "input", "--delimiter", "+1"]),
        s(&["simd", "scan", "input", "--delimiter", ""]),
        s(&["simd", "scan", "input", "--delimiter", "1x"]),
        s(&["simd", "scan", "input", "--delimiter", "007"]),
        s(&[
            "simd",
            "scan",
            "input",
            "--delimiter",
            "000000000000000000000255",
        ]),
        s(&[
            "simd",
            "scan",
            "input",
            "--delimiter",
            "99999999999999999999999",
        ]),
        s(&[
            "simd",
            "scan",
            "input",
            "--delimiter",
            "1",
            "--delimiter",
            "2",
        ]),
        s(&[
            "simd",
            "scan",
            "input",
            "--profile",
            "avx2",
            "--profile",
            "baseline128",
        ]),
        s(&["simd", "scan", "input", "--profile"]),
        s(&[
            "simd",
            "scan",
            "input",
            "--profile",
            "nope",
            "--delimiter",
            "1",
        ]),
        s(&["simd", "scan", "input", "--json", "--json"]),
        s(&["simd", "scan", "input", "extra", "--delimiter", "1"]),
        s(&["simd", "scan", "input", "--unknown"]),
        s(&["simd", "scan", "--delimiter", "1"]),
        s(&["simd", "scan", "--delimiter", "1", "--", "-input"]),
        s(&["simd", "scan", "--delimiter", "1", "--", "--help"]),
        s(&["simd", "scan", "--delimiter", "1", "--", "input", "--"]),
        s(&["simd", "scan", "input", "--delimiter", "0", "--help"]),
        s(&["simd", "scan", "missing", "--delimiter", "0", "--json"]),
        s(&["simd", "scan", ".", "--delimiter", "0", "--json"]),
        s(&["simd", "scan", ".", "--delimiter", "0"]),
        s(&[
            "simd",
            "scan",
            "input",
            "--delimiter",
            "0",
            "--profile",
            "baseline128",
        ]),
        s(&[
            "simd",
            "scan",
            "input",
            "--delimiter",
            "0",
            "--profile",
            "baseline128",
            "--json",
        ]),
        s(&["simd", "help", "scan"]),
        // Handed back to the Rust CLI.
        s(&["fmt", "--help"]),
        s(&["nope"]),
    ];
    for profile in ["avx2", "avx512", "sve2", "rvv1"] {
        v.push(s(&[
            "simd",
            "scan",
            "missing",
            "--delimiter",
            "0",
            "--profile",
            profile,
            "--json",
        ]));
    }
    for d in ["0", "7", "255"] {
        v.push(s(&["simd", "scan", "input", "--delimiter", d, "--json"]));
        v.push(s(&["simd", "scan", "input", "--delimiter", d]));
    }
    for h in headers() {
        v.push(vec!["traceparent".into(), h.clone()]);
        v.push(vec!["traceparent".into(), h.clone(), "--json".into()]);
        v.push(vec!["traceparent".into(), "--json".into(), h]);
    }
    v
}

/// Fixture inputs: each argument vector runs once per input.
fn inputs() -> Vec<Vec<u8>> {
    [0usize, 1, 15, 16, 17, 65535, 65536, 65537, 131089]
        .iter()
        .map(|&len| (0..len).map(|i| ((i * 131 + 7) % 256) as u8).collect())
        .collect()
}

/// Every file under `dir`, by relative path.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(dir).unwrap().display().to_string();
                files.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    files
}

struct Run {
    output: Output,
    files: BTreeMap<String, Vec<u8>>,
    time: Duration,
}

fn run(compiler: &Path, dir: &Path, input: &[u8], args: &[String], stats: bool) -> Run {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("input"), input).unwrap();
    std::fs::write(dir.join("-input"), [1u8, 2, 1]).unwrap();
    let mut c = Command::new(compiler);
    c.current_dir(dir).args(args);
    if stats {
        c.env("VESPER_HYBRID_STATS", "1");
    } else {
        c.env_remove("VESPER_HYBRID_STATS");
    }
    let start = Instant::now();
    let output = c
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", compiler.display()));
    let time = start.elapsed();
    let files = snapshot(dir);
    Run {
        output,
        files,
        time,
    }
}

#[test]
fn command_layers_match_the_default_compiler() {
    let default = default_compiler();
    let hybrid = hybrid();
    let scratch =
        std::env::temp_dir().join(format!("vesper-hybrid-commands-{}", std::process::id()));
    let (d_dir, h_dir) = (scratch.join("default"), scratch.join("hybrid"));
    let inputs = inputs();
    let vectors = vectors();
    let mut mismatches = vec![];
    let (mut d_time, mut h_time, mut runs) = (Duration::ZERO, Duration::ZERO, 0);
    for args in &vectors {
        // Only `simd scan input` reads the input; the others run once.
        let reads = args.first().is_some_and(|a| a == "simd") && args.iter().any(|a| a == "input");
        let count = if reads { inputs.len() } else { 1 };
        for input in &inputs[inputs.len() - count..] {
            let d = run(&default, &d_dir, input, args, false);
            let h = run(&hybrid, &h_dir, input, args, false);
            d_time += d.time;
            h_time += h.time;
            runs += 1;
            let mut diff = vec![];
            if d.output.status.code() != h.output.status.code() {
                diff.push("exit status");
            }
            if d.output.stdout != h.output.stdout {
                diff.push("stdout");
            }
            if d.output.stderr != h.output.stderr {
                diff.push("stderr");
            }
            if d.files != h.files {
                diff.push("files");
            }
            if !diff.is_empty() {
                mismatches.push(format!(
                    "{args:?} on {} bytes: {} (default {:?} {:?}, hybrid {:?} {:?})",
                    input.len(),
                    diff.join(", "),
                    String::from_utf8_lossy(&d.output.stdout),
                    String::from_utf8_lossy(&d.output.stderr),
                    String::from_utf8_lossy(&h.output.stdout),
                    String::from_utf8_lossy(&h.output.stderr),
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!(
        "command layers differential: {} argument vectors, {runs} runs; default {:.2} s, hybrid {:.2} s ({:.3}x); {} differ",
        vectors.len(),
        d_time.as_secs_f64(),
        h_time.as_secs_f64(),
        h_time.as_secs_f64() / d_time.as_secs_f64(),
        mismatches.len()
    );
    assert!(
        mismatches.is_empty(),
        "the Vesper command layers differ from the default CLI on {} runs:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// The ported commands really run in Vesper (they print through the
/// `HybridCommands` import), and the others in the Rust CLI.
#[test]
fn ported_commands_run_in_vesper() {
    let hybrid = hybrid();
    let dir = std::env::temp_dir().join(format!(
        "vesper-hybrid-commands-stats-{}",
        std::process::id()
    ));
    let prints = |args: &[&str]| -> u64 {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let r = run(&hybrid, &dir, b"abc", &args, true);
        let stderr = String::from_utf8_lossy(&r.output.stderr);
        stderr
            .lines()
            .find_map(|l| l.strip_prefix("vesper-hybrid: the command layers printed "))
            .and_then(|l| l.split(' ').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no command-layer report in {stderr:?}"))
    };
    assert_eq!(prints(&[]), 1);
    assert_eq!(prints(&["--help"]), 1);
    assert_eq!(prints(&["--version"]), 1);
    assert_eq!(prints(&["traceparent", VALID]), 1);
    assert_eq!(prints(&["traceparent", "--json", "bad"]), 2);
    assert_eq!(prints(&["simd", "--help"]), 1);
    assert_eq!(prints(&["simd", "scan", "input", "--delimiter", "97"]), 1);
    // Handed back: the Rust CLI prints.
    assert_eq!(prints(&["fmt", "--help"]), 0);
    assert_eq!(prints(&["check", "--fuel"]), 0);
    let _ = std::fs::remove_dir_all(&dir);
}
