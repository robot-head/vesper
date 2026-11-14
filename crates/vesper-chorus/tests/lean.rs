//! Checks `lean/ChorusQuorum.lean`, `lean/ChorusQuorum/SessionDuality.lean`
//! and `lean/ChorusQuorum/RecursiveDuality.lean`
//! with the pinned toolchain when `VESPER_LEAN` is set (CI sets it), and
//! always checks the sources for forbidden constructs. The theorems are
//! about the Lean models only.

use std::path::PathBuf;
use vesper_chorus::raft::QuorumRule;

const THEOREMS: [&str; 11] = [
    "raft_election_single_leader_per_term",
    "majority_quorums_intersect",
    "exists_common_voter",
    "half_floor_quorums_can_be_disjoint",
    "castVote_keeps_vote",
    "election_safety",
    "append_yields_leader_prefix",
    "follower_append_preserves_matching",
    "leader_append_preserves_matching",
    "system_log_matching",
    "system_entries_owned",
];
const DUALITY_FILE: &str = "ChorusQuorum/SessionDuality.lean";
const DUALITY_NS: &str = "Vesper.ChorusQuorum.SessionDuality";
const DUALITY_THEOREMS: [&str; 7] = [
    "dual_dual",
    "proj_dual",
    "proj_defined_symm",
    "sessionDual_of_proj",
    "raft_replicate_slot_session_dual",
    "raft_vote_session_dual",
    "fan_out_slot_session_dual",
];
const RECURSIVE_FILE: &str = "ChorusQuorum/RecursiveDuality.lean";
const RECURSIVE_NS: &str = "Vesper.ChorusQuorum.RecursiveDuality";
const RECURSIVE_THEOREMS: [&str; 13] = [
    "dual_dual",
    "merge_dual",
    "loopView_dual",
    "proj_dual",
    "proj_defined_symm",
    "sessionDual_of_proj",
    "raft_heartbeat_loop_projection",
    "raft_heartbeat_loop_session_dual",
    "commit_notify_client_view",
    "commit_notify_session_dual",
    "streaming_loop_session_dual",
    "absent_loop_projects_to_stop",
    "send_receive_conflict_is_undefined",
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

fn check_source(file: &str, theorems: &[&str]) {
    let src = std::fs::read_to_string(lean_dir().join(file)).unwrap();
    let code = strip_comments(&src);
    for forbidden in [
        "sorry",
        "admit",
        "native_decide",
        "ofReduceBool",
        "partial ",
        "unsafe ",
    ] {
        assert!(!code.contains(forbidden), "{file} uses {forbidden}");
    }
    assert!(!code.lines().any(|l| l.trim_start().starts_with("axiom ")));
    for t in theorems {
        assert!(
            code.contains(&format!("theorem {t}")),
            "{file}: missing {t}"
        );
        assert!(
            code.contains(&format!("#print axioms {t}\n")),
            "{file}: {t} not audited"
        );
    }
}

#[test]
fn lean_source_has_no_escape_hatches() {
    check_source("ChorusQuorum.lean", &THEOREMS);
    check_source(DUALITY_FILE, &DUALITY_THEOREMS);
    check_source(RECURSIVE_FILE, &RECURSIVE_THEOREMS);
}

/// Runs Lean on `file` and checks the axiom report of every theorem.
fn check_lean(lean: &str, file: &str, ns: &str, theorems: &[&str]) -> String {
    let out = std::process::Command::new(lean)
        .current_dir(lean_dir())
        .arg(file)
        .output()
        .expect("spawn Lean");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        out.status.success(),
        "Lean failed on {file}: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let mut audited = 0;
    for t in theorems {
        let tag = format!("'{ns}.{t}'");
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
    assert_eq!(audited, theorems.len());
    stdout
}

#[test]
fn lean_session_duality_model_checks() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean SessionDuality check skipped");
        return;
    };
    check_lean(&lean, DUALITY_FILE, DUALITY_NS, &DUALITY_THEOREMS);
}

#[test]
fn lean_model_checks_and_matches_rust_quorum_sizes() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean ChorusQuorum check skipped");
        return;
    };
    let stdout = check_lean(&lean, "ChorusQuorum.lean", "Vesper.ChorusQuorum", &THEOREMS);
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

/// The Rust projection agrees with the Lean `raft_replicate_slot_session_dual`
/// and `raft_vote_session_dual` statements on the same choreographies.
#[test]
fn rust_projections_of_the_modelled_choreographies_are_dual() {
    use vesper_chorus::raft::{APPEND_CHOREOGRAPHY, VOTE_CHOREOGRAPHY};
    use vesper_chorus::{RoleId, are_dual, parse_choreography, project_all};
    for src in [APPEND_CHOREOGRAPHY, VOTE_CHOREOGRAPHY] {
        let chor = parse_choreography(src).unwrap();
        let all = project_all(&chor).unwrap();
        assert!(are_dual(RoleId(0), &all[0], RoleId(1), &all[1]), "{src}");
    }
}

#[test]
fn lean_recursive_duality_model_checks() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean RecursiveDuality check skipped");
        return;
    };
    check_lean(&lean, RECURSIVE_FILE, RECURSIVE_NS, &RECURSIVE_THEOREMS);
}

/// The Rust projection of the choreographies modelled in
/// `RecursiveDuality.lean` behaves as the Lean statements say: the loop
/// projects to dual recursive types, the client's view is the receive-branch
/// merge (an offer with both labels), a loop the client is absent from ends
/// for it, and a send/receive conflict is unmergeable.
#[test]
fn rust_projection_matches_the_recursive_lean_examples() {
    use vesper_chorus::{
        LocalType, ProjectionError, RoleId, are_dual, parse_choreography, project, project_all,
    };
    let labels = |t: &LocalType| -> (u16, Vec<String>) {
        match t {
            LocalType::Offer { peer, branches } => {
                (peer.0, branches.iter().map(|b| b.label.clone()).collect())
            }
            other => panic!("expected an offer, got {other:?}"),
        }
    };
    // raftHeartbeatLoop.
    let chor = parse_choreography(
        "roles Leader, Follower;
         rec Loop { Leader -> Follower {
             AppendEntries() { Follower -> Leader: Accept(); continue Loop; }
             Stop() { }
         } }",
    )
    .unwrap();
    let all = project_all(&chor).unwrap();
    assert!(matches!(all[0], LocalType::Rec { .. }), "{:?}", all[0]);
    assert!(are_dual(RoleId(0), &all[0], RoleId(1), &all[1]));
    // commitNotify: Coordinator 0, Replica 1, Client 2.
    let chor = parse_choreography(
        "roles Coordinator, Replica, Client;
         Coordinator -> Replica {
             Commit() { Replica -> Client: Committed(); }
             Abort() { Replica -> Client: Aborted(); }
         }",
    )
    .unwrap();
    let c = project(&chor, "Client").unwrap();
    assert_eq!(labels(&c), (1, vec!["Aborted".into(), "Committed".into()]));
    // streamingLoop.
    let chor = parse_choreography(
        "roles Coordinator, Replica, Client;
         rec Loop { Coordinator -> Replica {
             More() { Replica -> Client: Item(); continue Loop; }
             Fin() { Replica -> Client: Fin(); }
         } }",
    )
    .unwrap();
    match project(&chor, "Client").unwrap() {
        LocalType::Rec { body, .. } => {
            assert_eq!(labels(&body), (1, vec!["Fin".into(), "Item".into()]))
        }
        other => panic!("{other:?}"),
    }
    // absentLoop (Rust rejects a role that never appears, so the client
    // hears from the coordinator before the loop): the loop adds nothing
    // to the client's type. (Had the client been told something after the
    // loop, its branch views `continue Loop` and that receive would not
    // merge, in Rust as in the Lean model.)
    let chor = parse_choreography(
        "roles Coordinator, Replica, Client;
         Coordinator -> Client: Done();
         rec Loop { Coordinator -> Replica { Ping() { continue Loop; } Stop() { } } }",
    )
    .unwrap();
    match project(&chor, "Client").unwrap() {
        LocalType::Recv {
            peer, label, next, ..
        } => {
            assert_eq!((peer.0, label.as_str(), *next), (0, "Done", LocalType::End))
        }
        other => panic!("{other:?}"),
    }
    // conflict.
    let chor = parse_choreography(
        "roles Coordinator, Replica, Client;
         Coordinator -> Replica {
             Commit() { Replica -> Client: Committed(); }
             Abort() { Client -> Replica: Aborted(); }
         }",
    )
    .unwrap();
    assert!(matches!(
        project(&chor, "Client"),
        Err(ProjectionError::Unmergeable(_))
    ));
}
