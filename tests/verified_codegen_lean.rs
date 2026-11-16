//! `lean/VerifiedCodegen.lean`: source hygiene always; with `VESPER_LEAN` set
//! (as in CI) compile it with the pinned Lean, check the kernel axiom report of
//! every theorem, and check that the instruction-selection table it prints is
//! the Rust table (`verified_codegen::table_listing`) line for line.
//!
//! The theorems are about the Lean models of the slot IR, the abstract
//! machine and the selection table, not about the Rust emitter.
//!
//! `lean/CodegenRv64Encoding.lean` (which imports that model) proves the
//! RV64 encoder model against the ISA's instruction formats; it prints the
//! encoded words of sample instructions, which must be the words
//! `verified_codegen::rv64_encoding_listing` gets from the Rust emitter.
//! `lean/CodegenA64Encoding.lean` does the same for AArch64 against the A64
//! encoding diagrams (`verified_codegen::a64_encoding_listing`), and
//! `lean/CodegenX64Encoding.lean` for x86-64 against the Intel SDM formats:
//! there the encodings are byte lists, the specification is a prefix decoder,
//! and the listing prints bytes (`verified_codegen::x64_encoding_listing`).
use std::collections::BTreeSet;
use std::path::PathBuf;

const NAMESPACE: &str = "Vesper.VerifiedCodegen";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const KEY_THEOREMS: [&str; 12] = [
    "sim_farith",
    "sim_fneg",
    "sim_fcompare",
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

const RV64_NAMESPACE: &str = "Vesper.VerifiedCodegen.Rv64";
const RV64_KEY_THEOREMS: [&str; 4] = ["decode_encode", "emit_ok", "emit_decodes", "bytes_encode"];

fn model() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean/VerifiedCodegen.lean")
}

const A64_NAMESPACE: &str = "Vesper.VerifiedCodegen.A64";
const A64_KEY_THEOREMS: [&str; 4] = ["decode_encode", "emit_ok", "emit_decodes", "bytes_encode"];

const X64_NAMESPACE: &str = "Vesper.VerifiedCodegen.X64";
const X64_KEY_THEOREMS: [&str; 4] = [
    "decode_encode",
    "decodeAll_encodeAll",
    "emit_ok",
    "emit_decodes",
];

fn x64_model() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean/CodegenX64Encoding.lean")
}

fn rv64_model() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean/CodegenRv64Encoding.lean")
}

fn a64_model() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean/CodegenA64Encoding.lean")
}

/// A fresh private directory (Bazel runfiles are symlinks, so the models
/// are compiled from regular-file copies).
fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "vesper-verified-codegen-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&dir).unwrap();
    dir
}

/// Every axiom report in Lean's output is standard; returns the audited names.
fn audit_axioms(stdout: &str) -> BTreeSet<String> {
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
    audited
}

/// The source has no escape hatches (other than the allowed imports), has
/// unique theorem names, and audits every theorem.
fn check_hygiene(file: &str, source: &str, namespace: &str, imports: &[&str], keys: &[&str]) {
    let mut code = strip_comments(source);
    for import in imports {
        code = code.replacen(&format!("import {import}\n"), "", 1);
    }
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
        assert!(!code.contains(banned), "{file} contains {banned:?}");
    }
    let names = theorems(&code);
    let unique: BTreeSet<&String> = names.iter().collect();
    assert_eq!(
        unique.len(),
        names.len(),
        "duplicate theorem names in {file}"
    );
    for t in &names {
        assert!(
            code.contains(&format!("#print axioms {namespace}.{t}\n")),
            "{t} not audited in {file}"
        );
    }
    for key in keys {
        assert!(names.iter().any(|n| n == key), "{key} missing from {file}");
    }
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
    check_hygiene(
        "VerifiedCodegen.lean",
        &source,
        NAMESPACE,
        &[],
        &KEY_THEOREMS,
    );
    let source = std::fs::read_to_string(rv64_model()).expect("read the RV64 model");
    check_hygiene(
        "CodegenRv64Encoding.lean",
        &source,
        RV64_NAMESPACE,
        &["VerifiedCodegen"],
        &RV64_KEY_THEOREMS,
    );
    let source = std::fs::read_to_string(a64_model()).expect("read the AArch64 model");
    check_hygiene(
        "CodegenA64Encoding.lean",
        &source,
        A64_NAMESPACE,
        &["VerifiedCodegen"],
        &A64_KEY_THEOREMS,
    );
    let source = std::fs::read_to_string(x64_model()).expect("read the x86-64 model");
    check_hygiene(
        "CodegenX64Encoding.lean",
        &source,
        X64_NAMESPACE,
        &["VerifiedCodegen"],
        &X64_KEY_THEOREMS,
    );
}

#[test]
fn rust_x64_listing_covers_every_instruction_kind() {
    let listing = vesper::verified_codegen::x64_encoding_listing();
    assert_eq!(listing.lines().count(), 63);
    // add rax, rcx; jb ovf (the abort exits sit at offset 0)
    assert!(listing.contains("addchk u: 48 01 c8 0f 82 f7 ff ff ff\n"));
    // push rbp; mov rbp, rsp; test [rsp - 4096], esp; sub rsp, 8000
    assert!(
        listing.contains(
            "enter 1000 (windows): 55 48 89 e5 85 a4 24 00 f0 ff ff 48 81 ec 40 1f 00 00\n"
        )
    );
    assert!(listing.contains("leave (windows, frame 8000): 48 81 c4 40 1f 00 00 5d c3\n"));
}

#[test]
fn rust_rv64_listing_covers_every_instruction_kind() {
    let listing = vesper::verified_codegen::rv64_encoding_listing();
    assert_eq!(listing.lines().count(), 59);
    // add a0,a0,a1; bgeu a0,a1,+8; j ovf (the abort exits sit at offset 0)
    assert!(listing.contains("addchk u: 00b50533 00b57463 ff9ff06f\n"));
    assert!(listing.contains("leave: 00040113 00013403 00813083 01010113 00008067\n"));
}

#[test]
fn rust_a64_listing_covers_every_instruction_kind() {
    let listing = vesper::verified_codegen::a64_encoding_listing();
    assert_eq!(listing.lines().count(), 61);
    // adds x0, x0, x1; b.hs ovf (the abort exits sit at offset 0)
    assert!(listing.contains("addchk u: ab010000 54ffffe2\n"));
    // sub sp, sp, #1, lsl #12; sub sp, sp, #0xf40
    assert!(listing.contains("enter 1000: a9bf7bfd 910003fd d14007ff d13d03ff\n"));
    assert!(listing.contains("leave: 910003bf a8c17bfd d65f03c0\n"));
}

#[test]
fn rust_table_covers_every_row_kind() {
    let listing = vesper::verified_codegen::table_listing();
    assert_eq!(listing.lines().count(), 93);
    assert!(listing.contains("farith div: ld r0 s1; ld r1 s2; fop div; st s0 r0\n"));
    assert!(listing.contains("fcompare ne: ld r0 s1; ld r1 s2; fcmp ne; st s0 r0\n"));
    assert!(listing.contains("fneg: ld r0 s1; fneg; st s0 r0\n"));
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
    let dir = scratch("lean");
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

    let audited = audit_axioms(&stdout);
    for t in &names {
        assert!(
            audited.contains(&format!("{NAMESPACE}.{t}")),
            "{t} has no axiom report"
        );
    }
}

/// Compile an encoding model (which imports `VerifiedCodegen`) and check
/// its printed listing against the Rust emitter's and its axiom report.
fn check_encoding_model(
    lean: &str,
    model_path: PathBuf,
    file_name: &str,
    namespace: &str,
    tag: &str,
    rust_listing: String,
) {
    let source = std::fs::read_to_string(model_path).expect("read the encoding model");
    let names = theorems(&strip_comments(&source));
    let dir = scratch(&tag.to_lowercase());
    let base = dir.join("VerifiedCodegen.lean");
    std::fs::write(&base, std::fs::read_to_string(model()).unwrap()).unwrap();
    let file = dir.join(file_name);
    std::fs::write(&file, &source).unwrap();
    let olean = std::process::Command::new(lean)
        .current_dir(&dir)
        .arg("-o")
        .arg(dir.join("VerifiedCodegen.olean"))
        .arg(&base)
        .output()
        .expect("spawn Lean");
    assert!(
        olean.status.success(),
        "Lean failed on VerifiedCodegen.lean ({}):\n{}\n{}",
        olean.status,
        String::from_utf8_lossy(&olean.stdout),
        String::from_utf8_lossy(&olean.stderr)
    );
    let out = std::process::Command::new(lean)
        .current_dir(&dir)
        .env("LEAN_PATH", &dir)
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
    let words = stdout
        .split_once(&format!("BEGIN {tag}\n"))
        .and_then(|(_, rest)| rest.split_once(&format!("END {tag}\n")))
        .map(|(t, _)| t)
        .expect("the Lean file prints the listing");
    assert_eq!(
        words, rust_listing,
        "the Lean {tag} encoder differs from the Rust one"
    );
    let audited = audit_axioms(&stdout);
    for t in &names {
        assert!(
            audited.contains(&format!("{namespace}.{t}")),
            "{t} has no axiom report"
        );
    }
}

#[test]
fn lean_rv64_encoding_model_compiles_and_prints_the_rust_words() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation of CodegenRv64Encoding.lean skipped");
        return;
    };
    check_encoding_model(
        &lean,
        rv64_model(),
        "CodegenRv64Encoding.lean",
        RV64_NAMESPACE,
        "RV64",
        vesper::verified_codegen::rv64_encoding_listing(),
    );
}

#[test]
fn lean_a64_encoding_model_compiles_and_prints_the_rust_words() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation of CodegenA64Encoding.lean skipped");
        return;
    };
    check_encoding_model(
        &lean,
        a64_model(),
        "CodegenA64Encoding.lean",
        A64_NAMESPACE,
        "A64",
        vesper::verified_codegen::a64_encoding_listing(),
    );
}

#[test]
fn lean_x64_encoding_model_compiles_and_prints_the_rust_bytes() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean compilation of CodegenX64Encoding.lean skipped");
        return;
    };
    check_encoding_model(
        &lean,
        x64_model(),
        "CodegenX64Encoding.lean",
        X64_NAMESPACE,
        "X64",
        vesper::verified_codegen::x64_encoding_listing(),
    );
}
