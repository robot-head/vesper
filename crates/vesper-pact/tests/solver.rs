mod common;

use common::*;
use vesper_pact::*;

// ---------------------------------------------------------------------------
// Basic resolution and diagnostics

#[test]
fn nested_impls_resolve_with_checked_derivation() {
    let p = world();
    let goal = implements(CLONE, t1(VEC, t1(OPTION, t1(VEC, c(U64)))), vec![]);
    let o = solve_goal(&p, goal);
    let s = proven(&o);
    assert_eq!(s.proof_kind, ProofKind::Finite);
    assert_eq!(s.derivation.size(), 4);
    match &s.derivation.step {
        Step::Conformance { source, .. } => assert_eq!(*source, ConformanceSource::Impl(ImplId(2))),
        other => panic!("{other:?}"),
    }
}

#[test]
fn missing_impl_is_no_solution_with_deterministic_explanation() {
    let p = world();
    let goal = implements(CLONE, t1(VEC, t1(OPTION, c(WRITE_TXN))), vec![]);
    let expected = "\
`type#2[type#3[type#4]]: trait#0 via canonical`: every candidate failed
  impl#2 requires
    `type#3[type#4]: trait#0 via canonical`: every candidate failed
      impl#3 requires
        `type#4: trait#0 via canonical`: no matching impl, model or assumption
";
    let first = failure(&solve_goal(&p, goal.clone())).render();
    assert_eq!(first, expected);
    // A solver warmed with unrelated and related queries renders identically.
    let mut s = Solver::new(&p);
    for g in [
        implements(CLONE, t1(VEC, c(U64)), vec![]),
        implements(CLONE, c(WRITE_TXN), vec![]),
        implements(CLONE, t1(OPTION, c(WRITE_TXN)), vec![]),
    ] {
        let _ = solve_with(&mut s, &p, &query(Env::default(), vec![], g));
    }
    let again = solve_with(&mut s, &p, &query(Env::default(), vec![], goal));
    assert_eq!(failure(&again).render(), expected);
    assert!(s.stats().cache_hits > 0);
}

#[test]
fn unresolved_self_type_is_ambiguous_not_success() {
    let p = world();
    let o = solve(
        &p,
        &query(
            Env::default(),
            vec![ParamKind::Type],
            implements(CLONE, inf(0), vec![]),
        ),
    );
    assert_eq!(o, Outcome::Ambiguous(Ambiguity::SelfTypeUnknown));
    let o = solve(
        &p,
        &query(
            Env::default(),
            vec![ParamKind::Type],
            implements(CLONE, t1(VEC, inf(0)), vec![]),
        ),
    );
    assert_eq!(o, Outcome::Ambiguous(Ambiguity::SelfTypeUnknown));
}

// ---------------------------------------------------------------------------
// Recursive goals and cycles

#[test]
fn coinductive_cycle_is_proven_and_marked() {
    let p = world();
    let o = solve_goal(&p, implements(SEND, t1(LIST, t1(LIST, c(U64))), vec![]));
    assert_eq!(o.verdict(), "proven-coinductive");
    let s = proven(&o);
    assert!(s.derivation.uses_coinduction());
    assert!(
        s.derivation
            .render()
            .contains("[by coinductive hypothesis]")
    );
}

#[test]
fn coinductive_cycle_still_requires_other_obligations() {
    let p = world();
    let o = solve_goal(&p, implements(SEND, t1(LIST, t1(RC, c(U64))), vec![]));
    let f = failure(&o);
    assert!(
        f.render()
            .contains("`type#13[type#0]: trait#1 via canonical`: no matching impl")
    );
}

#[test]
fn inductive_cycle_without_base_case_fails() {
    let p = world();
    let o = solve_goal(&p, implements(LOOP, t1(WRAP, c(U64)), vec![]));
    let f = failure(&o);
    let FailureReason::CandidatesFailed(cs) = &f.reason else {
        panic!("{f:?}")
    };
    let CandidateCause::Nested(inner) = &cs[0].cause else {
        panic!()
    };
    assert_eq!(inner.reason, FailureReason::InductiveCycle);
}

#[test]
fn cycle_through_an_inductive_goal_is_inductive() {
    // Send (coinductive) -> Clone (inductive) -> Send: not a coinductive cycle.
    let p = world();
    let mut s = Solver::new(&p);
    let o = solve_with(
        &mut s,
        &p,
        &query(
            Env::default(),
            vec![],
            implements(SEND, t1(WRAP, c(U64)), vec![]),
        ),
    );
    assert!(
        failure(&o)
            .render()
            .contains("inductive cycle without a base case")
    );
    assert!(s.stats().inductive_cycles >= 1);
    assert_eq!(s.stats().coinductive_cycles, 0);
}

#[test]
fn unbounded_non_cyclic_recursion_overflows() {
    let p = world();
    let o = solve_goal(&p, implements(GROW, t1(DEEP, c(U64)), vec![]));
    assert_eq!(o, Outcome::Overflow(OverflowKind::Depth));
    assert_ne!(o.verdict(), "proven-coinductive");
}

#[test]
fn step_budget_overflow_is_indeterminate() {
    let p = world();
    let mut s = Solver::with_config(
        &p,
        SolverConfig {
            max_steps: 3,
            ..SolverConfig::default()
        },
    );
    let q = query(
        Env::default(),
        vec![],
        implements(CLONE, t1(VEC, t1(VEC, t1(VEC, t1(VEC, c(U64))))), vec![]),
    );
    assert_eq!(s.solve(&q).unwrap(), Outcome::Overflow(OverflowKind::Steps));
}

#[test]
fn cycle_members_are_not_tabled_with_provisional_answers() {
    let p = world();
    let mut s = Solver::new(&p);
    let o1 = solve_with(
        &mut s,
        &p,
        &query(
            Env::default(),
            vec![],
            implements(SEND, t1(LIST, c(U64)), vec![]),
        ),
    );
    let o2 = solve_with(
        &mut s,
        &p,
        &query(
            Env::default(),
            vec![],
            implements(SEND, t1(LIST, c(U64)), vec![]),
        ),
    );
    assert_eq!(o1, o2);
    let o3 = solve_with(
        &mut s,
        &p,
        &query(
            Env::default(),
            vec![],
            implements(LOOP, t1(WRAP, c(U64)), vec![]),
        ),
    );
    let o4 = solve_with(
        &mut s,
        &p,
        &query(
            Env::default(),
            vec![],
            implements(LOOP, t1(WRAP, c(U64)), vec![]),
        ),
    );
    assert_eq!(o3, o4);
}

// ---------------------------------------------------------------------------
// Canonical queries and tabling

#[test]
fn alpha_equivalent_queries_share_a_canonical_key_and_table_entry() {
    let p = world();
    // fn f[T: Clone] asking Vec[Option[T]]: Clone.
    let env_a = Env {
        placeholders: vec![PlaceholderDecl::ty(None)],
        assumptions: vec![implements(CLONE, ph(0), vec![])],
        model_scopes: vec![],
    };
    // fn g[A, B, C: Clone] (A, B unused) asking the same about C.
    let env_b = Env {
        placeholders: vec![
            PlaceholderDecl::ty(None),
            PlaceholderDecl::ty(None),
            PlaceholderDecl::ty(None),
        ],
        assumptions: vec![implements(CLONE, ph(2), vec![])],
        model_scopes: vec![vec![]],
    };
    let qa = query(
        env_a,
        vec![],
        implements(CLONE, t1(VEC, t1(OPTION, ph(0))), vec![]),
    );
    let qb = query(
        env_b,
        vec![],
        implements(CLONE, t1(VEC, t1(OPTION, ph(2))), vec![]),
    );
    assert_eq!(
        canonicalize_query(&qa).unwrap(),
        canonicalize_query(&qb).unwrap()
    );

    let mut s = Solver::new(&p);
    let oa = solve_with(&mut s, &p, &qa);
    let before = s.stats();
    let ob = solve_with(&mut s, &p, &qb);
    let after = s.stats();
    assert_eq!(
        after.evaluations, before.evaluations,
        "second query answered from the table"
    );
    assert!(after.cache_hits > before.cache_hits);
    // Answers are reported in each caller's own placeholder numbering.
    assert!(proven(&oa).derivation.render().contains("!0: trait#0"));
    assert!(proven(&ob).derivation.render().contains("!2: trait#0"));
}

#[test]
fn inference_variable_numbering_is_canonical() {
    let q1 = query(
        Env::default(),
        vec![ParamKind::Type, ParamKind::Type],
        mode_bound(t(PAIR, vec![a(inf(0)), a(inf(1))]), Mode::Linear),
    );
    let q2 = query(
        Env::default(),
        vec![ParamKind::Type, ParamKind::Type],
        mode_bound(t(PAIR, vec![a(inf(1)), a(inf(0))]), Mode::Linear),
    );
    let (_, g1) = canonicalize_query(&q1).unwrap();
    let (_, g2) = canonicalize_query(&q2).unwrap();
    assert_eq!(g1, g2);
    let q3 = query(
        Env::default(),
        vec![ParamKind::Type],
        mode_bound(t(PAIR, vec![a(inf(0)), a(inf(0))]), Mode::Linear),
    );
    assert_ne!(
        canonicalize_query(&q3).unwrap().1,
        g1,
        "sharing is preserved"
    );
}

#[test]
fn capability_labels_canonicalize_by_first_occurrence() {
    let env = |n: usize| Env {
        placeholders: (0..n)
            .map(|_| PlaceholderDecl::of(ParamKind::Capability))
            .collect(),
        assumptions: vec![],
        model_scopes: vec![],
    };
    let goal = |cap: u32| {
        implements(
            SOURCE,
            c(PULSE_TCP),
            vec![row(vec![
                eff(NET),
                EffectLabel::Capability(Var::Placeholder(cap)),
            ])],
        )
    };
    let q1 = query(env(1), vec![], goal(0));
    let q2 = query(env(4), vec![], goal(3));
    assert_eq!(
        canonicalize_query(&q1).unwrap(),
        canonicalize_query(&q2).unwrap()
    );
    // The capability is not part of PulseTcp's row.
    assert!(matches!(solve(&world(), &q1), Outcome::NoSolution(_)));
}

// ---------------------------------------------------------------------------
// Effect-row arguments

#[test]
fn effect_row_argument_is_inferred() {
    let p = world();
    let o = solve(
        &p,
        &query(
            Env::default(),
            vec![ParamKind::Row],
            implements(
                SOURCE,
                t1(TILED_SEQ, c(U64)),
                vec![open_row(vec![], Var::Infer(0))],
            ),
        ),
    );
    assert_eq!(proven(&o).var_values, vec![row(vec![])]);
    let o = solve(
        &p,
        &query(
            Env::default(),
            vec![ParamKind::Row],
            implements(SOURCE, c(PULSE_TCP), vec![open_row(vec![], Var::Infer(0))]),
        ),
    );
    assert_eq!(
        proven(&o).var_values,
        vec![row(vec![eff(CANCEL), eff(NET)])]
    );
}

#[test]
fn open_row_impl_extends_rows() {
    let p = world();
    let o = solve(
        &p,
        &query(
            Env::default(),
            vec![ParamKind::Row],
            implements(
                SOURCE,
                t1(RETRY, c(PULSE_TCP)),
                vec![open_row(vec![], Var::Infer(0))],
            ),
        ),
    );
    assert_eq!(
        proven(&o).var_values,
        vec![row(vec![eff(NET), eff(CANCEL), fail(c(IO_ERROR))])]
    );
    // A closed row lacking Fail does not match.
    let o = solve_goal(
        &p,
        implements(
            SOURCE,
            t1(RETRY, c(PULSE_TCP)),
            vec![row(vec![eff(NET), eff(CANCEL)])],
        ),
    );
    assert_eq!(failure(&o).reason, FailureReason::NoCandidates);
    // Row order in the goal is irrelevant once sorted.
    let o = solve_goal(
        &p,
        implements(
            SOURCE,
            t1(RETRY, c(PULSE_TCP)),
            vec![row(vec![fail(c(IO_ERROR)), eff(CANCEL), eff(NET)])],
        ),
    );
    assert!(o.is_proven());
}

#[test]
fn rigid_row_tail_from_environment() {
    let p = world();
    // fn h[S, r: eff] where S: Source[r]  ⊢  Retry[S]: Source[eff [Fail[IoError] | r]]
    let env = Env {
        placeholders: vec![
            PlaceholderDecl::ty(None),
            PlaceholderDecl::of(ParamKind::Row),
        ],
        assumptions: vec![implements(
            SOURCE,
            ph(0),
            vec![open_row(vec![], Var::Placeholder(1))],
        )],
        model_scopes: vec![],
    };
    let ok = query(
        env.clone(),
        vec![],
        implements(
            SOURCE,
            t1(RETRY, ph(0)),
            vec![open_row(vec![fail(c(IO_ERROR))], Var::Placeholder(1))],
        ),
    );
    assert!(solve(&p, &ok).is_proven());
    let bad = query(
        env,
        vec![],
        implements(
            SOURCE,
            t1(RETRY, ph(0)),
            vec![open_row(vec![], Var::Placeholder(1))],
        ),
    );
    assert!(matches!(solve(&p, &bad), Outcome::NoSolution(_)));
}

#[test]
fn merging_duplicate_effect_labels_fails_closed() {
    let p = world();
    // Retry[Retry[_]] would need Fail[IoError] twice in one row.
    let q = query(
        Env::default(),
        vec![ParamKind::Row],
        implements(
            SOURCE,
            t1(RETRY, t1(RETRY, c(PULSE_TCP))),
            vec![open_row(vec![], Var::Infer(0))],
        ),
    );
    let o = solve(&p, &q);
    assert!(
        matches!(
            o,
            Outcome::Unsupported(UnsupportedReason::DuplicateEffectLabel(_))
        ),
        "{o:?}"
    );
    assert!(!o.is_proven());
}

// ---------------------------------------------------------------------------
// Modal arguments and bounds

#[test]
fn modal_trait_argument_is_inferred() {
    let p = world();
    let o = solve(
        &p,
        &query(
            Env::default(),
            vec![ParamKind::Mode],
            implements(
                CONTAINER,
                t1(VEC, c(WRITE_TXN)),
                vec![GenericArg::Mode(ModeTerm::Var(Var::Infer(0)))],
            ),
        ),
    );
    assert_eq!(proven(&o).var_values, vec![mode(Mode::Linear)]);
    // Bits has two modal conformances: the argument is ambiguous...
    let o = solve(
        &p,
        &query(
            Env::default(),
            vec![ParamKind::Mode],
            implements(
                CONTAINER,
                c(BITS),
                vec![GenericArg::Mode(ModeTerm::Var(Var::Infer(0)))],
            ),
        ),
    );
    assert_eq!(
        o,
        Outcome::Ambiguous(Ambiguity::MultipleCandidates(vec![
            CandidateSource::Impl(ImplId(12)),
            CandidateSource::Impl(ImplId(13))
        ]))
    );
    // ...unless given.
    assert!(solve_goal(&p, implements(CONTAINER, c(BITS), vec![mode(Mode::Affine)])).is_proven());
    assert!(matches!(
        solve_goal(&p, implements(CONTAINER, c(BITS), vec![mode(Mode::Linear)])),
        Outcome::NoSolution(_)
    ));
}

#[test]
fn mode_bounds_follow_the_lattice() {
    let p = world();
    assert!(solve_goal(&p, mode_bound(t1(OPTION, c(U64)), Mode::Copy)).is_proven());
    assert!(solve_goal(&p, mode_bound(t1(VEC, c(U64)), Mode::Affine)).is_proven());
    let f = failure(&solve_goal(
        &p,
        mode_bound(t1(VEC, c(WRITE_TXN)), Mode::Affine),
    ))
    .clone();
    assert_eq!(
        f.reason,
        FailureReason::ModeExceeds {
            atom: ModeTerm::Const(Mode::Linear),
            bound: ModeTerm::Const(Mode::Affine)
        }
    );
    // A definite violation dominates an unknown component.
    let q = query(
        Env::default(),
        vec![ParamKind::Type],
        mode_bound(t(PAIR, vec![a(c(WRITE_TXN)), a(inf(0))]), Mode::Affine),
    );
    assert!(matches!(solve(&p, &q), Outcome::NoSolution(_)));
    let q = query(
        Env::default(),
        vec![ParamKind::Type],
        mode_bound(t(PAIR, vec![a(c(U64)), a(inf(0))]), Mode::Affine),
    );
    assert_eq!(solve(&p, &q), Outcome::Ambiguous(Ambiguity::ModeUnknown));
    let q = query(
        Env::default(),
        vec![ParamKind::Type],
        mode_bound(inf(0), Mode::Linear),
    );
    assert!(solve(&p, &q).is_proven());
    // Placeholders use their declared bound; an unannotated parameter is linear.
    let env = Env {
        placeholders: vec![
            PlaceholderDecl::ty(Some(Mode::Copy)),
            PlaceholderDecl::ty(None),
        ],
        ..Env::default()
    };
    assert!(
        solve(
            &p,
            &query(
                env.clone(),
                vec![],
                mode_bound(t1(VEC, ph(0)), Mode::Affine)
            )
        )
        .is_proven()
    );
    assert!(matches!(
        solve(
            &p,
            &query(env, vec![], mode_bound(t1(VEC, ph(1)), Mode::Affine))
        ),
        Outcome::NoSolution(_)
    ));
}

#[test]
fn modal_bound_through_a_modal_projection() {
    let p = world();
    // mode(<Vec[WriteTxn] as Container[linear]>::Elem) ⊑ affine fails after normalization.
    let pr = proj(
        CONTAINER,
        t1(VEC, c(WRITE_TXN)),
        vec![mode(Mode::Linear)],
        0,
    );
    let o = solve_goal(&p, mode_bound(proj_ty(pr.clone()), Mode::Affine));
    assert!(matches!(o, Outcome::NoSolution(_)));
    let o = solve_goal(&p, mode_bound(proj_ty(pr), Mode::Linear));
    assert_eq!(proven(&o).derivation.step, Step::Normalize);
}

// ---------------------------------------------------------------------------
// Associated-type equalities

#[test]
fn projection_equality_by_impl_value() {
    let p = world();
    let pr = proj(SOURCE, t1(TILED_SEQ, c(U64)), vec![row(vec![])], 0);
    assert!(
        solve_goal(
            &p,
            Goal::ProjectionEq {
                projection: pr.clone(),
                ty: c(U64)
            }
        )
        .is_proven()
    );
    let f = failure(&solve_goal(
        &p,
        Goal::ProjectionEq {
            projection: pr,
            ty: c(WRITE_TXN),
        },
    ))
    .clone();
    assert_eq!(
        f.reason,
        FailureReason::CandidatesFailed(vec![CandidateFailure {
            candidate: CandidateSource::Impl(ImplId(7)),
            cause: CandidateCause::ValueMismatch
        }])
    );
}

#[test]
fn projection_through_adapter_value_and_row_inference() {
    let p = world();
    // <Fused[PulseTcp] as Source[?r]>::Item == ?t
    let pr = Projection {
        trait_ref: tr(
            SOURCE,
            t1(FUSED, c(PULSE_TCP)),
            vec![open_row(vec![], Var::Infer(0))],
        ),
        brand: Brand::Canonical,
        assoc: AssocIdx(0),
    };
    let q = query(
        Env::default(),
        vec![ParamKind::Row, ParamKind::Type],
        Goal::ProjectionEq {
            projection: pr,
            ty: inf(1),
        },
    );
    let o = solve(&p, &q);
    let s = proven(&o);
    assert_eq!(
        s.var_values,
        vec![row(vec![eff(NET), eff(CANCEL)]), a(c(RECORD_BATCH))]
    );
    match &s.derivation.step {
        Step::ProjectionConformance {
            source,
            value_normalizations,
            ..
        } => {
            assert_eq!(*source, ConformanceSource::Impl(ImplId(9)));
            assert_eq!(*value_normalizations, 1);
        }
        other => panic!("{other:?}"),
    }
}

fn fold_source_env() -> Env {
    // fn fold_source[S, T, r1: eff] where S: Source[r1, Item = T], T: Clone
    let src = |s| tr(SOURCE, ph(s), vec![open_row(vec![], Var::Placeholder(2))]);
    Env {
        placeholders: vec![
            PlaceholderDecl::ty(None),
            PlaceholderDecl::ty(Some(Mode::Copy)),
            PlaceholderDecl::of(ParamKind::Row),
        ],
        assumptions: vec![
            Goal::Implements {
                trait_ref: src(0),
                brand: Brand::Canonical,
            },
            Goal::ProjectionEq {
                projection: Projection {
                    trait_ref: src(0),
                    brand: Brand::Canonical,
                    assoc: AssocIdx(0),
                },
                ty: ph(1),
            },
            implements(CLONE, ph(1), vec![]),
        ],
        model_scopes: vec![],
    }
}

#[test]
fn associated_type_equality_assumption_normalizes() {
    let p = world();
    let item = proj(
        SOURCE,
        ph(0),
        vec![open_row(vec![], Var::Placeholder(2))],
        0,
    );
    let q = query(
        fold_source_env(),
        vec![],
        implements(CLONE, t1(VEC, proj_ty(item.clone())), vec![]),
    );
    let o = solve(&p, &q);
    let d = &proven(&o).derivation;
    assert_eq!(d.step, Step::Normalize);
    assert!(d.render().contains("[by assumption 1]"));
    // Mode of the bound item comes from T's declared bound.
    assert!(
        solve(
            &p,
            &query(
                fold_source_env(),
                vec![],
                mode_bound(proj_ty(item), Mode::Copy)
            )
        )
        .is_proven()
    );
}

#[test]
fn projection_without_binding_is_rigid() {
    let p = world();
    let env = Env {
        placeholders: vec![
            PlaceholderDecl::ty(None),
            PlaceholderDecl::of(ParamKind::Row),
        ],
        assumptions: vec![implements(
            SOURCE,
            ph(0),
            vec![open_row(vec![], Var::Placeholder(1))],
        )],
        model_scopes: vec![],
    };
    let item = proj(
        SOURCE,
        ph(0),
        vec![open_row(vec![], Var::Placeholder(1))],
        0,
    );
    let q = query(
        env.clone(),
        vec![ParamKind::Type],
        Goal::ProjectionEq {
            projection: item.clone(),
            ty: inf(0),
        },
    );
    let o = solve(&p, &q);
    assert_eq!(proven(&o).var_values, vec![a(proj_ty(item.clone()))]);
    assert_eq!(proven(&o).derivation.step, Step::RigidProjection);
    // A rigid item is not known to be Clone, and its mode is only bounded by `mod linear`.
    assert!(matches!(
        solve(
            &p,
            &query(
                env.clone(),
                vec![],
                implements(CLONE, proj_ty(item.clone()), vec![])
            )
        ),
        Outcome::NoSolution(_)
    ));
    assert!(matches!(
        solve(
            &p,
            &query(env, vec![], mode_bound(proj_ty(item), Mode::Affine))
        ),
        Outcome::NoSolution(_)
    ));
}

#[test]
fn alias_bound_proves_trait_of_rigid_projection() {
    let p = world();
    let env = Env {
        placeholders: vec![PlaceholderDecl::ty(None)],
        assumptions: vec![implements(COLLECT, ph(0), vec![])],
        model_scopes: vec![],
    };
    let out = proj(COLLECT, ph(0), vec![], 0);
    let o = solve(
        &p,
        &query(env, vec![], implements(CLONE, proj_ty(out), vec![])),
    );
    assert!(
        proven(&o)
            .derivation
            .render()
            .contains("[by alias bound 0]")
    );
    // For a concrete collector, the projection normalizes instead.
    let o = solve_goal(
        &p,
        implements(CLONE, proj_ty(proj(COLLECT, c(BITS), vec![], 0)), vec![]),
    );
    assert!(
        proven(&o)
            .derivation
            .render()
            .contains("[by value of impl#19]")
    );
}

// ---------------------------------------------------------------------------
// Named models and brands

#[test]
fn canonical_and_named_models_are_distinct_proofs() {
    let p = world();
    let canon = solve_goal(&p, via(ORD, c(UUID), vec![], Brand::Canonical));
    let named = solve_goal(
        &p,
        via(ORD, c(UUID), vec![], Brand::Named(REVERSE_UUID_ORD)),
    );
    assert!(matches!(
        proven(&canon).derivation.step,
        Step::Conformance {
            source: ConformanceSource::Impl(ImplId(6)),
            ..
        }
    ));
    assert!(matches!(
        proven(&named).derivation.step,
        Step::Conformance {
            source: ConformanceSource::Model(REVERSE_UUID_ORD),
            ..
        }
    ));
}

#[test]
fn named_models_never_satisfy_canonical_search() {
    let p = world();
    let f = failure(&solve_goal(&p, implements(WIRE_CODEC, c(UUID), vec![]))).clone();
    assert_eq!(f.reason, FailureReason::NoCandidates);
}

#[test]
fn implicit_brand_uses_lexical_model_scopes() {
    let p = world();
    let goal = via(WIRE_CODEC, c(UUID), vec![], Brand::Var(Var::Infer(0)));
    let scoped = |scopes: Vec<Vec<ModelId>>| {
        query(
            Env {
                model_scopes: scopes,
                ..Env::default()
            },
            vec![ParamKind::Brand],
            goal.clone(),
        )
    };
    // No `use model`: falls back to canonical, which does not exist.
    assert!(matches!(solve(&p, &scoped(vec![])), Outcome::NoSolution(_)));
    // `use model BigEndianUuidCodec;`
    let o = solve(&p, &scoped(vec![vec![BIG_ENDIAN_UUID]]));
    assert_eq!(
        proven(&o).var_values,
        vec![GenericArg::Brand(Brand::Named(BIG_ENDIAN_UUID))]
    );
    // Inner scope shadows outer.
    let o = solve(
        &p,
        &scoped(vec![vec![BIG_ENDIAN_UUID], vec![LITTLE_ENDIAN_UUID]]),
    );
    assert_eq!(
        proven(&o).var_values,
        vec![GenericArg::Brand(Brand::Named(LITTLE_ENDIAN_UUID))]
    );
    // Two models in one scope conflict.
    let o = solve(&p, &scoped(vec![vec![LITTLE_ENDIAN_UUID, BIG_ENDIAN_UUID]]));
    assert_eq!(
        o,
        Outcome::Ambiguous(Ambiguity::ConflictingScopedModels(vec![
            BIG_ENDIAN_UUID,
            LITTLE_ENDIAN_UUID
        ]))
    );
    // Models of other traits in scope are irrelevant: implicit Ord stays canonical.
    let o = solve(
        &p,
        &query(
            Env {
                model_scopes: vec![vec![BIG_ENDIAN_UUID]],
                ..Env::default()
            },
            vec![ParamKind::Brand],
            via(ORD, c(UUID), vec![], Brand::Var(Var::Infer(0))),
        ),
    );
    assert_eq!(
        proven(&o).var_values,
        vec![GenericArg::Brand(Brand::Canonical)]
    );
}

#[test]
fn named_model_of_another_trait_is_rejected() {
    let p = world();
    let f = failure(&solve_goal(
        &p,
        via(ORD, c(UUID), vec![], Brand::Named(BIG_ENDIAN_UUID)),
    ))
    .clone();
    assert_eq!(
        f.reason,
        FailureReason::ModelTraitMismatch {
            model: BIG_ENDIAN_UUID
        }
    );
}

fn btree(k: Ty, v: Ty, brand: Brand) -> Ty {
    t(CACHE_BTREE, vec![a(k), a(v), GenericArg::Brand(brand)])
}

#[test]
fn model_brands_separate_collection_types() {
    let p = world();
    let canonical = btree(c(UUID), c(U64), Brand::Canonical);
    let reverse = btree(c(UUID), c(U64), Brand::Named(REVERSE_UUID_ORD));
    // Same brand merges, checking Ord through the stamped model.
    let o = solve_goal(
        &p,
        implements(MERGE, reverse.clone(), vec![a(reverse.clone())]),
    );
    assert!(proven(&o).derivation.render().contains("[by model#0]"));
    assert!(
        solve_goal(
            &p,
            implements(MERGE, canonical.clone(), vec![a(canonical.clone())])
        )
        .is_proven()
    );
    // Mixing brands is rejected even though layouts are identical.
    let f = failure(&solve_goal(
        &p,
        implements(MERGE, canonical, vec![a(reverse)]),
    ))
    .clone();
    assert_eq!(f.reason, FailureReason::NoCandidates);
    // A brand naming a model of another trait fails at the nested Ord goal.
    let wrong = btree(c(UUID), c(U64), Brand::Named(BIG_ENDIAN_UUID));
    assert!(matches!(
        solve_goal(&p, implements(MERGE, wrong.clone(), vec![a(wrong)])),
        Outcome::NoSolution(_)
    ));
}

#[test]
fn brand_inference_and_placeholder_brands() {
    let p = world();
    let reverse = btree(c(UUID), c(U64), Brand::Named(REVERSE_UUID_ORD));
    let q = query(
        Env::default(),
        vec![ParamKind::Brand],
        implements(
            MERGE,
            reverse,
            vec![a(btree(c(UUID), c(U64), Brand::Var(Var::Infer(0))))],
        ),
    );
    assert_eq!(
        proven(&solve(&p, &q)).var_values,
        vec![GenericArg::Brand(Brand::Named(REVERSE_UUID_ORD))]
    );
    // fn m[K, V, M: brand] where K: Ord via M
    let env = Env {
        placeholders: vec![
            PlaceholderDecl::ty(None),
            PlaceholderDecl::ty(None),
            PlaceholderDecl::of(ParamKind::Brand),
        ],
        assumptions: vec![via(ORD, ph(0), vec![], Brand::Var(Var::Placeholder(2)))],
        model_scopes: vec![],
    };
    let tree = btree(ph(0), ph(1), Brand::Var(Var::Placeholder(2)));
    let o = solve(
        &p,
        &query(
            env.clone(),
            vec![],
            implements(MERGE, tree.clone(), vec![a(tree)]),
        ),
    );
    assert!(proven(&o).derivation.render().contains("[by assumption 0]"));
    // The branded assumption does not prove canonical Ord for K.
    assert!(matches!(
        solve(&p, &query(env, vec![], implements(ORD, ph(0), vec![]))),
        Outcome::NoSolution(_)
    ));
}

// ---------------------------------------------------------------------------
// Independent derivation checking

#[test]
fn tampered_derivations_are_rejected() {
    let p = world();
    let o = solve_goal(&p, implements(CLONE, t1(VEC, c(U64)), vec![]));
    let d = proven(&o).derivation.clone();
    let env = Env::default();
    check_derivation(&p, &env, &d).unwrap();

    let mut wrong_impl = d.clone();
    wrong_impl.step = Step::Conformance {
        source: ConformanceSource::Impl(ImplId(3)),
        args: vec![a(c(U64))],
    };
    assert!(check_derivation(&p, &env, &wrong_impl).is_err());

    let mut missing_child = d.clone();
    missing_child.children.clear();
    assert!(check_derivation(&p, &env, &missing_child).is_err());

    let mut model_brand = d.clone();
    if let Goal::Implements { brand, .. } = &mut model_brand.goal {
        *brand = Brand::Named(REVERSE_UUID_ORD);
    }
    assert!(check_derivation(&p, &env, &model_brand).is_err());

    // A coinductive hypothesis may not close an inductive (Clone) cycle.
    let mut fake = d.clone();
    fake.children[0] = Derivation::leaf(d.goal.clone(), Step::CoinductiveHypothesis);
    fake.children[0].goal = d.children[0].goal.clone();
    assert!(check_derivation(&p, &env, &fake).is_err());
    let mut self_loop = d;
    self_loop.children[0] = Derivation::leaf(self_loop.goal.clone(), Step::CoinductiveHypothesis);
    assert!(check_derivation(&p, &env, &self_loop).is_err());
}

#[test]
fn every_world_proof_rechecks_with_a_shared_table() {
    let p = world();
    let mut s = Solver::new(&p);
    let goals = vec![
        implements(CLONE, t1(VEC, t1(LIST, c(UUID))), vec![]),
        implements(SEND, t1(LIST, t1(LIST, t1(LIST, c(UUID)))), vec![]),
        implements(COLLECT, c(BITS), vec![]),
        implements(SOURCE, t1(FUSED, t1(TILED_SEQ, c(U64))), vec![row(vec![])]),
        mode_bound(
            proj_ty(proj(
                SOURCE,
                t1(FUSED, c(PULSE_TCP)),
                vec![row(vec![eff(NET), eff(CANCEL)])],
                0,
            )),
            Mode::Affine,
        ),
    ];
    for g in goals {
        let o = solve_with(&mut s, &p, &query(Env::default(), vec![], g.clone()));
        assert!(o.is_proven(), "{g}: {o:?}");
    }
}

#[test]
fn named_model_bounds_of_generic_code_are_assumptions() {
    let p = world();
    // fn f[T] where T: Ord via ReverseUuidOrd
    let env = Env {
        placeholders: vec![PlaceholderDecl::ty(None)],
        assumptions: vec![via(ORD, ph(0), vec![], Brand::Named(REVERSE_UUID_ORD))],
        model_scopes: vec![],
    };
    let o = solve(
        &p,
        &query(
            env.clone(),
            vec![],
            via(ORD, ph(0), vec![], Brand::Named(REVERSE_UUID_ORD)),
        ),
    );
    assert_eq!(proven(&o).derivation.step, Step::Assumption { index: 0 });
    // It proves neither the canonical model nor another named one.
    assert!(matches!(
        solve(
            &p,
            &query(env.clone(), vec![], implements(ORD, ph(0), vec![]))
        ),
        Outcome::NoSolution(_)
    ));
    let tree = btree(ph(0), c(U64), Brand::Named(REVERSE_UUID_ORD));
    assert!(
        solve(
            &p,
            &query(env, vec![], implements(MERGE, tree.clone(), vec![a(tree)]))
        )
        .is_proven()
    );
}
