//! ABBA timing of the port-backed build against the Rust seed copy (port
//! wave P1 lane 15, #912). Ignored by default; run it in release mode:
//!
//! ```text
//! cargo test --release -p vesper-semver --test port_bench -- --ignored --nocapture
//! ```
//!
//! Each round times the port (A) and the seed (B) in the order A B B A on
//! the same inputs and reports the median ratio over the rounds.
use std::hint::black_box;
use std::time::Instant;
use vesper_semver::*;

fn function(k: usize) -> Function {
    Function {
        generics: vec![Generic::Type {
            bound: Mode::Affine,
        }],
        mode: Mode::Copy,
        parameters: (0..3)
            .map(|p| Parameter {
                pass: [Pass::In, Pass::Inout, Pass::Sink][p],
                ty: if p == 0 {
                    Type::U64Range {
                        min: 0,
                        max: 10 + k as u64,
                    }
                } else {
                    Type::Tuple {
                        elements: vec![
                            Type::Primitive {
                                name: Primitive::String,
                            },
                            Type::Named {
                                name: "api::Value".into(),
                                arguments: vec![],
                            },
                        ],
                    }
                },
                projection: if p == 2 {
                    Projection::All
                } else {
                    Projection::Paths {
                        paths: [vec!["a".to_string(), "b".to_string()]].into(),
                    }
                },
            })
            .collect(),
        result: Type::I64Range { min: -5, max: 5 },
        result_mode: Mode::Linear,
        effects: Effects {
            capabilities: vec![Capability {
                identity: "cap:io".into(),
                effect: "IO".into(),
                arguments: vec![],
                resumption: Resumption::Linear,
                ceiling: Some(100),
                grade_relation: GradeRelation::UpperBound,
            }],
            row_variables: Default::default(),
        },
    }
}

/// An interface of `n` functions and a few types, and a minor revision of it
/// (one parameter range widened, one export added).
fn interfaces(n: usize) -> (Interface, Interface) {
    let mut old = Interface::default();
    old.exports.insert(
        "api::Value".into(),
        Declaration::Struct {
            generics: vec![],
            mode: Mode::Copy,
            fields: Fields::Public {
                fields: [(
                    "x".to_string(),
                    Type::Primitive {
                        name: Primitive::U64,
                    },
                )]
                .into(),
            },
        },
    );
    for k in 0..n {
        old.exports.insert(
            format!("api::f{k:03}"),
            Declaration::Function {
                signature: function(k),
            },
        );
    }
    let mut new = old.clone();
    if let Some(Declaration::Function { signature }) = new.exports.get_mut("api::f000") {
        signature.parameters[0].ty = Type::U64Range { min: 0, max: 1000 };
    }
    new.exports.insert(
        "api::added".into(),
        Declaration::Function {
            signature: function(0),
        },
    );
    (old, new)
}

fn seed<T: serde::de::DeserializeOwned>(value: &impl serde::Serialize) -> T {
    serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap()
}

fn time(iterations: u32, mut f: impl FnMut()) -> f64 {
    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    start.elapsed().as_secs_f64() / f64::from(iterations)
}

fn abba(name: &str, iterations: u32, mut a: impl FnMut(), mut b: impl FnMut()) {
    // Warm-up.
    time(iterations, &mut a);
    time(iterations, &mut b);
    let mut ratios = vec![];
    let (mut ta, mut tb) = (vec![], vec![]);
    for _ in 0..21 {
        let a1 = time(iterations, &mut a);
        let b1 = time(iterations, &mut b);
        let b2 = time(iterations, &mut b);
        let a2 = time(iterations, &mut a);
        ta.push((a1 + a2) / 2.0);
        tb.push((b1 + b2) / 2.0);
        ratios.push((a1 + a2) / (b1 + b2));
    }
    for v in [&mut ratios, &mut ta, &mut tb] {
        v.sort_by(f64::total_cmp);
    }
    println!(
        "{name}: port {:.2} us, Rust seed {:.2} us, ratio median {:.2}x (min {:.2}x, max {:.2}x, 21 ABBA rounds)",
        ta[10] * 1e6,
        tb[10] * 1e6,
        ratios[10],
        ratios[0],
        ratios[20]
    );
}

#[test]
#[ignore = "benchmark; run in release mode with --ignored --nocapture"]
fn port_against_rust_seed() {
    assert_eq!(IMPLEMENTATION, "vesper");
    for n in [8, 64] {
        let (old, new) = interfaces(n);
        let (sold, snew): (semver_seed::Interface, semver_seed::Interface) =
            (seed(&old), seed(&new));
        let iterations = if n == 8 { 2000 } else { 300 };
        abba(
            &format!("compare, {n} exports, minor change"),
            iterations,
            || {
                black_box(compare(black_box(&old), black_box(&new)));
            },
            || {
                black_box(semver_seed::compare(black_box(&sold), black_box(&snew)));
            },
        );
        abba(
            &format!("compare, {n} exports, identical"),
            iterations,
            || {
                black_box(compare(black_box(&old), black_box(&old)));
            },
            || {
                black_box(semver_seed::compare(black_box(&sold), black_box(&sold)));
            },
        );
        abba(
            &format!("check_release, {n} exports"),
            iterations,
            || {
                black_box(check_release(&old, &new, "1.2.3-rc.1+build.5", "1.3.0").unwrap());
            },
            || {
                black_box(
                    semver_seed::check_release(&sold, &snew, "1.2.3-rc.1+build.5", "1.3.0")
                        .unwrap(),
                );
            },
        );
    }
    let (empty, sempty) = (Interface::default(), semver_seed::Interface::default());
    abba(
        "check_release, empty interfaces (version parsing and policy)",
        20000,
        || {
            black_box(
                check_release(
                    &empty,
                    &empty,
                    "1.2.3-alpha.10.beta+exp.sha.5114f85",
                    "1.2.3-alpha.9",
                )
                .unwrap(),
            );
        },
        || {
            black_box(
                semver_seed::check_release(
                    &sempty,
                    &sempty,
                    "1.2.3-alpha.10.beta+exp.sha.5114f85",
                    "1.2.3-alpha.9",
                )
                .unwrap(),
            );
        },
    );
}
