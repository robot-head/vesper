//! The `scalar` codegen swap of the hybrid build (self-hosting SH4 lane 6,
//! #1280): scalar operators, locals, control flow and the raising arms
//! compile to the same objects with the hybrid compiler as with the
//! default one, and the hybrid's scalar requests really are answered from
//! Vesper.
//!
//! tests/differential.rs compares the whole corpus's objects; this test
//! checks that the scalar hook is the one answering. The full per-request
//! comparison is crates/vesper-codegen-swap-test's
//! `self_host_codegen_scalar_emit`.
use std::path::{Path, PathBuf};
use std::process::Command;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn hybrid() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_vesper-hybrid"));
    // Bazel's launcher gives a bare name found on PATH.
    if path.is_absolute() {
        path
    } else {
        which(&path).unwrap_or(path)
    }
}

fn which(name: &Path) -> Option<PathBuf> {
    let suffixed = format!("{}{}", name.display(), std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| [dir.join(name), dir.join(&suffixed)])
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
}

/// The default compiler, as tests/differential.rs finds it.
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
    let exe = hybrid();
    let sibling = exe.with_file_name(format!("vesper{}", std::env::consts::EXE_SUFFIX));
    if sibling.exists() {
        return sibling;
    }
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .current_dir(repository())
        .args(["build", "--locked", "-p", "vesper", "--bin", "vesper"])
        .status()
        .expect("run cargo build --bin vesper");
    assert!(status.success(), "cargo build --bin vesper failed");
    sibling
}

/// Integer and boolean operators, compares, casts and a loop over locals.
const OPERATORS: &str = "\
fn mix(a: u32, b: u32, c: i64) -> i64 eff [Fail[ArithOverflow]] {
    let x = wrapping_add(a ^ b, (a & 255) | (b >> 3));
    let y: i64 = if x > b && !(a == 0) { c - 7 } else { c * 3 };
    y + widen_i64(x)
}
fn sum(n: i64) -> i64 eff [Fail[ArithOverflow]] {
    let mut t: i64 = 0;
    let mut i: i64 = 0;
    loop {
        if i >= n {
            break;
        }
        if (i & 3) == 0 { t = t + i; } else { t = t - 1; }
        i = i + 1;
    }
    t
}
fn main() -> i64 {
    handle Fail[ArithOverflow] in {
        mix(12, 5, 40) + sum(30)
    } with {
        return v => v,
        raise(_, _) => -1,
    }
}
";

/// The raising arms: overflow, divisor, negate, shift range and narrowing.
const RAISING: &str = "\
fn n8(x: i64) -> i8 eff [Fail[ArithOverflow]] { narrow_i8(x) }
fn nu(x: u64) -> u16 eff [Fail[ArithOverflow]] { narrow_u16(x) }
fn sl(x: u32, n: u32) -> u32 eff [Fail[ArithOverflow]] { x << n }
fn ng(x: i32) -> i32 eff [Fail[ArithOverflow]] { -x }
fn dv(x: i64, y: i64) -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] { x / y + x % y }
fn all() -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    widen_i64(n8(100)) + widen_i64(nu(7)) + widen_i64(sl(1, 4)) + widen_i64(ng(9)) + dv(17, 5)
}
fn main() -> i64 {
    handle Fail[RuntimeError] in {
        handle Fail[ArithOverflow] in {
            all()
        } with {
            return v => v,
            raise(_, _) => -1,
        }
    } with {
        return v => v,
        raise(_, _) => -2,
    }
}
";

/// Runs `compiler` on `file`, returning the object and stderr.
fn object(compiler: &Path, file: &Path, out: &Path, optimize: bool) -> (Vec<u8>, String) {
    let _ = std::fs::remove_file(out);
    let mut cmd = Command::new(compiler);
    cmd.env("VESPER_HYBRID_STATS", "1")
        .args(["emit", "--emit", "object", "-o"])
        .arg(out)
        .arg(file);
    if !optimize {
        cmd.arg("--no-opt");
    }
    let output = cmd.output().expect("run the compiler");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{}: {stderr}", compiler.display());
    (std::fs::read(out).expect("an object"), stderr)
}

/// The count of the hybrid's `codegen emitted N scalar requests` line.
fn scalar_requests(stderr: &str) -> u64 {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("vesper-hybrid: codegen emitted "))
        .find(|l| l.contains(" scalar requests"))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no scalar report in {stderr:?}"))
}

#[test]
fn scalar_programs_match_the_default_compiler() {
    let scratch = std::env::temp_dir().join(format!("vesper-hybrid-scalar-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let default = default_compiler();
    for (name, source) in [("operators", OPERATORS), ("raising", RAISING)] {
        let file = scratch.join(format!("{name}.ves"));
        std::fs::write(&file, source).unwrap();
        for optimize in [false, true] {
            let (d, _) = object(
                &default,
                &file,
                &scratch.join(format!("{name}-default.o")),
                optimize,
            );
            let (h, stderr) = object(
                &hybrid(),
                &file,
                &scratch.join(format!("{name}-hybrid.o")),
                optimize,
            );
            assert!(
                d == h,
                "{name} (optimize {optimize}): the hybrid object differs from the default compiler's"
            );
            let n = scalar_requests(&stderr);
            assert!(
                n > 0,
                "{name} (optimize {optimize}): no scalar request answered from Vesper"
            );
            eprintln!(
                "{name} (optimize {optimize}): {n} scalar requests from Vesper, objects equal"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
