//! Lean model checks for `lean/CrucibleShrink.lean`.
//!
//! The static audit always runs. With `VESPER_LEAN` set (as in CI) the model
//! is compiled, which kernel-checks every proof, its axiom report is audited,
//! and the executable order `smallerB` is compared with
//! `ShrinkKey::smaller_than` on concrete tapes. Agreement on inputs is a
//! differential test, not a proof that the Rust shrinker refines the model.

use std::path::{Path, PathBuf};
use vesper_crucible::{ShrinkKey, SplitMix64};

const MODEL_ID: &str = "vesper.crucible.shrink-model/1";
const MODEL_CLAIM: &str = "Lean model of tape shrinking; proofs cover the model, not Rust";
const AUDITED_THEOREMS: usize = 8;
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];

fn lean_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean")
}

/// Remove `--` line comments and nested `/- -/` block comments.
fn strip_lean_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let (mut out, mut depth, mut i) = (Vec::new(), 0usize, 0);
    while i < bytes.len() {
        if bytes[i..].starts_with(b"/-") {
            depth += 1;
            i += 2;
        } else if depth > 0 && bytes[i..].starts_with(b"-/") {
            depth -= 1;
            i += 2;
        } else if depth == 0 && bytes[i..].starts_with(b"--") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else {
            if depth == 0 {
                out.push(bytes[i]);
            }
            i += 1;
        }
    }
    String::from_utf8(out).expect("UTF-8 Lean source")
}

#[test]
fn lean_model_source_audit() {
    let source =
        std::fs::read_to_string(lean_dir().join("CrucibleShrink.lean")).expect("read model");
    assert!(source.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    assert!(source.contains(&format!("\"{MODEL_CLAIM}\"")));
    assert_eq!(source.matches("\n#print axioms ").count(), AUDITED_THEOREMS);
    for item in [
        "theorem smaller_wf",
        "theorem choice_tape_shrink_terminates",
        "theorem smallerB_correct",
        "theorem delete_span_smaller",
        "theorem lower_pointwise_smaller",
        "theorem set_lower_smaller",
        "def shrink",
        "termination_by",
        "theorem shrink_preserves",
        "theorem shrink_le",
    ] {
        assert!(source.contains(item), "missing {item}");
    }
    let code = strip_lean_comments(&source);
    assert!(!code.contains("partial ") && !code.contains("unsafe "));
    for forbidden in [
        "sorry",
        "admit",
        "native_decide",
        "bv_decide",
        "ofReduceBool",
        "decreasing_trivial",
    ] {
        assert!(!code.contains(forbidden), "Lean model uses {forbidden}");
    }
    assert!(
        !code
            .lines()
            .any(|line| line.trim_start().starts_with("axiom ")),
        "Lean model declares an axiom"
    );
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn compile_model(lean: &str, out: &Path) {
    // Lean names the module after the path relative to its working
    // directory, so compile from `lean/`.
    let compiled = std::process::Command::new(lean)
        .current_dir(lean_dir())
        .arg("-o")
        .arg(out.join("CrucibleShrink.olean"))
        .arg("CrucibleShrink.lean")
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).expect("UTF-8 Lean output");
    assert!(
        compiled.status.success(),
        "Lean model failed: {}\n{report}\n{}",
        compiled.status,
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut audited = 0;
    for line in report.lines() {
        if line.ends_with(" does not depend on any axioms") {
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
    assert_eq!(audited, AUDITED_THEOREMS, "axiom report lines");
}

fn run_fixture(lean: &str, out: &Path, input: &str) -> Vec<String> {
    use std::io::Write as _;
    let mut child = std::process::Command::new(lean)
        .env("LEAN_PATH", out)
        .arg("--run")
        .arg(lean_dir().join("CrucibleShrink/Fixture.lean"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn Lean");
    let mut stdin = child.stdin.take().expect("stdin");
    let input = input.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().expect("wait for Lean");
    writer.join().expect("writer").expect("write stdin");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        output.status.success(),
        "Lean fixture failed: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = stdout.lines().map(str::to_owned);
    assert_eq!(
        lines.next().expect("header"),
        format!("{MODEL_ID} | {MODEL_CLAIM}")
    );
    lines.collect()
}

fn tape(rng: &mut SplitMix64) -> Vec<u64> {
    let len = rng.index(5);
    (0..len)
        .map(|_| match rng.index(4) {
            0 => 0,
            1 => u64::MAX - rng.below_inclusive(2),
            _ => rng.below_inclusive(6),
        })
        .collect()
}

fn render(values: &[u64]) -> String {
    values
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn lean_model_order_matches_rust_shrink_key() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean model compilation and differential skipped");
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let out = std::env::temp_dir().join(format!(
        "vesper-crucible-model-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&out).expect("private Lean output directory");
    let _cleanup = Cleanup(out.clone());
    compile_model(&lean, &out);

    let mut rng = SplitMix64(0x0c4c_1b1e);
    let mut input = String::new();
    let mut expected = Vec::new();
    for _ in 0..2_000 {
        let (a, b) = (tape(&mut rng), tape(&mut rng));
        input.push_str(&format!("{};{}\n", render(&a), render(&b)));
        expected.push(
            ShrinkKey::of(&a)
                .smaller_than(ShrinkKey::of(&b))
                .to_string(),
        );
    }
    let got = run_fixture(&lean, &out, &input);
    assert_eq!(got, expected);
}
