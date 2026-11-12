mod common;

use common::*;
use vesper_forge::*;

fn run(
    h: &mut HygieneHost,
    quote: Quotation,
    p: &PolicySet,
) -> Result<AcceptedExpansion, AuthorityRejection> {
    let toks = expand_in_app(h, "", &[], |_| quote).unwrap();
    let f = h.resolve(toks).unwrap();
    check_authority(h.session(), f, &graph(), p)
}

fn exceeded(r: &AuthorityRejection) -> Vec<(Authority, Vec<Denial>)> {
    r.violations
        .iter()
        .filter_map(|v| match v {
            Violation::Exceeds {
                requirement,
                denied_by,
                ..
            } => Some((requirement.clone(), denied_by.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn pure_output_is_accepted_under_empty_emits() {
    let mut h = host();
    let ok = run(
        &mut h,
        Quotation::new().text("hash_into(1)"),
        &policies(vec![]),
    )
    .unwrap();
    assert_eq!(ok.audit.operations.len(), 1);
    assert!(ok.audit.operations[0].requirements.is_empty());
    assert_eq!(ok.audit.policies[0].macro_policy_version, 3);
    assert_eq!(ok.audit.policies[0].caller_ceiling_version, 11);
    assert_eq!(ok.audit.graph_version, "graph-v1");
}

#[test]
fn privileged_call_from_macros_own_package_is_checked() {
    let mut h = host();
    let err = run(
        &mut h,
        call_claimed("@vesper-wire/derive::phone_home", PHONE_HOME),
        &policies(vec![]),
    )
    .unwrap_err();
    assert_eq!(
        exceeded(&err),
        vec![(
            net(),
            vec![Denial::MacroEmits {
                macro_def: DERIVE_MACRO,
                version: 3
            }]
        )]
    );
    // Unqualified lexical resolution to the same definition is checked too.
    let mut h = host();
    assert!(
        run(
            &mut h,
            Quotation::new().text("phone_home()"),
            &policies(vec![])
        )
        .is_err()
    );
}

#[test]
fn injected_dependency_call_is_checked_through_cycles() {
    let mut h = host();
    let err = run(
        &mut h,
        call_claimed("@util/misc::sneaky", SNEAKY),
        &policies(vec![]),
    )
    .unwrap_err();
    match &err.violations[..] {
        [
            Violation::Exceeds {
                requirement,
                witness,
                ..
            },
        ] => {
            assert_eq!(*requirement, net());
            assert_eq!(*witness, vec![SNEAKY, CYCLE_A, CYCLE_B, CONNECT]);
        }
        v => panic!("{v:?}"),
    }
    // Directly injected effect operation from another package.
    let mut h = host();
    assert!(
        run(
            &mut h,
            call_claimed("@std/net::connect", CONNECT),
            &policies(vec![])
        )
        .is_err()
    );
}

#[test]
fn empty_emits_in_privileged_caller_rejects_and_intersection_accepts() {
    let q = || call_claimed("@util/misc::sneaky", SNEAKY);
    // Caller holds Net, macro declares nothing: rejected.
    let mut h = host();
    assert!(run(&mut h, q(), &policies(vec![])).is_err());
    // Macro declares Net and the caller holds it: accepted with audit.
    let mut h = host();
    let ok = run(&mut h, q(), &policies(vec![exact(net())])).unwrap();
    assert_eq!(ok.audit.operations[0].requirements, vec![net()]);
    // Macro declares Net but the caller does not hold it: rejected by ceiling.
    let mut h = host();
    let p = PolicySet::new()
        .macro_policy(emits(DERIVE_MACRO, vec![exact(net())]))
        .ceiling(ceiling(APP, vec![]));
    let err = run(&mut h, q(), &p).unwrap_err();
    assert_eq!(
        exceeded(&err),
        vec![(
            net(),
            vec![Denial::CallerCeiling {
                package: APP,
                version: 11
            }]
        )]
    );
}

#[test]
fn ffi_authorities_are_typed() {
    let p = || policies(vec![exact(ffi(LIB_A))]);
    let mut h = host();
    run(&mut h, call_claimed("@std/ffi::call_a", FFI_CALL_A), &p()).unwrap();
    let mut h = host();
    let err = run(&mut h, call_claimed("@std/ffi::call_b", FFI_CALL_B), &p()).unwrap_err();
    assert_eq!(exceeded(&err)[0].0, ffi(LIB_B));
    assert_ne!(ffi(LIB_A), ffi(LIB_B));
}

#[test]
fn local_handlers_do_not_remove_authority() {
    // `quiet` handles Net internally (empty residual row) but still uses it.
    let mut h = host();
    let err = run(
        &mut h,
        call_claimed("@util/misc::quiet", QUIET),
        &policies(vec![]),
    )
    .unwrap_err();
    assert_eq!(exceeded(&err)[0].0, net());

    // Macro-constructed privileged handler around harmless code.
    let mut h = host();
    let q = Quotation::new()
        .text("handle")
        .token(claimed("@std/net::Net", NET_EFFECT))
        .text("with")
        .token(claimed("@std/net::tcp_handler", TCP_HANDLER))
        .text("{ 1 }");
    let err = run(&mut h, q, &policies(vec![])).unwrap_err();
    match &err.violations[..] {
        [Violation::Exceeds { operation, .. }] => assert_eq!(
            *operation,
            OperationKind::Handler {
                effect: NET_EFFECT,
                handler: TCP_HANDLER
            }
        ),
        v => panic!("{v:?}"),
    }
}

#[test]
fn unknown_and_late_resolution_fail_closed() {
    let mut h = host();
    let err = run(
        &mut h,
        call_claimed("@util/misc::late", LATE),
        &policies(vec![exact(net())]),
    )
    .unwrap_err();
    assert!(matches!(
        &err.violations[..],
        [Violation::Graph {
            fault: GraphFault::Unresolved { at: LATE, .. },
            ..
        }]
    ));
    let mut h = host();
    let err = run(
        &mut h,
        call_claimed("@util/misc::ghost", GHOST),
        &policies(vec![exact(net())]),
    )
    .unwrap_err();
    assert!(matches!(
        &err.violations[..],
        [Violation::Graph {
            fault: GraphFault::MissingNode { def: GHOST, .. },
            ..
        }]
    ));
}

#[test]
fn undeclared_policies_fail_closed() {
    let mut h = host();
    let p = PolicySet::new().ceiling(ceiling(APP, vec![exact(net())]));
    let err = run(&mut h, Quotation::new().text("hash_into(1)"), &p).unwrap_err();
    assert!(matches!(
        &err.violations[..],
        [Violation::MissingMacroPolicy { .. }]
    ));
    let mut h = host();
    let p = PolicySet::new().macro_policy(emits(DERIVE_MACRO, vec![]));
    let err = run(&mut h, Quotation::new().text("hash_into(1)"), &p).unwrap_err();
    assert_eq!(
        err.violations,
        vec![Violation::MissingCeiling { package: APP }]
    );
}

#[test]
fn spliced_caller_syntax_keeps_caller_origin() {
    // The caller's own `connect()` passes through the macro unchanged; it is
    // the caller's authority, not generated code.
    let mut h = host();
    let toks = expand_in_app(&mut h, "", &["connect()"], |x| {
        Quotation::new().text("let t = 1;").splice(x.splices[0])
    })
    .unwrap();
    let f = h.resolve(toks).unwrap();
    let ok = check_authority(h.session(), f, &graph(), &policies(vec![])).unwrap();
    assert!(ok.audit.operations.is_empty());
    assert_eq!(
        ok.audit.source_operations,
        vec![OperationKind::Use { def: CONNECT }]
    );
}

#[test]
fn nested_expansions_are_bounded_by_every_enclosing_policy() {
    let build = |outer_macro: DefId, inner_macro: DefId| {
        let mut h = host();
        let outer = h
            .begin_expansion(outer_macro, InvocationSite::Source(APP), vec![])
            .unwrap();
        h.finish_quotation(outer.id, &Quotation::new().text("1"))
            .unwrap();
        let inner = h
            .begin_expansion(inner_macro, InvocationSite::Expansion(outer.id), vec![])
            .unwrap();
        let toks = h
            .finish_quotation(inner.id, &call_claimed("@std/net::connect", CONNECT))
            .unwrap();
        let f = h.resolve(toks).unwrap();
        check_authority(
            h.session(),
            f,
            &graph(),
            &PolicySet::new()
                .macro_policy(emits(DERIVE_MACRO, vec![]))
                .macro_policy(emits(NET_MACRO, vec![exact(net())]))
                .ceiling(ceiling(APP, vec![exact(net())])),
        )
    };
    // Net-emitting macro inside a pure macro's output: the outer policy binds.
    let err = build(DERIVE_MACRO, NET_MACRO).unwrap_err();
    assert_eq!(
        exceeded(&err),
        vec![(
            net(),
            vec![Denial::MacroEmits {
                macro_def: DERIVE_MACRO,
                version: 3
            }]
        )]
    );
    // Net-emitting macro inside another Net-emitting macro: accepted.
    let ok = build(NET_MACRO, NET_MACRO).unwrap();
    assert_eq!(ok.audit.policies.len(), 2);
}

#[test]
fn audit_and_rejections_are_deterministic() {
    let accept = || {
        let mut h = host();
        run(
            &mut h,
            Quotation::new()
                .text("let a = hash_into(1);")
                .token(claimed("@util/misc::sneaky", SNEAKY))
                .text("(a)"),
            &policies(vec![exact(net())]),
        )
        .unwrap()
    };
    let a = accept().audit.to_json();
    assert_eq!(a, accept().audit.to_json());
    let back = AuthorityAudit::from_json(&a).unwrap();
    assert_eq!(back.to_json(), a);

    let mut v: serde_json::Value = serde_json::from_str(&a).unwrap();
    v["schema_version"] = serde_json::json!(AUTHORITY_SCHEMA_VERSION + 1);
    assert!(matches!(
        AuthorityAudit::from_json(&v.to_string()),
        Err(ForgeError::Schema { .. })
    ));
    v["schema_version"] = serde_json::json!(AUTHORITY_SCHEMA_VERSION);
    v["exempt"] = serde_json::json!(true);
    assert!(matches!(
        AuthorityAudit::from_json(&v.to_string()),
        Err(ForgeError::Decode(_))
    ));

    let reject = || {
        let mut h = host();
        run(
            &mut h,
            Quotation::new()
                .token(claimed("@util/misc::sneaky", SNEAKY))
                .text("(1)"),
            &policies(vec![]),
        )
        .unwrap_err()
        .to_string()
    };
    assert_eq!(reject(), reject());
    assert_eq!(
        reject(),
        "FORGE-A001 token 0: use of pkg5#0 from expansion x0 requires pkg3::Net \
         (via pkg5#0 -> pkg5#1 -> pkg5#2 -> pkg3#0), denied by emits of macro pkg2#0 v3;"
    );
}

#[test]
fn graph_and_policy_schemas_are_checked() {
    let g = graph();
    let json = serde_json::to_string(&g).unwrap();
    let back: AuthorityGraph = serde_json::from_str(&json).unwrap();
    assert_eq!(back, g);
    assert_eq!(serde_json::to_string(&back).unwrap(), json);
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v["schema_version"] = serde_json::json!(0);
    assert!(serde_json::from_value::<AuthorityGraph>(v).is_err());

    // Duplicate nodes are rejected rather than merged.
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let first = v["nodes"][0].clone();
    v["nodes"].as_array_mut().unwrap().push(first);
    assert!(serde_json::from_value::<AuthorityGraph>(v).is_err());

    let p = emits(DERIVE_MACRO, vec![exact(net())]);
    let pj = serde_json::to_value(&p).unwrap();
    assert_eq!(pj["schema_version"], AUTHORITY_SCHEMA_VERSION);
    assert_eq!(
        serde_json::from_value::<MacroPolicy>(pj.clone()).unwrap(),
        p
    );
    let mut bad = pj;
    bad["schema_version"] = serde_json::json!(2);
    assert!(serde_json::from_value::<MacroPolicy>(bad).is_err());

    let c = ceiling(APP, vec![]);
    let mut cj = serde_json::to_value(&c).unwrap();
    assert_eq!(
        serde_json::from_value::<CallerCeiling>(cj.clone()).unwrap(),
        c
    );
    cj["schema_version"] = serde_json::json!(2);
    assert!(serde_json::from_value::<CallerCeiling>(cj).is_err());
}

#[test]
fn handler_construction_without_rule_fails_closed() {
    let mut g = AuthorityGraph::new("g");
    g.add_node(AuthorityNode::new(TCP_HANDLER)).unwrap();
    let r = g.requirements(&OperationKind::Handler {
        effect: NET_EFFECT,
        handler: TCP_HANDLER,
    });
    assert_eq!(
        r,
        Err(GraphFault::MissingHandlerRule {
            effect: NET_EFFECT,
            path: vec![]
        })
    );
}

#[test]
fn generated_call_of_spliced_caller_global_is_attributed_to_the_macro() {
    let gate = |build: fn(&Expansion) -> Quotation| {
        let mut h = host();
        let toks = expand_in_app(&mut h, "", &["connect"], build).unwrap();
        let f = h.resolve(toks).unwrap();
        check_authority(h.session(), f, &graph(), &policies(vec![]))
    };
    // `$f()`: caller supplies only the name, the macro adds the call.
    let err = gate(|x| Quotation::new().splice(x.splices[0]).text("()")).unwrap_err();
    assert_eq!(exceeded(&err)[0].0, net());
    // `let g = $f; g()`: the call through a local alias is attributed too.
    let err = gate(|x| {
        Quotation::new()
            .text("let g =")
            .splice(x.splices[0])
            .text("; g()")
    })
    .unwrap_err();
    assert_eq!(exceeded(&err)[0].0, net());
    // A generated call whose callee is not a static global fails closed.
    let err = gate(|_| Quotation::new().text("{ hash_into }(1)")).unwrap_err();
    assert!(matches!(
        &err.violations[..],
        [Violation::Graph {
            operation: OperationKind::IndirectCall,
            fault: GraphFault::UnknownCallee,
            ..
        }]
    ));
}
