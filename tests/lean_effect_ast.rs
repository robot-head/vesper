use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{
    check, lean, parser,
    runtime::{Runtime, Value},
};
use vesper_lean_syntax::{Declaration, Ident, Name, Term, TypeDeclaration};

fn world(source: &str) -> check::World {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    checked.world
}
fn evaluate(source: &str) {
    let world = world(source);
    let mut module = lean::extract_module(&world).unwrap();
    assert_eq!(module.render().unwrap(), lean::extract(&world).unwrap());
    let expected = Runtime::new(&world).run("main", vec![]).unwrap();
    let mut entry = Term::Ref(
        Name::qualified([Ident::new("Vesper").unwrap(), Ident::new("main").unwrap()])
            .unwrap()
            .rooted(),
    );
    let expected = match expected {
        Value::Float(value) => {
            entry = Term::app(
                Term::Ref(
                    Name::qualified([Ident::new("Float").unwrap(), Ident::new("toBits").unwrap()])
                        .unwrap()
                        .rooted(),
                ),
                [entry],
            );
            value.to_bits().to_string()
        }
        value => value.to_string(),
    };
    module.declarations.push(Declaration::Eval(entry));
    let output = module.render().unwrap();
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    let version = match Command::new(&compiler).arg("--version").output() {
        Ok(v) => v,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("Lean validation skipped: install the pinned toolchain or set VESPER_LEAN");
            return;
        }
        Err(error) => panic!("Lean version probe failed: {error}"),
    };
    assert!(version.status.success());
    assert!(
        String::from_utf8_lossy(&version.stdout).contains("4.24.0"),
        "expected the pinned Lean 4.24.0 toolchain"
    );
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "vesper-lean-effect-ast-{}-{}.lean",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, &output).unwrap();
    let result = Command::new(compiler).arg(&path).output().unwrap();
    let _ = fs::remove_file(&path);
    assert!(
        result.status.success(),
        "{}\n{}\n{output}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&result.stdout)
            .trim()
            .lines()
            .last(),
        Some(expected.as_str()),
        "Lean/reference result mismatch:\n{output}"
    );
}

#[test]
fn module_has_structured_prelude_effect_class_instances_and_functions() {
    let world = world(
        "copy struct Pair { x: i64, y: i64 } linear effect Ask[T] { fn ask() -> T; } fn main() -> i64 { handle Ask[i64] in { Ask::ask() } with { return x => x, ask(k) => k(42), } }",
    );
    let module = lean::extract_module(&world).unwrap();
    let Declaration::Namespace { declarations, .. } = module.declarations.last().unwrap() else {
        panic!("missing structured namespace")
    };
    assert!(
        declarations
            .iter()
            .any(|d| matches!(d, Declaration::Type(TypeDeclaration::Class { .. })))
    );
    assert!(
        declarations
            .iter()
            .filter(|d| matches!(d, Declaration::Instance(_)))
            .count()
            >= 2
    );
    assert!(declarations.iter().any(|d|matches!(d,Declaration::MutualTypes(types)if types.iter().any(|t|matches!(t,TypeDeclaration::Structure{..})))));
    // Non-recursive functions are ordinary definitions in dependency order;
    // only recursive groups keep a `mutual` block.
    assert!(declarations.iter().any(|d| match d {
        Declaration::MutualDefinitions(definitions) => !definitions.is_empty(),
        Declaration::Definition(definition) => definition.name.components()[0].as_str() == "main",
        _ => false,
    }));
}

#[test]
fn stateful_projected_lenses_survive_non_tail_resumption() {
    evaluate(
        "copy struct Pair { x: i64, y: i64 } linear effect Mut { fn change(inout p.{x}: Pair) -> i64; } fn main() -> i64 { let p = Pair { x: 1, y: 3 }; let r = handle Mut state n = 10 in { let v = Mut::change(inout p); p.y = wrapping_add(p.y, 100); wrapping_add(v, p.x) } with { return (v, n) => wrapping_add(v, n), change(q, n, k) => { q.x = wrapping_add(q.x, n); let r = k(20, wrapping_add(n, 1)); q.x = wrapping_add(q.x, 1); r }, }; wrapping_add(wrapping_mul(r, 1000), wrapping_add(p.x, p.y)) }",
    );
}

#[test]
fn closures_capture_snapshots_and_keep_outer_evidence() {
    evaluate(
        "linear effect Ask { fn ask() -> i64; } fn apply[r: eff](sink f: fn() -> i64 eff [r]) -> i64 eff [r] { f() } fn main() -> i64 { let x = 5; handle outer: Ask in { let f = || { x = wrapping_add(x, 1); wrapping_add(x, outer.ask()) }; x = 99; handle Ask in { apply(f) } with { return v => v, ask(k) => k(1), } } with { return v => v, ask(k) => wrapping_add(k(20), 2), } }",
    );
}

#[test]
fn checked_failures_preserve_the_frame_and_do_not_commit_failed_assignment() {
    evaluate(
        "fn main() -> i64 { let x: i64 = 9223372036854775807; let y: i64 = 1; let r = handle Fail[ArithOverflow] in { x += y; 0 } with { return v => v, raise(e, k) => 42, }; if x == 9223372036854775807 { r } else { 0 } }",
    );
    evaluate(
        "fn divide(b: i64) -> i64 eff [Fail[RuntimeError], Fail[ArithOverflow]] { -9223372036854775808 / b } fn main() -> i64 { handle Fail[ArithOverflow] in { handle Fail[RuntimeError] in { divide(0) } with { return v => v, raise(e, k) => 41, } } with { return v => v, raise(e, k) => 99, } }",
    );
}

#[test]
fn subnormal_float_bits_survive_effectful_capture_and_handler_translation() {
    evaluate(
        "linear effect Ask { fn ask() -> f64; } fn main() -> f64 { let x: f64 = 1e-320; handle Ask in { let f = || x; let ignored = Ask::ask(); f() } with { return v => v, ask(k) => k(0.0), } }",
    );
}

#[test]
fn unsupported_alias_diagnostic_remains_explicit() {
    let world = world(
        "linear effect Inc { fn inc(inout x: i64); } fn helper(inout x: i64) eff [Inc] { Inc::inc(inout x); x = wrapping_add(x, 1); } fn main() -> i64 { let x = 1; let r = handle Inc in { helper(inout x); x } with { return n => n, inc(n, k) => { let r = k(()); n = wrapping_add(n, 2); r }, }; wrapping_add(wrapping_mul(r, 100), x) }",
    );
    let error = lean::extract_module(&world).unwrap_err();
    assert!(
        error.contains("through a helper, callback, or closure"),
        "{error}"
    );
}

#[test]
fn conditional_and_match_actions_thread_outer_mutable_frame_bindings() {
    evaluate(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { let x = 1; let r = handle Ask in { let first = if Ask::ask() > 2 { x = wrapping_add(x, 4); x } else { x = 0; x }; let second = match first { 5 => { x = wrapping_mul(x, 2); x }, _ => { x = 99; x }, }; second } with { return v => v, ask(k) => k(4), }; wrapping_add(wrapping_mul(r, 100), x) }",
    );
}
