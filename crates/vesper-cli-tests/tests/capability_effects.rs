//! Source capability effects `ConstTime`, `Realtime` and `DetFloat`
//! (design §23.2, §32.2, §36.2): fixtures through `vesper check`, typed
//! diagnostics with spans, and the verifier policy inputs.
mod support;
use std::path::PathBuf;

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let dir = support::repo_root()
        .join("tests/fixtures/capability_effects")
        .join(kind);
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

#[test]
fn accepted_fixtures_check_and_run_on_both_backends() {
    let expected = [
        ("closures.ves", "6.5"),
        ("consttime.ves", "true"),
        ("detfloat.ves", "0.75"),
        ("realtime.ves", "42"),
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
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                value,
                "{backend} {p}"
            );
        }
    }
}

#[test]
fn fixtures_are_canonically_formatted() {
    let dir = support::repo_root().join("tests/fixtures/capability_effects");
    let out = vesper(&["fmt", "--check", dir.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
