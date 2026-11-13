//! Link DTOs are derived from genuine checked source, never supplied approval.
use serde_json::json;
use std::collections::BTreeSet;
use vesper::{
    ast::Item,
    check, parser,
    tooling::{
        doc::{document_checked, document_source},
        frontend::{AnalysisLimits, check_source},
    },
};
use vesper_doc::{Document, Limits, links::LinkStatus};

const SOURCE: &str = "//! See [Tx], [Tx::commit], [Ask::ask], [Score] and [main].\n\
    linear struct Tx {}\n\
    impl Tx { fn commit(sink self) -> i64 { let Tx {} = self; 42 } }\n\
    linear effect Ask { fn ask() -> i64; }\n\
    trait Score { fn score(in self) -> i64; }\n\
    /// The same [Tx::commit] target occurs again.\n\
    fn main() -> i64 { let tx = Tx {}; tx.commit() }\n";

fn actual(source: &str) -> Document {
    document_source(source, Limits::default()).unwrap_or_else(|e| panic!("{source}: {e}"))
}

fn targets(document: &Document) -> BTreeSet<(String, String)> {
    serde_json::to_value(&document.intra_doc_links.targets)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["path"].as_str().unwrap().into(),
                t["kind"].as_str().unwrap().into(),
            )
        })
        .collect()
}

fn error(source: &str, code: &str, reference: &str) -> String {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let start = source.find(reference).unwrap();
    let expected = format!("{code} at bytes {start}..{}", start + reference.len());
    let error = document_source(source, Limits::default()).unwrap_err();
    assert!(error.contains(&expected), "{error}; expected {expected}");
    let snapshot = check_source(source, AnalysisLimits::default()).unwrap();
    assert_eq!(
        document_checked(&snapshot, Limits::default()).unwrap_err(),
        error
    );
    error
}

#[test]
fn local_targets_match_actual_checked_world_and_source_declarations() {
    let parsed = parser::parse(SOURCE);
    assert!(parsed.diagnostics.is_empty());
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let document = actual(SOURCE);
    assert!(matches!(
        document.intra_doc_links.status,
        LinkStatus::Resolved
    ));
    assert_eq!(document.intra_doc_links.source_bytes, SOURCE.len());
    assert!(document.intra_doc_links.diagnostics.is_empty());
    assert_eq!(
        targets(&document),
        [
            ("Ask", "effect"),
            ("Ask::ask", "effect_operation"),
            ("Score", "trait"),
            ("Tx", "type"),
            ("Tx::commit", "inherent_method"),
            ("main", "function"),
        ]
        .into_iter()
        .map(|(p, k)| (p.into(), k.into()))
        .collect()
    );
    assert!(checked.world.adts.contains_key("Tx"));
    assert!(checked.world.functions.contains_key("Tx::commit"));
    assert!(checked.world.functions.contains_key("main"));
    assert!(
        checked.world.effects["Ask"]
            .ops
            .iter()
            .any(|f| f.name == "ask")
    );
    assert!(
        parsed
            .module
            .items
            .iter()
            .any(|i| matches!(i, Item::Trait(t) if t.name == "Score"))
    );
    // Checker builtin types/effects are real, but not documented declarations.
    assert!(checked.world.adts.contains_key("Option"));
    assert!(checked.world.effects.contains_key("Fail"));
    assert!(
        targets(&document)
            .iter()
            .all(|(p, _)| p != "Option" && p != "Fail")
    );
    let references = &document.intra_doc_links.references;
    assert_eq!(references.len(), 6);
    for reference in references {
        assert_eq!(
            &SOURCE[reference.span.clone()],
            format!("[{}]", reference.display)
        );
    }
    assert_eq!(references[1].anchor, references[5].anchor);
    assert_ne!(references[1].span, references[5].span);
}

#[test]
fn retained_snapshot_and_direct_source_have_identical_complete_link_metadata() {
    let direct = actual(SOURCE);
    let snapshot = check_source(SOURCE, AnalysisLimits::default()).unwrap();
    assert_eq!(snapshot.source(), SOURCE);
    let from_snapshot = document_checked(&snapshot, Limits::default()).unwrap();
    assert_eq!(
        serde_json::to_value(&direct).unwrap(),
        serde_json::to_value(&from_snapshot).unwrap()
    );
    assert_eq!(
        direct.html(Limits::default()).unwrap(),
        from_snapshot.html(Limits::default()).unwrap()
    );
    assert_eq!(
        actual(SOURCE).json(Limits::default()).unwrap(),
        direct.json(Limits::default()).unwrap()
    );
}

#[test]
fn a_source_method_on_a_builtin_type_is_a_target_without_inventing_a_builtin_type() {
    let source = "/// [i64::value] and [main]\nimpl i64 { fn value(in self) -> i64 { self } } fn main() -> i64 { let n: i64 = 42; n.value() }";
    let checked = check::check(&parser::parse(source).module);
    assert!(checked.diagnostics.is_empty());
    assert!(checked.world.functions.contains_key("i64::value"));
    let document = actual(source);
    assert_eq!(
        targets(&document),
        [("i64::value", "inherent_method"), ("main", "function"),]
            .into_iter()
            .map(|(p, k)| (p.into(), k.into()))
            .collect()
    );
    assert_eq!(document.intra_doc_links.references.len(), 2);
    let snapshot = check_source(source, AnalysisLimits::default()).unwrap();
    assert_eq!(
        serde_json::to_value(document_checked(&snapshot, Limits::default()).unwrap()).unwrap(),
        serde_json::to_value(document).unwrap()
    );
}

#[test]
fn unknown_ambiguous_and_unsupported_links_reject_with_original_ranges() {
    error("/// [Missing]\nfn main() {}", "unknown", "[Missing]");
    error(
        "/// [Dual]\ncopy struct Dual {} effect Dual { fn act(); } fn main() {}",
        "ambiguous",
        "[Dual]",
    );
    for reference in ["[Self]", "[Option[i64]]", "[@local/pkg::run]"] {
        error(
            &format!("/// {reference}\nfn main() {{}}"),
            "unsupported",
            reference,
        );
    }
    // A documented trait name does not invent a callable trait method target.
    error(
        "/// [Score::score]\ntrait Score { fn score(in self) -> i64; } fn main() {}",
        "unknown",
        "[Score::score]",
    );
}

#[test]
fn edits_to_declarations_recompute_link_resolution_on_the_current_source() {
    let source = "/// [helper]\nfn main() {} fn helper() {}";
    let first = actual(source);
    assert_eq!(first.intra_doc_links.references[0].target.path, "helper");
    let renamed = source.replace("fn helper", "fn changed");
    error(&renamed, "unknown", "[helper]");
    let repaired = renamed.replace("[helper]", "[changed]");
    let current = actual(&repaired);
    assert_eq!(current.intra_doc_links.references[0].target.path, "changed");
    assert_ne!(
        first.intra_doc_links.references[0].anchor,
        current.intra_doc_links.references[0].anchor
    );
    assert_eq!(current.intra_doc_links.source_bytes, repaired.len());
    assert_eq!(first.intra_doc_links.references[0].target.path, "helper");
}

#[test]
fn unicode_crlf_spans_and_lexical_exclusions_keep_exact_original_association() {
    let source = "//! λ 😀 [main]\r\n\
        /// `[NotALink]` and \\[NotALink], ![NotALink](image), [NotALink](url).\r\n\
        /// ```vesper\r\n\
        /// [NotALink]\r\n\
        /// ```\r\n\
        /// Repeated [`main`]\r\n\
        fn main() { let text = \"/// [NotALink]\"; }\r\n\
        // [NotALink]\r\n";
    let document = actual(source);
    let refs = &document.intra_doc_links.references;
    assert_eq!(refs.len(), 2);
    assert_eq!(&source[refs[0].span.clone()], "[main]");
    assert_eq!(&source[refs[1].span.clone()], "[`main`]");
    assert_eq!(refs[1].display, "`main`");
    assert_eq!(refs[0].anchor, refs[1].anchor);
    assert!(refs[0].span.start > source.chars().take_while(|&c| c != '[').count());
    let doc = actual("/// ```\n/// [NotALink]\n\n/// [main]\nfn main() {}\n");
    assert_eq!(doc.intra_doc_links.references.len(), 1);
}

#[test]
fn actual_checker_and_parser_failures_take_precedence_over_broken_links() {
    let syntax =
        document_source("/// [Missing]\nfn main() { let = 0; }", Limits::default()).unwrap_err();
    assert!(syntax.contains("source parse failed"), "{syntax}");
    assert!(!syntax.contains("intra-doc"));
    for source in [
        "/// [Missing]\nfn main() -> bool { 42 }",
        "/// [Missing]\nlinear struct Tx {} fn main() { let tx = Tx {}; }",
    ] {
        let result = document_source(source, Limits::default()).unwrap_err();
        assert!(result.contains("source check failed"), "{result}");
        assert!(!result.contains("intra-doc"));
    }
}

#[test]
fn private_generated_trait_helpers_and_builtin_only_declarations_are_not_targets() {
    let source = "/// [Point], [Score], [result]\ncopy struct Point { x: i64 } trait Score { type Out: mod copy; fn score(in self) -> Self::Out; } impl Score for Point { type Out = i64; fn score(in self) -> i64 { self.x } } model ByX: Score for Point { type Out = i64; fn score(in self) -> Self::Out { self.x } } fn result(in p: Point) -> i64 { p.score() }";
    let checked = check::check(&parser::parse(source).module);
    assert!(checked.diagnostics.is_empty());
    assert!(!checked.world.elaboration.generated.is_empty());
    let document = actual(source);
    assert_eq!(document.intra_doc_links.references.len(), 3);
    let inventory = targets(&document);
    for name in checked.world.elaboration.generated.keys() {
        assert!(!inventory.iter().any(|(p, _)| p == name));
    }
    assert_eq!(
        inventory,
        [
            ("Point", "type"),
            ("Score", "trait"),
            ("result", "function")
        ]
        .into_iter()
        .map(|(p, k)| (p.into(), k.into()))
        .collect()
    );
    for path in ["Option", "Fail", "Point::score"] {
        error(
            &format!("/// [{path}].\n{source}"),
            "unknown",
            &format!("[{path}]"),
        );
    }
}

#[test]
fn finite_reference_refusal_does_not_publish_partially_analyzed_document() {
    // Commas keep these independent references rather than Markdown's
    // deliberately excluded `[label] [reference]` pairs.
    let source = format!("/// {}\nfn main() {{}}", "[main], ".repeat(1025));
    let checked = check::check(&parser::parse(&source).module);
    assert!(checked.diagnostics.is_empty());
    let result = document_source(&source, Limits::default()).unwrap_err();
    assert!(result.contains("resource"), "{result}");
    let snapshot = check_source(&source, AnalysisLimits::default()).unwrap();
    assert_eq!(
        document_checked(&snapshot, Limits::default()).unwrap_err(),
        result
    );
    let good = actual("/// [main]\nfn main() {}");
    let report = serde_json::to_value(good).unwrap();
    assert_eq!(report["intra_doc_links"]["status"], "resolved");
    assert_eq!(report["intra_doc_links"]["diagnostics"], json!([]));
    assert_eq!(
        report["intra_doc_links"]["references"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
