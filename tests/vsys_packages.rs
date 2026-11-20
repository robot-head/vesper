//! `.vsys` imports carried by packages, `bool` and C strings (self-hosting
//! SH1 lane 7, #1150).
//!
//! * A package names its `.vsys` files in its manifest (`[package] vsys`);
//!   `packages::load` reads them for every package in the graph, so a
//!   dependency carries its own imports to libraries and executables.
//! * `bool` parameters and results cross as one byte (this already worked
//!   on main; the test keeps it working).
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
fn cli_checks_and_jits_an_executable_over_std_fs() {
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
    // The evaluator has no host yet: a clear refusal.
    let out = vesper().arg("run").arg(&tool).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("use `vesper jit`"));
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
