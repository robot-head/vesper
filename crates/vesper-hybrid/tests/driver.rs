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
    // Source sugar (#1313): expanded before checking, and diagnostics
    // mapped back through the expansion's source map.
    (
        "sugar.ves",
        "sugar take_right: infix(`.+`, additive, left) { rule ($a:expr .+ $b:expr) => { $b } }\nfn main() -> i64 {\n    1 .+ 42\n}\n",
    ),
    (
        "sugar_error.ves",
        "sugar take_right: infix(`.+`, additive, left) { rule ($a:expr .+ $b:expr) => { $b } }\nfn main() -> i64 {\n    1 .+ nope\n}\n",
    ),
    // The parts of `check::check` the driver sequences (#1900): untyped
    // value-enum members (rewritten, and with their own diagnostics),
    // `use` outside a package after a rewrite, the Beacon prelude and
    // rules (tests/fixtures/trace_effects), and source tensor types
    // (tests/fixtures/tensor_types).
    (
        "untyped.ves",
        "enum Helper: _ { Trace = 6, Next = 1 << 40 }\nfn wide(x: u64) -> u64 {\n    x\n}\nfn main() -> i64 {\n    Helper::Trace + 1\n}\n",
    ),
    (
        "untyped_error.ves",
        "fn f() -> u64 {\n    3\n}\nenum E: _ { A = f() }\nfn main() -> u64 {\n    E::A + nope\n}\n",
    ),
    (
        "untyped_use.ves",
        "use self::x::y;\nenum E: _ { A = 1 }\nfn main() -> u64 {\n    E::A\n}\n",
    ),
    (
        "beacon.ves",
        "copy struct Secret[T] {\n    value: T,\n}\ncopy struct Login {\n    user_id: u64,\n    password: Secret[u64],\n}\nfn fingerprint(s: Secret[u64]) -> u64 {\n    7\n}\nimpl Telemetrable for Login {\n    fn telemetry_attr(in self) -> TelemetryAttr {\n        TelemetryAttr::Count(fingerprint(self.password))\n    }\n}\nfn main() -> u64 {\n    0\n}\n",
    ),
    (
        "tensor.ves",
        "fn rotate[D: dim](x: Tensor[F32, [1, D * 2]]) -> Tensor[F32, [1, D * 2]] eff [Tensor] {\n    Tensor::rope(x, 3)\n}\nfn f(x: Tensor[F32, [1, 4]]) -> Tensor[F32, [1, 4]] eff [Tensor] {\n    let g = rotate;\n    g(x)\n}\n",
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
    // One optimizer at a time (#1929; the arithmetic oracle's settings).
    &["run", "--no-ast-opt"],
    &["run", "--no-ir-opt"],
    &["jit", "--no-ast-opt"],
    &["jit", "--no-ir-opt"],
    &["emit", "--emit", "hir", "--no-ast-opt"],
    &["check", "--no-ir-opt"],
    &["emit", "--emit", "hir"],
    &["emit", "--emit", "ir"],
    &["emit", "--emit", "ir", "--no-opt"],
    &["emit", "--emit", "cst"],
    &["emit", "--emit", "lean"],
    &["emit", "--emit", "clif"],
    // Group 3 of #1367 (#1894): the driver writes emitted text itself.
    &["emit", "--emit", "ir", "-o", "{out}"],
    &["emit", "--emit", "ir", "--no-opt", "-o", "{out}"],
    &["emit", "--emit", "lean", "-o", "{out}"],
    &["emit", "--emit", "clif", "-o", "{out}"],
    &["emit", "--emit", "ir", "-o", "{dir}/a.out"],
    &["emit", "--emit", "clif", "-o", "{dir}/a.out"],
    &["run", "--entry", "missing"],
    // Slice 2 of #1894: the driver runs the native verifiers' orchestration
    // (whether they run, the policy file, the report and the verdict).
    &[
        "emit",
        "--emit",
        "clif",
        "--verify-stack",
        "--verify-policy",
        "{policy}",
    ],
    &[
        "emit",
        "--emit",
        "clif",
        "-o",
        "{out}",
        "--verify-stack",
        "--verify-policy",
        "{roomy}",
    ],
    &[
        "emit",
        "--emit",
        "object",
        "-o",
        "{out}",
        "--verify-stack",
        "--verify-policy",
        "{dir}/a.out",
    ],
    &[
        "build",
        "-o",
        "{out}",
        "--verify-stack",
        "--verify-policy",
        "{badpolicy}",
    ],
    &[
        "build",
        "-o",
        "{out}",
        "--verify-stack",
        "--verify-policy",
        "{binpolicy}",
    ],
    &[
        "build",
        "-o",
        "{out}",
        "--verify-stack",
        "--verify-policy",
        "{roomy}",
    ],
    &["emit", "--emit", "clif", "--verify-policy", "{badpolicy}"],
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
    // The wasm build in the Vesper driver (#1895).
    &["build", "-o", "{out}", "--target", "wasm32-wasip2"],
    &[
        "build",
        "-o",
        "{out}",
        "--target",
        "wasm32-wasip2",
        "--no-opt",
    ],
    &[
        "build",
        "-o",
        "{out}",
        "--target",
        "wasm32-wasip2",
        "--entry",
        "other",
    ],
    &["emit", "--emit", "object", "--target-features", "sse3"],
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
    // Native verifier policies: a stack budget most entries exceed, one
    // they fit, one that does not parse and one that is not UTF-8.
    for (name, text) in [
        (
            "policy",
            &br#"{"schema":"vesper.native-verifiers/1","stack":{"roots":{"main":1}}}"#[..],
        ),
        (
            "roomy",
            br#"{"schema":"vesper.native-verifiers/1","stack":{"roots":{"main":1048576}}}"#,
        ),
        ("badpolicy", b"{ not json"),
        ("binpolicy", &[0xff, 0xfe]),
    ] {
        std::fs::write(scratch.join(name), text).unwrap();
    }
    let invalid_utf8 = scratch.join("invalid-utf8.ves");
    std::fs::write(&invalid_utf8, [0xff]).unwrap();
    inputs.push(invalid_utf8);
    inputs.push(scratch.join("missing.ves"));

    let sets: Vec<&[&str]> = {
        #[cfg(unix)]
        {
            let mut s = OPTION_SETS.to_vec();
            s.push(FALSE_LINKER);
            s
        }
        #[cfg(not(unix))]
        {
            OPTION_SETS.to_vec()
        }
    };
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
                        "{policy}" | "{roomy}" | "{badpolicy}" | "{binpolicy}" => {
                            scratch.join(&a[1..a.len() - 1]).display().to_string()
                        }
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
            // Objects, archives, wasm components and texts are byte-identical
            // everywhere. Linked native executables embed timestamps/paths on
            // macOS (`N_OSO` path and mtime) and Windows (PE `TimeDateStamp`),
            // so only Linux compares raw linked executable bytes; non-Linux
            // verifies existence while `emit --emit object` checks the object.
            let linked_native = set.first() == Some(&"build")
                && !set.contains(&"--lib")
                && !set.contains(&"--target");
            if cfg!(target_os = "linux") || !linked_native {
                if d.0.written != h.0.written {
                    diff.push("output bytes");
                }
            } else if d.0.written.is_some() != h.0.written.is_some()
                || d.0.written.as_ref().is_some_and(Vec::is_empty)
                || h.0.written.as_ref().is_some_and(Vec::is_empty)
            {
                diff.push("output presence");
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

/// The session and I/O steps the Vesper driver runs itself (hd_step group
/// 1, #1896): what `--target` names (Windows or not), `--tee`'s
/// `vesper-tee` run, whether the input is a package, and the text of an I/O
/// error. Each option set runs on a program, a package, a directory that
/// is not a package, a missing file and a file in a missing directory.
#[test]
fn session_steps_match_the_default_compiler() {
    let root = repository();
    let scratch =
        std::env::temp_dir().join(format!("vesper-hybrid-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(scratch.join("plain-dir")).unwrap();
    // Each compiler runs from a copy in its own directory, so `--tee` looks
    // for `vesper-tee` in the same places for both: Bazel builds the tools
    // beside `//:bin/vesper` (at `../crates/vesper-tools/bin`) but not beside
    // the hybrid.
    let alone = |exe: PathBuf, tag: &str| -> PathBuf {
        let dir = scratch.join(format!("bin-{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join(exe.file_name().unwrap());
        std::fs::copy(&exe, &copy).unwrap_or_else(|e| panic!("copy {}: {e}", exe.display()));
        copy
    };
    let default = alone(default_compiler(), "default");
    let hybrid = alone(hybrid(), "hybrid");
    let program = scratch.join("ok.ves");
    std::fs::write(&program, "fn main() -> i64 {\n    40 + 2\n}\n").unwrap();
    let inputs = [
        program,
        root.join("examples/loom"),
        scratch.join("plain-dir"),
        scratch.join("missing.ves"),
        scratch.join("no-such-dir/x.ves"),
    ];
    let sets: &[&[&str]] = &[
        &["check"],
        &["build", "-o", "{out}", "--tee"],
        &["build", "-o", "{out}", "--target", "x86_64-pc-windows-msvc"],
        &["build", "-o", "{out}", "--target", "x86_64-windows"],
        &["build", "-o", "{out}", "--target", "aarch64-apple-darwin"],
        &[
            "build",
            "-o",
            "{out}",
            "--target",
            "x86_64-unknown-linux-gnu",
        ],
        &["build", "-o", "{out}", "--target", "not-a-triple"],
        &[
            "emit",
            "--emit",
            "object",
            "--target",
            "x86_64-pc-windows-msvc",
        ],
    ];
    let mut mismatches = vec![];
    let mut runs = 0;
    for (i, input) in inputs.iter().enumerate() {
        for (k, set) in sets.iter().enumerate() {
            let mut both = vec![];
            for (tag, compiler) in [("default", &default), ("hybrid", &hybrid)] {
                let out = scratch.join(format!("out-{i}-{k}-{tag}"));
                let mut args: Vec<String> = set
                    .iter()
                    .map(|a| match *a {
                        "{out}" => out.display().to_string(),
                        a => a.to_owned(),
                    })
                    .collect();
                args.push(input.display().to_string());
                let r = run(compiler, &scratch, &args, &out);
                both.push((r, out, tag));
            }
            runs += 1;
            let (d, h) = (&both[0], &both[1]);
            let mut diff = vec![];
            if d.0.output.status.code() != h.0.output.status.code() {
                diff.push("exit status");
            }
            for (what, a, b) in [
                ("stdout", &d.0.output.stdout, &h.0.output.stdout),
                ("stderr", &d.0.output.stderr, &h.0.output.stderr),
            ] {
                if normalized(a, &d.1, d.2) != normalized(b, &h.1, h.2) {
                    diff.push(what);
                }
            }
            if d.0.written.is_some() != h.0.written.is_some() {
                diff.push("output presence");
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
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!("session steps: {runs} runs, {} differ", mismatches.len());
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

/// Every `.ves` file under `dir`, sorted.
fn ves_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            ves_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "ves") {
            out.push(path);
        }
    }
}

/// The count the hybrid's `VESPER_HYBRID_STATS=1` report gives after
/// `prefix`, and stderr without the report.
fn stat(stderr: &[u8], prefix: &str, what: &str) -> (u64, Vec<u8>) {
    let text = String::from_utf8_lossy(stderr);
    let mut n = 0;
    let mut rest = String::new();
    for line in text.split_inclusive('\n') {
        if let Some(tail) = line.strip_prefix(prefix)
            && tail.contains(what)
        {
            n = tail
                .split(' ')
                .next()
                .and_then(|w| w.parse().ok())
                .unwrap_or(0);
        } else if !line.starts_with("vesper-hybrid: ") {
            rest.push_str(line);
        }
    }
    (n, rest.into_bytes())
}

/// Files where the Vesper AST optimizer settles an equal-cost tie
/// differently from egg (the accepted ties of tests/differential.rs), so
/// their optimized components differ from the default compiler's.
const OPTIMIZE_EQUAL_COST_TIES: &[&str] = &[
    "examples/traits/shapes.ves",
    "tests/fixtures/bounded_generics/nested.ves",
    "tests/fixtures/native_handlers/shared_locals.ves",
    "tests/fixtures/wide_literals.ves",
];

struct WasmRun {
    output: Output,
    written: Option<Vec<u8>>,
    time: Duration,
    out: PathBuf,
}

fn wasm_run(
    compiler: &Path,
    scratch: &Path,
    name: &str,
    input: &Path,
    no_opt: bool,
    env: &[(&str, &str)],
) -> WasmRun {
    let out = scratch.join(name);
    let _ = std::fs::remove_file(&out);
    let mut c = Command::new(compiler);
    c.current_dir(scratch)
        .env_remove("VESPER_HYBRID_STATS")
        .env_remove("VESPER_HYBRID_DRIVER_WASM")
        .args(["build", "--target", "wasm32-wasip2", "-o"])
        .arg(&out);
    if no_opt {
        c.arg("--no-opt");
    }
    c.arg(input);
    for (k, v) in env {
        c.env(k, v);
    }
    let start = Instant::now();
    let output = c.output().unwrap();
    let time = start.elapsed();
    let written = std::fs::read(&out).ok();
    let _ = std::fs::remove_file(&out);
    WasmRun {
        output,
        written,
        time,
        out,
    }
}

/// What differs between two runs (output paths normalized).
fn wasm_diff(a: &WasmRun, b: &WasmRun) -> Vec<&'static str> {
    let norm = |bytes: &[u8], out: &Path| {
        let name = out.file_name().unwrap().to_string_lossy().into_owned();
        String::from_utf8_lossy(bytes).replace(&name, "{out}")
    };
    let mut diff = vec![];
    if a.output.status.code() != b.output.status.code() {
        diff.push("exit status");
    }
    if norm(&a.output.stdout, &a.out) != norm(&b.output.stdout, &b.out) {
        diff.push("stdout");
    }
    if norm(&a.output.stderr, &a.out) != norm(&b.output.stderr, &b.out) {
        diff.push("stderr");
    }
    if a.written != b.written {
        diff.push("component bytes");
    }
    diff
}

/// `vesper build --target wasm32-wasip2` (#1895): the Vesper driver encodes
/// the checked world through the session, builds the component with
/// @vesper/wasm-backend and @vesper/wit-component and writes it itself.
///
/// Over every corpus program with a `main` (every `VESPER_WASM_STRIDE`th
/// under Bazel), with and without `--no-opt`:
/// - the hybrid agrees byte for byte (exit status, stdout, stderr,
///   component) with itself on the Rust pass (`VESPER_HYBRID_DRIVER_WASM=0`,
///   the `wasm` swap inside `wasm_backend::compile`), which is what the
///   driver replaces;
/// - against the default compiler, the only differences are the two the
///   wasm swap already has: programs the port lowers and Rust refuses
///   (loops, wide integers; tests/self_host_wasm_backend.rs runs them) and
///   the optimizer's accepted equal-cost ties.
#[test]
fn wasm_build_matches_the_rust_pass() {
    let root = repository();
    let default = default_compiler();
    let hybrid = hybrid();
    let stride: usize = std::env::var("VESPER_WASM_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let mut files = vec![];
    for dir in ["bootstrap/corpus", "examples", "tests/fixtures"] {
        ves_files(&root.join(dir), &mut files);
    }
    let files: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|t| t.contains("fn main")))
        .step_by(stride)
        .collect();
    let scratch = std::env::temp_dir().join(format!("vesper-hybrid-wasm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let (mut runs, mut built, mut in_vesper, mut extensions, mut ties) = (0, 0, 0, 0, 0);
    let (mut rust_time, mut vesper_time) = (Duration::ZERO, Duration::ZERO);
    let mut mismatches = vec![];
    for (i, input) in files.iter().enumerate() {
        let relative = input
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for no_opt in [false, true] {
            runs += 1;
            let tag = format!("{relative}{}", if no_opt { " --no-opt" } else { "" });
            let d = wasm_run(&default, &scratch, "d.wasm", input, no_opt, &[]);
            // ABBA: alternate which hybrid run goes first.
            let rust_pass = |name| {
                wasm_run(
                    &hybrid,
                    &scratch,
                    name,
                    input,
                    no_opt,
                    &[("VESPER_HYBRID_DRIVER_WASM", "0")],
                )
            };
            let driver = |name| {
                wasm_run(
                    &hybrid,
                    &scratch,
                    name,
                    input,
                    no_opt,
                    &[("VESPER_HYBRID_STATS", "1")],
                )
            };
            let (r, v) = if (i + usize::from(no_opt)) % 2 == 0 {
                let r = rust_pass("r.wasm");
                (r, driver("v.wasm"))
            } else {
                let v = driver("v.wasm");
                (rust_pass("r.wasm"), v)
            };
            rust_time += r.time;
            vesper_time += v.time;
            let (n, stderr) = stat(
                &v.output.stderr,
                "vesper-hybrid: driver built ",
                " wasm components ",
            );
            in_vesper += n;
            built += u64::from(v.written.is_some());
            let v = WasmRun {
                output: Output { stderr, ..v.output },
                ..v
            };
            let diff = wasm_diff(&r, &v);
            if !diff.is_empty() {
                mismatches.push(format!(
                    "{tag}: driver vs Rust pass: {} (Rust pass {:?}, driver {:?})",
                    diff.join(", "),
                    String::from_utf8_lossy(&r.output.stderr),
                    String::from_utf8_lossy(&v.output.stderr),
                ));
            }
            let diff = wasm_diff(&d, &v);
            if diff.is_empty() {
                continue;
            }
            let d_err = String::from_utf8_lossy(&d.output.stderr);
            let refused = !d.output.status.success()
                && (d_err.contains(": unsupported Operation")
                    || d_err.contains(": unsupported IntegerWidth"));
            if refused && v.output.status.success() {
                extensions += 1;
            } else if !no_opt
                && OPTIMIZE_EQUAL_COST_TIES.contains(&relative.as_str())
                && diff == ["component bytes"]
            {
                ties += 1;
            } else {
                mismatches.push(format!(
                    "{tag}: driver vs default compiler: {} (default {d_err:?}, driver {:?})",
                    diff.join(", "),
                    String::from_utf8_lossy(&v.output.stderr),
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!(
        "wasm build differential: {} programs, {runs} runs, {built} components, {in_vesper} built in the Vesper driver; \
         vs default: {extensions} port extensions, {ties} optimizer ties; \
         Rust pass {:.2} s, Vesper driver {:.2} s ({:.3}x); {} differ",
        files.len(),
        rust_time.as_secs_f64(),
        vesper_time.as_secs_f64(),
        vesper_time.as_secs_f64() / rust_time.as_secs_f64(),
        mismatches.len()
    );
    assert!(
        mismatches.is_empty(),
        "the Vesper wasm build differs on {} runs:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    assert!(in_vesper > 0, "no wasm build ran in the Vesper driver");
}

/// One `vesper build --target bpf` run in `scratch`: `-o <scratch>/<name>`
/// unless `name` is empty (the default output, `input` with `.o`).
fn ebpf_run(
    compiler: &Path,
    scratch: &Path,
    name: &str,
    input: &Path,
    entry: &str,
    env: &[(&str, &str)],
) -> WasmRun {
    let out = if name.is_empty() {
        input.with_extension("o")
    } else {
        scratch.join(name)
    };
    let _ = std::fs::remove_file(&out);
    let mut c = Command::new(compiler);
    c.current_dir(scratch)
        .env_remove("VESPER_HYBRID_STATS")
        .env_remove("VESPER_HYBRID_DRIVER_EBPF")
        .args(["build", "--target", "bpf", "--entry", entry]);
    if !name.is_empty() {
        c.arg("-o").arg(&out);
    }
    c.arg(input);
    for (k, v) in env {
        c.env(k, v);
    }
    let start = Instant::now();
    let output = c.output().unwrap();
    let time = start.elapsed();
    let written = std::fs::read(&out).ok();
    let _ = std::fs::remove_file(&out);
    WasmRun {
        output,
        written,
        time,
        out,
    }
}

/// `vesper build --target bpf` (#1895): the session parses, checks and
/// encodes the source, and @vesper/ebpf-backend compiles it, verifies it
/// with the @vesper/ebpf verifier and writes the ELF object in the Vesper
/// driver. Over the eBPF fixtures with every function as the entry (and
/// the default output path), truncated fixtures and every corpus program
/// with a `main` (every `VESPER_EBPF_STRIDE`th under Bazel), the hybrid
/// agrees byte for byte with the default compiler and with itself on the
/// Rust pass (`VESPER_HYBRID_DRIVER_EBPF=0`): exit status, stdout, stderr
/// and the object.
#[test]
fn ebpf_build_matches_the_rust_pass() {
    let root = repository();
    let default = default_compiler();
    let hybrid = hybrid();
    let stride: usize = std::env::var("VESPER_EBPF_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let scratch = std::env::temp_dir().join(format!("vesper-hybrid-ebpf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    // (input, entry, default output)
    let mut cases: Vec<(PathBuf, String, bool)> = vec![];
    let mut fixtures = vec![];
    ves_files(&root.join("tests/fixtures/ebpf_backend"), &mut fixtures);
    for (i, f) in fixtures.iter().enumerate() {
        let text = std::fs::read_to_string(f).unwrap();
        let copy = scratch.join(format!("fixture{i}.ves"));
        std::fs::write(&copy, &text).unwrap();
        cases.push((copy.clone(), "main".into(), true));
        for line in text.lines() {
            let Some(rest) = line.trim_start().strip_prefix("fn ") else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            cases.push((copy.clone(), name, false));
        }
        // Truncated sources: parse and check errors stay Rust's.
        for cut in [text.len() / 3, text.len() / 2, text.len() * 9 / 10] {
            let mut cut = cut;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            let t = scratch.join(format!("fixture{i}-cut{cut}.ves"));
            std::fs::write(&t, &text[..cut]).unwrap();
            cases.push((t, "main".into(), false));
        }
    }
    let mut corpus = vec![];
    for dir in ["bootstrap/corpus", "examples", "tests/fixtures"] {
        ves_files(&root.join(dir), &mut corpus);
    }
    for f in corpus
        .into_iter()
        .filter(|p| !p.starts_with(root.join("tests/fixtures/ebpf_backend")))
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|t| t.contains("fn main")))
        .step_by(stride)
    {
        cases.push((f, "main".into(), false));
    }
    let (mut objects, mut in_vesper) = (0, 0);
    let (mut rust_time, mut vesper_time) = (Duration::ZERO, Duration::ZERO);
    let mut mismatches = vec![];
    for (i, (input, entry, default_out)) in cases.iter().enumerate() {
        let name = |tag: &str| {
            if *default_out {
                String::new()
            } else {
                format!("{tag}.o")
            }
        };
        let tag = format!("{} --entry {entry}", input.display());
        let d = ebpf_run(&default, &scratch, &name("d"), input, entry, &[]);
        let rust_pass = || {
            ebpf_run(
                &hybrid,
                &scratch,
                &name("r"),
                input,
                entry,
                &[("VESPER_HYBRID_DRIVER_EBPF", "0")],
            )
        };
        let driver = || {
            ebpf_run(
                &hybrid,
                &scratch,
                &name("v"),
                input,
                entry,
                &[("VESPER_HYBRID_STATS", "1")],
            )
        };
        // ABBA: alternate which hybrid run goes first.
        let (r, v) = if i % 2 == 0 {
            let r = rust_pass();
            (r, driver())
        } else {
            let v = driver();
            (rust_pass(), v)
        };
        rust_time += r.time;
        vesper_time += v.time;
        let (n, stderr) = stat(
            &v.output.stderr,
            "vesper-hybrid: driver built ",
            " eBPF objects ",
        );
        in_vesper += n;
        objects += u64::from(v.written.is_some());
        let v = WasmRun {
            output: Output { stderr, ..v.output },
            ..v
        };
        for (what, other) in [("Rust pass", &r), ("default compiler", &d)] {
            let diff = wasm_diff(other, &v);
            if !diff.is_empty() {
                mismatches.push(format!(
                    "{tag}: driver vs {what}: {} ({what} {:?} {:?}, driver {:?} {:?})",
                    diff.join(", "),
                    String::from_utf8_lossy(&other.output.stdout),
                    String::from_utf8_lossy(&other.output.stderr),
                    String::from_utf8_lossy(&v.output.stdout),
                    String::from_utf8_lossy(&v.output.stderr),
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!(
        "eBPF build differential: {} runs, {objects} objects, {in_vesper} built in the Vesper driver; \
         Rust pass {:.2} s, Vesper driver {:.2} s ({:.3}x); {} differ",
        cases.len(),
        rust_time.as_secs_f64(),
        vesper_time.as_secs_f64(),
        vesper_time.as_secs_f64() / rust_time.as_secs_f64(),
        mismatches.len()
    );
    assert!(
        mismatches.is_empty(),
        "the Vesper eBPF build differs on {} runs:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    assert!(in_vesper > 0, "no eBPF build ran in the Vesper driver");
}
