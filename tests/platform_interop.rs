//! Platform interop (Rust-exit gap wave 1, lane 7, #1504).
//!
//! * Per-target imports (runtime glue G4): a manifest `.vsys` path with
//!   `@OS@`, `@ARCH@` or `@TARGET@` imports a different file per object
//!   target, and a target without one is an error naming the expected path.
//! * `vesper jit`, `run` and `test` bind libraries other than the standard
//!   I/O host (G5): the C library from the process, any other link name
//!   loaded from `--link-search` or the system's search path.
//! * Windows executables with `.vsys` imports (G11) link with link.exe.
//! * More CPU instructions in `@vesper-std/cpu`: protection keys
//!   (`rdpkru`/`wrpkru`, `POR_EL0`) and fences.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::ffi_lowering::{self, ForeignChecked, Handler, Imports};
use vesper::packages::VSYS_TARGETS;
use vesper::runtime::Value;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A scratch directory, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-platform-interop-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vesper() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}

fn run(c: &mut Command) -> String {
    let out = c.output().unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fails(c: &mut Command) -> String {
    let out = c.output().unwrap();
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Whether this host is one of the four `.vsys` targets.
fn host_jit() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    ))
}

const HEADER: &str = "target = \"@TARGET@\", abi = \"@ABI@\"";
const ARTIFACT: &str =
    "artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\"";

/// A `Probe` library whose one function is `fn_line`.
fn probe(fn_line: &str) -> String {
    format!(
        "sys library Probe ({HEADER}, link = \"static\", name = \"probe\", {ARTIFACT}) {{\n    {fn_line}\n}}\n"
    )
}

/// `@t/app`, whose manifest names `vsys_field`.
fn app(t: &Temp, vsys_field: &str, main: &str, ceiling: &str) -> PathBuf {
    t.write(
        "app/Loom.toml",
        &format!(
            "[package]\nname = \"@t/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [{ceiling}]\n{vsys_field}\n"
        ),
    );
    t.write("app/src/main.ves", main);
    t.0.join("app")
}

/// The function symbols the imports for `target` declare.
fn symbols(imports: &Imports) -> Vec<String> {
    imports
        .libraries()
        .flat_map(|(l, _)| l.functions().iter().map(|f| f.symbol().to_owned()))
        .collect()
}

#[test]
fn a_per_target_path_imports_each_targets_file() {
    let t = Temp::new("os");
    t.write(
        "app/sys/probe-linux.vsys",
        &probe("fn probe_linux(x: u32) -> ForeignVal[u64];"),
    );
    t.write(
        "app/sys/probe-macos.vsys",
        &probe("fn probe_macos(x: u64) -> ForeignVal[u64];"),
    );
    let dir = app(
        &t,
        "vsys = [\"sys/probe-@OS@.vsys\"]",
        "pub fn main() -> u64 {\n    1\n}\n",
        "\"Ffi[Probe]\"",
    );
    let p = vesper::packages::load(&dir).unwrap();
    assert!(p.ok(), "{:?} {:?}", p.diagnostics, p.manifest_errors);
    assert_eq!(p.vsys.len(), 1);
    assert_eq!(p.vsys[0].targets.len(), 3, "two Linux targets and macOS");
    for target in VSYS_TARGETS {
        let got = p.imports(Some(target.triple));
        match target.os {
            "linux" => assert_eq!(symbols(&got.unwrap()), ["probe_linux"]),
            "macos" => assert_eq!(symbols(&got.unwrap()), ["probe_macos"]),
            _ => {
                let Err(e) = got else {
                    panic!("Windows has no file")
                };
                assert!(
                    e.contains("no file for x86_64-pc-windows-msvc")
                        && e.contains("sys/probe-windows.vsys"),
                    "{e}"
                );
            }
        }
        assert!(
            p.vsys[0]
                .display_for(Some(target.triple))
                .ends_with(&format!("probe-{}.vsys", target.os))
                || target.os == "windows"
        );
    }
}

#[test]
fn arch_and_triple_tokens_select_layouts() {
    let t = Temp::new("arch");
    // The same wire struct with a different layout per architecture, as
    // `epoll_event` (packed on x86_64 only).
    for (arch, fields) in [
        ("x86_64", "a: u32, b: u32"),
        ("aarch64", "a: u32, pad: u32, b: u64"),
    ] {
        t.write(
            &format!("app/sys/{arch}/probe.vsys"),
            &format!(
                "sys library Probe ({HEADER}, link = \"static\", name = \"probe\", {ARTIFACT}) {{\n    \
                 wire struct Event {{ {fields} }}\n    \
                 fn probe_event(nocapture inout e: Event) -> ForeignVal[Unit];\n}}\n"
            ),
        );
    }
    t.write(
        "app/sys/x86_64-pc-windows-msvc.vsys",
        &format!("sys library ProbeWin ({HEADER}, link = \"static\", name = \"probe_win\", {ARTIFACT}) {{\n    fn probe_win() -> ForeignVal[u64];\n}}\n"),
    );
    let dir = app(
        &t,
        "vsys = [\"sys/@ARCH@/probe.vsys\", \"sys/@TARGET@.vsys\"]",
        "pub fn main() -> u64 {\n    1\n}\n",
        "\"Ffi[Probe]\", \"Ffi[ProbeWin]\"",
    );
    let p = vesper::packages::load(&dir).unwrap();
    assert!(p.ok(), "{:?} {:?}", p.diagnostics, p.manifest_errors);
    let sizes: Vec<_> = VSYS_TARGETS
        .iter()
        .filter(|t| t.os != "windows")
        .map(|t| {
            let source = p.vsys[0].source_for(Some(t.triple)).unwrap();
            assert!(source.contains(t.triple), "{source}");
            source.contains("pad: u32")
        })
        .collect();
    assert_eq!(sizes, [false, true, true]);
    // `@TARGET@` has a file only for Windows.
    assert!(p.vsys[1].source_for(Some("x86_64-pc-windows-msvc")).is_ok());
    assert!(p.vsys[1].source_for(Some("aarch64-apple-darwin")).is_err());
}

#[test]
fn a_per_target_path_with_no_file_is_a_manifest_error() {
    let t = Temp::new("none");
    let dir = app(
        &t,
        "vsys = [\"sys/probe-@OS@.vsys\"]",
        "pub fn main() -> u64 {\n    1\n}\n",
        "",
    );
    let p = vesper::packages::load(&dir).unwrap();
    let messages: Vec<_> = p
        .manifest_errors
        .iter()
        .map(|(_, d)| d.message.clone())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("no such file for any target")),
        "{messages:?}"
    );
}

/// The C library from the process: `strlen` of a C string and `abs`.
const LIBC_VSYS: &str =
    "sys library LibC (target = \"@TARGET@\", abi = \"@ABI@\", link = \"dynamic\", name = \"c\", \
     artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {
    fn strlen(nocapture in s: CStr) -> ForeignVal[u64];
    fn abs(x: i32) -> ForeignVal[i32];
}
";

const LIBC_MAIN: &str = "pub fn main() -> u64 eff [Ffi[LibC]] {
    // A `CStr` argument may hold a NUL, so the result is a `Result`.
    let n = match LibC::strlen(in \"vesper\".as_bytes()) {
        Result::Ok(n) => n,
        Result::Err(_) => 100,
    };
    let a = LibC::abs(-30);
    wrapping_add(n, wrapping_u64(a))
}
";

fn libc_app(t: &Temp) -> PathBuf {
    t.write("app/sys/libc.vsys", LIBC_VSYS);
    app(
        t,
        "vsys = [\"sys/libc.vsys\"]",
        LIBC_MAIN,
        "\"Fail\", \"Ffi[LibC]\"",
    )
}

#[test]
fn jit_and_run_bind_the_c_library_from_the_process() {
    if !host_jit() {
        return;
    }
    let t = Temp::new("libc");
    let dir = libc_app(&t);
    for opt in [&[][..], &["--no-opt"][..]] {
        let out = run(vesper().arg("jit").arg(&dir).args(opt));
        assert_eq!(out.trim(), "36", "{opt:?}");
    }
    let out = run(vesper().arg("run").arg(&dir));
    assert_eq!(out.trim(), "36");
}

#[test]
fn a_library_that_does_not_load_is_refused_before_running() {
    let t = Temp::new("missing");
    t.write(
        "app/sys/nope.vsys",
        "sys library Nope (target = \"@TARGET@\", abi = \"@ABI@\", link = \"dynamic\", name = \"vesper_no_such_library\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n    fn nope_f() -> ForeignVal[u64];\n}\n",
    );
    let dir = app(
        &t,
        "vsys = [\"sys/nope.vsys\"]",
        "pub fn main() -> u64 eff [Ffi[Nope]] {\n    Nope::nope_f()\n}\n",
        "\"Ffi[Nope]\"",
    );
    for command in ["jit", "run"] {
        let err = fails(vesper().arg(command).arg(&dir));
        assert!(
            err.contains("vesper_no_such_library") && err.contains("could not be loaded"),
            "{err}"
        );
    }
    // A loaded library without the symbol is refused too.
    t.write(
        "app/sys/nope.vsys",
        &LIBC_VSYS.replace("fn abs(x: i32)", "fn vesper_no_such_symbol(x: i32)"),
    );
    t.write(
        "app/src/main.ves",
        "pub fn main() -> u64 eff [Ffi[LibC]] {\n    wrapping_u64(LibC::vesper_no_such_symbol(1))\n}\n",
    );
    t.write(
        "app/Loom.toml",
        "[package]\nname = \"@t/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[LibC]\"]\nvsys = [\"sys/nope.vsys\"]\n",
    );
    if host_jit() {
        let err = fails(vesper().arg("jit").arg(&dir));
        assert!(err.contains("vesper_no_such_symbol"), "{err}");
    }
}

/// A shared library built with the system C compiler, loaded by `vesper
/// jit` from `--link-search`.
#[test]
fn jit_loads_a_shared_library_from_link_search() {
    if cfg!(windows) || !host_jit() || Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    let t = Temp::new("shared");
    t.write(
        "lib/twice.c",
        "unsigned long long twice_plus(unsigned long long x, unsigned int y) { return 2 * x + y; }\n",
    );
    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    let lib = t.0.join(format!("lib/libtwice.{ext}"));
    let status = Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(&lib)
        .arg(t.0.join("lib/twice.c"))
        .status()
        .unwrap();
    assert!(status.success());
    t.write(
        "app/sys/twice.vsys",
        "sys library Twice (target = \"@TARGET@\", abi = \"@ABI@\", link = \"dynamic\", name = \"twice\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n    \
         fn twice_plus(x: u64, y: u32) -> ForeignVal[u64];\n}\n",
    );
    let dir = app(
        &t,
        "vsys = [\"sys/twice.vsys\"]",
        "pub fn main() -> u64 eff [Ffi[Twice]] {\n    Twice::twice_plus(20, 3)\n}\n",
        "\"Ffi[Twice]\"",
    );
    let err = fails(vesper().arg("jit").arg(&dir));
    assert!(err.contains("could not be loaded"), "{err}");
    for command in ["jit", "run"] {
        let out = run(vesper()
            .arg(command)
            .arg(&dir)
            .arg("--link-search")
            .arg(t.0.join("lib")));
        assert_eq!(out.trim(), "43", "{command}");
    }
    // `vesper test` takes the same directories.
    t.write(
        "app/src/main.ves",
        "pub fn main() -> u64 eff [Ffi[Twice]] {\n    Twice::twice_plus(20, 3)\n}\n\n\
         fn test_twice() -> bool eff [Ffi[Twice]] {\n    Twice::twice_plus(20, 3) == 43\n}\n",
    );
    let err = fails(vesper().arg("test").arg(&dir));
    assert!(err.contains("could not be loaded"), "{err}");
    run(vesper()
        .arg("test")
        .arg(&dir)
        .arg("--link-search")
        .arg(t.0.join("lib")));
    // A second library that defines the same symbol differently is refused:
    // the JIT and the evaluator bind by name, as a C linker does.
    t.write(
        "lib/other.c",
        "unsigned long long twice_plus(unsigned long long x, unsigned int y) { return x + y; }\n",
    );
    let status = Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(t.0.join(format!("lib/libother.{ext}")))
        .arg(t.0.join("lib/other.c"))
        .status()
        .unwrap();
    assert!(status.success());
    t.write(
        "app/sys/other.vsys",
        "sys library Other (target = \"@TARGET@\", abi = \"@ABI@\", link = \"dynamic\", name = \"other\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n    \
         fn twice_plus(x: u64, y: u32) -> ForeignVal[u64];\n}\n",
    );
    t.write(
        "app/Loom.toml",
        "[package]\nname = \"@t/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[Twice]\", \"Ffi[Other]\"]\nvsys = [\"sys/twice.vsys\", \"sys/other.vsys\"]\n",
    );
    let err = fails(
        vesper()
            .arg("jit")
            .arg(&dir)
            .arg("--link-search")
            .arg(t.0.join("lib")),
    );
    assert!(err.contains("resolves to different functions"), "{err}");
}

/// G11: an executable with `.vsys` imports builds for Windows. It links
/// only on a Windows host (or with an `lld-link` and the MSVC libraries);
/// elsewhere the build gets as far as the linker.
#[test]
fn a_windows_executable_links_its_vsys_imports() {
    let t = Temp::new("windows");
    let dir = libc_app(&t);
    let exe = t.0.join("libc.exe");
    let out = vesper()
        .arg("build")
        .arg(&dir)
        .args(["--target", "x86_64-pc-windows-msvc", "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("not supported yet"), "{err}");
    if cfg!(all(windows, target_arch = "x86_64")) {
        assert!(out.status.success(), "{err}");
        let status = Command::new(&exe).status().unwrap();
        assert_eq!(status.code(), Some(36));
    }
}

#[test]
fn executables_link_the_c_library_on_unix() {
    if cfg!(windows) || !host_jit() {
        return;
    }
    let t = Temp::new("exe");
    let dir = libc_app(&t);
    let exe = t.0.join("libc.bin");
    run(vesper().arg("build").arg(&dir).arg("-o").arg(&exe));
    let status = Command::new(&exe).status().unwrap();
    assert_eq!(status.code(), Some(36));
}

// ---- CPU instructions -------------------------------------------------

fn cpu_dir() -> PathBuf {
    fs::canonicalize("packages/vesper-std/cpu").expect("run from the repository root")
}

fn cpu_imports(triple: &str, abi: &str) -> Imports {
    let source = fs::read_to_string(cpu_dir().join("cpu.vsys"))
        .unwrap()
        .replace("@TARGET@", triple)
        .replace("@ABI@", abi);
    let mut imports = Imports::new();
    imports.add(&source, Handler::Direct).unwrap();
    imports
}

fn cpu_driver(t: &Temp, source: &str) -> PathBuf {
    let cpu = cpu_dir().display().to_string().replace('\\', "/");
    t.write(
        "drv/Loom.toml",
        &format!(
            "[package]\nname = \"@local/cpu-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\
             capability_ceiling = [\"Fail\", \"Alloc\", \"Ffi[StdCpu]\"]\n\n\
             [dependencies]\n\"@vesper-std/cpu\" = {{ version = \"^0.1\", path = \"{cpu}\" }}\n"
        ),
    );
    t.write("drv/src/main.ves", source);
    t.0.join("drv")
}

fn cpu_check(dir: &Path, triple: &str, abi: &str) -> ForeignChecked {
    let assembled = vesper::packages::load(dir).unwrap();
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    ffi_lowering::check_module(&assembled.module, &cpu_imports(triple, abi))
        .unwrap_or_else(|e| panic!("{e}: {:?}", e.diagnostics))
}

fn word(v: &Value) -> u64 {
    match v {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        Value::SInt(n, _) => *n as u64,
        other => panic!("not a word: {other:?}"),
    }
}

const X86: &str = r#"use @vesper-std/cpu::{x86_has_pku, x86_rdpkru, x86_wrpkru, x86_lfence, x86_mfence, x86_sfence};

// The three fences, then 7.
pub fn fences() -> u64 eff [Ffi[StdCpu]] {
    x86_lfence();
    x86_mfence();
    x86_sfence();
    7
}

pub fn has_pku() -> u64 eff [Ffi[StdCpu]] {
    if x86_has_pku() { 1 } else { 0 }
}

// Write back the rights register's value and read it again: 1 when equal.
pub fn pkru_round_trip() -> u64 eff [Ffi[StdCpu]] {
    let a = x86_rdpkru();
    x86_wrpkru(a);
    if x86_rdpkru() == a { 1 } else { 0 }
}

pub fn main() -> u64 eff [Ffi[StdCpu]] {
    wrapping_add(fences(), wrapping_add(has_pku(), pkru_round_trip()))
}
"#;

const AARCH64: &str = r#"use @vesper-std/cpu::{aarch64_isb, aarch64_dmb_ish, aarch64_dsb_sy, aarch64_rdpor, aarch64_wrpor};

pub fn fences() -> u64 eff [Ffi[StdCpu]] {
    aarch64_isb();
    aarch64_dmb_ish();
    aarch64_dsb_sy();
    7
}

// Compiled, never run: POR_EL0 is undefined without FEAT_S1POE.
pub fn overlay() -> u64 eff [Ffi[StdCpu]] {
    let a = aarch64_rdpor();
    aarch64_wrpor(a);
    a
}

pub fn main() -> u64 eff [Ffi[StdCpu]] {
    wrapping_add(fences(), overlay())
}
"#;

fn holds(bytes: &[u8], code: &[u8]) -> bool {
    bytes.windows(code.len()).any(|w| w == code)
}

#[test]
fn objects_hold_the_protection_key_and_fence_instructions() {
    let t = Temp::new("cpu-objects");
    for (source, symbols, targets) in [
        (
            X86,
            &[
                "vstd_cpu_rdpkru",
                "vstd_cpu_wrpkru",
                "vstd_cpu_lfence",
                "vstd_cpu_mfence",
                "vstd_cpu_sfence",
            ][..],
            &[
                ("x86_64-unknown-linux-gnu", "sysv64"),
                ("x86_64-pc-windows-msvc", "win64"),
            ][..],
        ),
        (
            AARCH64,
            &[
                "vstd_cpu_isb",
                "vstd_cpu_dmb_ish",
                "vstd_cpu_dsb_sy",
                "vstd_cpu_rdpor",
                "vstd_cpu_wrpor",
            ][..],
            &[
                ("aarch64-unknown-linux-gnu", "aapcs64"),
                ("aarch64-apple-darwin", "darwin64"),
            ][..],
        ),
    ] {
        let dir = cpu_driver(&t, source);
        for (triple, abi) in targets {
            let checked = cpu_check(&dir, triple, abi);
            let bytes = checked
                .with_lowering(Some(triple), |world| {
                    vesper::native::object_with_options(world, "main", Some(triple), true)
                        .map(|(bytes, _)| bytes)
                })
                .unwrap();
            for symbol in symbols {
                let (_, code) = ffi_lowering::cpu::function(symbol).unwrap();
                assert!(
                    holds(&bytes, &code),
                    "{symbol} is not in the {triple} object"
                );
            }
        }
    }
}

#[test]
fn the_jit_runs_fences_and_protection_keys() {
    let t = Temp::new("cpu-jit");
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        let dir = cpu_driver(&t, X86);
        let checked = cpu_check(&dir, "x86_64-unknown-linux-gnu", "sysv64");
        for optimize in [true, false] {
            let v = checked.jit_with_options("fences", &[], optimize).unwrap();
            assert_eq!(word(&v), 7);
            let has = word(&checked.jit_with_options("has_pku", &[], optimize).unwrap());
            #[cfg(target_arch = "x86_64")]
            {
                let max = std::arch::x86_64::__cpuid(0).eax;
                let ospke = max >= 7 && (std::arch::x86_64::__cpuid_count(7, 0).ecx >> 4) & 1 == 1;
                assert_eq!(has == 1, ospke);
            }
            // RDPKRU is an invalid opcode unless the OS enabled keys.
            if has == 1 {
                let v = checked
                    .jit_with_options("pkru_round_trip", &[], optimize)
                    .unwrap();
                assert_eq!(word(&v), 1);
            }
        }
    } else if cfg!(target_arch = "aarch64") {
        let dir = cpu_driver(&t, AARCH64);
        let (triple, abi) = if cfg!(target_os = "macos") {
            ("aarch64-apple-darwin", "darwin64")
        } else {
            ("aarch64-unknown-linux-gnu", "aapcs64")
        };
        let checked = cpu_check(&dir, triple, abi);
        for optimize in [true, false] {
            let v = checked.jit_with_options("fences", &[], optimize).unwrap();
            assert_eq!(word(&v), 7);
        }
    }
}

// ---- Assembly objects (G10) -------------------------------------------

/// `twice_plus(x, y) = 2 * x + y` in each target's assembly.
const TWICE_ASM: [(&str, &str); 4] = [
    (
        "asm/twice-x86_64-unknown-linux-gnu.s",
        "    .text\n    .globl twice_plus\n    .type twice_plus, @function\ntwice_plus:\n    movl %esi, %eax\n    leaq (%rax,%rdi,2), %rax\n    ret\n    .section .note.GNU-stack,\"\",@progbits\n",
    ),
    (
        "asm/twice-aarch64-unknown-linux-gnu.s",
        "    .text\n    .globl twice_plus\n    .type twice_plus, %function\ntwice_plus:\n    add x0, x0, x0\n    add x0, x0, w1, uxtw\n    ret\n    .section .note.GNU-stack,\"\",%progbits\n",
    ),
    (
        "asm/twice-aarch64-apple-darwin.s",
        "    .text\n    .globl _twice_plus\n    .p2align 2\n_twice_plus:\n    add x0, x0, x0\n    add x0, x0, w1, uxtw\n    ret\n",
    ),
    (
        "asm/twice-x86_64-pc-windows-msvc.asm",
        ".code\ntwice_plus PROC\n    mov eax, edx\n    lea rax, [rax+rcx*2]\n    ret\ntwice_plus ENDP\nEND\n",
    ),
];

/// A package whose `objects` define the static library its `.vsys` names.
fn objects_app(t: &Temp, objects: &str) -> PathBuf {
    for (path, text) in TWICE_ASM {
        t.write(&format!("app/{path}"), text);
    }
    t.write(
        "app/sys/twice.vsys",
        "sys library Twice (target = \"@TARGET@\", abi = \"@ABI@\", link = \"static\", name = \"twice_asm\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n    \
         fn twice_plus(x: u64, y: u32) -> ForeignVal[u64];\n}\n",
    );
    app(
        t,
        &format!("vsys = [\"sys/twice.vsys\"]\nobjects = {objects}"),
        "pub fn main() -> u64 eff [Ffi[Twice]] {\n    Twice::twice_plus(20, 3)\n}\n",
        "\"Ffi[Twice]\"",
    )
}

const PER_TARGET_OBJECTS: &str = "[\"asm/twice-@TARGET@.s\", \"asm/twice-@TARGET@.asm\"]";

/// G10: `vesper build` assembles each target's source and links it in
/// place of the static library.
#[test]
fn executables_link_package_assembly() {
    if !host_jit() || (!cfg!(windows) && Command::new("cc").arg("--version").output().is_err()) {
        return;
    }
    let t = Temp::new("asm");
    let dir = objects_app(&t, PER_TARGET_OBJECTS);
    let exe = t.0.join(if cfg!(windows) {
        "twice.exe"
    } else {
        "twice.bin"
    });
    run(vesper().arg("build").arg(&dir).arg("-o").arg(&exe));
    let status = Command::new(&exe).status().unwrap();
    assert_eq!(status.code(), Some(43));
    // The assembled objects beside the output are removed.
    let left: Vec<_> = fs::read_dir(&t.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("vesper-asm") || n.contains("vesper.o"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

/// A C source compiles for the target and links in place of the static
/// library (BLAKE3 in @vesper-std/hash-sys is built this way).
#[test]
fn executables_link_a_package_c_source() {
    if !host_jit() || (!cfg!(windows) && Command::new("cc").arg("--version").output().is_err()) {
        return;
    }
    let t = Temp::new("c-object");
    t.write(
        "app/c/twice.c",
        "#include <stdint.h>\nuint64_t twice_plus(uint64_t x, uint32_t y) { return 2 * x + y; }\n",
    );
    let dir = objects_app(&t, "[\"c/twice.c\"]");
    let exe = t.0.join(if cfg!(windows) {
        "twice.exe"
    } else {
        "twice.bin"
    });
    run(vesper().arg("build").arg(&dir).arg("-o").arg(&exe));
    assert_eq!(Command::new(&exe).status().unwrap().code(), Some(43));
}

/// A prebuilt object links as it is.
#[test]
fn executables_link_a_prebuilt_object() {
    if cfg!(windows) || !host_jit() || Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    let t = Temp::new("prebuilt");
    let dir = objects_app(&t, "[\"obj/twice.o\"]");
    let (source, _) = TWICE_ASM
        .iter()
        .find(|(p, _)| p.contains(vesper::packages::vsys_target(None).unwrap().0))
        .unwrap();
    fs::create_dir_all(dir.join("obj")).unwrap();
    let status = Command::new("cc")
        .arg("-c")
        .arg(dir.join(source))
        .arg("-o")
        .arg(dir.join("obj/twice.o"))
        .status()
        .unwrap();
    assert!(status.success());
    let exe = t.0.join("twice.bin");
    run(vesper().arg("build").arg(&dir).arg("-o").arg(&exe));
    assert_eq!(Command::new(&exe).status().unwrap().code(), Some(43));
}

/// The JIT and the evaluator do not load package objects; they say so
/// rather than looking for a `twice_asm` library.
#[test]
fn jit_and_run_refuse_assembly_objects() {
    if !host_jit() {
        return;
    }
    let t = Temp::new("asm-jit");
    let dir = objects_app(&t, PER_TARGET_OBJECTS);
    for command in ["jit", "run"] {
        let err = fails(vesper().arg(command).arg(&dir));
        assert!(
            err.contains("objects of @t/app") && err.contains("only `vesper build` links"),
            "{command}: {err}"
        );
    }
}

/// MASM source assembles only for Windows.
#[test]
fn masm_source_is_refused_for_other_targets() {
    if cfg!(windows) || !host_jit() {
        return;
    }
    let t = Temp::new("masm");
    let dir = objects_app(&t, "[\"asm/twice-x86_64-pc-windows-msvc.asm\"]");
    let err = fails(vesper().arg("build").arg(&dir).arg("-o").arg(t.0.join("x")));
    assert!(err.contains("assembles only for Windows targets"), "{err}");
}

#[test]
fn bad_object_paths_are_manifest_errors() {
    for (objects, want) in [
        ("[\"asm/twice.cpp\"]", "expected a C source"),
        ("[\"asm/none.s\"]", "no such file"),
        ("[\"asm/none-@OS@.s\"]", "no such file for any target"),
        ("[\"../x.s\"]", "no `..`"),
        (
            "[\"asm/twice-@TARGET@.s\", \"./asm/twice-@TARGET@.s\"]",
            "named twice",
        ),
    ] {
        let t = Temp::new("bad-objects");
        let dir = objects_app(&t, objects);
        let assembled = vesper::packages::load(&dir).unwrap();
        let messages: Vec<String> = assembled
            .manifest_errors
            .iter()
            .map(|(_, d)| d.message.clone())
            .collect();
        assert!(
            messages.iter().any(|m| m.contains(want)),
            "{objects}: {messages:?}"
        );
    }
}

/// A per-target entry names a file for each target that has one.
#[test]
fn per_target_objects_select_each_targets_file() {
    let t = Temp::new("asm-targets");
    let dir = objects_app(&t, PER_TARGET_OBJECTS);
    let assembled = vesper::packages::load(&dir).unwrap();
    assert!(assembled.ok(), "{:?}", assembled.manifest_errors);
    assert_eq!(assembled.objects.len(), 2);
    for target in VSYS_TARGETS {
        let files: Vec<PathBuf> = assembled
            .objects
            .iter()
            .filter_map(|o| o.file_for(Some(target.triple)).unwrap())
            .map(Path::to_path_buf)
            .collect();
        assert_eq!(files.len(), 1, "{}", target.triple);
        assert!(
            files[0].to_string_lossy().contains(target.triple),
            "{files:?}"
        );
    }
}

/// Objects link only through a static library the program calls, which
/// its `Ffi[Lib]` ceiling confines (manuscript §16.3): a package cannot slip
/// native code, such as a constructor, into an executable without one.
#[test]
fn objects_link_only_through_a_called_static_library() {
    let t = Temp::new("asm-only");
    for (path, text) in TWICE_ASM {
        t.write(&format!("app/{path}"), text);
    }
    let dir = app(
        &t,
        &format!("objects = {PER_TARGET_OBJECTS}"),
        "pub fn main() -> u64 {\n    7\n}\n",
        "",
    );
    let assembled = vesper::packages::load(&dir).unwrap();
    let messages: Vec<String> = assembled
        .manifest_errors
        .iter()
        .map(|(_, d)| d.message.clone())
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("it names none")),
        "{messages:?}"
    );
    if cfg!(windows) || !host_jit() || Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    // With the library imported but not called, nothing is linked.
    let t = Temp::new("asm-unused");
    let dir = objects_app(&t, PER_TARGET_OBJECTS);
    t.write("app/src/main.ves", "pub fn main() -> u64 {\n    7\n}\n");
    let exe = t.0.join("unused.bin");
    run(vesper().arg("build").arg(&dir).arg("-o").arg(&exe));
    assert_eq!(Command::new(&exe).status().unwrap().code(), Some(7));
    let bytes = fs::read(&exe).unwrap();
    assert!(!bytes.windows(10).any(|w| w == b"twice_plus"));
}

#[test]
fn test_runner_refuses_a_called_assembly_backed_library() {
    if !host_jit() {
        return;
    }
    let t = Temp::new("asm-test");
    let dir = objects_app(&t, PER_TARGET_OBJECTS);
    t.write(
        "app/src/main.ves",
        "pub fn test_twice() -> bool eff [Ffi[Twice]] { Twice::twice_plus(20, 3) == 43 }\n",
    );
    let err = fails(vesper().arg("test").arg(&dir));
    assert!(
        err.contains("objects of @t/app") && err.contains("only `vesper build` links"),
        "{err}"
    );
}

// ---- Per-target .vsys constants (#1504 part 3) ----------------------------

#[test]
fn reading_constants_does_not_load_or_link_the_library() {
    let t = Temp::new("consts-only");
    let dir = app(
        &t,
        "vsys = [\"sys/flags.vsys\"]",
        "const COMBINED: i32 = Flags::O_CREAT | 4;\n\
         pub fn main() -> i32 { match COMBINED { Flags::O_CREAT => 0, _ => COMBINED } }\n\
         pub fn test_flags() -> bool { main() == 68 }\n",
        "",
    );
    for decl in ["", "fn unused() -> ForeignVal[i32];"] {
        t.write("app/sys/flags.vsys", &format!(
            "sys library Flags ({HEADER}, link = \"dynamic\", name = \"vesper_absent_flags_1581\", {ARTIFACT}) {{ const O_CREAT: i32 = 0o100; {decl} }}"
        ));
        run(vesper().arg("check").arg(&dir));
        assert_eq!(run(vesper().arg("run").arg(&dir)).trim(), "68");
        run(vesper().arg("test").arg(&dir));
        if host_jit() {
            assert_eq!(run(vesper().arg("jit").arg(&dir)).trim(), "68");
            run(vesper()
                .arg("build")
                .arg(&dir)
                .arg("-o")
                .arg(t.0.join("flags.bin")));
        }
    }
}

fn flags_app(t: &Temp, source: &str) -> PathBuf {
    for (os, o_creat, neg, big) in [
        ("linux", "0o100", "-7", "0x10_00"),
        ("macos", "0x200", "-11", "0x20_00"),
        ("windows", "0x100", "-13", "0x30_00"),
    ] {
        t.write(
            &format!("app/sys/flags-{os}.vsys"),
            &format!(
                "sys library Flags (target = \"@TARGET@\", abi = \"@ABI@\", link = \"dynamic\", name = \"c\", \
                 artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {{\n    \
                 const O_CREAT: i32 = {o_creat};\n    \
                 const NEG: i64 = {neg};\n    \
                 const BIG: u64 = {big};\n    \
                 const SMALL: u8 = 0b101;\n    \
                 const PAGE: usize = 4096;\n    \
                 fn abs(x: i32) -> ForeignVal[i32];\n\
                 }}\n"
            ),
        );
    }
    app(
        t,
        "vsys = [\"sys/flags-@OS@.vsys\"]",
        source,
        "\"Ffi[Flags]\"",
    )
}

#[test]
fn per_target_vsys_constants_work_across_targets_jit_run_and_patterns() {
    let t = Temp::new("consts");
    let dir = flags_app(
        &t,
        "const COMBINED: i32 = Flags::O_CREAT | 4;\n\n\
         fn match_flag(x: i32) -> i64 {\n    \
             match x {\n        \
                 Flags::O_CREAT => 10,\n        \
                 _ => 0,\n    \
             }\n\
         }\n\n\
         fn match_neg(x: i64) -> i64 {\n    \
             match x {\n        \
                 Flags::NEG => 5,\n        \
                 _ => 0,\n    \
             }\n\
         }\n\n\
         pub fn pure_sum() -> i64 {\n    \
             let c = Flags::O_CREAT;\n    \
             let neg = Flags::NEG;\n    \
             let ok_c: i32 = c;\n    \
             let ok_comb: i32 = COMBINED;\n    \
             let ok_big: u64 = Flags::BIG;\n    \
             let ok_page: usize = Flags::PAGE;\n    \
             let ok_small: u8 = Flags::SMALL;\n    \
             let mut total: i64 = widen_i64(ok_comb);\n    \
             total = wrapping_add(total, neg);\n    \
             total = wrapping_add(total, match_flag(ok_c));\n    \
             total = wrapping_add(total, match_neg(neg));\n    \
             total = wrapping_add(total, widen_i64(ok_small));\n    \
             total = wrapping_add(total, wrapping_i64(ok_big));\n    \
             wrapping_add(total, wrapping_i64(ok_page))\n\
         }\n\n\
         pub fn main() -> u64 eff [Ffi[Flags]] {\n    \
             let f = Flags::abs(Flags::O_CREAT);\n    \
             wrapping_u64(wrapping_add(pure_sum(), widen_i64(f)))\n\
         }\n",
    );
    let loaded = vesper::packages::load(&dir).unwrap();
    assert!(
        loaded.ok(),
        "{:?} {:?}",
        loaded.diagnostics,
        loaded.manifest_errors
    );

    extern "C" fn test_abs(x: i32) -> i32 {
        x.wrapping_abs()
    }

    for (triple, o_creat, want) in [
        (
            "x86_64-unknown-linux-gnu",
            64_i64,
            (64 | 4) + (-7) + 10 + 5 + 5 + 0x1000 + 4096 + 64,
        ),
        (
            "aarch64-unknown-linux-gnu",
            64_i64,
            (64 | 4) + (-7) + 10 + 5 + 5 + 0x1000 + 4096 + 64,
        ),
        (
            "aarch64-apple-darwin",
            0x200_i64,
            (0x200 | 4) + (-11) + 10 + 5 + 5 + 0x2000 + 4096 + 0x200,
        ),
        (
            "x86_64-pc-windows-msvc",
            0x100_i64,
            (0x100 | 4) + (-13) + 10 + 5 + 5 + 0x3000 + 4096 + 0x100,
        ),
    ] {
        let _ = o_creat;
        let imports = loaded.imports(Some(triple)).unwrap();
        let checked = ffi_lowering::check_module(&loaded.module, &imports)
            .unwrap_or_else(|e| panic!("{triple}: {e}"));
        let obj = checked.object("main", Some(triple)).unwrap();
        assert!(!obj.bytes.is_empty(), "{triple}");
        if vesper_ffi::sys::Target::LinuxX86_64.triple() == triple
            && cfg!(all(target_os = "linux", target_arch = "x86_64"))
            || vesper_ffi::sys::Target::LinuxAarch64.triple() == triple
                && cfg!(all(target_os = "linux", target_arch = "aarch64"))
            || vesper_ffi::sys::Target::DarwinAarch64.triple() == triple
                && cfg!(all(target_os = "macos", target_arch = "aarch64"))
            || vesper_ffi::sys::Target::WindowsX86_64.triple() == triple
                && cfg!(all(target_os = "windows", target_arch = "x86_64"))
        {
            for optimize in [true, false] {
                let v = checked
                    .jit_with_options("main", &[("abs", test_abs as *const u8)], optimize)
                    .unwrap();
                assert_eq!(word(&v), want as u64, "{triple} optimize={optimize}");
            }
            let ev = checked
                .evaluate("pure_sum", |_| Err("unused".into()))
                .unwrap();
            assert_eq!(word(&ev), (want - o_creat) as u64, "{triple} eval");
            if host_jit() {
                let run_out = run(vesper().arg("run").arg(&dir));
                assert_eq!(run_out.trim(), want.to_string());
                let jit_out = run(vesper().arg("jit").arg(&dir));
                assert_eq!(jit_out.trim(), want.to_string());
            }
        }
    }
}

#[test]
fn vsys_constant_types_and_misuses_are_checked() {
    let t = Temp::new("consts-bad");
    // Declared `O_CREAT: i32` cannot be used where `u64` is expected.
    let dir = flags_app(
        &t,
        "pub fn main() -> u64 {\n    let x: u64 = Flags::O_CREAT;\n    x\n}\n",
    );
    let loaded = vesper::packages::load(&dir).unwrap();
    let imports = loaded.imports(None).unwrap();
    assert!(ffi_lowering::check_module(&loaded.module, &imports).is_err());

    // Each value fits the scrutinee, but its declared integer kind must match.
    for (ty, pattern, diagnostic) in [
        ("u8", "Flags::O_CREAT", "expected u8, found i32"),
        (
            "(u32, u8)",
            "(Flags::O_CREAT, _)",
            "expected u32, found i32",
        ),
        ("u64", "Flags::PAGE", "expected u64, found usize"),
    ] {
        let source = format!("fn probe(x: {ty}) -> i32 {{ match x {{ {pattern} => 1, _ => 0 }} }}");
        let error =
            ffi_lowering::check_source(&source, &imports).expect_err("pattern type mismatch");
        assert!(
            error.diagnostics.iter().any(|d| d.code == "E0200"
                && d.message == diagnostic
                && source[d.span.clone()].starts_with("Flags::")),
            "{error:?}"
        );
    }

    // Calling a constant as a function is rejected.
    let dir2 = flags_app(&t, "pub fn main() -> i32 {\n    Flags::O_CREAT(1)\n}\n");
    let loaded2 = vesper::packages::load(&dir2).unwrap();
    let imports2 = loaded2.imports(None).unwrap();
    assert!(ffi_lowering::check_module(&loaded2.module, &imports2).is_err());

    // Unknown constant name fails cleanly.
    let dir3 = flags_app(&t, "pub fn main() -> i32 {\n    Flags::MISSING\n}\n");
    let loaded3 = vesper::packages::load(&dir3).unwrap();
    let imports3 = loaded3.imports(None).unwrap();
    assert!(ffi_lowering::check_module(&loaded3.module, &imports3).is_err());
}
