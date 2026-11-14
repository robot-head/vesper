//! Checks for `lean/ResonanceSpsc.lean` and `lean/ResonanceStability.lean`.
//!
//! The static audit always runs. With `VESPER_LEAN` set (as in CI) the file
//! is compiled, which kernel-checks every proof, and its axiom report is
//! checked. The theorems are about the Lean models (the SPSC index protocol,
//! the PDC latency equations, the SVF over exact rationals and the PID
//! clamp), not about this crate's Rust code, `f32` arithmetic or `rtrb`; the
//! Rust tests check the same properties on concrete runs.

use std::path::{Path, PathBuf};

const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];

struct Model {
    file: &'static str,
    namespace: &'static str,
    id: &'static str,
    theorems: &'static [&'static str],
}

const MODELS: [Model; 2] = [
    Model {
        file: "ResonanceSpsc",
        namespace: "Vesper.ResonanceSpsc",
        id: "vesper.resonance.spsc-pdc-model/1",
        theorems: &[
            "spsc_ring_occupancy_bounded",
            "slot_distinct",
            "producer_slot_not_readable",
            "step_preserves",
            "reachable_inv",
            "spsc_no_shared_slot",
            "pdc_equalizes_parallel_branch_latency",
            "le_maxList",
            "compiled_is_compensated",
            "path_latency",
            "pdc_paths_equal",
        ],
    },
    Model {
        file: "ResonanceStability",
        namespace: "Vesper.ResonanceStability",
        id: "vesper.resonance.stability-model/1",
        theorems: &[
            "W_nonneg",
            "W_coercive",
            "svf_step_implicit",
            "svf_energy_identity",
            "svf_cross_bound",
            "svf_stretch_identity",
            "svf_state_from_mid",
            "svf_lyapunov_combine",
            "svf_lyapunov_step",
            "svf_bound_combine",
            "svf_step_bounded",
            "svf_bound_mono",
            "svf_bibo",
            "svf_state_bounded",
            "svf_output_bounded",
            "clamp_mem",
            "pid_torque_clamped",
            "pid_integral_clamped",
            "pid_invariant",
        ],
    },
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
    for model in &MODELS {
        let source = std::fs::read_to_string(lean_dir().join(format!("{}.lean", model.file)))
            .expect("read model");
        assert!(source.contains(&format!("def modelId : String := \"{}\"", model.id)));
        assert!(source.contains("not a proof about the Rust implementation"));
        assert_eq!(
            source.matches("\n#print axioms ").count(),
            model.theorems.len(),
            "{}: every theorem's axioms are printed, and nothing else",
            model.file
        );
        for name in model.theorems {
            let decl = format!("theorem {name}");
            assert!(
                source
                    .match_indices(&decl)
                    .any(|(i, _)| source[i + decl.len()..].starts_with(char::is_whitespace)),
                "{}: missing theorem {name}",
                model.file
            );
            assert!(
                source.contains(&format!("#print axioms {}.{name}\n", model.namespace)),
                "{}: axioms of {name} not printed",
                model.file
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
            assert!(!code.contains(forbidden), "{} uses {forbidden}", model.file);
        }
        assert!(
            !code.lines().any(|l| l.trim_start().starts_with("axiom ")),
            "{} declares an axiom",
            model.file
        );
    }
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn compile_model(lean: &str, out: &Path, model: &Model) {
    // Lean names the module after the path relative to its working
    // directory, so compile from `lean/`.
    let compiled = std::process::Command::new(lean)
        .current_dir(lean_dir())
        .arg("-o")
        .arg(out.join(format!("{}.olean", model.file)))
        .arg(format!("{}.lean", model.file))
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).expect("UTF-8 Lean output");
    assert!(
        compiled.status.success(),
        "Lean model {} failed: {}\n{report}\n{}",
        model.file,
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
    assert_eq!(
        audited,
        model.theorems.len(),
        "{}: axiom report lines",
        model.file
    );
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
    for model in &MODELS {
        compile_model(&lean, &out, model);
    }
}
