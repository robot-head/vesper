//! `lean/ChronoReplay.lean`: source hygiene always; with `VESPER_LEAN` set (as
//! in CI) compile it, audit every theorem's axioms, and compare the Lean
//! lineage checker with the Rust `ObligationLineage` on generated hop logs.
//!
//! The comparison is over concrete logs. The general claims are the Lean
//! theorems, which are about the Lean models, not about this Rust code.

use std::io::Write;
use std::path::{Path, PathBuf};

use vesper_chrono::{HopKind, ObligationHop, ObligationLineage};

const MODEL_ID: &str = "vesper.chrono.replay-model/1";
const MODEL_CLAIM: &str = "interaction-tree replay, snapshot fold and lineage log models; not a proof about the Rust implementation";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const THEOREMS: [&str; 10] = [
    "chrono_replay_reproduces_live_execution",
    "replay_ignores_environment",
    "replay_from_snapshot",
    "lineage_creation_precedes",
    "lineage_complete",
    "reverse_watch_finds_latest_mutation",
    "checkpoint_restore_replays_to_live",
    "reexecuted_checkpoint_replays_to_live",
    "restore_rejects_relocation",
    "restore_at_captured_base",
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
    let source = std::fs::read_to_string(root().join("ChronoReplay.lean")).expect("read model");
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
        assert!(
            !code.contains(banned),
            "ChronoReplay.lean contains {banned:?}"
        );
    }
    assert!(code.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    for theorem in THEOREMS {
        assert!(
            code.contains(&format!("theorem {theorem}")),
            "{theorem} missing"
        );
        assert!(
            code.contains(&format!("#print axioms Vesper.ChronoReplay.{theorem}")),
            "{theorem} not audited"
        );
    }
}

/// Rust result in the fixture's output format.
fn rust_verdict(log: &[(&'static str, u64)]) -> String {
    let mut lineage = ObligationLineage::new();
    for (i, (kind, id)) in log.iter().enumerate() {
        let kind = match *kind {
            "mint" => HopKind::Mint { parent: None },
            "move" => HopKind::SinkMove,
            "proj" => HopKind::Projection { place_hash: 1 },
            "handoff" => HopKind::ChannelHandoff { to_fiber: 2 },
            "discharge" => HopKind::UnpackDischarge,
            "leak" => HopKind::Leak,
            _ => unreachable!(),
        };
        let hop = ObligationHop {
            lamport_seq: i as u64 + 1,
            fiber_id: 0,
            source_span_id: 0,
            kind,
        };
        if lineage.record(*id, hop).is_err() {
            return "reject".into();
        }
    }
    assert!(lineage.audit_complete());
    let mut done: Vec<u64> = (1..=lineage.len() as u64)
        .filter(|id| lineage.status(*id).unwrap() != vesper_chrono::ObligationStatus::Live)
        .collect();
    done.sort();
    std::iter::once("ok".to_string())
        .chain(done.iter().map(u64::to_string))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Logs with monotonically minted ids (the Rust invariant the model does not
/// state), mixing valid hops with unknown ids, hops after a terminal and
/// double mints.
fn generate(cases: usize) -> Vec<Vec<(&'static str, u64)>> {
    let mut state = 0x00c4_0e0f_1a2b_3c4du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let kinds = ["move", "proj", "handoff", "discharge", "leak"];
    (0..cases)
        .map(|_| {
            let mut log = Vec::new();
            let mut minted = 0u64;
            let len = 1 + next() % 12;
            for _ in 0..len {
                let r = next() % 100;
                if r < 30 || minted == 0 && r < 80 {
                    minted += 1;
                    log.push(("mint", minted));
                } else if r < 33 && minted > 0 {
                    log.push(("mint", 1 + next() % minted)); // double mint
                } else {
                    let id = 1 + next() % (minted + 1); // may be unminted
                    log.push((kinds[(next() % 5) as usize], id));
                }
            }
            log
        })
        .collect()
}

fn lean_path(lean: &str, dir: &Path) -> std::ffi::OsString {
    let mut paths = vec![dir.to_path_buf()];
    if let Ok(out) = std::process::Command::new(lean)
        .arg("--print-libdir")
        .output()
        && out.status.success()
    {
        let lib = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !lib.is_empty() {
            paths.push(PathBuf::from(lib));
        }
    }
    std::env::join_paths(paths).expect("LEAN_PATH")
}

#[test]
fn lean_model_compiles_and_matches_rust_lineage() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation and differential skipped");
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("vesper-chrono-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("private Lean output directory");
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());

    let compiled = std::process::Command::new(&lean)
        .current_dir(root())
        .arg("-o")
        .arg(dir.join("ChronoReplay.olean"))
        .arg("ChronoReplay.lean")
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

    let logs = generate(2_000);
    let input: String = logs
        .iter()
        .map(|log| {
            log.iter()
                .map(|(k, id)| format!("{k}:{id}"))
                .collect::<Vec<_>>()
                .join(" ")
                + "\n"
        })
        .collect();
    let mut child = std::process::Command::new(&lean)
        .env("LEAN_PATH", lean_path(&lean, &dir))
        .arg("--run")
        .arg(root().join("ChronoReplay/Fixture.lean"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn Lean");
    let mut stdin = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().expect("wait for Lean");
    writer.join().expect("writer").expect("write stdin");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        output.status.success(),
        "Lean fixture failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = stdout.lines();
    assert_eq!(
        lines.next(),
        Some(format!("{MODEL_ID} | {MODEL_CLAIM}").as_str())
    );
    let lines: Vec<_> = lines.collect();
    assert_eq!(lines.len(), logs.len());
    let (mut accepted, mut rejected) = (0, 0);
    for (log, lean_line) in logs.iter().zip(&lines) {
        let rust = rust_verdict(log);
        assert_eq!(&rust, lean_line, "log {log:?}");
        if rust == "reject" {
            rejected += 1;
        } else {
            accepted += 1;
        }
    }
    assert!(
        accepted > 200 && rejected > 200,
        "both outcomes exercised: {accepted}/{rejected}"
    );
    eprintln!(
        "Lean and Rust lineage agree on {} logs ({accepted} accepted, {rejected} rejected)",
        logs.len()
    );
}
