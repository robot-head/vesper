//! Independent finite-profile checks; these do not supply arithmetic answers.
#[path = "../src/bin/arithmetic_oracle42/mod.rs"]
pub mod oracle;

use oracle::{Callee, Case, Input, Intent, Kind, Node, Op, Program, Scope, Suite, corpus::*};
use std::collections::BTreeSet;
use vesper::{
    ast::{ExprKind, Item, Literal, Statement},
    check, parser,
};

fn tag(k: Kind) -> usize {
    match k {
        Kind::U8 => 0,
        Kind::U32 => 1,
        Kind::U64 => 2,
        Kind::I64 => 3,
    }
}
fn limits(k: Kind) -> (i128, i128) {
    match k {
        Kind::U8 => (0, 255),
        Kind::U32 => (0, 4294967295),
        Kind::U64 => (0, 18446744073709551615),
        Kind::I64 => (-9223372036854775808, 9223372036854775807),
    }
}

fn audit_program(p: &Program) {
    assert!(p.inputs.len() <= 3 && p.locals.len() <= 3);
    let mut nodes = 0;
    for (available, code) in p
        .locals
        .iter()
        .chain(std::iter::once(&p.result))
        .enumerate()
    {
        let mut depths = vec![];
        nodes += code.len();
        for n in code {
            match n {
                Node::Int(v) => {
                    let (lo, hi) = limits(p.kind);
                    assert!((lo..=hi).contains(v));
                    depths.push(1);
                }
                Node::Variable(Scope::Input, i) => {
                    assert!((*i as usize) < p.inputs.len());
                    assert_eq!(p.inputs[*i as usize].kind, p.kind);
                    depths.push(1);
                }
                Node::Variable(Scope::Local, i) => {
                    assert!((*i as usize) < available);
                    depths.push(1);
                }
                Node::Checked(_) | Node::Call(Callee::Intrinsic(_), 2) => {
                    let r = depths.pop().unwrap();
                    let l = depths.pop().unwrap();
                    depths.push(1 + usize::max(l, r));
                }
                _ => panic!("non-valid generated node"),
            }
        }
        assert_eq!(depths.len(), 1);
        assert!(depths[0] <= 8);
    }
    assert!(nodes <= 63);
}

#[test]
fn full_generator_has_exactly_100000_distinct_fitting_argument_pairs_and_fingerprints() {
    let mut pairs = BTreeSet::new();
    let mut fingerprints = Vec::with_capacity(100000);
    let mut kinds = [0; 4];
    for id in 0..100000 {
        let c = case_at(Suite::Full100000, 0, id).unwrap();
        assert_eq!(c.id, id);
        assert_eq!(c.intent, Intent::Valid);
        audit_program(&c.program);
        let k = tag(c.program.kind);
        kinds[k] += 1;
        assert_eq!(c.program.inputs.len(), 2);
        for i in &c.program.inputs {
            let (lo, hi) = limits(i.kind);
            assert!((lo..=hi).contains(&i.value));
        }
        assert!(pairs.insert((k, c.program.inputs[0].value, c.program.inputs[1].value)));
        fingerprints.push(semantic_fingerprint(&c).unwrap());
    }
    assert_eq!(kinds, [25000; 4]);
    assert_eq!(pairs.len(), 100000);
    fingerprints.sort_unstable();
    assert!(fingerprints.windows(2).all(|v| v[0] != v[1]));
    assert!(case_at(Suite::Full100000, 0, 100000).is_err());
}

#[test]
fn ci_rank_order_is_an_exact_seeded_full_subset_and_tables_follow_boundary_order() {
    for seed in [0, 1, u64::MAX] {
        let mut selected = BTreeSet::new();
        for id in 0..128 {
            let q = id / 4;
            let rank = if q < 16 {
                q
            } else {
                let j = q - 15;
                256 * j + j % 16
            };
            let ci = case_at(Suite::Ci128, seed, id).unwrap();
            let full = case_at(Suite::Full100000, seed, 4 * rank + id % 4).unwrap();
            assert_eq!(ci.program, full.program);
            assert!(selected.insert((tag(ci.program.kind), rank)));
            let source = render_case(&ci).unwrap();
            assert!(source.len() <= 16384);
            let parsed = parser::parse(&source);
            assert!(
                parsed.diagnostics.is_empty(),
                "{source}: {:?}",
                parsed.diagnostics
            );
            validate_rendered(&ci, &source, &parsed.module).unwrap();
            let checked = check::check(&parsed.module);
            assert!(
                checked.diagnostics.is_empty(),
                "{source}: {:?}",
                checked.diagnostics
            );
        }
        assert_eq!(selected.len(), 128);
    }
    for kind in [Kind::U8, Kind::U32, Kind::U64, Kind::I64] {
        let (lo, hi) = limits(kind);
        let mut expected = vec![];
        for n in [0, 1, lo, hi, lo + 1, hi - 1, -1, -2, 2, 3]
            .into_iter()
            .chain(0..=255)
        {
            if (lo..=hi).contains(&n) && !expected.contains(&n) {
                expected.push(n);
            }
            if expected.len() == 256 {
                break;
            }
        }
        for rank in 0..256 {
            let c = case_at(Suite::Full100000, 17, 4 * rank + tag(kind) as u64).unwrap();
            assert_eq!(c.program.inputs[0].value, expected[rank as usize]);
        }
    }
    assert!(case_at(Suite::Ci128, 0, 128).is_err());
}

#[test]
fn isolated_negative_matrix_matches_actual_parser_or_checker_single_codes() {
    let cases = negative_cases(128).unwrap();
    assert_eq!(cases.len(), 15);
    let codes = [
        "E0201", "E0200", "E0209", "E0209", "E0209", "E0002", "E0209", "E0209", "E0401", "E0207",
        "E0200", "E0206", "E0201", "E0201", "E0002",
    ];
    for (n, c) in cases.iter().enumerate() {
        assert_eq!(c.id, 128 + n as u64);
        assert_eq!(c.intent, Intent::StaticNegative);
        let source = render_case(c).unwrap();
        let parsed = parser::parse(&source);
        if matches!(n, 5 | 14) {
            assert_eq!(parsed.diagnostics.len(), 1);
            assert_eq!(parsed.diagnostics[0].code, codes[n]);
        } else {
            assert!(parsed.diagnostics.is_empty());
            validate_rendered(c, &source, &parsed.module).unwrap();
            let checked = check::check(&parsed.module);
            assert_eq!(
                checked.diagnostics.len(),
                1,
                "{source}: {:?}",
                checked.diagnostics
            );
            assert_eq!(checked.diagnostics[0].code, codes[n]);
        }
    }
    assert!(negative_cases(u64::MAX - 13).is_err());
}

#[test]
fn semantic_hash_has_independent_canonical_bytes_and_excludes_case_identity() {
    let c = case_at(Suite::Ci128, 0, 0).unwrap();
    let mut bytes = b"vesper.oracle42.program\0".to_vec();
    bytes.extend(1u16.to_le_bytes());
    bytes.extend([0, 2, 0]);
    bytes.extend(0i128.to_le_bytes());
    bytes.push(0);
    bytes.extend(0i128.to_le_bytes());
    bytes.extend([1, 0]);
    bytes.extend(1u16.to_le_bytes());
    bytes.push(0);
    bytes.extend(42i128.to_le_bytes());
    assert_eq!(
        semantic_fingerprint(&c).unwrap(),
        *blake3::hash(&bytes).as_bytes()
    );
    let mut changed = c.clone();
    changed.id = 999;
    changed.intent = Intent::StaticNegative;
    assert_eq!(
        semantic_fingerprint(&c).unwrap(),
        semantic_fingerprint(&changed).unwrap()
    );
    changed.program.inputs[1].value = 1;
    assert_ne!(
        semantic_fingerprint(&c).unwrap(),
        semantic_fingerprint(&changed).unwrap()
    );
}

#[test]
fn source_binding_rejects_wrapper_arguments_types_extra_functions_and_operator_drift() {
    let c = case_at(Suite::Ci128, 0, 4 * 13).unwrap();
    let source = render_case(&c).unwrap();
    for modified in [
        source.replacen("(false, f(", "(true, f(", 1),
        source.replacen("(true, 0)", "(true, 1)", 1),
        source.replacen("x0: u8", "x0: u32", 1),
        source.replacen("wrapping_add(x0, 0)", "wrapping_sub(x0, 0)", 1),
        source.replacen("return v => v", "return v => 0", 1),
        format!("{source} fn extra() -> u8 {{ 0 }}"),
    ] {
        assert_ne!(modified, source);
        let parsed = parser::parse(&modified);
        assert!(parsed.diagnostics.is_empty(), "{modified}");
        assert!(
            validate_rendered(&c, &modified, &parsed.module).is_err(),
            "accepted {modified}"
        );
    }
    let mut parsed = parser::parse(&source).module;
    let Item::Function(main) = &mut parsed.items[1] else {
        panic!()
    };
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &mut main.body.as_mut().unwrap().kind
    else {
        panic!()
    };
    let ExprKind::Handle { body, .. } = &mut tail.kind else {
        panic!()
    };
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &mut body.kind
    else {
        panic!()
    };
    let ExprKind::Tuple(values) = &mut tail.kind else {
        panic!()
    };
    let ExprKind::Call { args, .. } = &mut values[1].kind else {
        panic!()
    };
    args.swap(0, 1);
    assert!(validate_rendered(&c, &source, &parsed).is_err());
}

#[test]
fn unused_strict_local_and_its_order_are_bound_and_cannot_disappear() {
    let c = case_at(Suite::Ci128, 0, 4 * 14 + 2).unwrap();
    let source = render_case(&c).unwrap();
    let mut parsed = parser::parse(&source).module;
    let Item::Function(f) = &mut parsed.items[0] else {
        panic!()
    };
    let ExprKind::Block { statements, .. } = &mut f.body.as_mut().unwrap().kind else {
        panic!()
    };
    assert!(matches!(statements[0], Statement::Let { .. }));
    statements.clear();
    assert!(validate_rendered(&c, &source, &parsed).is_err());
    let c = case_at(Suite::Ci128, 0, 4 * 15).unwrap();
    let source = render_case(&c).unwrap();
    let mut parsed = parser::parse(&source).module;
    let Item::Function(f) = &mut parsed.items[0] else {
        panic!()
    };
    let ExprKind::Block { statements, .. } = &mut f.body.as_mut().unwrap().kind else {
        panic!()
    };
    statements.swap(0, 1);
    assert!(validate_rendered(&c, &source, &parsed).is_err());
}

fn minimal() -> Case {
    Case {
        id: 7,
        intent: Intent::Valid,
        program: Program {
            kind: Kind::U8,
            inputs: vec![Input {
                kind: Kind::U8,
                value: 42,
            }],
            locals: vec![],
            result: vec![Node::Variable(Scope::Input, 0)],
            allow_fail: true,
        },
    }
}

#[test]
fn malformed_postfix_or_caps_are_refused_before_source_or_hashing() {
    let mut variants = vec![];
    for code in [
        vec![],
        vec![Node::Checked(Op::Add)],
        vec![Node::Int(0), Node::Int(1)],
        vec![Node::Int(0); 64],
        vec![Node::Call(Callee::Intrinsic(Op::Add), 4)],
    ] {
        let mut c = minimal();
        c.program.result = code;
        variants.push(c);
    }
    let mut c = minimal();
    c.program.inputs[0].value = 256;
    variants.push(c);
    let mut c = minimal();
    c.program.locals = vec![vec![Node::Int(0)]; 4];
    variants.push(c);
    let mut c = minimal();
    c.program.inputs = vec![
        Input {
            kind: Kind::U8,
            value: 0
        };
        4
    ];
    variants.push(c);
    let mut c = minimal();
    c.program.result = vec![Node::Int(0)];
    for _ in 0..8 {
        c.program
            .result
            .extend([Node::Int(1), Node::Checked(Op::Add)]);
    }
    variants.push(c);
    for c in variants {
        assert!(render_case(&c).is_err());
        assert!(semantic_fingerprint(&c).is_err());
        assert!(shrink_candidates(&c).is_err());
    }
}

#[test]
fn shrinking_is_bounded_deterministic_and_rebinds_real_smaller_source() {
    for id in [4 * 13, 4 * 14, 4 * 15, 127] {
        let c = case_at(Suite::Ci128, 42, id).unwrap();
        let candidates = shrink_candidates(&c).unwrap();
        assert!(!candidates.is_empty());
        assert!(candidates.len() <= 64);
        assert_eq!(candidates, shrink_candidates(&c).unwrap());
        let mut hashes = BTreeSet::new();
        for candidate in candidates {
            assert_eq!(candidate.id, c.id);
            assert_eq!(candidate.intent, c.intent);
            assert_ne!(candidate.program, c.program);
            audit_program(&candidate.program);
            assert!(hashes.insert(semantic_fingerprint(&candidate).unwrap()));
            let source = render_case(&candidate).unwrap();
            let parsed = parser::parse(&source);
            assert!(parsed.diagnostics.is_empty());
            validate_rendered(&candidate, &source, &parsed.module).unwrap();
            assert!(check::check(&parsed.module).diagnostics.is_empty());
        }
    }
}

#[test]
fn source_binding_does_not_accept_a_modified_parsed_literal_as_the_intended_case() {
    let c = case_at(Suite::Ci128, 0, 0).unwrap();
    let source = render_case(&c).unwrap();
    let mut parsed = parser::parse(&source).module;
    let Item::Function(f) = &mut parsed.items[0] else {
        panic!()
    };
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &mut f.body.as_mut().unwrap().kind
    else {
        panic!()
    };
    tail.kind = ExprKind::Literal(Literal::Int(43));
    assert!(validate_rendered(&c, &source, &parsed).is_err());
}
