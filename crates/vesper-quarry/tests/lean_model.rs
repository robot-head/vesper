//! `lean/QuarryWal.lean`: source hygiene always; with `VESPER_LEAN` set (as
//! in CI) compile it and audit every theorem's axioms.
//!
//! The theorems are about the Lean models of page-LSN redo, commit groups,
//! undo under strict record locks and pin conservation, not about this Rust
//! code; `tests/store_crash.rs` checks the Rust store on concrete crashes.

use std::path::PathBuf;

const MODEL_ID: &str = "vesper.quarry.wal-model/1";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const THEOREMS: [&str; 8] = [
    "redo_recovers_history",
    "redo_idempotent",
    "complete_groups",
    "recovery_ignores_torn_tail",
    "undo_restores_winners",
    "no_steal_needs_no_undo",
    "scan_restores_pool",
    "scan_conserves_page_pins",
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
    let source = std::fs::read_to_string(root().join("QuarryWal.lean")).expect("read model");
    let code = strip_comments(&source);
    for banned in [
        "sorry",
        "admit",
        "axiom ",
        "unsafe",
        "partial ",
        "@[implemented_by",
        "@[extern",
        "native_decide",
    ] {
        assert!(!code.contains(banned), "QuarryWal.lean contains {banned:?}");
    }
    assert!(code.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    for theorem in THEOREMS {
        assert!(
            code.contains(&format!("theorem {theorem}")),
            "{theorem} missing"
        );
        assert!(
            code.contains(&format!("#print axioms Vesper.QuarryWal.{theorem}")),
            "{theorem} not audited"
        );
    }
    assert_eq!(
        code.matches("#print axioms").count(),
        THEOREMS.len(),
        "every audit line is listed here"
    );
}

#[test]
fn lean_model_compiles_with_standard_axioms_only() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation skipped");
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("vesper-quarry-{}-{nanos}", std::process::id()));
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
        .arg(dir.join("QuarryWal.olean"))
        .arg("QuarryWal.lean")
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
            .strip_prefix("'Vesper.QuarryWal.")
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
    eprintln!("QuarryWal.lean: {} theorems audited", audited.len());
}
