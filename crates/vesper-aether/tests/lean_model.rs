//! `lean/AetherMemory.lean`: source hygiene always; with `VESPER_LEAN` set (as
//! in CI) compile it and audit every theorem's axioms. The theorems are about
//! the Lean models (intervals, slot-plan steps, fork nonces, copy-on-write
//! pages), not about this Rust code.
use std::path::PathBuf;

const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const THEOREMS: [&str; 21] = [
    "checked_interval_sound",
    "checked_endpoint_exact",
    "separated_no_common_address",
    "adjacent_endpoint_excluded",
    "fault_page_contained",
    "fault_page_index_bounded",
    "page_slice_contained",
    "running_reach_overlap",
    "slot_end_within_width",
    "nonce_injective",
    "distinct_epochs_distinct_nonces",
    "issued_epochs_nodup",
    "fork_clones_nonce_disjoint",
    "cow_write_reads_back",
    "cow_write_other_pages",
    "scan_allows_iff_mem",
    "scan_denies_unlisted",
    "chunks_bounded",
    "chunks_cover",
    "advance_in_range",
    "advance_wraps_iff",
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
    let source = std::fs::read_to_string(root().join("AetherMemory.lean")).expect("read model");
    let code = strip_comments(&source);
    for forbidden in [
        "sorry",
        "admit",
        "native_decide",
        "ofReduceBool",
        "implemented_by",
    ] {
        assert!(!code.contains(forbidden), "Lean model uses {forbidden}");
    }
    assert!(
        !code.lines().any(|l| l.trim_start().starts_with("axiom ")),
        "Lean model declares an axiom"
    );
    for theorem in THEOREMS {
        assert!(code.contains(&format!("theorem {theorem} ")), "{theorem}");
        assert!(
            code.contains(&format!("#print axioms {theorem}\n")),
            "{theorem}"
        );
    }
    assert_eq!(code.matches("\ntheorem ").count(), THEOREMS.len());
}

#[test]
fn lean_model_compiles_with_standard_axioms_only() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation skipped");
        return;
    };
    let compiled = std::process::Command::new(&lean)
        .current_dir(root())
        .arg("AetherMemory.lean")
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).expect("UTF-8");
    assert!(
        compiled.status.success(),
        "Lean model failed: {}\n{report}\n{}",
        compiled.status,
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut audited = 0;
    for line in report.lines() {
        if line.ends_with("does not depend on any axioms") {
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
    assert_eq!(audited, THEOREMS.len(), "axiom report lines");
}
