//! `vesper verify SOURCE --format` in Vesper (`run_format` in
//! packages/vesper-verify/src/run.ves, SH5 lane 18, #1341) against the Rust
//! command (src/verify_commands.rs).
//!
//! A small executable built from the package runs each case beside the Rust
//! command; stdout, stderr and the exit status must be byte-identical. The
//! cases cover unreadable, oversized, non-UTF-8 and unparseable sources and
//! a Lean that cannot start, and, with Lean 4.35.0 (`VESPER_LEAN`), real
//! kernel checks of formatter models in the text, `--json` and
//! `--emit-lean` forms. It also keeps the generated `src/model.ves` (the
//! pinned Lean model as a Vesper string) current.
//!
//! Unix only for now: the driver executable links @vesper-std/hash-sys's C
//! BLAKE3, and the Windows runners have no Lean for the real cases.
#![cfg(unix)]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const PACKAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/packages/vesper-verify");
const MODEL_LEAN: &str = include_str!("../lean/StylusAndLintSafety.lean");

/// The repository root as an absolute path (under Bazel
/// `CARGO_MANIFEST_DIR` is `.`).
fn repo() -> PathBuf {
    fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap()
}

fn vesper() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}

struct Dir(PathBuf);
impl Dir {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-self-host-verify-format-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The generated `src/model.ves`: the model as one Vesper string literal
/// (Vesper string literals are JSON strings).
fn model_source() -> String {
    format!(
        "// GENERATED from lean/StylusAndLintSafety.lean (the `MODEL` of\n\
         // src/verify_commands.rs); tests/self_host_verify_format.rs checks it is\n\
         // current and rewrites it with VESPER_VERIFY_BLESS=1.\n\
         pub const MODEL: Str = {};\n",
        serde_json::to_string(MODEL_LEAN).unwrap()
    )
}

#[test]
fn model_text_is_current() {
    let path = Path::new(PACKAGE).join("src/model.ves");
    let expected = model_source();
    if std::env::var_os("VESPER_VERIFY_BLESS").is_some() {
        fs::write(&path, &expected).unwrap();
        return;
    }
    assert!(
        fs::read_to_string(&path).unwrap() == expected,
        "{} is stale; rerun with VESPER_VERIFY_BLESS=1",
        path.display()
    );
}

const DRIVER: &str = r#"use @vesper/verify::run_format;
use @vesper/proof-audit::strs::{push, put_str, strs_new};
use @vesper-std/env::{arg_bytes, arg_count};
use @vesper-std/fs::{IoError, write_stderr, write_stdout};

pub fn main() -> u64 eff [Ffi[StdFs], Ffi[StdEnv], Ffi[StdProcess], Ffi[Blake3]] {
    let mut args = strs_new();
    for i in 1..arg_count() {
        match arg_bytes(i) {
            Option::Some(a) => push(inout args, in a.as_slice()),
            Option::None => {},
        }
    }
    let o = run_format(in args);
    let mut err: ChunkSeq[u8] = ChunkSeq::new[u8]();
    if o.failed {
        put_str(inout err, in "vesper: ");
        err.extend(in o.err.as_slice());
        err.push(10);
    }
    handle Fail[IoError] in {
        if o.out.len() > 0 {
            write_stdout(in o.out.as_slice());
        }
        if err.len() > 0 {
            write_stderr(in err.as_slice());
        }
    } with {
        return v => v,
        raise(_e, _k) => {},
    }
    if o.failed {
        1
    } else {
        0
    }
}
"#;

/// Build the package's driver executable into `dir`.
fn build_port(dir: &Path) -> PathBuf {
    let package = dir.join("driver");
    fs::create_dir_all(package.join("src")).unwrap();
    let std = repo().join("packages/vesper-std");
    let path = |p: PathBuf| serde_json::to_string(p.to_str().unwrap()).unwrap();
    fs::write(
        package.join("Loom.toml"),
        format!(
            "[package]\nname = \"@test/verify-format-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\n\
             \"@vesper/verify\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper/proof-audit\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper-std/env\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper-std/fs\" = {{ version = \"^0.1\", path = {} }}\n",
            path(repo().join("packages/vesper-verify")),
            path(repo().join("packages/vesper-proof-audit")),
            path(std.join("env")),
            path(std.join("fs")),
        ),
    )
    .unwrap();
    fs::write(package.join("src/main.ves"), DRIVER).unwrap();
    let binary = dir.join("verify-format-port");
    let output = vesper()
        .arg("build")
        .arg(&package)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

struct Runner {
    port: PathBuf,
    cases: usize,
    rust_time: Duration,
    port_time: Duration,
}
impl Runner {
    fn run(command: &mut Command) -> (Output, Duration) {
        let started = Instant::now();
        let output = command.output().unwrap();
        (output, started.elapsed())
    }
    /// Run `verify args` both ways; they must agree.
    fn case(&mut self, args: &[&str]) -> Output {
        let (rust, rust_time) = Self::run(vesper().arg("verify").args(args));
        let (port, port_time) = Self::run(Command::new(&self.port).args(args));
        self.cases += 1;
        self.rust_time += rust_time;
        self.port_time += port_time;
        let show = |o: &Output| {
            format!(
                "status {:?}\nstdout:\n{}\nstderr:\n{}",
                o.status.code(),
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        };
        assert!(
            rust.status.code() == port.status.code()
                && rust.stdout == port.stdout
                && rust.stderr == port.stderr,
            "verify {args:?}\n--- Rust\n{}\n--- Vesper\n{}",
            show(&rust),
            show(&port)
        );
        rust
    }
}

fn resolve_lean(lean: String) -> String {
    let Ok(output) = Command::new(&lean)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("--print-prefix")
        .output()
    else {
        return lean;
    };
    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let binary = PathBuf::from(prefix)
        .join("bin")
        .join(format!("lean{}", std::env::consts::EXE_SUFFIX));
    if output.status.success() && binary.is_file() {
        binary.to_string_lossy().into_owned()
    } else {
        lean
    }
}

fn lean() -> Option<String> {
    let lean = resolve_lean(std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into()));
    match Command::new(&lean).arg("--version").output() {
        Ok(o)
            if o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains("version 4.35.0,") =>
        {
            Some(lean)
        }
        _ => None,
    }
}

/// Sources the Lean cases check: a commented program, an empty file and a
/// fixed-expression macro (proved without expansion).
const CHECKED: &[&[u8]] = &[
    b"// lead\nfn main() -> u64 { /* inner */ let x = 1; x } // tail\n",
    b"",
    b"fn main(){let x=1;x}",
];

/// Sources rejected before Lean runs: a parse error, an unclosed comment,
/// raw macro tokens and invalid UTF-8.
const REJECTED: &[&[u8]] = &[
    b"fn main(){let x=;",
    b"fn main(){/* unclosed",
    b"fn main(){unknown!{raw}}",
    b"fn main(){\xff}",
];

#[test]
fn vesper_port_matches_rust_verify_format() {
    let work = Dir::new("work");
    let mut r = Runner {
        port: build_port(&work.0),
        cases: 0,
        rust_time: Duration::ZERO,
        port_time: Duration::ZERO,
    };
    let root = work.0.join("cases");
    fs::create_dir_all(&root).unwrap();
    let file = |name: &str, bytes: &[u8]| {
        let p = root.join(name);
        fs::write(&p, bytes).unwrap();
        p.to_str().unwrap().to_owned()
    };
    let missing = root.join("missing.ves").to_str().unwrap().to_owned();
    let dir = root.to_str().unwrap().to_owned();
    let big = file("big.ves", &vec![b' '; 128 * 1024 + 1]);
    let link = root.join("link.ves");
    let target = file("target.ves", CHECKED[0]);
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let link = link.to_str().unwrap().to_owned();
    for path in [&missing, &dir, &big, &link] {
        r.case(&[path, "--format"]);
        r.case(&[path, "--format", "--json"]);
    }
    for (i, source) in REJECTED.iter().enumerate() {
        let path = file(&format!("rejected{i}.ves"), source);
        r.case(&[&path, "--format"]);
        r.case(&[&path, "--format", "--json"]);
    }
    for lean in ["/nonexistent/lean", &target] {
        r.case(&[&target, "--format", "--lean", lean]);
        r.case(&[&target, "--format", "--json", "--lean", lean]);
    }
    let Some(lean) = lean() else {
        eprintln!("Lean 4.35.0 not found; skipped the kernel cases");
        return;
    };
    for (i, source) in CHECKED.iter().enumerate() {
        let path = file(&format!("checked{i}.ves"), source);
        let ok = r.case(&[&path, "--format", "--lean", &lean, "--timeout", "300"]);
        assert!(
            ok.status.success(),
            "{}",
            String::from_utf8_lossy(&ok.stderr)
        );
        r.case(&[
            &path,
            "--format",
            "--json",
            "--lean",
            &lean,
            "--timeout",
            "300",
        ]);
        r.case(&[
            &path,
            "--format",
            "--emit-lean",
            "--lean",
            &lean,
            "--timeout",
            "300",
        ]);
    }
    eprintln!(
        "verify --format: {} cases, 0 diffs; Rust {:?}, Vesper {:?} ({:.2}x)",
        r.cases,
        r.rust_time,
        r.port_time,
        r.port_time.as_secs_f64() / r.rust_time.as_secs_f64()
    );
}
