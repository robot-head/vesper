//! Wave-seven lane 14 (§40): FieldTree heatmaps, the static signature-search
//! index and page, `.olean` certificate links from a Loom store, and doctests
//! split between one shared JIT module and the reference evaluator.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::doctest::{self, DocTestLimits, Engine};

struct Scratch(PathBuf);
impl Scratch {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-codex-rest-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        for (path, bytes) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vesper(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}
fn ok(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const MIXED: &str = "/// A pair of counters.\ncopy struct Pair {\n    left: u64,\n    right: u64\n}\n\n/// ```vesper,entry=scalar\n/// fn scalar() -> u64 {\n///     wrapping_add(20, 22)\n/// }\n/// ```\n///\n/// ```vesper,entry=aggregate\n/// fn aggregate() -> u64 {\n///     total(Pair {\n///         left: 40,\n///         right: 2\n///     })\n/// }\n/// ```\n///\n/// ```vesper,entry=also_scalar\n/// fn also_scalar() -> u64 {\n///     wrapping_add(scalar_helper(), 1)\n/// }\n/// fn scalar_helper() -> u64 {\n///     6\n/// }\n/// ```\nfn total(p: Pair) -> u64 {\n    wrapping_add(p.left, p.right)\n}\n";

#[test]
fn doctests_share_one_jit_module_and_fall_back_to_the_evaluator_per_example() {
    let report = doctest::run(MIXED, DocTestLimits::default()).unwrap();
    assert!(report.passed, "{report:?}");
    let engines: Vec<_> = report.examples.iter().map(|e| e.engine).collect();
    assert_eq!(
        engines,
        [
            Some(Engine::Jit),
            Some(Engine::Evaluator),
            Some(Engine::Jit)
        ]
    );
    let results: Vec<_> = report
        .examples
        .iter()
        .map(|e| e.result.as_deref())
        .collect();
    assert_eq!(results, [Some("42"), Some("42"), Some("7")]);
    assert!(
        report.examples[1]
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("aggregate")
    );
    let batch = report.batch.as_ref().unwrap();
    // Both covered examples ran in the same single module.
    assert_eq!((batch.module_count, batch.entry_count), (1, 2));
    assert_eq!(report.coverage_probes, 3);
    assert_eq!((report.jit_examples, report.evaluator_examples), (2, 1));

    // When everything is covered no probe is compiled.
    let scalar = "/// ```vesper,entry=a\n/// fn a() -> u64 {\n///     1\n/// }\n/// ```\n///\n/// ```vesper,entry=b\n/// fn b() -> u64 {\n///     2\n/// }\n/// ```\nfn anchor() -> u64 {\n    0\n}\n";
    let report = doctest::run(scalar, DocTestLimits::default()).unwrap();
    assert!(report.passed);
    assert_eq!(report.coverage_probes, 0);
    assert_eq!(report.batch.as_ref().unwrap().entry_count, 2);
    assert!(
        report
            .examples
            .iter()
            .all(|e| e.engine == Some(Engine::Jit))
    );

    // Only evaluator-run examples: no shared module at all.
    let only = "copy struct P {\n    x: u64\n}\n/// ```vesper,entry=a\n/// fn a() -> u64 {\n///     let p = P {\n///         x: 3\n///     };\n///     p.x\n/// }\n/// ```\nfn anchor() -> u64 {\n    0\n}\n";
    let report = doctest::run(only, DocTestLimits::default()).unwrap();
    assert!(report.passed, "{report:?}");
    assert!(report.batch.is_none());
    assert_eq!(report.examples[0].engine, Some(Engine::Evaluator));
    assert_eq!(report.examples[0].result.as_deref(), Some("3"));
}

#[test]
fn evaluator_fallback_failures_are_reported_not_skipped() {
    // An ambient effect is outside the native subset, and the evaluator
    // refuses to invent a handler for it.
    let source = "effect Ask {\n    fn ask() -> u64;\n}\n/// ```vesper,entry=a\n/// fn a() -> u64 eff [Ask] {\n///     Ask::ask()\n/// }\n/// ```\nfn anchor() -> u64 {\n    0\n}\n";
    let report = doctest::run(source, DocTestLimits::default()).unwrap();
    assert!(!report.passed);
    let example = &report.examples[0];
    assert_eq!(example.engine, Some(Engine::Evaluator));
    assert!(
        example.message.starts_with("evaluator execution failed"),
        "{}",
        example.message
    );

    let dir = Scratch::new(&[("mixed.ves", MIXED.as_bytes())]);
    let out = vesper(&dir.0, &["test", "--doc", "mixed.ves"]);
    ok(&out);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("PASS doctest 0: executed repeatedly in shared JIT module [jit]"),
        "{text}"
    );
    assert!(
        text.contains("PASS doctest 1: executed repeatedly on the reference evaluator"),
        "{text}"
    );
    assert!(text.contains("[evaluator]"), "{text}");
}
