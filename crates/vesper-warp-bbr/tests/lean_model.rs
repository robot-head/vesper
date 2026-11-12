//! Lean model checks for `lean/WarpBbr.lean`.
//!
//! The static audit always runs. With `VESPER_LEAN` set (as in CI), the
//! model is compiled (which kernel-checks every proof and prints the axiom
//! report) and its executable definitions are compared with the Rust
//! functions in `src/pacing.rs` on concrete inputs. Agreement on inputs is
//! a differential test, not a proof that the Rust code refines the model.

use std::path::{Path, PathBuf};
use vesper_warp_bbr::{SplitMix64, clamp_target_cwnd, edt_step, pacing_delay_ns, send_allowed};

const MODEL_ID: &str = "vesper.warp.bbr.pacing-model/1";
const MODEL_CLAIM: &str =
    "Lean transcription of pacing.rs; proofs cover the transcription, not Rust";
const AUDITED_THEOREMS: usize = 10;
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
    let source = std::fs::read_to_string(lean_dir().join("WarpBbr.lean")).expect("read model");
    assert!(source.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    assert!(source.contains(&format!("\"{MODEL_CLAIM}\"")));
    assert_eq!(source.matches("\n#print axioms ").count(), AUDITED_THEOREMS);
    for theorem in [
        "theorem bbrv3_edt_launch_monotonic",
        "theorem edtRun_ascending",
        "theorem pacingDelay_rounding",
        "theorem targetCwnd_bounds",
        "theorem send_respects_cwnd",
        "theorem sendTrace_bound",
        "theorem cut_blocks_sends",
        "theorem global_bound_fails",
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
        .arg(out.join("WarpBbr.olean"))
        .arg("WarpBbr.lean")
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
        .arg(lean_dir().join("WarpBbr/Fixture.lean"))
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

fn edge(rng: &mut SplitMix64) -> u64 {
    match rng.below(5) {
        0 => 0,
        1 => u64::MAX - rng.below(4),
        2 => rng.next_u64(),
        _ => rng.below(1 << 40),
    }
}

#[test]
fn lean_model_matches_rust_pacing_functions() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean model compilation and differential skipped");
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let out = std::env::temp_dir().join(format!(
        "vesper-warp-bbr-model-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&out).expect("private Lean output directory");
    let _cleanup = Cleanup(out.clone());
    compile_model(&lean, &out);

    let mut rng = SplitMix64(0x05ee_db0b);
    let mut input = String::new();
    let mut expected = Vec::new();
    let mut push = |line: String, want: String| {
        input.push_str(&line);
        input.push('\n');
        expected.push(want);
    };
    let bytes_edges = [0u32, 1, 1200, 1500, 65_535, u32::MAX];
    let rate_edges = [0u64, 1, 3, 999_999, 1_250_000, 12_500_000_000, u64::MAX];
    for &b in &bytes_edges {
        for &r in &rate_edges {
            push(format!("0 {b} {r}"), pacing_delay_ns(b, r).to_string());
        }
    }
    for _ in 0..400 {
        let b = if rng.below(2) == 0 {
            rng.next_u64() as u32
        } else {
            rng.below(65_536) as u32
        };
        let r = edge(&mut rng);
        push(format!("0 {b} {r}"), pacing_delay_ns(b, r).to_string());
        let (prev, now, delay) = (edge(&mut rng), edge(&mut rng), edge(&mut rng));
        let (launch, next) = edt_step(prev, now, delay);
        push(
            format!("1 {prev} {now} {delay}"),
            format!("{launch} {next}"),
        );
        let (cwnd, mss, hi) = (edge(&mut rng), rng.next_u64() as u32, edge(&mut rng));
        push(
            format!("2 {cwnd} {mss} {hi}"),
            clamp_target_cwnd(cwnd, mss, hi).to_string(),
        );
        let (inflight, cwnd) = (edge(&mut rng), edge(&mut rng));
        push(
            format!("3 {inflight} {cwnd}"),
            send_allowed(inflight, cwnd).to_string(),
        );
    }
    let got = run_fixture(&lean, &out, &input);
    assert_eq!(got.len(), expected.len(), "one result per case");
    for ((line, want), got) in input.lines().zip(&expected).zip(&got) {
        assert_eq!(got, want, "case `{line}`");
    }
}
