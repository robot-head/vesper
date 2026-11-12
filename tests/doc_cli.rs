#[path = "../src/doc_commands.rs"]
mod adapter;
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_doc::{Limits, NativeLayout, Type};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "vesper-doc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn write(&self, name: &str, text: &str) -> String {
        let p = self.0.join(name);
        fs::write(&p, text).unwrap();
        p.to_str().unwrap().into()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap()
}
fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)))
}
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
fn cli_json_html_and_local_output_are_bounded_escaped_and_deterministic() {
    let fixture = Fixture::new();
    let source = fixture.write("api.ves", SOURCE);
    let out = run(&["doc", &source, "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(report(&out)["status"], "valid");
    let a = run(&["doc", &source]);
    let b = run(&["doc", &source]);
    assert!(a.status.success());
    assert_eq!(a.stdout, b.stdout);
    let html = String::from_utf8(a.stdout).unwrap();
    assert!(html.contains("&lt;b&gt;native&lt;/b&gt;"));
    assert!(!html.contains("<b>native</b>"));
    let output = fixture.0.join("docs.html");
    let out = run(&["doc", &source, "-o", output.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(fs::read(&output).unwrap(), html.as_bytes());
    let out = run(&["doc", &source, "-o", output.to_str().unwrap(), "--json"]);
    assert!(!out.status.success());
    assert_eq!(report(&out)["status"], "invalid");
    assert_eq!(fs::read(output).unwrap(), html.as_bytes());
}
#[test]
fn cli_structural_search_rejects_equal_mode_vectors_with_incompatible_full_types() {
    let fixture = Fixture::new();
    let source = fixture.write(
        "types.ves",
        "fn number(in x: i64) -> i64 { x } fn boolean(in x: bool) -> bool { x }",
    );
    let query = vesper_doc::Query {
        parameters: vec![vesper_doc::QueryParameter {
            pass: vesper_doc::Pass::In,
            mode: vesper_doc::Mode::Copy,
            ty: Type::named("bool"),
            selector: vesper_doc::Selector::All,
        }],
        result: Type::named("bool"),
        result_max_mode: vesper_doc::Mode::Copy,
        effect_ceiling: Default::default(),
    };
    let query = fixture.write("query.json", &serde_json::to_string(&query).unwrap());
    let out = run(&["doc", &source, "--query", &query, "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = report(&out);
    assert_eq!(r["search"]["matches"].as_array().unwrap().len(), 1);
    assert_eq!(r["search"]["matches"][0]["function"], "boolean");
}
#[test]
fn invalid_source_arguments_utf8_and_source_budgets_create_no_output() {
    let fixture = Fixture::new();
    let source = fixture.write("bad.ves", "fn broken() -> bool { 1 }");
    let dest = fixture.0.join("bad.html");
    let out = run(&["doc", &source, "--json", "-o", dest.to_str().unwrap()]);
    assert!(!out.status.success());
    assert_eq!(report(&out)["status"], "invalid");
    assert!(!dest.exists());
    for args in [
        vec!["doc", "--json"],
        vec!["doc", "--bad", "--json"],
        vec!["doc", &source, "extra", "--json"],
        vec!["doc", &source, "-o", "--json"],
    ] {
        let out = run(&args);
        assert!(!out.status.success());
        assert_eq!(report(&out)["status"], "invalid");
    }
    fs::write(&source, [0xff]).unwrap();
    assert!(!run(&["doc", &source, "--json"]).status.success());
    fs::write(&source, vec![b' '; 131_073]).unwrap();
    assert!(!run(&["doc", &source, "--json"]).status.success());
    assert!(run(&["doc", "--help"]).status.success());
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
    assert!(
        adapter::document_source(&source, Limits::default())
            .unwrap_err()
            .contains("AST node/depth")
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
