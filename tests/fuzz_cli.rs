//! Integration B (#136): `vesper fuzz` and `vesper verify --refute` over
//! source functions with Vesper Crucible.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

fn fixture() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/integration_b/fuzz.ves")
        .to_string_lossy()
        .into_owned()
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-fuzz-cli-{}-{}",
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
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn fuzz_finds_shrinks_and_writes_an_overflow_regression() {
    let temp = Temp::new();
    let regressions = temp.path("regressions.ves");
    let file = fixture();
    let o = vesper(&[
        "fuzz",
        &file,
        "--target",
        "midpoint",
        "--regressions",
        &regressions,
    ]);
    let t = text(&o);
    assert!(!o.status.success(), "{t}");
    assert!(
        t.contains("fuzz: counterexample found for 'midpoint'"),
        "{t}"
    );
    assert!(t.contains("failure [evaluator: "), "{t}");
    // Shrinking reaches the (len, sum) minimum of an overflowing pair.
    let line = t.lines().find(|l| l.starts_with("counterexample")).unwrap();
    let args = line
        .split("midpoint(")
        .nth(1)
        .unwrap()
        .trim_end_matches(')');
    let sum: u32 = args.split(", ").map(|a| a.parse::<u32>().unwrap()).sum();
    assert_eq!(sum, 256, "{line}");
    let written = fs::read_to_string(&regressions).unwrap();
    assert!(written.starts_with(&fs::read_to_string(&file).unwrap().trim_end().to_string()));
    assert!(
        written.contains("// @test regression: vesper fuzz [evaluator: "),
        "{written}"
    );
    assert!(
        written.contains("fn midpoint_regression_1() -> u8 eff [Fail[ArithOverflow]] {"),
        "{written}"
    );
    let o = vesper(&["check", &regressions]);
    assert!(o.status.success(), "{}", text(&o));
    let o = vesper(&["fmt", "--check", &regressions]);
    assert!(o.status.success(), "{}", text(&o));
    // A second campaign appends a new case instead of reusing the name.
    let o = vesper(&[
        "fuzz",
        &file,
        "--target",
        "midpoint",
        "--regressions",
        &regressions,
    ]);
    assert!(!o.status.success());
    let written = fs::read_to_string(&regressions).unwrap();
    assert!(written.contains("fn midpoint_regression_2()"), "{written}");
}

#[test]
fn fuzz_reports_no_failure_within_budget_deterministically() {
    let file = fixture();
    let run = || {
        vesper(&[
            "fuzz",
            &file,
            "--target",
            "midpoint_safe",
            "--execs",
            "1500",
            "--seed",
            "7",
        ])
    };
    let a = run();
    assert!(a.status.success(), "{}", text(&a));
    assert!(
        text(&a).contains("fuzz midpoint_safe: 1500 execution(s)"),
        "{}",
        text(&a)
    );
    assert!(text(&a).contains("no failure found within budget"));
    assert_eq!(a.stdout, run().stdout);
}

#[test]
fn differential_fuzzing_finds_the_magic_divergence() {
    let file = fixture();
    let o = vesper(&[
        "fuzz",
        &file,
        "--target",
        "midpoint_drift",
        "--differential",
        "midpoint_safe",
    ]);
    let t = text(&o);
    assert!(!o.status.success(), "{t}");
    assert!(
        t.contains("counterexample") && t.contains("midpoint_drift(200, 200)"),
        "{t}"
    );
    assert!(
        t.contains(
            "failure [divergence]: midpoint_drift returned 0, shadow midpoint_safe returned 200"
        ),
        "{t}"
    );
    assert!(t.contains("isolation unavailable"), "{t}");
    // The same pair agrees when shadowed by itself.
    let o = vesper(&[
        "fuzz",
        &file,
        "--target",
        "midpoint_safe",
        "--differential",
        "midpoint_safe",
        "--execs",
        "300",
    ]);
    assert!(o.status.success(), "{}", text(&o));
    let o = vesper(&[
        "fuzz",
        &file,
        "--target",
        "midpoint",
        "--differential",
        "clamp",
    ]);
    assert!(text(&o).contains("must have the same parameter and result types"));
}

#[test]
fn native_fuzzing_compares_the_jit_with_the_evaluator() {
    let file = fixture();
    let o = vesper(&[
        "fuzz", &file, "--target", "clamp", "--native", "--execs", "60",
    ]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("fuzz clamp: 60 execution(s)"), "{t}");
    let o = vesper(&["fuzz", &file, "--target", "midpoint", "--native"]);
    assert!(
        text(&o).contains("--native supports effect-free targets"),
        "{}",
        text(&o)
    );
}

#[test]
fn corpus_tapes_are_saved_and_replayed_first() {
    let temp = Temp::new();
    let corpus = temp.path("corpus");
    let file = fixture();
    let o = vesper(&[
        "fuzz", &file, "--target", "clamp", "--execs", "200", "--corpus", &corpus,
    ]);
    assert!(o.status.success(), "{}", text(&o));
    let saved = fs::read_dir(&corpus).unwrap().count();
    assert!(saved > 1, "{saved}");
    let o = vesper(&[
        "fuzz", &file, "--target", "clamp", "--execs", "50", "--corpus", &corpus,
    ]);
    assert!(
        text(&o).contains(&format!("{saved} corpus tape(s) replayed")),
        "{}",
        text(&o)
    );
    // A failing saved tape is reported (and shrunk) before any new search.
    let failing = temp.path("failing");
    fs::create_dir_all(&failing).unwrap();
    fs::write(
        Path::new(&failing).join("overflow.tape"),
        "vesper-crucible-tape/1\nff\nff\n",
    )
    .unwrap();
    let o = vesper(&["fuzz", &file, "--target", "midpoint", "--corpus", &failing]);
    let t = text(&o);
    assert!(!o.status.success());
    assert!(t.contains("counterexample (corpus "), "{t}");
    fs::write(Path::new(&failing).join("bad.tape"), "not a tape\n").unwrap();
    let o = vesper(&["fuzz", &file, "--target", "midpoint", "--corpus", &failing]);
    assert!(!o.status.success());
    assert!(text(&o).contains("bad.tape"), "{}", text(&o));
}

#[test]
fn fuzz_rejects_unsupported_targets_with_typed_messages() {
    let file = fixture();
    for (args, message) in [
        (vec!["fuzz", file.as_str()], "fuzz requires --target <fn>"),
        (
            vec!["fuzz", file.as_str(), "--target", "nope"],
            "fuzz target 'nope' is not a function of the module",
        ),
        (
            vec!["fuzz", file.as_str(), "--target", "takes_tuple"],
            "parameter 'pair' has type",
        ),
        (
            vec!["fuzz", file.as_str(), "--target", "clamp", "--execs", "0"],
            "--execs must be between 1 and 10000000",
        ),
        (
            vec!["fuzz", file.as_str(), "--bogus"],
            "unknown fuzz option '--bogus'",
        ),
        (vec!["fuzz", "missing.ves", "--target", "x"], "missing.ves"),
    ] {
        let o = vesper(&args);
        assert!(!o.status.success(), "{args:?}");
        assert!(text(&o).contains(message), "{args:?}: {}", text(&o));
    }
}

#[test]
fn refute_finds_a_magic_counterexample_and_materializes_it() {
    let temp = Temp::new();
    let regressions = temp.path("refuted.ves");
    let file = fixture();
    let o = vesper(&[
        "verify",
        "--refute",
        &file,
        "--contract",
        "not_magic",
        "--regressions",
        &regressions,
    ]);
    let t = text(&o);
    assert!(!o.status.success(), "{t}");
    assert!(
        t.contains("counterexample not_magic(4660, true) is false"),
        "{t}"
    );
    assert!(
        t.contains("verify --refute: contract 'not_magic' is refuted"),
        "{t}"
    );
    let o = vesper(&["run", &regressions, "--entry", "not_magic_refuted_1"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "false");
    assert!(vesper(&["fmt", "--check", &regressions]).status.success());
}

#[test]
fn refute_reports_survival_preconditions_and_crashes() {
    let file = fixture();
    let o = vesper(&[
        "verify",
        "--refute",
        &file,
        "--contract",
        "min_is_at_most_a",
    ]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains("no counterexample found within budget (5000 execution(s)"),
        "{t}"
    );
    let o = vesper(&["verify", "--refute", &file, "--contract", "positive"]);
    assert!(
        text(&o).contains("counterexample positive(0) is false"),
        "{}",
        text(&o)
    );
    let o = vesper(&[
        "verify",
        "--refute",
        &file,
        "--contract",
        "positive",
        "--requires",
        "positive",
        "--execs",
        "500",
    ]);
    assert!(o.status.success(), "{}", text(&o));
    let o = vesper(&[
        "verify",
        "--refute",
        &file,
        "--contract",
        "square_is_nonnegative",
    ]);
    let t = text(&o);
    assert!(!o.status.success());
    assert!(
        t.contains("'square_is_nonnegative' could not be evaluated"),
        "{t}"
    );
    let o = vesper(&["verify", "--refute", &file, "--contract", "clamp"]);
    assert!(
        text(&o).contains("contract 'clamp' must return bool"),
        "{}",
        text(&o)
    );
    let o = vesper(&["verify", "--refute", &file]);
    assert!(text(&o).contains("verify --refute requires --contract <fn>"));
    let o = vesper(&[
        "verify",
        "--refute",
        &file,
        "--contract",
        "positive",
        "--requires",
        "clamp",
    ]);
    assert!(
        text(&o).contains("precondition 'clamp' must take"),
        "{}",
        text(&o)
    );
}
