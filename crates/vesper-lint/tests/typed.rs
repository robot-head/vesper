use std::collections::BTreeMap;
use vesper_core::ast::{Effect, Effects, Expr, ExprKind, Function, Generic, Mode, Type};
use vesper_lint::*;
fn effect(label: Option<&str>, ty: &str) -> Effect {
    Effect {
        label: label.map(str::to_owned),
        name: "Capability".into(),
        args: vec![Type::named(ty)],
    }
}
fn function(name: &str, effects: Vec<Effect>, actions: Vec<Action>) -> FunctionFacts {
    FunctionFacts {
        declaration: Function {
            name: name.into(),
            generics: vec![],
            params: vec![],
            result: Type::unit(),
            effects: Effects {
                instances: effects,
                tail: None,
            },
            body: Some(Expr {
                kind: ExprKind::Tuple(vec![]),
                span: 0..2,
            }),
            span: 0..2,
        },
        actions,
        parameters: vec![],
        source_owned: true,
    }
}
#[test]
fn structural_instances_not_just_names_and_labels() {
    let a = effect(Some("a"), "u32");
    let b = effect(Some("b"), "u32");
    let c = effect(None, "u64");
    let p = CheckedProgram {
        functions: BTreeMap::from([(
            "f".into(),
            function(
                "f",
                vec![a.clone(), b.clone(), c.clone()],
                vec![Action::Perform(a)],
            ),
        )]),
    };
    let r = analyze(&p, Limits::default()).unwrap();
    assert_eq!(r.findings.len(), 2);
    assert!(r.skipped.is_empty());
    assert!(
        matches!(&r.findings[0].detail,FindingKind::UnusedEffectCapability{effect,..} if effect==&b)
    );
    assert!(
        matches!(&r.findings[1].detail,FindingKind::UnusedEffectCapability{effect,..} if effect==&c)
    );
}
#[test]
fn actual_generic_call_substitutions_are_applied() {
    let mut g = function(
        "generic",
        vec![effect(None, "T")],
        vec![Action::Perform(effect(None, "T"))],
    );
    g.declaration.generics.push(Generic {
        name: "T".into(),
        bound: Mode::Copy,
        row: false,
    });
    let caller = function(
        "caller",
        vec![effect(None, "u32"), effect(None, "u64")],
        vec![Action::Call {
            callee: "generic".into(),
            substitutions: BTreeMap::from([("T".into(), Type::named("u32"))]),
        }],
    );
    let p = CheckedProgram {
        functions: BTreeMap::from([("generic".into(), g), ("caller".into(), caller)]),
    };
    let r = analyze(&p, Limits::default()).unwrap();
    assert_eq!(r.findings.len(), 1);
    assert!(
        matches!(&r.findings[0].detail,FindingKind::UnusedEffectCapability{effect,..} if effect.args==vec![Type::named("u64")])
    );
}
#[test]
fn unresolved_instances_and_cycles_fail_closed() {
    let f = function(
        "f",
        vec![effect(None, "u32")],
        vec![Action::Perform(effect(None, "u64"))],
    );
    let p = CheckedProgram {
        functions: BTreeMap::from([("f".into(), f)]),
    };
    let r = analyze(&p, Limits::default()).unwrap();
    assert!(r.findings.is_empty());
    assert_eq!(r.skipped.len(), 1);
    let f = function(
        "f",
        vec![effect(None, "u32")],
        vec![Action::Call {
            callee: "f".into(),
            substitutions: BTreeMap::new(),
        }],
    );
    let p = CheckedProgram {
        functions: BTreeMap::from([("f".into(), f)]),
    };
    let r = analyze(&p, Limits::default()).unwrap();
    assert!(r.findings.is_empty());
    assert_eq!(r.skipped.len(), 1);
    assert!(
        analyze(
            &p,
            Limits {
                work: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
}
#[test]
fn edits_require_exact_original_nonoverlap_and_utf8_boundaries() {
    let e = SourceEdit {
        span: 1..3,
        expected: "é".into(),
        replacement: "x".into(),
    };
    assert_eq!(
        apply_edits("aéb", std::slice::from_ref(&e), 100).unwrap(),
        "axb"
    );
    assert!(apply_edits("aeb", std::slice::from_ref(&e), 100).is_err());
    assert!(apply_edits("aéb", &[e.clone(), e], 100).is_err());
    assert!(
        apply_edits(
            "aéb",
            &[SourceEdit {
                span: 2..3,
                expected: String::new(),
                replacement: "x".into()
            }],
            100
        )
        .is_err()
    );
    assert!(
        apply_edits(
            "abc",
            &[SourceEdit {
                span: 0..1,
                expected: "a".into(),
                replacement: "longer".into()
            }],
            3
        )
        .is_err()
    );
}
