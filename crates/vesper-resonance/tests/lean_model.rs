//! Checks for `lean/ResonanceSpsc.lean`.
//!
//! The static audit always runs. With `VESPER_LEAN` set (as in CI) the file
//! is compiled, which kernel-checks every proof, and its axiom report is
//! checked. The theorems are about the Lean models (the SPSC index protocol
//! and the PDC latency equations), not about this crate's Rust code or
//! `rtrb`; the Rust tests check the same properties on concrete runs.

use std::path::{Path, PathBuf};

const MODEL_ID: &str = "vesper.resonance.spsc-pdc-model/1";
const AUDITED_THEOREMS: usize = 11;
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const THEOREMS: [&str; 11] = [
    "theorem spsc_ring_occupancy_bounded",
    "theorem slot_distinct",
    "theorem producer_slot_not_readable",
    "theorem step_preserves",
    "theorem reachable_inv",
    "theorem spsc_no_shared_slot",
    "theorem pdc_equalizes_parallel_branch_latency",
    "theorem le_maxList",
    "theorem compiled_is_compensated",
    "theorem path_latency",
    "theorem pdc_paths_equal",
];

fn lean_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean")
}

/// Remove `--` line comments and nested `/- -/` block comments.
fn strip_lean_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let (mut out, mut depth, mut i) = (Vec::new(), 0usize, 0);
    while i < bytes.len() {
        if bytes[i..].starts_with(b"/-") {
            depth += 1;
            i += 2;
        } else if depth > 0 && bytes[i..].starts_with(b"-/") {
            depth -= 1;
            i += 2;
        } else if depth == 0 && bytes[i..].starts_with(b"--") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else {
            if depth == 0 {
                out.push(bytes[i]);
            }
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn lean_model_source_audit() {
    let source =
        std::fs::read_to_string(lean_dir().join("ResonanceSpsc.lean")).expect("read model");
    assert!(source.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    assert!(source.contains("not a proof about the Rust implementation"));
    assert_eq!(source.matches("\n#print axioms ").count(), AUDITED_THEOREMS);
    for theorem in THEOREMS {
        assert!(source.contains(theorem), "missing {theorem}");
        let name = theorem.trim_start_matches("theorem ");
        assert!(
            source.contains(&format!("#print axioms Vesper.ResonanceSpsc.{name}\n")),
            "axioms of {name} not printed"
        );
    }
    let code = strip_lean_comments(&source);
    assert!(!code.contains("partial ") && !code.contains("unsafe "));
    for forbidden in [
        "sorry",
        "admit",
        "native_decide",
        "bv_decide",
        "ofReduceBool",
    ] {
        assert!(!code.contains(forbidden), "Lean model uses {forbidden}");
    }
    assert!(
        !code.lines().any(|l| l.trim_start().starts_with("axiom ")),
        "Lean model declares an axiom"
    );
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn compile_model(lean: &str, out: &Path) {
    // Lean names the module after the path relative to its working
    // directory, so compile from `lean/`.
    let compiled = std::process::Command::new(lean)
        .current_dir(lean_dir())
        .arg("-o")
        .arg(out.join("ResonanceSpsc.olean"))
        .arg("ResonanceSpsc.lean")
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).expect("UTF-8 Lean output");
    assert!(
        compiled.status.success(),
        "Lean model failed: {}\n{report}\n{}",
        compiled.status,
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut audited = 0;
    for line in report.lines() {
        let axioms = line
            .split_once(" depends on axioms: [")
            .and_then(|(_, rest)| rest.strip_suffix(']'))
            .unwrap_or_else(|| panic!("unexpected Lean output: {line}"));
        for axiom in axioms.split(", ") {
            assert!(
                STANDARD_AXIOMS.contains(&axiom),
                "unapproved axiom {axiom} in {line}"
            );
        }
        audited += 1;
    }
    assert_eq!(audited, AUDITED_THEOREMS, "axiom report lines");
}

#[test]
fn lean_model_compiles_with_standard_axioms_only() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean model compilation skipped");
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let out = std::env::temp_dir().join(format!(
        "vesper-resonance-model-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&out).expect("private Lean output directory");
    let _cleanup = Cleanup(out.clone());
    compile_model(&lean, &out);
}
