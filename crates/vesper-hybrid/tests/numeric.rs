//! The `numeric` codegen swap of the hybrid build (self-hosting SH4 lane
//! 11, #1285): vector, `TiledSeq`, float, region and atomic programs
//! compile to the same objects with the hybrid compiler as with the default
//! one, and the hybrid's numeric requests really are answered from Vesper.
//!
//! The differential corpus (tests/differential.rs) has none of these
//! programs, so this test brings its own. The full per-intrinsic
//! comparison is crates/vesper-codegen-swap-test's
//! `self_host_codegen_numeric_swap`.
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

/// Lane arithmetic, compares and masks on 128-bit vectors.
const VECTORS: &str = "\
fn k(a: Vec[f64, 2], b: Vec[f64, 2], x: f64) -> f64 eff [Isa[Baseline128]] {
    let s = Vec::add(Vec::mul(a, b), Vec::splat[f64, 2](x));
    Vec::extract(Vec::max(s, a), 1) + Vec::reduce_add(Vec::sqrt(s))
}
fn m(a: Vec[u32, 4], b: Vec[u32, 4]) -> u64 eff [Isa[Baseline128]] {
    let lt = Vec::lt(a, b);
    wrapping_add(Mask::count(Mask::or(lt, Vec::eq(Vec::min(a, b), a))), Mask::bitmask(lt))
}
fn main() -> u64 {
    if Isa::supported[Baseline128]() {
        let x = k(Vec::from[f64, 2](1.5, 2.5), Vec::from[f64, 2](3.5, 4.5), 2.0);
        wrapping_add(m(Vec::from[u32, 4](1, 9, 3, 7), Vec::from[u32, 4](4, 2, 6, 8)), to_bits_f64(x))
    } else {
        0
    }
}
";

/// Every `TiledSeq` operation on hot and cold columns (the shape of
/// self_host_codegen_numeric_swap's column programs).
const TILED: &str = "\
copy struct Pair { a: u64, b: u64 }
tiled(8) copy struct Row { hot x: u32, hot k: u64, cold p: Pair, cold f: f64 }
fn main() -> u64 {
    let mut s: TiledSeq[Row] = TiledSeq::with_capacity[Row](4);
    s.push(Row { x: 3, k: 1, p: Pair { a: 2, b: 3 }, f: 1.5 });
    s.push(Row { x: 9, k: 4, p: Pair { a: 5, b: 6 }, f: 2.5 });
    s.col_set[x](1, 70000);
    s.col_set[p](0, Pair { a: 7, b: 8 });
    let r = s.get(1);
    s.set(0, r);
    let same: u64 = if s.col_get[x](0) == r.x { 1 } else { 0 };
    wrapping_add(wrapping_add(s.col_get[p](0).a, s.col_get[k](1)), wrapping_add(same, wrapping_add(s.len(), s.tile_count())))
}
";

/// Floats, a region block and atomics (part 3).
const SCALARS: &str = "\
copy struct Pair { a: u64, b: u64 }
fn f(x: f64, y: f32) -> u64 { to_bits_f64(sqrt(x) + widen_f64(y)) }
fn main() -> u64 {
    let c: Atomic[u32] = Atomic::new(5);
    let old = c.fetch_add(3, MemoryOrder::SeqCst);
    let r = region r(256) {
        let h = r.box(Pair { a: 2, b: 3 });
        let x: RegionBox[r, f32] = r.box(1.5);
        r.set(x, 2.5);
        let p = r.get(h);
        wrapping_add(wrapping_add(p.a, p.b), widen_u64(to_bits_f32(r.get(x))))
    };
    wrapping_add(wrapping_add(r, f(2.25, 0.5)), wrapping_add(widen_u64(old), widen_u64(c.load(MemoryOrder::Acquire))))
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

/// The count of the hybrid's `codegen emitted N numeric requests` line.
fn numeric_requests(stderr: &str) -> u64 {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("vesper-hybrid: codegen emitted "))
        .find(|l| l.contains(" numeric requests"))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no numeric report in {stderr:?}"))
}

#[test]
fn numeric_programs_match_the_default_compiler() {
    let scratch =
        std::env::temp_dir().join(format!("vesper-hybrid-numeric-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let default = default_compiler();
    for (name, source) in [("vectors", VECTORS), ("tiled", TILED), ("scalars", SCALARS)] {
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
            let n = numeric_requests(&stderr);
            assert!(
                n > 0,
                "{name} (optimize {optimize}): no numeric request answered from Vesper"
            );
            eprintln!(
                "{name} (optimize {optimize}): {n} numeric requests from Vesper, objects equal"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
