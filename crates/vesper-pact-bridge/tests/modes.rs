//! Source type/mode agreement between `World::mode` and Pact's constructor
//! schemes, over every allowed copy/affine/linear argument combination.

mod common;

use common::*;
use std::collections::BTreeMap;
use vesper::ast::{Mode, Type};
use vesper_pact_bridge::*;

const MODE_SOURCE: &str = "
linear struct Lin { id: u64 }
copy struct Tag[T] { }
struct Holder[T] { t: T }
copy struct CopyBox[T: mod copy] { t: T }
struct Capped[T: mod affine] { t: T @@ affine }
enum List[T] { Nil, Cons(T, List[T]) }
enum Rose[T] { Node(T, List[Rose[T]]) }
struct Swap[T, U] { t: T, s: Option[Swap[U, T]] }
copy struct Two[T, U] { u: U }
copy struct Wrap[T] { tag: Tag[T] }
struct Eff[T, r: eff] { v: T }
linear enum Pick[A, B: mod affine] { L(A), R(B) }
struct Pinned[T] { s: String }
";

fn pkg() -> PackageIdentity {
    PackageIdentity::new("@test/modes", "1.0.0")
}

fn session() -> (CheckedPackage, CoherentSession) {
    let checked = check(pkg(), MODE_SOURCE);
    let s = SessionBuilder::new()
        .package(checked.clone(), BindingsDto::empty(pkg()))
        .build()
        .unwrap_or_else(|e| panic!("{e}"));
    (checked, s)
}

/// The least mode Pact proves for `t` via `ModeBound` queries.
fn pact_mode(s: &CoherentSession, t: &Type) -> Mode {
    for m in [Mode::Copy, Mode::Affine, Mode::Linear] {
        let q = query(
            pkg(),
            GoalDto::ModeBound {
                ty: TypeDto::from_source(t).unwrap(),
                bound: ModeDto::Const(m),
            },
        );
        match s.solve(&q).unwrap() {
            BridgeOutcome::Proven(e) => {
                s.consume(&e).unwrap();
                return m;
            }
            BridgeOutcome::NoSolution { .. } => {}
            other => panic!("mode query for {t} was {}", other.verdict()),
        }
    }
    panic!("no mode bound holds for {t}")
}

fn named(n: &str, args: Vec<Type>) -> Type {
    Type::Named(n.into(), args)
}

#[test]
fn every_allowed_argument_combination_agrees_with_world_mode() {
    let (checked, s) = session();
    let world = checked.world();
    let candidates = [
        (Mode::Copy, named("u8", vec![])),
        (Mode::Affine, named("String", vec![])),
        (Mode::Linear, named("Lin", vec![])),
    ];
    let mut checked_cases = 0;
    for adt in checked.adt_names() {
        let a = &world.adts[adt];
        // Cartesian product over type parameters, respecting declared bounds.
        let mut combos: Vec<Vec<Type>> = vec![vec![]];
        for g in &a.generics {
            let mut next = vec![];
            for c in &combos {
                if g.row {
                    let mut c = c.clone();
                    c.push(Type::Row(Default::default()));
                    next.push(c);
                    continue;
                }
                for (m, t) in &candidates {
                    if *m <= g.bound {
                        let mut c = c.clone();
                        c.push(t.clone());
                        next.push(c);
                    }
                }
            }
            combos = next;
        }
        for args in combos {
            let t = named(adt, args);
            let expected = world.mode(&t, &BTreeMap::new());
            assert_eq!(pact_mode(&s, &t), expected, "mode of {t}");
            checked_cases += 1;
        }
    }
    assert!(checked_cases > 40, "{checked_cases}");
}

#[test]
fn nested_and_tuple_instantiations_agree() {
    let (checked, s) = session();
    let world = checked.world();
    let lin = named("Lin", vec![]);
    let cases = vec![
        named("List", vec![named("Holder", vec![named("String", vec![])])]),
        named("Rose", vec![named("Tag", vec![lin.clone()])]),
        named("Swap", vec![named("u8", vec![]), lin.clone()]),
        named("Wrap", vec![lin.clone()]),
        named("Option", vec![named("List", vec![lin.clone()])]),
        named("Result", vec![named("u8", vec![]), named("String", vec![])]),
        Type::Tuple(vec![named("u8", vec![]), named("String", vec![])]),
        Type::Tuple(vec![named("u8", vec![]), named("Tag", vec![lin.clone()])]),
        Type::Tuple(vec![]),
        named("Unit", vec![]),
        named("Never", vec![]),
        named(
            "CopyBox",
            vec![Type::Tuple(vec![
                named("u64", vec![]),
                named("bool", vec![]),
            ])],
        ),
    ];
    for t in cases {
        assert_eq!(
            pact_mode(&s, &t),
            world.mode(&t, &BTreeMap::new()),
            "mode of {t}"
        );
    }
}

#[test]
fn schemes_record_phantom_parameters_and_bases() {
    let (_, s) = session();
    let scheme = |name: &str| {
        let id = s
            .tables()
            .type_id(&DefPath::new(pkg(), DefKind::Type, name))
            .unwrap();
        s.tables().type_entry(id).unwrap().mode.clone()
    };
    use vesper_pact::Mode as P;
    assert_eq!(scheme("Tag").joins, Vec::<u32>::new());
    assert_eq!(scheme("Tag").base, P::Copy);
    assert_eq!(scheme("Wrap").joins, Vec::<u32>::new());
    assert_eq!(scheme("Two").joins, vec![1]);
    assert_eq!(scheme("Holder").base, P::Affine);
    assert_eq!(scheme("Holder").joins, vec![0]);
    assert_eq!(scheme("Swap").joins, vec![0, 1]);
    assert_eq!(scheme("Lin").base, P::Linear);
    assert_eq!(scheme("Pinned").joins, Vec::<u32>::new());
    // Row parameters never contribute to the mode.
    assert_eq!(scheme("Eff").joins, vec![0]);
}

#[test]
fn placeholder_bounds_follow_world_mode() {
    let (checked, s) = session();
    let world = checked.world();
    for bound in [Mode::Copy, Mode::Affine, Mode::Linear] {
        for t in [
            named("Holder", vec![named("T", vec![])]),
            named("Tag", vec![named("T", vec![])]),
            named("List", vec![named("T", vec![])]),
        ] {
            let mut bounds = BTreeMap::new();
            bounds.insert("T".to_string(), bound);
            let expected = world.mode(&t, &bounds);
            let mut least = None;
            for m in [Mode::Copy, Mode::Affine, Mode::Linear] {
                let q = QueryBindingDto::new(
                    pkg(),
                    EnvDto {
                        params: vec![tparam_bound("T", bound)],
                        ..EnvDto::default()
                    },
                    vec![],
                    GoalDto::ModeBound {
                        ty: TypeDto::from_source(&t).unwrap(),
                        bound: ModeDto::Const(m),
                    },
                );
                if s.solve(&q).unwrap().evidence().is_some() {
                    least = Some(m);
                    break;
                }
            }
            assert_eq!(least, Some(expected), "{t} with T: mod {bound:?}");
        }
    }
}

#[test]
fn ill_formed_instantiations_are_rejected() {
    let (_, s) = session();
    for t in [
        named("CopyBox", vec![named("String", vec![])]),
        named("Capped", vec![named("Lin", vec![])]),
        named("Pick", vec![named("u8", vec![]), named("Lin", vec![])]),
    ] {
        let q = query(
            pkg(),
            GoalDto::ModeBound {
                ty: TypeDto::from_source(&t).unwrap(),
                bound: ModeDto::Const(Mode::Linear),
            },
        );
        let e = s.solve(&q).unwrap_err();
        assert_eq!(e.kind, ErrorKind::InvalidQuery, "{t}: {e}");
        assert!(e.mentions("mod"), "{e}");
    }
}

#[test]
fn unverifiable_schemes_are_unsupported_not_approximated() {
    let src = "struct Wide[A, B, C, D, E, F, G, H, I] { a: A }";
    let p = PackageIdentity::new("@test/wide", "1.0.0");
    let e = SessionBuilder::new()
        .package(check(p.clone(), src), BindingsDto::empty(p))
        .build()
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unsupported);
    assert!(e.mentions("argument-mode combinations"), "{e}");
    assert!(e.diagnostics[0].span.is_some(), "{e}");
    // Bounded parameters shrink the verified domain enough to accept it.
    let src =
        "struct Wide[A: mod copy, B: mod copy, C: mod copy, D: mod copy, E, F, G, H, I] { a: A }";
    let p = PackageIdentity::new("@test/wide", "1.0.0");
    SessionBuilder::new()
        .package(check(p.clone(), src), BindingsDto::empty(p))
        .build()
        .unwrap();
}
