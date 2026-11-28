//! `.vsys` types imported through packages (self-hosting SH5 lane 3,
//! #1326).
//!
//! A package that names a `.vsys` library in its manifest (`[package]
//! vsys`) carries it to every package that depends on it (SH1 lane 7). The
//! library's functions are named `Lib::f(..)` with no import. Its types
//! (extern types, wire structs, enums) keep their global names too, and a
//! `use` of the carrying package's root may now name them:
//! `use @vesper/cranelift::{Builder};` used to fail with E0111. Every
//! importer names the same nominal type, through any number of
//! re-exports, aliases or qualified paths.
//!
//! The repros come from the SH4 reports (lane 1, `self-host-codegen-swap.md`
//! gap 1; lane 9, `self-host-codegen-seq.md` gap 5; lane 11,
//! `self-host-codegen-numeric.md` gap 4).
mod support;
use std::{fs, path::PathBuf, process::Command};

/// A scratch directory, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-vsys-package-imports-{tag}-{}",
            std::process::id()
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

const PROBE_VSYS: &str = "sys library Probe (target = \"@TARGET@\", abi = \"@ABI@\", link = \"static\", name = \"probe\", \
     artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {
    linear extern type Counter;
    enum Step: u32 { One = 1, Ten = 10 }
    fn probe_open(start: u64) -> ForeignVal[Option[Counter]];
    fn probe_bump(inout c: Counter, step: Step) -> ForeignVal[u64];
    fn probe_close(sink c: Counter) -> ForeignVal[u64];
}
";

fn manifest(name: &str, deps: &[(&str, &str)], vsys: bool) -> String {
    let mut out = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Ffi[Probe]\"]\n"
    );
    if vsys {
        out.push_str("vsys = [\"sys/probe.vsys\"]\n");
    }
    if !deps.is_empty() {
        out.push_str("\n[dependencies]\n");
        for (dep, path) in deps {
            out.push_str(&format!(
                "\"{dep}\" = {{ version = \"^0.1\", path = \"{path}\" }}\n"
            ));
        }
    }
    out
}

/// `@t/counter` carries `Probe`; its functions take and give `Counter`
/// handles without naming the library in a `use` (the type is global).
const COUNTER_LIB: &str = "\
pub fn open(start: u64) -> Option[Counter] eff [Ffi[Probe]] {
    Probe::probe_open(start)
}

pub fn bump(inout c: Counter, step: Step) -> u64 eff [Ffi[Probe]] {
    Probe::probe_bump(inout c, step)
}

pub fn close(sink c: Counter) -> u64 eff [Ffi[Probe]] {
    Probe::probe_close(c)
}
";

/// `@t/app` imports `Counter` and `Step` from `@t/counter` (the SH4 repro:
/// `use @vesper/cranelift::{Builder};`) and writes its own functions over
/// them.
const APP_MAIN: &str = "\
use @t/counter::{Counter, Step, open, bump, close};

fn twice(inout c: Counter) -> u64 eff [Ffi[Probe]] {
    bump(inout c, Step::One);
    bump(inout c, Step::Ten)
}

pub fn main() -> u64 eff [Ffi[Probe]] {
    match open(30) {
        Option::Some(c) => {
            let mut c = c;
            twice(inout c);
            close(c)
        },
        Option::None => 0,
    }
}
";

/// The graph `@t/app` -> `@t/counter`; `vsys` says whether `@t/counter`'s
/// manifest names its library.
fn graph(t: &Temp, vsys: bool, main: &str) -> PathBuf {
    t.write("counter/Loom.toml", &manifest("@t/counter", &[], vsys));
    t.write("counter/sys/probe.vsys", PROBE_VSYS);
    t.write("counter/src/lib.ves", COUNTER_LIB);
    t.write(
        "app/Loom.toml",
        &manifest("@t/app", &[("@t/counter", "../counter")], false),
    );
    t.write("app/src/main.ves", main);
    t.0.join("app")
}

fn host_jit() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    ))
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

/// `vesper check` of the executable, and `vesper build` linking it with a
/// C `libprobe.a` (Unix hosts with `cc` and `ar`; Windows executables with
/// imports are not linked yet, SH1 lane 7 gap 2).
#[test]
fn cli_checks_and_builds_an_executable_that_imports_a_type() {
    let t = Temp::new("cli");
    let app = graph(&t, true, APP_MAIN);
    let checked = run(vesper().arg("check").arg(&app));
    assert!(checked.contains("checked @t/app"), "{checked}");
    if cfg!(windows) || !host_jit() {
        return;
    }
    t.write(
        "c/probe.c",
        "#include <stdint.h>\n#include <stdlib.h>\n\
         uint64_t *probe_open(uint64_t s) { uint64_t *c = malloc(8); *c = s; return c; }\n\
         uint64_t probe_bump(uint64_t *c, uint32_t step) { *c += step; return *c; }\n\
         uint64_t probe_close(uint64_t *c) { uint64_t v = *c; free(c); return v; }\n",
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
    assert_eq!(status.code(), Some(41));
}

/// `vesper build --lib` of a library that imports the type: its exported
/// function keeps handles inside (the export ABI refuses them in
/// signatures), and the binding links the carried library by name.
#[test]
fn a_library_build_imports_a_type() {
    let t = Temp::new("lib");
    let app = graph(&t, true, "");
    fs::remove_file(app.join("src/main.ves")).unwrap();
    t.write(
        "app/src/lib.ves",
        "// @export vsys Probe rust=crate::probe\n\
         use @t/counter::{Counter, Step, open, bump, close};\n\n\
         fn ten(inout c: Counter) -> u64 eff [Ffi[Probe]] {\n    bump(inout c, Step::Ten)\n}\n\n\
         pub fn go(start: u64) -> u64 eff [Ffi[Probe]] {\n    \
         match open(start) {\n        Option::Some(c) => {\n            let mut c = c;\n            \
         ten(inout c);\n            close(c)\n        },\n        Option::None => 0,\n    }\n}\n",
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
    assert!(text.contains("crate::probe::probe_open"), "{text}");
    assert!(text.contains("pub fn go"), "{text}");
}
