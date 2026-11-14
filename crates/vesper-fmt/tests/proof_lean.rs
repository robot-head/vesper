//! Pure AST/budget checks always run. Real Lean elaboration runs when the
//! explicit VESPER_LEAN toolchain is configured (required in proof/workspace CI).
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use vesper_fmt::{Limits, ProofError, ProofLimits, format_for_verification};

fn artifact(source: &str) -> vesper_fmt::FormatVerification {
    format_for_verification(source, Limits::default(), ProofLimits::default()).unwrap()
}

#[test]
fn exact_source_and_formatted_hashes_are_metadata_for_the_actual_doc() {
    let source = "// first 🦀\r\nfn main( )->i64{/*second*/42}";
    let value = artifact(source);
    assert!(value.changed());
    assert_eq!(
        value.source_hash(),
        *blake3::hash(source.as_bytes()).as_bytes()
    );
    assert_eq!(
        value.output_hash(),
        *blake3::hash(value.output().as_bytes()).as_bytes()
    );
    assert_ne!(value.source_hash(), value.output_hash());
    assert_eq!(value.model().comments(), &["// first 🦀\r", "/*second*/"]);
    let lean = value.emit_lean(ProofLimits::default()).unwrap();
    assert!(lean.starts_with("import «StylusAndLintSafety»\n"));
    assert!(lean.contains("«Vesper».«StylusGenerated»"));
    assert!(lean.contains("«comments_preserved»"));
    assert!(lean.contains("«comments_render_invariant»"));
    assert!(lean.contains("noncomputable def «document»"));
    assert!(lean.contains("def «source_blake3»"));
    assert!(lean.contains("def «output_blake3»"));
    assert!(!lean.contains("noncomputable def «source_blake3»"));
    assert!(!lean.contains("noncomputable def «output_blake3»"));
    assert!(lean.contains(&blake3::hash(source.as_bytes()).to_hex().to_string()));
    assert!(lean.contains(&blake3::hash(value.output().as_bytes()).to_hex().to_string()));
}

#[test]
fn macro_projection_keeps_actual_comment_order_without_expanding() {
    let source = "/*definition*/macro keep($x:expr)->expr{quote{/*quoted*/$x}}fn main()->i64{/*caller*/keep!(42)}";
    let value = artifact(source);
    assert_eq!(
        value.model().comments(),
        &["/*definition*/", "/*quoted*/", "/*caller*/"]
    );
    assert!(value.output().contains("keep!(42)"));
    let lean = value.emit_lean(ProofLimits::default()).unwrap();
    let a = lean.find("/*definition*/").unwrap();
    let b = lean.find("/*quoted*/").unwrap();
    let c = lean.find("/*caller*/").unwrap();
    assert!(a < b && b < c);
}

#[test]
fn hostile_lean_text_and_controls_remain_string_payloads() {
    let source = "/* \" \\ -/\nend Vesper.StylusGenerated\naxiom injected : False\n\u{0001}\u{2028} 🦀 */fn main()->i64{42}";
    let value = artifact(source);
    let lean = value.emit_lean(ProofLimits::default()).unwrap();
    assert!(lean.contains("\\\""));
    assert!(lean.contains("\\\\"));
    assert!(lean.contains("\\nend Vesper.StylusGenerated\\naxiom injected : False\\n"));
    assert!(lean.contains("\\x01\\u2028"));
    assert!(!lean.contains('\u{0001}'));
    assert!(
        !lean
            .lines()
            .any(|line| line.trim_start().starts_with("axiom injected"))
    );
}

#[test]
fn every_tighter_emission_guard_refuses_without_a_partial_module() {
    let value = artifact("/*comment*/fn main()->i64{42}");
    for limits in [
        ProofLimits {
            max_nodes: 1,
            ..ProofLimits::default()
        },
        ProofLimits {
            max_text_bytes: 1,
            ..ProofLimits::default()
        },
        ProofLimits {
            max_depth: 1,
            ..ProofLimits::default()
        },
        ProofLimits {
            max_metadata_bytes: 1,
            ..ProofLimits::default()
        },
        ProofLimits {
            max_lean_bytes: 1,
            ..ProofLimits::default()
        },
        ProofLimits {
            max_work: 1,
            ..ProofLimits::default()
        },
    ] {
        assert!(matches!(
            value.emit_lean(limits),
            Err(ProofError::Resource(_))
        ));
    }
    assert_eq!(
        value
            .emit_lean(ProofLimits {
                max_work: 0,
                ..ProofLimits::default()
            })
            .unwrap_err(),
        ProofError::InvalidLimits
    );
}

#[test]
fn escaped_printer_temporaries_are_charged_as_metadata_before_emission() {
    let source = ["/*", &"\u{0001}".repeat(16 * 1024), "*/fn main()->i64{42}"].concat();
    let value = artifact(&source);
    assert!(matches!(
        value.emit_lean(ProofLimits {
            max_metadata_bytes: 64 * 1024,
            ..ProofLimits::default()
        }),
        Err(ProofError::Resource("emitter metadata"))
    ));
}

#[test]
fn damaged_and_unknown_macro_source_cannot_manufacture_a_source_artifact() {
    for source in ["fn main(){let x=;}", "fn main(){unknown!{token}}"] {
        assert!(
            format_for_verification(source, Limits::default(), ProofLimits::default()).is_err()
        );
    }
    // Syntactic document evidence is deliberately not semantic acceptance.
    assert!(
        format_for_verification(
            "linear struct Ticket{n:i64} fn main(){let t=Ticket{n:1};}",
            Limits::default(),
            ProofLimits::default(),
        )
        .is_ok()
    );
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-proof-emitter-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn command_output(mut command: Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let read = |pipe: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.take(1024 * 1024 + 1).read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = read(Box::new(out));
    let stderr = read(Box::new(err));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(45) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("configured Lean timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(stdout.len() <= 1024 * 1024 && stderr.len() <= 1024 * 1024);
    Output {
        status,
        stdout,
        stderr,
    }
}
fn compile(lean: &Path, directory: &Path, module: &str) -> Output {
    let mut command = Command::new(lean);
    command
        .env_clear()
        .env("LEAN_PATH", directory)
        .current_dir(directory);
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .args(["-j1", "-M1024", "-o"])
        .arg(directory.join(module).with_extension("olean"))
        .arg(directory.join(module).with_extension("lean"));
    command_output(command)
}

#[test]
fn configured_lean_elaborates_real_ast_witnesses_and_rejects_corruption() {
    let Some(launcher) = std::env::var_os("VESPER_LEAN") else {
        eprintln!(
            "VESPER_LEAN not configured: pure AST/budget tests ran; real elaboration is required in proof/workspace CI"
        );
        return;
    };
    let mut prefix = Command::new(launcher);
    prefix.arg("--print-prefix");
    let prefix = command_output(prefix);
    assert!(prefix.status.success());
    let prefix = PathBuf::from(String::from_utf8(prefix.stdout).unwrap().trim());
    let lean = prefix
        .join("bin")
        .join(if cfg!(windows) { "lean.exe" } else { "lean" });
    let mut version = Command::new(&lean);
    version.arg("--version");
    let version = command_output(version);
    assert!(version.status.success());
    assert!(
        String::from_utf8(version.stdout)
            .unwrap()
            .contains("version 4.24.0,")
    );
    let scratch = Scratch::new();
    fs::write(
        scratch.0.join("StylusAndLintSafety.lean"),
        include_str!("../../../lean/StylusAndLintSafety.lean"),
    )
    .unwrap();
    let result = compile(&lean, &scratch.0, "StylusAndLintSafety");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    for source in [
        "// first 🦀\r\nfn main( )->i64{/*second*/42}",
        "/* \" \\ -/\nend Vesper.StylusGenerated\naxiom injected : False\n\u{0001}\u{2028} 🦀 */fn main()->i64{42}",
        "macro keep($x:expr)->expr{quote{/*literal*/$x}}fn main()->i64{keep!(42)}",
    ] {
        let witness = artifact(source).emit_lean(ProofLimits::default()).unwrap();
        fs::write(scratch.0.join("Witness.lean"), &witness).unwrap();
        let result = compile(&lean, &scratch.0, "Witness");
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        fs::write(
            scratch.0.join("Corrupt.lean"),
            witness.replace("«comments_render_invariant»", "«missing_theorem»"),
        )
        .unwrap();
        let result = compile(&lean, &scratch.0, "Corrupt");
        assert!(
            !result.status.success(),
            "corrupt theorem reference unexpectedly elaborated"
        );
    }
}
