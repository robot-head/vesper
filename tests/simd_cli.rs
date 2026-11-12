use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-simd-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
            .arg("simd")
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn streamed_native_results_match_scalar_reference_at_chunk_and_vector_boundaries() {
    let f = Fixture::new();
    // On unsupported hosts, ensure the CLI rejects execution explicitly.
    if let Err(error) = vesper_simd::profile_support(vesper_simd::IsaProfileKind::Baseline128) {
        let output = f.run(&["scan", "input", "--delimiter", "0", "--json"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(&error.to_string()));
        assert!(output.stdout.is_empty());
        return;
    }
    for len in [0, 1, 15, 16, 17, 65535, 65536, 65537, 131089] {
        let bytes = (0..len)
            .map(|i| ((i * 131 + 7) % 256) as u8)
            .collect::<Vec<_>>();
        fs::write(f.0.join("input"), &bytes).unwrap();
        for delimiter in [0u8, 7, 255] {
            let output = f.run(&[
                "scan",
                "input",
                "--delimiter",
                &delimiter.to_string(),
                "--json",
            ]);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["schema_version"], 1);
            assert_eq!(value["execution"], "native_jit");
            assert_eq!(value["bytes"], len);
            assert_eq!(value["delimiter"], delimiter);
            assert_eq!(
                value["matches"],
                bytes.iter().filter(|&&b| b == delimiter).count()
            );
            assert_eq!(value["xor"], bytes.iter().fold(0u8, |acc, b| acc ^ b));
            assert_eq!(value["code"]["profile"], "baseline128");
            assert_eq!(value["code"]["vector_bits"], 128);
            assert_eq!(value["code"]["tail_strategy"], "scalar-checked-remainder");
            assert!(value["code"]["codegen_settings"].as_array().unwrap().len() > 10);
            assert_eq!(fs::read(f.0.join("input")).unwrap(), bytes);
        }
    }
}

#[test]
fn unsupported_profiles_fail_before_opening_input_and_never_report_success() {
    let f = Fixture::new();
    for profile in ["avx2", "avx512", "sve2", "rvv1"] {
        let output = f.run(&[
            "scan",
            "missing",
            "--delimiter",
            "0",
            "--profile",
            profile,
            "--json",
        ]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("is not implemented"));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn argument_errors_are_explicit_and_help_is_discoverable() {
    let f = Fixture::new();
    for args in [
        vec![],
        vec!["unknown"],
        vec!["scan"],
        vec!["scan", "input"],
        vec!["scan", "input", "--delimiter", "256"],
        vec!["scan", "input", "--delimiter", "-1"],
        vec!["scan", "input", "--delimiter", "+1"],
        vec!["scan", "input", "--delimiter", ""],
        vec!["scan", "input", "--delimiter", "1", "--delimiter", "2"],
        vec![
            "scan",
            "input",
            "--profile",
            "avx2",
            "--profile",
            "baseline128",
        ],
        vec!["scan", "input", "--json", "--json"],
        vec!["scan", "input", "extra", "--delimiter", "1"],
        vec!["scan", "input", "--unknown"],
    ] {
        let output = f.run(&args);
        assert!(!output.status.success(), "unexpected success: {args:?}");
        assert!(output.stdout.is_empty());
    }
    let help = f.run(&["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Baseline128"));
}

#[test]
fn text_output_dash_filename_and_read_errors() {
    if vesper_simd::profile_support(vesper_simd::IsaProfileKind::Baseline128).is_err() {
        return;
    }
    let f = Fixture::new();
    fs::write(f.0.join("-input"), [1, 2, 1]).unwrap();
    let output = f.run(&["scan", "--delimiter", "1", "--", "-input"]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"bytes=3 delimiter=1 matches=2 xor=2\n");
    for input in ["missing", "."] {
        let output = f.run(&["scan", input, "--delimiter", "0", "--json"]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}
