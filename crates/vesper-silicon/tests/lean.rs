//! Checks `lean/SiliconFrames.lean`: always scans it for `sorry`, `admit`
//! and new axioms; elaborates and kernel-checks it when `VESPER_LEAN` names
//! a Lean 4 binary (as in CI). The theorems are about the Lean models, not
//! about this crate's Rust code.

use std::path::PathBuf;
use std::process::Command;

fn lean_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean/SiliconFrames.lean")
}

fn strip_comments(src: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"/-") {
            depth += 1;
            i += 2;
        } else if depth > 0 && b[i..].starts_with(b"-/") {
            depth -= 1;
            i += 2;
        } else if depth == 0 && b[i..].starts_with(b"--") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else {
            if depth == 0 {
                out.push(b[i] as char);
            }
            i += 1;
        }
    }
    out
}

#[test]
fn lean_model_has_no_escape_hatches() {
    let src = std::fs::read_to_string(lean_file()).expect("read Lean file");
    let code = strip_comments(&src);
    for word in [
        "sorry",
        "admit",
        "axiom ",
        "unsafe ",
        "implemented_by",
        "native_decide",
    ] {
        assert!(!code.contains(word), "SiliconFrames.lean uses `{word}`");
    }
    for theorem in [
        "w1c_preserves_unwritten",
        "w1c_single_ack",
        "rmw_clears_all_pending",
        "step_preserves_pfns",
        "step_preserves_nodup",
        "count_partition",
        "step_conserves_total",
        "no_reuse_without_shootdown",
        "pending_exit_is_shootdown",
    ] {
        assert!(
            code.contains(&format!("theorem {theorem}")),
            "missing {theorem}"
        );
    }
}

#[test]
fn lean_model_checks() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; SiliconFrames.lean not elaborated");
        return;
    };
    let out = Command::new(&lean)
        .arg(lean_file())
        .output()
        .unwrap_or_else(|e| panic!("run {lean}: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "lean failed:\n{text}");
    assert!(!text.contains("error"), "lean reported errors:\n{text}");
    assert!(!text.contains("declaration uses 'sorry'"), "{text}");
}
