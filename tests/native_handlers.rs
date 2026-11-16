//! The native handler ABI (`vesper::native_handlers`, #586) against the
//! evaluator: resuming handlers, nested handlers and forwarding, aborts
//! through handlers, stateful handlers, and closures passed and returned at
//! run time. Each fixture under `tests/fixtures/native_handlers/` must give
//! the evaluator's value.
use std::path::{Path, PathBuf};
use vesper::native_handlers::{self, HandlerAbiError};
use vesper::{check, parser, runtime::Runtime};

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native_handlers");
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ves"))
        .collect();
    out.sort();
    out
}

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}

fn evaluate(world: &check::World) -> String {
    let mut runtime = Runtime::new(world);
    runtime.fuel = 200_000_000;
    runtime.run("main", vec![]).unwrap().to_string()
}

/// Run on a thread with a roomy stack: the evaluator and Cranelift recurse.
fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn fixtures_agree_with_the_evaluator() {
    big_stack(|| {
        let paths = fixtures();
        assert!(paths.len() >= 9, "fixtures missing");
        for path in paths {
            if let Ok(only) = std::env::var("NATIVE_HANDLERS_ONLY")
                && !path.to_string_lossy().contains(&only)
            {
                continue;
            }
            eprintln!("fixture {}", path.display());
            let source = std::fs::read_to_string(&path).unwrap();
            let w = world(&source);
            let expected = evaluate(&w);
            let run = native_handlers::jit(&w, "main")
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(
                run.value.to_string(),
                expected,
                "{}: the handler ABI differs from the evaluator",
                path.display()
            );
            assert!(
                run.fibers > 0,
                "{}: no handler ran on a fiber",
                path.display()
            );
        }
    });
}

#[test]
fn effectful_functions_are_compiled_once() {
    big_stack(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/native_handlers/shared_function.ves");
        let w = world(&std::fs::read_to_string(path).unwrap());
        let size = native_handlers::code_size(&w, "main").unwrap();
        let names: Vec<&String> = size.functions.keys().collect();
        assert_eq!(
            names.iter().filter(|n| n.as_str() == "twice").count(),
            1,
            "{names:?}"
        );
        // `under` installs the handler; `twice` is reached through it twice
        // at run time but compiled once.
        assert!(size.functions.contains_key("under"));
        assert!(size.total() > 0);
    });
}

#[test]
fn unsupported_programs_are_rejected_with_typed_errors() {
    big_stack(|| {
        let cases = [
            ("fn main() -> i64 { let t = (1, 2); 3 }", "aggregate values"),
            (
                "fn id[T](sink x: T) -> T { x }\nfn main() -> i64 { id(3) }",
                "generic function",
            ),
            ("fn main() -> f64 { 1.5 }", "float"),
        ];
        for (source, needle) in cases {
            let w = world(source);
            match native_handlers::jit(&w, "main") {
                Err(HandlerAbiError::Unsupported(m)) => {
                    assert!(m.contains(needle), "{source}: {m}")
                }
                other => panic!("{source}: expected Unsupported, got {other:?}"),
            }
        }
    });
}

/// Bytes of code in the specializing backend's object for `main`.
fn specializing_text(w: &check::World) -> Result<u64, String> {
    use object::{Object, ObjectSection, SectionKind};
    let (bytes, _) = vesper::native::object(w, "main")?;
    let file = object::File::parse(&*bytes).map_err(|e| e.to_string())?;
    Ok(file
        .sections()
        .filter(|s| s.kind() == SectionKind::Text)
        .map(|s| s.size())
        .sum())
}

/// Code size on the corpus (`examples/*.ves` and the fixtures), handler ABI
/// against the specializing path. Prints the table the report quotes;
/// asserts only that every fixture compiles under the handler ABI and that
/// both sizes are positive where both paths compile.
#[test]
fn code_size_against_the_specializing_path() {
    big_stack(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut corpus: Vec<PathBuf> = std::fs::read_dir(root.join("examples"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "ves"))
            .collect();
        corpus.sort();
        let fixtures = fixtures();
        corpus.extend(fixtures.iter().cloned());
        eprintln!("| program | handler ABI bytes | specializing bytes |");
        eprintln!("|---|---:|---:|");
        for path in corpus {
            let source = std::fs::read_to_string(&path).unwrap();
            let p = parser::parse(&source);
            let c = check::check(&p.module);
            if !p.diagnostics.is_empty() || !c.diagnostics.is_empty() {
                continue;
            }
            let w = c.world;
            if !w.functions.contains_key("main") {
                continue;
            }
            let handlers = native_handlers::code_size(&w, "main");
            let specializing = specializing_text(&w);
            let show = |r: Result<u64, String>| match r {
                Ok(n) => n.to_string(),
                Err(e) => format!("unsupported ({})", e.lines().next().unwrap_or("")),
            };
            let name = path.strip_prefix(root).unwrap().display().to_string();
            if fixtures.contains(&path) {
                assert!(handlers.is_ok(), "{name}: {:?}", handlers.as_ref().err());
            }
            let h = handlers
                .map(|s| s.total() as u64)
                .map_err(|e| e.to_string());
            if let (Ok(a), Ok(b)) = (&h, &specializing) {
                assert!(*a > 0 && *b > 0, "{name}");
            }
            eprintln!("| {name} | {} | {} |", show(h), show(specializing));
        }
    });
}
