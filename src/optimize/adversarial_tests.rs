//! Corrupt real producer proposals at the same private gate used by traversal.
use super::{
    certificate::{RewriteOutcome, RewriteProposal, propose_rewrite},
    install_proposal,
    witness::{
        Equality, IntOp, Reason, Substitution, Term, TermNode, WitnessBudget, WitnessError,
        WitnessLimits, bind_source, build_candidate,
    },
};
use crate::{
    ast::{Arg, Expr, ExprKind, Item, Literal, Type, TypeMap},
    check,
    numeric::IntegerKind,
    parser,
};

fn fixture(text: &str) -> (Expr, TypeMap) {
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
fn proposal(source: &Expr, types: &TypeMap) -> RewriteProposal {
    match propose_rewrite(source, types, WitnessLimits::default())
        .expect("coherent producer must produce this simple witness")
    {
        RewriteOutcome::Candidate(proposal) => proposal,
        RewriteOutcome::Barrier => panic!("simple integer fixture is admitted"),
        RewriteOutcome::Unchanged => panic!("simple fixture must simplify"),
    }
}
fn corrupt(mutate: impl FnOnce(&mut RewriteProposal)) {
    let (mut source, types) = fixture("fn f()->i64{2+3}");
    let before = source.clone();
    let mut proposed = proposal(&source, &types);
    mutate(&mut proposed);
    assert!(install_proposal(&mut source, &types, proposed).is_err());
    assert_eq!(
        source, before,
        "refused genuine proposal changed an original value node or span"
    );
}

#[test]
fn genuine_fold_and_egg_candidates_pass_the_actual_install_gate() {
    for text in [
        "fn f()->i64{2+3}",
        "fn f(x:i64)->i64{wrapping_add(x,0)}",
        "fn f()->u64{18446744073709551615-1}",
        "fn f()->u64{wrapping_mul(18446744073709551615,0)}",
    ] {
        let (mut source, types) = fixture(text);
        let proposed = proposal(&source, &types);
        let expected = proposed.candidate.clone();
        let root_span = source.span.clone();
        assert_eq!(install_proposal(&mut source, &types, proposed), Ok(()));
        assert_eq!(source, expected);
        assert_eq!(source.span, root_span);
    }
}

#[test]
fn unknown_reasons_versions_and_missing_conclusion_in_real_proposals_preserve_entire_island() {
    corrupt(|p| p.certificate.schema_version = 2);
    corrupt(|p| p.certificate.arithmetic_version = 0);
    corrupt(|p| p.certificate.conclusion = u32::MAX);
    corrupt(|p| p.certificate.proofs.clear());
    corrupt(|p| {
        let proof = p
            .certificate
            .proofs
            .iter_mut()
            .find(|step| matches!(step.reason, Reason::Rule { .. }))
            .expect("actual fold rule witness");
        let Reason::Rule { rule, .. } = &mut proof.reason else {
            unreachable!()
        };
        *rule = u16::MAX;
    });
}

#[test]
fn real_proposal_terms_and_proof_cycles_are_checked_even_when_unused() {
    corrupt(|p| {
        p.certificate.terms.push(Term {
            kind: IntegerKind::I64,
            node: TermNode::InputVar(0),
        })
    });
    corrupt(|p| {
        let next = p.certificate.terms.len() as u32;
        p.certificate.terms.push(Term {
            kind: IntegerKind::I64,
            node: TermNode::Checked {
                op: IntOp::Add,
                children: [next, next],
            },
        });
    });
    corrupt(|p| {
        p.certificate.terms.push(Term {
            kind: IntegerKind::I64,
            node: TermNode::Wrapping {
                op: IntOp::Sub,
                children: [u32::MAX, 0],
            },
        })
    });
    corrupt(|p| {
        let next = p.certificate.proofs.len() as u32;
        p.certificate.proofs.push(Equality {
            from: p.certificate.input,
            to: p.certificate.output,
            reason: Reason::Symmetry { proof: next },
        });
    });
    corrupt(|p| {
        p.certificate.proofs.push(Equality {
            from: p.certificate.input,
            to: p.certificate.output,
            reason: Reason::Symmetry { proof: u32::MAX },
        })
    });
}

#[test]
fn real_fold_inputs_outputs_kinds_and_substitutions_cannot_be_replaced() {
    corrupt(|p| p.candidate.kind = ExprKind::Literal(Literal::Int(6)));
    corrupt(|p| p.candidate.span.start = p.candidate.span.start.saturating_add(1));
    corrupt(|p| p.certificate.output = p.certificate.input);
    corrupt(|p| p.certificate.input = p.certificate.output);
    corrupt(|p| p.certificate.terms[0].kind = IntegerKind::U8);
    corrupt(|p| {
        let constant = p
            .certificate
            .terms
            .iter_mut()
            .find(|t| matches!(t.node, TermNode::Const(_)))
            .expect("actual literal input");
        let TermNode::Const(n) = &mut constant.node else {
            unreachable!()
        };
        *n += 1;
    });
    corrupt(|p| {
        let proof = p
            .certificate
            .proofs
            .iter_mut()
            .find(|step| matches!(step.reason, Reason::Rule { .. }))
            .expect("actual fold rule witness");
        let Reason::Rule { substitution, .. } = &mut proof.reason else {
            unreachable!()
        };
        *substitution = Substitution {
            bindings: [u32::MAX, 0, 0],
            len: 1,
        };
    });
}

#[test]
fn genuine_guarded_rewrite_cannot_point_its_totality_guard_at_itself() {
    let (mut source, types) = fixture("fn f(x:i64)->i64 eff [Fail[ArithOverflow]]{x*0}");
    let before = source.clone();
    let mut proposed = proposal(&source, &types);
    let (index, proof) = proposed
        .certificate
        .proofs
        .iter_mut()
        .enumerate()
        .find(|(_, step)| {
            matches!(
                step.reason,
                Reason::Rule {
                    rule: 6 | 7 | 8 | 16 | 17 | 18,
                    ..
                }
            )
        })
        .expect("actual erasing rule");
    let Reason::Rule { guard, .. } = &mut proof.reason else {
        unreachable!()
    };
    *guard = Some(index as u32);
    assert!(install_proposal(&mut source, &types, proposed).is_err());
    assert_eq!(source, before);
}

#[test]
fn candidate_wrapping_calls_keep_actual_same_operator_callee_origin() {
    let (mut source, types) = fixture("fn f(x:i64,y:i64)->i64{wrapping_add(wrapping_mul(x,1),y)}");
    let before = source.clone();
    let mut proposed = proposal(&source, &types);
    let ExprKind::Call { callee, .. } = &mut proposed.candidate.kind else {
        panic!("simplified result still has a wrapping_add call")
    };
    callee.span = source.span.start..source.span.start;
    assert!(install_proposal(&mut source, &types, proposed).is_err());
    assert_eq!(source, before);
}

#[test]
fn unrelated_checked_width_span_cannot_be_installed_on_a_candidate_child() {
    let (mut source, types) =
        fixture("fn f(x:i64,y:i64)->i64{let unrelated:u8=1;wrapping_add(wrapping_mul(x,1),y)}");
    let foreign_span = types
        .iter()
        .find_map(|(&(start, end), ty)| (ty == &Type::named("u8")).then_some(start..end))
        .expect("real checked unrelated u8 value span");
    assert_ne!(foreign_span, source.span);
    let before = source.clone();
    let mut proposed = proposal(&source, &types);
    let ExprKind::Call { args, .. } = &mut proposed.candidate.kind else {
        panic!("simplified wrapping_add candidate")
    };
    args[0].expr.span = foreign_span;
    assert_eq!(
        install_proposal(&mut source, &types, proposed),
        Err(WitnessError::OutputMismatch)
    );
    assert_eq!(source, before);
}

fn oversized_candidate(text: &str, mutate: impl FnOnce(&mut Expr, usize)) {
    let (mut source, types) = fixture(text);
    let before = source.clone();
    let mut proposed = proposal(&source, &types);
    mutate(
        &mut proposed.candidate,
        WitnessLimits::default().max_metadata_bytes,
    );
    assert_eq!(
        install_proposal(&mut source, &types, proposed),
        Err(WitnessError::Limit),
        "retained candidate capacity must consume the carried metadata budget"
    );
    assert_eq!(source, before);
}

#[test]
fn retained_candidate_string_capacity_counts_even_with_short_contents() {
    oversized_candidate("fn f(x:i64)->i64{wrapping_add(x,0)}", |candidate, cap| {
        let ExprKind::Name(name) = &mut candidate.kind else {
            panic!("simplified source variable")
        };
        name.reserve_exact(cap);
        assert!(name.capacity() > cap);
    });
    oversized_candidate(
        "fn f(x:i64,y:i64)->i64 eff [Fail[ArithOverflow]]{(x+0)+y}",
        |candidate, cap| {
            let ExprKind::Binary(operator, ..) = &mut candidate.kind else {
                panic!("simplified checked addition")
            };
            operator.reserve_exact(cap);
            assert!(operator.capacity() > cap);
        },
    );
    oversized_candidate(
        "fn f(x:i64,y:i64)->i64{wrapping_add(wrapping_mul(x,1),y)}",
        |candidate, cap| {
            let ExprKind::Call { callee, .. } = &mut candidate.kind else {
                panic!("simplified wrapping addition")
            };
            let ExprKind::Name(name) = &mut callee.kind else {
                unreachable!()
            };
            name.reserve_exact(cap);
            assert!(name.capacity() > cap);
        },
    );
}

#[test]
fn retained_candidate_vector_capacity_counts_at_length_two_or_zero() {
    let text = "fn f(x:i64,y:i64)->i64{wrapping_add(wrapping_mul(x,1),y)}";
    oversized_candidate(text, |candidate, cap| {
        let ExprKind::Call { args, .. } = &mut candidate.kind else {
            panic!("simplified wrapping addition")
        };
        args.reserve_exact(cap / std::mem::size_of::<Arg>() + 1);
        assert_eq!(args.len(), 2);
        assert!(args.capacity() * std::mem::size_of::<Arg>() > cap);
    });
    oversized_candidate(text, |candidate, cap| {
        let ExprKind::Call { types, .. } = &mut candidate.kind else {
            panic!("simplified wrapping addition")
        };
        types.reserve_exact(cap / std::mem::size_of::<Type>() + 1);
        assert!(types.is_empty());
        assert!(types.capacity() * std::mem::size_of::<Type>() > cap);
    });
}

#[test]
fn gate_uses_the_consumed_producer_budget_instead_of_resetting_the_checker() {
    let (mut source, types) = fixture("fn f()->i64{2+3}");
    let before = source.clone();
    let mut proposed = proposal(&source, &types);
    let mut successful = 0usize;
    for _ in 0..=WitnessLimits::default().max_work {
        if proposed.budget.work(1).is_err() {
            break;
        }
        successful += 1;
    }
    assert!(successful > 0);
    assert_eq!(
        install_proposal(&mut source, &types, proposed),
        Err(WitnessError::Limit)
    );
    assert_eq!(
        source, before,
        "gate reset the consumed attempt budget and installed a candidate"
    );
}

#[test]
fn candidate_builder_refuses_expanded_shared_dag_before_materializing_its_tree() {
    let (source, types) = fixture("fn f(x:i64)->i64{wrapping_add(x,0)}");
    let mut budget = WitnessBudget::new(WitnessLimits::default()).unwrap();
    let bound = bind_source(&source, &types, &mut budget).unwrap();
    let mut terms = vec![Term {
        kind: bound.kind(),
        node: TermNode::InputVar(0),
    }];
    for _ in 0..12 {
        let child = terms.len() as u32 - 1;
        terms.push(Term {
            kind: bound.kind(),
            node: TermNode::Wrapping {
                op: IntOp::Add,
                children: [child, child],
            },
        });
    }
    let root = terms.len() as u32 - 1;
    assert_eq!(
        build_candidate(&bound, &terms, root, &mut budget),
        Err(WitnessError::Limit)
    );
}
