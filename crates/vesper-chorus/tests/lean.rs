//! Checks `lean/ChorusQuorum.lean` with the pinned toolchain when
//! `VESPER_LEAN` is set (CI sets it), and always checks the source for
//! forbidden constructs. The theorems are about the Lean models only.

use std::path::PathBuf;
use vesper_chorus::raft::QuorumRule;

const THEOREMS: [&str; 9] = [
    "raft_election_single_leader_per_term",
    "majority_quorums_intersect",
    "exists_common_voter",
    "half_floor_quorums_can_be_disjoint",
    "castVote_keeps_vote",
    "election_safety",
    "append_yields_leader_prefix",
    "follower_append_preserves_matching",
    "leader_append_preserves_matching",
];
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];

fn lean_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean")
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
fn lean_source_has_no_escape_hatches() {
    let src = std::fs::read_to_string(lean_dir().join("ChorusQuorum.lean")).unwrap();
    let code = strip_comments(&src);
    for forbidden in [
        "sorry",
        "admit",
        "native_decide",
        "ofReduceBool",
        "partial ",
        "unsafe ",
    ] {
        assert!(!code.contains(forbidden), "uses {forbidden}");
    }
    assert!(!code.lines().any(|l| l.trim_start().starts_with("axiom ")));
    for t in THEOREMS {
        assert!(code.contains(&format!("theorem {t}")), "missing {t}");
        assert!(
            code.contains(&format!("#print axioms {t}\n")),
            "{t} not audited"
        );
    }
}

#[test]
fn lean_model_checks_and_matches_rust_quorum_sizes() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean ChorusQuorum check skipped");
        return;
    };
    let out = std::process::Command::new(lean)
        .current_dir(lean_dir())
        .arg("ChorusQuorum.lean")
        .output()
        .expect("spawn Lean");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        out.status.success(),
        "Lean failed: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    // Axiom audit.
    let mut audited = 0;
    for t in THEOREMS {
        let tag = format!("'Vesper.ChorusQuorum.{t}'");
        let line = stdout
            .lines()
            .find(|l| l.starts_with(&tag))
            .unwrap_or_else(|| panic!("no axiom report for {t}:\n{stdout}"));
        if let Some((_, list)) = line.split_once("depends on axioms: [") {
            for ax in list.trim_end_matches(']').split(", ") {
                assert!(STANDARD_AXIOMS.contains(&ax), "{t} depends on {ax}");
            }
        } else {
            assert!(line.contains("does not depend on any axioms"), "{line}");
        }
        audited += 1;
    }
    assert_eq!(audited, THEOREMS.len());
    // Executable fixture: the Lean majority table equals the Rust rule.
    let start = stdout.find('[').expect("majority table");
    let end = stdout[start..].find(']').expect("table end") + start;
    let table: Vec<u32> = stdout[start + 1..end]
        .split(',')
        .map(|s| s.trim().parse().unwrap())
        .collect();
    assert_eq!(table.len(), 65);
    for (n, q) in table.iter().enumerate() {
        assert_eq!(*q, QuorumRule::Majority.size(n as u32), "n = {n}");
    }
}
