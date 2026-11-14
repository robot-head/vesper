//! The device sort-and-sweep broadphase against vesper-kinetic's CPU LBVH and
//! brute-force pair sets, bit for bit, plus determinism and typed errors.
use vesper_gpu_compute::broadphase::{BroadphaseError, GpuBroadphase, order_key};
use vesper_gpu_compute::test_context;
use vesper_kinetic::lbvh::{Aabb, Lbvh, LbvhError, brute_force_pairs};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform in [lo, hi).
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        let t = (self.next() >> 40) as f32 / (1u64 << 24) as f32;
        lo + (hi - lo) * t
    }
}

fn scene(seed: u64, n: usize, extent: f32, max_half: f32) -> Vec<Aabb> {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    (0..n)
        .map(|_| {
            let c = [
                r.range(-extent, extent),
                r.range(-extent, extent),
                r.range(-extent, extent),
            ];
            let h = [
                r.range(0.0, max_half),
                r.range(0.0, max_half),
                r.range(0.0, max_half),
            ];
            Aabb {
                min: [c[0] - h[0], c[1] - h[1], c[2] - h[2]],
                max: [c[0] + h[0], c[1] + h[1], c[2] + h[2]],
            }
        })
        .collect()
}

fn cpu_lbvh(boxes: &[Aabb]) -> Vec<(u32, u32)> {
    let mut tree = Lbvh::with_capacity(boxes.len());
    tree.build(boxes).unwrap();
    let mut out = Vec::new();
    tree.overlapping_pairs(&mut out, usize::MAX).unwrap();
    out
}

#[test]
fn order_key_preserves_the_float_order() {
    let mut values = vec![
        f32::MIN,
        -1.0e30,
        -1.0,
        -f32::MIN_POSITIVE,
        -1.0e-45,
        -0.0,
        0.0,
        1.0e-45,
        f32::MIN_POSITIVE,
        0.5,
        1.0,
        f32::MAX,
    ];
    let mut r = Rng(7);
    for _ in 0..10_000 {
        values.push(f32::from_bits(r.next() as u32));
    }
    values.retain(|v| v.is_finite());
    for a in &values {
        for b in values.iter().take(64) {
            assert_eq!(a <= b, order_key(*a) <= order_key(*b), "{a} vs {b}");
        }
    }
}

#[test]
fn random_scenes_give_exactly_the_cpu_pair_set() {
    let Some(ctx) = test_context() else { return };
    let mut gpu = GpuBroadphase::new(&ctx, 4096, 1 << 20).unwrap();
    let mut out = Vec::new();
    let cases = [
        (1u64, 2usize, 1.0f32, 1.0f32),
        (2, 3, 1.0, 1.0),
        (11, 2, 0.5, 1.0),
        (12, 33, 1.0, 1.0),
        (3, 17, 10.0, 2.0),
        (4, 64, 10.0, 1.0),
        (5, 65, 10.0, 1.0),
        (6, 500, 50.0, 2.0),
        (13, 700, 4.0, 1.0),
        (7, 1000, 20.0, 1.0),
        (8, 2047, 100.0, 3.0),
        (9, 4096, 100.0, 2.0),
        (10, 4096, 5.0, 0.5),
    ];
    for (seed, n, extent, half) in cases {
        let boxes = scene(seed, n, extent, half);
        gpu.overlapping_pairs(&boxes, &mut out).unwrap();
        let lbvh = cpu_lbvh(&boxes);
        if n <= 2048 {
            assert_eq!(
                lbvh,
                brute_force_pairs(&boxes),
                "CPU LBVH vs brute force, n = {n}"
            );
        }
        assert_eq!(out, lbvh, "seed {seed}, n = {n}");
        eprintln!("n = {n}: {} pairs, identical", out.len());
    }
}

#[test]
fn degenerate_boxes_give_exactly_the_cpu_pair_set() {
    let Some(ctx) = test_context() else { return };
    let mut gpu = GpuBroadphase::new(&ctx, 512, 1 << 18).unwrap();
    let mut out = Vec::new();
    let unit = Aabb {
        min: [0.0; 3],
        max: [1.0; 3],
    };
    // Every box identical: n (n - 1) / 2 pairs, all with equal sort keys.
    let same = vec![unit; 300];
    gpu.overlapping_pairs(&same, &mut out).unwrap();
    assert_eq!(out.len(), 300 * 299 / 2);
    assert_eq!(out, brute_force_pairs(&same));
    // Boxes that only touch (closed intervals), on every axis, plus -0 / +0
    // and subnormal bounds that must compare exactly.
    let tiny = f32::from_bits(1);
    let mut edge = Vec::new();
    for i in 0..40 {
        let x = i as f32;
        edge.push(Aabb {
            min: [x, 0.0, -0.0],
            max: [x + 1.0, 1.0, 1.0],
        });
        edge.push(Aabb {
            min: [x + 0.5, 1.0, 1.0],
            max: [x + 0.75, 2.0, 2.0],
        });
        edge.push(Aabb {
            min: [-0.0, -tiny, 0.0],
            max: [0.0, -0.0, tiny],
        });
        edge.push(Aabb {
            min: [tiny, tiny * 2.0, -tiny],
            max: [tiny * 3.0, tiny * 2.0, 0.0],
        });
    }
    // Points (zero extent) and huge coordinates.
    edge.push(Aabb {
        min: [3.0e38, -3.0e38, 0.0],
        max: [3.0e38, 3.0e38, 0.0],
    });
    edge.push(Aabb {
        min: [-3.0e38; 3],
        max: [3.0e38; 3],
    });
    gpu.overlapping_pairs(&edge, &mut out).unwrap();
    assert_eq!(out, brute_force_pairs(&edge));
    assert_eq!(out, cpu_lbvh(&edge));
    // Zero and one box.
    gpu.overlapping_pairs(&[], &mut out).unwrap();
    assert!(out.is_empty());
    gpu.overlapping_pairs(&[unit], &mut out).unwrap();
    assert!(out.is_empty());
}

#[test]
fn sphere_bodies_like_the_world_broadphase() {
    let Some(ctx) = test_context() else { return };
    let mut gpu = GpuBroadphase::new(&ctx, 3000, 1 << 18).unwrap();
    let mut r = Rng(99);
    let boxes: Vec<Aabb> = (0..3000)
        .map(|_| {
            Aabb::around(
                [
                    r.range(-30.0, 30.0),
                    r.range(0.0, 10.0),
                    r.range(-30.0, 30.0),
                ],
                r.range(0.1, 0.6),
            )
        })
        .collect();
    let mut out = Vec::new();
    gpu.overlapping_pairs(&boxes, &mut out).unwrap();
    assert_eq!(out, cpu_lbvh(&boxes));
}

#[test]
fn device_output_is_deterministic_across_runs_and_contexts() {
    let Some(ctx) = test_context() else { return };
    let boxes = scene(42, 3000, 30.0, 1.5);
    let mut first = None;
    for round in 0..3 {
        let ctx = if round == 2 {
            vesper_gpu_compute::Context::new().unwrap()
        } else {
            ctx.clone()
        };
        let mut gpu = GpuBroadphase::new(&ctx, 3000, 1 << 18).unwrap();
        for _ in 0..3 {
            let mut out = Vec::new();
            gpu.overlapping_pairs(&boxes, &mut out).unwrap();
            let raw = (
                gpu.device_order().to_vec(),
                gpu.sorted_keys(3000).unwrap(),
                out,
            );
            match &first {
                None => first = Some(raw),
                Some(f) => assert_eq!(f, &raw),
            }
        }
    }
    // The device sort is the unique ascending (key, index) order.
    let (_, keys, _) = first.unwrap();
    let mut expected: Vec<(u32, u32)> = boxes
        .iter()
        .enumerate()
        .map(|(i, b)| (order_key(b.min[0]), i as u32))
        .collect();
    expected.sort_unstable();
    assert_eq!(keys, expected);
}

#[test]
fn invalid_inputs_are_the_cpu_broadphase_errors() {
    let Some(ctx) = test_context() else { return };
    let mut gpu = GpuBroadphase::new(&ctx, 8, 4).unwrap();
    let mut out = vec![(9, 9)];
    let unit = Aabb {
        min: [0.0; 3],
        max: [1.0; 3],
    };
    let mut bad = vec![unit; 4];
    bad[2].max[1] = f32::NAN;
    assert_eq!(
        gpu.overlapping_pairs(&bad, &mut out),
        Err(BroadphaseError::Kinetic(LbvhError::InvalidBox { index: 2 }))
    );
    assert!(out.is_empty());
    bad[2] = Aabb {
        min: [2.0, 0.0, 0.0],
        max: [1.0, 1.0, 1.0],
    };
    assert_eq!(
        gpu.overlapping_pairs(&bad, &mut out),
        Err(BroadphaseError::Kinetic(LbvhError::InvalidBox { index: 2 }))
    );
    bad[2].min[0] = f32::NEG_INFINITY;
    assert!(gpu.overlapping_pairs(&bad, &mut out).is_err());
    assert_eq!(
        gpu.overlapping_pairs(&[unit; 9], &mut out),
        Err(BroadphaseError::Kinetic(LbvhError::CapacityExceeded {
            primitives: 9,
            capacity: 8
        }))
    );
    // Four identical boxes overlap in six pairs; the capacity is four.
    assert_eq!(
        gpu.overlapping_pairs(&[unit; 4], &mut out),
        Err(BroadphaseError::Kinetic(LbvhError::PairOverflow {
            capacity: 4
        }))
    );
    assert!(out.is_empty());
    // Still usable afterwards.
    gpu.overlapping_pairs(&[unit; 3], &mut out).unwrap();
    assert_eq!(out, vec![(0, 1), (0, 2), (1, 2)]);
    assert!(GpuBroadphase::new(&ctx, 0, 4).is_err());
    assert!(GpuBroadphase::new(&ctx, (1 << 16) + 1, 4).is_err());
}
