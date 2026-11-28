//! The checked-in files generated from `cranelift.vsys` match what
//! build.rs generates: the WIT, the Wasm backend's import table and the
//! Stage 2 declaration table. Set `VESPER_BLESS=1` to rewrite them.
//!
//! The component itself is exercised by crates/vesper-cranelift-sys's
//! fixture tests, which run Stage 2 builds of them with their Cranelift
//! imports served by it and compare the objects with Rust-built ones.

use std::path::{Path, PathBuf};

/// The checked-in WIT, relative to the repository.
const WIT_PATH: &str = "crates/vesper-cranelift-component/wit/vcl.wit";

/// The repository root: the nearest ancestor holding the checked-in WIT.
/// Under Bazel `CARGO_MANIFEST_DIR` is `.` (tests start in their package
/// directory in the runfiles tree), so the search starts from its absolute
/// form.
fn repo() -> PathBuf {
    let dir = std::path::absolute(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    dir.ancestors()
        .find(|d| d.join(WIT_PATH).is_file())
        .map(Path::to_path_buf)
        .expect("cannot find the repository root")
}

#[test]
fn the_checked_in_wit_is_the_generated_one() {
    let path = repo().join(WIT_PATH);
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    if std::env::var_os("VESPER_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vesper_cranelift_component::WIT).unwrap();
        return;
    }
    assert!(
        on_disk == vesper_cranelift_component::WIT,
        "{} is stale: rerun with VESPER_BLESS=1",
        path.display()
    );
}
#[test]
fn the_wasm_backend_import_table_is_the_generated_one() {
    let path = repo().join("packages/vesper-wasm-backend/src/vcl.ves");
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    if std::env::var_os("VESPER_BLESS").is_some() {
        std::fs::write(&path, vesper_cranelift_component::VCL_VES).unwrap();
        return;
    }
    assert!(
        on_disk == vesper_cranelift_component::VCL_VES,
        "{} is stale: rerun with VESPER_BLESS=1",
        path.display()
    );
}
#[test]
fn the_stage2_declaration_table_is_the_generated_one() {
    let path = repo().join("packages/vesper-stage2/src/vcl_decls.ves");
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    if std::env::var_os("VESPER_BLESS").is_some() {
        std::fs::write(&path, vesper_cranelift_component::VCL_DECLS_VES).unwrap();
        return;
    }
    assert!(
        on_disk == vesper_cranelift_component::VCL_DECLS_VES,
        "{} is stale: rerun with VESPER_BLESS=1",
        path.display()
    );
}
