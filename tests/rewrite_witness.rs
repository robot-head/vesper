//! Independent malformed-certificate tests over actual parsed/checked islands.
use vesper::{
    ast::{Expr, ExprKind, Item, Literal, Pass, Type, TypeMap},
    check,
    numeric::IntegerKind,
    optimize::{
        self,
        witness::{
            Equality, IntOp, Reason, RewriteCertificate, Substitution, Term, TermNode,
            WitnessError, WitnessLimits, verify_rewrite,
        },
    },
    parser,
};

fn checked_island(text: &str) -> (Expr, TypeMap) {
    let parsed = parser::parse(text);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let Item::Function(function) = &parsed.module.items[0] else {
        panic!("function fixture")
    };
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &function.body.as_ref().unwrap().kind
    else {
        panic!("tail fixture")
    };
    ((**tail).clone(), checked.expression_types)
}
fn canonical_candidate(source: &Expr) -> Expr {
    fn values(expression: &mut Expr, span: &std::ops::Range<usize>) {
        expression.span = span.clone();
        match &mut expression.kind {
            ExprKind::Binary(_, left, right) => {
                values(left, span);
                values(right, span);
            }
            ExprKind::Call { args, .. } => {
                for arg in args {
                    values(&mut arg.expr, span);
                }
            }
            _ => {}
        }
    }
    let mut candidate = source.clone();
    values(&mut candidate, &source.span);
    candidate
}
fn kind_type(kind: IntegerKind) -> &'static str {
    match kind {
        IntegerKind::U8 => "u8",
        IntegerKind::U32 => "u32",
        IntegerKind::U64 => "u64",
        IntegerKind::I64 => "i64",
    }
}
fn term(kind: IntegerKind, node: TermNode) -> Term {
    Term { kind, node }
}
fn sub(bindings: &[u32]) -> Substitution {
    let mut value = Substitution {
        bindings: [0; 3],
        len: bindings.len() as u8,
    };
    value.bindings[..bindings.len()].copy_from_slice(bindings);
    value
}
fn certificate(
    terms: Vec<Term>,
    proofs: Vec<Equality>,
    input: u32,
    output: u32,
) -> RewriteCertificate {
    RewriteCertificate {
        schema_version: 1,
        arithmetic_version: 1,
        terms,
        conclusion: proofs.len() as u32 - 1,
        proofs,
        input,
        output,
    }
}
fn add_zero(kind: IntegerKind) -> (Expr, Expr, TypeMap, RewriteCertificate) {
    let ty = kind_type(kind);
    let (source, types) = checked_island(&format!(
        "fn f(x:{ty})->{ty} eff [Fail[ArithOverflow]] {{x+0}}"
    ));
    let candidate = Expr {
        kind: ExprKind::Name("x".into()),
        span: source.span.clone(),
    };
    let terms = vec![
        term(kind, TermNode::InputVar(0)),
        term(kind, TermNode::Const(0)),
        term(
            kind,
            TermNode::Checked {
                op: IntOp::Add,
                children: [0, 1],
            },
        ),
    ];
    let proof = Equality {
        from: 2,
        to: 0,
        reason: Reason::Rule {
            rule: 1,
            substitution: sub(&[0]),
            guard: None,
        },
    };
    (
        source,
        candidate,
        types,
        certificate(terms, vec![proof], 2, 0),
    )
}
fn verify(
    source: &Expr,
    candidate: &Expr,
    types: &TypeMap,
    cert: &RewriteCertificate,
) -> Result<(), WitnessError> {
    verify_rewrite(source, candidate, types, cert, WitnessLimits::default())
}

#[test]
fn real_checked_contextual_literal_metadata_never_supplies_the_constant_value() {
    for kind in [
        IntegerKind::U8,
        IntegerKind::U32,
        IntegerKind::U64,
        IntegerKind::I64,
    ] {
        let (source, candidate, mut types, cert) = add_zero(kind);
        assert_eq!(verify(&source, &candidate, &types, &cert), Ok(()));
        let ExprKind::Binary(_, _, literal) = &source.kind else {
            unreachable!()
        };
        for metadata in [0, 1, i128::from(u64::MAX)] {
            types.insert(
                (literal.span.start, literal.span.end),
                Type::IntLiteral(metadata),
            );
            assert_eq!(verify(&source, &candidate, &types, &cert), Ok(()));
        }
        let mut forged = cert.clone();
        forged.terms[1].node = TermNode::Const(1);
        assert!(verify(&source, &candidate, &types, &forged).is_err());
    }
}

#[test]
fn missing_infer_noninteger_and_closed_mixed_types_refuse_every_source_value_position() {
    let (source, candidate, types, cert) = add_zero(IntegerKind::I64);
    let ExprKind::Binary(_, name, literal) = &source.kind else {
        unreachable!()
    };
    for expr in [&source, name.as_ref(), literal.as_ref()] {
        let key = (expr.span.start, expr.span.end);
        let mut changed = types.clone();
        changed.remove(&key);
        assert_eq!(
            verify(&source, &candidate, &changed, &cert),
            Err(WitnessError::MissingType)
        );
        for invalid in [
            Type::Infer,
            Type::named("u8"),
            Type::named("bool"),
            Type::Named("i64".into(), vec![Type::named("u8")]),
        ] {
            let mut changed = types.clone();
            changed.insert(key, invalid);
            assert!(verify(&source, &candidate, &changed, &cert).is_err());
        }
    }
}

#[test]
fn literal_roots_require_closed_width_and_full_u64_constants_are_checked_without_truncation() {
    for (kind, n) in [
        (IntegerKind::U8, 255),
        (IntegerKind::U32, i128::from(u32::MAX)),
        (IntegerKind::U64, i128::from(u64::MAX)),
        (IntegerKind::I64, i128::from(i64::MIN)),
    ] {
        let (source, types) = checked_island(&format!("fn f()->{}{{{n}}}", kind_type(kind)));
        let cert = certificate(
            vec![term(kind, TermNode::Const(n))],
            vec![Equality {
                from: 0,
                to: 0,
                reason: Reason::Refl,
            }],
            0,
            0,
        );
        assert_eq!(verify(&source, &source, &types, &cert), Ok(()));
        let mut unresolved = types.clone();
        unresolved.insert((source.span.start, source.span.end), Type::IntLiteral(n));
        assert!(verify(&source, &source, &unresolved, &cert).is_err());
        let mut outside = cert.clone();
        outside
            .terms
            .push(term(kind, TermNode::Const(kind.max() + 1)));
        assert_eq!(
            verify(&source, &source, &types, &outside),
            Err(WitnessError::ConstantOutOfRange)
        );
    }
}

#[test]
fn unknown_rules_versions_and_substitution_corruption_cannot_become_authority() {
    let (source, candidate, types, cert) = add_zero(IntegerKind::I64);
    assert_eq!(verify(&source, &candidate, &types, &cert), Ok(()));
    for id in [0, 25, u16::MAX] {
        let mut changed = cert.clone();
        let Reason::Rule { rule, .. } = &mut changed.proofs[0].reason else {
            unreachable!()
        };
        *rule = id;
        assert_eq!(
            verify(&source, &candidate, &types, &changed),
            Err(WitnessError::UnknownRule)
        );
    }
    for schema in [true, false] {
        let mut changed = cert.clone();
        if schema {
            changed.schema_version = 2;
        } else {
            changed.arithmetic_version = 0;
        }
        assert_eq!(
            verify(&source, &candidate, &types, &changed),
            Err(WitnessError::UnsupportedVersion)
        );
    }
    for substitution in [
        sub(&[]),
        sub(&[0, 1]),
        Substitution {
            bindings: [0, 1, 0],
            len: 1,
        },
        Substitution {
            bindings: [u32::MAX, 0, 0],
            len: 1,
        },
    ] {
        let mut changed = cert.clone();
        let Reason::Rule {
            substitution: value,
            ..
        } = &mut changed.proofs[0].reason
        else {
            unreachable!()
        };
        *value = substitution;
        assert!(verify(&source, &candidate, &types, &changed).is_err());
    }
}

#[test]
fn all_unused_terms_and_proofs_still_require_valid_kinds_variables_and_topology() {
    let (source, candidate, types, cert) = add_zero(IntegerKind::I64);
    for node in [
        TermNode::InputVar(1),
        TermNode::Checked {
            op: IntOp::Add,
            children: [3, 0],
        },
        TermNode::Wrapping {
            op: IntOp::Mul,
            children: [u32::MAX, 0],
        },
    ] {
        let mut changed = cert.clone();
        changed.terms.push(term(IntegerKind::I64, node));
        assert!(verify(&source, &candidate, &types, &changed).is_err());
    }
    let mut changed = cert.clone();
    changed
        .terms
        .push(term(IntegerKind::U8, TermNode::Const(0)));
    assert_eq!(
        verify(&source, &candidate, &types, &changed),
        Err(WitnessError::TypeMismatch)
    );
    for reason in [
        Reason::Symmetry { proof: 1 },
        Reason::Symmetry { proof: u32::MAX },
        Reason::Transitivity {
            first: 0,
            second: 1,
        },
    ] {
        let mut changed = cert.clone();
        changed.proofs.push(Equality {
            from: 0,
            to: 2,
            reason,
        });
        assert!(verify(&source, &candidate, &types, &changed).is_err());
    }
    let mut changed = cert.clone();
    changed.input = 0;
    assert!(verify(&source, &candidate, &types, &changed).is_err());
    let mut changed = cert.clone();
    changed.output = 2;
    assert!(verify(&source, &candidate, &types, &changed).is_err());
    let mut changed = cert.clone();
    changed.conclusion = u32::MAX;
    assert!(verify(&source, &candidate, &types, &changed).is_err());
}

#[test]
fn shallow_reflexivity_and_transitivity_cannot_connect_different_endpoints() {
    let (source, candidate, types, cert) = add_zero(IntegerKind::I64);
    let mut changed = cert.clone();
    changed.proofs[0].reason = Reason::Refl;
    assert_eq!(
        verify(&source, &candidate, &types, &changed),
        Err(WitnessError::InvalidEquality)
    );
    let mut changed = cert.clone();
    changed.proofs.push(Equality {
        from: 1,
        to: 1,
        reason: Reason::Refl,
    });
    changed.proofs.push(Equality {
        from: 2,
        to: 1,
        reason: Reason::Transitivity {
            first: 0,
            second: 1,
        },
    });
    changed.conclusion = 2;
    changed.output = 1;
    let zero = Expr {
        kind: ExprKind::Literal(Literal::Int(0)),
        span: source.span.clone(),
    };
    assert_eq!(
        verify(&source, &zero, &types, &changed),
        Err(WitnessError::InvalidEquality)
    );
}

#[test]
fn guarded_erasure_cannot_bootstrap_totality_or_borrow_an_unrelated_leaf_fact() {
    let kind = IntegerKind::I64;
    let (source, types) = checked_island("fn f(x:i64)->i64 eff [Fail[ArithOverflow]]{(x+1)*0}");
    let candidate = Expr {
        kind: ExprKind::Literal(Literal::Int(0)),
        span: source.span.clone(),
    };
    let terms = vec![
        term(kind, TermNode::InputVar(0)),
        term(kind, TermNode::Const(1)),
        term(
            kind,
            TermNode::Checked {
                op: IntOp::Add,
                children: [0, 1],
            },
        ),
        term(kind, TermNode::Const(0)),
        term(
            kind,
            TermNode::Checked {
                op: IntOp::Mul,
                children: [2, 3],
            },
        ),
    ];
    for guard in [None, Some(0), Some(1), Some(u32::MAX)] {
        let cert = certificate(
            terms.clone(),
            vec![Equality {
                from: 4,
                to: 3,
                reason: Reason::Rule {
                    rule: 6,
                    substitution: sub(&[2]),
                    guard,
                },
            }],
            4,
            3,
        );
        assert!(verify(&source, &candidate, &types, &cert).is_err());
    }
    let unrelated = Equality {
        from: 0,
        to: 0,
        reason: Reason::Refl,
    };
    let rule = Equality {
        from: 4,
        to: 3,
        reason: Reason::Rule {
            rule: 6,
            substitution: sub(&[2]),
            guard: Some(0),
        },
    };
    let cert = certificate(terms, vec![unrelated, rule], 4, 3);
    assert_eq!(
        verify(&source, &candidate, &types, &cert),
        Err(WitnessError::UnprovenTotality)
    );
}

#[test]
fn earlier_exact_equality_to_a_leaf_proves_totality_in_either_orientation() {
    let kind = IntegerKind::I64;
    let (source, types) = checked_island("fn f(x:i64)->i64 eff [Fail[ArithOverflow]]{(x+0)*0}");
    let candidate = Expr {
        kind: ExprKind::Literal(Literal::Int(0)),
        span: source.span.clone(),
    };
    let terms = vec![
        term(kind, TermNode::InputVar(0)),
        term(kind, TermNode::Const(0)),
        term(
            kind,
            TermNode::Checked {
                op: IntOp::Add,
                children: [0, 1],
            },
        ),
        term(
            kind,
            TermNode::Checked {
                op: IntOp::Mul,
                children: [2, 1],
            },
        ),
    ];
    for reverse in [false, true] {
        let mut proofs = vec![Equality {
            from: 2,
            to: 0,
            reason: Reason::Rule {
                rule: 1,
                substitution: sub(&[0]),
                guard: None,
            },
        }];
        if reverse {
            proofs.push(Equality {
                from: 0,
                to: 2,
                reason: Reason::Symmetry { proof: 0 },
            });
        }
        let guard = proofs.len() as u32 - 1;
        proofs.push(Equality {
            from: 3,
            to: 1,
            reason: Reason::Rule {
                rule: 6,
                substitution: sub(&[2]),
                guard: Some(guard),
            },
        });
        assert_eq!(
            verify(
                &source,
                &candidate,
                &types,
                &certificate(terms.clone(), proofs, 3, 1)
            ),
            Ok(())
        );
    }
    let (_, candidate, types, mut cert) = add_zero(kind);
    cert.proofs.insert(
        0,
        Equality {
            from: 0,
            to: 0,
            reason: Reason::Refl,
        },
    );
    cert.conclusion = 1;
    if let Reason::Rule { guard, .. } = &mut cert.proofs[1].reason {
        *guard = Some(0);
    }
    let (source, _) = checked_island("fn f(x:i64)->i64 eff [Fail[ArithOverflow]]{x+0}");
    assert!(
        verify(&source, &candidate, &types, &cert).is_err(),
        "unguarded identity cannot carry an irrelevant guard"
    );
}

#[test]
fn checked_overflow_folds_never_become_wrapped_values_in_any_integer_kind() {
    for (kind, op, a, b, wrapped, rule) in [
        (IntegerKind::U8, IntOp::Add, 255, 1, 0, 19),
        (IntegerKind::U32, IntOp::Sub, 0, 1, i128::from(u32::MAX), 20),
        (
            IntegerKind::U64,
            IntOp::Mul,
            i128::from(u64::MAX),
            i128::from(u64::MAX),
            1,
            21,
        ),
        (
            IntegerKind::I64,
            IntOp::Add,
            i128::from(i64::MAX),
            1,
            i128::from(i64::MIN),
            19,
        ),
        (
            IntegerKind::I64,
            IntOp::Mul,
            i128::from(i64::MIN),
            -1,
            i128::from(i64::MIN),
            21,
        ),
    ] {
        let operator = match op {
            IntOp::Add => "+",
            IntOp::Sub => "-",
            IntOp::Mul => "*",
        };
        let (source, types) = checked_island(&format!(
            "fn f()->{} eff [Fail[ArithOverflow]]{{{a}{operator}{b}}}",
            kind_type(kind)
        ));
        let candidate = Expr {
            kind: ExprKind::Literal(Literal::Int(wrapped)),
            span: source.span.clone(),
        };
        let terms = vec![
            term(kind, TermNode::Const(a)),
            term(kind, TermNode::Const(b)),
            term(
                kind,
                TermNode::Checked {
                    op,
                    children: [0, 1],
                },
            ),
            term(kind, TermNode::Const(wrapped)),
        ];
        let cert = certificate(
            terms,
            vec![Equality {
                from: 2,
                to: 3,
                reason: Reason::Rule {
                    rule,
                    substitution: sub(&[0, 1]),
                    guard: None,
                },
            }],
            2,
            3,
        );
        assert!(verify(&source, &candidate, &types, &cert).is_err());
    }
}

fn wrapping_identity() -> (Expr, TypeMap, RewriteCertificate) {
    let (source, types) = checked_island("fn f(x:i64,y:i64)->i64{wrapping_add(x,y)}");
    let kind = IntegerKind::I64;
    let terms = vec![
        term(kind, TermNode::InputVar(0)),
        term(kind, TermNode::InputVar(1)),
        term(
            kind,
            TermNode::Wrapping {
                op: IntOp::Add,
                children: [0, 1],
            },
        ),
    ];
    let cert = certificate(
        terms,
        vec![Equality {
            from: 2,
            to: 2,
            reason: Reason::Refl,
        }],
        2,
        2,
    );
    (source, types, cert)
}

#[test]
fn candidate_variables_endpoints_and_callee_origin_cannot_be_swapped() {
    let (source, types, cert) = wrapping_identity();
    let canonical = canonical_candidate(&source);
    assert_eq!(verify(&source, &canonical, &types, &cert), Ok(()));
    assert_eq!(
        verify(&source, &source, &types, &cert),
        Err(WitnessError::OutputMismatch),
        "raw source child spans do not satisfy the candidate profile"
    );
    for mutation in 0..8 {
        let mut candidate = canonical.clone();
        let root_start = candidate.span.start;
        let ExprKind::Call {
            callee,
            args,
            types: generics,
        } = &mut candidate.kind
        else {
            unreachable!()
        };
        match mutation {
            0 => callee.span = root_start..root_start,
            1 => callee.kind = ExprKind::Name("wrapping_mul".into()),
            2 => args[0].expr.kind = ExprKind::Name("missing".into()),
            3 => {
                args[0].expr.kind = ExprKind::Name("y".into());
                args[1].expr.kind = ExprKind::Name("x".into());
            }
            4 => args[0].pass = Pass::Inout,
            5 => args[0].explicit = true,
            6 => generics.push(Type::named("i64")),
            _ => candidate.span.end += 1,
        }
        assert!(
            verify(&source, &candidate, &types, &cert).is_err(),
            "mutation{mutation}"
        );
    }
    let mut candidate = canonical.clone();
    let ExprKind::Call { args, .. } = &mut candidate.kind else {
        unreachable!()
    };
    args[0].expr.span = source.span.clone();
    args[1].expr.span = source.span.clone();
    assert_eq!(
        verify(&source, &candidate, &types, &cert),
        Ok(()),
        "every candidate value uses the checked root span"
    );
}

#[test]
fn typed_local_shadow_calls_and_effectful_shapes_cannot_be_intrinsic_islands() {
    let (source, types) =
        checked_island("fn f(x:i64,y:i64)->i64{let wrapping_add=|a:i64,b:i64|a;wrapping_add(x,y)}");
    let (_, _, cert) = wrapping_identity();
    assert!(verify(&source, &source, &types, &cert).is_err());
    let (source, types) =
        checked_island("fn f(x:i64,y:i64)->i64 eff [Fail[RuntimeError],Fail[ArithOverflow]]{x/y}");
    assert!(verify(&source, &source, &types, &cert).is_err());
}

#[test]
fn congruence_must_preserve_corresponding_child_order_even_for_commutative_wrapping() {
    let (source, types, mut cert) = wrapping_identity();
    cert.terms.push(term(
        IntegerKind::I64,
        TermNode::Wrapping {
            op: IntOp::Add,
            children: [1, 0],
        },
    ));
    cert.proofs = vec![
        Equality {
            from: 0,
            to: 0,
            reason: Reason::Refl,
        },
        Equality {
            from: 1,
            to: 1,
            reason: Reason::Refl,
        },
        Equality {
            from: 2,
            to: 3,
            reason: Reason::Congruence { children: [0, 1] },
        },
    ];
    cert.conclusion = 2;
    cert.output = 3;
    let mut candidate = canonical_candidate(&source);
    let ExprKind::Call { args, .. } = &mut candidate.kind else {
        unreachable!()
    };
    args.swap(0, 1);
    assert_eq!(
        verify(&source, &candidate, &types, &cert),
        Err(WitnessError::InvalidEquality)
    );
    cert.proofs = vec![Equality {
        from: 2,
        to: 3,
        reason: Reason::Rule {
            rule: 12,
            substitution: sub(&[0, 1]),
            guard: None,
        },
    }];
    cert.conclusion = 0;
    assert_eq!(verify(&source, &candidate, &types, &cert), Ok(()));
}

#[test]
fn invalid_configuration_and_small_resource_limits_never_weaken_endpoint_checking() {
    let (source, candidate, types, cert) = add_zero(IntegerKind::I64);
    let setters: [fn(&mut WitnessLimits, usize); 9] = [
        |l, n| l.max_terms = n,
        |l, n| l.max_proofs = n,
        |l, n| l.max_input_nodes = n,
        |l, n| l.max_output_nodes = n,
        |l, n| l.max_depth = n,
        |l, n| l.max_variables = n,
        |l, n| l.max_name_bytes = n,
        |l, n| l.max_metadata_bytes = n,
        |l, n| l.max_work = n,
    ];
    for setter in setters {
        for n in [0, usize::MAX] {
            let mut limits = WitnessLimits::default();
            setter(&mut limits, n);
            assert_eq!(
                verify_rewrite(&source, &candidate, &types, &cert, limits),
                Err(WitnessError::InvalidLimits)
            );
        }
    }
    for limits in [
        WitnessLimits {
            max_terms: 2,
            ..Default::default()
        },
        WitnessLimits {
            max_input_nodes: 2,
            ..Default::default()
        },
        WitnessLimits {
            max_depth: 1,
            ..Default::default()
        },
        WitnessLimits {
            max_metadata_bytes: 1,
            ..Default::default()
        },
        WitnessLimits {
            max_work: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            verify_rewrite(&source, &candidate, &types, &cert, limits),
            Err(WitnessError::Limit)
        );
    }
    let (source, types, cert) = wrapping_identity();
    let candidate = canonical_candidate(&source);
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &cert,
            WitnessLimits {
                max_variables: 1,
                ..Default::default()
            }
        ),
        Err(WitnessError::Limit)
    );
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &cert,
            WitnessLimits {
                max_name_bytes: 1,
                ..Default::default()
            }
        ),
        Err(WitnessError::Limit)
    );
}

#[test]
fn source_binding_counts_expanded_ast_occurrences_instead_of_unique_dag_nodes() {
    let kind = IntegerKind::I64;
    let (source, types) = checked_island("fn f(x:i64)->i64 eff [Fail[ArithOverflow]]{(x+0)-(x+0)}");
    let candidate = Expr {
        kind: ExprKind::Literal(Literal::Int(0)),
        span: source.span.clone(),
    };
    let terms = vec![
        term(kind, TermNode::InputVar(0)),
        term(kind, TermNode::Const(0)),
        term(
            kind,
            TermNode::Checked {
                op: IntOp::Add,
                children: [0, 1],
            },
        ),
        term(
            kind,
            TermNode::Checked {
                op: IntOp::Sub,
                children: [2, 2],
            },
        ),
    ];
    let proofs = vec![
        Equality {
            from: 2,
            to: 0,
            reason: Reason::Rule {
                rule: 1,
                substitution: sub(&[0]),
                guard: None,
            },
        },
        Equality {
            from: 3,
            to: 1,
            reason: Reason::Rule {
                rule: 8,
                substitution: sub(&[2]),
                guard: Some(0),
            },
        },
    ];
    let cert = certificate(terms, proofs, 3, 1);
    assert_eq!(verify(&source, &candidate, &types, &cert), Ok(()));
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &cert,
            WitnessLimits {
                max_input_nodes: 6,
                ..Default::default()
            }
        ),
        Err(WitnessError::Limit)
    );
}

#[test]
fn whole_maximal_island_survives_resource_refusal_without_child_constant_fallback() {
    fn balanced(leaves: &[&str]) -> String {
        if leaves.len() == 1 {
            return leaves[0].into();
        }
        let middle = leaves.len() / 2;
        format!(
            "({}+{})",
            balanced(&leaves[..middle]),
            balanced(&leaves[middle..])
        )
    }
    let mut leaves = vec!["x"; 300];
    leaves[0] = "(3+4)";
    let text = format!(
        "fn f(x:i64)->i64 eff [Fail[ArithOverflow]]{{{}}}",
        balanced(&leaves)
    );
    let mut parsed = parser::parse(&text);
    assert!(parsed.diagnostics.is_empty());
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let Item::Function(function) = &parsed.module.items[0] else {
        unreachable!()
    };
    let before = function.body.clone().unwrap();
    optimize::optimize(&mut parsed.module, &checked.expression_types);
    let Item::Function(function) = &parsed.module.items[0] else {
        unreachable!()
    };
    assert_eq!(function.body.as_ref().unwrap(), &before);
}
