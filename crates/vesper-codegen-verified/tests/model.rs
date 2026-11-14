//! `lean/BootstrapDdc.lean` (§42.4 theorems and the gate model), the Path B
//! reference `lean/BootstrapDdc/Reference.lean`, the bootstrap corpus and the
//! committed seed lock.
//!
//! Source hygiene and the seed lock are always checked. With `VESPER_LEAN` set
//! (as in CI) the theorems are kernel-checked with their axioms audited, and
//! the reference runs the corpus interpreted (`lean --run`) and, on Linux with
//! gcc, through Lean's C emitter compiled by gcc. The Lean theorems are about
//! the Lean models, not about the Rust gate.

use std::path::{Path, PathBuf};
use std::process::Command;
use vesper_codegen_verified::seed::{ResultTranscript, SeedLock};

const THEOREMS: [&str; 8] = [
    "end_to_end_verified_codegen_simulation",
    "ddc_eliminates_seed_trojan_on_compiler",
    "gate_accepts_only_fixed_point",
    "gate_rejects_stage_divergence",
    "gate_rejects_ddc_mismatch",
    "gate_rejects_missing_signature",
    "gate_accepts_identical_artifacts",
    "gate_eliminates_seed_trojan",
];
/// The lock's `allowed_lean_axioms`; the model needs no `Classical.choice`.
const ALLOWED_AXIOMS: [&str; 2] = ["propext", "Quot.sound"];

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
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

fn corpus() -> Vec<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(repo().join("bootstrap/corpus"))
        .expect("corpus directory")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ves"))
        .collect();
    files.sort();
    files
}

fn expected() -> ResultTranscript {
    ResultTranscript::parse(
        &std::fs::read(repo().join("bootstrap/corpus/expected-status.txt")).expect("expected"),
    )
    .expect("expected transcript")
}

#[test]
fn lean_sources_have_no_escape_hatches_and_audit_every_theorem() {
    let model = strip_comments(
        &std::fs::read_to_string(repo().join("lean/BootstrapDdc.lean")).expect("model"),
    );
    for banned in [
        "sorry",
        "admit",
        "axiom ",
        "unsafe",
        "partial ",
        "native_decide",
        "@[implemented_by",
        "@[extern",
        "import ",
    ] {
        assert!(
            !model.contains(banned),
            "BootstrapDdc.lean contains {banned:?}"
        );
    }
    for theorem in THEOREMS {
        assert!(
            model.contains(&format!("theorem {theorem}")),
            "{theorem} missing"
        );
        assert!(
            model.contains(&format!("#print axioms Vesper.BootstrapDdc.{theorem}")),
            "{theorem} not audited"
        );
    }
    let reference = strip_comments(
        &std::fs::read_to_string(repo().join("lean/BootstrapDdc/Reference.lean")).expect("ref"),
    );
    // Path B must not share code with the Rust seed: no imports, no FFI.
    for banned in [
        "sorry",
        "axiom ",
        "unsafe",
        "@[extern",
        "@[implemented_by",
        "import ",
    ] {
        assert!(
            !reference.contains(banned),
            "Reference.lean contains {banned:?}"
        );
    }
}

#[test]
fn corpus_and_expected_statuses_agree() {
    let names: Vec<String> = corpus()
        .iter()
        .map(|p| p.file_stem().unwrap().to_str().unwrap().to_owned())
        .collect();
    let expected = expected();
    assert!(names.len() >= 10, "corpus has {} programs", names.len());
    assert_eq!(
        expected.lines.iter().map(|l| &l.0).collect::<Vec<_>>(),
        names.iter().collect::<Vec<_>>()
    );
}

#[test]
fn committed_seed_lock_is_consistent() {
    let path = repo().join("bootstrap/stage0_seed.lock");
    let lock = SeedLock::parse(&std::fs::read(&path).expect("seed lock")).expect("valid");
    assert_eq!(lock.seed.corpus_programs as usize, corpus().len());
    assert!(lock.reproducibility.corpus_objects_identical);
    assert!(lock.ddc_semantic.results_agree);
    assert_eq!(
        lock.ddc_semantic.path_b_results,
        format!("blake3:{}", expected().digest().to_hex()),
        "the recorded Path B transcript is the committed expected one"
    );
}

fn lean() -> Option<String> {
    std::env::var("VESPER_LEAN").ok()
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("vesper-ddc-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("private output directory");
    dir
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn lean_theorems_check_with_standard_axioms_only() {
    let Some(lean) = lean() else {
        eprintln!("VESPER_LEAN unset; Lean kernel check skipped");
        return;
    };
    let out = Command::new(&lean)
        .current_dir(repo().join("lean"))
        .arg("BootstrapDdc.lean")
        .output()
        .expect("run lean");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "lean failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!stdout.contains("error"), "{stdout}");
    for theorem in THEOREMS {
        let name = format!("'Vesper.BootstrapDdc.{theorem}'");
        let line = stdout
            .split('\n')
            .collect::<Vec<_>>()
            .join(" ")
            .split(&name)
            .nth(1)
            .and_then(|rest| rest.split('\'').next().map(str::to_owned))
            .unwrap_or_else(|| panic!("{theorem} not reported:\n{stdout}"));
        if line.contains("does not depend on any axioms") {
            continue;
        }
        let list = line
            .split('[')
            .nth(1)
            .and_then(|l| l.split(']').next())
            .unwrap_or_else(|| panic!("{theorem}: {line}"));
        for axiom in list.split(',').map(str::trim) {
            assert!(ALLOWED_AXIOMS.contains(&axiom), "{theorem} uses {axiom}");
        }
    }
}

fn run_reference(cmd: &mut Command) -> ResultTranscript {
    let out = cmd.args(corpus()).output().expect("run Path B reference");
    assert!(
        out.status.success(),
        "reference failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    ResultTranscript::parse(&out.stdout).expect("reference transcript")
}

#[test]
fn path_b_reference_matches_expected_statuses_interpreted() {
    let Some(lean) = lean() else {
        eprintln!("VESPER_LEAN unset; Path B reference skipped");
        return;
    };
    let got = run_reference(
        Command::new(&lean)
            .current_dir(repo().join("lean"))
            .arg("--run")
            .arg("BootstrapDdc/Reference.lean"),
    );
    expected().require_agreement(&got).expect("Path B agrees");
}

fn gcc_is_gcc() -> bool {
    Command::new("gcc")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("clang"))
}

#[test]
fn path_b_reference_compiled_by_gcc_matches_expected_statuses() {
    let Some(lean) = lean() else {
        eprintln!("VESPER_LEAN unset; Path B C build skipped");
        return;
    };
    if !cfg!(target_os = "linux") || !gcc_is_gcc() {
        eprintln!("Path B C build needs Linux and GNU gcc; skipped");
        return;
    }
    let prefix = Command::new(&lean)
        .arg("--print-prefix")
        .output()
        .expect("prefix");
    let prefix = PathBuf::from(String::from_utf8_lossy(&prefix.stdout).trim());
    let dir = temp_dir("pathb");
    let _cleanup = Cleanup(dir.clone());
    let c = dir.join("reference.c");
    let status = Command::new(&lean)
        .current_dir(repo().join("lean"))
        .arg("-c")
        .arg(&c)
        .arg("BootstrapDdc/Reference.lean")
        .status()
        .expect("lean -c");
    assert!(status.success());
    let gcc = |args: &[&std::ffi::OsStr]| {
        let status = Command::new("gcc").args(args).status().expect("gcc");
        assert!(status.success(), "gcc {args:?}");
    };
    let include = prefix.join("include");
    let lib = prefix.join("lib/lean");
    let obj = dir.join("reference.o");
    let exe = dir.join("vesperc-lean-ddc");
    gcc(&[
        "-O2".as_ref(),
        format!("-I{}", include.display()).as_ref(),
        "-c".as_ref(),
        c.as_os_str(),
        "-o".as_ref(),
        obj.as_os_str(),
    ]);
    gcc(&[
        obj.as_os_str(),
        format!("-L{}", lib.display()).as_ref(),
        "-lleanshared".as_ref(),
        format!("-Wl,-rpath,{}", lib.display()).as_ref(),
        "-o".as_ref(),
        exe.as_os_str(),
    ]);
    let got = run_reference(&mut Command::new(Path::new(&exe)));
    expected().require_agreement(&got).expect("Path B agrees");
}

/// The full two-build reproducibility run: it builds the Stage 0 seed twice
/// from clean target directories, so it takes tens of minutes and needs Lean
/// and gcc; CI runs the fast checks above instead. Run with
/// `cargo test -p vesper-codegen-verified --test model -- --ignored`.
#[test]
#[ignore = "builds the Stage 0 compiler twice from clean target directories (tens of minutes); run bootstrap/run.sh"]
fn two_seed_builds_compile_the_corpus_bit_identically() {
    let dir = temp_dir("run");
    let _cleanup = Cleanup(dir.clone());
    let status = Command::new("bash")
        .arg(repo().join("bootstrap/run.sh"))
        .env("WORK", dir.join("work"))
        .env("OUT", dir.join("stage0_seed.lock"))
        .status()
        .expect("run bootstrap/run.sh");
    assert!(status.success(), "bootstrap/run.sh failed");
    SeedLock::parse(&std::fs::read(dir.join("stage0_seed.lock")).unwrap()).expect("seed lock");
}

#[test]
fn path_b_reference_follows_the_signed_corner_contract() {
    let Some(lean) = lean() else {
        eprintln!("VESPER_LEAN unset; Path B corner cases skipped");
        return;
    };
    // crates/vesper-core numeric contract: `i64::MIN % -1` is 0; `i64::MIN / -1`
    // overflows (here the handler's 9).
    let dir = temp_dir("corner");
    let _cleanup = Cleanup(dir.clone());
    let program = |body: &str| {
        format!(
            "fn main() -> i64 {{\n    handle Fail[ArithOverflow] in {{\n        {body}\n    }} with {{\n        return value => value,\n        raise(error, resume) => 9,\n    }}\n}}\n"
        )
    };
    std::fs::write(
        dir.join("rem.ves"),
        program("(-9223372036854775807 - 1) % -1"),
    )
    .unwrap();
    std::fs::write(
        dir.join("div.ves"),
        program("(-9223372036854775807 - 1) / -1"),
    )
    .unwrap();
    let out = Command::new(&lean)
        .current_dir(repo().join("lean"))
        .arg("--run")
        .arg("BootstrapDdc/Reference.lean")
        .arg(dir.join("div.ves"))
        .arg(dir.join("rem.ves"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "div 9\nrem 0\n");
}
