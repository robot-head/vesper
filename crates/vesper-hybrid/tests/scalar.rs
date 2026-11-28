//! The `scalar` codegen swap of the hybrid build (self-hosting SH4 lane 6,
//! #1280): scalar operators, locals, control flow and the raising arms
//! compile to the same objects with the hybrid compiler as with the
//! default one, and the hybrid's scalar requests really are answered from
//! Vesper. Plain scalar functions are emitted whole through the `body`
//! hook (native walk part 6a, #2094), and those bodies come from Vesper
//! too.
//!
//! tests/differential.rs compares the whole corpus's objects; this test
//! checks that the scalar hook is the one answering, for a generic
//! function the body hook leaves to the walk. The full per-request
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
// A generic function is not a plain frame (#2657 handlers made `main`
// one), so the scalar hook answers its operators.
fn scaled[T](in tag: T, a: i64, b: i64) -> i64 eff [Fail[ArithOverflow]] {
    a * b - (a ^ b)
}
fn main() -> i64 {
    handle Fail[ArithOverflow] in {
        mix(12, 5, 40) + sum(30) + scaled(true, 6, 7)
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
// A generic function is not a plain frame (#2657 handlers made `main`
// one), so the scalar hook answers its operators.
fn scaled[T](in tag: T, a: i64, b: i64) -> i64 eff [Fail[ArithOverflow]] {
    a * b - (a ^ b)
}
fn main() -> i64 {
    handle Fail[RuntimeError] in {
        handle Fail[ArithOverflow] in {
            all() + scaled(1, 9, 4)
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

/// Plain scalar functions (no effects, so plain frames): a narrow local
/// written in one branch, a loop carrying a `bool`, a float local and a
/// chain of `if`s.
const BODIES: &str = "\
fn pick(c: bool, x: u8) -> u8 {
    let mut y: u8 = x;
    if c {
        y = x & 15;
    }
    y
}
fn flip(a: u32, b: u32) -> u32 {
    let mut s: u32 = a;
    let mut go: bool = true;
    loop {
        if !go {
            break;
        }
        s = s ^ b;
        go = s != a;
    }
    s | (a & 3)
}
fn choose(c: bool, x: f64) -> f64 {
    let mut y: f64 = 0.5;
    if c {
        y = x;
    }
    y
}
fn sign(x: i16) -> i16 {
    if x < 0 {
        -1
    } else if x == 0 {
        0
    } else {
        1
    }
}
fn main() -> i64 {
    let f: i64 = if choose(true, 2.0) > 1.0 { 64 } else { 0 };
    widen_i64(pick(true, 200)) ^ widen_i64(pick(false, 7)) ^ widen_i64(flip(5, 9))
        ^ widen_i64(sign(-4)) ^ f
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

/// The count of the hybrid's `codegen emitted N <what>` line.
fn emitted(stderr: &str, what: &str) -> u64 {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("vesper-hybrid: codegen emitted "))
        .find(|l| l.contains(what))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no {what} report in {stderr:?}"))
}

/// The count of the hybrid's `codegen emitted N scalar requests` line.
fn scalar_requests(stderr: &str) -> u64 {
    emitted(stderr, " scalar requests")
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

#[test]
fn scalar_bodies_match_the_default_compiler() {
    let scratch = std::env::temp_dir().join(format!("vesper-hybrid-bodies-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let file = scratch.join("bodies.ves");
    std::fs::write(&file, BODIES).unwrap();
    let default = default_compiler();
    // The walk lends whole bodies on the optimize path only.
    for optimize in [false, true] {
        let (d, _) = object(&default, &file, &scratch.join("default.o"), optimize);
        let (h, stderr) = object(&hybrid(), &file, &scratch.join("hybrid.o"), optimize);
        assert!(
            d == h,
            "bodies (optimize {optimize}): the hybrid object differs from the default compiler's"
        );
        let n = emitted(&stderr, " function bodies");
        if optimize {
            assert!(n >= 4, "only {n} function bodies from Vesper");
        }
        eprintln!("bodies (optimize {optimize}): {n} function bodies from Vesper, objects equal");
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
