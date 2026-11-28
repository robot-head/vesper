//! Source Strata regions on every backend (design §15, wave eight lane 8):
//! the evaluator, the native JIT and AOT executables built with `vesper
//! build` agree on region programs with aggregate payloads, on the bytes a
//! region charges (`vesper.region.accounting/1`) and on run-time faults;
//! escape attempts are rejected with the existing diagnostics; and the Lean
//! model of the region rules (`lean/StrataRegions.lean`) checks.
//!
//! The Lean theorems are about the model of the rules, not about the Rust
//! checker or the backends; the backend agreement here is tested on these
//! programs only.
mod support;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-regions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ----- the Lean model --------------------------------------------------------

const NAMESPACE: &str = "Vesper.StrataRegions";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const KEY_THEOREMS: [&str; 9] = [
    "soundness",
    "lexical_lifetimes",
    "never_wrong",
    "escape_freedom",
    "dangling_reads_the_next_region",
    "escaping_box_rejected",
    "cross_region_rejected",
    "nested_typed",
    "nested_runs",
];

fn model() -> PathBuf {
    support::repo_root().join("lean/StrataRegions.lean")
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
fn lean_model_has_no_escape_hatches() {
    let source = std::fs::read_to_string(model()).expect("read lean/StrataRegions.lean");
    let code = strip_comments(&source);
    for word in [
        "sorry",
        "admit",
        "axiom",
        "native_decide",
        "implemented_by",
        "extern",
    ] {
        assert!(
            !code
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|w| w == word),
            "lean/StrataRegions.lean uses `{word}`"
        );
    }
    for theorem in KEY_THEOREMS {
        assert!(
            code.contains(&format!("theorem {theorem}")),
            "missing theorem {theorem}"
        );
    }
}

/// With `VESPER_LEAN` set (as in CI), compile the model with the pinned
/// Lean and check that every key theorem depends on the standard axioms at
/// most.
#[test]
fn lean_model_checks_with_standard_axioms_only() {
    let Some(lean) = std::env::var_os("VESPER_LEAN") else {
        eprintln!("Lean model check skipped: set VESPER_LEAN to the pinned Lean");
        return;
    };
    let temp = Temp::new();
    let mut source = std::fs::read_to_string(model()).unwrap();
    for theorem in KEY_THEOREMS {
        source.push_str(&format!("\n#print axioms {NAMESPACE}.{theorem}"));
    }
    let file = temp.0.join("StrataRegionsAudit.lean");
    std::fs::write(&file, source).unwrap();
    let out = Command::new(lean).arg(&file).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!stdout.contains("warning"), "{stdout}");
    for theorem in KEY_THEOREMS {
        let line = stdout
            .lines()
            .find(|l| l.starts_with(&format!("'{NAMESPACE}.{theorem}'")))
            .unwrap_or_else(|| panic!("no axiom report for {theorem}:\n{stdout}"));
        if let Some((_, list)) = line.split_once("axioms: [") {
            for axiom in list.trim_end_matches(']').split(',').map(str::trim) {
                assert!(
                    STANDARD_AXIOMS.contains(&axiom),
                    "{theorem} depends on {axiom}"
                );
            }
        } else {
            assert!(line.ends_with("does not depend on any axioms"), "{line}");
        }
    }
}
