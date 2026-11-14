//! `lean/Metatheory/Closures.lean`: source hygiene always; with `VESPER_LEAN`
//! set (as in CI) compile it with the pinned Lean and check the kernel axiom
//! report of every theorem.
//!
//! The theorems are about the Lean model of the λ_Vesper fragment with
//! closures, stateful handlers, several operations per effect and effect-row
//! variables (progress, preservation, one-shot resumption, Theorem 2 per kind,
//! row instantiation), not about the Rust compiler.

use std::collections::BTreeSet;
use std::path::PathBuf;

const NAMESPACE: &str = "Vesper.Metatheory.Closures";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
/// The headline results; the report lists all of them.
const KEY_THEOREMS: [&str; 26] = [
    "subst_ty",
    "plug_inv",
    "head_pres",
    "preservation",
    "progress",
    "type_safety",
    "release_at_most_once",
    "one_shot_resumption",
    "total_linear_exhaustion",
    "minted_never_resumed",
    "captured_never_consumed",
    "total_linear_exhaustion_per_kind",
    "inst_ty",
    "row_poly_safety",
    "typed_below",
    "captured_kont_self_contained",
    "vt_rleM",
    "linear_capture_copy_closure_rejected",
    "lin_closure_twice_rejected",
    "lin_closure_typed",
    "lin_closure_exhausts",
    "st_handler_typed",
    "st_handler_runs",
    "st_handler_exhausts",
    "linear_state_across_abort_rejected",
    "call_it_at_fail",
];

/// Resolve an elan proxy to the pinned toolchain's real binary from the
/// checkout (which holds `lean-toolchain`), as `tests/proof_audit_cli.rs` does.
fn resolve_lean(lean: String) -> String {
    let Ok(output) = std::process::Command::new(&lean)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("--print-prefix")
        .output()
    else {
        return lean;
    };
    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let binary = PathBuf::from(prefix)
        .join("bin")
        .join(format!("lean{}", std::env::consts::EXE_SUFFIX));
    if output.status.success() && binary.is_file() {
        binary.to_string_lossy().into_owned()
    } else {
        lean
    }
}

fn model() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean/Metatheory/Closures.lean")
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

/// Fully qualified names of every `theorem` declaration, tracking
/// `namespace`/`end` blocks.
fn declared_theorems(code: &str) -> Vec<String> {
    let mut namespaces: Vec<String> = Vec::new();
    let mut names = Vec::new();
    for line in code.lines() {
        if let Some(ns) = line.strip_prefix("namespace ") {
            namespaces.push(ns.trim().to_string());
        } else if let Some(ns) = line.strip_prefix("end ") {
            if namespaces.last().map(String::as_str) == Some(ns.trim()) {
                namespaces.pop();
            }
        } else if let Some(rest) = line.strip_prefix("theorem ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '\''))
                .collect();
            let mut full = namespaces.clone();
            full.push(name);
            names.push(full.join("."));
        }
    }
    names
}

#[test]
fn closures_source_has_no_escape_hatches_and_audits_every_theorem() {
    let source = std::fs::read_to_string(model()).expect("read lean/Metatheory/Closures.lean");
    let code = strip_comments(&source);
    for banned in [
        "sorry",
        "admit",
        "axiom ",
        "unsafe",
        "partial ",
        "native_decide",
        "@[implemented_by",
        "@[extern",
        "opaque ",
    ] {
        assert!(!code.contains(banned), "Closures.lean contains {banned:?}");
    }
    assert!(
        !code.contains("import "),
        "the model uses Lean core only, without imports"
    );
    let theorems = declared_theorems(&code);
    assert!(theorems.len() >= 150, "only {} theorems", theorems.len());
    let unique: BTreeSet<&String> = theorems.iter().collect();
    assert_eq!(unique.len(), theorems.len(), "duplicate theorem names");
    for theorem in &theorems {
        assert!(
            code.contains(&format!("#print axioms {theorem}\n")),
            "{theorem} not audited"
        );
    }
    for key in KEY_THEOREMS {
        let full = format!("{NAMESPACE}.{key}");
        assert!(theorems.contains(&full), "{full} missing");
    }
}

#[test]
fn closures_metatheory_compiles_with_only_standard_axioms() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation of Closures.lean skipped");
        return;
    };
    let source = std::fs::read_to_string(model()).expect("read lean/Metatheory/Closures.lean");
    let theorems = declared_theorems(&strip_comments(&source));

    // Compile a private regular-file copy: Bazel runfiles are symlinks.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "vesper-metatheory-closures-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&dir).expect("private Lean directory");
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    std::fs::write(dir.join("Closures.lean"), &source).expect("copy model");

    let lean = resolve_lean(lean);
    // Run from the checkout so an unresolved elan proxy still finds
    // `lean-toolchain`; the source itself is the private copy.
    let out = std::process::Command::new(&lean)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg(dir.join("Closures.lean"))
        .output()
        .expect("spawn Lean");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 report");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "Lean failed:\n{stdout}\n{stderr}");
    assert!(
        !stdout.contains("warning:") && !stdout.contains("error:"),
        "Lean diagnostics:\n{stdout}"
    );
    assert!(!stdout.contains("sorryAx"), "sorry in the axiom report");

    let mut audited = BTreeSet::new();
    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix('\'') else {
            continue;
        };
        let (name, tail) = rest.split_once("' ").expect("quoted theorem name");
        if let Some(list) = tail.strip_prefix("depends on axioms: [") {
            let list = list.strip_suffix(']').expect("axiom list");
            for axiom in list.split(", ") {
                assert!(
                    STANDARD_AXIOMS.contains(&axiom),
                    "{name} depends on non-standard axiom {axiom}"
                );
            }
        } else {
            assert_eq!(
                tail, "does not depend on any axioms",
                "unexpected report line: {line}"
            );
        }
        assert!(audited.insert(name.to_string()), "{name} audited twice");
    }
    let declared: BTreeSet<String> = theorems.into_iter().collect();
    assert_eq!(audited, declared, "every theorem is audited exactly once");
}
