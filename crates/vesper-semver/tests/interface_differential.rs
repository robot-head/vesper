//! Differential test of the interface checker (port wave P1 lane 15, #912):
//! the port-backed `compare` and `Projection::is_subset_of` against the
//! frozen Rust seed copy, over generated interface pairs. Only the
//! port-backed package runs it (the seed package lists its tests and leaves
//! this one out); on the seed it would compare the Rust code with itself.
use std::collections::{BTreeMap, BTreeSet};
use vesper_semver::*;

/// splitmix64, so the corpus is the same on every platform.
/// A splitmix64 stream, plus whether to generate only well-formed
/// interfaces (which the checker can classify as patch, minor or major).
struct Rng(u64, bool);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }
    fn pick<T: Clone>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())].clone()
    }
}

const NAMES: &[&str] = &[
    "", "api::A", "api::B", "api::E", "api::f", "api::g", "ext::T", "x",
];
const EFFECTS: &[&str] = &["", "IO", "api::E", "Stack", "ConstTime"];
const MODES: &[Mode] = &[Mode::Copy, Mode::Affine, Mode::Linear];
const RESUMPTIONS: &[Resumption] = &[Resumption::Affine, Resumption::Linear];
const PRIMITIVES: &[Primitive] = &[
    Primitive::Unit,
    Primitive::Never,
    Primitive::Bool,
    Primitive::U8,
    Primitive::U32,
    Primitive::U64,
    Primitive::I64,
    Primitive::F64,
    Primitive::String,
];
const I64S: &[i64] = &[
    i64::MIN,
    i64::MIN + 1,
    -10,
    -1,
    0,
    1,
    10,
    i64::MAX - 1,
    i64::MAX,
];
const U64S: &[u64] = &[0, 1, 9, 10, 20, u64::MAX - 1, u64::MAX];

fn name(rng: &mut Rng) -> String {
    if rng.1 {
        rng.pick(&["api::f", "api::g", "api::h", "api::V"])
            .to_string()
    } else {
        rng.pick(NAMES).to_string()
    }
}

fn ty(rng: &mut Rng, depth: usize) -> Type {
    if rng.1 {
        return clean_ty(rng, depth);
    }
    let leaf = depth == 0;
    match rng.below(if leaf { 5 } else { 9 }) {
        0 | 1 => Type::Primitive {
            name: rng.pick(PRIMITIVES),
        },
        2 => Type::I64Range {
            min: rng.pick(I64S),
            max: rng.pick(I64S),
        },
        3 => Type::U64Range {
            min: rng.pick(U64S),
            max: rng.pick(U64S),
        },
        4 => Type::Parameter {
            index: rng.below(3),
        },
        5 => Type::Named {
            name: name(rng),
            arguments: (0..rng.below(3)).map(|_| ty(rng, depth - 1)).collect(),
        },
        6 => Type::Tuple {
            elements: (0..rng.below(3)).map(|_| ty(rng, depth - 1)).collect(),
        },
        7 => Type::Function {
            signature: Box::new(function(rng, depth - 1)),
        },
        _ => Type::Unsupported {
            description: rng.pick(&["unmodeled", "scoped capability"]).into(),
        },
    }
}

/// Well-formed types: every named type is the declared `ext::T`, ranges are
/// valid, and there are no type parameters or unsupported types.
fn clean_ty(rng: &mut Rng, depth: usize) -> Type {
    match rng.below(if depth == 0 { 4 } else { 6 }) {
        0 | 1 => Type::Primitive {
            name: rng.pick(PRIMITIVES),
        },
        2 => {
            let (a, b) = (rng.pick(I64S), rng.pick(I64S));
            Type::I64Range {
                min: a.min(b),
                max: a.max(b),
            }
        }
        3 => {
            let (a, b) = (rng.pick(U64S), rng.pick(U64S));
            Type::U64Range {
                min: a.min(b),
                max: a.max(b),
            }
        }
        4 => Type::Tuple {
            elements: (0..rng.below(3))
                .map(|_| clean_ty(rng, depth - 1))
                .collect(),
        },
        _ => {
            if rng.chance(2) {
                Type::Named {
                    name: "ext::T".into(),
                    arguments: vec![],
                }
            } else {
                Type::Function {
                    signature: Box::new(function(rng, depth - 1)),
                }
            }
        }
    }
}

fn generics(rng: &mut Rng) -> Vec<Generic> {
    (0..rng.below(3))
        .map(|_| {
            if rng.chance(3) {
                Generic::Row {
                    minimum_resumption: rng.pick(RESUMPTIONS),
                }
            } else {
                Generic::Type {
                    bound: rng.pick(MODES),
                }
            }
        })
        .collect()
}

fn projection(rng: &mut Rng) -> Projection {
    if rng.chance(2) {
        return Projection::All;
    }
    let parts: &[&str] = if rng.1 {
        &["a", "b"]
    } else {
        &["a", "b", "c", ""]
    };
    if rng.1 {
        let paths: BTreeSet<Vec<String>> = (0..1 + rng.below(2))
            .map(|_| {
                (0..1 + rng.below(2))
                    .map(|_| rng.pick(parts).to_string())
                    .collect()
            })
            .collect();
        return Projection::Paths { paths };
    }
    let paths: BTreeSet<Vec<String>> = (0..rng.below(3))
        .map(|_| {
            (0..rng.below(3))
                .map(|_| rng.pick(parts).to_string())
                .collect()
        })
        .collect();
    Projection::Paths { paths }
}

fn capability(rng: &mut Rng, depth: usize) -> Capability {
    Capability {
        identity: rng.pick(&["", "cap:a", "cap:b"]).into(),
        effect: rng.pick(EFFECTS).into(),
        arguments: (0..rng.below(2)).map(|_| ty(rng, depth)).collect(),
        resumption: rng.pick(RESUMPTIONS),
        ceiling: if rng.chance(2) {
            Some(rng.pick(&[0, 100, 256, 384, u64::MAX]))
        } else {
            None
        },
        grade_relation: if rng.chance(3) {
            GradeRelation::Exact
        } else {
            GradeRelation::UpperBound
        },
    }
}

fn function(rng: &mut Rng, depth: usize) -> Function {
    if rng.1 {
        let mut capabilities = vec![];
        for effect in ["IO", "Stack"] {
            if rng.chance(2) {
                let exact = rng.chance(4);
                capabilities.push(Capability {
                    identity: format!("cap:{effect}"),
                    effect: effect.into(),
                    arguments: vec![],
                    resumption: rng.pick(RESUMPTIONS),
                    ceiling: if exact || rng.chance(2) {
                        Some(rng.pick(&[0, 100, 384]))
                    } else {
                        None
                    },
                    grade_relation: if exact {
                        GradeRelation::Exact
                    } else {
                        GradeRelation::UpperBound
                    },
                });
            }
        }
        return Function {
            generics: (0..rng.below(2))
                .map(|_| Generic::Type {
                    bound: rng.pick(MODES),
                })
                .collect(),
            mode: rng.pick(MODES),
            parameters: (0..rng.below(3))
                .map(|_| {
                    let pass = rng.pick(&[Pass::In, Pass::Inout, Pass::Sink]);
                    Parameter {
                        pass,
                        ty: ty(rng, depth),
                        projection: if pass == Pass::Sink {
                            Projection::All
                        } else {
                            projection(rng)
                        },
                    }
                })
                .collect(),
            result: ty(rng, depth),
            result_mode: Mode::Linear,
            effects: Effects {
                capabilities,
                row_variables: BTreeSet::new(),
            },
        };
    }
    Function {
        generics: generics(rng),
        mode: rng.pick(MODES),
        parameters: (0..rng.below(3))
            .map(|_| Parameter {
                pass: rng.pick(&[Pass::In, Pass::Inout, Pass::Sink]),
                ty: ty(rng, depth),
                projection: projection(rng),
            })
            .collect(),
        result: ty(rng, depth),
        result_mode: rng.pick(MODES),
        effects: Effects {
            capabilities: (0..rng.below(3)).map(|_| capability(rng, depth)).collect(),
            row_variables: (0..rng.below(3)).map(|_| rng.below(3)).collect(),
        },
    }
}

fn declaration(rng: &mut Rng) -> Declaration {
    let kinds = if rng.1 { 7 } else { 9 };
    match rng.below(kinds) {
        0..=2 => Declaration::Function {
            signature: function(rng, 2),
        },
        3 | 4 => Declaration::Struct {
            generics: generics(rng),
            mode: rng.pick(MODES),
            fields: if rng.chance(2) {
                Fields::Closed
            } else {
                Fields::Public {
                    fields: (0..rng.below(3))
                        .map(|_| {
                            let field = if rng.1 {
                                "x"
                            } else {
                                rng.pick(&["x", "y", ""])
                            };
                            (field.to_string(), ty(rng, 1))
                        })
                        .collect(),
                }
            },
        },
        5 | 6 => Declaration::Enum {
            generics: generics(rng),
            mode: rng.pick(MODES),
            open: rng.chance(2),
            variants: (0..rng.below(4))
                .map(|_| {
                    let payload = (0..rng.below(3)).map(|_| ty(rng, 1)).collect();
                    (rng.pick(&["A", "B", "C"]).to_string(), payload)
                })
                .collect(),
        },
        7 => {
            let g = generics(rng);
            Declaration::Effect {
                generics: g.clone(),
                sealed: rng.chance(2),
                resumption: rng.pick(RESUMPTIONS),
                operations: (0..rng.below(3))
                    .map(|_| {
                        let mut f = function(rng, 1);
                        if !rng.chance(4) {
                            let mut binders = g.clone();
                            binders.extend(f.generics);
                            f.generics = binders;
                        }
                        (rng.pick(&["get", "put"]).to_string(), f)
                    })
                    .collect(),
            }
        }
        _ => Declaration::Unsupported {
            description: "unmodeled".into(),
        },
    }
}

fn interface(rng: &mut Rng) -> Interface {
    let mut i = Interface::default();
    if rng.1 {
        i.external_types.insert(
            "ext::T".into(),
            ExternalType {
                arity: 0,
                mode: Mode::Copy,
            },
        );
        for _ in 0..rng.below(4) {
            i.exports.insert(name(rng), declaration(rng));
        }
        return i;
    }
    if rng.chance(20) {
        i.schema_version += 1;
    }
    if rng.chance(20) {
        i.semantics_version = 1;
    }
    for _ in 0..rng.below(4) {
        i.exports.insert(name(rng), declaration(rng));
    }
    for _ in 0..rng.below(3) {
        i.external_types.insert(
            name(rng),
            ExternalType {
                arity: rng.below(3),
                mode: rng.pick(MODES),
            },
        );
    }
    i
}

/// A small edit of `old`, so most pairs share most structure.
fn mutate(rng: &mut Rng, old: &Interface) -> Interface {
    let mut new = old.clone();
    for _ in 0..1 + rng.below(3) {
        match rng.below(6) {
            0 => {
                new.exports.insert(name(rng), declaration(rng));
            }
            1 => {
                let key = name(rng);
                new.exports.remove(&key);
            }
            2 if !rng.1 => {
                new.external_types.insert(
                    name(rng),
                    ExternalType {
                        arity: rng.below(3),
                        mode: rng.pick(MODES),
                    },
                );
            }
            _ => {
                // Edit one function signature in place.
                let keys: Vec<String> = new.exports.keys().cloned().collect();
                if keys.is_empty() {
                    continue;
                }
                let key = rng.pick(&keys);
                if let Some(Declaration::Function { signature }) = new.exports.get_mut(&key) {
                    match rng.below(6) {
                        0 => signature.mode = rng.pick(MODES),
                        1 => signature.result = ty(rng, 2),
                        2 => signature.result_mode = rng.pick(MODES),
                        3 if !signature.parameters.is_empty() => {
                            let k = rng.below(signature.parameters.len());
                            let p = &mut signature.parameters[k];
                            match rng.below(3) {
                                0 => p.pass = rng.pick(&[Pass::In, Pass::Inout, Pass::Sink]),
                                1 => p.ty = ty(rng, 2),
                                _ => p.projection = projection(rng),
                            }
                        }
                        4 if !signature.effects.capabilities.is_empty() => {
                            let k = rng.below(signature.effects.capabilities.len());
                            let c = &mut signature.effects.capabilities[k];
                            match rng.below(3) {
                                0 => c.ceiling = Some(rng.pick(&[0, 100, 384, u64::MAX])),
                                1 => c.resumption = rng.pick(RESUMPTIONS),
                                _ => c.grade_relation = GradeRelation::UpperBound,
                            }
                        }
                        _ => signature.generics = generics(rng),
                    }
                } else {
                    new.exports.insert(key, declaration(rng));
                }
            }
        }
    }
    new
}

fn seed<T: serde::de::DeserializeOwned>(value: &impl serde::Serialize) -> T {
    serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap()
}

#[test]
fn compare_matches_the_rust_seed_on_generated_interfaces() {
    assert_eq!(IMPLEMENTATION, "vesper");
    assert_eq!(semver_seed::IMPLEMENTATION, "rust");
    let mut rng = Rng(0x0912, false);
    let mut classes = BTreeMap::new();
    for case in 0..4000 {
        // Half the cases are well-formed.
        rng.1 = case % 2 == 1;
        let old = interface(&mut rng);
        let new = if rng.chance(5) {
            interface(&mut rng)
        } else {
            mutate(&mut rng, &old)
        };
        let got = serde_json::to_value(compare(&old, &new)).unwrap();
        let expected = serde_json::to_value(semver_seed::compare(
            &seed::<semver_seed::Interface>(&old),
            &seed::<semver_seed::Interface>(&new),
        ))
        .unwrap();
        assert_eq!(
            got,
            expected,
            "case {case}\nold {}\nnew {}",
            serde_json::to_string(&old).unwrap(),
            serde_json::to_string(&new).unwrap()
        );
        *classes
            .entry(got["classification"].as_str().unwrap().to_string())
            .or_insert(0) += 1;
    }
    // Every classification occurs.
    assert_eq!(classes.len(), 4, "{classes:?}");
    assert!(classes.values().all(|&n| n >= 100), "{classes:?}");
}

#[test]
fn projection_subsets_match_the_rust_seed() {
    let mut rng = Rng(0x5ab5e7, false);
    for _ in 0..5000 {
        let a = projection(&mut rng);
        let b = projection(&mut rng);
        let expected = seed::<semver_seed::Projection>(&a).is_subset_of(&seed(&b));
        assert_eq!(a.is_subset_of(&b), expected, "{a:?} <= {b:?}");
    }
}

/// A type nested `depth` levels deep: tuples, with a function type every
/// seventh level, around `leaf`. The edit at `edit_at` (if any) swaps one
/// level's tuple for a two-element tuple.
fn deep_ty(depth: usize, leaf: Primitive, edit_at: Option<usize>) -> Type {
    let mut t = Type::Primitive { name: leaf };
    for level in (0..depth).rev() {
        t = if level % 7 == 3 {
            Type::Function {
                signature: Box::new(Function {
                    generics: vec![],
                    mode: Mode::Copy,
                    parameters: vec![],
                    result: t,
                    result_mode: Mode::Copy,
                    effects: Effects::default(),
                }),
            }
        } else if edit_at == Some(level) {
            Type::Tuple {
                elements: vec![
                    t,
                    Type::Primitive {
                        name: Primitive::Unit,
                    },
                ],
            }
        } else {
            Type::Tuple { elements: vec![t] }
        };
    }
    t
}

fn deep_interface(pass: Pass, ty: Type) -> Interface {
    let mut i = Interface::default();
    i.exports.insert(
        "f".into(),
        Declaration::Function {
            signature: Function {
                generics: vec![],
                mode: Mode::Copy,
                parameters: vec![Parameter {
                    pass,
                    ty,
                    projection: Projection::All,
                }],
                result: Type::Primitive {
                    name: Primitive::Unit,
                },
                result_mode: Mode::Copy,
                effects: Effects::default(),
            },
        },
    );
    i
}

/// Types nested far past the checker's 128-level guard: the port's encoder
/// cuts them (at 256 levels) without changing any report, including where
/// the checker compares whole deep types for equality.
#[test]
fn deeply_nested_types_match_the_rust_seed() {
    // The seed's own `==`, serde and drop recurse over the whole tree.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(|| {
            for depth in [200, 300, 5_000] {
                for pass in [Pass::In, Pass::Inout] {
                    let base = deep_ty(depth, Primitive::U8, None);
                    for new_ty in [
                        base.clone(),
                        deep_ty(depth, Primitive::U32, None),
                        deep_ty(depth, Primitive::U8, Some(5)),
                        deep_ty(depth, Primitive::U8, Some(depth - 2)),
                    ] {
                        let old = deep_interface(pass, base.clone());
                        let new = deep_interface(pass, new_ty);
                        let got = serde_json::to_value(compare(&old, &new)).unwrap();
                        let expected = serde_json::to_value(semver_seed::compare(
                            &seed::<semver_seed::Interface>(&old),
                            &seed::<semver_seed::Interface>(&new),
                        ))
                        .unwrap();
                        assert_eq!(got, expected, "depth {depth}, {pass:?}");
                    }
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

/// The port's encoder does not recurse past its cut, so a type far too deep
/// for the stack still yields the checker's nesting report. (The inputs are
/// leaked: Rust's own drop of such a type recurses over the whole tree.)
#[test]
fn types_deeper_than_the_stack_are_reported_unsupported() {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(|| {
            let old = deep_interface(Pass::In, deep_ty(200_000, Primitive::U8, None));
            let new = deep_interface(Pass::In, deep_ty(200_000, Primitive::U32, None));
            let report = compare(&old, &new);
            assert_eq!(report.classification, Classification::Unknown);
            assert!(
                report
                    .issues
                    .iter()
                    .any(|i| i.kind == IssueKind::Unsupported
                        && i.message.ends_with("nesting exceeds 128 levels")),
                "{report:?}"
            );
            std::mem::forget((old, new));
        })
        .unwrap()
        .join()
        .unwrap();
}
