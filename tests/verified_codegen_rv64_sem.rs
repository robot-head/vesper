//! `lean/CodegenRv64Semantics.lean`: an RV64IM semantics for the instructions
//! the verified RV64 emitter writes, and a proof that the sequence emitted for
//! every data instruction of the abstract machine implements `Mach.step` of
//! `lean/VerifiedCodegen.lean` (composed over sequences and over every data
//! row of the selection table).
//!
//! Source hygiene always. With `VESPER_LEAN` set (as in CI), compile
//! `VerifiedCodegen.lean` and `CodegenRv64Encoding.lean` to private oleans,
//! check the semantics file with the pinned Lean, and audit the kernel axiom
//! report of every theorem.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const NAMESPACE: &str = "Vesper.VerifiedCodegen.Rv64.Sem";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
/// One lemma per emitted sequence, and the theorems that compose them.
const KEY_THEOREMS: [&str; 33] = [
    "sim_addchk_u",
    "sim_addchk_s",
    "sim_subchk_u",
    "sim_subchk_s",
    "sim_mulchk_s",
    "sim_mulchk_u",
    "sim_wrap",
    "sim_andr2",
    "sim_trapgtu",
    "sim_trapzero",
    "sim_trapdivovf",
    "sim_div_s",
    "sim_div_u",
    "sim_rem_s",
    "sim_rem_u",
    "sim_negchk_s",
    "sim_negchk_u",
    "sim_xorone",
    "sim_cmp",
    "sim_fop",
    "sim_fneg",
    "sim_fcmp",
    "sim_trap",
    "sim_imm",
    "sim_ld",
    "sim_st",
    "sim_bz",
    "sim_jmp",
    "emit_sim",
    "run_sim",
    "row_sim",
    "steps_seq",
    "imm_lands",
];

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// A fresh private directory (Bazel runfiles are symlinks, so the models
/// are compiled from regular-file copies).
fn scratch() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("vesper-rv64-sem-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    dir
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

/// Theorem names, including `@[simp]` ones.
fn theorems(code: &str) -> Vec<String> {
    code.lines()
        .filter_map(|l| {
            l.strip_prefix("theorem ")
                .or_else(|| l.strip_prefix("@[simp] theorem "))
        })
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '\''))
                .collect()
        })
        .collect()
}

#[test]
fn semantics_has_no_escape_hatches_and_audits_every_theorem() {
    let source = read("lean/CodegenRv64Semantics.lean");
    let code = strip_comments(&source).replacen("import CodegenRv64Encoding\n", "", 1);
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
            "CodegenRv64Semantics.lean contains {banned:?}"
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

/// Every data instruction of the abstract machine has its lemma, so a new
/// `M` constructor cannot slip past `emit_sim`.
#[test]
fn every_data_instruction_has_a_semantics_lemma() {
    let model = strip_comments(&read("lean/VerifiedCodegen.lean"));
    let decl = model
        .split_once("inductive M\n")
        .and_then(|(_, rest)| rest.split_once("\n\n"))
        .map(|(d, _)| d)
        .expect("the abstract machine's instruction type");
    let control = ["label", "jmp", "bz", "call", "enter", "leave"];
    let source = strip_comments(&read("lean/CodegenRv64Semantics.lean"));
    let emit_sim = source
        .split_once("theorem emit_sim")
        .map(|(_, rest)| rest.split("\ntheorem ").next().unwrap())
        .expect("emit_sim");
    let mut seen = 0;
    for line in decl.lines() {
        let Some(name) = line.trim().strip_prefix("| ") else {
            continue;
        };
        let name: String = name
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        seen += 1;
        if control.contains(&name.as_str()) {
            continue;
        }
        assert!(
            emit_sim.contains(&format!("| {name} ")) || emit_sim.contains(&format!("| {name} =>")),
            "emit_sim has no case for M.{name}"
        );
    }
    assert!(seen >= 25, "parsed only {seen} constructors of M");
}

#[test]
fn ci_checks_the_semantics_file() {
    let script = read("scripts/ci/check-lean-models.sh");
    assert!(script.contains("lean/CodegenRv64Semantics.lean"));
    assert!(script.contains("-o \"$oracle_model_dir/CodegenRv64Encoding.olean\""));
}

#[test]
fn lean_semantics_compiles_with_standard_axioms() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation of CodegenRv64Semantics.lean skipped");
        return;
    };
    let source = read("lean/CodegenRv64Semantics.lean");
    let names = theorems(&strip_comments(&source));
    let dir = scratch();
    let compile = |file: &str, olean: Option<&str>| {
        std::fs::write(dir.join(file), read(&format!("lean/{file}"))).unwrap();
        let mut cmd = std::process::Command::new(&lean);
        cmd.current_dir(&dir).env("LEAN_PATH", &dir);
        if let Some(o) = olean {
            cmd.arg("-o").arg(dir.join(o));
        }
        let out = cmd.arg(dir.join(file)).output().expect("spawn Lean");
        let stdout = String::from_utf8(out.stdout).expect("UTF-8 output");
        assert!(
            out.status.success(),
            "Lean failed on {file} ({}):\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    };
    compile("VerifiedCodegen.lean", Some("VerifiedCodegen.olean"));
    compile(
        "CodegenRv64Encoding.lean",
        Some("CodegenRv64Encoding.olean"),
    );
    let stdout = compile("CodegenRv64Semantics.lean", None);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !stdout.contains("warning:") && !stdout.contains("error:"),
        "Lean diagnostics:\n{stdout}"
    );
    assert!(!stdout.contains("sorryAx"), "sorry in the axiom report");
    let mut audited = BTreeSet::new();
    for line in stdout.lines() {
        let Some((name, report)) = line
            .strip_prefix('\'')
            .and_then(|rest| rest.split_once('\''))
        else {
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
