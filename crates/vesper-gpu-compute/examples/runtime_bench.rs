//! One timed run of the runtime workload, for the ABBA comparison of the Rust
//! build and the Vesper build (feature `vesper-port`) that
//! `crates/vesper-gpu-compute/port/abba.py` drives. Prints the build, the
//! elapsed nanoseconds and a digest of the results, so both builds are seen
//! to compute the same thing.
//!
//! The workload is what the runtime itself does per frame: open a device,
//! compile one kernel, then `ROUNDS` times write a buffer, record and submit
//! `DISPATCHES` dispatches, wait, and read the result back.
use std::time::Instant;

use vesper_gpu_compute::Context;

const ADD: &str = "
struct P { n: u32, add: u32 }
var<immediate> p: P;
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@compute @workgroup_size(64)
fn add(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x < p.n { dst[id.x] = src[id.x] + p.add; }
}
";
const N: u32 = 1 << 12;
const ROUNDS: u32 = 200;
const DISPATCHES: u32 = 4;

fn main() {
    let start = Instant::now();
    let ctx = Context::new().expect("a Vulkan device");
    let kernel = ctx.kernel(ADD, "add", 2, 8).unwrap();
    let mut a = ctx.buffer((N * 4) as u64).unwrap();
    let b = ctx.buffer((N * 4) as u64).unwrap();
    let mut digest = 0u64;
    let input: Vec<u32> = (0..N).collect();
    let mut out = vec![0u32; N as usize];
    for round in 0..ROUNDS {
        a.write(0, &input).unwrap();
        let mut rec = ctx.record().unwrap();
        for _ in 0..DISPATCHES {
            rec.dispatch(&kernel, &[&a, &b], &[N, round], [N / 64, 1, 1])
                .unwrap();
        }
        rec.submit().unwrap().wait().unwrap();
        b.read(0, &mut out).unwrap();
        digest = out
            .iter()
            .fold(digest, |h, v| h.rotate_left(5) ^ u64::from(*v));
    }
    let build = if cfg!(feature = "vesper-port") {
        "vesper"
    } else {
        "rust"
    };
    println!("{build} {} {digest:016x}", start.elapsed().as_nanos());
}
