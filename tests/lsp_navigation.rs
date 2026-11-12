//! Real parser/checker snapshots and strict UTF-16 ranges; no compiler execution.
use serde_json::Value;
use std::sync::Arc;
use vesper::{
    lsp::navigation::{self, Error, Limits, OpenDocument},
    tooling::frontend::{AnalysisFailure, AnalysisLimits, CheckedSource, check_source},
};
use vesper_span::{ByteSpan, LineIndex, Position};
fn checked(source: &str) -> (CheckedSource, LineIndex) {
    let snapshot = check_source(source, AnalysisLimits::default())
        .unwrap_or_else(|e| panic!("{e:?}\n{source}"));
    let index = LineIndex::new(Arc::from(source), vesper_span::Limits::default()).unwrap();
    (snapshot, index)
}
fn at(index: &LineIndex, needle: &str, delta: usize) -> Position {
    index
        .position(index.text().find(needle).unwrap() + delta)
        .unwrap()
}
fn target_start(value: &Value) -> Position {
    Position {
        line: value["range"]["start"]["line"].as_u64().unwrap() as u32,
        character: value["range"]["start"]["character"].as_u64().unwrap() as u32,
    }
}
fn definition(
    snapshot: &CheckedSource,
    index: &LineIndex,
    needle: &str,
    delta: usize,
) -> Option<Value> {
    navigation::definition(
        "untitled:source",
        snapshot,
        index,
        at(index, needle, delta),
        Limits::default(),
    )
    .unwrap()
}
#[test]
fn checked_hover_uses_actual_expression_type_plaintext_docs_and_generic_bounds() {
    let source = "/// <script>literal docs</script>\nfn identity[T: mod linear](sink x:T)->T{x}\nfn main()->u64{42}";
    let (snapshot, index) = checked(source);
    let h = navigation::hover(
        &snapshot,
        &index,
        at(&index, "->T{x}", 4),
        Limits::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(h["contents"]["kind"], "plaintext");
    let text = h["contents"]["value"].as_str().unwrap();
    assert!(text.contains("type: T"), "{text}");
    assert!(
        text.contains("type mode: linear (not current ownership or a live obligation)"),
        "{text}"
    );
    assert!(text.contains("parameter: sink x"), "{text}");
    let declaration = navigation::hover(
        &snapshot,
        &index,
        at(&index, "identity", 0),
        Limits::default(),
    )
    .unwrap()
    .unwrap();
    let text = declaration["contents"]["value"].as_str().unwrap();
    assert!(text.contains("T: mod linear"));
    assert!(text.contains("<script>literal docs</script>"));
    assert!(!text.contains("proof:"));
}
#[test]
fn checked_closure_effects_and_pass_modes_are_not_erased() {
    let source = "linear effect Ask {fn ask()->i64;} fn main()->i64{let get: fn()->i64 eff [Ask] = || Ask::ask();handle Ask in { get() } with {return x=>x,ask(k)=>k(42),}}";
    let (snapshot, index) = checked(source);
    let value = navigation::hover(&snapshot, &index, at(&index, "get()", 0), Limits::default())
        .unwrap()
        .unwrap();
    let text = value["contents"]["value"].as_str().unwrap();
    assert!(text.contains("Ask"), "{text}");
    assert!(text.contains("fn("), "{text}");
    // Handler names remain unresolved rather than inventing state/continuation scopes.
    assert!(definition(&snapshot, &index, "get()", 0).is_none());
    let (snapshot, index) = checked(
        "effect Event[T] {fn emit(in value:T);} fn emit(in value:i64) eff [local: Event[i64]] {local.emit(value);} fn main(){} ",
    );
    let value = navigation::hover(
        &snapshot,
        &index,
        at(&index, "fn emit(in value:i64)", 3),
        Limits::default(),
    )
    .unwrap()
    .unwrap();
    assert!(
        value["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("local: Event[i64]")
    );
}
#[test]
fn nested_function_effect_arguments_preserve_labels_and_passes() {
    let (snapshot, index) = checked(
        "effect Carry[T]{fn send(in x:T);} fn f() eff [slot:Carry[fn(in i64)->i64 eff [Fail[u8]]]] {} ",
    );
    let h = navigation::hover(
        &snapshot,
        &index,
        at(&index, "fn f()", 3),
        Limits::default(),
    )
    .unwrap()
    .unwrap();
    let text = h["contents"]["value"].as_str().unwrap();
    assert!(text.contains("slot: Carry["), "{text}");
    assert!(text.contains("fn(in i64) -> i64 eff [Fail[u8]]"), "{text}");
}

#[test]
fn parameter_selectors_and_type_modes_come_from_actual_source() {
    let (snapshot, index) = checked(
        "copy struct Pair{left:i64,right:i64} fn update(inout p.{left}:Pair){p.left=1;} fn main(){}",
    );
    let h = navigation::hover(
        &snapshot,
        &index,
        at(&index, "p.left=", 0),
        Limits::default(),
    )
    .unwrap()
    .unwrap();
    let text = h["contents"]["value"].as_str().unwrap();
    assert!(text.contains("type: Pair"));
    assert!(text.contains("type mode: copy"));
    assert!(text.contains("parameter: inout p.{left}"), "{text}");
}
#[test]
fn definitions_respect_shadowing_initializer_order_and_block_exit() {
    let source = "fn f(in x:u64)->u64{let x=x; {let x:u64=7;x}; x}";
    let (snapshot, index) = checked(source);
    let own_initializer = definition(&snapshot, &index, "let x=x", 6).unwrap();
    assert_eq!(target_start(&own_initializer), at(&index, "in x:", 3));
    let inner = definition(&snapshot, &index, "7;x", 2).unwrap();
    assert_eq!(target_start(&inner), at(&index, "let x:u64", 4));
    let after = definition(&snapshot, &index, "; x}", 2).unwrap();
    assert_eq!(target_start(&after), at(&index, "let x=x", 4));
}
#[test]
fn global_function_and_function_local_shadow_are_distinct_targets() {
    let (snapshot, index) =
        checked("fn target()->u64{1} fn main()->u64{let target=target;target()}");
    assert_eq!(
        target_start(&definition(&snapshot, &index, "=target", 1).unwrap()),
        at(&index, "fn target", 3)
    );
    assert_eq!(
        target_start(&definition(&snapshot, &index, ";target()", 1).unwrap()),
        at(&index, "let target", 4)
    );
}
#[test]
fn tuple_match_and_closure_scopes_resolve_actual_binding_spans() {
    let (snapshot, index) = checked(
        "fn main(in pair:(i64,i64))->i64{let (a,b)=pair;let f:fn(i64)->i64=|a:i64| a;f(a)}",
    );
    assert_eq!(
        target_start(&definition(&snapshot, &index, "| a;", 2).unwrap()),
        at(&index, "|a:i64", 1)
    );
    assert_eq!(
        target_start(&definition(&snapshot, &index, "f(a)", 2).unwrap()),
        at(&index, "(a,b)", 1)
    );
    let (snapshot, index) = checked("fn main(in x:Option[i64])->i64{match x{Some(y)=>y,None=>0,}}");
    assert_eq!(
        target_start(&definition(&snapshot, &index, "=>y", 2).unwrap()),
        at(&index, "Some(y)", 5)
    );
}
#[test]
fn field_and_trait_dispatch_never_guess_same_named_globals() {
    let (snapshot, index) =
        checked("copy struct S{target:u64} fn target()->u64{1} fn main(in s:S)->u64{s.target}");
    assert!(definition(&snapshot, &index, "s.target", 2).is_none());
    assert_eq!(
        target_start(&definition(&snapshot, &index, "s.target", 0).unwrap()),
        at(&index, "in s:", 3)
    );
    let (snapshot, index) = checked(
        "copy struct S{n:i64} trait Get{fn get(in self)->i64;} impl Get for S{fn get(in self)->i64{self.n}} fn get()->i64{9} fn main(in s:S)->i64{s.get()}",
    );
    assert!(definition(&snapshot, &index, "s.get()", 2).is_none());
}
#[test]
fn unicode_crlf_ranges_are_strict_and_follow_current_revision() {
    let source = "// 😀\r\nfn first()->u64{1}\r\nfn second()->u64{first()}\r\n";
    let (snapshot, index) = checked(source);
    let d = definition(&snapshot, &index, "{first()", 1).unwrap();
    assert_eq!(
        target_start(&d),
        Position {
            line: 1,
            character: 3
        }
    );
    assert!(matches!(
        navigation::hover(
            &snapshot,
            &index,
            Position {
                line: 0,
                character: 4
            },
            Limits::default()
        ),
        Err(Error::InvalidPosition(_))
    ));
    let symbols = navigation::document_symbols(&snapshot, &index, Limits::default()).unwrap();
    assert_eq!(symbols[1]["range"]["start"]["line"], 2);
    let edited = source.replace("{1}", "{\r\n1\r\n}");
    let (new_snapshot, new_index) = checked(&edited);
    let new_symbols =
        navigation::document_symbols(&new_snapshot, &new_index, Limits::default()).unwrap();
    assert_eq!(new_symbols[1]["range"]["start"]["line"], 4);
    assert_eq!(new_symbols[1]["selectionRange"]["start"]["character"], 3);
    assert_eq!(
        navigation::document_symbols(&snapshot, &new_index, Limits::default()).unwrap_err(),
        Error::SourceMismatch
    );
}
#[test]
fn document_and_open_workspace_symbols_have_exact_selection_locations() {
    let (one, one_index) = checked("copy struct S{x:u64} fn alpha()->u64{1}");
    let (two, two_index) = checked("\nfn beta()->u64{2}");
    let docs = [
        OpenDocument {
            uri: "untitled:one",
            index: &one_index,
            snapshot: &one,
        },
        OpenDocument {
            uri: "opaque:two",
            index: &two_index,
            snapshot: &two,
        },
    ];
    let symbols = navigation::workspace_symbols(&docs, "beta", Limits::default()).unwrap();
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0]["location"]["uri"], "opaque:two");
    assert_eq!(symbols[0]["location"]["range"]["start"]["line"], 1);
    assert_eq!(symbols[0]["location"]["range"]["start"]["character"], 3);
    let symbols = navigation::document_symbols(&one, &one_index, Limits::default()).unwrap();
    let range = &symbols[1]["selectionRange"];
    let span = one_index
        .span(vesper_span::Range {
            start: Position {
                line: range["start"]["line"].as_u64().unwrap() as u32,
                character: range["start"]["character"].as_u64().unwrap() as u32,
            },
            end: Position {
                line: range["end"]["line"].as_u64().unwrap() as u32,
                character: range["end"]["character"].as_u64().unwrap() as u32,
            },
        })
        .unwrap();
    assert_eq!(&one.source()[span.start..span.end], "alpha");
}
#[test]
fn invalid_analysis_never_supplies_navigation_snapshot_and_limits_fail_closed() {
    assert!(matches!(
        check_source("fn broken(", AnalysisLimits::default()),
        Err(AnalysisFailure::Parse { .. })
    ));
    assert!(matches!(
        check_source("fn main()->u64{unknown}", AnalysisLimits::default()),
        Err(AnalysisFailure::Semantic { .. })
    ));
    let mut growth = String::from("fn main(){let x=0;");
    for _ in 0..25 {
        growth.push_str("let x=(x,x);");
    }
    growth.push('}');
    assert!(matches!(
        check_source(&growth, AnalysisLimits::default()),
        Err(AnalysisFailure::Unsupported { .. } | AnalysisFailure::Resource { .. })
    ));
    let (snapshot, index) = checked("fn main()->u64{42}");
    for limits in [
        Limits {
            max_nodes: 1,
            ..Limits::default()
        },
        Limits {
            max_source_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_output_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_type_work: 1,
            ..Limits::default()
        },
        Limits {
            max_depth: 0,
            ..Limits::default()
        },
    ] {
        assert!(navigation::hover(&snapshot, &index, at(&index, "42", 0), limits).is_err());
    }
    assert!(
        navigation::definition(
            "",
            &snapshot,
            &index,
            at(&index, "42", 0),
            Limits::default()
        )
        .is_err()
    );
    let docs = [
        OpenDocument {
            uri: "one",
            index: &index,
            snapshot: &snapshot,
        },
        OpenDocument {
            uri: "two",
            index: &index,
            snapshot: &snapshot,
        },
    ];
    assert!(
        navigation::workspace_symbols(
            &docs,
            "",
            Limits {
                max_documents: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(
        navigation::workspace_symbols(
            &docs,
            "main",
            Limits {
                max_symbols: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(index.range(ByteSpan { start: 0, end: 999 }).is_err());
}

#[test]
fn nominal_mode_cost_is_bounded_and_recursive_sources_have_no_snapshot() {
    let source = "copy enum List[T]{Nil,Cons(T,List[T])} linear struct Tx{} fn carry(sink xs:List[Tx])->List[Tx]{xs}";
    assert!(matches!(
        check_source(source, AnalysisLimits::default()),
        Err(AnalysisFailure::Unsupported { .. })
    ));
    let (snapshot, index) = checked(
        "copy struct Wrap[T]{value:T} linear struct Tx{} fn carry(sink xs:Wrap[Tx])->Wrap[Tx]{xs}",
    );
    let h = navigation::hover(&snapshot, &index, at(&index, "{xs}", 1), Limits::default())
        .unwrap()
        .unwrap();
    assert!(
        h["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("type mode: linear")
    );
    assert!(
        navigation::hover(
            &snapshot,
            &index,
            at(&index, "{xs}", 1),
            Limits {
                max_type_work: 3,
                ..Limits::default()
            }
        )
        .is_err()
    );
}
