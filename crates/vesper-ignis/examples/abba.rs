//! ABBA comparison of the Rust build (baseline) and the Vesper port
//! (candidate). The two builds cannot share a binary (the `vesper-port`
//! feature swaps the engine behind one `RenderGraph`), so this example is
//! built twice and, in `compare` mode, runs the two executables as separate
//! processes in Rust, port, port, Rust order per epoch, then analyzes the
//! paired totals with vesper-bench. Run `aspect crate ignis bench-abba`.
//!
//! `abba measure <ops>` warms each workload up and prints its total
//! nanoseconds over `<ops>` operations; `abba profile <workload> <passes>
//! <ops>` runs one workload alone. `abba digest` prints a hash of each
//! graph's plan and lowering, which must be equal in the two builds. `abba compare <rust> <port> <epochs>
//! <ops>` drives the two builds.
//!
//! The graph: 8 textures with 4 mips and one buffer on a direct and a
//! compute queue. Pass `i` writes (even `i`) or reads (odd `i`) mip
//! `(i / 8) % 4` of texture `i % 8`, and reads the buffer; every fourth pass
//! runs on the compute queue, and every sixteenth one writes the buffer.
//! Workloads, each at 64 and 256 passes:
//! - `build`: build the graph (queues, resources, passes).
//! - `plan`: `RenderGraph::plan` of a graph built in setup.
//! - `lower`: `lower_plan(Vulkan14)` of a plan made in setup.
use std::{process::Command, time::Instant};
use vesper_bench::{PairedSample, analyze_pairs};
use vesper_ignis::multi_backend::{GpuApiTarget, lower_plan};
use vesper_ignis::*;

const TEXTURES: u32 = 8;
const MIPS: u32 = 4;

/// The benchmark graph with `passes` passes.
fn graph(passes: u32) -> RenderGraph {
    let mut g = RenderGraph::new();
    let direct = g.add_queue(QueueKind::Direct);
    let compute = g.add_queue(QueueKind::Compute);
    let textures: Vec<TextureHandle> = (0..TEXTURES)
        .map(|t| {
            g.add_texture(TextureDesc::simple(format!("t{t}"), Layout::Common).with_mips(MIPS))
                .unwrap()
        })
        .collect();
    let buffer = g
        .add_buffer(BufferDesc {
            name: "lights".into(),
            size: 1 << 16,
        })
        .unwrap();
    for i in 0..passes {
        let on_compute = i % 4 == 3;
        let queue = if on_compute { compute } else { direct };
        let shading = if on_compute {
            Sync::COMPUTE_SHADING
        } else {
            Sync::PIXEL_SHADING
        };
        let texture = textures[(i % TEXTURES) as usize];
        let mip = SubresourceRange::single((i / TEXTURES) % MIPS, 0, 0);
        let (access, sync) = if i % 2 == 0 {
            (Access::UNORDERED_ACCESS, Sync::COMPUTE_SHADING)
        } else {
            (Access::SHADER_RESOURCE, shading)
        };
        let (buffer_access, buffer_sync) = if i % 16 == 0 {
            (Access::UNORDERED_ACCESS, Sync::COMPUTE_SHADING)
        } else {
            (Access::SHADER_RESOURCE, shading)
        };
        g.add_pass(
            PassDesc::new(format!("p{i}"), queue)
                .texture(texture, mip, access, sync, None)
                .buffer(buffer, ByteRange::WHOLE, buffer_access, buffer_sync),
        )
        .unwrap();
    }
    g
}

const WORKLOADS: [(&str, u32); 6] = [
    ("build", 64),
    ("build", 256),
    ("plan", 64),
    ("plan", 256),
    ("lower", 64),
    ("lower", 256),
];

/// Times `ops` calls of `once` after `ops / 4` warm-up ones.
fn time(ops: u64, mut once: impl FnMut() -> usize) -> u128 {
    let mut sink = 0;
    for _ in 0..ops / 4 {
        sink += once();
    }
    let start = Instant::now();
    for _ in 0..ops {
        sink += once();
    }
    let elapsed = start.elapsed().as_nanos();
    std::hint::black_box(sink);
    elapsed
}

/// Total nanoseconds for `ops` timed operations of one workload. Inputs are
/// built before the clock starts.
fn measure(name: &str, passes: u32, ops: u64) -> u128 {
    match name {
        "build" => time(ops, || graph(passes).pass_count()),
        "plan" => {
            let g = graph(passes);
            time(ops, || g.plan().unwrap().barrier_count())
        }
        "lower" => {
            let plan = graph(passes).plan().unwrap();
            time(ops, || {
                lower_plan(GpuApiTarget::Vulkan14, &plan).barrier_count()
            })
        }
        _ => unreachable!(),
    }
}

/// Ops per run, scaled so the 256-pass workloads take about as long as the
/// 64-pass ones.
fn ops_for(passes: u32, ops: u64) -> u64 {
    (ops * 64 / passes as u64).max(4)
}

fn run(exe: &str, ops: u64) -> Vec<u64> {
    let out = Command::new(exe)
        .args(["measure", &ops.to_string()])
        .output()
        .expect("run a build");
    assert!(out.status.success(), "{exe} failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.split_whitespace().nth(2).unwrap().parse().unwrap())
        .collect()
}

fn compare(rust: &str, port: &str, epochs: usize, ops: u64) {
    let mut pairs = vec![Vec::new(); WORKLOADS.len()];
    for _ in 0..epochs {
        let a1 = run(rust, ops);
        let b1 = run(port, ops);
        let b2 = run(port, ops);
        let a2 = run(rust, ops);
        for (w, pair) in pairs.iter_mut().enumerate() {
            let n = ops_for(WORKLOADS[w].1, ops);
            pair.push(PairedSample::new(a1[w] + a2[w], b1[w] + b2[w], 2 * n).unwrap());
        }
    }
    for ((name, passes), pairs) in WORKLOADS.iter().zip(&pairs) {
        let n = ops_for(*passes, ops);
        let per_op = |t: u64| t as f64 / (2 * n) as f64;
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let rust_ns = median(pairs.iter().map(|p| per_op(p.baseline_total())).collect());
        let port_ns = median(pairs.iter().map(|p| per_op(p.candidate_total())).collect());
        let ratios: Vec<f64> = pairs
            .iter()
            .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
            .collect();
        let lo = ratios.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = ratios.iter().copied().fold(0.0, f64::max);
        let ratio = median(ratios);
        let analysis = analyze_pairs(pairs, 0).unwrap();
        let x = |s: PairedSample| s.candidate_total() as f64 / s.baseline_total() as f64;
        let interval = match analysis.interval {
            Some(i) => format!("95% {:.2}x..{:.2}x", x(i.lower), x(i.upper)),
            None => "95% interval needs 6+ epochs".into(),
        };
        println!(
            "{name:<5} {passes:>3} passes  rust {rust_ns:>10.0} ns/op  port {port_ns:>10.0} ns/op  \
             port/rust {ratio:.2}x ({interval}, range {lo:.2}x..{hi:.2}x, {epochs} epochs)",
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["measure", ops] => {
            let ops: u64 = ops.parse().unwrap();
            for (name, passes) in WORKLOADS {
                let n = ops_for(passes, ops);
                println!("{name} {passes} {}", measure(name, passes, n));
            }
        }
        ["profile", name, passes, ops] => {
            // One workload alone, for callgrind or perf.
            let ns = measure(name, passes.parse().unwrap(), ops.parse().unwrap());
            println!("{name} {passes} {ns}");
        }
        ["digest"] => {
            for passes in [64, 256] {
                let g = graph(passes);
                let plan = g.plan().unwrap();
                let text = format!(
                    "{}{:?}{:?}",
                    plan.render(&g),
                    plan.dependencies,
                    lower_plan(GpuApiTarget::Vulkan14, &plan)
                );
                // FNV-1a, so the two builds print comparable lines.
                let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                    (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
                });
                println!("{passes} {hash:016x} {}", plan.barrier_count());
            }
        }
        ["compare", rust, port, epochs, ops] => {
            compare(rust, port, epochs.parse().unwrap(), ops.parse().unwrap())
        }
        _ => {
            eprintln!(
                "usage: abba measure <ops> | abba profile <workload> <passes> <ops> | abba digest | abba compare <rust> <port> <epochs> <ops>"
            );
            std::process::exit(2);
        }
    }
}
