//! `lean/FluxDbsp.lean`: source hygiene always; with `VESPER_LEAN` set (as in
//! CI) kernel-check it and audit every theorem's axioms.
//!
//! The theorems are about the Lean models (a deterministic step function,
//! an honest decoder, Z-sets as functions), not about this Rust crate.
use std::path::PathBuf;

const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const THEOREMS: [&str; 12] = [
    "bilinear_join_exact",
    "retraction_cancels",
    "integrate_delta",
    "frontier_chain_monotonic",
    "advance_preserves_live_bound",
    "retirement_preserves_live_bound",
    "run_append",
    "restore_rejected_keeps_live",
    "restore_replay_eq_uninterrupted",
    "repeated_recovery_eq_uninterrupted",
    "linear_distributes_over_partitions",
    "copartitioned_join_key",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean")
}

fn strip_comments(source: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('/', Some('-')) => {
                chars.next();
                depth += 1;
            }
            ('-', Some('/')) if depth > 0 => {
                chars.next();
                depth -= 1;
            }
            ('-', Some('-')) if depth == 0 => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

#[test]
fn lean_source_has_no_escape_hatches_and_audits_every_theorem() {
    let source = std::fs::read_to_string(root().join("FluxDbsp.lean")).expect("read model");
    let code = strip_comments(&source);
    for banned in [
        "sorry",
        "admit",
        "axiom ",
        "unsafe",
        "partial ",
        "@[implemented_by",
        "@[extern",
    ] {
        assert!(!code.contains(banned), "FluxDbsp.lean contains {banned:?}");
    }
    for theorem in THEOREMS {
        assert!(
            code.contains(&format!("theorem {theorem}")),
            "{theorem} missing"
        );
        assert!(
            code.contains(&format!("#print axioms {theorem}")),
            "{theorem} not audited"
        );
    }
}

#[test]
fn lean_model_kernel_checks_with_standard_axioms_only() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation skipped");
        return;
    };
    let compiled = std::process::Command::new(&lean)
        .current_dir(root())
        .arg("FluxDbsp.lean")
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).expect("UTF-8");
    assert!(
        compiled.status.success(),
        "Lean model failed: {}\n{report}\n{}",
        compiled.status,
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut audited = Vec::new();
    for line in report.lines() {
        let name = line
            .strip_prefix("'Vesper.FluxDbsp.")
            .and_then(|rest| rest.split_once('\''))
            .map(|(name, _)| name)
            .unwrap_or_else(|| panic!("unexpected Lean output: {line}"));
        if !line.ends_with("does not depend on any axioms") {
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
        }
        audited.push(name.to_string());
    }
    assert_eq!(audited, THEOREMS, "axiom report lines");
}
