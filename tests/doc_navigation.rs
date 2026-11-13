//! Actual checked-source navigation, including exact original declaration tokens.
use serde_json::Value;
use std::{ops::Range, sync::Arc};
use vesper::{
    ast::Item,
    lsp::navigation::{self, Error, Limits},
    tooling::frontend::{AnalysisLimits, CheckedSource, check_source},
};
use vesper_span::{LineIndex, Position};

fn checked(source: &str) -> (CheckedSource, LineIndex) {
    let snapshot = check_source(source, AnalysisLimits::default())
        .unwrap_or_else(|e| panic!("{e:?}\n{source}"));
    let index = LineIndex::new(Arc::from(source), Default::default()).unwrap();
    (snapshot, index)
}
fn at(index: &LineIndex, text: &str, delta: usize) -> Position {
    index
        .position(index.text().find(text).unwrap() + delta)
        .unwrap()
}
fn result_span(result: &Value, index: &LineIndex) -> Range<usize> {
    let position = |value: &Value| Position {
        line: value["line"].as_u64().unwrap() as u32,
        character: value["character"].as_u64().unwrap() as u32,
    };
    index.offset(position(&result["range"]["start"])).unwrap()
        ..index.offset(position(&result["range"]["end"])).unwrap()
}
fn definition(
    snapshot: &CheckedSource,
    index: &LineIndex,
    text: &str,
    delta: usize,
) -> Option<Range<usize>> {
    navigation::definition(
        "untitled:current",
        snapshot,
        index,
        at(index, text, delta),
        Limits::default(),
    )
    .unwrap()
    .map(|value| {
        assert_eq!(value["uri"], "untitled:current");
        result_span(&value, index)
    })
}
fn token_within(source: &str, declaration: &Range<usize>, name: &str) -> Range<usize> {
    let offset = source[declaration.clone()].find(name).unwrap() + declaration.start;
    offset..offset + name.len()
}

#[test]
fn supported_references_resolve_to_actual_original_declaration_name_tokens() {
    let source = "//! [Tx], [Tx::commit], [Ask], [Ask::ask], [Score], [main], [`Tx::commit`].\nlinear struct Tx {} impl Tx { fn commit(sink self) -> i64 { let Tx {} = self; 42 } } linear effect Ask { fn ask() -> i64; } trait Score { fn score(in self) -> i64; } fn main() -> i64 { let tx = Tx {}; tx.commit() }";
    let (snapshot, index) = checked(source);
    let world = snapshot.world();
    let trait_decl = snapshot
        .parsed()
        .module
        .items
        .iter()
        .find_map(|i| match i {
            Item::Trait(t) if t.name == "Score" => Some(t),
            _ => None,
        })
        .unwrap();
    let op = world.effects["Ask"]
        .ops
        .iter()
        .find(|f| f.name == "ask")
        .unwrap();
    for (reference, declaration, name) in [
        ("[Tx]", &world.adts["Tx"].span, "Tx"),
        (
            "[Tx::commit]",
            &world.functions["Tx::commit"].span,
            "commit",
        ),
        ("[Ask]", &world.effects["Ask"].span, "Ask"),
        ("[Ask::ask]", &op.span, "ask"),
        ("[Score]", &trait_decl.span, "Score"),
        ("[main]", &world.functions["main"].span, "main"),
        (
            "[`Tx::commit`]",
            &world.functions["Tx::commit"].span,
            "commit",
        ),
    ] {
        let span = definition(&snapshot, &index, reference, 1).unwrap();
        assert_eq!(span, token_within(source, declaration, name));
        assert_eq!(&source[span], name);
    }
}

#[test]
fn successful_occurrences_navigate_amid_unknown_ambiguous_and_unsupported_links() {
    let source = "//! [main], [Missing], [Dual], [Self], [Option[i64]], [@pkg/name::Thing], [Dual::act].\ncopy struct Dual {} effect Dual { fn act(); } fn main() {}";
    let (snapshot, index) = checked(source);
    assert_eq!(
        definition(&snapshot, &index, "[main]", 1).unwrap(),
        token_within(source, &snapshot.world().functions["main"].span, "main")
    );
    assert!(definition(&snapshot, &index, "[Dual::act]", 1).is_some());
    for reference in [
        "[Missing]",
        "[Dual]",
        "[Self]",
        "[Option[i64]]",
        "[@pkg/name::Thing]",
    ] {
        assert!(
            definition(&snapshot, &index, reference, 1).is_none(),
            "{reference}"
        );
    }
}

#[test]
fn identical_leaf_names_keep_owner_and_namespace_identity() {
    let source = "//! [A::take], [B::take], [take], [A], [B].\ncopy struct A {} copy struct B {} impl A { fn take(in self) -> i64 { 1 } } impl B { fn take(in self) -> i64 { 2 } } fn take() -> i64 { 3 } fn main() {}";
    let (snapshot, index) = checked(source);
    for (reference, path) in [
        ("[A::take]", "A::take"),
        ("[B::take]", "B::take"),
        ("[take]", "take"),
    ] {
        assert_eq!(
            definition(&snapshot, &index, reference, 1).unwrap(),
            token_within(source, &snapshot.world().functions[path].span, "take")
        );
    }
    let ambiguous = "//! [A::take], [take], [main].\ncopy struct A {} impl A { fn take(in self) -> i64 { 1 } } effect A { fn take() -> i64; } fn take() -> i64 { 3 } fn main() {}";
    let (snapshot, index) = checked(ambiguous);
    assert!(definition(&snapshot, &index, "[A::take]", 1).is_none());
    assert!(definition(&snapshot, &index, "[take]", 1).is_some());
    assert!(definition(&snapshot, &index, "[main]", 1).is_some());
}

#[test]
fn source_methods_on_builtin_owners_do_not_invent_builtin_type_targets() {
    let source = "//! [i64::value], [i64], [Option], [Fail].\nimpl i64 { fn value(in self) -> i64 { self } } fn main() -> i64 { let n: i64 = 42; n.value() }";
    let (snapshot, index) = checked(source);
    assert_eq!(
        definition(&snapshot, &index, "[i64::value]", 1).unwrap(),
        token_within(
            source,
            &snapshot.world().functions["i64::value"].span,
            "value"
        )
    );
    for reference in ["[i64]", "[Option]", "[Fail]"] {
        assert!(definition(&snapshot, &index, reference, 1).is_none());
    }
}

#[test]
fn unsupported_owner_names_do_not_disable_other_genuine_source_targets() {
    let source = "//! [main], [Self], [Self::answer], [Ghost::answer].\nimpl Self { fn answer() -> i64 { 42 } } impl Ghost { fn answer() -> i64 { 42 } } impl (i64, i64) { fn tuple_answer() -> i64 { 42 } } fn main() {}";
    let (snapshot, index) = checked(source);
    assert!(snapshot.world().functions.contains_key("Self::answer"));
    assert!(snapshot.world().functions.contains_key("::tuple_answer"));
    assert!(definition(&snapshot, &index, "[main]", 1).is_some());
    assert!(definition(&snapshot, &index, "[Self]", 1).is_none());
    assert!(definition(&snapshot, &index, "[Self::answer]", 1).is_none());
    assert_eq!(
        definition(&snapshot, &index, "[Ghost::answer]", 1).unwrap(),
        token_within(
            source,
            &snapshot.world().functions["Ghost::answer"].span,
            "answer"
        )
    );
}

#[test]
fn private_trait_model_helpers_are_not_guessed_from_public_names() {
    let source = "//! [Point], [Score], [Point::score], [Score::score], [ByX], [result].\ncopy struct Point { x: i64 } trait Score { type Out: mod copy; fn score(in self) -> Self::Out; } impl Score for Point { type Out = i64; fn score(in self) -> i64 { self.x } } model ByX: Score for Point { type Out = i64; fn score(in self) -> Self::Out { self.x } } fn result(in p: Point) -> i64 { p.score() }";
    let (snapshot, index) = checked(source);
    assert!(!snapshot.world().elaboration.generated.is_empty());
    for reference in ["[Point]", "[Score]", "[result]"] {
        assert!(definition(&snapshot, &index, reference, 1).is_some());
    }
    for reference in ["[Point::score]", "[Score::score]", "[ByX]"] {
        assert!(definition(&snapshot, &index, reference, 1).is_none());
    }
}

#[test]
fn literal_escaped_code_markdown_and_non_doc_comments_are_not_links() {
    for comment in [
        "/// `[main]`",
        "/// \\[main]",
        "/// ![main](image)",
        "/// [main](https://example.invalid)",
        "/// [main] [label]\n/// [label]: path",
        "/// ```vesper\n/// [main]\n/// ```",
        "/// ~~~\n/// [main]\n/// ~~~",
        "// [main]",
        "//// [main]",
    ] {
        let source = format!("{comment}\nfn main() {{}}");
        let (snapshot, index) = checked(&source);
        assert!(
            definition(&snapshot, &index, "[main]", 1).is_none(),
            "{comment}"
        );
    }
    let (snapshot, index) = checked("fn main() { let text = \"/// [main]\"; }");
    assert!(definition(&snapshot, &index, "[main]", 1).is_none());
    let (snapshot, index) = checked("/// ```\n/// [NotALink]\n\n/// [main]\nfn main() {}");
    assert!(definition(&snapshot, &index, "[main]", 1).is_some());
}

#[test]
fn ordinary_lexical_definitions_retain_local_binding_precedence() {
    let source =
        "//! [helper].\nfn helper() -> i64 { 42 } fn main() -> i64 { let helper = 1; helper }";
    let (snapshot, index) = checked(source);
    assert_eq!(
        definition(&snapshot, &index, "[helper]", 1).unwrap(),
        token_within(source, &snapshot.world().functions["helper"].span, "helper")
    );
    let local = source.find("let helper").unwrap() + 4;
    assert_eq!(
        definition(&snapshot, &index, "helper }", 0),
        Some(local..local + 6)
    );
    assert!(definition(&snapshot, &index, "fn helper", 3).is_none());
}

#[test]
fn unicode_crlf_and_half_open_reference_ranges_use_strict_utf16_positions() {
    let source = "//! 😀 λ [Tx::commit], [`Tx`].\r\nlinear struct Tx {}\r\nimpl Tx { fn commit(sink self) { let Tx {} = self; } }\r\nfn main() {}\r\n";
    let (snapshot, index) = checked(source);
    let expected = token_within(
        source,
        &snapshot.world().functions["Tx::commit"].span,
        "commit",
    );
    let reference = "[Tx::commit]";
    assert_eq!(
        definition(&snapshot, &index, reference, 0),
        Some(expected.clone())
    );
    assert_eq!(
        definition(&snapshot, &index, reference, reference.len() - 1),
        Some(expected)
    );
    assert!(definition(&snapshot, &index, reference, reference.len()).is_none());
    assert!(matches!(
        navigation::definition(
            "untitled:current",
            &snapshot,
            &index,
            Position {
                line: 0,
                character: 5
            },
            Limits::default()
        ),
        Err(Error::InvalidPosition(_))
    ));
}

#[test]
fn stale_indexes_and_declaration_edits_never_reuse_old_locations() {
    let original = "//! [value].\nfn value() {} fn main() {}";
    let (snapshot, index) = checked(original);
    let before = definition(&snapshot, &index, "[value]", 1).unwrap();
    let shifted = format!("// newly inserted 😀\n{original}");
    let (current, current_index) = checked(&shifted);
    assert!(matches!(
        navigation::definition(
            "untitled:current",
            &snapshot,
            &current_index,
            at(&current_index, "[value]", 1),
            Limits::default()
        ),
        Err(Error::SourceMismatch)
    ));
    let after = definition(&current, &current_index, "[value]", 1).unwrap();
    assert_eq!(after.start - before.start, shifted.len() - original.len());
    let changed = shifted.replace("fn value", "fn changed");
    let (current, current_index) = checked(&changed);
    assert!(definition(&current, &current_index, "[value]", 1).is_none());
    assert_eq!(&original[before], "value");
}

#[test]
fn original_navigation_limits_and_actual_link_parser_resource_refusal_apply() {
    let source = "//! [main].\nfn main() {}";
    let (snapshot, index) = checked(source);
    for limits in [
        Limits {
            max_source_bytes: 1,
            ..Default::default()
        },
        Limits {
            max_nodes: 1,
            ..Default::default()
        },
        Limits {
            max_output_bytes: 1,
            ..Default::default()
        },
    ] {
        assert!(matches!(
            navigation::definition(
                "untitled:current",
                &snapshot,
                &index,
                at(&index, "[main]", 1),
                limits
            ),
            Err(Error::Resource(_))
        ));
    }
    let many = format!("//! {}\nfn main() {{}}", "[main], ".repeat(1025));
    let (snapshot, index) = checked(&many);
    assert_eq!(
        navigation::definition(
            "untitled:current",
            &snapshot,
            &index,
            at(&index, "[main]", 1),
            Limits::default()
        ),
        Err(Error::Resource("doc link parser budget"))
    );
    let source = "//! [A].\ncopy struct A {} fn main() {}";
    let (snapshot, index) = checked(source);
    assert!(matches!(
        navigation::definition(
            "untitled:current",
            &snapshot,
            &index,
            at(&index, "[A]", 1),
            Limits {
                max_symbols: 1,
                ..Default::default()
            }
        ),
        Err(Error::Resource(_))
    ));
}

#[test]
fn real_large_checked_inventory_refuses_before_unbounded_world_or_token_scans() {
    let first = format!("f000_{}", "a".repeat(995));
    let mut source = format!("//! [{first}].\n");
    for n in 0..128 {
        source.push_str(&format!("fn f{n:03}_{}(){{", "a".repeat(995)));
        for binding in 0..8 {
            source.push_str(&format!("let x{binding}=0;"));
        }
        source.push_str("}\n");
    }
    let snapshot = check_source(
        &source,
        AnalysisLimits {
            max_source_bytes: 262_144,
            max_ast_nodes: 8192,
            max_type_nodes: 8192,
            max_identifier_bytes: 1024,
            max_metadata_bytes: 2 * 1024 * 1024,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(snapshot.checked().diagnostics.is_empty());
    let index = LineIndex::new(
        Arc::from(source.as_str()),
        vesper_span::Limits {
            max_source_bytes: 262_144,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        navigation::definition(
            "untitled:current",
            &snapshot,
            &index,
            at(&index, &first, 0),
            Limits {
                max_source_bytes: 262_144,
                ..Default::default()
            }
        ),
        Err(Error::Resource("doc navigation inventory work/metadata"))
    );
    // The exact same genuine source still has a valid original checked snapshot.
    assert_eq!(snapshot.source(), source);
}
