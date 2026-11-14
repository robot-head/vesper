//! `lean/VerifiedCodegen.lean`: source hygiene always; with `VESPER_LEAN` set
//! (as in CI) compile it with the pinned Lean, check the kernel axiom report of
//! every theorem, and check that the instruction-selection table it prints is
//! the Rust table (`verified_codegen::table_listing`) line for line.
//!
//! The theorems are about the Lean models of the slot IR, the abstract
//! machine and the selection table, not about the Rust emitter.
use std::collections::BTreeSet;
use std::path::PathBuf;

const NAMESPACE: &str = "Vesper.VerifiedCodegen";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const KEY_THEOREMS: [&str; 9] = [
    "select_simulates",
    "select_branch",
    "select_control",
    "sim_arith",
    "sim_wrapping",
    "sim_neg",
    "sim_compare",
    "tdiv_fits_i64",
    "toS_mod_eq",
];

fn model() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean/VerifiedCodegen.lean")
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

fn theorems(code: &str) -> Vec<String> {
    code.lines()
        .filter_map(|l| l.strip_prefix("theorem "))
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '\''))
                .collect()
        })
        .collect()
}

#[test]
fn model_has_no_escape_hatches_and_audits_every_theorem() {
    let source = std::fs::read_to_string(model()).expect("read lean/VerifiedCodegen.lean");
    let code = strip_comments(&source);
    for banned in [
        "sorry",
        "admit",
        "axiom ",
        "unsafe",
        "partial ",
        "native_decide",
        "bv_decide",
        "@[implemented_by",
        "@[extern",
        "opaque ",
        "import ",
    ] {
        assert!(
            !code.contains(banned),
            "VerifiedCodegen.lean contains {banned:?}"
        );
    }
    let names = theorems(&code);
    let unique: BTreeSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "duplicate theorem names");
    for t in &names {
        assert!(
            code.contains(&format!("#print axioms {NAMESPACE}.{t}\n")),
            "{t} not audited"
        );
    }
    for key in KEY_THEOREMS {
        assert!(names.iter().any(|n| n == key), "{key} missing");
    }
}

#[test]
fn rust_table_covers_every_row_kind() {
    let listing = vesper::verified_codegen::table_listing();
    assert_eq!(listing.lines().count(), 82);
    assert!(
        listing
            .contains("arith div i64: ld r0 s1; ld r1 s2; trapzero; trapdivovf; div s; st s0 r0\n")
    );
    assert!(
        listing.contains(
            "wrapping mul u8: ld r0 s1; ld r1 s2; wrap mul; li r2 255; andr2; st s0 r0\n"
        )
    );
}

#[test]
fn lean_model_compiles_and_prints_the_rust_table() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation of VerifiedCodegen.lean skipped");
        return;
    };
    let source = std::fs::read_to_string(model()).expect("read the model");
    let names = theorems(&strip_comments(&source));
    // Compile a private regular-file copy: Bazel runfiles are symlinks.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "vesper-verified-codegen-lean-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&dir).unwrap();
    let file = dir.join("VerifiedCodegen.lean");
    std::fs::write(&file, &source).unwrap();
    let out = std::process::Command::new(&lean)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg(&file)
        .output()
        .expect("spawn Lean");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 output");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "Lean failed:\n{stdout}\n{stderr}");
    assert!(
        !stdout.contains("warning:") && !stdout.contains("error:"),
        "Lean diagnostics:\n{stdout}"
    );
    assert!(!stdout.contains("sorryAx"), "sorry in the axiom report");

    let table = stdout
        .split_once("BEGIN TABLE\n")
        .and_then(|(_, rest)| rest.split_once("END TABLE\n"))
        .map(|(t, _)| t)
        .expect("the Lean file prints the table");
    assert_eq!(
        table,
        vesper::verified_codegen::table_listing(),
        "the Lean selection table differs from the Rust one"
    );

    let mut audited = BTreeSet::new();
    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix('\'') else {
            continue;
        };
        let Some((name, report)) = rest.split_once('\'') else {
            continue;
        };
        if let Some(list) = report
            .trim()
            .strip_prefix("depends on axioms: [")
            .and_then(|r| r.strip_suffix(']'))
        {
            for axiom in list.split(',').map(str::trim) {
                assert!(
                    STANDARD_AXIOMS.contains(&axiom),
                    "{name} depends on {axiom}"
                );
            }
        } else {
            assert!(report.contains("does not depend on any axioms"), "{line}");
        }
        audited.insert(name.to_owned());
    }
    for t in &names {
        assert!(
            audited.contains(&format!("{NAMESPACE}.{t}")),
            "{t} has no axiom report"
        );
    }
}
