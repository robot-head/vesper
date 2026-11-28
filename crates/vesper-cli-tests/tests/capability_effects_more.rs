//! Wave-seven capability effects (design §23.2, §32.1-§32.2, §36): the
//! built-in secret operations and their bitwise intrinsics, `ConstDenorm`,
//! `RtBus`, interprocedural secret-parameter inference, taint through
//! handler resumes, the policy inputs, and the Lean model of the taint rules.
mod support;
use std::path::PathBuf;
use std::process::Command;

fn dir(kind: &str) -> PathBuf {
    support::repo_root()
        .join("tests/fixtures/capability_effects/more")
        .join(kind)
}

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let dir = dir(kind);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "ves"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no fixtures in {}", dir.display());
    files
}

fn vesper(args: &[&str]) -> std::process::Output {
    support::vesper().args(args).output().unwrap()
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn accepted_fixtures_check_and_agree_on_both_backends() {
    let expected = [
        ("bit_intrinsics.ves", "534"),
        ("const_denorm.ves", "1"),
        ("inferred_params.ves", "71"),
        ("resume_public.ves", "42"),
        ("rtbus.ves", "64"),
        ("secret_ops.ves", "1"),
    ];
    let files = fixtures("accept");
    assert_eq!(files.len(), expected.len());
    for (path, (name, value)) in files.iter().zip(expected) {
        assert!(path.ends_with(name), "{}", path.display());
        let p = path.to_str().unwrap();
        let out = vesper(&["check", p]);
        assert!(
            out.status.success(),
            "{p}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        for backend in ["run", "jit"] {
            let out = vesper(&[backend, p]);
            assert!(
                out.status.success(),
                "{backend} {p}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(stdout(&out), value, "{backend} {p}");
        }
    }
}

#[test]
fn fixtures_are_canonically_formatted() {
    let out = vesper(&["fmt", "--check", dir("").to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn secret_operations_pass_the_constant_time_verifier() {
    let path = dir("accept").join("secret_ops.ves");
    let out_path = std::env::temp_dir().join(format!("vesper-ct-ops-{}.o", std::process::id()));
    let out = vesper(&[
        "emit",
        path.to_str().unwrap(),
        "--emit",
        "object",
        "-o",
        out_path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&out_path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("const-time"), "{stderr}");
    // Lean extraction has no bitwise operators and rejects them by name.
    let out = vesper(&["emit", path.to_str().unwrap(), "--emit", "lean"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("bit_"));
}

#[test]
fn lean_model_checks_without_axioms_or_sorry() {
    let path = support::repo_root().join("lean/CapabilityEffects.lean");
    let text = std::fs::read_to_string(&path).unwrap();
    for word in ["sorry", "admit", "axiom "] {
        assert!(!text.contains(word), "{word} in {}", path.display());
    }
    for theorem in [
        "theorem noninterference",
        "theorem ok_mono",
        "theorem call_ok_of_summary",
    ] {
        assert!(text.contains(theorem), "{theorem}");
    }
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("Lean check skipped: set VESPER_LEAN to the pinned Lean 4.35.0 binary");
        return;
    };
    let out = Command::new(lean).arg(&path).output().unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{all}");
    assert!(!all.contains("declaration uses 'sorry'"), "{all}");
}
