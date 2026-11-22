//! `@vesper-std/cpu` (audit gap G4): CPU instructions Cranelift has no
//! instruction for, emitted by the native backend as leaf functions of
//! machine code (`ffi_lowering::cpu`).
//!
//! * The JIT runs the reads that every common system enables in user
//!   space: the time-stamp counter and (where CPUID reports it) RDRAND on
//!   x86_64, and the generic
//!   timer on AArch64. The performance counter reads (RDPMC, PMCCNTR_EL0,
//!   PMXEVCNTR_EL0) trap unless the operating system grants them, so they
//!   are only compiled.
//! * Objects for both architectures hold each function's code, whatever
//!   the host is.
//! * A function of the other architecture is a compile error, and the
//!   evaluator refuses CPU instructions by name.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::ffi_lowering::{self, ForeignChecked, Handler, Imports};
use vesper::runtime::Value;

fn cpu_dir() -> PathBuf {
    fs::canonicalize("packages/vesper-std/cpu").expect("run from the repository root")
}

fn host_target() -> (&'static str, &'static str) {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        ("aarch64-unknown-linux-gnu", "aapcs64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", "darwin64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        ("x86_64-pc-windows-msvc", "win64")
    } else {
        ("x86_64-unknown-linux-gnu", "sysv64")
    }
}

fn imports(target: (&str, &str)) -> Imports {
    let source = fs::read_to_string(cpu_dir().join("cpu.vsys"))
        .unwrap()
        .replace("@TARGET@", target.0)
        .replace("@ABI@", target.1);
    let mut imports = Imports::new();
    imports.add(&source, Handler::Direct).unwrap();
    imports
}

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A driver package over `@vesper-std/cpu` whose `src/main.ves` is
/// `source`.
fn driver(source: &str) -> Temp {
    let dir = std::env::temp_dir().join(format!(
        "vesper-std-cpu-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    let cpu = cpu_dir().display().to_string().replace('\\', "/");
    fs::write(
        dir.join("Loom.toml"),
        format!(
            "[package]\nname = \"@local/std-cpu-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\
             capability_ceiling = [\"Fail\", \"Alloc\", \"Ffi[StdCpu]\"]\n\n\
             [dependencies]\n\"@vesper-std/cpu\" = {{ version = \"^0.1\", path = \"{cpu}\" }}\n"
        ),
    )
    .unwrap();
    fs::write(dir.join("src/main.ves"), source).unwrap();
    Temp(dir)
}

fn check(dir: &Path, target: (&str, &str)) -> ForeignChecked {
    let assembled = vesper::packages::load(dir).unwrap();
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    ffi_lowering::check_module(&assembled.module, &imports(target)).unwrap_or_else(|e| {
        let shown: Vec<_> = e
            .diagnostics
            .iter()
            .map(|d| assembled.sources.localize(d))
            .collect();
        panic!("{e}: {shown:?}")
    })
}

fn word(v: &Value) -> u64 {
    match v {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        Value::SInt(n, _) => *n as u64,
        other => panic!("not a word: {other:?}"),
    }
}

const X86: &str = r#"use @vesper-std/cpu::{x86_rdtsc, x86_rdpmc, x86_rdrand64};

// 1 when two time-stamp reads are nonzero and do not go backwards.
pub fn tsc() -> u64 eff [Ffi[StdCpu]] {
    let a = x86_rdtsc();
    let b = x86_rdtsc();
    if a != 0 && b >= a { 1 } else { 0 }
}

// How many of 16 RDRAND tries gave a word, plus 100 when two words differ.
pub fn rand() -> u64 eff [Ffi[StdCpu]] {
    let mut got: u64 = 0;
    let mut first: u64 = 0;
    let mut differ: u64 = 0;
    for i in 0..16 {
        match x86_rdrand64() {
            Option::Some(v) => {
                if got == 0 {
                    first = v;
                } else if v != first {
                    differ = 100;
                }
                got = wrapping_add(got, 1);
            },
            Option::None => {},
        }
    }
    wrapping_add(got, differ)
}

// Compiled, never run: RDPMC traps without CR4.PCE.
pub fn counter(index: u32) -> u64 eff [Ffi[StdCpu]] {
    x86_rdpmc(index)
}

pub fn main() -> u64 eff [Ffi[StdCpu]] {
    wrapping_add(tsc(), wrapping_add(rand(), counter(0)))
}
"#;

const AARCH64: &str = r#"use @vesper-std/cpu::{aarch64_cntvct, aarch64_pmccntr, aarch64_pmevcntr, aarch64_rndr};

// 1 when two generic-timer reads do not go backwards.
pub fn timer() -> u64 eff [Ffi[StdCpu]] {
    let a = aarch64_cntvct();
    let b = aarch64_cntvct();
    if b >= a { 1 } else { 0 }
}

// Compiled, never run: these trap without PMUSERENR_EL0 or FEAT_RNG.
pub fn counters() -> u64 eff [Ffi[StdCpu]] {
    let r = match aarch64_rndr() {
        Option::Some(v) => v,
        Option::None => 0,
    };
    wrapping_add(wrapping_add(aarch64_pmccntr(), aarch64_pmevcntr(3)), r)
}

pub fn main() -> u64 eff [Ffi[StdCpu]] {
    wrapping_add(timer(), counters())
}
"#;

/// Whether `code` occurs in `bytes`.
fn holds(bytes: &[u8], code: &[u8]) -> bool {
    bytes.windows(code.len()).any(|w| w == code)
}

fn object(source: &str, target: (&str, &str)) -> Result<Vec<u8>, String> {
    let pkg = driver(source);
    let checked = check(&pkg.0, target);
    checked
        .with_lowering(Some(target.0), |world| {
            vesper::native::object_with_options(world, "main", Some(target.0), true)
                .map(|(bytes, _)| bytes)
        })
        .map_err(|e| e.to_string())
}

#[test]
fn objects_hold_each_instruction() {
    let linux = ("x86_64-unknown-linux-gnu", "sysv64");
    let bytes = object(X86, linux).unwrap();
    for symbol in ["vstd_cpu_rdtsc", "vstd_cpu_rdpmc", "vstd_cpu_rdrand64"] {
        let (_, code) = ffi_lowering::cpu::function(symbol).unwrap();
        assert!(holds(&bytes, &code), "{symbol} is not in the x86_64 object");
    }
    let windows = ("x86_64-pc-windows-msvc", "win64");
    let bytes = object(X86, windows).unwrap();
    let (_, code) = ffi_lowering::cpu::function("vstd_cpu_rdpmc").unwrap();
    assert!(holds(&bytes, &code), "rdpmc is not in the Windows object");
    for target in [
        ("aarch64-unknown-linux-gnu", "aapcs64"),
        ("aarch64-apple-darwin", "darwin64"),
    ] {
        let bytes = object(AARCH64, target).unwrap();
        for symbol in [
            "vstd_cpu_cntvct",
            "vstd_cpu_pmccntr",
            "vstd_cpu_pmevcntr",
            "vstd_cpu_rndr",
        ] {
            let (_, code) = ffi_lowering::cpu::function(symbol).unwrap();
            assert!(
                holds(&bytes, &code),
                "{symbol} is not in the {} object",
                target.0
            );
        }
    }
}

#[test]
fn the_other_architecture_does_not_compile() {
    let e = object(X86, ("aarch64-unknown-linux-gnu", "aapcs64")).unwrap_err();
    assert!(e.contains("x86_64 instruction"), "{e}");
    let e = object(AARCH64, ("x86_64-unknown-linux-gnu", "sysv64")).unwrap_err();
    assert!(e.contains("aarch64 instruction"), "{e}");
}

#[test]
fn the_jit_runs_the_unprivileged_reads() {
    let host = host_target();
    if cfg!(target_arch = "x86_64") {
        let pkg = driver(X86);
        let checked = check(&pkg.0, host);
        for optimize in [true, false] {
            let tsc = checked.jit_with_options("tsc", &[], optimize).unwrap();
            assert_eq!(word(&tsc), 1, "rdtsc (optimize {optimize})");
            // RDRAND is an invalid opcode on a CPU without it, so the
            // read runs only where CPUID reports it.
            #[cfg(target_arch = "x86_64")]
            if std::arch::is_x86_feature_detected!("rdrand") {
                let rand = word(&checked.jit_with_options("rand", &[], optimize).unwrap());
                assert!(rand >= 100 + 8, "rdrand gave {rand} (optimize {optimize})");
            }
        }
    } else if cfg!(target_arch = "aarch64") {
        let pkg = driver(AARCH64);
        let checked = check(&pkg.0, host);
        for optimize in [true, false] {
            let timer = checked.jit_with_options("timer", &[], optimize).unwrap();
            assert_eq!(word(&timer), 1, "cntvct (optimize {optimize})");
        }
    }
}

#[test]
fn the_evaluator_refuses_cpu_instructions() {
    let host = host_target();
    let (source, entry) = if cfg!(target_arch = "aarch64") {
        (AARCH64, "timer")
    } else {
        (X86, "tsc")
    };
    let pkg = driver(source);
    let checked = check(&pkg.0, host);
    let eval = ffi_lowering::EvalHost::new(&imports(host), &[]).unwrap();
    let e = checked
        .evaluate(entry, |call| eval.call(call))
        .unwrap_err()
        .to_string();
    assert!(e.contains("CPU instruction"), "{e}");
}
