//! P3b gaps lane 8 (#1093): a library package exports submodule items
//! through `pub use` in its root module (Flux gap 2), names `.vsys`
//! libraries from every module (Kinetic gap 5), and reports the errors of
//! a `.vsys` package build at their own file and line.
use std::{path::PathBuf, process::Command};
use vesper::{check, packages, runtime::Runtime};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}

fn package(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vesper-submodule-exports-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

const MANIFEST: &str = "[package]\nname = \"@demo/exp\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = []\n";

const FRONTIER: &str = "pub copy struct Pair { a: u64, b: u64 }
pub fn advance(x: u64) -> u64 { wrapping_add(x, 1) }
pub fn merge(in p: Pair) -> u64 { wrapping_add(p.a, helper(p.b)) }
fn helper(x: u64) -> u64 { wrapping_mul(x, 3) }
pub fn not_exported(x: u64) -> u64 { x }
";

#[test]
fn pub_use_in_the_root_module_exports_submodule_items_under_their_root_names() {
    let dir = package(
        "exports",
        &[
            ("Loom.toml", MANIFEST),
            (
                "src/lib.ves",
                "pub use self::frontier::{advance, merge as frontier_merge, Pair};
pub fn root_fn(x: u64) -> u64 { advance(x) }
pub fn main() -> u64 { wrapping_add(root_fn(1), frontier_merge(in Pair { a: 1, b: 2 })) }
",
            ),
            ("src/frontier.ves", FRONTIER),
        ],
    );
    // The assembled module names the re-exported items by their root names,
    // so every backend and the export path see them as root items.
    let assembled = packages::load(&dir).unwrap();
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let checked = check::check(&assembled.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    for name in ["advance", "frontier_merge", "root_fn"] {
        assert!(
            checked.world.functions.contains_key(name),
            "{name}: {:?}",
            checked.world.functions.keys()
        );
    }
    assert!(
        checked
            .world
            .functions
            .contains_key("demo__exp__frontier__helper")
    );
    let value = Runtime::new(&checked.world).run("main", vec![]).unwrap();
    assert_eq!(value.to_string(), "9");

    let object = dir.join("exp.o");
    let interface = dir.join("exp.json");
    let o = cli()
        .arg("build")
        .arg("--lib")
        .arg(&dir)
        .arg("-o")
        .arg(&object)
        .arg("--interface")
        .arg(&interface)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&interface).unwrap()).unwrap();
    let mut functions: Vec<String> = json["functions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_owned())
        .collect();
    functions.sort();
    assert_eq!(functions, ["advance", "frontier_merge", "main", "root_fn"]);
    assert!(json["types"].get("Pair").is_some(), "{json}");
    let _ = std::fs::remove_dir_all(&dir);
}

const VSYS: &str = "// @export vsys direct
// | sys library Libm (
// |     target = \"@TARGET@\", abi = \"@ABI@\",
// |     link = \"static\", name = \"demo_libm\",
// |     artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\"
// | ) {
// |     fn demo_expf(bits: u32) -> ForeignVal[u32];
// | }
pub use self::fastmath::fexp;
pub fn exp_bits(b: u32) -> u32 eff [Ffi[Libm]] { self::fastmath::fexp(b) }
";

#[test]
fn a_submodule_names_a_vsys_library_without_an_import() {
    let dir = package(
        "vsys",
        &[
            ("Loom.toml", MANIFEST),
            ("src/lib.ves", VSYS),
            (
                "src/fastmath.ves",
                "pub fn fexp(b: u32) -> u32 eff [Ffi[Libm]] { Libm::demo_expf(b) }\n",
            ),
        ],
    );
    let object = dir.join("exp.o");
    let interface = dir.join("exp.json");
    let o = cli()
        .arg("build")
        .arg("--lib")
        .arg(&dir)
        .arg("-o")
        .arg(&object)
        .arg("--interface")
        .arg(&interface)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let json = std::fs::read_to_string(&interface).unwrap();
    assert!(
        json.contains("\"fexp\"") && json.contains("\"exp_bits\""),
        "{json}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_vsys_package_build_reports_errors_at_their_file_and_line() {
    let dir = package(
        "vsys-error",
        &[
            ("Loom.toml", MANIFEST),
            ("src/lib.ves", VSYS),
            (
                "src/fastmath.ves",
                "pub fn fexp(b: u32) -> u32 eff [Ffi[Libm]] {\n    let x: u64 = Libm::demo_expf(b);\n    b\n}\n",
            ),
        ],
    );
    let o = cli()
        .arg("build")
        .arg("--lib")
        .arg("--json")
        .arg(&dir)
        .arg("-o")
        .arg(dir.join("exp.o"))
        .output()
        .unwrap();
    assert!(!o.status.success());
    let stdout = String::from_utf8_lossy(&o.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap_or("[]"))
        .unwrap_or_else(|e| panic!("{e}: {stdout} {}", String::from_utf8_lossy(&o.stderr)));
    let first = &value[0];
    assert!(
        first["file"]
            .as_str()
            .unwrap()
            .ends_with("src/fastmath.ves"),
        "{stdout}"
    );
    let text = std::fs::read_to_string(dir.join("src/fastmath.ves")).unwrap();
    let start = first["span"]["start"].as_u64().unwrap() as usize;
    assert_eq!(&text[start..start + 4], "Libm", "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}
