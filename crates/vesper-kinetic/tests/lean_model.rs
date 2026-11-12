//! Lean model checks for `lean/KineticColoring.lean`.
//!
//! The static audit always runs. With `VESPER_LEAN` set (as in CI), the model
//! is compiled (which kernel-checks every proof and prints the axiom report)
//! and its executable definitions are compared with the Rust Morton encoder
//! and colourer on concrete inputs. Agreement on inputs is a differential
//! test, not a proof that the Rust code refines the model.

use std::path::{Path, PathBuf};
use vesper_kinetic::morton::{decode_masked, encode_masked};
use vesper_kinetic::{Colorer, ConstraintBodies};

const MODEL_ID: &str = "vesper.kinetic.coloring-morton-model/1";
const MODEL_CLAIM: &str =
    "Lean model of greedy colouring and Morton codes; proofs cover the model, not Rust";
const AUDITED_THEOREMS: usize = 7;
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
    String::from_utf8(out).expect("ASCII-delimited comments")
}

#[test]
fn lean_model_source_audit() {
    let source =
        std::fs::read_to_string(lean_dir().join("KineticColoring.lean")).expect("read model");
    assert!(source.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    assert!(source.contains(&format!("\"{MODEL_CLAIM}\"")));
    assert_eq!(source.matches("\n#print axioms ").count(), AUDITED_THEOREMS);
    for theorem in [
        "theorem firstFree_not_mem",
        "theorem colorAll_pairwise_disjoint",
        "theorem colorAll_constraints",
        "theorem same_color_constraints_disjoint_bodies",
        "theorem morton_deinterleave_interleave",
        "theorem morton_10bit_roundtrip",
        "theorem morton_10bit_injective",
        "theorem interleave_lt",
    ] {
        assert!(source.contains(theorem), "missing {theorem}");
    }
    let code = strip_lean_comments(&source);
    assert!(!code.contains("partial ") && !code.contains("unsafe "));
    for forbidden in [
        "sorry",
        "admit",
        "native_decide",
        "bv_decide",
        "ofReduceBool",
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
        .arg(out.join("KineticColoring.olean"))
        .arg("KineticColoring.lean")
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).expect("UTF-8 Lean output");
    let stderr = String::from_utf8_lossy(&compiled.stderr);
    assert!(
        compiled.status.success(),
        "Lean model failed: {}\n{report}\n{stderr}",
        compiled.status
    );
    let mut audited = 0;
    for line in report.lines() {
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
        .arg(lean_dir().join("KineticColoring/Fixture.lean"))
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

struct SplitMix(u64);
impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[test]
fn lean_model_matches_rust_morton_and_colouring() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean model compilation and differential skipped");
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let out = std::env::temp_dir().join(format!(
        "vesper-kinetic-model-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&out).expect("private Lean output directory");
    let _cleanup = Cleanup(out.clone());
    compile_model(&lean, &out);

    let mut rng = SplitMix(0x4b1e);
    let mut input = String::new();
    let mut expected = Vec::new();
    let mut push = |line: String, want: String| {
        input.push_str(&line);
        input.push('\n');
        expected.push(want);
    };
    let mut morton_cases: Vec<(u16, u16, u16)> = (0..=1023u16)
        .step_by(31)
        .map(|v| (v, 1023 - v, v / 3))
        .collect();
    morton_cases.extend([
        (0, 0, 0),
        (1023, 1023, 1023),
        (1, 0, 0),
        (0, 1, 0),
        (0, 0, 1),
    ]);
    morton_cases.extend((0..300).map(|_| {
        let r = rng.next();
        (
            (r & 0x3ff) as u16,
            ((r >> 10) & 0x3ff) as u16,
            ((r >> 20) & 0x3ff) as u16,
        )
    }));
    for (x, y, z) in morton_cases {
        let code = encode_masked(x, y, z);
        let (a, b, c) = decode_masked(code);
        push(format!("0 {x} {y} {z}"), format!("{code} {a} {b} {c}"));
    }
    for round in 0..150 {
        let bodies = 2 + (rng.next() % 24) as u32;
        let count = (rng.next() % 40) as usize;
        let cs: Vec<ConstraintBodies> = (0..count)
            .map(|_| {
                let a = (rng.next() % u64::from(bodies)) as u32;
                let b = (rng.next() % u64::from(bodies)) as u32;
                ConstraintBodies {
                    a,
                    b: (a != b).then_some(b),
                }
            })
            .collect();
        let mut colorer = Colorer::new(bodies as usize, cs.len());
        colorer
            .color(&cs)
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
        // Feed the Rust visiting order to the model, which colours in list
        // order; it must assign the same colour to each constraint.
        let order = colorer.visiting_order();
        let mut line = String::from("1");
        let mut want = Vec::new();
        for &i in order {
            let c = cs[i as usize];
            line.push_str(&format!(" {} {}", c.a, c.b.unwrap_or(c.a)));
            want.push(colorer.color_of(i as usize).to_string());
        }
        push(line, want.join(" "));
    }
    push("2 1".into(), "error".into());
    push("1 3".into(), "error".into());
    let got = run_fixture(&lean, &out, &input);
    assert_eq!(got.len(), expected.len(), "one answer per case");
    for (n, (g, w)) in got.iter().zip(&expected).enumerate() {
        assert_eq!(g, w, "case {n}: {}", input.lines().nth(n).unwrap());
    }
}
