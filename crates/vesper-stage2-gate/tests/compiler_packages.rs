//! Stage 2 (stage A: today's sources built by the committed seed) builds
//! every compiler package's probe and runs it, and builds and runs a few
//! lowering smokes (#1397, #2671). Also checks that every directory under
//! `packages/` is listed in the gate's tables.
mod support;

use support::packages::*;
use support::*;

/// A package with no dependencies, named `@vesper-test/<stem>`.
fn smoke_manifest(stem: &str) -> String {
    format!("[package]\nname = \"@vesper-test/{stem}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n")
}

/// Fixed-array writes, on-demand `Result` ADTs, integer intrinsics and
/// conversions, and `usize`/`isize`.
const LOWERING_SMOKE: &str = "\
fn res_val(flag: bool) -> u64 {
    let r: Result[u64, u64] = if flag {
        Result::Ok(10)
    } else {
        Result::Err(3)
    };
    match r {
        Result::Ok(x) => x,
        Result::Err(e) => e,
    }
}

pub fn main() -> u64 {
    let mut arr: [u32; 4] = [1, 2, 3, 4];
    let idx: u64 = 2;
    arr[1] = 20;
    arr[idx] += 10;
    let (lo, hi) = mul_wide(3, 5);
    let sh = wrapping_shl_u8(1, 3);
    let zero64: i64 = 0;
    let w32: i32 = wrapping_i32(wrapping_sub(zero64, 5));
    let neg_adj: u64 = wrapping_u64(wrapping_add(w32, 7));
    let u: usize = 4;
    let i: isize = 5;
    let us_sum: u64 = wrapping_add(wrapping_u64(u), wrapping_u64(i));
    let arr_sum: u64 = widen_u64(wrapping_add(arr[1], arr[2]));
    wrapping_add(
        wrapping_add(wrapping_add(arr_sum, neg_adj), wrapping_add(widen_u64(sh), us_sum)),
        wrapping_add(wrapping_add(res_val(true), res_val(false)), wrapping_add(lo, hi))
    )
}
";
const LOWERING_SMOKE_ANSWER: u64 = 80;

/// An unresolved call: the WErr diagnostic names the symbol (`werr_syms`).
const WERR_SYMS_SMOKE: &str = "pub fn main() -> u64 { unknown_host_fn() }\n";

/// Unsuffixed integer literals on the left take their type from the right,
/// in binary operators and same-integer builtins (`wrapping_sub(0, i128)`).
const LHS_LITERAL_SMOKE: &str = "\
fn neg_i128(x: i128) -> u64 {
    let n = wrapping_sub(0, x);
    let shifted = 10 + n;
    let bonus = if 0 < shifted { 1 } else { 0 };
    wrapping_add(wrapping_u64(shifted), bonus)
}
pub fn main() -> u64 {
    let neg: i128 = -5;
    let wide: u128 = 20;
    let sum = 2 + wide;
    wrapping_add(neg_i128(neg), wrapping_u64(sum))
}
";
const LHS_LITERAL_SMOKE_ANSWER: u64 = 38;

/// `for` over array literals (of integers, narrowed by their use, and of
/// text), a fixed array, `ChunkSeq` places and temporaries and a `Slice`,
/// with `continue` and `break` (#2703: these loops ran zero times).
const FOR_SEQUENCES_SMOKE: &str = "\
fn digits(in xs: Slice[u64]) -> u64 {
    let mut s: u64 = 0;
    for x in xs {
        if x == 3 {
            continue;
        }
        if x == 9 {
            break;
        }
        s = s * 10 + x;
    }
    s
}

fn pair() -> ChunkSeq[u64] {
    let mut xs: ChunkSeq[u64] = ChunkSeq::new[u64]();
    xs.push(2);
    xs.push(4);
    xs
}

pub fn main() -> u64 {
    let mut s: u64 = 0;
    for w in [1, 2, 3] {
        s = s * 10 + w;
    }
    let mut narrow: u32 = 0;
    for w in [4, 5] {
        narrow = narrow * 10 + w;
    }
    let fixed: [u64; 3] = [6, 7, 8];
    let mut seq: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for x in fixed {
        seq.push(x);
    }
    let mut from_seq: u64 = 0;
    for x in seq {
        from_seq = from_seq * 10 + x;
    }
    let mut cut: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for x in [1, 3, 2, 9, 5] {
        cut.push(x);
    }
    let mut nested: u64 = 0;
    for x in pair() {
        for y in [1, 2] {
            nested = nested * 10 + x * y;
        }
    }
    let mut text: u64 = 0;
    for t in [\"ab\", \"cde\"] {
        text = text * 10 + t.len();
    }
    let mut out = s * 100 + widen_u64(narrow);
    out = out * 1000 + from_seq;
    out = out * 100 + digits(in cut.as_slice());
    out = out * 10000 + nested;
    out * 100 + text
}
";
const FOR_SEQUENCES_SMOKE_ANSWER: u64 = 1_234_567_812_244_823;

/// What the evaluator answers for [`STAGE2_FLOAT_SMOKE`] (checked against
/// it while the Rust evaluator existed, tests/wasm_seed_package_io.rs).
const FLOAT_SMOKE_ANSWER: u64 = 3_732_379_685_888_365_023;

/// Builds `lib_src` as a package in `<dir>/wrap` with `compiler`.
fn build(
    compiler: &Compiler,
    dir: &std::path::Path,
    manifest: &str,
    lib_src: &str,
) -> Result<Vec<u8>, String> {
    write_probe(&dir.join("wrap"), manifest, lib_src);
    build_compiler_package_probe(compiler, dir)
}

#[test]
fn stage2_compiles_compiler_packages() {
    with_large_stack(|| {
        let dir = fresh("compiler-packages");
        let a = stage_a(&dir);
        let mut failures = Vec::new();
        for (stem, pkg_name, pkg_rel, expr, exec_check) in COMPILER_PACKAGES.iter() {
            let (manifest, lib_src) = compiler_package_probe(stem, pkg_name, pkg_rel, expr);
            let bytes = build(&a, &dir, &manifest, &lib_src)
                .unwrap_or_else(|e| panic!("Stage 2 on {stem}: {e}"));
            match (exec_check, run(&bytes, &Ctx::default())) {
                (CompilerExecCheck::Exact(want), Ok(got)) if got == *want => {}
                (CompilerExecCheck::NonZero, Ok(got)) if got > 0 => {}
                (CompilerExecCheck::Exact(want), Ok(got)) => {
                    failures.push(format!("{stem}: expected {want}, got {got}"))
                }
                (CompilerExecCheck::NonZero, Ok(got)) => {
                    failures.push(format!("{stem}: expected > 0, got {got}"))
                }
                (_, Err(e)) => failures.push(format!("{stem}: trap {e}")),
            }
        }
        assert!(
            failures.is_empty(),
            "unexpected compiler package execution results:\n{}",
            failures.join("\n")
        );

        for (stem, src, want) in [
            (
                "stage2-lowering-smoke",
                LOWERING_SMOKE,
                LOWERING_SMOKE_ANSWER,
            ),
            (
                "stage2-lhs-int-lit",
                LHS_LITERAL_SMOKE,
                LHS_LITERAL_SMOKE_ANSWER,
            ),
            ("stage2-floats", STAGE2_FLOAT_SMOKE, FLOAT_SMOKE_ANSWER),
            (
                "stage2-for-sequences",
                FOR_SEQUENCES_SMOKE,
                FOR_SEQUENCES_SMOKE_ANSWER,
            ),
        ] {
            let bytes = build(&a, &dir, &smoke_manifest(stem), src)
                .unwrap_or_else(|e| panic!("Stage 2 on {stem}: {e}"));
            let got = run(&bytes, &Ctx::default()).unwrap_or_else(|e| panic!("run {stem}: {e}"));
            assert_eq!(got, want, "{stem}");
        }

        let err = build(
            &a,
            &dir,
            &smoke_manifest("stage2-werr-syms"),
            WERR_SYMS_SMOKE,
        )
        .expect_err("an unresolved call must fail");
        assert!(
            err.contains("ExternalFunction")
                && err.contains("sym=")
                && err.contains("unknown_host_fn"),
            "expected werr_syms symbol details in the diagnostic, got:\n{err}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    });
}

#[test]
fn stage2_fixed_point_lists_every_package() {
    let mut listed: Vec<String> = COMPILER_PACKAGES
        .iter()
        .map(|(_, _, rel, _, _)| *rel)
        .chain(STAGE2_PACKAGE_GAPS.iter().map(|(_, _, rel, _, _)| *rel))
        .chain(STAGE2_CRANELIFT_PACKAGES.iter().map(|(_, _, rel, _)| *rel))
        .chain(STAGE2_HOST_PACKAGES.iter().map(|(_, _, rel, _, _)| *rel))
        .map(|rel| rel.trim_start_matches("packages/").to_string())
        .chain(
            STAGE2_PACKAGES_COVERED_ELSEWHERE
                .iter()
                .map(|(d, _)| d.to_string()),
        )
        .collect();
    let n = listed.len();
    listed.sort();
    listed.dedup();
    assert_eq!(n, listed.len(), "a package is listed twice");
    let mut on_disk: Vec<String> = std::fs::read_dir(repo().join("packages"))
        .expect("read packages/")
        .filter_map(|e| {
            let e = e.ok()?;
            (e.file_type().ok()?.is_dir() && e.path().join("Loom.toml").is_file()
                || e.file_name() == "vesper-std")
                .then(|| e.file_name().to_string_lossy().into_owned())
        })
        .collect();
    on_disk.sort();
    assert_eq!(
        listed, on_disk,
        "every directory under packages/ must be in COMPILER_PACKAGES, STAGE2_CRANELIFT_PACKAGES, STAGE2_HOST_PACKAGES, STAGE2_PACKAGE_GAPS or STAGE2_PACKAGES_COVERED_ELSEWHERE (crates/vesper-stage2-gate/tests/support/packages.rs)"
    );
}
