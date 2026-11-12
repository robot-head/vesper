use vesper::{
    check,
    native::batch::{BatchLimits, NativeBatch},
    parser,
    runtime::Runtime,
};
fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}
#[test]
fn actual_shared_module_union_and_repeated_results_match_evaluator() {
    let w = world(
        "fn helper(x: i64) -> i64 { wrapping_add(x, 1) } fn first() -> i64 { helper(41) } fn second() -> i64 { helper(8) }",
    );
    let mut batch = NativeBatch::compile(&w, &["first", "second"], BatchLimits::default()).unwrap();
    assert_eq!(batch.stats().module_count, 1);
    assert_eq!(batch.stats().compilation_count, 1);
    assert_eq!(batch.stats().defined_functions, 3);
    assert_eq!(batch.stats().entry_count, 2);
    for _ in 0..20 {
        for entry in ["first", "second"] {
            let expected = Runtime::new(&w).run(entry, vec![]).unwrap().to_string();
            assert_eq!(batch.run(entry).unwrap().to_string(), expected);
        }
    }
    assert_eq!(batch.stats().runs, 40);
    assert!(batch.run("missing").is_err());
    assert_eq!(batch.stats().runs, 40);
}
#[test]
fn scalar_abis_are_cached_and_results_survive_module_drop() {
    let w = world(
        "fn signed() -> i64 { -7 } fn byte() -> u8 { 255 } fn wide() -> u64 { 18446744073709551615 } fn yes() -> bool { true } fn real() -> f64 { 3.5 } fn unit() {} ",
    );
    let names = ["signed", "byte", "wide", "yes", "real", "unit"];
    let mut batch = NativeBatch::compile(&w, &names, BatchLimits::default()).unwrap();
    let values: Vec<_> = names.iter().map(|n| batch.run(n).unwrap()).collect();
    drop(batch);
    for (name, value) in names.iter().zip(values) {
        assert_eq!(
            value.to_string(),
            Runtime::new(&w).run(name, vec![]).unwrap().to_string()
        );
    }
}
#[test]
fn rejects_recursion_foreign_imports_indirection_and_bad_entry_shapes() {
    // Ordinary checked World imports can have body=None even though the current
    // source parser has no standalone foreign-declaration syntax.
    let mut imported = world("fn host() -> i64 { 7 } fn main() -> i64 { host() }");
    imported.functions.get_mut("host").unwrap().body = None;
    let error = NativeBatch::compile(&imported, &["main"], BatchLimits::default())
        .err()
        .unwrap();
    assert!(error.contains("ambient/foreign import"), "{error}");
    for (source, entry) in [
        ("fn main() -> i64 { main() }", "main"),
        ("fn a() -> i64 { b() } fn b() -> i64 { a() }", "a"),
        ("fn main(x: i64) -> i64 { x }", "main"),
        (
            "copy struct P { x: i64 } fn main() -> P { P { x: 1 } }",
            "main",
        ),
        ("fn main() -> i64 { let f = || 1; f() }", "main"),
    ] {
        let w = world(source);
        assert!(
            NativeBatch::compile(&w, &[entry], BatchLimits::default()).is_err(),
            "{source}"
        );
    }
}
#[test]
fn bounded_runs_and_expanded_dag_work_precede_execution() {
    let w = world("fn main() -> i64 { 42 }");
    let mut batch = NativeBatch::compile(
        &w,
        &["main"],
        BatchLimits {
            max_runs: 2,
            ..BatchLimits::default()
        },
    )
    .unwrap();
    batch.run("main").unwrap();
    batch.run("main").unwrap();
    assert!(batch.run("main").is_err());
    assert_eq!(batch.stats().runs, 2);
    assert!(NativeBatch::compile(&w, &[], BatchLimits::default()).is_err());
    assert!(NativeBatch::compile(&w, &["main", "main"], BatchLimits::default()).is_err());
    let larger = world("fn main() -> i64 { wrapping_add(41, 1) }");
    assert!(
        NativeBatch::compile(
            &larger,
            &["main"],
            BatchLimits {
                max_ir_instructions: 1,
                ..BatchLimits::default()
            }
        )
        .is_err()
    );
    let mut source = "fn f0() -> i64 { 1 }".to_string();
    for n in 1..20 {
        source.push_str(&format!(
            "fn f{n}() -> i64 {{ wrapping_add(f{}(), f{}()) }}",
            n - 1,
            n - 1
        ));
    }
    let w = world(&source);
    let error = NativeBatch::compile(&w, &["f19"], BatchLimits::default())
        .err()
        .unwrap();
    assert!(error.contains("expanded-work"), "{error}");
}
