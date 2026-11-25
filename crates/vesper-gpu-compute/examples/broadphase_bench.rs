//! One timed run of the broadphase workload, for the ABBA comparison that
//! `aspect crate gpu-compute bench-abba -- broadphase` drives against the Rust broadphase at its
//! last commit. Prints a label, the elapsed nanoseconds and a digest of the
//! pair sets, so both arms are seen to compute the same thing.
//!
//! The workload: open a device, build a broadphase for 4,096 boxes, then
//! `ROUNDS` times move every box and find the overlapping pairs.
use std::time::Instant;

use vesper_gpu_compute::Context;
use vesper_gpu_compute::broadphase::GpuBroadphase;
use vesper_kinetic::lbvh::Aabb;

const N: usize = 4096;
const ROUNDS: u32 = 40;

fn main() {
    let start = Instant::now();
    let ctx = Context::new().expect("a Vulkan device");
    let mut bp = GpuBroadphase::new(&ctx, N, 1 << 20).unwrap();
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / (1u64 << 24) as f32
    };
    let mut centers: Vec<[f32; 3]> = (0..N)
        .map(|_| [next() * 64.0, next() * 64.0, next() * 64.0])
        .collect();
    let mut boxes = Vec::with_capacity(N);
    let mut pairs = Vec::new();
    let mut digest = 0u64;
    for round in 0..ROUNDS {
        boxes.clear();
        for c in &mut centers {
            c[0] += 0.05 * (round % 7) as f32;
            boxes.push(Aabb {
                min: [c[0] - 0.6, c[1] - 0.6, c[2] - 0.6],
                max: [c[0] + 0.6, c[1] + 0.6, c[2] + 0.6],
            });
        }
        bp.overlapping_pairs(&boxes, &mut pairs).unwrap();
        digest = pairs.iter().fold(digest, |h, &(a, b)| {
            h.rotate_left(5) ^ (u64::from(a) << 32 | u64::from(b))
        });
    }
    println!("vesper {} {digest:016x}", start.elapsed().as_nanos());
}
