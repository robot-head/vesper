mod common;

use common::*;
use vesper_forge::*;

fn local_binder_token(f: &ResolvedFragment, at: usize) -> usize {
    match f.resolution_at(at) {
        Some(Resolution::Local(b)) => f.binder(b).unwrap().token,
        other => panic!("token {at} resolved to {other:?}"),
    }
}

#[test]
fn introduced_binder_cannot_capture_caller_identifier() {
    let mut h = host();
    // let x = 1;  MACRO!(x)  ==>  let x = 2; x(caller)
    let toks = expand_in_app(&mut h, "let x = 1;", &["x"], |x| {
        Quotation::new().text("let x = 2;").splice(x.splices[0])
    })
    .unwrap();
    assert_eq!(toks.len(), 11);
    let f = h.resolve(toks).unwrap();
    assert_eq!(
        local_binder_token(&f, 10),
        1,
        "caller x must bind to caller let"
    );
    // The macro binder exists but captured nothing.
    assert_eq!(f.binders.len(), 2);
    assert!(matches!(f.binders[1].origin, Origin::Expansion(_)));
}

#[test]
fn macro_reference_binds_its_own_binder_not_callers() {
    let mut h = host();
    // let tmp = 1; MACRO!() ==> let tmp = 2; hash_into(tmp)
    let toks = expand_in_app(&mut h, "let tmp = 1;", &[], |_| {
        Quotation::new().text("let tmp = 2; hash_into(tmp)")
    })
    .unwrap();
    let f = h.resolve(toks).unwrap();
    // tokens: 0..4 caller, 5..9 macro let, 10 hash_into, 11 (, 12 tmp, 13 )
    assert_eq!(local_binder_token(&f, 12), 6);
    assert_eq!(f.resolution_at(10), Some(Resolution::Global(HASH_INTO)));
}

#[test]
fn caller_locals_cannot_hijack_quoted_globals() {
    let mut h = host();
    // Caller shadows `hash_into` and `phone_home` locally; the macro's
    // unqualified references still resolve in the macro's definition site.
    let toks = expand_in_app(
        &mut h,
        "let hash_into = 5; let phone_home = 6;",
        &[],
        |_| Quotation::new().text("hash_into(phone_home)"),
    )
    .unwrap();
    let f = h.resolve(toks).unwrap();
    assert_eq!(f.resolution_at(10), Some(Resolution::Global(HASH_INTO)));
    assert_eq!(f.resolution_at(12), Some(Resolution::Global(PHONE_HOME)));

    // Pre-resolved quoted globals ignore lexical bindings entirely.
    let mut h = host();
    let toks = expand_in_app(&mut h, "let hash_into = 5;", &[], |_| {
        Quotation::new()
            .token(claimed("@vesper-wire/hash::hash_into", HASH_INTO))
            .text("(1)")
    })
    .unwrap();
    let f = h.resolve(toks).unwrap();
    assert_eq!(f.resolution_at(5), Some(Resolution::Global(HASH_INTO)));
    assert!(f.references.iter().any(|r| r.token == 5 && r.pre_resolved));
}

#[test]
fn macro_text_cannot_reach_caller_only_globals() {
    // `print` is only in the caller's environment.
    let mut h = host();
    let toks = expand_in_app(&mut h, "", &[], |_| Quotation::new().text("print(1)")).unwrap();
    assert_eq!(
        h.resolve(toks).unwrap_err(),
        ForgeError::Unbound {
            token: 0,
            name: "print".into()
        }
    );
    // The caller itself can.
    let mut h = host();
    let toks = h.lex_source(APP, "print(1)").unwrap();
    let f = h.resolve(toks).unwrap();
    assert_eq!(f.resolution_at(0), Some(Resolution::Global(APP_PRINT)));
}

#[test]
fn caller_shadowing_resolves_innermost() {
    let mut h = host();
    let toks = h.lex_source(APP, "let x = 1; let x = x; x").unwrap();
    let f = h.resolve(toks).unwrap();
    // The initializer `x` (token 8) sees the outer binder only.
    assert_eq!(local_binder_token(&f, 8), 1);
    assert_eq!(local_binder_token(&f, 10), 6);
}

#[test]
fn nested_expansions_resolve_consistently() {
    let mut h = host();
    let outer = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap();
    // Outer expansion: let y = 1; INNER!(y)
    let out1 = h
        .finish_quotation(outer.id, &Quotation::new().text("let y = 1; y"))
        .unwrap();
    let arg = vec![out1[5].clone()];
    let inner = h
        .begin_expansion(NET_MACRO, InvocationSite::Expansion(outer.id), vec![arg])
        .unwrap();
    // Inner expansion: let y = 2; hash_into($arg, y)
    let out2 = h
        .finish_quotation(
            inner.id,
            &Quotation::new()
                .text("let y = 2; hash_into(")
                .splice(inner.splices[0])
                .text(", y)"),
        )
        .unwrap();
    let mut program = out1[..5].to_vec();
    program.extend(out2);
    let f = h.resolve(program).unwrap();
    // 0..4 outer let, 5..9 inner let, 10 hash_into, 11 (, 12 spliced y, 13 ',', 14 y
    assert_eq!(
        local_binder_token(&f, 12),
        1,
        "outer y is not captured by inner let"
    );
    assert_eq!(local_binder_token(&f, 14), 6, "inner y binds inner let");
    let rec = &f.expansions[&inner.id];
    assert_eq!(rec.site, InvocationSite::Expansion(outer.id));
    assert_eq!(rec.root_caller, APP);
    assert_eq!(f.expansions.len(), 2);
}

#[test]
fn incomparable_bindings_are_ambiguous() {
    let mut h = host();
    let s1 = h.fresh_scope().unwrap();
    let s2 = h.fresh_scope().unwrap();
    let mut a = h.lex_source(APP, "let x = 1;").unwrap();
    let mut b = h.lex_source(APP, "let x = 2;").unwrap();
    let mut r = h.lex_source(APP, "x").unwrap();
    h.add_scope(&mut a, s1).unwrap();
    h.add_scope(&mut b, s2).unwrap();
    h.add_scope(&mut r, s1).unwrap();
    h.add_scope(&mut r, s2).unwrap();
    let mut program = a;
    program.extend(b);
    program.extend(r);
    match h.resolve(program).unwrap_err() {
        ForgeError::Ambiguous {
            token, candidates, ..
        } => {
            assert_eq!(token, 10);
            assert_eq!(candidates.len(), 2);
        }
        e => panic!("{e}"),
    }
}

#[test]
fn forged_stale_and_foreign_splice_handles_fail() {
    // Stale: a handle from a finished expansion used in a later one.
    let mut h = host();
    let arg = h.lex_source(APP, "x").unwrap();
    let first = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![arg.clone()])
        .unwrap();
    h.finish_quotation(first.id, &Quotation::new().text("1"))
        .unwrap();
    let second = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![arg.clone()])
        .unwrap();
    let err = h
        .finish_quotation(second.id, &Quotation::new().splice(first.splices[0]))
        .unwrap_err();
    assert!(matches!(
        err,
        ForgeError::Splice {
            fault: SpliceFault::Stale,
            ..
        }
    ));
    // The failed expansion is closed: it cannot be retried with a good handle.
    assert_eq!(
        h.finish_quotation(second.id, &Quotation::new().splice(second.splices[0]))
            .unwrap_err(),
        ForgeError::ExpansionClosed(second.id)
    );

    // Forged: a nonce never minted for this expansion.
    let third = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![arg.clone()])
        .unwrap();
    let mut json = serde_json::to_value(third.splices[0]).unwrap();
    json["nonce"] = serde_json::json!(4242);
    let forged: SpliceHandle = serde_json::from_value(json).unwrap();
    assert!(matches!(
        h.finish_quotation(third.id, &Quotation::new().splice(forged))
            .unwrap_err(),
        ForgeError::Splice {
            fault: SpliceFault::Forged,
            ..
        }
    ));

    // Foreign: a handle minted by another session.
    let mut other = host_session(SessionId(8));
    let oarg = other.lex_source(APP, "x").unwrap();
    let ox = other
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![oarg])
        .unwrap();
    let fourth = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![arg])
        .unwrap();
    assert!(matches!(
        h.finish_quotation(fourth.id, &Quotation::new().splice(ox.splices[0]))
            .unwrap_err(),
        ForgeError::Splice {
            fault: SpliceFault::Foreign,
            ..
        }
    ));
}

#[test]
fn forged_and_foreign_scope_marks_fail() {
    let mut h = host();
    let toks = h.lex_source(APP, "print(1)").unwrap();
    let mut json = serde_json::to_value(&toks[0]).unwrap();
    json["scopes"] = serde_json::json!([{ "session": 7, "index": 9999 }]);
    let forged: HostToken = serde_json::from_value(json).unwrap();
    let mut program = vec![forged];
    program.extend(toks[1..].iter().cloned());
    assert!(matches!(
        h.resolve(program).unwrap_err(),
        ForgeError::ForgedScope(_)
    ));

    let mut other = host_session(SessionId(8));
    let foreign = other.lex_source(APP, "print(1)").unwrap();
    assert!(matches!(
        h.resolve(foreign).unwrap_err(),
        ForgeError::ForeignScope(_)
    ));

    // Tokens of an unfinished expansion cannot be resolved or spliced.
    let mut json = serde_json::to_value(&toks[0]).unwrap();
    let x = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap();
    json["origin"] = serde_json::to_value(Origin::Expansion(x.id)).unwrap();
    let pending: HostToken = serde_json::from_value(json).unwrap();
    assert_eq!(
        h.resolve(vec![pending]).unwrap_err(),
        ForgeError::ExpansionNotFinished(x.id)
    );
}

#[test]
fn identity_exhaustion_is_an_error_not_reuse() {
    let limits = IdentityLimits {
        max_scopes: 2,
        ..IdentityLimits::default()
    };
    let mut h = HygieneHost::with_limits(SessionId(1), registry(), limits, QuoteLimits::default());
    let a = h.module_scope(APP).unwrap();
    let b = h.module_scope(DERIVE).unwrap();
    assert_ne!(a, b);
    let err = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap_err();
    let expected = ForgeError::IdentityExhausted {
        kind: IdentityKind::Scope,
        limit: 2,
    };
    assert_eq!(err, expected);
    assert_eq!(h.fresh_scope().unwrap_err(), expected);
    assert_eq!(h.fresh_scope().unwrap_err(), expected);

    let limits = IdentityLimits {
        max_expansions: 1,
        max_binders: 1,
        ..IdentityLimits::default()
    };
    let mut h = HygieneHost::with_limits(SessionId(1), registry(), limits, QuoteLimits::default());
    let x = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap();
    assert_eq!(
        h.begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
            .unwrap_err(),
        ForgeError::IdentityExhausted {
            kind: IdentityKind::Expansion,
            limit: 1
        }
    );
    let toks = h
        .finish_quotation(x.id, &Quotation::new().text("let a = 1; let b = 2; a"))
        .unwrap();
    assert_eq!(
        h.resolve(toks).unwrap_err(),
        ForgeError::IdentityExhausted {
            kind: IdentityKind::Binder,
            limit: 1
        }
    );
}

#[test]
fn forged_def_id_claims_are_rejected() {
    let cases: Vec<(GeneratedToken, ForgeError)> = vec![
        (
            claimed("@nowhere::f", d(PackageId(9), 9)),
            ForgeError::UnknownDefinition(d(PackageId(9), 9)),
        ),
        // A macro cannot claim a new definition in its own package.
        (
            claimed("@vesper-wire/derive::backdoor", d(DERIVE, 77)),
            ForgeError::UnknownDefinition(d(DERIVE, 77)),
        ),
        (
            claimed("other::f", OTHER_FN),
            ForgeError::NotADependency {
                from: DERIVE,
                def: OTHER_FN,
            },
        ),
        (
            claimed("@util/misc::private", UTIL_PRIVATE),
            ForgeError::PrivateDefinition {
                from: DERIVE,
                def: UTIL_PRIVATE,
            },
        ),
        (
            claimed("innocent_hash", CONNECT),
            ForgeError::PrintedNameMismatch {
                def: CONNECT,
                printed: "innocent_hash".into(),
                registered: "@std/net::connect".into(),
            },
        ),
    ];
    for (tok, expected) in cases {
        let mut h = host();
        let toks = expand_in_app(&mut h, "", &[], |_| Quotation::new().token(tok)).unwrap();
        assert_eq!(h.resolve(toks).unwrap_err(), expected);
    }

    let mut h = host();
    let toks = expand_in_app(&mut h, "", &[], |_| {
        Quotation::new()
            .text("let")
            .token(claimed("@vesper-wire/hash::hash_into", HASH_INTO))
            .text("= 1; 2")
    })
    .unwrap();
    assert_eq!(
        h.resolve(toks).unwrap_err(),
        ForgeError::PreResolvedBinder {
            token: 1,
            def: HASH_INTO
        }
    );
}

#[test]
fn macro_must_be_registered_and_visible() {
    let mut h = host();
    assert_eq!(
        h.begin_expansion(HASH_INTO, InvocationSite::Source(APP), vec![])
            .unwrap_err(),
        ForgeError::NotAMacro(HASH_INTO)
    );
    // `other` does not depend on the macro package.
    assert_eq!(
        h.begin_expansion(DERIVE_MACRO, InvocationSite::Source(OTHER), vec![])
            .unwrap_err(),
        ForgeError::NotADependency {
            from: OTHER,
            def: DERIVE_MACRO
        }
    );
}

#[test]
fn quotation_bounds_and_syntax_errors() {
    let limits = QuoteLimits {
        max_output_tokens: 3,
        ..QuoteLimits::default()
    };
    let mut h =
        HygieneHost::with_limits(SessionId(1), registry(), IdentityLimits::default(), limits);
    let x = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap();
    assert_eq!(
        h.finish_quotation(x.id, &Quotation::new().text("a b c d"))
            .unwrap_err(),
        ForgeError::QuoteLimit {
            what: "output tokens",
            limit: 3
        }
    );

    let limits = QuoteLimits {
        max_depth: 4,
        ..QuoteLimits::default()
    };
    let mut h =
        HygieneHost::with_limits(SessionId(1), registry(), IdentityLimits::default(), limits);
    let deep = format!("{}1{}", "(".repeat(5), ")".repeat(5));
    let toks = h.lex_source(APP, &deep).unwrap();
    assert!(matches!(
        h.resolve(toks).unwrap_err(),
        ForgeError::QuoteLimit {
            what: "nesting depth",
            ..
        }
    ));

    let mut h = host();
    let x = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap();
    assert_eq!(
        h.finish_quotation(x.id, &Quotation::new().text("f(1"))
            .unwrap_err(),
        ForgeError::UnbalancedDelimiters { token: 3 }
    );
    let toks = h.lex_source(APP, "let = 1; 2").unwrap();
    assert_eq!(
        h.resolve(toks).unwrap_err(),
        ForgeError::Parse {
            token: 1,
            expected: "identifier"
        }
    );
}

#[test]
fn diagnostics_are_deterministic() {
    let e = ForgeError::Splice {
        fault: SpliceFault::Stale,
        handle: {
            let mut h = host();
            let a = h.lex_source(APP, "x").unwrap();
            h.begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![a])
                .unwrap()
                .splices[0]
        },
    };
    assert_eq!(
        e.to_string(),
        "FORGE-H009 Stale splice handle for expansion x0"
    );
    assert_eq!(
        ForgeError::NotADependency {
            from: DERIVE,
            def: OTHER_FN
        }
        .to_string(),
        "FORGE-R004 pkg2 cannot name pkg6#0: its package is not a dependency"
    );
}

#[test]
fn registry_schema_is_checked() {
    let r = registry();
    let json = serde_json::to_string(&r).unwrap();
    let back: DefinitionRegistry = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_string(&back).unwrap(), json);

    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v["schema_version"] = serde_json::json!(REGISTRY_SCHEMA_VERSION + 1);
    assert!(serde_json::from_value::<DefinitionRegistry>(v.clone()).is_err());
    v["schema_version"] = serde_json::json!(REGISTRY_SCHEMA_VERSION);
    v["trusted_by_guest"] = serde_json::json!(true);
    assert!(serde_json::from_value::<DefinitionRegistry>(v).is_err());
}
