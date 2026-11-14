//! Genuine parser/checker snapshots and early numeric tuple admission.
use vesper::{
    ast::{ExprKind, Item, Type},
    check, parser,
    tooling::frontend::{AnalysisFailure, AnalysisLimits, check_source},
};
fn accepted(source: &str) {
    let snapshot = check_source(source, AnalysisLimits::default())
        .unwrap_or_else(|e| panic!("{source}: {e:?}"));
    assert_eq!(snapshot.source(), source);
    assert_eq!(
        snapshot.source_hash(),
        *blake3::hash(source.as_bytes()).as_bytes()
    );
    assert!(snapshot.checked().diagnostics.is_empty());
    assert!(std::ptr::eq(snapshot.world(), &snapshot.checked().world));
}
#[test]
fn bounded_shapes_parameters_helpers_and_destructuring_reach_actual_checker() {
    for source in [
        "fn unit()->(){()} fn one()->(i64,){(42,)} fn pair()->(i64,bool){(42,true)}",
        "fn pair(in n:i64)->(i64,i64){(n,42)} fn use_pair(in p:(i64,i64))->i64{let (a,b)=p;a} fn main()->i64{use_pair(pair(0))}",
        "fn nest(in p:(i64,i64))->((i64,i64),()){(p,())}",
        "fn f()->(i64,i64){let x=42;let x=(x,x);x}",
        "fn f()->(i64,i64){let x=42;let y={let x=true;(x,);};(x,x)}",
        "fn f(in c:bool)->(i64,i64){if c {(1,2)} else {(3,4)}}",
        "fn f(in c:bool)->(i64,i64){match c{true=>(1,2),false=>(3,4)}}",
        "fn update(inout p:(i64,i64)){} fn f()->(i64,i64){let p=(1,42);update(inout p);update(inout p);p}",
        "copy struct Boxed{value:(i64,i64)} fn f(in p:Boxed)->((i64,i64),){(p.value,)}",
        "copy enum Mark{Ready} fn f()->(Mark,){(Mark::Ready,)}",
    ] {
        accepted(source);
    }
}
#[test]
fn snapshots_contain_the_actual_tuple_type_and_ast() {
    let snapshot = check_source("fn f()->(i64,bool){(42,true)}", Default::default()).unwrap();
    let Item::Function(f) = &snapshot.parsed().module.items[0] else {
        panic!()
    };
    let ExprKind::Block { tail: Some(t), .. } = &f.body.as_ref().unwrap().kind else {
        panic!()
    };
    assert!(matches!(&t.kind, ExprKind::Tuple(es) if es.len() == 2));
    assert!(
        matches!(&snapshot.checked().expression_types[&(t.span.start,t.span.end)], Type::Tuple(ts) if ts.len() == 2)
    );
}
#[test]
fn tuple_semantic_errors_are_the_unchanged_real_checker_diagnostics() {
    for (source, code) in [
        ("fn f()->(i64,i64){(true,42)}", "E0200"),
        (
            "linear struct Ticket{} fn f(sink t:Ticket)->(Ticket,Ticket){(t,t)}",
            "E0303",
        ),
        (
            "linear struct Ticket{} fn f(sink t:Ticket)->(){(42,);()}",
            "E0302",
        ),
        ("fn f()->(i64,i64){({let hidden=1;hidden},hidden)}", "E0201"),
        ("fn f()->(i64,i64){let x=1;let x=(x,true);x}", "E0200"),
    ] {
        let parsed = parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let ordinary = check::check(&parsed.module);
        assert!(ordinary.diagnostics.iter().any(|d| d.code == code));
        let AnalysisFailure::Semantic { diagnostics } =
            check_source(source, Default::default()).unwrap_err()
        else {
            panic!("tuple must reach the ordinary checker: {source}")
        };
        assert_eq!(diagnostics, ordinary.diagnostics);
    }
}
#[test]
fn linear_product_construction_and_consumption_are_accepted() {
    accepted(
        "linear struct Ticket{} fn build(sink t:Ticket)->(Ticket,i64){(t,42)} fn consume(sink p:(Ticket,i64))->i64{let (t,n)=p;let Ticket{}=t;n} fn main()->i64{consume(build(Ticket{}))}",
    );
}
#[test]
fn exponential_growth_refuses_before_semantic_diagnostics() {
    for prefix in [
        "fn f(){let x=0;",
        "fn f(in x:(i64,i64)){",
        "fn pair()->(i64,i64){(1,2)} fn f(){let x=pair();",
    ] {
        let mut source = prefix.to_owned();
        // A semantic error earlier in the body must not distract the admission
        // guard into entering the checker on the later doubling adversary.
        source.push_str("unknown;");
        for _ in 0..25 {
            source.push_str("let x=(x,x);");
        }
        source.push('}');
        let failure = check_source(&source, Default::default()).unwrap_err();
        let AnalysisFailure::Resource { reason } = failure else {
            panic!("{failure:?}")
        };
        assert!(reason.contains("before checking"), "{reason}");
    }
}
#[test]
fn tight_type_depth_and_metadata_limits_refuse_without_snapshots() {
    let mut deep = "fn f(){let x=0;".to_owned();
    for _ in 0..15 {
        deep.push_str("let x=(x,);");
    }
    deep.push('}');
    let failure = check_source(
        &deep,
        AnalysisLimits {
            max_depth: 12,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        matches!(failure, AnalysisFailure::Resource { ref reason } if reason.contains("before checking"))
    );
    let source = "fn f(){let x=0;let x=(x,x);let x=(x,x);let x=(x,x);let x=(x,x);}";
    for limits in [
        AnalysisLimits {
            max_type_nodes: 8,
            ..Default::default()
        },
        AnalysisLimits {
            max_depth: 4,
            ..Default::default()
        },
        AnalysisLimits {
            max_metadata_bytes: 3000,
            ..Default::default()
        },
    ] {
        assert!(matches!(
            check_source(source, limits),
            Err(AnalysisFailure::Resource { .. })
        ));
    }
}
#[test]
fn unresolved_calls_higher_order_and_generic_projection_refuse_explicitly() {
    for source in [
        "fn f(){let x=unknown();(x,x)}",
        "fn f(in g:fn()->(i64,i64)){let x=g();(x,x)}",
        "fn f(){let x=(1,2);(x[0],)}",
        "trait Pair{fn pair(in self)->(i64,i64);} fn f()->(i64,){(1,)}",
        "copy struct P{} impl P{fn pair(in self)->(i64,i64){(1,2)}}",
        "fn helper()->i64{1} fn helper()->(i64,i64){(1,2)} fn f(){(helper(),)}",
    ] {
        let result = check_source(source, Default::default());
        assert!(
            matches!(result, Err(AnalysisFailure::Unsupported { .. })),
            "{source}: {result:?}"
        );
    }
}
#[test]
fn generic_projection_is_bounded_and_reaches_the_checker() {
    // Generic nominal fields are bounded by substituting the value's type
    // arguments, so an unknown field is the checker's diagnostic.
    let result = check_source(
        "fn f(in p:Option[(i64,i64)]){(p.value,)}",
        Default::default(),
    );
    assert!(
        matches!(result, Err(AnalysisFailure::Semantic { ref diagnostics }) if diagnostics[0].code == "E0202"),
        "{result:?}"
    );
}
#[test]
fn ill_typed_wrapping_calls_cannot_hide_tuple_growth() {
    let mut source = "fn f(){let x=(1,2);".to_owned();
    for _ in 0..25 {
        source.push_str("let x=(wrapping_add(x,0),x);");
    }
    source.push('}');
    assert!(matches!(
        check_source(&source, Default::default()),
        Err(AnalysisFailure::Resource { .. })
    ));
}
#[test]
fn bounded_function_values_include_parameters_results_and_effect_arguments() {
    accepted("fn helper(in p:i64)->i64 eff[Fail[ArithOverflow]]{p} fn f(){(helper,helper);}");
    accepted(
        "fn helper(in p:(i64,i64))->(i64,i64){p} fn f()->((i64,i64),){let g=helper;(g((1,42)),)}",
    );
    let source = "fn helper(in p:(i64,i64))->(i64,i64){p} fn f(){(helper,helper)}";
    let failure = check_source(
        source,
        AnalysisLimits {
            max_type_nodes: 12,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(failure, AnalysisFailure::Resource { .. }));
}

#[test]
fn duplicate_nominal_and_field_inventories_refuse_before_checker() {
    let wide = format!("({})", ["i64"; 12].join(","));
    for declarations in [
        format!("copy struct Boxed{{value:i64}} copy struct Boxed{{value:{wide}}}"),
        format!("copy struct Boxed{{value:i64,value:{wide}}}"),
        "copy struct Option{}".to_owned(),
    ] {
        let source =
            format!("{declarations} fn f(in b:Boxed){{let x=b.value;let x=(x,x);let x=(x,x);}}");
        let failure = check_source(
            &source,
            AnalysisLimits {
                max_type_nodes: 64,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(failure, AnalysisFailure::Unsupported { ref reason }
            if reason.contains("before checking")),
            "{failure:?}"
        );
    }
}

#[test]
fn malformed_nominal_contexts_never_clone_wide_expected_types_in_checker() {
    let wide = format!("({})", ["i64"; 20].join(","));
    for (declaration, result, body) in [
        (
            "copy struct Empty{}",
            "Empty",
            "if true{if true{Empty}else{Empty}}else{if true{Empty}else{Empty}}",
        ),
        ("", "i64", "1"),
        ("", "f64", "1.0+2.0"),
        ("", "String", "\"text\""),
    ] {
        let source =
            format!("{declaration} fn trigger(){{(0,);}} fn f()->{result}[{wide}]{{{body}}}");
        let parsed = parser::parse(&source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let failure = check_source(
            &source,
            AnalysisLimits {
                max_type_nodes: 64,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(failure, AnalysisFailure::Unsupported { ref reason }
            if reason.contains("before checking")),
            "{failure:?}"
        );
    }
    for source in [
        "copy enum Capsule[T]{Empty} fn f(){let x=Capsule::Extra::Empty;(x,x)}",
        "fn f(){let x=Option::Extra::None;(x,x)}",
    ] {
        assert!(matches!(
            check_source(source, Default::default()),
            Err(AnalysisFailure::Parse { .. })
        ));
    }
    // String payload length never changes the actual scalar String type.
    accepted(&format!("fn f()->(String,){{(\"{}\",)}}", "λ".repeat(1000)));
}

#[test]
fn short_circuit_rhs_prepays_environment_copies_even_when_dead() {
    let wide = format!("({})", ["i64"; 12].join(","));
    let limits = AnalysisLimits {
        max_metadata_bytes: 20_000,
        ..Default::default()
    };
    for operator in ["&&", "||"] {
        let mut expression = "true".to_owned();
        for _ in 0..6 {
            expression = format!("false {operator} ({expression})");
        }
        let source = format!("fn trigger(){{(0,);}} fn f(in unused:{wide})->bool{{{expression}}}");
        let failure = check_source(&source, limits).unwrap_err();
        assert!(
            matches!(failure, AnalysisFailure::Resource { ref reason }
            if reason.contains("before checking")),
            "{failure:?}"
        );
        check_source(
            &format!("fn trigger(){{(0,);}} fn f()->bool{{{expression}}}"),
            limits,
        )
        .unwrap();
    }
    check_source(
        &format!("fn trigger(){{(0,);}} fn f(in unused:{wide})->bool{{true}}"),
        limits,
    )
    .unwrap();
}

#[test]
fn header_and_annotation_holes_refuse_while_initializer_inference_remains_supported() {
    let values = ["0"; 12].join(",");
    let mut inferred = format!("fn grow()->_{{({values})}} fn main(){{let x=grow;");
    for _ in 0..4 {
        inferred.push_str("let x=(x,x);");
    }
    inferred.push('}');
    for source in [
        inferred.as_str(),
        "fn f(in p:(_,i64)){(p,p);}",
        "fn f()->(i64,_){(1,2)}",
        "fn f(){let x:(_,i64)=(true,42);(x,x);}",
        "fn f(){let x:(i64,(_,i64))=(1,(true,42));(x,x);}",
    ] {
        let parsed = parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let failure = check_source(
            source,
            AnalysisLimits {
                max_type_nodes: 64,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(failure, AnalysisFailure::Unsupported { ref reason }
            if reason.contains("before checking")),
            "{source}: {failure:?}"
        );
    }
    accepted("fn f()->((i64,bool),(i64,bool)){let x=(42,true);(x,x)}");
    accepted("fn helper(in n:i64)->i64{n} fn f()->(i64,i64){let g=helper;let x=g(42);(x,x)}");
}
