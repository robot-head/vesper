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
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use vesper::ffi_lowering;

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

/// A heap counter behind the `Counter` handle word.
extern "C" fn probe_open(start: u64) -> *mut u64 {
    Box::into_raw(Box::new(start))
}
extern "C" fn probe_bump(c: *mut u64, step: u32) -> u64 {
    // SAFETY: `c` came from probe_open and is not yet closed.
    unsafe {
        *c += u64::from(step);
        *c
    }
}
extern "C" fn probe_close(c: *mut u64) -> u64 {
    // SAFETY: the sink import consumes the handle exactly once.
    *unsafe { Box::from_raw(c) }
}

fn symbols() -> [(&'static str, *const u8); 3] {
    [
        ("probe_open", probe_open as *const u8),
        ("probe_bump", probe_bump as *const u8),
        ("probe_close", probe_close as *const u8),
    ]
}

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

/// Check the package at `dir` with its carried imports, and run `main` in
/// the JIT with the optimizer on and off.
fn jit_main(dir: &Path) -> Vec<String> {
    let p = vesper::packages::load(dir).unwrap();
    assert!(p.ok(), "{:?} {:?}", p.diagnostics, p.manifest_errors);
    let imports = p.imports(None).unwrap();
    let checked = ffi_lowering::check_module(&p.module, &imports).unwrap();
    [true, false]
        .into_iter()
        .map(|optimize| {
            checked
                .jit_with_options("main", &symbols(), optimize)
                .unwrap()
                .to_string()
        })
        .collect()
}

fn messages(dir: &Path) -> Vec<String> {
    let p = vesper::packages::load(dir).unwrap();
    p.diagnostics
        .iter()
        .map(|d| format!("{} {}", d.diagnostic.code, d.diagnostic.message))
        .collect()
}

fn host_jit() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    ))
}

#[test]
fn a_dependency_s_extern_type_is_importable() {
    let t = Temp::new("use");
    let app = graph(&t, true, APP_MAIN);
    assert_eq!(messages(&app), Vec::<String>::new());
    if host_jit() {
        assert_eq!(jit_main(&app), ["41", "41"]);
    }
}

/// Without the manifest field `@t/counter` carries no library, and the
/// import is the old E0111 (with the hint about libraries).
#[test]
fn only_a_package_that_carries_the_library_exports_its_types() {
    let t = Temp::new("nocarry");
    let app = graph(&t, false, APP_MAIN);
    let m = messages(&app);
    assert!(
        m.iter()
            .any(|m| m == "E0111 module @t/counter has no item 'Counter'"),
        "{m:?}"
    );
    // A name the library does not declare is still unknown.
    let t = Temp::new("unknown");
    let app = graph(
        &t,
        true,
        "use @t/counter::{Countr};\n\npub fn main() -> u64 {\n    0\n}\n",
    );
    let m = messages(&app);
    assert!(
        m.iter()
            .any(|m| m == "E0111 module @t/counter has no item 'Countr'"),
        "{m:?}"
    );
    // The library's types live at the package root, not in its modules.
    let t = Temp::new("submodule");
    let app = graph(
        &t,
        true,
        "use @t/counter::util::{Counter};\n\npub fn main() -> u64 {\n    0\n}\n",
    );
    t.write("counter/src/util.ves", "pub fn one() -> u64 {\n    1\n}\n");
    let m = messages(&app);
    assert!(
        m.iter()
            .any(|m| m == "E0111 module @t/counter::util has no item 'Counter'"),
        "{m:?}"
    );
}

/// An alias, a qualified path and a re-export through a middle package all
/// name the one nominal type: a handle `@t/mid` opens is bumped by `@t/app`
/// and closed by `@t/counter`.
#[test]
fn every_importer_names_the_same_type() {
    let t = Temp::new("same");
    let app = graph(&t, true, "");
    t.write(
        "mid/Loom.toml",
        &manifest("@t/mid", &[("@t/counter", "../counter")], false),
    );
    t.write(
        "mid/src/lib.ves",
        "pub use @t/counter::{Counter};\nuse @t/counter::{Counter as Handle, open};\n\n\
         pub fn start() -> Option[Handle] eff [Ffi[Probe]] {\n    open(5)\n}\n",
    );
    t.write(
        "app/Loom.toml",
        &manifest(
            "@t/app",
            &[("@t/counter", "../counter"), ("@t/mid", "../mid")],
            false,
        ),
    );
    t.write(
        "app/src/main.ves",
        "use @t/mid::{Counter, start};\nuse @t/counter::{bump, close};\n\n\
         fn ten(inout c: @t/counter::Counter) -> u64 eff [Ffi[Probe]] {\n    \
         bump(inout c, @t/counter::Step::Ten)\n}\n\n\
         fn both(inout c: Counter) -> u64 eff [Ffi[Probe]] {\n    ten(inout c)\n}\n\n\
         pub fn main() -> u64 eff [Ffi[Probe]] {\n    \
         match start() {\n        Option::Some(c) => {\n            let mut c = c;\n            \
         both(inout c);\n            close(c)\n        },\n        Option::None => 0,\n    }\n}\n",
    );
    assert_eq!(messages(&app), Vec::<String>::new());
    if host_jit() {
        assert_eq!(jit_main(&app), ["15", "15"]);
    }
}

/// The importer may not declare the name it imports, as for any item.
#[test]
fn an_imported_type_collides_with_a_declared_one() {
    let t = Temp::new("clash");
    let app = graph(
        &t,
        true,
        "use @t/counter::{Counter};\n\nstruct Counter {\n    n: u64,\n}\n\npub fn main() -> u64 {\n    0\n}\n",
    );
    let m = messages(&app);
    assert!(
        m.iter()
            .any(|m| m.contains("'Counter' is both declared in this module and imported")),
        "{m:?}"
    );
}

/// A root module may re-export a library type with `pub use`; it is not one
/// of the package's exported items, so the interface does not change.
#[test]
fn a_root_reexport_is_not_an_exported_item() {
    let t = Temp::new("export");
    let app = graph(&t, true, "");
    fs::remove_file(app.join("src/main.ves")).unwrap();
    t.write(
        "app/src/lib.ves",
        "pub use @t/counter::{Counter};\n\npub fn one() -> u64 {\n    1\n}\n",
    );
    let p = vesper::packages::load(&app).unwrap();
    assert!(p.ok(), "{:?}", p.diagnostics);
    assert!(p.module.public_items.iter().any(|n| n == "one"));
    assert!(
        !p.module.public_items.iter().any(|n| n == "Counter"),
        "{:?}",
        p.module.public_items
    );
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

fn repo() -> PathBuf {
    fs::canonicalize(".").unwrap()
}

/// The `.vsys` text of `@vesper/cranelift`'s `// @export vsys` directive
/// (the lines after `// | `).
fn cranelift_vsys(lib: &str) -> String {
    lib.lines()
        .take_while(|l| l.starts_with("//"))
        .filter_map(|l| l.strip_prefix("// | ").or_else(|| l.strip_prefix("// |")))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// The real `@vesper/cranelift` source as a dependency: `use
/// @vesper/cranelift::{Builder, Module, ..}` (SH4 lane 1, gap 1). When the
/// package's manifest does not name its `.vsys` yet, a copy of the package
/// with the field added stands in for it; the source is not edited.
#[test]
fn a_package_imports_cranelift_handles() {
    let t = Temp::new("cranelift");
    let real = repo().join("packages/vesper-cranelift");
    let manifest_text = fs::read_to_string(real.join("Loom.toml")).unwrap();
    let cranelift = if manifest_text
        .lines()
        .any(|l| l.trim_start().starts_with("vsys"))
    {
        real
    } else {
        let lib = fs::read_to_string(real.join("src/lib.ves")).unwrap();
        t.write("cranelift/src/lib.ves", &lib);
        t.write("cranelift/cranelift.vsys", &cranelift_vsys(&lib));
        t.write(
            "cranelift/Loom.toml",
            &format!("{manifest_text}vsys = [\"cranelift.vsys\"]\n"),
        );
        t.0.join("cranelift")
    };
    t.write(
        "user/Loom.toml",
        &format!(
            "[package]\nname = \"@t/user\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\
             capability_ceiling = [\"Fail\", \"Ffi\"]\n\n[dependencies]\n\
             \"@vesper/cranelift\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
            cranelift.to_string_lossy().replace('\\', "/")
        ),
    );
    t.write(
        "user/src/lib.ves",
        "use @vesper/cranelift::{Builder, Data, Module, Object};\n\n\
         pub fn pointer_type(in m: Module) -> u32 eff [Ffi[Cranelift]] {\n    \
         Cranelift::vcl_module_pointer_type(in m)\n}\n\n\
         pub fn drop_builder(sink b: Builder) eff [Ffi[Cranelift]] {\n    Cranelift::vcl_builder_free(b);\n}\n\n\
         pub fn drop_data(sink d: Data) eff [Ffi[Cranelift]] {\n    Cranelift::vcl_data_free(d);\n}\n\n\
         pub fn drop_object(sink o: Object) eff [Ffi[Cranelift]] {\n    Cranelift::vcl_object_free(o);\n}\n",
    );
    let user = t.0.join("user");
    let p = vesper::packages::load(&user).unwrap();
    assert!(p.ok(), "{:?} {:?}", p.diagnostics, p.manifest_errors);
    let imports = p.imports(None).unwrap();
    if let Err(e) = ffi_lowering::check_module(&p.module, &imports) {
        panic!("{e:?}");
    }
}

/// An SH4 package over `@vesper/cranelift` checks as a real dependency,
/// unedited: `@vesper/codegen-base` (SH4 lane 1, #1309), once it is on
/// `main`. Until then there is nothing to check.
#[test]
fn codegen_base_checks_over_its_cranelift_dependency() {
    let dir = repo().join("packages/vesper-codegen-base");
    if !dir.join("Loom.toml").is_file() {
        return;
    }
    let p = vesper::packages::load(&dir).unwrap();
    assert!(p.ok(), "{:?} {:?}", p.diagnostics, p.manifest_errors);
    let imports = p.imports(None).unwrap();
    if let Err(e) = ffi_lowering::check_module(&p.module, &imports) {
        panic!("{e:?}");
    }
}
