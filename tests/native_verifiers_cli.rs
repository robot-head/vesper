//! Integration B (#136): Stage-5 verifier hooks on the Cranelift functions
//! `src/native.rs` emits, on x86_64 and AArch64, and the guarantee that
//! programs compile to identical code without a verifier flag.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integration_b/verifiers.ves")
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-native-verifiers-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
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

/// Without a verifier flag the CLI output is exactly the backend's, and the
/// observing hook does not change the emitted object.
#[test]
fn programs_compile_to_identical_code_without_verifier_flags() {
    let temp = Temp::new();
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut checked = 0;
    let mut sources: Vec<PathBuf> = fs::read_dir(&examples)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ves"))
        .collect();
    sources.push(fixture());
    sources.sort();
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
        for target in TARGETS {
            let Ok((plain, plain_clif)) =
                vesper::native::object_with_options(&world.world, "main", Some(target), true)
            else {
                continue;
            };
            let mut seen = 0usize;
            let mut observer = |_: vesper::native::EmittedFunction| seen += 1;
            let (observed, observed_clif) = vesper::native::object_with_observer(
                &world.world,
                "main",
                Some(target),
                true,
                Some(&mut observer),
            )
            .unwrap();
            assert_eq!(plain, observed, "{} {target}", path.display());
            assert_eq!(plain_clif, observed_clif);
            assert!(seen > 0);
            let out = temp.0.join("cli.o");
            let o = vesper(&[
                "emit",
                path.to_str().unwrap(),
                "--emit",
                "object",
                "-o",
                out.to_str().unwrap(),
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
            // Verifying observes only: the object is the same with a flag.
            let o = vesper(&[
                "emit",
                path.to_str().unwrap(),
                "--emit",
                "object",
                "-o",
                out.to_str().unwrap(),
                "--target",
                target,
                "--verify-const-time",
            ]);
            assert!(o.status.success(), "{}", text(&o));
            assert_eq!(
                fs::read(&out).unwrap(),
                plain,
                "{} {target}",
                path.display()
            );
            checked += 1;
        }
    }
    assert!(checked >= 8, "only {checked} programs compiled");
}

#[test]
fn hook_reports_every_emitted_function_with_resolvable_names() {
    let source = fs::read_to_string(fixture()).unwrap();
    let parsed = vesper::parser::parse(&source);
    let checked = vesper::check::check(&parsed.module);
    let (_, _, functions) =
        vesper::native_verifiers::emit_and_collect(&checked.world, "float_main", None, true)
            .unwrap();
    let keys: Vec<_> = functions.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["float_main", "wrap"]);
    let wrap = &functions[1];
    assert_eq!(
        wrap.function.name.to_string(),
        format!("u0:{}", wrap.func_id)
    );
    assert!(
        wrap.callees.iter().any(|(_, s)| s == "fmod"),
        "{:?}",
        wrap.callees
    );
    assert_eq!(functions[0].symbol, "float_main");
}
