//! Public checker cases use explicit certificates, independently of egg.
use vesper::ast::{Arg, Expr, ExprKind, Literal, Pass, Span, Type, TypeMap};
use vesper::numeric::IntegerKind;
use vesper::optimize::witness::{
    Equality, IntOp, Reason, RewriteCertificate, Substitution, Term, TermId, TermNode,
    WitnessError, WitnessLimits, verify_rewrite,
};

const KINDS: [IntegerKind; 4] = [
    IntegerKind::U8,
    IntegerKind::U32,
    IntegerKind::U64,
    IntegerKind::I64,
];

struct Terms {
    kind: IntegerKind,
    nodes: Vec<Term>,
}
impl Terms {
    fn new(kind: IntegerKind) -> Self {
        Self {
            kind,
            nodes: Vec::new(),
        }
    }
    fn add(&mut self, node: TermNode) -> TermId {
        let id = self.nodes.len() as TermId;
        self.nodes.push(Term {
            kind: self.kind,
            node,
        });
        id
    }
    fn c(&mut self, value: i128) -> TermId {
        self.add(TermNode::Const(value))
    }
    fn v(&mut self, id: u32) -> TermId {
        self.add(TermNode::InputVar(id))
    }
    fn op(&mut self, wrapping: bool, op: IntOp, a: TermId, b: TermId) -> TermId {
        self.add(if wrapping {
            TermNode::Wrapping {
                op,
                children: [a, b],
            }
        } else {
            TermNode::Checked {
                op,
                children: [a, b],
            }
        })
    }
    fn certificate(
        self,
        input: TermId,
        output: TermId,
        proofs: Vec<Equality>,
    ) -> RewriteCertificate {
        RewriteCertificate {
            schema_version: 1,
            arithmetic_version: 1,
            terms: self.nodes,
            conclusion: proofs.len() as u32 - 1,
            proofs,
            input,
            output,
        }
    }
}

fn name(op: IntOp, wrapping: bool) -> &'static str {
    match (op, wrapping) {
        (IntOp::Add, false) => "+",
        (IntOp::Sub, false) => "-",
        (IntOp::Mul, false) => "*",
        (IntOp::Add, true) => "wrapping_add",
        (IntOp::Sub, true) => "wrapping_sub",
        (IntOp::Mul, true) => "wrapping_mul",
    }
}

fn source_ast(
    terms: &[Term],
    id: TermId,
    next: &mut usize,
    types: &mut TypeMap,
    origins: &mut Vec<(IntOp, Expr)>,
) -> Expr {
    let span = *next..*next + 1;
    *next += 2;
    let term = terms[id as usize];
    let kind = match term.node {
        TermNode::Const(value) => ExprKind::Literal(Literal::Int(value)),
        TermNode::InputVar(variable) => ExprKind::Name(format!("v{variable}")),
        TermNode::Checked {
            op,
            children: [a, b],
        } => ExprKind::Binary(
            name(op, false).into(),
            Box::new(source_ast(terms, a, next, types, origins)),
            Box::new(source_ast(terms, b, next, types, origins)),
        ),
        TermNode::Wrapping {
            op,
            children: [a, b],
        } => {
            let callee = Expr {
                kind: ExprKind::Name(name(op, true).into()),
                span: *next..*next + 1,
            };
            *next += 2;
            origins.push((op, callee.clone()));
            let first = source_ast(terms, a, next, types, origins);
            let second = source_ast(terms, b, next, types, origins);
            ExprKind::Call {
                callee: Box::new(callee),
                types: vec![],
                args: vec![
                    Arg {
                        pass: Pass::In,
                        explicit: false,
                        expr: first,
                    },
                    Arg {
                        pass: Pass::In,
                        explicit: false,
                        expr: second,
                    },
                ],
            }
        }
    };
    types.insert((span.start, span.end), term.kind.ty());
    Expr { kind, span }
}

fn output_ast(terms: &[Term], id: TermId, span: &Span, origins: &[(IntOp, Expr)]) -> Expr {
    let kind = match terms[id as usize].node {
        TermNode::Const(value) => ExprKind::Literal(Literal::Int(value)),
        TermNode::InputVar(variable) => ExprKind::Name(format!("v{variable}")),
        TermNode::Checked {
            op,
            children: [a, b],
        } => ExprKind::Binary(
            name(op, false).into(),
            Box::new(output_ast(terms, a, span, origins)),
            Box::new(output_ast(terms, b, span, origins)),
        ),
        TermNode::Wrapping {
            op,
            children: [a, b],
        } => ExprKind::Call {
            callee: Box::new(
                origins
                    .iter()
                    .find(|(actual, _)| *actual == op)
                    .expect("source intrinsic")
                    .1
                    .clone(),
            ),
            types: vec![],
            args: vec![
                Arg {
                    pass: Pass::In,
                    explicit: false,
                    expr: output_ast(terms, a, span, origins),
                },
                Arg {
                    pass: Pass::In,
                    explicit: false,
                    expr: output_ast(terms, b, span, origins),
                },
            ],
        },
    };
    Expr {
        kind,
        span: span.clone(),
    }
}

fn ast_pair(certificate: &RewriteCertificate) -> (Expr, Expr, TypeMap) {
    let mut types = TypeMap::new();
    let mut origins = Vec::new();
    let source = source_ast(
        &certificate.terms,
        certificate.input,
        &mut 10,
        &mut types,
        &mut origins,
    );
    let candidate = output_ast(
        &certificate.terms,
        certificate.output,
        &source.span,
        &origins,
    );
    (source, candidate, types)
}

fn rule_step(
    from: TermId,
    to: TermId,
    rule: u16,
    bindings: &[TermId],
    guard: Option<u32>,
) -> Equality {
    let mut substitution = Substitution {
        bindings: [0; 3],
        len: bindings.len() as u8,
    };
    substitution.bindings[..bindings.len()].copy_from_slice(bindings);
    Equality {
        from,
        to,
        reason: Reason::Rule {
            rule,
            substitution,
            guard,
        },
    }
}

fn fixture(kind: IntegerKind, rule: u16) -> RewriteCertificate {
    let mut t = Terms::new(kind);
    let zero = t.c(0);
    let one = t.c(1);
    let x = if rule < 19 { t.v(0) } else { t.c(7) };
    let mut bindings = vec![x];
    let y = if matches!(rule, 12..=16 | 19..=24) {
        let y = if rule < 19 { t.v(1) } else { t.c(3) };
        bindings.push(y);
        y
    } else {
        0
    };
    let z = if matches!(rule, 14..=15) {
        let z = t.v(2);
        bindings.push(z);
        z
    } else {
        0
    };
    let (from, to) = match rule {
        1 => (t.op(false, IntOp::Add, x, zero), x),
        2 => (t.op(false, IntOp::Add, zero, x), x),
        3 => (t.op(false, IntOp::Sub, x, zero), x),
        4 => (t.op(false, IntOp::Mul, x, one), x),
        5 => (t.op(false, IntOp::Mul, one, x), x),
        6 => (t.op(false, IntOp::Mul, x, zero), zero),
        7 => (t.op(false, IntOp::Mul, zero, x), zero),
        8 => (t.op(false, IntOp::Sub, x, x), zero),
        9 => (t.op(true, IntOp::Add, x, zero), x),
        10 => (t.op(true, IntOp::Sub, x, zero), x),
        11 => (t.op(true, IntOp::Mul, x, one), x),
        12..=13 => {
            let op = if rule == 12 { IntOp::Add } else { IntOp::Mul };
            (t.op(true, op, x, y), t.op(true, op, y, x))
        }
        14..=15 => {
            let op = if rule == 14 { IntOp::Add } else { IntOp::Mul };
            let xy = t.op(true, op, x, y);
            let yz = t.op(true, op, y, z);
            (t.op(true, op, xy, z), t.op(true, op, x, yz))
        }
        16 => {
            let xy = t.op(true, IntOp::Add, x, y);
            (t.op(true, IntOp::Sub, xy, y), x)
        }
        17 => (t.op(true, IntOp::Mul, x, zero), zero),
        18 => (t.op(true, IntOp::Sub, x, x), zero),
        19..=24 => {
            let (op, result) = match (rule - 19) % 3 {
                0 => (IntOp::Add, 10),
                1 => (IntOp::Sub, 4),
                _ => (IntOp::Mul, 21),
            };
            let from = t.op(rule >= 22, op, x, y);
            (from, t.c(result))
        }
        _ => panic!("fixture rule"),
    };
    t.certificate(from, to, vec![rule_step(from, to, rule, &bindings, None)])
}

fn check(certificate: &RewriteCertificate) -> Result<(), WitnessError> {
    let (source, candidate, types) = ast_pair(certificate);
    verify_rewrite(
        &source,
        &candidate,
        &types,
        certificate,
        WitnessLimits::default(),
    )
}

#[test]
fn all_twenty_four_rules_check_for_every_closed_integer_kind() {
    for kind in KINDS {
        for rule in 1..=24 {
            assert_eq!(check(&fixture(kind, rule)), Ok(()), "{kind:?} rule {rule}");
        }
    }
}

#[test]
fn shallow_reflexivity_congruence_symmetry_and_transitivity_are_checked() {
    let mut t = Terms::new(IntegerKind::I64);
    let x = t.v(0);
    let zero = t.c(0);
    let inner = t.op(false, IntOp::Add, x, zero);
    let outer = t.op(false, IntOp::Add, inner, zero);
    let proofs = vec![
        rule_step(inner, x, 1, &[x], None),
        Equality {
            from: zero,
            to: zero,
            reason: Reason::Refl,
        },
        Equality {
            from: outer,
            to: inner,
            reason: Reason::Congruence { children: [0, 1] },
        },
        Equality {
            from: outer,
            to: x,
            reason: Reason::Transitivity {
                first: 2,
                second: 0,
            },
        },
    ];
    let certificate = t.certificate(outer, x, proofs);
    assert_eq!(check(&certificate), Ok(()));
    let mut reverse = certificate.clone();
    reverse.input = x;
    reverse.output = outer;
    reverse.proofs.push(Equality {
        from: x,
        to: outer,
        reason: Reason::Symmetry { proof: 3 },
    });
    reverse.conclusion = 4;
    assert_eq!(check(&reverse), Ok(()));
    let mut bad = certificate.clone();
    bad.proofs[2].reason = Reason::Congruence { children: [1, 0] };
    assert_eq!(check(&bad), Err(WitnessError::InvalidEquality));
}

fn guarded() -> RewriteCertificate {
    let mut t = Terms::new(IntegerKind::U8);
    let zero = t.c(0);
    let x = t.v(0);
    let inner = t.op(false, IntOp::Add, x, zero);
    let outer = t.op(false, IntOp::Mul, inner, zero);
    t.certificate(
        outer,
        zero,
        vec![
            rule_step(inner, x, 1, &[x], None),
            rule_step(outer, zero, 6, &[inner], Some(0)),
        ],
    )
}

#[test]
fn erasure_needs_actual_leaf_or_prior_validated_equality_to_leaf() {
    let good = guarded();
    assert_eq!(check(&good), Ok(()));
    for (guard, expected) in [
        (None, WitnessError::UnprovenTotality),
        (Some(1), WitnessError::NonTopologicalProof),
        (Some(99), WitnessError::InvalidProofIndex),
    ] {
        let mut bad = good.clone();
        let Reason::Rule { guard: actual, .. } = &mut bad.proofs[1].reason else {
            unreachable!()
        };
        *actual = guard;
        assert_eq!(check(&bad), Err(expected));
    }
    let mut irrelevant = good.clone();
    irrelevant.proofs[0] = Equality {
        from: 0,
        to: 0,
        reason: Reason::Refl,
    };
    assert_eq!(check(&irrelevant), Err(WitnessError::UnprovenTotality));
}

#[test]
fn checked_overflow_is_not_wrapping_and_u64_product_is_safe() {
    for (kind, rule, a, b, result, accepted) in [
        (IntegerKind::U8, 19, 255, 1, 0, false),
        (IntegerKind::U8, 20, 0, 1, 255, false),
        (
            IntegerKind::I64,
            21,
            i64::MIN as i128,
            -1,
            i64::MIN as i128,
            false,
        ),
        (
            IntegerKind::U64,
            21,
            u64::MAX as i128,
            u64::MAX as i128,
            1,
            false,
        ),
        (
            IntegerKind::U64,
            24,
            u64::MAX as i128,
            u64::MAX as i128,
            1,
            true,
        ),
        (
            IntegerKind::I64,
            22,
            i64::MAX as i128,
            1,
            i64::MIN as i128,
            true,
        ),
        (IntegerKind::U8, 23, 0, 1, 255, true),
    ] {
        let mut t = Terms::new(kind);
        let x = t.c(a);
        let y = t.c(b);
        let op = match (rule - 19) % 3 {
            0 => IntOp::Add,
            1 => IntOp::Sub,
            _ => IntOp::Mul,
        };
        let from = t.op(rule >= 22, op, x, y);
        let to = t.c(result);
        let certificate = t.certificate(from, to, vec![rule_step(from, to, rule, &[x, y], None)]);
        assert_eq!(
            check(&certificate),
            if accepted {
                Ok(())
            } else {
                Err(WitnessError::InvalidRuleInstance)
            }
        );
    }
}

#[test]
fn unused_malformed_terms_and_proofs_are_not_ignored() {
    let good = fixture(IntegerKind::I64, 1);
    let mut unknown = good.clone();
    unknown.proofs.push(rule_step(0, 0, 65535, &[], None));
    assert_eq!(check(&unknown), Err(WitnessError::UnknownRule));
    let mut variable = good.clone();
    variable.terms.push(Term {
        kind: IntegerKind::I64,
        node: TermNode::InputVar(99),
    });
    assert_eq!(check(&variable), Err(WitnessError::UnknownVariable));
    let mut cyclic = good.clone();
    let id = cyclic.terms.len() as u32;
    cyclic.terms.push(Term {
        kind: IntegerKind::I64,
        node: TermNode::Checked {
            op: IntOp::Add,
            children: [id, 0],
        },
    });
    assert_eq!(check(&cyclic), Err(WitnessError::NonTopologicalTerm));
    let mut proof = good.clone();
    let id = proof.proofs.len() as u32;
    proof.proofs.push(Equality {
        from: 0,
        to: 0,
        reason: Reason::Symmetry { proof: id },
    });
    assert_eq!(check(&proof), Err(WitnessError::NonTopologicalProof));
    let mut wrong_kind = good.clone();
    wrong_kind.terms.push(Term {
        kind: IntegerKind::U8,
        node: TermNode::Const(0),
    });
    assert_eq!(check(&wrong_kind), Err(WitnessError::TypeMismatch));
    let mut range = good.clone();
    range.terms.push(Term {
        kind: IntegerKind::I64,
        node: TermNode::Const(i128::MAX),
    });
    assert_eq!(check(&range), Err(WitnessError::ConstantOutOfRange));
}

#[test]
fn versions_substitutions_and_actual_endpoint_values_are_bound() {
    let good = fixture(IntegerKind::U8, 1);
    for arithmetic in [false, true] {
        let mut bad = good.clone();
        if arithmetic {
            bad.arithmetic_version = 2;
        } else {
            bad.schema_version = 2;
        }
        assert_eq!(check(&bad), Err(WitnessError::UnsupportedVersion));
    }
    let mut bad = good.clone();
    let Reason::Rule { substitution, .. } = &mut bad.proofs[0].reason else {
        unreachable!()
    };
    substitution.bindings[2] = 1;
    assert_eq!(check(&bad), Err(WitnessError::InvalidSubstitution));
    let (mut source, candidate, types) = ast_pair(&good);
    let ExprKind::Binary(_, _, right) = &mut source.kind else {
        unreachable!()
    };
    right.kind = ExprKind::Literal(Literal::Int(1));
    assert_eq!(
        verify_rewrite(&source, &candidate, &types, &good, WitnessLimits::default()),
        Err(WitnessError::InputMismatch)
    );
    let (source, mut candidate, types) = ast_pair(&good);
    candidate.kind = ExprKind::Literal(Literal::Int(0));
    assert_eq!(
        verify_rewrite(&source, &candidate, &types, &good, WitnessLimits::default()),
        Err(WitnessError::OutputMismatch)
    );
}

#[test]
fn contextual_literal_metadata_does_not_replace_actual_literal_value() {
    let certificate = fixture(IntegerKind::U8, 1);
    let (source, candidate, mut types) = ast_pair(&certificate);
    let ExprKind::Binary(_, _, zero) = &source.kind else {
        unreachable!()
    };
    let key = (zero.span.start, zero.span.end);
    types.insert(key, Type::IntLiteral(u64::MAX as i128));
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &certificate,
            WitnessLimits::default()
        ),
        Ok(())
    );
    types.insert(key, Type::Infer);
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &certificate,
            WitnessLimits::default()
        ),
        Err(WitnessError::TypeMismatch)
    );
    types.remove(&key);
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &certificate,
            WitnessLimits::default()
        ),
        Err(WitnessError::MissingType)
    );
    types.insert(key, IntegerKind::U32.ty());
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &certificate,
            WitnessLimits::default()
        ),
        Err(WitnessError::TypeMismatch)
    );
}

#[test]
fn candidate_intrinsics_need_original_callee_name_and_span() {
    let certificate = fixture(IntegerKind::I64, 12);
    let (source, mut candidate, mut types) = ast_pair(&certificate);
    let ExprKind::Call { callee, .. } = &mut candidate.kind else {
        unreachable!()
    };
    callee.span = 0..0;
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &certificate,
            WitnessLimits::default()
        ),
        Err(WitnessError::InvalidIntrinsicOrigin)
    );
    let ExprKind::Call { callee, .. } = &source.kind else {
        unreachable!()
    };
    types.insert((callee.span.start, callee.span.end), Type::Infer);
    assert_eq!(
        verify_rewrite(
            &source,
            &source,
            &types,
            &certificate,
            WitnessLimits::default()
        ),
        Err(WitnessError::InvalidIntrinsicOrigin)
    );
}

#[test]
fn all_limit_fields_are_positive_and_hard_bounded() {
    let certificate = fixture(IntegerKind::I64, 1);
    let (source, candidate, types) = ast_pair(&certificate);
    macro_rules! reject_limits { ($($field:ident),+ $(,)?)=>{$(
        let mut limits=WitnessLimits::default(); limits.$field=0;
        assert_eq!(verify_rewrite(&source,&candidate,&types,&certificate,limits),Err(WitnessError::InvalidLimits));
        let mut limits=WitnessLimits::default(); limits.$field=WitnessLimits::HARD.$field+1;
        assert_eq!(verify_rewrite(&source,&candidate,&types,&certificate,limits),Err(WitnessError::InvalidLimits));
    )+}; }
    reject_limits!(
        max_terms,
        max_proofs,
        max_input_nodes,
        max_output_nodes,
        max_depth,
        max_variables,
        max_name_bytes,
        max_metadata_bytes,
        max_work
    );
    let limits = WitnessLimits {
        max_output_nodes: 1,
        ..WitnessLimits::default()
    };
    assert_eq!(
        verify_rewrite(&source, &candidate, &types, &certificate, limits),
        Ok(())
    );
    for limits in [
        WitnessLimits {
            max_input_nodes: 2,
            ..WitnessLimits::default()
        },
        WitnessLimits {
            max_depth: 1,
            ..WitnessLimits::default()
        },
        WitnessLimits {
            max_work: 1,
            ..WitnessLimits::default()
        },
        WitnessLimits {
            max_metadata_bytes: 1,
            ..WitnessLimits::default()
        },
    ] {
        assert_eq!(
            verify_rewrite(&source, &candidate, &types, &certificate, limits),
            Err(WitnessError::Limit)
        );
    }
}

#[test]
fn certificate_reservation_capacity_counts_even_when_unused() {
    let mut certificate = fixture(IntegerKind::I64, 1);
    let (source, candidate, types) = ast_pair(&certificate);
    certificate.terms.try_reserve_exact(100_000).unwrap();
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &certificate,
            WitnessLimits::default()
        ),
        Err(WitnessError::Limit)
    );
}

#[test]
fn candidate_value_spans_cannot_borrow_conflicting_or_arbitrary_typing() {
    let certificate = fixture(IntegerKind::U64, 12);
    let (source, mut candidate, mut types) = ast_pair(&certificate);
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &certificate,
            WitnessLimits::default()
        ),
        Ok(())
    );
    // This is a real other typing location. A backend coercion of a 256-valued
    // operand through u8 would change the mathematically certified U64 term.
    types.insert((1000, 1001), IntegerKind::U8.ty());
    let ExprKind::Call { args, .. } = &mut candidate.kind else {
        unreachable!()
    };
    args[0].expr.span = 1000..1001;
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &certificate,
            WitnessLimits::default()
        ),
        Err(WitnessError::OutputMismatch)
    );
    types.insert((1000, 1001), IntegerKind::U64.ty());
    assert_eq!(
        verify_rewrite(
            &source,
            &candidate,
            &types,
            &certificate,
            WitnessLimits::default()
        ),
        Err(WitnessError::OutputMismatch)
    );
}

#[test]
fn candidate_retained_strings_and_empty_or_live_vectors_are_metered() {
    let limits = WitnessLimits {
        max_metadata_bytes: 64 * 1024,
        ..WitnessLimits::default()
    };
    for field in 0..5 {
        let mut certificate = fixture(IntegerKind::U64, if field < 2 { 1 } else { 12 });
        if field == 1 {
            let old_input = certificate.input;
            let old_output = certificate.output;
            certificate.input = old_output;
            certificate.output = old_input;
            certificate.proofs.push(Equality {
                from: old_output,
                to: old_input,
                reason: Reason::Symmetry { proof: 0 },
            });
            certificate.conclusion = 1;
        }
        let (source, mut candidate, types) = ast_pair(&certificate);
        assert_eq!(
            verify_rewrite(&source, &candidate, &types, &certificate, limits),
            Ok(()),
            "baseline field {field}"
        );
        let extra = limits.max_metadata_bytes + 1;
        match (&mut candidate.kind, field) {
            (ExprKind::Name(name), 0) => name.try_reserve_exact(extra).unwrap(),
            (ExprKind::Binary(op, _, _), 1) => op.try_reserve_exact(extra).unwrap(),
            (ExprKind::Call { callee, .. }, 2) => {
                let ExprKind::Name(name) = &mut callee.kind else {
                    unreachable!()
                };
                name.try_reserve_exact(extra).unwrap();
            }
            (ExprKind::Call { args, .. }, 3) => args
                .try_reserve_exact(extra / std::mem::size_of::<Arg>() + 1)
                .unwrap(),
            (ExprKind::Call { types, .. }, 4) => {
                assert!(types.is_empty());
                types
                    .try_reserve_exact(extra / std::mem::size_of::<Type>() + 1)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            verify_rewrite(&source, &candidate, &types, &certificate, limits),
            Err(WitnessError::Limit),
            "retained field {field}"
        );
    }
}
