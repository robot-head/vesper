use vesper_doc::*;
fn function(name: &str, ty: Type, result: Type) -> FunctionDoc {
    FunctionDoc {
        name: name.into(),
        docs: String::new(),
        span: 0..0,
        generics: vec![],
        parameters: vec![Parameter {
            name: "p".into(),
            pass: Pass::In,
            mode: Mode::Copy,
            ty,
            selector: Selector::All,
        }],
        declared_result: result.clone(),
        result,
        result_mode: Mode::Copy,
        effects: Effects::default(),
        effect_resumptions: vec![],
        unsupported: vec![],
    }
}
fn query(ty: Type, result: Type) -> Query {
    Query {
        parameters: vec![QueryParameter {
            pass: Pass::In,
            mode: Mode::Copy,
            ty,
            selector: Selector::All,
        }],
        result,
        result_max_mode: Mode::Copy,
        effect_ceiling: Effects::default(),
    }
}
#[test]
fn complete_types_and_nested_arrow_modes_passes_and_results_match_structurally() {
    let base = function("good", Type::named("i64"), Type::named("bool"));
    let mut candidates = vec![
        base.clone(),
        function("wrong_parameter", Type::named("bool"), Type::named("bool")),
        function("wrong_result", Type::named("i64"), Type::named("i64")),
    ];
    let mut wrong_pass = base;
    wrong_pass.name = "wrong_pass".into();
    wrong_pass.parameters[0].pass = Pass::Sink;
    candidates.push(wrong_pass);
    let result = search(
        &query(Type::named("i64"), Type::named("bool")),
        &candidates,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        result
            .matches
            .iter()
            .map(|h| h.function.as_str())
            .collect::<Vec<_>>(),
        ["good"]
    );
    let arrow = Type::Function {
        params: vec![(Pass::In, Type::named("i64"))],
        result: Box::new(Type::named("bool")),
        effects: Effects::default(),
        mode: Mode::Copy,
    };
    let good = function("arrow", arrow.clone(), Type::unit());
    let mut changed = arrow.clone();
    if let Type::Function { params, .. } = &mut changed {
        params[0].0 = Pass::Sink;
    }
    let bad = function("arrow_sink", changed, Type::unit());
    assert_eq!(
        search(
            &query(arrow, Type::named("Unit")),
            &[bad, good],
            Limits::default()
        )
        .unwrap()
        .matches[0]
            .function,
        "arrow"
    );
}
#[test]
fn effect_instances_labels_arguments_and_selector_ceilings_are_preserved() {
    let mut f = function("left", Type::named("Pair"), Type::unit());
    f.parameters[0].pass = Pass::Inout;
    f.parameters[0].selector = Selector::Paths(vec![vec!["left".into()]]);
    let cap = Effect {
        label: Some("disk".into()),
        name: "Storage".into(),
        args: vec![Type::named("u64")],
    };
    f.effects.instances.push(cap.clone());
    f.effect_resumptions.push(Some(Mode::Linear));
    let mut q = query(Type::named("Pair"), Type::unit());
    q.parameters[0].pass = Pass::Inout;
    q.parameters[0].selector = Selector::Paths(vec![vec!["left".into()]]);
    q.effect_ceiling.instances.push(cap);
    assert_eq!(
        search(&q, &[f.clone()], Limits::default())
            .unwrap()
            .matches
            .len(),
        1
    );
    for change in [0, 1, 2, 3] {
        let mut bad = q.clone();
        match change {
            0 => bad.effect_ceiling.instances[0].label = Some("other".into()),
            1 => bad.effect_ceiling.instances[0].args[0] = Type::named("i64"),
            2 => bad.parameters[0].selector = Selector::Paths(vec![vec!["right".into()]]),
            _ => bad.effect_ceiling.instances.clear(),
        };
        assert!(
            search(&bad, &[f.clone()], Limits::default())
                .unwrap()
                .matches
                .is_empty()
        );
    }
    let mut broad = f.clone();
    broad.name = "all_fields".into();
    broad.parameters[0].selector = Selector::All;
    assert!(
        search(&q, &[broad], Limits::default())
            .unwrap()
            .matches
            .is_empty()
    );
}
#[test]
fn open_rows_generics_inference_and_effectful_arrow_binders_fail_explicitly() {
    let mut generic = function("generic", Type::named("T"), Type::named("T"));
    generic.generics.push(Generic {
        name: "T".into(),
        bound: Mode::Linear,
        row: false,
    });
    let mut open = function("open", Type::named("i64"), Type::unit());
    open.effects.tail = Some("r".into());
    let infer = function("infer", Type::Infer, Type::unit());
    let arrow = Type::Function {
        params: vec![],
        result: Box::new(Type::unit()),
        effects: Effects {
            instances: vec![Effect {
                label: None,
                name: "Log".into(),
                args: vec![],
            }],
            tail: None,
        },
        mode: Mode::Copy,
    };
    let latent = function("latent", arrow.clone(), Type::unit());
    let result = search(
        &query(Type::named("i64"), Type::unit()),
        &[generic, open, infer, latent],
        Limits::default(),
    )
    .unwrap();
    assert!(result.matches.is_empty());
    assert_eq!(
        result.unsupported_functions,
        ["generic", "infer", "latent", "open"]
    );
    assert!(search(&query(arrow, Type::unit()), &[], Limits::default()).is_err());
}
#[test]
fn graph_validates_api_identities_and_terminal_existence_does_not_erase_cycles() {
    let mut close = function("close", Type::named("Tx"), Type::unit());
    close.parameters[0].pass = Pass::Sink;
    close.parameters[0].mode = Mode::Linear;
    let mut cycle = close.clone();
    cycle.name = "cycle".into();
    cycle.result = Type::named("Tx");
    cycle.result_mode = Mode::Linear;
    let functions = vec![close, cycle];
    let graph = TypestateGraph::from_functions(&functions, Limits::default()).unwrap();
    assert_eq!(graph.edges.len(), 2);
    assert!(graph.reachability(&functions, Limits::default()).unwrap()[0].terminal_path_exists);
    let mut bad = graph.clone();
    bad.states.push(bad.states[0].clone());
    assert!(bad.validate(&functions, Limits::default()).is_err());
    let mut bad = graph.clone();
    bad.edges[0].function = "invented".into();
    assert!(bad.validate(&functions, Limits::default()).is_err());
    let mut bad = graph.clone();
    bad.edges[0].to = Some("invented".into());
    assert!(bad.validate(&functions, Limits::default()).is_err());
    let mut bad = graph.clone();
    bad.edges[0].parameter = 99;
    assert!(bad.validate(&functions, Limits::default()).is_err());
    let mut bad = graph;
    bad.states[0].identity = "Tx".into();
    assert!(bad.validate(&functions, Limits::default()).is_err());
}
#[test]
fn comment_tokens_cannot_be_forged_by_strings_and_blank_lines_break_attachment() {
    let s = "//! Module docs\n/// item <script> & \"quote\"\nfn first() {}\nfn second() { \"/// fake\"; }\n/// unattached\n\nfn third() {}";
    assert_eq!(
        comments(s, s.find("fn first").unwrap()).1,
        "item <script> & \"quote\""
    );
    assert!(comments(s, s.find("fn third").unwrap()).1.is_empty());
    assert_eq!(comments(s, s.len() + 1).0, "Module docs");
}
#[test]
fn escaping_determinism_budgets_and_ranking_have_no_certificate_boolean() {
    let payload = serde_json::json!({"docs":"<script>alert('x') & \"unsafe\"</script>"});
    let html = render_html(&payload, Limits::default()).unwrap();
    assert_eq!(html, render_html(&payload, Limits::default()).unwrap());
    let html = String::from_utf8(html).unwrap();
    assert!(!html.contains("<script>"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("src=\"http"));
    assert!(
        render_html(
            &payload,
            Limits {
                max_output_bytes: 10,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(source_preflight(&"x".repeat(131_073), Limits::default()).is_err());
    assert!(
        source_preflight(
            &format!("fn f() {{{}0{}}}", "(".repeat(49), ")".repeat(49)),
            Limits::default()
        )
        .is_err()
    );
    let a = function("a", Type::named("i64"), Type::unit());
    let mut b = a.clone();
    b.name = "b".into();
    let q = query(Type::named("i64"), Type::unit());
    let hits = search(&q, &[b, a], Limits::default()).unwrap().matches;
    assert_eq!(
        hits.iter().map(|h| h.function.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
}

#[test]
fn unknown_nested_query_contracts_cannot_be_silently_ignored() {
    let q = query(Type::named("i64"), Type::unit());
    let mut value = serde_json::to_value(&q).unwrap();
    assert!(parse_query_json(&value.to_string(), Limits::default()).is_ok());
    value["effect_ceiling"]["unknown_row"] = "r".into();
    assert!(
        parse_query_json(&value.to_string(), Limits::default())
            .unwrap_err()
            .contains("unsupported query contract field")
    );
    let mut value = serde_json::to_value(&q).unwrap();
    value["parameters"][0]["ty"] = serde_json::json!({"Function":{"params":[],"result":{"Tuple":[]},"effects":{"instances":[],"tail":null},"mode":"Copy","verified":true}});
    assert!(
        parse_query_json(&value.to_string(), Limits::default())
            .unwrap_err()
            .contains("verified")
    );
}
#[test]
fn exact_effect_contract_ranks_before_allowed_subset_and_typestate_depth_is_checked() {
    let pure = function("a", Type::named("i64"), Type::unit());
    let mut effectful = pure.clone();
    effectful.name = "z".into();
    let effect = Effect {
        label: None,
        name: "Log".into(),
        args: vec![],
    };
    effectful.effects.instances.push(effect.clone());
    effectful.effect_resumptions.push(Some(Mode::Linear));
    let mut q = query(Type::named("i64"), Type::unit());
    q.effect_ceiling.instances.push(effect);
    let result = search(&q, &[pure, effectful], Limits::default()).unwrap();
    assert_eq!(result.matches[0].function, "z");
    assert_eq!(result.matches[0].score, 10);
    assert_eq!(result.matches[1].score, 0);
    let mut deep = Type::named("End");
    for _ in 0..50 {
        deep = Type::Named("Wrap".into(), vec![deep]);
    }
    let graph = TypestateGraph {
        states: vec![State {
            identity: "invalid".into(),
            ty: deep,
        }],
        ..Default::default()
    };
    assert!(
        graph
            .validate(&[], Limits::default())
            .unwrap_err()
            .contains("budget")
    );
}

#[test]
fn selector_order_is_semantic_and_identity_helper_is_bounded() {
    let mut f = function("fields", Type::named("Pair"), Type::unit());
    f.parameters[0].selector = Selector::Paths(vec![vec!["left".into()], vec!["right".into()]]);
    let mut q = query(Type::named("Pair"), Type::unit());
    q.parameters[0].selector = Selector::Paths(vec![vec!["right".into()], vec!["left".into()]]);
    assert_eq!(
        search(&q, &[f], Limits::default()).unwrap().matches[0].score,
        10
    );
    let mut deep = Type::named("End");
    for _ in 0..50 {
        deep = Type::Named("Wrap".into(), vec![deep]);
    }
    assert!(type_identity(&deep).is_err());
}

#[test]
fn direct_typed_queries_and_candidates_cannot_bypass_effect_text_count_budgets() {
    let mut q = query(Type::named("i64"), Type::unit());
    let effect = Effect {
        label: None,
        name: "Log".into(),
        args: vec![],
    };
    q.effect_ceiling.instances = vec![effect.clone(); 65];
    assert!(
        search(&q, &[], Limits::default())
            .unwrap_err()
            .contains("effect instance")
    );
    q.effect_ceiling.instances = vec![Effect {
        label: None,
        name: "x".repeat(257),
        args: vec![],
    }];
    assert!(
        search(&q, &[], Limits::default())
            .unwrap_err()
            .contains("effect name")
    );
    q.effect_ceiling.instances = vec![Effect {
        label: Some("x".repeat(129)),
        ..effect.clone()
    }];
    assert!(
        search(&q, &[], Limits::default())
            .unwrap_err()
            .contains("effect name")
    );
    let mut f = function("bad", Type::named("i64"), Type::unit());
    f.effects.instances = vec![Effect {
        label: None,
        name: "x".repeat(257),
        args: vec![],
    }];
    f.effect_resumptions.push(Some(Mode::Linear));
    assert!(
        search(
            &query(Type::named("i64"), Type::unit()),
            &[f],
            Limits::default()
        )
        .is_err()
    );
}

#[test]
fn unsupported_row_contents_still_obey_complete_structure_budgets() {
    let mut deep = Type::named("u64");
    for _ in 0..50 {
        deep = Type::Row(Effects {
            instances: vec![Effect {
                label: None,
                name: "Nested".into(),
                args: vec![deep],
            }],
            tail: None,
        });
    }
    let f = function("deep_row", deep.clone(), Type::unit());
    assert!(
        search(
            &query(Type::named("i64"), Type::unit()),
            &[f],
            Limits::default()
        )
        .unwrap_err()
        .contains("budget")
    );
    assert!(type_identity(&deep).is_err());
}

#[test]
fn duplicate_decoded_query_keys_cannot_erase_unknown_constraints() {
    let q = query(Type::named("i64"), Type::unit());
    let text = serde_json::to_string(&q).unwrap();
    let duplicate = text.replacen(
        "\"effect_ceiling\":",
        "\"effect_ceiling\":{\"unknown_contract\":true},\"effect_ceiling\":",
        1,
    );
    assert!(
        parse_query_json(&duplicate, Limits::default())
            .unwrap_err()
            .contains("duplicate decoded")
    );
    let duplicate = text.replacen(
        "\"parameters\":",
        "\"parameters\":[],\"par\\u0061meters\":",
        1,
    );
    assert!(
        parse_query_json(&duplicate, Limits::default())
            .unwrap_err()
            .contains("duplicate decoded")
    );
}
#[test]
fn weighted_segments_reject_nested_block_left_spines_before_parse() {
    let mut expression = "0".to_owned();
    for _ in 0..40 {
        expression = format!("{{{expression}}}{}", "+1".repeat(100));
    }
    let source = format!("fn f() -> i64 {{ {expression} }}");
    assert!(
        source_preflight(&source, Limits::default())
            .unwrap_err()
            .contains("weighted")
    );
}

#[test]
fn completed_inner_statements_cannot_hide_recursive_clone_depth() {
    let mut expression = "0".to_owned();
    for _ in 0..40 {
        expression = format!("{{{expression};0}}{}", "+1".repeat(100));
    }
    assert!(
        source_preflight(
            &format!("fn f() -> i64 {{ {expression} }}"),
            Limits::default()
        )
        .unwrap_err()
        .contains("weighted")
    );
}
