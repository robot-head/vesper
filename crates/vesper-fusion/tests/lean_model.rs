//! Checks for `lean/FusionRules.lean`.
//!
//! The static audit always runs: one refinement lemma per egglog rule in
//! [`vesper_fusion::RULE_LEMMAS`], every rule set rewrite accounted for, every
//! theorem's axioms printed, and no `sorry`, `admit` or new axioms. With
//! `VESPER_LEAN` set (as in CI) the file is compiled, which kernel-checks every
//! proof, and its axiom report is checked. The theorems are about the Lean
//! operator models, not about this crate's Rust code, egglog or `f32`.

use std::path::PathBuf;

const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const NAMESPACE: &str = "Vesper.FusionRules";
/// Theorems beyond the per-rule lemmas.
const EXTRA_THEOREMS: [&str; 1] = ["cascade_fused_eq_seq"];

fn lean_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean")
}

fn theorems() -> Vec<&'static str> {
    vesper_fusion::RULE_LEMMAS
        .iter()
        .map(|(_, lemma)| *lemma)
        .chain(EXTRA_THEOREMS)
        .collect()
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
fn every_rewrite_has_a_named_rule_and_lemma() {
    let rules = format!(
        "{}{}",
        vesper_fusion::tensor::RULES,
        vesper_fusion::dsp::RULES
    );
    assert_eq!(
        rules.matches("(rewrite ").count(),
        vesper_fusion::RULE_LEMMAS.len(),
        "one lemma per rewrite"
    );
    for (rule, _) in vesper_fusion::RULE_LEMMAS {
        assert!(
            rules.contains(rule),
            "rule {rule} is not named in a rule set comment"
        );
    }
}

#[test]
fn lean_model_source_audit() {
    let source = std::fs::read_to_string(lean_dir().join("FusionRules.lean")).expect("read model");
    assert!(source.contains("def modelId : String := \"vesper.fusion.rules-model/1\""));
    assert!(source.contains("not a proof about the Rust implementation"));
    let names = theorems();
    assert_eq!(
        source.matches("\n#print axioms ").count(),
        names.len(),
        "every theorem's axioms are printed, and nothing else"
    );
    for (rule, lemma) in vesper_fusion::RULE_LEMMAS {
        assert!(
            source.contains(&format!("`{rule}`")),
            "rule {rule} is not cited in the model"
        );
        let _ = lemma;
    }
    for name in &names {
        let decl = format!("theorem {name}");
        assert!(
            source
                .match_indices(&decl)
                .any(|(i, _)| source[i + decl.len()..].starts_with(char::is_whitespace)),
            "missing theorem {name}"
        );
        assert!(
            source.contains(&format!("#print axioms {NAMESPACE}.{name}\n")),
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
        assert!(!code.contains(forbidden), "model uses {forbidden}");
    }
    assert!(
        !code.lines().any(|l| l.trim_start().starts_with("axiom ")),
        "model declares an axiom"
    );
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
        "vesper-fusion-model-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&out).expect("private Lean output directory");
    let _cleanup = Cleanup(out.clone());
    // Lean names the module after the path relative to its working
    // directory, so compile from `lean/`.
    let compiled = std::process::Command::new(lean)
        .current_dir(lean_dir())
        .arg("-o")
        .arg(out.join("FusionRules.olean"))
        .arg("FusionRules.lean")
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
        if line.ends_with(" does not depend on any axioms") {
            audited += 1;
            continue;
        }
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
    assert_eq!(audited, theorems().len(), "axiom report lines");
}
