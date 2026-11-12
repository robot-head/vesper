//! Actual tuple source compilation retains shape, origin and linear checking.
use vesper::{
    ast::{Expr, ExprKind, Item, Type},
    forge_frontend::{self as forge, CompileConfig, CompileLimits, ExpandedChecked, Stage, lower},
    parser,
};
use vesper_syntax::macro_source;

fn compile(source: &str) -> ExpandedChecked {
    forge::expand_source(source, &CompileConfig::default(), CompileLimits::default())
        .unwrap_or_else(|failure| panic!("{failure:?}"))
}
fn tail<'a>(artifact: &'a ExpandedChecked, name: &str) -> &'a Expr {
    let function = artifact
        .checked()
        .parsed()
        .module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(f) if f.name == name => Some(f),
            _ => None,
        })
        .unwrap();
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &function.body.as_ref().unwrap().kind
    else {
        panic!("missing function tail")
    };
    tail
}
#[test]
fn unit_group_singleton_multiple_and_nested_tuples_have_actual_distinct_asts() {
    for (value, ty, arity) in [
        ("()", "()", Some(0)),
        ("(42)", "i64", None),
        ("(42,)", "(i64,)", Some(1)),
        ("(1,42,)", "(i64,i64)", Some(2)),
        ("((),(42,),\"comma, (λ)\")", "((),(i64,),String)", Some(3)),
    ] {
        let source =
            format!("macro value()->expr{{quote{{{value}}}}} fn target()->{ty}{{value!()}}");
        let artifact = compile(&source);
        match (arity, &tail(&artifact, "target").kind) {
            (Some(n), ExprKind::Tuple(values)) => assert_eq!(values.len(), n),
            (None, ExprKind::Literal(_)) => {}
            other => panic!("wrong actual shape: {other:?}"),
        }
        if arity == Some(1) {
            assert!(artifact.expanded_source().contains(",)"));
        }
        assert_eq!(artifact.checked().source(), artifact.expanded_source());
    }
}
#[test]
fn body_comparison_does_not_equate_grouping_singletons_order_or_unit_blocks() {
    let parsed = parser::parse(
        "fn grouped(){(42)} fn singleton(){(42,)} fn left(){(1,2)} fn right(){(2,1)} fn unit(){()} fn block(){{}}",
    );
    assert!(parsed.diagnostics.is_empty());
    let values: Vec<_> = parsed
        .module
        .items
        .iter()
        .map(|item| {
            let Item::Function(f) = item else { panic!() };
            let ExprKind::Block { tail: Some(t), .. } = &f.body.as_ref().unwrap().kind else {
                panic!()
            };
            t.as_ref()
        })
        .collect();
    for (a, b) in [(0, 1), (2, 3), (4, 5)] {
        assert!(!lower::shape_matches(values[a], values[b], Default::default()).unwrap());
    }
    assert!(lower::shape_matches(values[1], values[1], Default::default()).unwrap());
}
#[test]
fn tuple_slots_and_plain_parameter_identities_survive_repeated_splicing() {
    let source = "// 🦀 λ\nmacro repeat($x:expr)->expr{quote{($x,$x)}} fn target(in n:i64)->((i64,i64),(i64,i64)){repeat!((n,42))}";
    let artifact = compile(source);
    let syntax = macro_source::parse(source, Default::default()).unwrap();
    let invocation = syntax.invocations().first().unwrap();
    let argument = &invocation.arguments[0];
    assert_eq!(&source[argument.clone()], "(n,42)");
    let n = argument.start + 1;
    let anchors: Vec<_> = artifact
        .source_map()
        .iter()
        .filter(|entry| entry.original.start == n && entry.original.end == n + 1)
        .collect();
    assert_eq!(anchors.len(), 2);
    assert_ne!(anchors[0].expanded.start, anchors[1].expanded.start);
    for anchor in anchors {
        assert!(matches!(anchor.kind, forge::MapKind::Source));
    }
    let closes: Vec<_> = artifact
        .source_map()
        .iter()
        .filter(|entry| {
            entry.original.start == argument.end - 1
                && entry.original.end == argument.end
                && &artifact.expanded_source()[entry.expanded.start..entry.expanded.end] == ")"
        })
        .collect();
    assert_eq!(closes.len(), 2);
    let ExprKind::Tuple(outer) = &tail(&artifact, "target").kind else {
        panic!()
    };
    assert_eq!(outer.len(), 2);
    assert!(
        outer
            .iter()
            .all(|e| matches!(&e.kind, ExprKind::Tuple(v) if v.len()==2))
    );
}
#[test]
fn tuple_elements_keep_definition_globals_and_separate_block_scopes() {
    let source = "macro values()->expr{quote{({let helper=0;helper},helper())}} fn helper()->i64{42} fn target(in helper:i64)->(i64,i64){values!()}";
    let artifact = compile(source);
    let ExprKind::Tuple(values) = &tail(&artifact, "target").kind else {
        panic!()
    };
    assert_eq!(values.len(), 2);
    let ExprKind::Call { callee, .. } = &values[1].kind else {
        panic!()
    };
    assert!(matches!(&callee.kind, ExprKind::Name(n) if n=="helper"));
    let escaped = "macro keep($x:expr)->expr{quote{$x}} fn target()->(i64,i64){keep!(({let hidden=1;hidden},hidden))}";
    assert!(
        forge::expand_source(escaped, &CompileConfig::default(), CompileLimits::default()).is_err()
    );
}
#[test]
fn generated_calls_inside_nested_tuples_cannot_hide_current_effects() {
    let source = "macro wrap($x:expr)->expr{quote{((helper($x),),())}} fn helper(in n:u64)->u64 eff[Fail[ArithOverflow]]{wrapping_add(n,1)} fn target(in n:u64)->((u64,),()) eff[Fail[ArithOverflow]]{wrap!(n)}";
    assert_eq!(compile(source).audits().len(), 1);
    let changed = source.replace("wrapping_add(n,1)", "n+1");
    let failure = forge::expand_source(
        &changed,
        &CompileConfig::default(),
        CompileLimits::default(),
    )
    .unwrap_err();
    assert_eq!(failure.stage(), Stage::Authority, "{failure:?}");
}
#[test]
fn effectful_caller_tuple_slots_keep_source_origin_instead_of_macro_authority() {
    let source = "macro wrap($x:expr)->expr{quote{($x,)}} fn helper(in n:u64)->u64 eff[Fail[ArithOverflow]]{n+1} fn target(in n:u64)->(u64,) eff[Fail[ArithOverflow]]{wrap!(helper(n))}";
    let artifact = compile(source);
    assert_eq!(artifact.audits().len(), 1);
    let syntax = macro_source::parse(source, Default::default()).unwrap();
    let argument = &syntax.invocations()[0].arguments[0];
    let helper = argument.start;
    assert_eq!(&source[helper..helper + 6], "helper");
    assert!(
        artifact
            .source_map()
            .iter()
            .any(|entry| entry.original.start == helper
                && entry.original.end == helper + 6
                && matches!(entry.kind, forge::MapKind::Source))
    );
}
#[test]
fn real_checker_preserves_linear_tuple_product_modes_and_field_types() {
    let source = "linear struct Ticket{} macro build($x:expr)->expr{quote{($x,42)}} fn target(sink ticket:Ticket)->(Ticket,i64){build!(ticket)} fn consume(sink pair:(Ticket,i64))->i64{let (ticket,n)=pair;let Ticket{}=ticket;n} fn main()->i64{consume(target(Ticket{}))}";
    let artifact = compile(source);
    assert!(matches!(&tail(&artifact, "target").kind, ExprKind::Tuple(v) if v.len()==2));
    for (source, code) in [
        (
            "linear struct Ticket{} macro twice($x:expr)->expr{quote{($x,$x)}} fn target(sink ticket:Ticket)->(Ticket,Ticket){twice!(ticket)}",
            "E0303",
        ),
        (
            "linear struct Ticket{} macro discard($x:expr)->expr{quote{()}} fn target(sink ticket:Ticket)->(){discard!(ticket)}",
            "E0302",
        ),
        (
            "macro bad()->expr{quote{(true,42)}} fn target()->(i64,i64){bad!()}",
            "E0200",
        ),
    ] {
        let failure =
            forge::expand_source(source, &CompileConfig::default(), CompileLimits::default())
                .unwrap_err();
        assert_eq!(failure.stage(), Stage::Check, "{failure:?}");
        assert!(
            failure.diagnostics().iter().any(|d| d.code == code),
            "{failure:?}"
        );
    }
}
#[test]
fn tuple_arity_is_not_the_macro_parameter_limit() {
    let source = "macro values()->expr{quote{(0,1,2,3,4,5,6,7,8,42)}} fn target()->(i64,i64,i64,i64,i64,i64,i64,i64,i64,i64){values!()}";
    let artifact = compile(source);
    assert!(matches!(&tail(&artifact, "target").kind, ExprKind::Tuple(v) if v.len()==10));
    let f = &artifact.checked().world().functions["target"];
    assert_eq!(f.result, Type::Tuple(vec![Type::named("i64"); 10]));
}
#[test]
fn malformed_deep_and_tight_output_tuples_never_return_partial_artifacts() {
    for value in ["(,)", "(1,,2)", "(1,2]", "(1 2)"] {
        let source = format!("macro value()->expr{{quote{{{value}}}}} fn target(){{value!()}}");
        assert!(
            forge::expand_source(&source, &CompileConfig::default(), CompileLimits::default())
                .is_err()
        );
    }
    let nested = format!("{}42{}", "(".repeat(100), ",)".repeat(100));
    let source = format!("macro value()->expr{{quote{{{nested}}}}} fn target(){{value!()}}");
    assert!(
        forge::expand_source(&source, &CompileConfig::default(), CompileLimits::default()).is_err()
    );
    let source = "macro value()->expr{quote{((42,),())}} fn target()->((i64,),()){value!()}";
    let exact = compile(source).expanded_source().len();
    assert!(
        forge::expand_source(
            source,
            &CompileConfig::default(),
            CompileLimits {
                max_expanded_bytes: exact,
                ..CompileLimits::default()
            }
        )
        .is_ok()
    );
    for limits in [
        CompileLimits {
            max_expanded_bytes: exact - 1,
            ..CompileLimits::default()
        },
        CompileLimits {
            max_metadata_bytes: 1,
            ..CompileLimits::default()
        },
        CompileLimits {
            max_work: 1,
            ..CompileLimits::default()
        },
    ] {
        assert!(forge::expand_source(source, &CompileConfig::default(), limits).is_err());
    }
}

// This exercises the real sealed lowering independently of the editor aggregate
// admission guard. It does not construct or claim an accepted CheckedSource.
fn lower_actual(source: &str) -> (lower::LoweredBody, String) {
    use vesper::forge_frontend::authority::{self, AuthorityLimits};
    use vesper_forge::{
        HygieneHost, InvocationSite, SessionId,
        typed_quote::{TypedBudget, TypedLimits},
    };
    let syntax = macro_source::parse(source, Default::default()).unwrap();
    let bindings =
        authority::bind_source(&syntax, "@local/source", AuthorityLimits::default()).unwrap();
    let mut host = HygieneHost::new(SessionId(11), bindings.registry().clone());
    for name in bindings.global_names() {
        if let Some(def) = bindings.def_id(name) {
            host.declare_global(bindings.source_package(), name, def)
                .unwrap();
        }
    }
    let mut budget = TypedBudget::new(TypedLimits::default()).unwrap();
    let imported = host
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
        let slots = definition
            .parameters
            .iter()
            .map(|p| p.name_span.clone())
            .collect::<Vec<_>>();
        let template = host
            .expr_quotation(
                &imported,
                def,
                definition.quote_span.clone(),
                &slots,
                &mut budget,
            )
            .unwrap();
        let inputs = invocation
            .arguments
            .iter()
            .map(|span| {
                host.expr_input(&imported, span.clone(), &mut budget)
                    .unwrap()
            })
            .collect();
        let expansion = host
            .begin_expr_expansion(
                def,
                InvocationSite::Source(bindings.source_package()),
                inputs,
                &mut budget,
            )
            .unwrap();
        let value = host
            .finish_expr(expansion, template.items(), &mut budget)
            .unwrap();
        replacements.push((invocation.span.clone(), value));
    }
    let resolved = host
        .resolve_expr_function_body(&imported, &syntax, function.id, replacements, &mut budget)
        .unwrap();
    let lowered = lower::lower_body(&resolved, &host, &bindings, Default::default()).unwrap();
    let mut edits = syntax
        .definitions()
        .iter()
        .map(|d| (d.span.clone(), ""))
        .collect::<Vec<_>>();
    edits.push((function.body_span.clone(), lowered.source()));
    edits.extend(
        lowered
            .parameter_renames()
            .iter()
            .map(|r| (r.original().clone(), r.name())),
    );
    edits.sort_by_key(|e| e.0.start);
    let mut candidate = String::new();
    let mut cursor = 0;
    for (span, value) in edits {
        assert!(span.start >= cursor);
        candidate.push_str(&source[cursor..span.start]);
        candidate.push_str(value);
        cursor = span.end;
    }
    candidate.push_str(&source[cursor..]);
    (lowered, candidate)
}
#[test]
fn actual_sealed_lowering_reparses_and_matches_real_tuple_checker_asts() {
    for source in [
        "macro v()->expr{quote{()}} fn target()->(){v!()}",
        "macro v()->expr{quote{(42)}} fn target()->i64{v!()}",
        "macro v()->expr{quote{(42,)}} fn target()->(i64,){v!()}",
        "macro v()->expr{quote{(1,42)}} fn target()->(i64,i64){v!()}",
        "macro v()->expr{quote{((),(42,),\"comma, (λ)\")}} fn target()->((),(i64,),String){v!()}",
        "macro v($x:expr)->expr{quote{($x,$x)}} fn target(in x:i64)->(i64,i64){v!(x)}",
        "macro v($x:expr)->expr{quote{($x,)}} fn target(in x:(i64,i64))->((i64,i64),){v!(x)}",
        "macro v()->expr{quote{({let helper=0;helper},helper())}} fn helper()->i64{42} fn target(in helper:i64)->(i64,i64){v!()}",
    ] {
        let (lowered, candidate) = lower_actual(source);
        let parsed = parser::parse(&candidate);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let body = parsed
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "target" => f.body.as_ref(),
                _ => None,
            })
            .unwrap();
        assert!(lowered.shape_matches(body, Default::default()).unwrap());
        for segment in lowered.segments() {
            assert!(lowered.source().get(segment.span.clone()).is_some());
        }
    }
}
