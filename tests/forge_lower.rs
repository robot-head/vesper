//! Real source syntax, typed host resolution, identity lowering and core parsing.
use vesper::{
    ast::*,
    forge_frontend::{
        authority::{self, AuthorityLimits},
        lower::{self, LowerError, LowerLimits, SegmentOrigin},
    },
    parser,
    tooling::frontend::{AnalysisFailure, AnalysisLimits, check_source},
};
use vesper_forge::{
    HygieneHost, InvocationSite, SessionId,
    typed_quote::{ResolvedBody, TypedBudget, TypedLimits},
};
use vesper_syntax::macro_source::{self, MacroSource};
fn prepare(
    source: &str,
) -> (
    MacroSource,
    authority::SourceBindings,
    HygieneHost,
    ResolvedBody,
) {
    let syntax = macro_source::parse(source, macro_source::Limits::default()).unwrap();
    let bindings =
        authority::bind_source(&syntax, "@app/lower", AuthorityLimits::default()).unwrap();
    let mut host = HygieneHost::new(SessionId(17), bindings.registry().clone());
    for name in bindings.global_names() {
        if let Some(def) = bindings.def_id(name) {
            host.declare_global(bindings.source_package(), name, def)
                .unwrap();
        }
    }
    let mut budget = TypedBudget::new(TypedLimits::default()).unwrap();
    let input = host
        .import_expr_source(bindings.source_package(), source, &mut budget)
        .unwrap();
    let function = syntax
        .functions()
        .iter()
        .find(|f| !f.invocations.is_empty())
        .unwrap();
    let mut replacements = Vec::new();
    for id in &function.invocations {
        let invocation = syntax.invocation(*id).unwrap();
        let definition = syntax.definition(invocation.definition).unwrap();
        let def = bindings.macro_def(definition.id).unwrap();
        let params = definition
            .parameters
            .iter()
            .map(|p| p.name_span.clone())
            .collect::<Vec<_>>();
        let quotation = host
            .expr_quotation(
                &input,
                def,
                definition.quote_span.clone(),
                &params,
                &mut budget,
            )
            .unwrap();
        let args = invocation
            .arguments
            .iter()
            .map(|span| host.expr_input(&input, span.clone(), &mut budget).unwrap())
            .collect();
        let expansion = host
            .begin_expr_expansion(
                def,
                InvocationSite::Source(bindings.source_package()),
                args,
                &mut budget,
            )
            .unwrap();
        let result = host
            .finish_expr(expansion, quotation.items(), &mut budget)
            .unwrap();
        replacements.push((invocation.span.clone(), result));
    }
    let body = host
        .resolve_expr_body(
            &input,
            function.body_span.clone(),
            replacements,
            &mut budget,
        )
        .unwrap();
    (syntax, bindings, host, body)
}
fn candidate(source: &str, syntax: &MacroSource, body: &str) -> String {
    let function = syntax
        .functions()
        .iter()
        .find(|f| !f.invocations.is_empty())
        .unwrap();
    let mut edits = syntax
        .definitions()
        .iter()
        .map(|d| (d.span.clone(), String::new()))
        .collect::<Vec<_>>();
    edits.push((function.body_span.clone(), body.into()));
    edits.sort_by_key(|(s, _)| s.start);
    let mut out = source.to_owned();
    for (s, text) in edits.into_iter().rev() {
        out.replace_range(s, &text);
    }
    out
}
fn lower_source(source: &str) -> (String, lower::LoweredBody) {
    let (syntax, bindings, host, body) = prepare(source);
    let lowered = lower::lower_body(&body, &host, &bindings, LowerLimits::default()).unwrap();
    let candidate = candidate(source, &syntax, lowered.source());
    let parsed = parser::parse(&candidate);
    assert!(
        parsed.diagnostics.is_empty(),
        "{:?}\n{candidate}",
        parsed.diagnostics
    );
    let name = &syntax
        .functions()
        .iter()
        .find(|f| !f.invocations.is_empty())
        .unwrap()
        .header
        .name;
    let f = parsed
        .module
        .items
        .iter()
        .find_map(|i| match i {
            Item::Function(f) if &f.name == name => Some(f),
            _ => None,
        })
        .unwrap();
    assert!(
        lowered
            .shape_matches(f.body.as_ref().unwrap(), LowerLimits::default())
            .unwrap()
    );
    assert!(lowered.work_used() > 0);
    assert!(lowered.work_used() <= LowerLimits::default().max_work);
    (candidate, lowered)
}
fn value(source: &str) -> String {
    let checked = check_source(source, AnalysisLimits::default()).unwrap();
    match vesper::runtime::Runtime::new(checked.world())
        .run("main", vec![])
        .unwrap()
    {
        vesper::runtime::Value::String(s) => s,
        value => value.to_string(),
    }
}
#[test]
fn generated_and_caller_locals_are_alpha_renamed_by_distinct_binder_identity() {
    let source =
        "macro keep($x:expr)->expr{quote{{let tmp=0;$x}}} fn main()->i64{let tmp=42;keep!(tmp)}";
    let (candidate, lowered) = lower_source(source);
    assert_eq!(value(&candidate), "42");
    assert!(lowered.source().contains("let __forge_local_0"));
    assert!(lowered.source().contains("let __forge_local_1"));
    assert!(!lowered.source().contains("let tmp"));
}
#[test]
fn global_defid_is_not_captured_by_callers_same_spelling() {
    let source = "macro answer()->expr{quote{helper()}} fn helper()->i64{42} fn main()->i64{let helper=0;answer!()}";
    let (candidate, lowered) = lower_source(source);
    assert_eq!(value(&candidate), "42");
    assert!(lowered.source().contains("helper()"));
    assert!(!lowered.source().contains("let helper"));
}
#[test]
fn own_initializer_uses_outer_identity_and_fresh_names_avoid_all_source_globals() {
    let source = "macro keep($x:expr)->expr{quote{{let x=$x;x}}} fn __forge_local_0()->i64{99} fn main()->i64{let __forge_local_1=42;let x=__forge_local_1;keep!(x)}";
    let (candidate, lowered) = lower_source(source);
    assert_eq!(value(&candidate), "42");
    assert!(!lowered.source().contains("let __forge_local_0"));
    assert!(!lowered.source().contains("let __forge_local_1"));
}
#[test]
fn trailing_semicolon_and_empty_block_unit_semantics_are_preserved() {
    for source in [
        "macro unit()->expr{quote{{42;}}} fn main(){unit!()}",
        "macro unit()->expr{quote{{}}} fn main(){unit!()}",
        "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!({42;});}",
    ] {
        let (candidate, _) = lower_source(source);
        assert_eq!(value(&candidate), "()");
    }
    let (candidate, _) =
        lower_source("macro answer()->expr{quote{{42}}} fn main()->i64{answer!()}");
    assert_eq!(value(&candidate), "42");
}
#[test]
fn integer_bool_and_json_string_values_come_from_actual_ast_literals() {
    for (literal, result, ty) in [
        ("0x2_a", "42", "i64"),
        ("true", "true", "bool"),
        ("\"é\\n\\uD83D\\uDE00\"", "é\n😀", "String"),
    ] {
        let source =
            format!("macro literal()->expr{{quote{{{literal}}}}} fn main()->{ty}{{literal!()}}");
        let (candidate, _) = lower_source(&source);
        assert_eq!(value(&candidate), result);
    }
}
#[test]
fn repeated_source_argument_has_unique_real_positions_and_shared_original_origin() {
    let source = "macro twice($x:expr)->expr{quote{{consume($x);consume($x);}}} linear struct Tx{} fn make()->Tx{Tx{}} fn consume(sink tx:Tx){let Tx{}=tx;} fn main(){let tx=make();twice!(tx)}";
    let (candidate, lowered) = lower_source(source);
    let argument = source.rfind("!(tx)").unwrap() + 2;
    let original = argument..argument + 2;
    let spans = lowered
        .segments()
        .iter()
        .filter_map(|s| match &s.origin {
            SegmentOrigin::Source { span } if span == &original => Some(s.span.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(spans.len(), 2);
    assert_ne!(spans[0], spans[1]);
    assert_eq!(
        &lowered.source()[spans[0].clone()],
        &lowered.source()[spans[1].clone()]
    );
    let Err(AnalysisFailure::Semantic { diagnostics }) =
        check_source(&candidate, AnalysisLimits::default())
    else {
        panic!("linear duplicate was accepted");
    };
    assert!(diagnostics.iter().any(|d| d.code == "E0303"));
    let parsed = parser::parse(&candidate);
    let f = parsed
        .module
        .items
        .iter()
        .find_map(|i| match i {
            Item::Function(f) if f.name == "main" => Some(f),
            _ => None,
        })
        .unwrap();
    let mut all = std::collections::BTreeSet::new();
    fn visit(e: &Expr, seen: &mut std::collections::BTreeSet<(usize, usize)>) {
        assert!(seen.insert((e.span.start, e.span.end)));
        match &e.kind {
            ExprKind::Block { statements, tail } => {
                for s in statements {
                    match s {
                        Statement::Let { value, .. } | Statement::Expr(value) => visit(value, seen),
                    }
                }
                if let Some(t) = tail {
                    visit(t, seen)
                }
            }
            ExprKind::Call { callee, args, .. } => {
                visit(callee, seen);
                for a in args {
                    visit(&a.expr, seen)
                }
            }
            _ => {}
        }
    }
    visit(f.body.as_ref().unwrap(), &mut all);
}
#[test]
fn foreign_host_source_and_resource_failures_return_no_partial_lowered_body() {
    let source = "macro keep($x:expr)->expr{quote{{let value=$x;value}}} fn main()->i64{keep!(42)}";
    let (_, bindings, host, body) = prepare(source);
    let foreign = HygieneHost::new(host.session(), bindings.registry().clone());
    assert_eq!(
        lower::lower_body(&body, &foreign, &bindings, LowerLimits::default()).unwrap_err(),
        LowerError::ForeignHost
    );
    let same_syntax = macro_source::parse(source, macro_source::Limits::default()).unwrap();
    let foreign_package =
        authority::bind_source(&same_syntax, "@app/other", AuthorityLimits::default()).unwrap();
    assert_eq!(
        lower::lower_body(&body, &host, &foreign_package, LowerLimits::default()).unwrap_err(),
        LowerError::InvalidInventory("package identity")
    );
    let changed = source.replace("42", "43");
    let (_, other, _, _) = prepare(&changed);
    assert_eq!(
        lower::lower_body(&body, &host, &other, LowerLimits::default()).unwrap_err(),
        LowerError::ForeignSource
    );
    for limits in [
        LowerLimits {
            max_nodes: 1,
            ..LowerLimits::default()
        },
        LowerLimits {
            max_output_bytes: 1,
            ..LowerLimits::default()
        },
        LowerLimits {
            max_map_entries: 1,
            ..LowerLimits::default()
        },
        LowerLimits {
            max_work: 1,
            ..LowerLimits::default()
        },
        LowerLimits {
            max_depth: 1,
            ..LowerLimits::default()
        },
        LowerLimits {
            max_identifier_bytes: 1,
            ..LowerLimits::default()
        },
    ] {
        assert!(lower::lower_body(&body, &host, &bindings, limits).is_err());
    }
    assert!(lower::lower_body(&body, &host, &bindings, LowerLimits::default()).is_ok());
    assert!(
        LowerLimits {
            max_work: 0,
            ..LowerLimits::default()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn shape_comparison_ignores_positions_but_rejects_semantic_changes() {
    let (_, lowered) = lower_source(
        "macro keep($x:expr)->expr{quote{{let local=$x;helper(local)}}} fn helper(in value:i64)->i64{value} fn main()->i64{keep!(42)}",
    );
    let mut actual = lowered.expected().clone();
    actual.span = 1000..2000;
    assert!(
        lowered
            .shape_matches(&actual, LowerLimits::default())
            .unwrap()
    );
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &mut actual.kind
    else {
        panic!()
    };
    let ExprKind::Block { statements, .. } = &mut tail.kind else {
        panic!()
    };
    let Statement::Let { ty, .. } = &mut statements[0] else {
        panic!()
    };
    *ty = Type::Named("i64".into(), vec![]);
    assert!(
        !lowered
            .shape_matches(&actual, LowerLimits::default())
            .unwrap()
    );
    let mut actual = lowered.expected().clone();
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &mut actual.kind
    else {
        panic!()
    };
    let ExprKind::Block {
        tail: Some(call), ..
    } = &mut tail.kind
    else {
        panic!()
    };
    let ExprKind::Call { args, .. } = &mut call.kind else {
        panic!()
    };
    args[0].explicit = true;
    assert!(
        !lowered
            .shape_matches(&actual, LowerLimits::default())
            .unwrap()
    );
}

#[test]
fn reported_work_is_exact_and_can_be_shared_across_body_calls() {
    let (_, bindings, host, body) =
        prepare("macro keep($x:expr)->expr{quote{{let value=$x;value}}} fn main()->i64{keep!(42)}");
    let first = lower::lower_body(&body, &host, &bindings, LowerLimits::default()).unwrap();
    let work = first.work_used();
    let exact = LowerLimits {
        max_work: work,
        ..LowerLimits::default()
    };
    assert_eq!(
        lower::lower_body(&body, &host, &bindings, exact)
            .unwrap()
            .work_used(),
        work
    );
    assert_eq!(
        lower::lower_body(
            &body,
            &host,
            &bindings,
            LowerLimits {
                max_work: work - 1,
                ..exact
            }
        )
        .unwrap_err(),
        LowerError::Budget(lower::Resource::Work)
    );
    let total = work.checked_mul(2).unwrap();
    let one = lower::lower_body(
        &body,
        &host,
        &bindings,
        LowerLimits {
            max_work: total,
            ..exact
        },
    )
    .unwrap();
    let remaining = total - one.work_used();
    let two = lower::lower_body(
        &body,
        &host,
        &bindings,
        LowerLimits {
            max_work: remaining,
            ..exact
        },
    )
    .unwrap();
    assert_eq!(remaining - two.work_used(), 0);
}

#[test]
fn qualified_effect_defid_preserves_the_actual_core_operation_path() {
    let (candidate, lowered) = lower_source(
        "effect Log{fn event();} macro event()->expr{quote{Log::event()}} fn main() eff[Log]{event!()}",
    );
    assert!(lowered.source().contains("Log::event()"));
    check_source(&candidate, AnalysisLimits::default()).unwrap();
}
