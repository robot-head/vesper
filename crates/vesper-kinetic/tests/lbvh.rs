//! LBVH broadphase against the brute-force pair set, 64-bit Morton codes and
//! typed errors.

#![allow(clippy::needless_range_loop)]

use vesper_kinetic::lbvh::{
    Aabb, Child, Lbvh, LbvhError, MORTON64_AXIS_MAX, brute_force_pairs, demorton64, morton64,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[test]
fn morton64_round_trips_and_orders_by_interleaved_bits() {
    let mut rng = Rng(0xA5A5_1234_5678_9ABC);
    for _ in 0..10_000 {
        let x = (rng.next() as u32) & MORTON64_AXIS_MAX;
        let y = (rng.next() as u32) & MORTON64_AXIS_MAX;
        let z = (rng.next() as u32) & MORTON64_AXIS_MAX;
        let code = morton64(x, y, z);
        assert_eq!(demorton64(code), [x, y, z]);
        assert!(code < 1 << 63);
        // Bit-by-bit reference.
        let mut spec = 0u64;
        for bit in 0..21 {
            spec |= u64::from((x >> bit) & 1) << (3 * bit);
            spec |= u64::from((y >> bit) & 1) << (3 * bit + 1);
            spec |= u64::from((z >> bit) & 1) << (3 * bit + 2);
        }
        assert_eq!(code, spec);
    }
    assert_eq!(
        morton64(MORTON64_AXIS_MAX, MORTON64_AXIS_MAX, MORTON64_AXIS_MAX),
        (1 << 63) - 1
    );
}

fn random_boxes(rng: &mut Rng, n: usize, world: f32, size: f32) -> Vec<Aabb> {
    (0..n)
        .map(|_| {
            let c = [rng.unit() * world, rng.unit() * world, rng.unit() * world];
            let h = [rng.unit() * size, rng.unit() * size, rng.unit() * size];
            Aabb {
                min: [c[0] - h[0], c[1] - h[1], c[2] - h[2]],
                max: [c[0] + h[0], c[1] + h[1], c[2] + h[2]],
            }
        })
        .collect()
}

/// Every leaf is reachable exactly once and every internal node exactly
/// once from the root (the Karras construction gives a binary tree).
fn check_tree_shape(tree: &Lbvh) {
    let n = tree.len();
    if n < 2 {
        assert!(tree.nodes().is_empty());
        return;
    }
    let mut leaf_seen = vec![0u32; n];
    let mut node_seen = vec![0u32; n - 1];
    let mut stack = vec![Child::Internal(0)];
    while let Some(c) = stack.pop() {
        match c {
            Child::Leaf(i) => leaf_seen[i as usize] += 1,
            Child::Internal(k) => {
                node_seen[k as usize] += 1;
                let node = tree.nodes()[k as usize];
                stack.push(node.left);
                stack.push(node.right);
            }
        }
    }
    assert!(leaf_seen.iter().all(|c| *c == 1), "leaves {leaf_seen:?}");
    assert!(node_seen.iter().all(|c| *c == 1), "nodes {node_seen:?}");
}

#[test]
fn overlapping_pairs_equal_the_brute_force_set() {
    let mut rng = Rng(0x0DDB_A11C_0FFE_E123);
    let mut tree = Lbvh::with_capacity(600);
    let mut pairs = Vec::with_capacity(20_000);
    for (round, &(n, world, size)) in [
        (0usize, 10.0f32, 1.0f32),
        (1, 10.0, 1.0),
        (2, 10.0, 1.0),
        (17, 10.0, 1.0),
        (64, 4.0, 1.0),
        (300, 20.0, 0.7),
        (600, 30.0, 1.5),
        (600, 2.0, 0.05),
    ]
    .iter()
    .enumerate()
    {
        let boxes = random_boxes(&mut rng, n, world, size);
        tree.build(&boxes).unwrap();
        check_tree_shape(&tree);
        tree.overlapping_pairs(&mut pairs, 20_000).unwrap();
        let reference = brute_force_pairs(&boxes);
        assert_eq!(pairs, reference, "round {round}: n={n}");
        if n > 100 {
            assert!(!reference.is_empty(), "round {round} exercises overlaps");
        }
    }
}

#[test]
fn duplicate_codes_and_identical_boxes_are_ordered_by_index() {
    // 40 identical boxes (all codes equal) plus a far one.
    let mut boxes = vec![
        Aabb {
            min: [0.0; 3],
            max: [1.0; 3],
        };
        40
    ];
    boxes.push(Aabb {
        min: [100.0; 3],
        max: [101.0; 3],
    });
    let mut tree = Lbvh::with_capacity(boxes.len());
    tree.build(&boxes).unwrap();
    check_tree_shape(&tree);
    let order: Vec<u32> = tree.keys().iter().map(|k| k.1).collect();
    assert_eq!(order, (0..41).collect::<Vec<_>>());
    let mut pairs = Vec::new();
    tree.overlapping_pairs(&mut pairs, 10_000).unwrap();
    assert_eq!(pairs, brute_force_pairs(&boxes));
    assert_eq!(pairs.len(), 40 * 39 / 2);
    // Building twice gives the same tree.
    let first = tree.nodes().to_vec();
    tree.build(&boxes).unwrap();
    assert_eq!(tree.nodes(), first.as_slice());
}

#[test]
fn malformed_inputs_are_typed_errors() {
    let mut tree = Lbvh::with_capacity(2);
    let good = Aabb {
        min: [0.0; 3],
        max: [1.0; 3],
    };
    assert_eq!(
        tree.build(&[good, good, good]),
        Err(LbvhError::CapacityExceeded {
            primitives: 3,
            capacity: 2
        })
    );
    let nan = Aabb {
        min: [f32::NAN, 0.0, 0.0],
        max: [1.0; 3],
    };
    assert_eq!(
        tree.build(&[good, nan]),
        Err(LbvhError::InvalidBox { index: 1 })
    );
    let inverted = Aabb {
        min: [2.0; 3],
        max: [1.0; 3],
    };
    assert_eq!(
        tree.build(&[inverted]),
        Err(LbvhError::InvalidBox { index: 0 })
    );
    tree.build(&[good, good]).unwrap();
    let mut pairs = Vec::new();
    assert_eq!(
        tree.overlapping_pairs(&mut pairs, 0),
        Err(LbvhError::PairOverflow { capacity: 0 })
    );
    assert!(pairs.is_empty());
}
