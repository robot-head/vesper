//! Package inputs (port wave P1 lane 2, #899): the closure, the
//! rerun-if-changed inputs and edited copies of a package with a path
//! dependency.
use std::path::{Path, PathBuf};
use vesper_port_harness::{inputs, package_closure, prepare_source};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// `<tmp>/ports/app` depends on `<tmp>/libs/math` (a sibling tree, so the
/// copy must keep the relative layout).
fn fixture(name: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("vesper-port-harness-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    write(
        &root.join("ports/app/Loom.toml"),
        "[package]\nname = \"@t/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n\"@t/math\" = { version = \"^0.1\", path = \"../../libs/math\" }\n\"@t/gone\" = { version = \"^0.1\", path = \"../gone\", optional = true }\n",
    );
    write(
        &root.join("ports/app/src/lib.ves"),
        "pub fn f() -> i64 { 1 }\n",
    );
    write(
        &root.join("ports/app/src/a/b.ves"),
        "pub fn g() -> i64 { 2 }\n",
    );
    write(
        &root.join("libs/math/Loom.toml"),
        "[package]\nname = \"@t/math\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    );
    write(
        &root.join("libs/math/src/lib.ves"),
        "pub fn h() -> i64 { 2 }\n",
    );
    root
}

#[test]
fn closure_follows_path_dependencies_and_skips_missing_ones() {
    let root = fixture("closure");
    let closure = package_closure(&root.join("ports/app")).unwrap();
    let canonical = |p: &str| std::fs::canonicalize(root.join(p)).unwrap();
    assert_eq!(closure, [canonical("ports/app"), canonical("libs/math")]);
    assert_eq!(inputs(&root.join("ports/app")).unwrap(), closure);
    let file = root.join("ports/app/src/lib.ves");
    assert_eq!(inputs(&file).unwrap(), [file]);
    assert!(inputs(&root.join("nowhere")).is_err());
}

#[test]
fn edits_apply_to_a_copy_with_the_same_layout() {
    let root = fixture("edits");
    let app = root.join("ports/app");
    // No edits: the package itself.
    assert_eq!(
        prepare_source(&app, &root.join("scratch"), &[]).unwrap(),
        app
    );
    let edit = |from: &str, to: &str| vec![(from.to_owned(), to.to_owned())];
    let copy = prepare_source(&app, &root.join("scratch"), &edit("{ 1 }", "{ 3 }")).unwrap();
    assert_eq!(copy, root.join("scratch/ports/app"));
    let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
    assert_eq!(read(copy.join("src/lib.ves")), "pub fn f() -> i64 { 3 }\n");
    assert_eq!(read(copy.join("src/a/b.ves")), "pub fn g() -> i64 { 2 }\n");
    // The dependency is copied beside it, unedited, and the original is untouched.
    assert_eq!(
        read(root.join("scratch/libs/math/src/lib.ves")),
        "pub fn h() -> i64 { 2 }\n"
    );
    assert_eq!(read(app.join("src/lib.ves")), "pub fn f() -> i64 { 1 }\n");
    // An edit must hit exactly one place in the root package; the
    // dependency's `{ 2 }` does not count, but two root-package hits do.
    assert!(prepare_source(&app, &root.join("scratch"), &edit("{ 2 }", "{ 4 }")).is_ok());
    let err = prepare_source(&app, &root.join("scratch"), &edit("pub fn", "fn")).unwrap_err();
    assert!(err.contains("exactly once"), "{err}");
    let err = prepare_source(&app, &root.join("scratch"), &edit("missing", "x")).unwrap_err();
    assert!(err.contains("exactly once"), "{err}");
}

#[test]
fn a_single_file_is_edited_in_a_copy() {
    let root = fixture("file");
    let file = root.join("ports/app/src/lib.ves");
    let copy = prepare_source(
        &file,
        &root.join("scratch"),
        &[("{ 1 }".to_owned(), "{ 5 }".to_owned())],
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(copy).unwrap(),
        "pub fn f() -> i64 { 5 }\n"
    );
}
