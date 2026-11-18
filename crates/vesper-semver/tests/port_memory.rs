//! The Vesper port frees everything it allocates: after warm-up, repeated
//! comparisons and release checks (accepted, rejected and malformed) leave
//! no live port allocation behind. In the Rust build the count is always 0.
//! Its own test binary, so no other test allocates concurrently.
use vesper_semver::{
    Declaration, Effects, Fields, Function, Interface, Mode, Parameter, Pass, Primitive,
    Projection, Type, check_release, compare, port_live_allocations,
};

fn interfaces() -> (Interface, Interface) {
    let signature = Function {
        generics: vec![],
        mode: Mode::Copy,
        parameters: vec![Parameter {
            pass: Pass::Inout,
            ty: Type::U64Range { min: 0, max: 10 },
            projection: Projection::Paths {
                paths: [vec!["a".to_string()]].into(),
            },
        }],
        result: Type::Primitive {
            name: Primitive::I64,
        },
        result_mode: Mode::Copy,
        effects: Effects::default(),
    };
    let mut old = Interface::default();
    old.exports.insert(
        "api::read".into(),
        Declaration::Function {
            signature: signature.clone(),
        },
    );
    old.exports.insert(
        "api::Value".into(),
        Declaration::Struct {
            generics: vec![],
            mode: Mode::Copy,
            fields: Fields::Closed,
        },
    );
    let mut new = old.clone();
    let mut changed = signature;
    changed.parameters[0].pass = Pass::In;
    changed.result = Type::I64Range { min: -1, max: 1 };
    new.exports.insert(
        "api::read".into(),
        Declaration::Function { signature: changed },
    );
    new.exports.insert(
        "api::Unknown".into(),
        Declaration::Unsupported {
            description: "unmodeled".into(),
        },
    );
    (old, new)
}

fn cycle(old: &Interface, new: &Interface) {
    assert!(!compare(old, new).issues.is_empty());
    assert!(compare(old, old).is_compatible());
    assert!(check_release(old, old, "1.0.0", "1.0.1").unwrap().accepted);
    assert!(
        !check_release(old, new, "1.0.0-rc.1", "1.0.0")
            .unwrap()
            .accepted
    );
    assert!(check_release(old, new, "1.0", "2.0.0").is_err());
    assert!(check_release(old, new, "1.0.0", "2.0.0+").is_err());
}

#[test]
fn the_port_frees_every_allocation() {
    let (old, new) = interfaces();
    cycle(&old, &new);
    let before = port_live_allocations();
    for _ in 0..1000 {
        cycle(&old, &new);
    }
    assert_eq!(
        port_live_allocations(),
        before,
        "the Vesper port leaked over 1000 cycles"
    );
}
