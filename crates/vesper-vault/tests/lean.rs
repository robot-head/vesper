//! Checks lean/VaultConstantTime.lean with the pinned toolchain when
//! VESPER_LEAN is set (as in CI); otherwise reports a skip.

use std::path::PathBuf;
use std::process::Command;

fn lean_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean/VaultConstantTime.lean")
}

#[test]
fn lean_model_has_no_escape_hatches() {
    let src = std::fs::read_to_string(lean_file()).unwrap();
    for bad in [
        "sorry",
        "admit",
        "axiom ",
        "native_decide",
        "implemented_by",
    ] {
        assert!(!src.contains(bad), "Lean model contains `{bad}`");
    }
    for theorem in [
        "theorem verify_tag_16_ct_sound",
        "theorem trace_independent",
        "theorem tagKernel_ct",
    ] {
        assert!(src.contains(theorem), "missing {theorem}");
    }
}

#[test]
fn lean_model_compiles() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean proof check skipped");
        return;
    };
    let out = Command::new(lean)
        .arg(lean_file())
        .output()
        .expect("run lean");
    assert!(
        out.status.success(),
        "lean failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!text.contains("declaration uses 'sorry'"), "{text}");
}
