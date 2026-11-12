use vesper_flux::{CircuitBuilder, Error, Field, Frontier, Limits, Predicate, TickInput, ZSet};

fn set(width: usize, rows: &[(&[i64], i64)], limits: Limits) -> ZSet {
    let rows: Vec<_> = rows.iter().map(|(r, w)| (r.to_vec(), *w)).collect();
    ZSet::consolidate(width, &rows, limits).unwrap()
}
fn join(limits: Limits) -> vesper_flux::Circuit {
    let mut b = CircuitBuilder::new(limits).unwrap();
    let a = b.input(2).unwrap();
    let c = b.input(2).unwrap();
    let out = b.equijoin(a, c, 0, 0).unwrap();
    b.finish(out).unwrap()
}
#[test]
fn exact_consolidation_retractions_and_wide_cancellation() {
    let l = Limits::default();
    let raw = vec![
        (vec![1], i64::MAX),
        (vec![1], 1),
        (vec![1], -1),
        (vec![2], -4),
        (vec![2], 4),
    ];
    let z = ZSet::consolidate(1, &raw, l).unwrap();
    assert_eq!(z.entries().collect::<Vec<_>>(), vec![(&vec![1], i64::MAX)]);
    assert_eq!(raw.len(), 5);
    assert_eq!(
        ZSet::consolidate(1, &[(vec![1], i64::MAX), (vec![1], 1)], l),
        Err(Error::Overflow)
    );
    assert_eq!(
        ZSet::consolidate(1, &[(vec![1, 2], 1)], l),
        Err(Error::Schema)
    );
}
#[test]
fn both_join_inputs_change_and_retract_in_same_tick() {
    let l = Limits::default();
    let c = join(l);
    let mut e = c.clone().executor();
    let mut integrals = vec![ZSet::empty(2, l).unwrap(); 2];
    let mut integrated = ZSet::empty(4, l).unwrap();
    let ticks = [
        vec![set(2, &[(&[1, 10], 2)], l), set(2, &[(&[1, 20], 3)], l)],
        vec![
            set(2, &[(&[1, 10], -1), (&[2, 11], 1)], l),
            set(2, &[(&[1, 20], -2), (&[2, 21], 2)], l),
        ],
        vec![
            set(2, &[(&[1, 10], -1), (&[2, 11], -1)], l),
            set(2, &[(&[1, 20], -1), (&[2, 21], -2)], l),
        ],
    ];
    for (tick, deltas) in ticks.into_iter().enumerate() {
        for i in 0..2 {
            integrals[i] = integrals[i].plus(&deltas[i], l).unwrap();
        }
        let delta = e.tick(&TickInput { deltas }).unwrap();
        integrated = integrated.plus(&delta, l).unwrap();
        assert_eq!(integrated, c.batch(&integrals).unwrap(), "tick {tick}");
        assert_eq!(&integrated, e.materialized());
    }
    assert!(integrated.is_empty());
    assert_eq!(e.ticks(), 3);
}
#[test]
fn differential_nested_shared_relations_128_ticks() {
    let l = Limits {
        max_work: 4_000_000,
        ..Limits::default()
    };
    let mut b = CircuitBuilder::new(l).unwrap();
    let a = b.input(2).unwrap();
    let other = b.input(2).unwrap();
    let f = b
        .filter(
            a,
            Predicate::AtLeast {
                column: 1,
                value: 0,
            },
        )
        .unwrap();
    let m = b
        .map(f, vec![Field::Column(0), Field::Constant(7)])
        .unwrap();
    let u = b.union(m, m).unwrap();
    let j = b.equijoin(u, other, 0, 0).unwrap();
    let nested = b.equijoin(j, a, 0, 0).unwrap();
    let root = b
        .map(nested, vec![Field::Column(0), Field::Column(3)])
        .unwrap();
    let c = b.finish(root).unwrap();
    let mut e = c.clone().executor();
    let mut inputs = vec![ZSet::empty(2, l).unwrap(); 2];
    let mut total = ZSet::empty(2, l).unwrap();
    let mut seed = 7183u64;
    for _ in 0..128 {
        let mut deltas = Vec::new();
        for integral in &mut inputs {
            let mut rows = Vec::new();
            for _ in 0..3 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                rows.push((
                    vec![((seed >> 24) % 3) as i64, ((seed >> 32) % 3) as i64 - 1],
                    ((seed >> 40) % 5) as i64 - 2,
                ));
            }
            let delta = ZSet::consolidate(2, &rows, l).unwrap();
            *integral = integral.plus(&delta, l).unwrap();
            deltas.push(delta);
        }
        let delta = e.tick(&TickInput { deltas }).unwrap();
        total = total.plus(&delta, l).unwrap();
        assert_eq!(total, c.batch(&inputs).unwrap());
        assert_eq!(&total, e.materialized());
    }
}
#[test]
fn overflow_is_atomic_and_input_remains_reusable() {
    let l = Limits::default();
    let c = join(l);
    let mut e = c.executor();
    let first = TickInput {
        deltas: vec![set(2, &[(&[1, 1], i64::MAX)], l), set(2, &[], l)],
    };
    e.tick(&first).unwrap();
    let previous = e.materialized().clone();
    let bad = TickInput {
        deltas: vec![set(2, &[], l), set(2, &[(&[1, 2], 2)], l)],
    };
    assert_eq!(e.tick(&bad), Err(Error::Overflow));
    assert_eq!(*e.materialized(), previous);
    assert_eq!(e.ticks(), 1);
    assert_eq!(e.tick(&bad), Err(Error::Overflow));
    assert_eq!(bad.deltas[1].len(), 1);
    let good = TickInput {
        deltas: vec![set(2, &[], l), set(2, &[(&[1, 2], 1)], l)],
    };
    e.tick(&good).unwrap();
    assert_eq!(e.materialized().entries().next().unwrap().1, i64::MAX);
}
#[test]
fn schema_and_all_growth_limits_reject_without_commit() {
    let base = Limits::default();
    assert_eq!(
        ZSet::empty(
            2,
            Limits {
                max_columns: 1,
                ..base
            }
        ),
        Err(Error::Limit("columns"))
    );
    assert_eq!(
        ZSet::consolidate(
            1,
            &[(vec![1], 1)],
            Limits {
                max_input_entries: 0,
                ..base
            }
        ),
        Err(Error::Limit("input entries"))
    );
    assert_eq!(
        ZSet::consolidate(
            1,
            &[(vec![1], 1)],
            Limits {
                max_rows: 0,
                ..base
            }
        ),
        Err(Error::Limit("rows"))
    );
    let mut b = CircuitBuilder::new(Limits {
        max_depth: 1,
        ..base
    })
    .unwrap();
    let a = b.input(1).unwrap();
    assert_eq!(
        b.filter(
            a,
            Predicate::Equal {
                column: 0,
                value: 0
            }
        ),
        Err(Error::Limit("depth"))
    );
    assert_eq!(b.map(a, vec![Field::Column(1)]), Err(Error::Schema));
    let foreign = CircuitBuilder::new(base).unwrap().input(1).unwrap();
    assert_eq!(b.union(a, foreign), Err(Error::InvalidNode));
    let mut small = CircuitBuilder::new(Limits {
        max_nodes: 3,
        ..base
    })
    .unwrap();
    let a = small.input(1).unwrap();
    let d = small.input(1).unwrap();
    let j = small.equijoin(a, d, 0, 0).unwrap();
    assert!(matches!(
        small.finish(j),
        Err(Error::Limit("differentiated nodes"))
    ));
    assert_eq!(small.input(1), Err(Error::Limit("nodes")));
    for (limits, expected) in [
        (
            Limits {
                max_product: 0,
                ..base
            },
            Error::Limit("join product"),
        ),
        (
            Limits {
                max_output: 0,
                ..base
            },
            Error::Limit("output"),
        ),
        (
            Limits {
                max_work: 0,
                ..base
            },
            Error::Limit("work"),
        ),
    ] {
        let mut e = join(limits).executor();
        let tick = TickInput {
            deltas: vec![set(2, &[(&[1, 1], 1)], base), set(2, &[(&[1, 2], 1)], base)],
        };
        assert_eq!(e.tick(&tick), Err(expected));
        assert_eq!(e.ticks(), 0);
        assert!(e.materialized().is_empty());
    }
    let mut e = join(base).executor();
    assert_eq!(
        e.tick(&TickInput { deltas: vec![] }),
        Err(Error::InputCount)
    );
    assert_eq!(
        e.tick(&TickInput {
            deltas: vec![ZSet::empty(1, base).unwrap(); 2]
        }),
        Err(Error::Schema)
    );
}
#[test]
fn repeated_shared_union_has_linear_program_size() {
    let l = Limits {
        max_nodes: 128,
        ..Limits::default()
    };
    let mut b = CircuitBuilder::new(l).unwrap();
    let mut a = b.input(1).unwrap();
    for _ in 0..80 {
        a = b.union(a, a).unwrap();
    }
    let c = b.finish(a).unwrap();
    assert_eq!(c.node_count(), 81);
    assert_eq!(c.differentiated_node_count(), 81);
}
#[test]
fn frontier_respects_every_hold_and_returns_error_owners() {
    let mut f = Frontier::new(0, 2).unwrap();
    let t = f.acquire(1).unwrap();
    let (t, u) = f.fork(t).unwrap();
    let t = f.advance(t, 10).unwrap();
    assert_eq!(f.watermark(), 1);
    let err = f.fork(t).unwrap_err();
    assert_eq!(err.error, Error::Limit("frontier holds"));
    let t = err.ticket;
    let err = f.advance(u, 0).unwrap_err();
    assert_eq!(err.error, Error::WatermarkRegression);
    let u = err.ticket;
    let mut foreign = Frontier::new(0, 1).unwrap();
    let err = foreign.retire(u).unwrap_err();
    assert_eq!(err.error, Error::ForeignTicket);
    let u = err.ticket;
    assert_eq!(f.live_holds(), 2);
    f.retire(u).unwrap();
    assert_eq!(f.watermark(), 10);
    f.retire(t).unwrap();
    assert_eq!(f.live_holds(), 0);
    assert_eq!(f.watermark(), 10);
    assert!(matches!(f.acquire(9), Err(Error::WatermarkRegression)));
    let t = f.acquire(12).unwrap();
    drop(t);
    assert_eq!(f.live_holds(), 1);
}

#[test]
fn late_integral_overflow_rolls_back_other_integrals() {
    let l = Limits::default();
    let mut e = join(l).executor();
    let first = TickInput {
        deltas: vec![set(2, &[(&[1, 1], i64::MAX)], l), set(2, &[], l)],
    };
    e.tick(&first).unwrap();
    // All delta operators succeed (empty right input), but integrating input A fails.
    let bad = TickInput {
        deltas: vec![set(2, &[(&[1, 1], 1)], l), set(2, &[], l)],
    };
    assert_eq!(e.tick(&bad), Err(Error::Overflow));
    assert_eq!(e.ticks(), 1);
    assert!(e.materialized().is_empty());
    let correction = TickInput {
        deltas: vec![
            set(2, &[(&[1, 1], -i64::MAX)], l),
            set(2, &[(&[1, 2], 1)], l),
        ],
    };
    e.tick(&correction).unwrap();
    assert!(e.materialized().is_empty());
    assert_eq!(e.ticks(), 2);
}

#[test]
fn filter_projection_union_have_concrete_row_semantics() {
    let l = Limits::default();
    let mut b = CircuitBuilder::new(l).unwrap();
    let a = b.input(2).unwrap();
    let f = b
        .filter(
            a,
            Predicate::Equal {
                column: 0,
                value: 5,
            },
        )
        .unwrap();
    let m = b.map(f, vec![Field::Constant(9)]).unwrap();
    let root = b.union(m, m).unwrap();
    let c = b.finish(root).unwrap();
    let a = set(2, &[(&[5, 1], 3), (&[5, 2], -1), (&[6, 3], 100)], l);
    assert_eq!(c.batch(&[a]).unwrap(), set(1, &[(&[9], 4)], l));
}

#[test]
fn differentiated_depth_is_bounded_separately() {
    let l = Limits {
        max_depth: 2,
        ..Limits::default()
    };
    let mut b = CircuitBuilder::new(l).unwrap();
    let a = b.input(1).unwrap();
    let c = b.input(1).unwrap();
    let root = b.equijoin(a, c, 0, 0).unwrap();
    assert!(matches!(
        b.finish(root),
        Err(Error::Limit("differentiated depth"))
    ));
    // Failed compilation leaves the original builder intact for more construction.
    assert!(b.union(a, a).is_ok());
}
