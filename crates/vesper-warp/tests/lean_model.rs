//! Differential test: the Rust decoder against the executable Lean model in
//! `lean/WarpPacketNumber.lean`, plus a source audit of that file.
//!
//! Agreement on these inputs is evidence about these inputs. The general
//! claims (congruence, 2^62 bound, window recovery) are Lean theorems about
//! the Lean model over `Nat`, not about this Rust code.

mod common;
use common::Rng;
use std::io::Write as _;
use std::path::PathBuf;
use vesper_warp::PN_LIMIT;
use vesper_warp::packet_number::decode_with_largest;

const MODEL_ID: &str = "vesper.warp.packet-number.a3-model/1";
const MODEL_CLAIM: &str =
    "RFC 9000 A.3 arithmetic over Nat only; not a proof about the Rust u64 code";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const AUDITED_THEOREMS: usize = 5;

fn lean_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean")
}

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
fn lean_source_audit() {
    let source = std::fs::read_to_string(lean_dir().join("WarpPacketNumber.lean")).unwrap();
    assert!(source.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    assert!(source.contains(&format!("\"{MODEL_CLAIM}\"")));
    assert_eq!(source.matches("\n#print axioms ").count(), AUDITED_THEOREMS);
    let code = strip_lean_comments(&source);
    for forbidden in [
        "sorry",
        "admit",
        "native_decide",
        "bv_decide",
        "ofReduceBool",
        "partial ",
        "unsafe ",
    ] {
        assert!(!code.contains(forbidden), "Lean model uses {forbidden}");
    }
    assert!(
        !code.lines().any(|l| l.trim_start().starts_with("axiom ")),
        "Lean model declares an axiom"
    );
}

fn cases() -> Vec<(Option<u64>, u64, u32)> {
    let mut rng = Rng(0x77a2_0401);
    let mut cases = Vec::new();
    for nbits in [8u32, 16, 24, 32] {
        let win = 1u64 << nbits;
        for largest in [
            None,
            Some(0),
            Some(win / 2 - 1),
            Some(win - 1),
            Some(PN_LIMIT - win),
            Some(PN_LIMIT - 2),
            Some(PN_LIMIT - 1),
        ] {
            for t in [0, 1, win / 2 - 1, win / 2, win / 2 + 1, win - 1] {
                cases.push((largest, t, nbits));
            }
        }
        for _ in 0..400 {
            cases.push((Some(rng.below(PN_LIMIT)), rng.below(win), nbits));
            cases.push((
                Some(PN_LIMIT - 1 - rng.below(win * 2)),
                rng.below(win),
                nbits,
            ));
        }
    }
    // Invalid widths and out-of-range inputs.
    cases.extend([
        (None, 0, 7),
        (None, 0, 64),
        (Some(5), 300, 8),
        (Some(PN_LIMIT), 0, 8),
    ]);
    cases.push((Some(0xa82f_30ea), 0x9b32, 16));
    cases
}

#[test]
fn rust_decoder_matches_executable_lean_model() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean executable-model differential skipped");
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let out_dir =
        std::env::temp_dir().join(format!("vesper-warp-pn-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&out_dir).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(out_dir.clone());
    let compiled = std::process::Command::new(&lean)
        .current_dir(lean_dir())
        .arg("-o")
        .arg(out_dir.join("WarpPacketNumber.olean"))
        .arg("WarpPacketNumber.lean")
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&compiled.stderr);
    assert!(
        compiled.status.success(),
        "Lean failed:\n{report}\n{stderr}"
    );
    let mut audited = 0;
    for line in report.lines() {
        let Some((_, rest)) = line.split_once(" depends on axioms: [") else {
            assert!(
                line.ends_with("does not depend on any axioms"),
                "unexpected Lean output: {line}"
            );
            audited += 1;
            continue;
        };
        for axiom in rest.trim_end_matches(']').split(", ") {
            assert!(STANDARD_AXIOMS.contains(&axiom), "unapproved axiom {axiom}");
        }
        audited += 1;
    }
    assert_eq!(audited, AUDITED_THEOREMS);

    let cases = cases();
    let mut input = String::new();
    for (largest, t, nbits) in &cases {
        match largest {
            None => input.push('-'),
            Some(l) => input.push_str(&l.to_string()),
        }
        input.push_str(&format!(" {t} {nbits}\n"));
    }
    let mut child = std::process::Command::new(&lean)
        .env("LEAN_PATH", &out_dir)
        .arg("--run")
        .arg(lean_dir().join("WarpPacketNumber/Fixture.lean"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn Lean fixture");
    // Feed stdin from a thread while wait_with_output drains stdout, so a
    // small pipe buffer cannot deadlock the fixture's line-by-line replies.
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    assert!(
        output.status.success(),
        "fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut lines = stdout.lines();
    assert_eq!(
        lines.next(),
        Some(format!("{MODEL_ID} | {MODEL_CLAIM}").as_str())
    );
    let mut compared = 0;
    for ((largest, t, nbits), line) in cases.iter().zip(lines) {
        let rust = decode_with_largest(*largest, *t, *nbits).ok();
        let lean = if line == "none" {
            None
        } else {
            Some(
                line.parse::<u64>()
                    .unwrap_or_else(|_| panic!("bad fixture line {line}")),
            )
        };
        assert_eq!(rust, lean, "largest={largest:?} t={t} nbits={nbits}");
        compared += 1;
    }
    assert_eq!(compared, cases.len());
}
