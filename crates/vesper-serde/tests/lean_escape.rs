//! Check `lean/SimdJsonEscape.lean` with the pinned Lean when `VESPER_LEAN` is
//! set (as in CI); otherwise report a skip. The file proves the branchless
//! escape kernel model equal to the scalar recurrence; it is a theorem about
//! the Lean definitions, not about the compiled Rust code.
use std::path::PathBuf;
use std::process::Command;

fn lean_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("lean/SimdJsonEscape.lean")
        .canonicalize()
        .expect("lean/SimdJsonEscape.lean")
}

#[test]
fn proof_file_has_no_escape_hatches() {
    let text = std::fs::read_to_string(lean_file()).unwrap();
    for word in [
        "sorry",
        "admit",
        "axiom",
        "native_decide",
        "unsafe",
        "partial",
    ] {
        assert!(
            !text
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|w| w == word),
            "{word} in proof file"
        );
    }
    assert!(text.contains("theorem odd_backslash_simd_eq_scalar"));
}

#[test]
fn lean_checks_escape_kernel_proof() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean proof check skipped");
        return;
    };
    let file = lean_file();
    let output = Command::new(&lean)
        .arg(&file)
        .current_dir(file.parent().unwrap())
        .output()
        .unwrap_or_else(|e| panic!("running {lean}: {e}"));
    assert!(
        output.status.success(),
        "lean failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
