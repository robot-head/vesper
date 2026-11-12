mod common;

use common::*;
use vesper::ast::Mode;
use vesper_pact::{OverflowKind, Solver, SolverConfig, canonicalize_query};
use vesper_pact_bridge::*;

#[test]
fn proves_and_consumes_source_type_goal() {
    let s = std_session();
    let q = query(
        std_pkg(),
        implements(ty("Vec", vec![ty("List", vec![ty("Uuid", vec![])])]), "Eq"),
    );
    let o = s.solve(&q).unwrap();
    let e = proven(&o);
    let c = s.consume(e).unwrap();
    assert_eq!(
        c.selection,
        Selection::Impl {
            path: DefPath::new(std_pkg(), DefKind::Impl, "eq.vec"),
            args: vec!["@std/core@1.0.0::List[@std/core@1.0.0::Uuid]".into()],
        }
    );
    assert!(
        c.explanation.contains("impl @std/core@1.0.0::eq.list"),
        "{}",
        c.explanation
    );
}

#[test]
fn source_diagnostics_are_rejected() {
    let e = CheckedPackage::check(std_pkg(), "struct Broken { x: Missing }").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Source);
    let e = CheckedPackage::check(std_pkg(), "struct { ").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Source);
    // A field may not relax a linear component to affine (E0105).
    let e = CheckedPackage::check(
        std_pkg(),
        "linear struct T { x: u64 } struct S { t: T @@ affine }",
    )
    .unwrap_err();
    assert!(e.diagnostics.iter().any(|d| d.code == "E0105"), "{e}");
    assert!(e.diagnostics.iter().all(|d| d.span.is_some()));
    let e = CheckedPackage::check(PackageIdentity::new("Bad Name", "1"), "").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Invalid);
    let e = CheckedPackage::check(PackageIdentity::builtin(), "").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Invalid);
}

fn app_source() -> &'static str {
    "copy struct Point { x: i64, y: i64 }"
}

#[test]
fn identity_is_stable_under_reordering_and_whitespace() {
    let reordered = "
        effect Region { fn touch() -> Unit; }

        linear struct WriteTxn {   id: u64 }
        struct CacheBTree[K, V] { key: Option[K],
                                  value: Option[V] }
        struct Vec[T] { head: Option[T] }
        enum List[T] { Nil, Cons(T, List[T]) }
        copy struct Uuid { hi: u64, lo: u64 }
    ";
    let mut b = std_bindings();
    b.impls.reverse();
    b.models.reverse();
    b.traits.reverse();
    let a = std_session();
    let r = SessionBuilder::new()
        .package(check(std_pkg(), reordered), b)
        .build()
        .unwrap();
    assert_eq!(a.revision(), r.revision());
    assert_eq!(a.program_dto(), r.program_dto());
    assert_eq!(a.tables(), r.tables());
    // Evidence from one is accepted by the other: same declarations.
    let q = query(std_pkg(), implements(ty("Uuid", vec![]), "Eq"));
    let e = s_proven(&a, &q);
    r.consume(&e).unwrap();
}

fn s_proven(s: &CoherentSession, q: &QueryBindingDto) -> Evidence {
    proven(&s.solve(q).unwrap()).clone()
}

#[test]
fn same_named_definitions_in_two_packages_never_alias() {
    let a = PackageIdentity::new("@geo/a", "1.0.0");
    let b = PackageIdentity::new("@geo/b", "1.0.0");
    let mut ab = BindingsDto::empty(a.clone());
    ab.traits = vec![simple_trait("Eq")];
    ab.impls = vec![imp(
        "eq.point",
        conf(vec![], "Eq", ty("Point", vec![]), vec![]),
    )];
    let s = SessionBuilder::new()
        .package(check(a.clone(), app_source()), ab)
        .package(
            check(b.clone(), app_source()),
            BindingsDto::empty(b.clone()),
        )
        .build()
        .unwrap();
    let ida = s
        .tables()
        .type_id(&DefPath::new(a.clone(), DefKind::Type, "Point"))
        .unwrap();
    let idb = s
        .tables()
        .type_id(&DefPath::new(b.clone(), DefKind::Type, "Point"))
        .unwrap();
    assert_ne!(ida, idb);
    let eq = DefRefDto::qualified(a.clone(), "Eq");
    let goal = |self_ty| GoalDto::Implements {
        self_ty,
        trait_ref: eq.clone(),
        args: vec![],
        brand: None,
    };
    // `Point` at site b is b's Point.
    let o = s
        .solve(&query(b.clone(), goal(ty("Point", vec![]))))
        .unwrap();
    assert_eq!(o.verdict(), "no-solution");
    if let BridgeOutcome::NoSolution { explanation, .. } = &o {
        assert!(explanation.contains("@geo/b@1.0.0::Point"), "{explanation}");
        assert!(!explanation.contains("type#"), "{explanation}");
    }
    let o = s
        .solve(&query(b.clone(), goal(qty(a.clone(), "Point", vec![]))))
        .unwrap();
    proven(&o);
    let o = s
        .solve(&query(a.clone(), goal(qty(b, "Point", vec![]))))
        .unwrap();
    assert_eq!(o.verdict(), "no-solution");
}

fn expect_err(b: SessionBuilder, kind: ErrorKind, needle: &str) {
    let e = b.build().unwrap_err();
    assert_eq!(e.kind, kind, "{e}");
    assert!(e.mentions(needle), "expected {needle:?} in {e}");
}

fn std_with(f: impl FnOnce(&mut BindingsDto)) -> SessionBuilder {
    let mut b = std_bindings();
    f(&mut b);
    SessionBuilder::new().package(check(std_pkg(), STD_SOURCE), b)
}

#[test]
fn invalid_bindings_are_rejected() {
    expect_err(
        std_with(|b| {
            b.impls
                .push(imp("x", conf(vec![], "Eq", ty("Missing", vec![]), vec![])))
        }),
        ErrorKind::Invalid,
        "unknown type `Missing`",
    );
    expect_err(
        std_with(|b| {
            b.impls
                .push(imp("x", conf(vec![], "Eq", ty("Vec", vec![]), vec![])))
        }),
        ErrorKind::Invalid,
        "expects 1 argument",
    );
    expect_err(
        std_with(|b| {
            b.impls.push(imp(
                "x",
                conf(
                    vec![ParamDto::of("r", ParamKindDto::Row)],
                    "Eq",
                    TypeDto::Def {
                        def: tr("Vec"),
                        args: vec![ArgDto::Row(RowDto {
                            labels: vec![],
                            tail: Some("r".into()),
                        })],
                    },
                    vec![],
                ),
            ))
        }),
        ErrorKind::Invalid,
        "expected a Type argument",
    );
    expect_err(
        std_with(|b| {
            b.impls.push(imp(
                "x",
                conf(
                    vec![],
                    "Eq",
                    qty(PackageIdentity::new("@nope/x", "1.0.0"), "Uuid", vec![]),
                    vec![],
                ),
            ))
        }),
        ErrorKind::Invalid,
        "unknown package",
    );
    expect_err(
        std_with(|b| b.traits.push(simple_trait("Eq"))),
        ErrorKind::Invalid,
        "duplicate trait",
    );
    expect_err(
        std_with(|b| {
            b.impls.push(imp(
                "eq.u64",
                conf(vec![], "Show", ty("u64", vec![]), vec![]),
            ))
        }),
        ErrorKind::Invalid,
        "duplicate impl key",
    );
    expect_err(
        std_with(|b| b.package = app_pkg()),
        ErrorKind::Invalid,
        "bindings name package",
    );
    expect_err(
        std_with(|b| b.schema = BindingSchema { major: 0, minor: 9 }),
        ErrorKind::Invalid,
        "schema",
    );
    expect_err(
        std_with(|b| {
            b.impls
                .push(imp("x", conf(vec![], "Nope", ty("u64", vec![]), vec![])))
        }),
        ErrorKind::Invalid,
        "unknown trait `Nope`",
    );
    expect_err(
        std_with(|b| {
            b.traits.push(TraitDto {
                name: "Container".into(),
                params: vec![],
                assoc: vec![AssocDto {
                    name: "Elem".into(),
                    mode_bound: ModeDto::Const(Mode::Linear),
                    bounds: vec![],
                }],
                coinductive: false,
            });
            b.impls.push(imp(
                "c",
                conf(vec![], "Container", ty("u64", vec![]), vec![]),
            ));
        }),
        ErrorKind::Invalid,
        "missing associated type `Elem`",
    );
    expect_err(
        std_with(|b| {
            b.impls.push(imp(
                "x",
                conf(
                    vec![tparam("T"), tparam("T")],
                    "Eq",
                    ty("Vec", vec![p("T")]),
                    vec![],
                ),
            ))
        }),
        ErrorKind::Invalid,
        "declared twice",
    );
    expect_err(
        std_with(|b| {
            b.impls.push(imp(
                "$cap0",
                conf(vec![], "Show", ty("u64", vec![]), vec![]),
            ))
        }),
        ErrorKind::Invalid,
        "invalid impl key",
    );
    // Brands must identify a model of the declared trait.
    expect_err(
        std_with(|b| {
            b.models.push(model(
                "UuidEqAlt",
                conf(vec![], "Eq", ty("Uuid", vec![]), vec![]),
            ));
            b.impls.push(imp(
                "x",
                conf(
                    vec![],
                    "Eq",
                    TypeDto::Def {
                        def: tr("CacheBTree"),
                        args: vec![
                            ArgDto::Type(ty("Uuid", vec![])),
                            ArgDto::Type(ty("u64", vec![])),
                            ArgDto::Brand(BrandDto::Model(tr("UuidEqAlt"))),
                        ],
                    },
                    vec![],
                ),
            ));
        }),
        ErrorKind::Invalid,
        "models another trait",
    );
    expect_err(
        std_with(|b| {
            b.branded_types.push(BrandedTypeDto {
                ty: "Uuid2".into(),
                brands: vec![],
            })
        }),
        ErrorKind::Invalid,
        "not a source ADT",
    );
}

#[test]
fn unsupported_constructs_fail_closed() {
    expect_err(
        std_with(|b| {
            b.impls.push(imp(
                "x",
                conf(
                    vec![ParamDto::capability(
                        "c",
                        EffectUseDto {
                            effect: tr("Region"),
                            args: vec![],
                        },
                    )],
                    "Show",
                    ty("u64", vec![]),
                    vec![],
                ),
            ))
        }),
        ErrorKind::Unsupported,
        "capability parameters",
    );
    let long = TypeDto::Tuple((0..13).map(|_| ty("u8", vec![])).collect());
    expect_err(
        std_with(|b| b.impls.push(imp("x", conf(vec![], "Show", long, vec![])))),
        ErrorKind::Unsupported,
        "tuple arity 13",
    );
    // Function types, unresolved types and checker-internal labels have no
    // faithful source-facing Pact form.
    let f = vesper::parser::parse("fn f(x: fn(u64) -> u64) -> u64 { 0 }").module;
    let vesper::ast::Item::Function(f) = &f.items[0] else {
        panic!()
    };
    assert!(
        TypeDto::from_source(&f.params[0].ty)
            .unwrap_err()
            .contains("function type")
    );
    assert!(TypeDto::from_source(&vesper::ast::Type::Infer).is_err());
    let internal = vesper::ast::Type::Named(
        "Eff".into(),
        vec![vesper::ast::Type::Row(vesper::ast::Effects {
            instances: vec![vesper::ast::Effect {
                label: Some("$cap12".into()),
                name: "Region".into(),
                args: vec![],
            }],
            tail: None,
        })],
    );
    assert!(
        TypeDto::from_source(&internal)
            .unwrap_err()
            .contains("checker-internal")
    );
}

#[test]
fn incoherence_blocks_the_gate_even_when_the_solver_proves() {
    // Two canonical impls with identical heads: the solver sees identical
    // substitutions and reports Proven, but the gate must refuse.
    let b = std_with(|b| {
        b.impls.push(imp(
            "eq.uuid.again",
            conf(vec![], "Eq", ty("Uuid", vec![]), vec![]),
        ))
    });
    let lowered = b.lower().unwrap();
    let program = vesper_pact::Program::new(lowered.dto.clone()).unwrap();
    let uuid = lowered
        .tables
        .type_id(&DefPath::new(std_pkg(), DefKind::Type, "Uuid"))
        .unwrap();
    let eq = lowered
        .tables
        .trait_id(&DefPath::new(std_pkg(), DefKind::Trait, "Eq"))
        .unwrap();
    let q = vesper_pact::QueryDto::new(
        Default::default(),
        vec![],
        vesper_pact::Goal::implements(
            vesper_pact::TraitRef {
                trait_id: eq,
                self_ty: vesper_pact::Ty::Ctor {
                    ctor: uuid,
                    args: vec![],
                },
                args: vec![],
            },
            vesper_pact::Brand::Canonical,
        ),
    );
    assert!(Solver::new(&program).solve(&q).unwrap().is_proven());
    let e = b.build().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Incoherent);
    assert!(e.diagnostics.iter().any(|d| d.code == "PACT0102"), "{e}");
    assert!(e.mentions("impl @std/core@1.0.0::eq.uuid"), "{e}");

    // A blanket impl overlapping a concrete one.
    expect_err(
        std_with(|b| {
            b.impls.push(imp(
                "show.all",
                conf(vec![tparam("T")], "Show", p("T"), vec![]),
            ));
            b.impls.push(imp(
                "show.u64",
                conf(vec![], "Show", ty("u64", vec![]), vec![]),
            ));
        }),
        ErrorKind::Incoherent,
        "PACT0102",
    );

    // Orphan: the app package implements a std trait for a std type.
    let mut app = BindingsDto::empty(app_pkg());
    app.impls = vec![imp(
        "show.uuid",
        ConformanceDto {
            trait_ref: DefRefDto::qualified(std_pkg(), "Show"),
            ..conf(vec![], "Show", qty(std_pkg(), "Uuid", vec![]), vec![])
        },
    )];
    let orphan = SessionBuilder::new()
        .package(check(std_pkg(), STD_SOURCE), std_bindings())
        .package(check(app_pkg(), app_source()), app.clone());
    expect_err(orphan, ErrorKind::Incoherent, "PACT0101");
    // The same declaration as a named model is legal in any package.
    let mut app_model = BindingsDto::empty(app_pkg());
    app_model.models = vec![model("AppShowUuid", app.impls[0].decl.clone())];
    SessionBuilder::new()
        .package(check(std_pkg(), STD_SOURCE), std_bindings())
        .package(check(app_pkg(), app_source()), app_model)
        .build()
        .unwrap();
}

fn uuid() -> TypeDto {
    ty("Uuid", vec![])
}

#[test]
fn named_models_and_brands_stay_distinct() {
    let s = std_session();
    let rev = DefPath::new(std_pkg(), DefKind::Model, "ReverseUuidOrd");
    let o = s
        .solve(&query(
            std_pkg(),
            implements_via(uuid(), "Ord", BrandDto::Model(tr("ReverseUuidOrd"))),
        ))
        .unwrap();
    let c = s.consume(proven(&o)).unwrap();
    assert_eq!(
        c.selection,
        Selection::Model {
            path: rev.clone(),
            args: vec![]
        }
    );
    let o = s
        .solve(&query(
            std_pkg(),
            implements_via(uuid(), "Ord", BrandDto::Canonical),
        ))
        .unwrap();
    assert!(matches!(
        s.consume(proven(&o)).unwrap().selection,
        Selection::Impl { .. }
    ));
    // Implicit resolution: a lexical `use model` shadows the canonical impl.
    let scoped = |models: Vec<&str>| {
        QueryBindingDto::new(
            std_pkg(),
            EnvDto {
                use_models: vec![models.into_iter().map(tr).collect()],
                ..EnvDto::default()
            },
            vec![],
            implements(uuid(), "Ord"),
        )
    };
    let o = s.solve(&scoped(vec!["ReverseUuidOrd"])).unwrap();
    let c = s.consume(proven(&o)).unwrap();
    assert_eq!(
        c.selection,
        Selection::Model {
            path: rev,
            args: vec![]
        }
    );
    assert_eq!(
        c.substitution[0].1,
        "brand model @std/core@1.0.0::ReverseUuidOrd"
    );
    let o = s
        .solve(&scoped(vec!["ReverseUuidOrd", "OtherUuidOrd"]))
        .unwrap();
    assert!(
        matches!(
            &o,
            BridgeOutcome::Ambiguous { ambiguity: vesper_pact::Ambiguity::ConflictingScopedModels(_), explanation }
                if explanation.contains("ReverseUuidOrd")
        ),
        "{o:?}"
    );

    // Branded collections: identical layouts, distinct types.
    let cache = |brand: Option<BrandDto>| {
        let mut args = vec![ArgDto::Type(uuid()), ArgDto::Type(ty("u64", vec![]))];
        args.extend(brand.map(ArgDto::Brand));
        TypeDto::Def {
            def: tr("CacheBTree"),
            args,
        }
    };
    let canonical = s.translate_type(&std_pkg(), &cache(None)).unwrap();
    let explicit = s
        .translate_type(&std_pkg(), &cache(Some(BrandDto::Canonical)))
        .unwrap();
    let reverse = s
        .translate_type(
            &std_pkg(),
            &cache(Some(BrandDto::Model(tr("ReverseUuidOrd")))),
        )
        .unwrap();
    assert_eq!(canonical, explicit);
    assert_ne!(canonical, reverse);
    assert_eq!(s.ground_mode(&canonical), s.ground_mode(&reverse));
    proven(
        &s.solve(&query(std_pkg(), implements(cache(None), "Show")))
            .unwrap(),
    );
    let o = s
        .solve(&query(
            std_pkg(),
            implements(cache(Some(BrandDto::Model(tr("ReverseUuidOrd")))), "Show"),
        ))
        .unwrap();
    assert_eq!(o.verdict(), "no-solution");
}

fn container_bindings(b: &mut BindingsDto) {
    b.traits.push(TraitDto {
        name: "Container".into(),
        params: vec![],
        assoc: vec![AssocDto {
            name: "Elem".into(),
            mode_bound: ModeDto::Const(Mode::Linear),
            bounds: vec![],
        }],
        coinductive: false,
    });
    b.impls.push(imp(
        "container.vec",
        ConformanceDto {
            assoc: vec![("Elem".into(), p("T"))],
            ..conf(
                vec![tparam("T")],
                "Container",
                ty("Vec", vec![p("T")]),
                vec![],
            )
        },
    ));
    b.traits.push(simple_trait("Grow"));
    b.impls.push(imp(
        "grow.list",
        conf(
            vec![tparam("T")],
            "Grow",
            ty("List", vec![p("T")]),
            vec![implements(
                ty("List", vec![ty("List", vec![p("T")])]),
                "Grow",
            )],
        ),
    ));
}

#[test]
fn every_solver_outcome_is_preserved() {
    let s = std_with(container_bindings).build().unwrap();
    // Ambiguous: unknown self type.
    let q = QueryBindingDto::new(
        std_pkg(),
        EnvDto::default(),
        vec![tparam("X")],
        implements(p("X"), "Eq"),
    );
    let o = s.solve(&q).unwrap();
    assert!(
        matches!(
            o,
            BridgeOutcome::Ambiguous {
                ambiguity: vesper_pact::Ambiguity::SelfTypeUnknown,
                ..
            }
        ),
        "{o:?}"
    );
    // Proven with a substitution translated back to source names.
    let q = QueryBindingDto::new(
        std_pkg(),
        EnvDto::default(),
        vec![tparam("X")],
        GoalDto::ProjectionEq {
            projection: ProjectionDto {
                self_ty: ty("Vec", vec![uuid()]),
                trait_ref: tr("Container"),
                args: vec![],
                brand: None,
                assoc: "Elem".into(),
            },
            ty: p("X"),
        },
    );
    let o = s.solve(&q).unwrap();
    let c = s.consume(proven(&o)).unwrap();
    assert_eq!(
        c.substitution[0],
        ("X".into(), "@std/core@1.0.0::Uuid".into())
    );
    // NoSolution with a named explanation.
    let o = s
        .solve(&query(std_pkg(), implements(ty("WriteTxn", vec![]), "Eq")))
        .unwrap();
    match &o {
        BridgeOutcome::NoSolution { explanation, .. } => {
            assert!(
                explanation.contains("@std/core@1.0.0::WriteTxn"),
                "{explanation}"
            )
        }
        other => panic!("{other:?}"),
    }
    // Overflow: unbounded non-cyclic recursion.
    let o = s
        .solve(&query(
            std_pkg(),
            implements(ty("List", vec![uuid()]), "Grow"),
        ))
        .unwrap();
    assert!(
        matches!(
            o,
            BridgeOutcome::Overflow {
                kind: OverflowKind::Depth
            }
        ),
        "{o:?}"
    );
}

#[test]
fn pact_unsupported_outcomes_are_not_acceptance() {
    let s = std_with(|b| {
        b.traits.push(TraitDto {
            name: "Run".into(),
            params: vec![ParamDto::of("r", ParamKindDto::Row)],
            assoc: vec![],
            coinductive: false,
        });
        b.traits.push(TraitDto {
            name: "Inner".into(),
            params: vec![ParamDto::of("r", ParamKindDto::Row)],
            assoc: vec![],
            coinductive: false,
        });
        let row = |labels: Vec<LabelDto>, tail: Option<&str>| {
            ArgDto::Row(RowDto {
                labels,
                tail: tail.map(str::to_string),
            })
        };
        let region = || LabelDto {
            capability: None,
            effect: tr("Region"),
            args: vec![],
        };
        b.impls.push(imp(
            "run.u64",
            ConformanceDto {
                args: vec![row(vec![], Some("r"))],
                where_clauses: vec![GoalDto::Implements {
                    self_ty: ty("u64", vec![]),
                    trait_ref: tr("Inner"),
                    args: vec![row(vec![region()], Some("r"))],
                    brand: None,
                }],
                ..conf(
                    vec![ParamDto::of("r", ParamKindDto::Row)],
                    "Run",
                    ty("u64", vec![]),
                    vec![],
                )
            },
        ));
        b.impls.push(imp(
            "inner.u64",
            ConformanceDto {
                args: vec![row(vec![], Some("r"))],
                ..conf(
                    vec![ParamDto::of("r", ParamKindDto::Row)],
                    "Inner",
                    ty("u64", vec![]),
                    vec![],
                )
            },
        ));
    })
    .build()
    .unwrap();
    let q = query(
        std_pkg(),
        GoalDto::Implements {
            self_ty: ty("u64", vec![]),
            trait_ref: tr("Run"),
            args: vec![ArgDto::Row(RowDto {
                labels: vec![LabelDto {
                    capability: None,
                    effect: tr("Region"),
                    args: vec![],
                }],
                tail: None,
            })],
            brand: Some(BrandDto::Canonical),
        },
    );
    let o = s.solve(&q).unwrap();
    assert!(
        matches!(&o, BridgeOutcome::Unsupported { pact: Some(_), explanation } if explanation.contains("Region")),
        "{o:?}"
    );
    assert!(o.evidence().is_none());
}

#[test]
fn bounded_overflow_never_succeeds() {
    let s = std_with(container_bindings)
        .solver_config(SolverConfig {
            max_steps: 2,
            ..SolverConfig::default()
        })
        .build()
        .unwrap();
    let deep = ty(
        "Vec",
        vec![ty("Vec", vec![ty("Vec", vec![ty("List", vec![uuid()])])])],
    );
    let o = s.solve(&query(std_pkg(), implements(deep, "Eq"))).unwrap();
    assert!(
        matches!(
            o,
            BridgeOutcome::Overflow {
                kind: OverflowKind::Steps
            }
        ),
        "{o:?}"
    );
}

fn region_cap(name: &str) -> ParamDto {
    ParamDto::capability(
        name,
        EffectUseDto {
            effect: tr("Region"),
            args: vec![],
        },
    )
}

#[test]
fn rows_and_capabilities_alpha_rename() {
    let s = std_with(|b| {
        b.traits.push(TraitDto {
            name: "Run".into(),
            params: vec![ParamDto::of("r", ParamKindDto::Row)],
            assoc: vec![],
            coinductive: false,
        });
        b.impls.push(imp(
            "run.uuid",
            ConformanceDto {
                args: vec![ArgDto::Row(RowDto {
                    labels: vec![],
                    tail: Some("r".into()),
                })],
                ..conf(
                    vec![ParamDto::of("r", ParamKindDto::Row)],
                    "Run",
                    uuid(),
                    vec![],
                )
            },
        ));
    })
    .build()
    .unwrap();
    let q = |row: &str, cap: &str, swap: bool| {
        let mut params = vec![ParamDto::of(row, ParamKindDto::Row), region_cap(cap)];
        if swap {
            params.reverse();
        }
        QueryBindingDto::new(
            std_pkg(),
            EnvDto {
                params,
                ..EnvDto::default()
            },
            vec![],
            GoalDto::Implements {
                self_ty: uuid(),
                trait_ref: tr("Run"),
                args: vec![ArgDto::Row(RowDto {
                    labels: vec![LabelDto {
                        capability: Some(cap.into()),
                        effect: tr("Region"),
                        args: vec![],
                    }],
                    tail: Some(row.into()),
                })],
                brand: None,
            },
        )
    };
    let a = s.translate_query(&q("r", "scratch", false)).unwrap();
    let b = s.translate_query(&q("tail", "arena", false)).unwrap();
    let c = s.translate_query(&q("r", "scratch", true)).unwrap();
    assert_eq!(a.query(), b.query());
    assert_ne!(a.query(), c.query());
    assert_eq!(
        canonicalize_query(a.query()).unwrap(),
        canonicalize_query(c.query()).unwrap()
    );
    for query in [
        q("r", "scratch", false),
        q("tail", "arena", false),
        q("r", "scratch", true),
    ] {
        s.consume(&s_proven(&s, &query)).unwrap();
    }
    // Names stay in the bridge: the Pact query carries none of them.
    let json = serde_json::to_string(a.query()).unwrap();
    for name in ["scratch", "Uuid", "Region", "Run", "@std"] {
        assert!(!json.contains(name), "{name} leaked into {json}");
    }
    // A capability must label the effect it was declared with.
    let mut bad = q("r", "scratch", false);
    bad.env.params[1] = ParamDto::capability(
        "scratch",
        EffectUseDto {
            effect: tr("Fail"),
            args: vec![ArgDto::Type(ty("u64", vec![]))],
        },
    );
    let e = s.translate_query(&bad).unwrap_err();
    assert!(e.mentions("different effect"), "{e}");
}

#[test]
fn malformed_queries_are_diagnosed() {
    let s = std_session();
    let e = s
        .solve(&QueryBindingDto::new(
            std_pkg(),
            EnvDto::default(),
            vec![tparam("Unused")],
            implements(uuid(), "Eq"),
        ))
        .unwrap_err();
    assert!(e.mentions("does not occur"), "{e}");
    let e = s
        .solve(&QueryBindingDto::new(
            std_pkg(),
            EnvDto::default(),
            vec![tparam_bound("X", Mode::Copy)],
            implements(p("X"), "Eq"),
        ))
        .unwrap_err();
    assert!(e.mentions("cannot carry a bound"), "{e}");
    let e = s
        .solve(&query(app_pkg(), implements(uuid(), "Eq")))
        .unwrap_err();
    assert!(e.mentions("not a package of this session"), "{e}");
    let e = s
        .solve(&query(std_pkg(), implements(p("Nope"), "Eq")))
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::InvalidQuery);
    let mut q = query(std_pkg(), implements(uuid(), "Eq"));
    q.schema = BindingSchema { major: 1, minor: 0 };
    assert!(s.solve(&q).is_err());
}

#[test]
fn mode_parameters_are_rigid() {
    let s = std_session();
    let q = |t: TypeDto| {
        QueryBindingDto::new(
            std_pkg(),
            EnvDto {
                params: vec![ParamDto::of("m", ParamKindDto::Mode)],
                ..EnvDto::default()
            },
            vec![],
            GoalDto::ModeBound {
                ty: t,
                bound: ModeDto::Param("m".into()),
            },
        )
    };
    s.consume(&s_proven(&s, &q(uuid()))).unwrap();
    assert_eq!(
        s.solve(&q(ty("String", vec![]))).unwrap().verdict(),
        "no-solution"
    );
}

#[test]
fn declaration_changes_invalidate_evidence() {
    let s1 = std_session();
    let q = query(std_pkg(), implements(ty("List", vec![uuid()]), "Eq"));
    let e = s_proven(&s1, &q);
    let s2 = std_with(|b| {
        b.impls.push(imp(
            "show.u64",
            conf(vec![], "Show", ty("u64", vec![]), vec![]),
        ))
    })
    .build()
    .unwrap();
    assert_ne!(s1.revision(), s2.revision());
    let err = s2.consume(&e).unwrap_err();
    assert_eq!(err.kind, ErrorKind::StaleEvidence);
    assert_eq!(
        s2.accept_certificate(&e.certificate()).unwrap_err().kind,
        ErrorKind::StaleEvidence
    );
    // A source change (new ADT) also changes the revision.
    let s3 = SessionBuilder::new()
        .package(
            check(
                std_pkg(),
                &format!("{STD_SOURCE} copy struct Extra {{ x: u8 }}"),
            ),
            std_bindings(),
        )
        .build()
        .unwrap();
    assert_eq!(s3.consume(&e).unwrap_err().kind, ErrorKind::StaleEvidence);
    // Rebuilding the same declarations reproduces the revision.
    std_session().consume(&e).unwrap();
}

#[test]
fn malformed_certificates_return_diagnostics() {
    let s = std_session();
    let q = query(std_pkg(), implements(ty("List", vec![uuid()]), "Eq"));
    let e = s_proven(&s, &q);
    let good = e.certificate();
    // Round trip through JSON.
    let json = serde_json::to_string(&good).unwrap();
    let back: Certificate = serde_json::from_str(&json).unwrap();
    let accepted = s.accept_certificate(&back).unwrap();
    assert_eq!(accepted, e);

    let mut tampered = vec![];
    let mut c = good.clone();
    c.revision = Revision("00".into());
    tampered.push((c, ErrorKind::StaleEvidence));
    let mut c = good.clone();
    c.derivation.children.clear();
    tampered.push((c, ErrorKind::InvalidEvidence));
    let mut c = good.clone();
    c.derivation.step = vesper_pact::Step::Conformance {
        source: vesper_pact::ConformanceSource::Impl(vesper_pact::ImplId(u32::MAX)),
        args: vec![],
    };
    tampered.push((c, ErrorKind::InvalidEvidence));
    let mut c = good.clone();
    if let vesper_pact::Goal::Implements { brand, .. } = &mut c.derivation.goal {
        *brand = vesper_pact::Brand::Named(vesper_pact::ModelId(0));
    }
    tampered.push((c, ErrorKind::InvalidEvidence));
    let mut c = good.clone();
    c.var_values = vec![vesper_pact::GenericArg::Mode(vesper_pact::ModeTerm::Const(
        vesper_pact::Mode::Copy,
    ))];
    tampered.push((c, ErrorKind::InvalidEvidence));
    let mut c = good.clone();
    c.query.goal = vesper_pact::Goal::ModeBound {
        ty: vesper_pact::Ty::Ctor {
            ctor: vesper_pact::TypeCtorId(u32::MAX),
            args: vec![],
        },
        bound: vesper_pact::ModeTerm::Const(vesper_pact::Mode::Linear),
    };
    tampered.push((c, ErrorKind::InvalidEvidence));
    let mut c = good.clone();
    c.binder_names.push("extra".into());
    tampered.push((c, ErrorKind::InvalidEvidence));
    let mut c = good.clone();
    c.schema = BindingSchema { major: 7, minor: 0 };
    tampered.push((c, ErrorKind::InvalidEvidence));
    // The derivation proves a different (valid) goal than the query asks.
    let other = s_proven(&s, &query(std_pkg(), implements(uuid(), "Eq")));
    let mut c = good.clone();
    c.derivation = other.derivation().clone();
    tampered.push((c, ErrorKind::InvalidEvidence));
    for (c, kind) in tampered {
        let err = s.accept_certificate(&c).unwrap_err();
        assert_eq!(err.kind, kind, "{err}");
    }
    // Garbage JSON is a deserialization error, not a panic.
    assert!(serde_json::from_str::<Certificate>("{\"schema\":1}").is_err());
}

#[test]
fn engine_tables_answers_across_queries() {
    let s = std_session();
    let mut engine = s.engine();
    let q = query(
        std_pkg(),
        implements(ty("Vec", vec![ty("List", vec![uuid()])]), "Eq"),
    );
    let first = engine.solve(&q).unwrap();
    assert!(engine.table_len() > 0);
    let second = engine.solve(&q).unwrap();
    assert_eq!(proven(&first), proven(&second));
}

#[test]
fn tuples_and_unit_are_shared_builtin_constructors() {
    let s = std_session();
    let pair = TypeDto::Tuple(vec![uuid(), ty("List", vec![ty("u64", vec![])])]);
    s.consume(&s_proven(&s, &query(std_pkg(), implements(pair, "Eq"))))
        .unwrap();
    let unit = s
        .translate_type(&std_pkg(), &TypeDto::Tuple(vec![]))
        .unwrap();
    assert_eq!(
        unit,
        s.translate_type(&std_pkg(), &ty("Unit", vec![])).unwrap()
    );
    // Tuple constructors cannot be named directly.
    assert!(
        s.translate_type(&std_pkg(), &ty("(tuple/2)", vec![uuid(), uuid()]))
            .is_err()
    );
}

#[test]
fn environment_assumptions_and_template_bounds() {
    let s = std_session();
    // fn f[T](..) where T: Eq  |-  List[T]: Eq
    let q = QueryBindingDto::new(
        std_pkg(),
        EnvDto {
            params: vec![tparam("T")],
            assumptions: vec![implements(p("T"), "Eq")],
            use_models: vec![],
        },
        vec![],
        implements(ty("List", vec![p("T")]), "Eq"),
    );
    let c = s.consume(&s_proven(&s, &q)).unwrap();
    assert!(matches!(c.selection, Selection::Impl { .. }));
    assert!(
        c.explanation.contains("by assumption 0"),
        "{}",
        c.explanation
    );
    assert!(c.explanation.contains("List[T]"), "{}", c.explanation);
    // Without the assumption the rigid parameter has no impl.
    let mut q2 = q.clone();
    q2.env.assumptions.clear();
    assert_eq!(s.solve(&q2).unwrap().verdict(), "no-solution");

    // Impl templates must respect declared bounds of the types they mention.
    let src = "copy struct CopyBox[T: mod copy] { t: T }";
    let pkg = PackageIdentity::new("@test/bounds", "1.0.0");
    let bindings = |bound: Option<Mode>| {
        let mut b = BindingsDto::empty(pkg.clone());
        b.traits = vec![simple_trait("Show")];
        b.impls = vec![imp(
            "show.box",
            conf(
                vec![ParamDto::ty("T", bound)],
                "Show",
                ty("CopyBox", vec![p("T")]),
                vec![],
            ),
        )];
        b
    };
    let e = SessionBuilder::new()
        .package(check(pkg.clone(), src), bindings(None))
        .build()
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Invalid);
    assert!(e.mentions("T: mod Copy"), "{e}");
    let s = SessionBuilder::new()
        .package(check(pkg.clone(), src), bindings(Some(Mode::Copy)))
        .build()
        .unwrap();
    let ok = query(
        pkg.clone(),
        implements(ty("CopyBox", vec![ty("u8", vec![])]), "Show"),
    );
    s.consume(&s_proven(&s, &ok)).unwrap();
    // A solution that would instantiate the bounded parameter with an
    // affine type is never accepted, even through inference.
    let mut b = bindings(Some(Mode::Copy));
    b.traits.push(TraitDto {
        name: "Pick".into(),
        params: vec![tparam("A"), tparam("B")],
        assoc: vec![],
        coinductive: false,
    });
    b.impls.push(imp(
        "pick.string",
        ConformanceDto {
            args: vec![ArgDto::Type(ty("String", vec![])), ArgDto::Type(p("T"))],
            ..conf(vec![tparam("T")], "Pick", ty("u8", vec![]), vec![])
        },
    ));
    let s = SessionBuilder::new()
        .package(check(pkg.clone(), src), b)
        .build()
        .unwrap();
    let pick = |x: TypeDto| {
        QueryBindingDto::new(
            pkg.clone(),
            EnvDto::default(),
            vec![tparam("X")],
            GoalDto::Implements {
                self_ty: ty("u8", vec![]),
                trait_ref: tr("Pick"),
                args: vec![ArgDto::Type(p("X")), ArgDto::Type(x)],
                brand: None,
            },
        )
    };
    // The solver binds X := String, making CopyBox[X] ill-formed.
    let o = s.solve(&pick(ty("CopyBox", vec![p("X")]))).unwrap();
    assert!(
        matches!(&o, BridgeOutcome::Unsupported { pact: None, explanation } if explanation.contains("well-formed")),
        "{o:?}"
    );
    assert!(o.evidence().is_none());
    let ok = s.solve(&pick(ty("Option", vec![p("X")]))).unwrap();
    assert_eq!(s.consume(proven(&ok)).unwrap().substitution[0].1, "String");
}

#[test]
fn pact_validation_failures_block_the_gate() {
    // An impl parameter that does not occur in the head (PACT0008).
    expect_err(
        std_with(|b| {
            b.impls.push(imp(
                "show.u64",
                conf(vec![tparam("T")], "Show", ty("u64", vec![]), vec![]),
            ))
        }),
        ErrorKind::Incoherent,
        "PACT0008",
    );
    // `Self` is reserved.
    expect_err(
        std_with(|b| {
            b.impls.push(imp(
                "show.vec",
                conf(
                    vec![tparam("Self")],
                    "Show",
                    ty("Vec", vec![p("Self")]),
                    vec![],
                ),
            ))
        }),
        ErrorKind::Invalid,
        "reserved",
    );
}

#[test]
fn example_fixture_binds_real_source() {
    let pkg = PackageIdentity::new("@demo/geometry", "1.0.0");
    let checked = check(pkg.clone(), include_str!("../examples/geometry.ves"));
    let bindings: BindingsDto =
        serde_json::from_str(include_str!("../examples/geometry.bindings.json")).unwrap();
    let s = SessionBuilder::new()
        .package(checked, bindings)
        .build()
        .unwrap();
    let q = query(
        pkg,
        implements(ty("Layer", vec![ty("Point", vec![])]), "Eq"),
    );
    let c = s.consume(&s_proven(&s, &q)).unwrap();
    assert_eq!(
        c.selection.to_string(),
        "impl @demo/geometry@1.0.0::eq.layer[@demo/geometry@1.0.0::Point]"
    );
}

const WF_SOURCE: &str = "
copy struct CopyBox[T: mod copy] { t: T }
copy struct Uuid { hi: u64 }
struct Index[K, V] { key: Option[K], value: Option[V] }
effect Emit[T: mod copy] { fn emit(x: T) -> Unit; }
";

fn wf_session() -> (PackageIdentity, CoherentSession) {
    let pkg = PackageIdentity::new("@test/wf", "1.0.0");
    let mut b = BindingsDto::empty(pkg.clone());
    b.branded_types = vec![BrandedTypeDto {
        ty: "Index".into(),
        brands: vec![BrandParamDto {
            name: "M".into(),
            trait_ref: tr("Ord"),
        }],
    }];
    b.traits = vec![
        simple_trait("Eq"),
        simple_trait("Ord"),
        // PickA[A, B] { type Out }
        TraitDto {
            name: "PickA".into(),
            params: vec![tparam("A"), tparam("B")],
            assoc: vec![AssocDto {
                name: "Out".into(),
                mode_bound: ModeDto::Const(Mode::Linear),
                bounds: vec![],
            }],
            coinductive: false,
        },
        TraitDto {
            name: "Two".into(),
            params: vec![tparam("A"), ParamDto::of("r", ParamKindDto::Row)],
            assoc: vec![],
            coinductive: false,
        },
        TraitDto {
            name: "PickB".into(),
            params: vec![ParamDto::of("M", ParamKindDto::Brand), tparam("T")],
            assoc: vec![],
            coinductive: false,
        },
    ];
    b.models = vec![model(
        "UuidEq",
        conf(vec![], "Eq", ty("Uuid", vec![]), vec![]),
    )];
    b.impls = vec![
        imp(
            "picka.string",
            ConformanceDto {
                args: vec![ArgDto::Type(ty("String", vec![])), ArgDto::Type(p("T"))],
                assoc: vec![("Out".into(), ty("u8", vec![]))],
                ..conf(vec![tparam("T")], "PickA", ty("u8", vec![]), vec![])
            },
        ),
        imp(
            "two.string",
            ConformanceDto {
                args: vec![
                    ArgDto::Type(ty("String", vec![])),
                    ArgDto::Row(RowDto {
                        labels: vec![],
                        tail: Some("r".into()),
                    }),
                ],
                ..conf(
                    vec![ParamDto::of("r", ParamKindDto::Row)],
                    "Two",
                    ty("u8", vec![]),
                    vec![],
                )
            },
        ),
        imp(
            "pickb.eqmodel",
            ConformanceDto {
                args: vec![
                    ArgDto::Brand(BrandDto::Model(tr("UuidEq"))),
                    ArgDto::Type(p("T")),
                ],
                ..conf(vec![tparam("T")], "PickB", ty("u8", vec![]), vec![])
            },
        ),
    ];
    let s = SessionBuilder::new()
        .package(check(pkg.clone(), WF_SOURCE), b)
        .build()
        .unwrap_or_else(|e| panic!("{e}"));
    (pkg, s)
}

fn assert_rejected_solution(s: &CoherentSession, q: &QueryBindingDto) {
    let o = s.solve(q).unwrap();
    assert!(
        matches!(&o, BridgeOutcome::Unsupported { pact: None, explanation } if explanation.contains("well-formed")),
        "{o:?}"
    );
}

#[test]
fn solutions_are_rechecked_inside_projection_effect_and_brand_arguments() {
    let (pkg, s) = wf_session();
    // Projection trait arguments: X := String makes CopyBox[X] ill-formed.
    let q = QueryBindingDto::new(
        pkg.clone(),
        EnvDto::default(),
        vec![tparam("X")],
        GoalDto::ProjectionEq {
            projection: ProjectionDto {
                self_ty: ty("u8", vec![]),
                trait_ref: tr("PickA"),
                args: vec![
                    ArgDto::Type(p("X")),
                    ArgDto::Type(ty("CopyBox", vec![p("X")])),
                ],
                brand: None,
                assoc: "Out".into(),
            },
            ty: ty("u8", vec![]),
        },
    );
    assert_rejected_solution(&s, &q);
    // Effect-label arguments: Emit[X] with X := String violates `T: mod copy`.
    let emit = |arg: TypeDto| {
        ArgDto::Row(RowDto {
            labels: vec![LabelDto {
                capability: None,
                effect: tr("Emit"),
                args: vec![ArgDto::Type(arg)],
            }],
            tail: None,
        })
    };
    let q = QueryBindingDto::new(
        pkg.clone(),
        EnvDto::default(),
        vec![tparam("X")],
        GoalDto::Implements {
            self_ty: ty("u8", vec![]),
            trait_ref: tr("Two"),
            args: vec![ArgDto::Type(p("X")), emit(p("X"))],
            brand: None,
        },
    );
    assert_rejected_solution(&s, &q);
    // Constructor brands: M := UuidEq (a model of Eq) for an Ord brand.
    let q = QueryBindingDto::new(
        pkg.clone(),
        EnvDto::default(),
        vec![ParamDto::of("M", ParamKindDto::Brand)],
        GoalDto::Implements {
            self_ty: ty("u8", vec![]),
            trait_ref: tr("PickB"),
            args: vec![
                ArgDto::Brand(BrandDto::Param("M".into())),
                ArgDto::Type(TypeDto::Def {
                    def: tr("Index"),
                    args: vec![
                        ArgDto::Type(ty("Uuid", vec![])),
                        ArgDto::Type(ty("u8", vec![])),
                        ArgDto::Brand(BrandDto::Param("M".into())),
                    ],
                }),
            ],
            brand: None,
        },
    );
    assert_rejected_solution(&s, &q);
}

#[test]
fn source_effect_bounds_are_enforced() {
    let (pkg, s) = wf_session();
    let row = |arg: &str| {
        ArgDto::Row(RowDto {
            labels: vec![LabelDto {
                capability: None,
                effect: tr("Emit"),
                args: vec![ArgDto::Type(ty(arg, vec![]))],
            }],
            tail: None,
        })
    };
    let goal = |arg| GoalDto::Implements {
        self_ty: ty("u8", vec![]),
        trait_ref: tr("Two"),
        args: vec![ArgDto::Type(ty("String", vec![])), row(arg)],
        brand: None,
    };
    let e = s.solve(&query(pkg.clone(), goal("String"))).unwrap_err();
    assert!(e.mentions("of `effect @test/wf@1.0.0::Emit`"), "{e}");
    s.consume(&s_proven(&s, &query(pkg, goal("u64")))).unwrap();
}

#[test]
fn bridge_only_constraints_change_the_revision() {
    let base = std_session();
    // Same lowered ProgramDto, different brand trait.
    let other = std_with(|b| b.branded_types[0].brands[0].trait_ref = tr("Eq"))
        .build()
        .unwrap();
    assert_eq!(base.program_dto(), other.program_dto());
    assert_ne!(base.revision(), other.revision());
    let e = s_proven(&base, &query(std_pkg(), implements(uuid(), "Eq")));
    assert_eq!(
        other.consume(&e).unwrap_err().kind,
        ErrorKind::StaleEvidence
    );
    // A source parameter bound that leaves the mode scheme unchanged.
    let build = |extra: &str| {
        SessionBuilder::new()
            .package(
                check(std_pkg(), &format!("{STD_SOURCE}{extra}")),
                std_bindings(),
            )
            .build()
            .unwrap()
    };
    let loose = build("struct Spare[T] { s: u64 }");
    let tight = build("struct Spare[T: mod affine] { s: u64 }");
    assert_eq!(loose.program_dto(), tight.program_dto());
    assert_ne!(loose.revision(), tight.revision());
}
