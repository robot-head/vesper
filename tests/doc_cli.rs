#[path = "../src/doc_commands.rs"]
mod adapter;
use vesper_doc::{Limits, NativeLayout, Type};
const SOURCE: &str = include_str!("../crates/vesper-doc/examples/api.ves");
#[test]
fn actual_checked_extraction_keeps_full_inferred_results_native_layouts_and_docs() {
    adapter::run(vec!["--help".into()]).unwrap();
    let doc = adapter::document_source(SOURCE, Limits::default()).unwrap();
    assert_eq!(doc.module_docs, "Local module & documentation");
    assert!(
        doc.doc_comments
            .iter()
            .any(|c| c.text.contains("Native byte field"))
    );
    let result = doc.functions.iter().find(|f| f.name == "inferred").unwrap();
    let checked = vesper::check::check(&vesper::parser::parse(SOURCE).module);
    assert_eq!(
        result.result,
        vesper::numeric::default_type(&checked.world.function_results["inferred"])
    );
    assert_ne!(result.result, result.declared_result);
    assert!(matches!(result.result, Type::Function { .. }));
    let pair = doc
        .types
        .iter()
        .find(|t| t.declaration.name == "Pair")
        .unwrap();
    assert_eq!(pair.docs, "Pair <b>native</b> layout");
    let NativeLayout::Available {
        alignment,
        bytes,
        variants,
        ..
    } = &pair.layout
    else {
        panic!("native layout unavailable")
    };
    assert_eq!((*alignment, *bytes), (8, 16));
    assert_eq!(variants[0].fields[0].bytes, 8);
    assert_eq!(variants[0].fields[1].offset, 8);
    let update = doc.functions.iter().find(|f| f.name == "update").unwrap();
    assert_eq!(
        serde_json::to_value(&update.parameters[0].selector).unwrap()["paths"][0][0],
        "left"
    );
    assert!(
        doc.reachability
            .iter()
            .any(|s| s.identity.contains("Unused") && !s.terminal_path_exists)
    );
    assert!(
        doc.reachability
            .iter()
            .any(|s| s.identity.contains("Tx") && s.terminal_path_exists)
    );
    assert_eq!(
        doc.html(Limits::default()).unwrap(),
        doc.html(Limits::default()).unwrap()
    );
}
#[test]
fn generated_private_trait_functions_are_excluded_from_checked_metadata() {
    let source = "copy struct Point { x: i64 } trait Score { type Out: mod copy; fn score(in self) -> Self::Out; } impl Score for Point { type Out = i64; fn score(in self) -> i64 { self.x } } model ByX: Score for Point { type Out = i64; fn score(in self) -> Self::Out { self.x } } fn result(in p: Point) -> i64 { p.score() }";
    let checked = vesper::check::check(&vesper::parser::parse(source).module);
    assert!(checked.diagnostics.is_empty());
    assert!(!checked.world.elaboration.generated.is_empty());
    let doc = adapter::document_source(source, Limits::default()).unwrap();
    for name in checked.world.elaboration.generated.keys() {
        assert!(!doc.functions.iter().any(|f| &f.name == name));
        assert!(
            !String::from_utf8(doc.json(Limits::default()).unwrap())
                .unwrap()
                .contains(name)
        );
    }
    assert!(!doc.traits.is_empty());
    assert_eq!(doc.source_conformances.len(), 2);
    assert!(
        doc.source_conformances
            .iter()
            .all(|c| c.self_type == Type::named("Point"))
    );
}

#[test]
fn effect_operation_names_do_not_reuse_unrelated_function_result_inference() {
    let source = "linear effect Log { fn value() -> u64; } fn value() -> bool { true } fn use_log() -> u64 eff [left: Log] { left.value() }";
    let doc = adapter::document_source(source, Limits::default()).unwrap();
    assert_eq!(doc.effects[0].operations[0].result, Type::named("u64"));
    let f = doc.functions.iter().find(|f| f.name == "use_log").unwrap();
    assert_eq!(f.effects.instances[0].label.as_deref(), Some("left"));
    assert_eq!(f.effect_resumptions, [Some(vesper_doc::Mode::Linear)]);
}
#[test]
fn conservative_expansion_guards_precede_existing_checker() {
    let mut source = "copy struct A { x: u64 }\n".to_owned();
    for i in 0..20 {
        let previous = if i == 0 {
            "A".into()
        } else {
            format!("A{}", i - 1)
        };
        source.push_str(&format!(
            "copy struct A{i} {{ x: {previous}, y: {previous} }}\n"
        ));
    }
    assert!(
        adapter::document_source(&source, Limits::default())
            .unwrap_err()
            .contains("expansion budget")
    );
    let source = format!("fn f() {{ {} true {} }}", "if true {} else ".repeat(65), "");
    assert!(
        adapter::document_source(&source, Limits::default())
            .unwrap_err()
            .contains("conditional")
    );
}

#[test]
fn recursive_nominal_dependencies_fail_before_generic_mode_exploration() {
    let source = "copy struct Loop[T] { next: Loop[Option[T]] } fn main() {}";
    assert!(
        adapter::document_source(source, Limits::default())
            .unwrap_err()
            .contains("recursive nominal")
    );
}

#[test]
fn parsed_ast_depth_is_audited_before_checker_world_cloning() {
    let mut expression = "0".to_owned();
    for _ in 0..43 {
        expression = format!("{{{expression}}}+1+1");
    }
    let source = format!("fn f() -> i64 {{ {expression} }}");
    vesper_doc::source_preflight(&source, Limits::default()).unwrap();
    let error = adapter::document_source(&source, Limits::default()).unwrap_err();
    // The integrated bounded parser can refuse this before the adapter's AST
    // audit. Both are explicit resource errors preceding checker/World cloning.
    assert!(
        error.contains("AST node/depth") || error.starts_with("source parse failed: E0003:"),
        "{error}"
    );
}

#[test]
fn inferred_tuple_generic_and_closure_growth_is_refused_before_checking() {
    let mut source = "fn main() { let x=0;".to_owned();
    for _ in 0..6 {
        source.push_str("let x=(x,x);");
    }
    source.push('}');
    assert!(
        adapter::document_source(&source, Limits::default())
            .unwrap_err()
            .contains("tuple aggregate")
    );
    let source = "fn dup[T: mod copy](x:T) -> (T,T) { (x,x) } fn main() { let x=0; let x=dup(x); let x=dup(x); }";
    assert!(
        adapter::document_source(source, Limits::default())
            .unwrap_err()
            .contains("generic")
    );
    let source = "fn main() { let x=0; let x=||x; let x=||x; }";
    assert!(
        adapter::document_source(source, Limits::default())
            .unwrap_err()
            .contains("closure growth")
    );
    // Complete tuple signatures and generic declarations still document/search
    // conservatively; the unsafe inference operations, not DTO types, are refused.
    adapter::document_source(
        "fn tuple(in x:(i64,bool)) -> (i64,bool) { x } fn id[T](sink x:T) -> T { x }",
        Limits::default(),
    )
    .unwrap();
}

#[test]
fn computed_adt_mode_is_not_confused_with_its_declared_floor() {
    let source = "linear struct Tx {} copy struct Wrap { tx: Tx } fn close(sink tx: Tx) { let Tx {} = tx; } fn close_wrap(sink w: Wrap) { let Wrap { tx } = w; close(sink tx); }";
    let doc = adapter::document_source(source, Limits::default()).unwrap();
    let wrap = doc
        .types
        .iter()
        .find(|t| t.declaration.name == "Wrap")
        .unwrap();
    assert_eq!(wrap.declaration.mode, vesper_doc::Mode::Copy);
    assert_eq!(wrap.concrete_mode, Some(vesper_doc::Mode::Linear));
}
