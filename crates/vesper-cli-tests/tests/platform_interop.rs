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
mod support;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

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
    support::vesper()
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
