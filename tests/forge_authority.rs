use vesper::{
    forge_frontend::authority::*,
    tooling::frontend::{AnalysisLimits, check_source},
};
use vesper_forge::*;
use vesper_syntax::macro_source::{self, MacroSource};
fn source(text: &str) -> MacroSource {
    macro_source::parse(text, macro_source::Limits::default()).unwrap()
}
fn pair(support: &str, body: &str, result: &str, effects: &str) -> (MacroSource, String) {
    (
        source(&format!(
            "{support} macro make()->expr{{quote{{{body}}}}} fn main()->{result} {effects}{{make!()}}"
        )),
        format!("{support} fn main()->{result} {effects}{{{body}}}"),
    )
}
fn make_bundle(
    support: &str,
    body: &str,
    result: &str,
    effects: &str,
) -> (SourceBindings, CheckedAuthorityBundle) {
    let (original, candidate) = pair(support, body, result, effects);
    let bindings = bind_source(&original, "@app/test", AuthorityLimits::default()).unwrap();
    let checked = check_source(&candidate, AnalysisLimits::default()).unwrap();
    let graph = derive(&checked, &bindings, AuthorityLimits::default()).unwrap();
    (bindings, graph)
}
fn requirements(
    bindings: &SourceBindings,
    bundle: &CheckedAuthorityBundle,
    name: &str,
) -> Requirements {
    bundle
        .graph()
        .requirements(&OperationKind::Use {
            def: bindings.def_id(name).unwrap(),
        })
        .unwrap()
}
#[test]
fn syntax_binding_is_identity_only_and_candidate_mapping_must_match_actual_world() {
    let (original, candidate) = pair("fn helper()->u64{42}", "helper()", "u64", "");
    let bindings = bind_source(&original, "@app/test", AuthorityLimits::default()).unwrap();
    assert_eq!(bindings.source_hash(), original.source_hash());
    assert_eq!(
        bindings
            .kind(bindings.macro_def(original.definitions()[0].id).unwrap())
            .unwrap(),
        DefKind::Macro
    );
    assert_eq!(
        bindings
            .core_name(bindings.def_id("helper").unwrap())
            .unwrap(),
        "helper"
    );
    assert!(bindings.global_names().any(|n| n == "helper"));
    assert!(bindings.global_names().any(|n| n == "wrapping_add"));
    let checked = check_source(&candidate, AnalysisLimits::default()).unwrap();
    let graph = derive(&checked, &bindings, AuthorityLimits::default()).unwrap();
    assert_ne!(bindings.source_hash(), graph.checked_source_hash());
    assert_eq!(graph.checked_source_hash(), checked.source_hash());
    let changed = check_source(
        "fn helper()->u64{42} fn main()->u64{helper()} fn extra(){}",
        AnalysisLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        derive(&changed, &bindings, AuthorityLimits::default()),
        Err(AuthorityFailure::Mapping(_))
    ));
    let changed = check_source(
        "fn helper()->bool{true} fn main()->bool{helper()}",
        AnalysisLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        derive(&changed, &bindings, AuthorityLimits::default()),
        Err(AuthorityFailure::Mapping(_))
    ));
    assert!(
        bindings
            .core_name(bindings.def_id("make").unwrap())
            .is_err()
    );
}
#[test]
fn compiler_derived_transitive_smuggling_fails_empty_emits_even_in_privileged_caller() {
    let support = "effect Log{fn event();} fn hidden()->u64 eff[Log]{Log::event();42} fn middle()->u64 eff[Log]{hidden()}";
    let (bindings, bundle) = make_bundle(support, "middle()", "u64", "eff[Log]");
    let req = requirements(&bindings, &bundle, "main");
    assert_eq!(req.len(), 1);
    let log = Authority::new(bindings.effect_id("Log").unwrap());
    assert!(req.contains_key(&log));
    let mut host = HygieneHost::new(SessionId(105), bindings.registry().clone());
    host.declare_global(SOURCE_PACKAGE, "middle", bindings.def_id("middle").unwrap())
        .unwrap();
    let expansion = host
        .begin_expansion(
            bindings.def_id("make").unwrap(),
            InvocationSite::Source(SOURCE_PACKAGE),
            vec![],
        )
        .unwrap();
    let tokens = host
        .finish_quotation(expansion.id, &Quotation::new().text("middle()"))
        .unwrap();
    let fragment = host.resolve(tokens).unwrap();
    let policy = PolicySet::new()
        .macro_policy(MacroPolicy {
            macro_def: bindings.def_id("make").unwrap(),
            version: 1,
            emits: vec![],
        })
        .ceiling(CallerCeiling {
            package: SOURCE_PACKAGE,
            version: 1,
            grants: vec![AuthorityGrant::Exact(log)],
        });
    assert!(check_authority(host.session(), fragment, bundle.graph(), &policy).is_err());
}
#[test]
fn both_implicit_failure_sets_are_independent_and_total_wrapping_helpers_are_pure() {
    let support =
        "fn both(in x:i64,in y:i64)->i64 eff[Fail[RuntimeError],Fail[ArithOverflow]]{x/y+1}";
    let (bindings, bundle) = make_bundle(
        support,
        "both(41,1)",
        "i64",
        "eff[Fail[RuntimeError],Fail[ArithOverflow]]",
    );
    let req = requirements(&bindings, &bundle, "both");
    assert_eq!(req.len(), 2);
    let args = req
        .keys()
        .flat_map(|a| &a.args)
        .map(|a| match a {
            AuthorityArg::Text(s) => s.as_str(),
            _ => panic!(),
        })
        .collect::<Vec<_>>();
    assert!(args.iter().any(|s| s.contains("RuntimeError")));
    assert!(args.iter().any(|s| s.contains("ArithOverflow")));
    for support in ["fn safe()->u64{1+1}", "fn safe()->u64{wrapping_add(1,1)}"] {
        let (bindings, bundle) = make_bundle(support, "safe()", "u64", "");
        assert!(requirements(&bindings, &bundle, "main").is_empty());
        for name in ["wrapping_add", "wrapping_sub", "wrapping_mul"] {
            assert!(requirements(&bindings, &bundle, name).is_empty());
        }
        assert!(bindings.def_id("wrapping_div").is_none());
    }
}
#[test]
fn generic_effect_instances_in_helpers_are_concrete_distinct_and_never_global_union_nodes() {
    let support = "copy struct A{} copy struct B{} effect Use[T:mod copy]{fn touch(in x:T);} fn a(in x:A) eff[Use[A]]{Use::touch(in x);} fn b(in x:B) eff[Use[B]]{Use::touch(in x);}";
    let (bindings, bundle) = make_bundle(support, "0", "u64", "");
    let a = requirements(&bindings, &bundle, "a");
    let b = requirements(&bindings, &bundle, "b");
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert_ne!(a.keys().next(), b.keys().next());
    let def = bindings.def_id("Use::touch").unwrap();
    assert!(bundle.unsupported_emitted_def(def));
    assert!(
        bundle
            .graph()
            .requirements(&OperationKind::Use { def })
            .is_err()
    );
    let arg = a.keys().next().unwrap().args.first().unwrap();
    assert!(
        matches!(arg,AuthorityArg::Text(s) if s.contains("vesper.forge.authority-type/1") && s.contains("\"package\":1") && s.contains("\"A\""))
    );
}
#[test]
fn body_changes_with_equal_headers_never_reuse_old_graph_or_source_identity() {
    let common = "effect Log{fn event();}";
    let (a, ga) = make_bundle(
        &format!("{common} fn helper()->u64 eff[Log]{{42}}"),
        "helper()",
        "u64",
        "eff[Log]",
    );
    let (b, gb) = make_bundle(
        &format!("{common} fn helper()->u64 eff[Log]{{Log::event();42}}"),
        "helper()",
        "u64",
        "eff[Log]",
    );
    assert_eq!(a.def_id("helper"), b.def_id("helper"));
    assert_ne!(a.identity(), b.identity());
    assert_ne!(ga.identity(), gb.identity());
    assert_ne!(ga.graph().graph_version(), gb.graph().graph_version());
    assert!(requirements(&a, &ga, "helper").is_empty());
    assert_eq!(requirements(&b, &gb, "helper").len(), 1);
}
#[test]
fn recursion_static_ir_edges_are_bounded_and_not_inferred_from_world_calls_values() {
    let support = "effect Log{fn event();} fn looped(in n:u64)->u64 eff[Log]{if n==0{Log::event();0}else{looped(wrapping_sub(n,1))}}";
    let (bindings, bundle) = make_bundle(support, "looped(1)", "u64", "eff[Log]");
    assert_eq!(requirements(&bindings, &bundle, "looped").len(), 1);
    assert_eq!(requirements(&bindings, &bundle, "main").len(), 1);
}
#[test]
fn source_package_foreign_ids_and_builtins_cannot_be_forged() {
    let (a, _) = pair("", "1", "u64", "");
    let (b, _) = pair("", "2", "u64", "");
    for package in ["@vesper/builtin", "not a package", "@app/../test"] {
        assert!(matches!(
            bind_source(&a, package, AuthorityLimits::default()),
            Err(AuthorityFailure::InvalidPackage)
        ));
    }
    let bindings = bind_source(&a, "@app/test", AuthorityLimits::default()).unwrap();
    assert!(matches!(
        bindings.macro_def(b.definitions()[0].id),
        Err(AuthorityFailure::ForeignSource)
    ));
    assert!(
        bindings
            .core_name(DefId {
                package: PackageId(77),
                item: 0
            })
            .is_err()
    );
    assert_eq!(bindings.effect_id("Fail").unwrap().package, BUILTIN_PACKAGE);
    assert_eq!(
        bindings.effect_id("builtin::Fail").unwrap().package,
        BUILTIN_PACKAGE
    );
    assert!(bindings.effect_id("@other/pkg::Fail").is_err());
}
#[test]
fn unsupported_higher_order_generics_and_function_aliases_refuse_before_graph_publication() {
    let cases = [
        ("fn helper[T:mod copy](in x:T)->T{x}", "0", "u64", ""),
        ("fn helper(sink f:fn()->u64)->u64{f()}", "0", "u64", ""),
        ("fn helper()->fn()->u64{||42}", "0", "u64", ""),
        (
            "fn helper()->u64{let alias=other;alias()} fn other()->u64{42}",
            "0",
            "u64",
            "",
        ),
    ];
    for (support, body, result, effects) in cases {
        let (original, candidate) = pair(support, body, result, effects);
        let bindings = bind_source(&original, "@app/test", AuthorityLimits::default()).unwrap();
        let checked = check_source(&candidate, AnalysisLimits::default()).unwrap();
        assert!(
            matches!(
                derive(&checked, &bindings, AuthorityLimits::default()),
                Err(AuthorityFailure::Unsupported(_))
            ),
            "{candidate}"
        );
    }
}
#[test]
fn source_and_postcheck_limits_fail_without_partial_bundle_and_stage_budget_is_shared() {
    let (original, candidate) = pair("fn helper()->u64{42}", "helper()", "u64", "");
    for limits in [
        AuthorityLimits {
            max_source_bytes: 1,
            ..AuthorityLimits::default()
        },
        AuthorityLimits {
            max_definitions: 1,
            ..AuthorityLimits::default()
        },
        AuthorityLimits {
            max_metadata_bytes: 1,
            ..AuthorityLimits::default()
        },
        AuthorityLimits {
            max_work: 1,
            ..AuthorityLimits::default()
        },
        AuthorityLimits {
            max_work: usize::MAX,
            ..AuthorityLimits::default()
        },
    ] {
        assert!(bind_source(&original, "@app/test", limits).is_err());
    }
    let bindings = bind_source(&original, "@app/test", AuthorityLimits::default()).unwrap();
    let checked = check_source(&candidate, AnalysisLimits::default()).unwrap();
    for limits in [
        AuthorityLimits {
            max_source_bytes: 1,
            ..AuthorityLimits::default()
        },
        AuthorityLimits {
            max_ir_nodes: 1,
            ..AuthorityLimits::default()
        },
        AuthorityLimits {
            max_ast_nodes: 1,
            ..AuthorityLimits::default()
        },
        AuthorityLimits {
            max_type_nodes: 1,
            ..AuthorityLimits::default()
        },
        AuthorityLimits {
            max_depth: 1,
            ..AuthorityLimits::default()
        },
        AuthorityLimits {
            max_metadata_bytes: 1,
            ..AuthorityLimits::default()
        },
        AuthorityLimits {
            max_work: 1,
            ..AuthorityLimits::default()
        },
    ] {
        assert!(
            matches!(
                derive(&checked, &bindings, limits),
                Err(AuthorityFailure::Resource(_))
            ),
            "{limits:?}"
        );
    }
}
