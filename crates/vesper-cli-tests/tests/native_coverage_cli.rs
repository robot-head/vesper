//! Lane 23 (#265): native edge counters and comparison-operand logging in
//! the Cranelift backend, feeding `vesper fuzz --native-coverage`.
mod support;
use std::{
    fs,
    path::PathBuf,
    process::Output,
    sync::atomic::{AtomicU64, Ordering},
};

fn fixture() -> PathBuf {
    support::repo_root().join("tests/fixtures/native_coverage/magic.ves")
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-native-coverage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vesper(args: &[&str]) -> Output {
    support::vesper().args(args).output().unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn planted_magic_value_is_found_only_with_native_coverage() {
    let file = fixture();
    let file = file.to_str().unwrap();
    for (target, call) in [
        ("unlock", "unlock(21715677, 65536)"),
        ("gate", "gate(-81000003, "),
    ] {
        let o = vesper(&[
            "fuzz",
            file,
            "--target",
            target,
            "--native-coverage",
            "--execs",
            "2000",
        ]);
        let t = text(&o);
        assert!(!o.status.success(), "{t}");
        assert!(t.contains("native coverage: "), "{t}");
        assert!(t.contains("counterexample (execution "), "{t}");
        assert!(t.contains(call), "{t}");
        assert!(t.contains("failure [native Fail[ArithOverflow]]"), "{t}");
        // The same budget without native coverage misses it, guided by the
        // evaluator's coarse feedback and source literals, or at random.
        for mode in [None, Some("--random")] {
            let mut args = vec!["fuzz", file, "--target", target, "--execs", "2000"];
            args.extend(mode);
            let o = vesper(&args);
            let t = text(&o);
            assert!(o.status.success(), "{mode:?}: {t}");
            assert!(t.contains("no failure found within budget"), "{t}");
        }
    }
}

#[test]
fn native_coverage_writes_regression_cases_and_saves_the_corpus() {
    let temp = Temp::new();
    let regressions = temp.path("regressions.ves");
    let corpus = temp.path("corpus");
    let file = fixture();
    let file = file.to_str().unwrap();
    let run = || {
        vesper(&[
            "fuzz",
            file,
            "--target",
            "unlock",
            "--native-coverage",
            "--regressions",
            &regressions,
            "--corpus",
            &corpus,
        ])
    };
    let o = run();
    let t = text(&o);
    assert!(!o.status.success(), "{t}");
    assert!(t.contains("wrote 1 regression case(s)"), "{t}");
    let written = fs::read_to_string(&regressions).unwrap();
    assert!(
        written.contains("// @test regression: vesper fuzz [native Fail[ArithOverflow]]"),
        "{written}"
    );
    assert!(
        written.contains("fn unlock_regression_1() -> u32 eff [Fail[ArithOverflow]] {"),
        "{written}"
    );
    assert!(written.contains("unlock(21715677, 65536)"), "{written}");
    let o = vesper(&["check", &regressions]);
    assert!(o.status.success(), "{}", text(&o));
    let o = vesper(&["fmt", "--check", &regressions]);
    assert!(o.status.success(), "{}", text(&o));
    // The saved failing tape is replayed first on the next run.
    let o = run();
    let t = text(&o);
    assert!(t.contains("counterexample (corpus "), "{t}");
    let written = fs::read_to_string(&regressions).unwrap();
    assert!(written.contains("fn unlock_regression_2()"), "{written}");
}
