//! `.vsys` imports carried by packages, `bool` and C strings (self-hosting
//! SH1 lane 7, #1150).
//!
//! * A package names its `.vsys` files in its manifest (`[package] vsys`);
//!   `packages::load` reads them for every package in the graph, so a
//!   dependency carries its own imports to libraries and executables.
//! * `bool` parameters and results cross as one byte (this already worked
//!   on main; the test keeps it working).
//! * `nocapture in s: CStr` passes a terminated copy of a `Slice[u8]` and
//!   refuses one holding a NUL; `-> ForeignVal[T] errno` pairs the result
//!   with the OS error code; `ForeignVal[NonNull[H]]` has no error arm.
//! * `vesper run` reaches the standard I/O host through the evaluator.
mod support;
use std::{fs, path::PathBuf, process::Command};

/// A scratch directory, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("vesper-vsys-packages-{tag}-{}", std::process::id()));
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

const PROBE_VSYS: &str = "sys library Probe (target = \"@TARGET@\", abi = \"@ABI@\", link = \"static\", name = \"probe\", \
     artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {
    fn probe_not(b: bool) -> ForeignVal[bool];
}
";

/// `@t/app` (root, an executable) over `@t/flip`, which carries `Probe`.
fn probe_graph(t: &Temp, vsys_field: &str) -> PathBuf {
    t.write(
        "flip/Loom.toml",
        &format!(
            "[package]\nname = \"@t/flip\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[Probe]\"]\n{vsys_field}\n"
        ),
    );
    t.write("flip/sys/probe.vsys", PROBE_VSYS);
    t.write(
        "flip/src/lib.ves",
        "pub fn flip(b: bool) -> bool eff [Ffi[Probe]] {\n    match Probe::probe_not(b) {\n        Result::Ok(v) => v,\n        Result::Err(_) => b,\n    }\n}\n",
    );
    t.write(
        "app/Loom.toml",
        "[package]\nname = \"@t/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[Probe]\"]\n\n[dependencies]\n\"@t/flip\" = { version = \"^0.1\", path = \"../flip\" }\n",
    );
    t.write(
        "app/src/main.ves",
        "use @t/flip::flip;\n\npub fn main() -> u64 eff [Ffi[Probe]] {\n    if flip(false) { 7 } else { 9 }\n}\n",
    );
    t.0.join("app")
}

fn vesper() -> Command {
    support::vesper()
}

fn repo() -> PathBuf {
    support::repo_root()
}

/// `@t/tool`, an executable over `@vesper-std/fs` as a dependency, with no
/// `.vsys` text of its own: it writes, reads back and removes a file.
fn fs_tool(t: &Temp) -> PathBuf {
    let fs_dir = repo().join("packages/vesper-std/fs");
    let file = t.0.join("probe.txt");
    let lit = file.to_string_lossy().replace('\\', "/");
    t.write(
        "tool/Loom.toml",
        &format!(
            "[package]\nname = \"@t/tool\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Fail\", \"Alloc\", \"Ffi[StdFs]\"]\n\n[dependencies]\n\"@vesper-std/fs\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
            fs_dir.to_string_lossy().replace('\\', "/")
        ),
    );
    t.write(
        "tool/src/main.ves",
        &format!(
            "use @vesper-std/fs::{{IoError, write, read, remove_file, exists}};\n\n\
             pub fn main() -> u64 eff [Ffi[StdFs]] {{\n    \
             handle Fail[IoError] in {{\n        \
             write(in \"{lit}\", in \"vsys\".as_bytes());\n        \
             let back = read(in \"{lit}\");\n        \
             let n = back.len();\n        \
             remove_file(in \"{lit}\");\n        \
             if exists(in \"{lit}\") {{ 0 }} else {{ n }}\n    \
             }} with {{\n        return v => v,\n        raise(e, _) => wrapping_add(1000, widen_u64(e.kind)),\n    }}\n}}\n"
        ),
    );
    t.0.join("tool")
}

fn host_jit() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    ))
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

#[test]
fn cli_checks_jits_and_runs_an_executable_over_std_fs() {
    if !host_jit() {
        return;
    }
    let t = Temp::new("cli");
    let tool = fs_tool(&t);
    let checked = run(vesper().arg("check").arg(&tool));
    assert!(checked.contains("checked @t/tool"), "{checked}");
    for opt in [&[][..], &["--no-opt"][..]] {
        let out = run(vesper().arg("jit").arg(&tool).args(opt));
        assert_eq!(out.trim(), "4", "{opt:?}");
    }
    // The evaluator reaches the same host through `EvalHost`.
    let out = run(vesper().arg("run").arg(&tool));
    assert_eq!(out.trim(), "4");
}

#[test]
fn cli_reports_a_dependency_error_at_its_file() {
    let t = Temp::new("err");
    let tool = fs_tool(&t);
    // A call the library does not declare.
    t.write(
        "tool/src/main.ves",
        "pub fn main() -> u64 eff [Ffi[StdFs]] {\n    StdFs::vstd_fs_nope()\n}\n",
    );
    let out = vesper().arg("check").arg(&tool).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("vstd_fs_nope"), "{err}");
}

#[test]
fn jit_refuses_a_library_it_cannot_load() {
    let t = Temp::new("nohost");
    let app = probe_graph(&t, "vsys = [\"sys/probe.vsys\"]");
    let out = vesper().arg("jit").arg(&app).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("could not be loaded"), "{err}");
}

#[test]
fn a_library_build_links_a_carried_import_by_name() {
    let t = Temp::new("lib");
    let app = probe_graph(&t, "vsys = [\"sys/probe.vsys\"]");
    fs::remove_file(app.join("src/main.ves")).unwrap();
    t.write(
        "app/src/lib.ves",
        "// @export vsys Probe rust=crate::probe\nuse @t/flip::flip;\n\npub fn go(b: bool) -> u64 eff [Ffi[Probe]] {\n    if flip(b) { 1 } else { 2 }\n}\n",
    );
    let binding = t.0.join("binding.rs");
    run(vesper()
        .args(["build", "--lib", "--lib-name", "app"])
        .arg(&app)
        .arg("-o")
        .arg(t.0.join("app.o"))
        .arg("--rust-binding")
        .arg(&binding));
    let text = fs::read_to_string(&binding).unwrap();
    assert!(text.contains("crate::probe::probe_not"), "{text}");
    // A directive naming a library no package carries is an error.
    t.write(
        "app/src/lib.ves",
        "// @export vsys Nope rust=crate::nope\npub fn go() -> u64 {\n    1\n}\n",
    );
    let out = vesper()
        .args(["build", "--lib", "--lib-name", "app"])
        .arg(&app)
        .arg("-o")
        .arg(t.0.join("app.o"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no loaded package carries"), "{err}");
}

/// `vesper build` links an executable with the static library a dependency's
/// `.vsys` names: a C `probe_not` archived as `libprobe.a`. Unix hosts with
/// `cc` and `ar` (Windows executables with imports are not linked yet).
#[test]
fn build_links_an_executable_with_a_dependency_library() {
    if cfg!(windows) || !host_jit() {
        return;
    }
    let t = Temp::new("exe");
    let app = probe_graph(&t, "vsys = [\"sys/probe.vsys\"]");
    t.write(
        "c/probe.c",
        "#include <stdbool.h>\nbool probe_not(bool b) { return !b; }\n",
    );
    let c = t.0.join("c");
    run(Command::new("cc")
        .current_dir(&c)
        .args(["-c", "probe.c", "-o", "probe.o"]));
    run(Command::new("ar")
        .current_dir(&c)
        .args(["rcs", "libprobe.a", "probe.o"]));
    let exe = t.0.join("app.bin");
    run(vesper()
        .arg("build")
        .arg(&app)
        .arg("-o")
        .arg(&exe)
        .arg("--link-search")
        .arg(&c));
    let status = Command::new(&exe).status().unwrap();
    assert_eq!(status.code(), Some(7));
}

/// An executable over a package with imports still runs the verifiers its
/// capability effects imply: `Realtime` code calling `fmod` is refused.
#[test]
fn build_keeps_capability_implied_verifiers() {
    if cfg!(windows) || !host_jit() {
        return;
    }
    let t = Temp::new("rt");
    let app = probe_graph(&t, "vsys = [\"sys/probe.vsys\"]");
    t.write(
        "app/src/main.ves",
        "fn wrap_phase(phase: f64, period: f64) -> f64 eff [Realtime] {\n    phase % period\n}\n\n\
         pub fn main() -> u64 {\n    let p = wrap_phase(7.5, 2.0);\n    if p > 1.0 { 1 } else { 0 }\n}\n",
    );
    let out = vesper()
        .arg("build")
        .arg(&app)
        .arg("-o")
        .arg(t.0.join("rt.bin"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("rt-unknown-callee"), "{err}");
}

/// A `link = "static"` library links its archive even when a shared library
/// of the same name sits beside it (Linux: the linker would pick the `.so`).
#[test]
fn static_libraries_link_their_archive() {
    if !cfg!(target_os = "linux") || !host_jit() {
        return;
    }
    let t = Temp::new("static");
    let app = probe_graph(&t, "vsys = [\"sys/probe.vsys\"]");
    t.write(
        "c/probe.c",
        "#include <stdbool.h>\nbool probe_not(bool b) { return !b; }\n",
    );
    t.write(
        "c/shared.c",
        "#include <stdbool.h>\nbool probe_not(bool b) { return b; }\n",
    );
    let c = t.0.join("c");
    run(Command::new("cc")
        .current_dir(&c)
        .args(["-c", "probe.c", "-o", "probe.o"]));
    run(Command::new("ar")
        .current_dir(&c)
        .args(["rcs", "libprobe.a", "probe.o"]));
    run(Command::new("cc").current_dir(&c).args([
        "-shared",
        "-fPIC",
        "shared.c",
        "-o",
        "libprobe.so",
    ]));
    let exe = t.0.join("app.bin");
    run(vesper()
        .arg("build")
        .arg(&app)
        .arg("-o")
        .arg(&exe)
        .arg("--link-search")
        .arg(&c));
    // The archive's `probe_not` negates, so main returns 7; the shared one
    // would give 9 (or fail to load).
    let status = Command::new(&exe).status().unwrap();
    assert_eq!(status.code(), Some(7));
}

/// A C string goes to libc directly: `access(2)` of a missing path, linked
/// into an executable, fails with `ENOENT`.
#[test]
fn an_executable_passes_a_path_to_libc() {
    if cfg!(windows) || !host_jit() {
        return;
    }
    let t = Temp::new("libc");
    t.write(
        "app/Loom.toml",
        "[package]\nname = \"@t/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[LibC]\"]\nvsys = [\"sys/libc.vsys\"]\n",
    );
    t.write(
        "app/sys/libc.vsys",
        "sys library LibC (target = \"@TARGET@\", abi = \"@ABI@\", link = \"dynamic\", name = \"c\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n    \
         fn access(nocapture in path: CStr, mode: i32) -> ForeignVal[i32] errno;\n}\n",
    );
    t.write(
        "app/src/main.ves",
        "pub fn main() -> u64 eff [Ffi[LibC]] {\n    let path = \"vesper-no-such-dir/x\".as_bytes();\n    \
         match LibC::access(in path, 0) {\n        Result::Ok(pair) => {\n            let (r, code) = pair;\n            \
         if r == -1 { widen_u64(code) } else { 100 }\n        }\n        Result::Err(_) => 101,\n    }\n}\n",
    );
    let exe = t.0.join("libc.bin");
    run(vesper()
        .arg("build")
        .arg(t.0.join("app"))
        .arg("-o")
        .arg(&exe));
    let status = Command::new(&exe).current_dir(&t.0).status().unwrap();
    assert_eq!(status.code(), Some(2));
}

const HANDLE_LIBRARY: &str = "    affine extern type Cell;\n    \
     fn probe_make(v: u64) -> ForeignVal[NonNull[Cell]];\n    \
     fn probe_get(in c: Cell) -> ForeignVal[u64];";

/// A library that breaks the promise and returns NULL traps the program.
#[test]
fn a_null_non_null_handle_traps() {
    if cfg!(windows) || !host_jit() {
        return;
    }
    let t = Temp::new("nonnull");
    t.write(
        "app/Loom.toml",
        "[package]\nname = \"@t/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[Probe]\"]\nvsys = [\"sys/probe.vsys\"]\n",
    );
    t.write(
        "app/sys/probe.vsys",
        &format!(
            "sys library Probe (target = \"@TARGET@\", abi = \"@ABI@\", link = \"static\", name = \"probe\", \
             artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {{\n{HANDLE_LIBRARY}\n}}\n"
        ),
    );
    t.write(
        "app/src/main.ves",
        "fn get(v: u64) -> u64 eff [Ffi[Probe]] {\n    let c = Probe::probe_make(v);\n    Probe::probe_get(in c)\n}\n\n\
         pub fn main() -> u64 eff [Ffi[Probe]] {\n    let a = get(5);\n    let b = get(0);\n    wrapping_add(a, b)\n}\n",
    );
    t.write(
        "c/probe.c",
        "#include <stdint.h>\nvoid *probe_make(uint64_t v) { return (void *)(uintptr_t)v; }\n\
         uint64_t probe_get(void *h) { return (uint64_t)(uintptr_t)h; }\n",
    );
    let c = t.0.join("c");
    run(Command::new("cc")
        .current_dir(&c)
        .args(["-c", "probe.c", "-o", "probe.o"]));
    run(Command::new("ar")
        .current_dir(&c)
        .args(["rcs", "libprobe.a", "probe.o"]));
    let exe = t.0.join("nonnull.bin");
    run(vesper()
        .arg("build")
        .arg(t.0.join("app"))
        .arg("-o")
        .arg(&exe)
        .arg("--link-search")
        .arg(&c));
    let status = Command::new(&exe).status().unwrap();
    assert!(
        !status.success() && status.code().is_none_or(|c| c != 5),
        "{status:?}"
    );
}
