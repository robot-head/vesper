//! The data-parallel sort-and-sweep broadphase against brute force and the
//! LBVH, its sort order, and typed errors.

use vesper_kinetic::lbvh::{Aabb, Lbvh, brute_force_pairs};
use vesper_kinetic::sap::{SapError, SweepAndPrune, WORKGROUP, sortable};

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

fn random_boxes(rng: &mut Rng, n: usize, extent: [f32; 3], size: f32) -> Vec<Aabb> {
    (0..n)
        .map(|_| {
            let c = [
                rng.unit() * extent[0] - extent[0] * 0.5,
                rng.unit() * extent[1] - extent[1] * 0.5,
                rng.unit() * extent[2] - extent[2] * 0.5,
            ];
            let h = [rng.unit() * size, rng.unit() * size, rng.unit() * size];
            Aabb {
                min: [c[0] - h[0], c[1] - h[1], c[2] - h[2]],
                max: [c[0] + h[0], c[1] + h[1], c[2] + h[2]],
            }
        })
        .collect()
}

#[test]
fn sortable_keys_preserve_float_order() {
    let mut values = vec![
        f32::NEG_INFINITY,
        -f32::MAX,
        -1.0,
        -f32::MIN_POSITIVE,
        -1e-45,
        -0.0,
        0.0,
        1e-45,
        f32::MIN_POSITIVE,
        1.0,
        f32::MAX,
        f32::INFINITY,
    ];
    let mut rng = Rng(7);
    for _ in 0..2000 {
        values.push(f32::from_bits(rng.next() as u32 & 0xff7f_ffff));
    }
    values.retain(|v| !v.is_nan());
    for a in &values {
        for b in values.iter().take(200) {
            if a < b {
                assert!(sortable(*a) < sortable(*b), "{a} {b}");
            }
            if a == b && a.to_bits() == b.to_bits() {
                assert_eq!(sortable(*a), sortable(*b));
            }
        }
    }
    assert!(sortable(-0.0) < sortable(0.0));
}

#[test]
fn overlapping_pairs_equal_the_brute_force_set() {
    let mut rng = Rng(0x5A9_0123_4567_89AB);
    let mut sap = SweepAndPrune::with_capacity(3000);
    let mut pairs = Vec::with_capacity(60_000);
    for (round, &(n, extent, size)) in [
        (0usize, [10.0f32, 10.0, 10.0], 1.0f32),
        (1, [10.0, 10.0, 10.0], 1.0),
        (2, [1.0, 1.0, 1.0], 1.0),
        (17, [10.0, 10.0, 10.0], 1.0),
        (WORKGROUP - 1, [4.0, 4.0, 4.0], 0.5),
        (WORKGROUP, [30.0, 2.0, 2.0], 0.5),
        (WORKGROUP + 1, [2.0, 30.0, 2.0], 0.5),
        (1000, [2.0, 2.0, 40.0], 0.3),
        (3000, [60.0, 60.0, 60.0], 1.5),
        (3000, [3.0, 3.0, 3.0], 0.05),
    ]
    .iter()
    .enumerate()
    {
        let boxes = random_boxes(&mut rng, n, extent, size);
        sap.build(&boxes).unwrap();
        // The sweep order is sorted by (lower bound on the axis, index).
        let axis = sap.axis();
        for w in sap.order().windows(2) {
            let (a, b) = (w[0] as usize, w[1] as usize);
            let (ka, kb) = (sortable(boxes[a].min[axis]), sortable(boxes[b].min[axis]));
            assert!(ka < kb || (ka == kb && a < b), "round {round}: order");
        }
        sap.overlapping_pairs(&mut pairs, 60_000).unwrap();
        let reference = brute_force_pairs(&boxes);
        assert_eq!(pairs, reference, "round {round}: n={n}");
        if n > 100 {
            assert!(!reference.is_empty(), "round {round} exercises overlaps");
        }
        if n >= 1000 {
            // The longest extent is chosen as the sweep axis.
            let widest = (0..3).fold(0, |b, a| if extent[a] > extent[b] { a } else { b });
            if extent.iter().filter(|e| **e == extent[widest]).count() == 1 {
                assert_eq!(axis, widest, "round {round}");
            }
        }
    }
}

/// Past 65,536 boxes the pair keys need all eight radix passes.
#[test]
fn large_sparse_sets_match_the_lbvh() {
    let mut rng = Rng(0xFEED_BEEF);
    let n = 70_000;
    let boxes = random_boxes(&mut rng, n, [400.0, 400.0, 400.0], 1.5);
    let mut sap = SweepAndPrune::with_capacity(n);
    let mut tree = Lbvh::with_capacity(n);
    let (mut a, mut b) = (Vec::new(), Vec::new());
    sap.build(&boxes).unwrap();
    sap.overlapping_pairs(&mut a, 1 << 20).unwrap();
    tree.build(&boxes).unwrap();
    tree.overlapping_pairs(&mut b, 1 << 20).unwrap();
    assert!(a.len() > 1000, "{} pairs", a.len());
    assert!(a.iter().any(|p| p.1 > 65_536));
    assert_eq!(a, b);
}

#[test]
fn identical_boxes_are_ordered_by_index() {
    let mut boxes = vec![
        Aabb {
            min: [0.0; 3],
            max: [1.0; 3],
        };
        600
    ];
    boxes.push(Aabb {
        min: [100.0; 3],
        max: [101.0; 3],
    });
    let mut sap = SweepAndPrune::with_capacity(boxes.len());
    sap.build(&boxes).unwrap();
    assert_eq!(
        &sap.order()[..600],
        (0..600).collect::<Vec<u32>>().as_slice()
    );
    let mut pairs = Vec::new();
    sap.overlapping_pairs(&mut pairs, 1 << 20).unwrap();
    assert_eq!(pairs.len(), 600 * 599 / 2);
    assert_eq!(pairs, brute_force_pairs(&boxes));
}

#[test]
fn malformed_inputs_are_typed_errors() {
    let mut sap = SweepAndPrune::with_capacity(2);
    let good = Aabb {
        min: [0.0; 3],
        max: [1.0; 3],
    };
    assert_eq!(
        sap.build(&[good, good, good]),
        Err(SapError::CapacityExceeded {
            boxes: 3,
            capacity: 2
        })
    );
    let nan = Aabb {
        min: [0.0, f32::NAN, 0.0],
        max: [1.0; 3],
    };
    assert_eq!(
        sap.build(&[good, nan]),
        Err(SapError::InvalidBox { index: 1 })
    );
    let inf = Aabb {
        min: [0.0; 3],
        max: [f32::INFINITY, 1.0, 1.0],
    };
    assert_eq!(
        sap.build(&[inf, nan]),
        Err(SapError::InvalidBox { index: 0 })
    );
    let inverted = Aabb {
        min: [2.0; 3],
        max: [1.0; 3],
    };
    assert_eq!(
        sap.build(&[inverted]),
        Err(SapError::InvalidBox { index: 0 })
    );
    sap.build(&[good, good]).unwrap();
    let mut pairs = vec![(9, 9)];
    assert_eq!(
        sap.overlapping_pairs(&mut pairs, 0),
        Err(SapError::PairOverflow { capacity: 0 })
    );
    assert!(pairs.is_empty());
    sap.overlapping_pairs(&mut pairs, 1).unwrap();
    assert_eq!(pairs, vec![(0, 1)]);
}
