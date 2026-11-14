//! `vesper-bootstrap seed-lock` refuses to certify a corpus its result
//! transcripts do not cover.

use std::path::Path;
use std::process::Command;

fn temp_dir() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "vesper-bootstrap-cli-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&dir).unwrap();
    dir
}

fn run(dir: &Path, results_a: &str, results_b: &str) -> std::process::ExitStatus {
    for side in ["a", "b"] {
        let objects = dir.join(format!("objects-{side}"));
        std::fs::create_dir_all(&objects).unwrap();
        std::fs::write(objects.join("fib.o"), b"object fib").unwrap();
        std::fs::write(objects.join("gcd.o"), b"object gcd").unwrap();
        std::fs::write(dir.join(format!("seed-{side}")), b"seed").unwrap();
    }
    std::fs::write(dir.join("results-a.txt"), results_a).unwrap();
    std::fs::write(dir.join("results-b.txt"), results_b).unwrap();
    let _ = std::fs::remove_file(dir.join("lock"));
    let p = |name: &str| dir.join(name);
    Command::new(env!("CARGO_BIN_EXE_vesper-bootstrap"))
        .arg("seed-lock")
        .args(["--base-commit", &"0".repeat(40)])
        .args(["--rustc", "rustc test", "--profile", "dev", "--epoch", "1"])
        .arg("--seed-a")
        .arg(p("seed-a"))
        .arg("--seed-b")
        .arg(p("seed-b"))
        .arg("--objects-a")
        .arg(p("objects-a"))
        .arg("--objects-b")
        .arg(p("objects-b"))
        .arg("--results-a")
        .arg(p("results-a.txt"))
        .arg("--results-b")
        .arg(p("results-b.txt"))
        .args(["--lean", "Lean test", "--cc", "gcc test"])
        .arg("--out")
        .arg(p("lock"))
        .output()
        .unwrap()
        .status
}

#[test]
fn seed_lock_requires_transcripts_covering_the_compiled_programs() {
    let dir = temp_dir();
    assert!(run(&dir, "fib 1\ngcd 2\n", "fib 1\ngcd 2\n").success());
    assert!(dir.join("lock").exists());
    for (a, b) in [
        ("", ""),
        ("fib 1\n", "fib 1\n"),
        ("fib 1\nlcm 2\n", "fib 1\nlcm 2\n"),
    ] {
        assert!(!run(&dir, a, b).success(), "{a:?}");
        assert!(!dir.join("lock").exists(), "no lock for {a:?}");
    }
    assert!(!run(&dir, "fib 1\ngcd 2\n", "fib 1\ngcd 3\n").success());
    let _ = std::fs::remove_dir_all(&dir);
}
