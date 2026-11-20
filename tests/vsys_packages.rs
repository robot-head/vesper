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
use std::{fs, path::PathBuf, process::Command};
use vesper::ffi_lowering::{self, Handler, Imports};

fn target() -> (&'static str, &'static str) {
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

fn library(body: &str) -> String {
    let (t, a) = target();
    format!(
        "sys library Probe (target = \"{t}\", abi = \"{a}\", link = \"static\", name = \"probe\", \
         artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {{\n{body}\n}}\n"
    )
}

fn imports(body: &str) -> Result<Imports, String> {
    let mut i = Imports::new();
    i.add(&library(body), Handler::Direct)
        .map_err(|e| e.to_string())?;
    Ok(i)
}

extern "C" fn probe_not(b: bool) -> bool {
    !b
}

#[test]
fn bool_parameters_and_results() {
    let imports = imports("    fn probe_not(b: bool) -> ForeignVal[bool];").unwrap();
    let src = "pub fn main() -> u64 eff [Ffi[Probe]] {\n    match Probe::probe_not(true) {\n        Result::Ok(b) => if b { 1 } else { 2 },\n        Result::Err(_) => 3,\n    }\n}\n";
    let checked = ffi_lowering::check_source(src, &imports).unwrap();
    let v = checked
        .jit("main", &[("probe_not", probe_not as *const u8)])
        .unwrap();
    assert_eq!(v.to_string(), "2");
}

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

#[test]
fn a_dependency_carries_its_vsys_import() {
    let t = Temp::new("dep");
    let app = probe_graph(&t, "vsys = [\"sys/probe.vsys\"]");
    let p = vesper::packages::load(&app).unwrap();
    assert!(p.ok(), "{:?} {:?}", p.diagnostics, p.manifest_errors);
    assert_eq!(p.vsys.len(), 1);
    assert_eq!(p.vsys[0].package, "@t/flip");
    assert!(
        p.vsys[0].display.ends_with("flip/sys/probe.vsys"),
        "{}",
        p.vsys[0].display
    );
    let imports = p.imports(None).unwrap();
    let checked = ffi_lowering::check_module(&p.module, &imports).unwrap();
    for optimize in [true, false] {
        let v = checked
            .jit_with_options("main", &[("probe_not", probe_not as *const u8)], optimize)
            .unwrap();
        assert_eq!(v.to_string(), "7");
    }
    // Without the manifest field the dependency's calls do not check.
    let t = Temp::new("nodep");
    let app = probe_graph(&t, "");
    let p = vesper::packages::load(&app).unwrap();
    assert!(p.ok());
    assert!(p.vsys.is_empty());
    let imports = p.imports(None).unwrap();
    assert!(ffi_lowering::check_module(&p.module, &imports).is_err());
}

#[test]
fn manifest_vsys_paths_are_checked() {
    for (field, why) in [
        ("vsys = [\"sys/missing.vsys\"]", "no such file"),
        ("vsys = [\"../flip/sys/probe.vsys\"]", "no `..`"),
        ("vsys = [\"/etc/probe.vsys\"]", "relative path"),
        ("vsys = [\"sys/probe.txt\"]", "expected a .vsys file"),
        (
            "vsys = [\"sys/probe.vsys\", \"./sys/probe.vsys\"]",
            "named twice",
        ),
    ] {
        let t = Temp::new("bad");
        let app = probe_graph(&t, field);
        let p = vesper::packages::load(&app).unwrap();
        let messages: Vec<_> = p
            .manifest_errors
            .iter()
            .map(|(_, d)| d.message.clone())
            .collect();
        assert!(
            messages.iter().any(|m| m.contains(why)),
            "{field}: {messages:?}"
        );
    }
}

fn vesper() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}

fn repo() -> PathBuf {
    fs::canonicalize(".").unwrap()
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
fn jit_refuses_a_library_it_does_not_host() {
    let t = Temp::new("nohost");
    let app = probe_graph(&t, "vsys = [\"sys/probe.vsys\"]");
    let out = vesper().arg("jit").arg(&app).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("does not host"), "{err}");
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

// ---------------------------------------------------------------------------
// Part 2: C strings, OS error codes and non-NULL handle results.

static PROBE_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `len * 1000 + sum of bytes` of a NUL-terminated string.
extern "C" fn probe_len(s: *const std::ffi::c_char, scale: u32) -> u64 {
    PROBE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // SAFETY: the lowering passes a NUL-terminated buffer it owns.
    let bytes = unsafe { std::ffi::CStr::from_ptr(s) }.to_bytes();
    let sum: u64 = bytes.iter().map(|&b| u64::from(b)).sum();
    (bytes.len() as u64 * 1000 + sum) * u64::from(scale)
}

/// Looks `path` up and returns the OS error code it left (0 when found).
extern "C" fn probe_stat(path: *const std::ffi::c_char) -> u32 {
    // SAFETY: as above.
    let path = unsafe { std::ffi::CStr::from_ptr(path) }.to_str().unwrap();
    match fs::metadata(path) {
        Ok(_) => 0,
        Err(e) => e.raw_os_error().unwrap_or(0) as u32,
    }
}

const CSTR_LIBRARY: &str = "    fn probe_len(nocapture in s: CStr, scale: u32) -> ForeignVal[u64];\n    \
     fn probe_stat(nocapture in path: CStr) -> ForeignVal[u32] errno;";

fn cstr_symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("probe_len", probe_len as *const u8),
        ("probe_stat", probe_stat as *const u8),
    ]
}

/// `nocapture in s: CStr` takes a `Slice[u8]`: C receives a terminated
/// copy, and an argument holding a NUL byte is refused without a call.
#[test]
fn cstr_arguments_are_terminated_copies() {
    let imports = imports(CSTR_LIBRARY).unwrap();
    let src = "fn bytes(nul: bool) -> ChunkSeq[u8] {\n    let mut p: ChunkSeq[u8] = ChunkSeq::new[u8]();\n    \
               p.push(104);\n    p.push(105);\n    if nul { p.push(0); }\n    p.push(33);\n    p\n}\n\n\
               pub fn whole() -> u64 eff [Ffi[Probe]] {\n    let p = bytes(false);\n    \
               match Probe::probe_len(in p.as_slice(), 2) {\n        Result::Ok(n) => n,\n        Result::Err(e) => wrapping_add(1000000, e.raw),\n    }\n}\n\n\
               fn sub() -> u64 eff [Ffi[Probe], Fail[RuntimeError]] {\n    let p = bytes(false);\n    \
               match Probe::probe_len(in p[1..3], 1) {\n        Result::Ok(n) => n,\n        Result::Err(e) => wrapping_add(1000000, e.raw),\n    }\n}\n\n\
               pub fn part() -> u64 eff [Ffi[Probe]] {\n    handle Fail[RuntimeError] in {\n        sub()\n    } with {\n        \
               return v => v,\n        raise(_, _) => 5,\n    }\n}\n\n\
               pub fn empty() -> u64 eff [Ffi[Probe]] {\n    let p: ChunkSeq[u8] = ChunkSeq::new[u8]();\n    \
               match Probe::probe_len(in p.as_slice(), 1) {\n        Result::Ok(n) => wrapping_add(n, 7),\n        Result::Err(e) => wrapping_add(1000000, e.raw),\n    }\n}\n\n\
               pub fn refused() -> u64 eff [Ffi[Probe]] {\n    let p = bytes(true);\n    \
               match Probe::probe_len(in p.as_slice(), 1) {\n        Result::Ok(n) => n,\n        Result::Err(e) => wrapping_add(1000000, e.raw),\n    }\n}\n";
    let checked = ffi_lowering::check_source(src, &imports).unwrap_or_else(|e| panic!("{e}"));
    for optimize in [true, false] {
        let run = |entry: &str| {
            checked
                .jit_with_options(entry, &cstr_symbols(), optimize)
                .unwrap()
                .to_string()
        };
        // "hi!" = 3 bytes summing to 104 + 105 + 33 = 242.
        assert_eq!(run("whole"), "6484");
        // "i!" = 2 bytes, 138.
        assert_eq!(run("part"), "2138");
        assert_eq!(run("empty"), "7");
        let before = PROBE_CALLS.load(std::sync::atomic::Ordering::SeqCst);
        // The NUL is at offset 2; C is not called.
        assert_eq!(run("refused"), "1000002");
        assert_eq!(
            PROBE_CALLS.load(std::sync::atomic::Ordering::SeqCst),
            before
        );
    }
    // The evaluator's host marshals the same way.
    let host = ffi_lowering::EvalHost::new(&imports, &cstr_symbols()).unwrap();
    let eval = |entry: &str| {
        checked
            .evaluate(entry, |call| host.call(call))
            .unwrap()
            .to_string()
    };
    assert_eq!(eval("whole"), "6484");
    assert_eq!(eval("part"), "2138");
    assert_eq!(eval("empty"), "7");
    assert_eq!(eval("refused"), "1000002");
}

/// `-> ForeignVal[T] errno` pairs the result with the OS error code the
/// call left (`errno`, or `GetLastError()` on Windows).
#[test]
fn errno_is_read_after_the_call() {
    if !host_jit() {
        return;
    }
    let imports = imports(CSTR_LIBRARY).unwrap();
    let src = "pub fn main() -> u64 eff [Ffi[Probe]] {\n    let path = \"vesper-no-such-dir/x\".as_bytes();\n    \
               match Probe::probe_stat(in path) {\n        Result::Ok(pair) => {\n            let (seen, code) = pair;\n            \
               if seen != 0 && seen == code { widen_u64(code) } else { wrapping_add(1000, widen_u64(code)) }\n        }\n        \
               Result::Err(_) => 99999,\n    }\n}\n";
    let checked = ffi_lowering::check_source(src, &imports).unwrap_or_else(|e| panic!("{e}"));
    for optimize in [true, false] {
        let v = checked
            .jit_with_options("main", &cstr_symbols(), optimize)
            .unwrap()
            .to_string();
        let code: u64 = v.parse().unwrap();
        // ENOENT (2) on Unix; ERROR_PATH_NOT_FOUND (3) on Windows.
        assert!(code == 2 || code == 3, "{v}");
    }
    let host = ffi_lowering::EvalHost::new(&imports, &cstr_symbols()).unwrap();
    let v = checked
        .evaluate("main", |call| host.call(call))
        .unwrap()
        .to_string();
    assert!(v == "2" || v == "3", "{v}");
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

/// The opaque word of a handle the library promises is not NULL.
extern "C" fn probe_make(v: u64) -> u64 {
    v
}
extern "C" fn probe_get(h: u64) -> u64 {
    h
}

const HANDLE_LIBRARY: &str = "    affine extern type Cell;\n    \
     fn probe_make(v: u64) -> ForeignVal[NonNull[Cell]];\n    \
     fn probe_get(in c: Cell) -> ForeignVal[u64];";

/// `ForeignVal[NonNull[H]]` gives source code `H` itself, with no error arm.
#[test]
fn non_null_handle_results_have_no_error_arm() {
    let imports = imports(HANDLE_LIBRARY).unwrap();
    let src = "pub fn main() -> u64 eff [Ffi[Probe]] {\n    let c = Probe::probe_make(41);\n    \
               wrapping_add(Probe::probe_get(in c), 1)\n}\n";
    let checked = ffi_lowering::check_source(src, &imports).unwrap_or_else(|e| panic!("{e}"));
    let symbols = [
        ("probe_make", probe_make as *const u8),
        ("probe_get", probe_get as *const u8),
    ];
    for optimize in [true, false] {
        let v = checked
            .jit_with_options("main", &symbols, optimize)
            .unwrap();
        assert_eq!(v.to_string(), "42");
    }
    let host = ffi_lowering::EvalHost::new(&imports, &symbols).unwrap();
    let v = checked.evaluate("main", |call| host.call(call)).unwrap();
    assert_eq!(v.to_string(), "42");
    // Matching it as a `Result` is a type error now.
    let src = "pub fn main() -> u64 eff [Ffi[Probe]] {\n    match Probe::probe_make(41) {\n        \
               Result::Ok(c) => Probe::probe_get(in c),\n        Result::Err(_) => 0,\n    }\n}\n";
    assert!(ffi_lowering::check_source(src, &imports).is_err());
}

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

extern "C" fn probe_flip(b: bool) -> bool {
    !b
}
extern "C" fn probe_next(state: u32) -> u32 {
    state + 1
}

/// Review cases: an `errno` pair around a validated result still declares
/// `ForeignInvalid`; the evaluator passes enums as their discriminants and
/// keys functions by library; `CStr` with `sink` and isolated imports are
/// refused.
#[test]
fn errno_enum_and_refusal_edges() {
    let imports = imports(
        "    enum State: u32 { Idle = 3, Busy = 4, Done = 5 }\n    \
         fn probe_flip(b: bool) -> ForeignVal[bool] errno;\n    \
         fn probe_next(s: State) -> ForeignVal[State];",
    )
    .unwrap();
    let src = "pub fn main() -> u64 eff [Ffi[Probe]] {\n    let (r, _) = Probe::probe_flip(false);\n    \
               let a = match r {\n        Result::Ok(b) => if b { 1 } else { 2 },\n        Result::Err(_) => 3,\n    };\n    \
               let n = match Probe::probe_next(State::Idle) {\n        Result::Ok(State::Busy) => 10,\n        Result::Ok(_) => 20,\n        Result::Err(_) => 30,\n    };\n    \
               wrapping_add(a, n)\n}\n";
    let checked = ffi_lowering::check_source(src, &imports).unwrap_or_else(|e| panic!("{e}"));
    let symbols = [
        ("probe_flip", probe_flip as *const u8),
        ("probe_next", probe_next as *const u8),
    ];
    let v = checked.jit("main", &symbols).unwrap();
    assert_eq!(v.to_string(), "11");
    let host = ffi_lowering::EvalHost::new(&imports, &symbols).unwrap();
    let v = checked.evaluate("main", |call| host.call(call)).unwrap();
    assert_eq!(v.to_string(), "11");
    // A refused C string would skip a finalizer.
    let e = imports_err(
        "    linear extern type H;\n    fn probe_close(sink h: H, nocapture in why: CStr) -> ForeignVal[Unit];",
    );
    assert!(e.contains("sink"), "{e}");
    // The evaluator does not drop an isolated(mpk) grant to a direct call.
    let mut isolated = Imports::new();
    isolated
        .add(
            &library("    fn probe_flip(b: bool) -> ForeignVal[bool];"),
            Handler::IsolatedMpk,
        )
        .unwrap();
    assert!(ffi_lowering::EvalHost::new(&isolated, &symbols).is_err());
}

fn imports_err(body: &str) -> String {
    match imports(body) {
        Ok(_) => panic!("accepted"),
        Err(e) => e,
    }
}
