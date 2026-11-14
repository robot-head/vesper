//! Lane 23 (#265): native edge counters and comparison-operand logging in
//! the Cranelift backend, feeding `vesper fuzz --native-coverage`.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::native_coverage::{NativeCoverage, NativeOutcome, NativeParam};

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];
/// `4660 * 4660 + 77`, computed in the fixture and never written there.
const MAGIC: u64 = 21_715_677;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native_coverage/magic.ves")
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

fn checked(source: &str) -> vesper::check::World {
    let parsed = vesper::parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    checked.world
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

#[test]
fn instrumented_code_logs_edges_and_comparison_operands() {
    let source = fs::read_to_string(fixture()).unwrap();
    let world = checked(&source);
    let mut coverage = NativeCoverage::compile(&source, &world, "unlock").unwrap();
    assert_eq!(coverage.params(), &[NativeParam::U64, NativeParam::U32]);
    let stats = coverage.stats();
    assert_eq!(stats.stubs, 2);
    assert!(stats.edges > 0 && stats.comparisons > 0, "{stats:?}");

    assert_eq!(coverage.run(&[5, 9]).unwrap(), NativeOutcome::Returned);
    let miss_edges = coverage.buffer().edges();
    assert!(!miss_edges.is_empty());
    let logged = coverage.buffer().comparisons();
    assert!(
        logged.iter().any(|&(_, l, r)| (l, r) == (5, MAGIC)),
        "{logged:?}"
    );
    let mut probe = vesper_crucible::Probe::default();
    coverage.feed(&mut probe);
    assert!(!probe.edges().is_empty());
    assert!(probe.comparisons().iter().any(|c| matches!(
        c.operands,
        vesper_crucible::CmpOperands::Int {
            lhs: 5,
            rhs: MAGIC,
            ..
        }
    ) && !c.taken));

    // The magic key takes a different path; a small length still returns.
    assert_eq!(coverage.run(&[MAGIC, 3]).unwrap(), NativeOutcome::Returned);
    assert_ne!(coverage.buffer().edges(), miss_edges);
    assert_eq!(
        coverage.run(&[MAGIC, 65_536]).unwrap(),
        NativeOutcome::Failed {
            effect: "Fail[ArithOverflow]".into()
        }
    );
    // Runs are independent: the buffer is reset before each one.
    assert_eq!(coverage.run(&[5, 9]).unwrap(), NativeOutcome::Returned);
    assert_eq!(coverage.buffer().edges(), miss_edges);

    // Arguments outside the parameter type are typed errors.
    assert!(
        coverage
            .run(&[1, 1 << 32])
            .unwrap_err()
            .contains("out of range")
    );
    assert!(coverage.run(&[1]).unwrap_err().contains("expected 2"));

    // Signed operands are also reported in the i64 zigzag tape encoding.
    let mut gate = NativeCoverage::compile(&source, &world, "gate").unwrap();
    assert_eq!(gate.run(&[4, 0]).unwrap(), NativeOutcome::Returned);
    let mut probe = vesper_crucible::Probe::default();
    gate.feed(&mut probe);
    let wanted: i64 = -81_000_003;
    let zigzag = ((wanted << 1) ^ (wanted >> 63)) as u64;
    assert!(probe.comparisons().iter().any(|c| c.site & 0x8000_0000 != 0
        && matches!(c.operands, vesper_crucible::CmpOperands::Int { lhs: 8, rhs, .. } if rhs == zigzag)));
}

#[test]
fn unsupported_targets_are_typed_errors() {
    let file = fixture();
    let file = file.to_str().unwrap();
    let o = vesper(&[
        "fuzz",
        file,
        "--target",
        "unlock",
        "--native-coverage",
        "--native",
    ]);
    assert!(!o.status.success());
    assert!(text(&o).contains("cannot be combined"), "{}", text(&o));

    let reserved =
        "fn f(x: i64) -> i64 {\n    x\n}\n\nfn __vesper_fuzz_arg_0() -> i64 {\n    0\n}\n";
    let world = checked(reserved);
    match NativeCoverage::compile(reserved, &world, "f") {
        Ok(_) => panic!("reserved name accepted"),
        Err(e) => assert!(e.contains("reserved"), "{e}"),
    }
    let source = "effect Log {\n    fn write(x: i64) -> Unit;\n}\n\nfn logged(x: i64) -> i64 eff [Log] {\n    x\n}\n\nfn pair(p: (i64, i64)) -> i64 {\n    0\n}\n";
    let world = checked(source);
    let error = |name| match NativeCoverage::compile(source, &world, name) {
        Ok(_) => panic!("{name} compiled"),
        Err(e) => e,
    };
    assert!(
        error("logged").contains("only unlabeled Fail"),
        "{}",
        error("logged")
    );
    assert!(
        error("pair").contains("supported: i64"),
        "{}",
        error("pair")
    );
    assert!(error("missing").contains("not a function"));
}

#[test]
fn programs_compile_to_identical_code_without_the_coverage_hook() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut sources: Vec<PathBuf> = fs::read_dir(&examples)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ves"))
        .collect();
    sources.push(fixture());
    sources.sort();
    let temp = Temp::new();
    let mut compared = 0;
    for path in sources {
        let source = fs::read_to_string(&path).unwrap();
        let parsed = vesper::parser::parse(&source);
        if !parsed.diagnostics.is_empty() {
            continue;
        }
        let mut module = parsed.module;
        let mut world = vesper::check::check(&module);
        if !world.diagnostics.is_empty() || !world.world.functions.contains_key("main") {
            continue;
        }
        vesper::optimize::optimize(&mut module, &world.expression_types);
        world = vesper::check::check(&module);
        let world = &world.world;
        for target in TARGETS {
            let Ok((plain, plain_clif)) =
                vesper::native::object_with_options(world, "main", Some(target), true)
            else {
                continue;
            };
            let (none, none_clif) =
                vesper::native::object_instrumented(world, "main", Some(target), true, None, None)
                    .unwrap();
            assert_eq!(plain, none, "{} {target}", path.display());
            assert_eq!(plain_clif, none_clif);
            // A hook that changes nothing leaves the bytes unchanged too.
            let mut visited = 0usize;
            let mut noop = |_: &str, _: &mut cranelift_codegen::ir::Function| {
                visited += 1;
                Ok(())
            };
            let (inert, _) = vesper::native::object_instrumented(
                world,
                "main",
                Some(target),
                true,
                None,
                Some(&mut noop),
            )
            .unwrap();
            assert_eq!(plain, inert, "{} {target}", path.display());
            assert!(visited > 0);
            // Instrumented code is verified and generated for both ISAs.
            let (instrumented, clif, stats) = vesper::native_coverage::object_with_coverage(
                world,
                "main",
                Some(target),
                true,
                0x1000_0000,
            )
            .unwrap();
            assert_ne!(plain, instrumented, "{} {target}", path.display());
            assert!(stats.edges > 0, "{stats:?}");
            assert!(clif.contains("iconst.i64 0x1000_0000"), "{clif}");
            if stats.comparisons > 0 {
                assert!(clif.contains("umin"), "{clif}");
            }
            // The CLI, which never passes the hook, emits the plain object
            // after an instrumented compile in this process.
            let after =
                vesper::native::object_with_options(world, "main", Some(target), true).unwrap();
            assert_eq!(plain, after.0);
            let out = temp.path("cli.o");
            let o = vesper(&[
                "emit",
                path.to_str().unwrap(),
                "--emit",
                "object",
                "-o",
                &out,
                "--target",
                target,
            ]);
            assert!(o.status.success(), "{}", text(&o));
            assert_eq!(
                fs::read(&out).unwrap(),
                plain,
                "{} {target}",
                path.display()
            );
            compared += 1;
        }
    }
    assert!(compared >= 8, "compared {compared} program/target pairs");
}
