//! Deterministic colouring tests.

use vesper_kinetic::coloring::{MAX_COLORS, islands_are_disjoint};
use vesper_kinetic::{Colorer, ColoringError, ConstraintBodies};

fn pair(a: u32, b: u32) -> ConstraintBodies {
    ConstraintBodies { a, b: Some(b) }
}

fn single(a: u32) -> ConstraintBodies {
    ConstraintBodies { a, b: None }
}

fn key(c: ConstraintBodies) -> (u32, u32) {
    let b = c.b.unwrap_or(c.a);
    (c.a.min(b), c.a.max(b))
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

#[test]
fn chain_uses_two_colours_and_islands_are_disjoint() {
    let chain: Vec<_> = (0..9).map(|i| pair(i, i + 1)).collect();
    let mut c = Colorer::new(10, 16);
    c.color(&chain).unwrap();
    assert_eq!(c.layer_count(), 2);
    assert_eq!(c.layer(0), &[0, 2, 4, 6, 8]);
    assert_eq!(c.layer(1), &[1, 3, 5, 7]);
    assert!(islands_are_disjoint(&c, &chain));
}

#[test]
fn colouring_is_greedy_lowest_free_in_body_order() {
    // Visiting order sorts by (min body, max body): (0,1) (0,2) (1,2) (2) (3,4).
    let cs = [pair(2, 1), single(2), pair(4, 3), pair(0, 2), pair(1, 0)];
    let mut c = Colorer::new(5, 8);
    c.color(&cs).unwrap();
    assert_eq!(c.visiting_order(), &[4, 3, 0, 1, 2]);
    let colors: Vec<u8> = (0..cs.len()).map(|i| c.color_of(i)).collect();
    assert_eq!(colors, vec![2, 0, 0, 1, 0]);
    assert!(islands_are_disjoint(&c, &cs));
}

#[test]
fn random_graphs_are_disjoint_and_independent_of_input_order() {
    let mut rng = Lcg(42);
    for round in 0..200 {
        let bodies = 2 + rng.next() % 60;
        let count = rng.next() % 200;
        let cs: Vec<_> = (0..count)
            .map(|_| {
                let a = rng.next() % bodies;
                // Bias toward chains so colours are reused.
                let b = if rng.next().is_multiple_of(2) {
                    (a + 1) % bodies
                } else {
                    rng.next() % bodies
                };
                if b == a { single(a) } else { pair(a, b) }
            })
            .collect();
        let mut c = Colorer::new(bodies as usize, cs.len());
        match c.color(&cs) {
            Ok(()) => {}
            Err(ColoringError::TooManyColors { .. }) => continue,
            Err(e) => panic!("round {round}: {e}"),
        }
        assert!(islands_are_disjoint(&c, &cs), "round {round}");
        // Reversing the input permutes indices but not which (unordered) body
        // pairs share a colour; duplicates of one pair are interchangeable.
        let reversed: Vec<_> = cs.iter().rev().copied().collect();
        let mut r = Colorer::new(bodies as usize, cs.len());
        r.color(&reversed).unwrap();
        let mut sets_a: Vec<Vec<(u32, u32)>> = c
            .layers()
            .map(|l| {
                let mut v: Vec<_> = l.iter().map(|&i| key(cs[i as usize])).collect();
                v.sort();
                v
            })
            .collect();
        let mut sets_b: Vec<Vec<(u32, u32)>> = r
            .layers()
            .map(|l| {
                let mut v: Vec<_> = l.iter().map(|&i| key(reversed[i as usize])).collect();
                v.sort();
                v
            })
            .collect();
        sets_a.sort();
        sets_b.sort();
        assert_eq!(sets_a, sets_b, "round {round}");
    }
}

#[test]
fn malformed_and_over_budget_inputs_are_typed_errors() {
    let mut c = Colorer::new(3, 4);
    assert_eq!(
        c.color(&[pair(0, 3)]),
        Err(ColoringError::BodyOutOfRange {
            constraint: 0,
            body: 3
        })
    );
    assert_eq!(
        c.color(&[pair(1, 1)]),
        Err(ColoringError::SelfConstraint {
            constraint: 0,
            body: 1
        })
    );
    assert_eq!(
        c.color(&[single(0); 5]),
        Err(ColoringError::CapacityExceeded {
            constraints: 5,
            capacity: 4
        })
    );
    // A star: every constraint touches body 0, so it needs one colour each.
    let star: Vec<_> = (1..=MAX_COLORS as u32 + 1).map(|i| pair(0, i)).collect();
    let mut s = Colorer::new(MAX_COLORS + 2, star.len());
    assert!(matches!(
        s.color(&star),
        Err(ColoringError::TooManyColors { .. })
    ));
    s.color(&star[..MAX_COLORS]).unwrap();
    assert_eq!(s.layer_count(), MAX_COLORS);
}
