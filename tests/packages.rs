//! Port wave P0 lane 1 (F0): multi-file packages, path dependencies and
//! `use` imports. Fixtures live in tests/fixtures/packages/.
use std::{path::Path, process::Command};
use vesper::{check, native, packages, runtime::Runtime};

const APP: &str = "tests/fixtures/packages/workspace/app";
const GEOMETRY: &str = "tests/fixtures/packages/workspace/geometry";
const REJECT: &str = "tests/fixtures/packages/reject";
/// `total(7, 9 + 10) + 50 + 0 + 7 + count_twice()` (4).
const EXPECTED: i64 = 780;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}

fn world(dir: &str) -> check::Checked {
    let assembled = packages::load(Path::new(dir)).unwrap();
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    assert!(assembled.module.uses.is_empty());
    let checked = check::check(&assembled.module);
    let localized: Vec<_> = checked
        .diagnostics
        .iter()
        .map(|d| assembled.sources.localize(d))
        .collect();
    assert!(checked.diagnostics.is_empty(), "{localized:?}");
    checked
}

#[test]
fn two_packages_agree_in_the_evaluator_and_natively() {
    let checked = world(APP);
    let interpreted = Runtime::new(&checked.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    assert_eq!(interpreted, EXPECTED.to_string());
    let jit = native::jit(&checked.world, "main").unwrap().to_string();
    assert_eq!(jit, interpreted);
    let optimized = native::jit_with_options(&checked.world, "main", true)
        .unwrap()
        .to_string();
    assert_eq!(optimized, interpreted);
    // Both native targets the repository builds for.
    let (arm, _) =
        native::object_for_target(&checked.world, "main", Some("aarch64-unknown-linux-gnu"))
            .unwrap();
    assert_eq!(&arm[..4], b"\x7fELF");
    let (x86, _) =
        native::object_for_target(&checked.world, "main", Some("x86_64-unknown-linux-gnu"))
            .unwrap();
    assert_eq!(&x86[..4], b"\x7fELF");
}

#[test]
fn items_get_qualified_names_and_the_root_module_keeps_its_own() {
    let checked = world(APP);
    let functions = &checked.world.functions;
    for name in [
        "main",
        "scale",
        "origin",
        "demo__app__report__total",
        "demo__geometry__point",
        "demo__geometry__scale",
        "demo__geometry__util__abs",
        "demo__geometry__util__helper",
        "demo__geometry__shapes__area__area",
        "demo__geometry__Point::sum",
    ] {
        assert!(
            functions.contains_key(name),
            "{name}: {:?}",
            functions.keys()
        );
    }
    // Two private `scale` functions coexist.
    assert_eq!(
        Runtime::new(&checked.world)
            .run("demo__geometry__scale", vec![])
            .unwrap()
            .to_string(),
        "10"
    );
    assert!(checked.world.adts.contains_key("demo__geometry__Point"));
    assert!(
        checked
            .world
            .adts
            .contains_key("demo__geometry__shapes__area__Shape")
    );
}

#[test]
fn a_library_package_checks_on_its_own() {
    let checked = world(GEOMETRY);
    assert!(checked.world.functions.contains_key("manhattan"));
    assert!(
        checked
            .world
            .functions
            .contains_key("demo__geometry__util__abs")
    );
}

#[test]
fn lean_extraction_sees_the_assembled_world() {
    let checked = world(APP);
    let lean = vesper::lean::extract(&checked.world).unwrap();
    assert!(lean.contains("demo__geometry__util__abs"), "{lean}");
}

#[test]
fn cli_check_run_and_jit_accept_a_package_directory() {
    let o = cli().args(["check", APP]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    for command in ["run", "jit"] {
        let o = cli().args([command, APP]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).trim(),
            EXPECTED.to_string()
        );
    }
}

#[test]
#[cfg_attr(
    target_os = "windows",
    ignore = "vesper build uses a Unix cc linker; MSVC executable linking is not implemented"
)]
fn cli_build_links_a_package_executable() {
    let exe = std::env::temp_dir().join(format!("vesper-package-{}", std::process::id()));
    let o = cli()
        .arg("build")
        .arg(APP)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let path = exe;
    let status = Command::new(&path).status().unwrap();
    // Exit statuses are truncated to a byte outside Windows.
    assert_eq!(
        status.code().map(|c| c % 256),
        Some((EXPECTED % 256) as i32)
    );
    let _ = std::fs::remove_file(path);
}

/// `vesper check --json <dir>`: the codes and files reported.
fn rejected(dir: &str) -> Vec<(String, String, String)> {
    let o = cli()
        .args(["check", "--json", &format!("{REJECT}/{dir}")])
        .output()
        .unwrap();
    assert!(!o.status.success(), "{dir} unexpectedly checked");
    let stdout = String::from_utf8_lossy(&o.stdout);
    let line = stdout.lines().next().unwrap_or("[]");
    let value: serde_json::Value = serde_json::from_str(line).unwrap();
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["code"].as_str().unwrap().to_owned(),
                d["file"].as_str().unwrap_or("").to_owned(),
                d["message"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn assert_rejects(dir: &str, code: &str, file: &str, message: &str) {
    let found = rejected(dir);
    assert!(
        found
            .iter()
            .any(|(c, f, m)| c == code && f.ends_with(file) && m.contains(message)),
        "{dir}: expected {code} in {file} containing {message:?}, got {found:?}"
    );
}

#[test]
fn every_import_diagnostic_has_a_reject_fixture() {
    assert_rejects(
        "unknown_item",
        packages::UNKNOWN_IMPORT,
        "unknown_item/src/main.ves",
        "has no item 'distance'",
    );
    assert_rejects(
        "unknown_module",
        packages::UNKNOWN_IMPORT,
        "unknown_module/src/main.ves",
        "no module @demo/geometry::polygons",
    );
    assert_rejects(
        "undeclared_dependency",
        packages::UNKNOWN_IMPORT,
        "undeclared_dependency/src/main.ves",
        "is not a dependency of @reject/undeclared",
    );
    assert_rejects(
        "unimported_name",
        packages::UNKNOWN_IMPORT,
        "unimported_name/src/math.ves",
        "'limit' is not declared or imported",
    );
    assert_rejects(
        "private_item",
        packages::PRIVATE_ITEM,
        "private_item/src/main.ves",
        "'helper' is private to module @demo/geometry::util",
    );
    assert_rejects(
        "ambiguous_import",
        packages::AMBIGUOUS,
        "ambiguous_import/src/main.ves",
        "'point' is imported twice",
    );
    assert_rejects(
        "ambiguous_local",
        packages::AMBIGUOUS,
        "ambiguous_local/src/main.ves",
        "'point' is both declared in this module and imported",
    );
    assert_rejects(
        "ambiguous_variant",
        packages::AMBIGUOUS,
        "src/shapes/area.ves",
        "'Square' is also declared in",
    );
    assert_rejects(
        "private_variant",
        packages::UNKNOWN_IMPORT,
        "private_variant/src/main.ves",
        "variant 'Hidden' belongs to enum @reject/private-variant::secret::Secret",
    );
    assert_rejects(
        "cycle/a",
        packages::CYCLE,
        "Loom.toml",
        "package dependency cycle: @reject/cycle-a -> @reject/cycle-b -> @reject/cycle-a",
    );
    assert_rejects(
        "registry_dependency",
        packages::INVALID_PACKAGE,
        "registry_dependency/Loom.toml",
        "registry dependencies are not supported yet",
    );
    assert_rejects(
        "version_mismatch",
        packages::INVALID_PACKAGE,
        "version_mismatch/Loom.toml",
        "requires version ^2.0",
    );
}

#[test]
fn traits_models_and_bounds_work_across_modules() {
    // `best(square(3), square(4))` (16) * 1000 + side 5 * 100 + 9 * 10 + 1
    // (`use model Descending` picks the smaller element).
    let checked = world("tests/fixtures/packages/accept/traits_across_modules");
    let interpreted = Runtime::new(&checked.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    assert_eq!(interpreted, "16591");
    let jit = native::jit(&checked.world, "main").unwrap().to_string();
    assert_eq!(jit, interpreted);
    assert_rejects(
        "ambiguous_trait",
        packages::AMBIGUOUS,
        "ambiguous_trait/src/other.ves",
        "'Score' is also declared in",
    );
}

#[test]
fn optional_dependencies_are_not_loaded() {
    let o = cli()
        .args(["run", "tests/fixtures/packages/accept/optional_dependency"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "7");
}

#[test]
fn use_in_a_single_file_needs_a_package() {
    let o = cli()
        .args([
            "check",
            "--json",
            "tests/fixtures/packages/use_outside_package.ves",
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.contains(packages::USE_OUTSIDE_PACKAGE),
        "{stdout} {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn semantic_errors_point_at_the_right_file_with_qualified_names() {
    let dir = std::env::temp_dir().join(format!("vesper-package-error-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/deep")).unwrap();
    std::fs::write(
        dir.join("Loom.toml"),
        "[package]\nname = \"@demo/errors\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.ves"),
        "use self::deep::thing::value;\n\nfn main() -> i64 {\n    value()\n}\n",
    )
    .unwrap();
    // A type error in a nested module: `true` is not an i64.
    std::fs::write(
        dir.join("src/deep/thing.ves"),
        "pub fn value() -> i64 {\n    true\n}\n",
    )
    .unwrap();
    let o = cli().arg("check").arg("--json").arg(&dir).output().unwrap();
    assert!(!o.status.success());
    let stdout = String::from_utf8_lossy(&o.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
    let first = &value[0];
    assert!(
        first["file"]
            .as_str()
            .unwrap()
            .ends_with("src/deep/thing.ves"),
        "{stdout}"
    );
    let span = first["span"]["start"].as_u64().unwrap() as usize;
    let text = std::fs::read_to_string(dir.join("src/deep/thing.ves")).unwrap();
    assert!(span <= text.len(), "{stdout}");
    assert!(
        !first["message"]
            .as_str()
            .unwrap()
            .contains("demo__errors__"),
        "{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
