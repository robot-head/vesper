//! ABBA benchmark of the Resonance port (port wave P3b lane 9, #1071),
//! comparing the frozen Rust seed copy (`vesper-resonance-rust`) with this
//! package's build. Run it with the port:
//!
//! ```sh
//! cargo run --release -p vesper-resonance --features vesper-port --example abba
//! ```
//!
//! Both builds live in one binary, so the arms run in one process: A is the
//! Rust build and B this build, timed in windows A B B A, `ABBA_EPOCHS`
//! times (default 9). The report is each workload's median B/A ratio and
//! its range over the epochs. The workloads:
//!
//! - `kernel`: a 64-voice kernel block, four 16-lane `SvfBank`s rendering
//!   one 64-frame block each into a shared stereo bus, `ABBA_KERNEL_BLOCKS`
//!   blocks per window (default 20,000).
//! - `graph`: a 32-node delay-compensated graph (sources, latent mixes and
//!   gains in layers, with compensation delays) rendered by
//!   `pdc::render_builtin_graph`, 64-frame blocks, `ABBA_GRAPH_BLOCKS`
//!   blocks per call (default 2,000), `ABBA_GRAPH_CALLS` calls per window
//!   (default 10).
//!
//! - `simd`: one 16-voice tile and its 16 parameter ramps advanced sample by
//!   sample through `simd::ramp_tick` and `simd::svf_tick` on the widest
//!   path, `ABBA_SIMD_TICKS` samples per window (default 200,000).
//!
//! - `schedule`, `events`, `slab`, `pid`, `confine`: one workload per other
//!   ported module (`pdc::compile_schedule`, the event wire codec and
//!   ramps, `slab::DelaySlab`, `kernels::PidController`,
//!   `confine::verify_realtime`), scaled by `ABBA_MODULE_ITERS` (default
//!   2,000).
//!
//! `ABBA_WORKLOADS=a,b` runs only the named workloads.
//!
//! `ABBA_ONLY=rust|port` with `ABBA_WORKLOAD=kernel|graph|simd` runs one arm of
//! one workload once and nothing else (for `valgrind --tool=callgrind`).
use std::hint::black_box;
use std::time::Instant;
use vesper_resonance as port;
use vesper_resonance_rust as rust;

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

const FRAMES: usize = 64;
const BANKS: usize = 4;

fn input() -> Vec<f32> {
    (0..FRAMES)
        .map(|i| ((i as f32 * 0.07).sin() + if i % 50 == 0 { 0.8 } else { 0.0 }) * 0.5)
        .collect()
}

/// The 32-node graph: 8 sources, 8 latent mixes of source pairs with
/// latencies 0..56, 8 gains over the mixes, 6 gains over gain pairs, a
/// pre-sink sum and the sink. Paths of unequal latency meet at every mix, so
/// the compiler inserts compensation delays.
fn graph() -> Vec<(u32, u32, Vec<u32>)> {
    let mut g = Vec::new();
    for i in 0..8 {
        g.push((i, 0, vec![]));
    }
    for i in 0..8u32 {
        g.push((8 + i, i * 8, vec![i, (i + 1) % 8]));
    }
    for i in 0..8u32 {
        g.push((16 + i, 0, vec![8 + i, 8 + (i + 3) % 8]));
    }
    for i in 0..6u32 {
        g.push((24 + i, 0, vec![16 + i, 16 + i + 2]));
    }
    g.push((30, 0, (24..30).collect()));
    g.push((31, 0, vec![30, 23, 7]));
    g
}

/// 0 = source, 1 = latent mix, 2 = gain, by node.
fn kind(id: u32) -> u8 {
    match id {
        0..=7 => 0,
        8..=15 => 1,
        _ => 2,
    }
}

macro_rules! arm {
    ($m:ident, $lib:ident) => {
        mod $m {
            use super::*;
            use $lib::kernels::{LANES, SvfBank, VoicePatch, VoicePatchBank};
            use $lib::pdc::{BuiltinKind, Input, NodeId, NodeSpec};

            pub struct Kernel {
                banks: Vec<SvfBank>,
                input: Vec<f32>,
                left: Vec<f32>,
                right: Vec<f32>,
            }

            pub fn kernel() -> Kernel {
                let banks = (0..BANKS)
                    .map(|b| {
                        let p = |g: f32, k: f32, gain: f32| VoicePatch {
                            g,
                            k,
                            gain,
                            patch_id: 7,
                        };
                        let mut bank = SvfBank::new(p(0.0, 1.0, 0.0));
                        let mut patches = VoicePatchBank {
                            voices: [p(0.0, 0.0, 0.0); LANES],
                        };
                        for v in 0..LANES {
                            let voice = b * LANES + v;
                            let fc = 60.0 * 1.07f64.powi(voice as i32);
                            patches.voices[v] = p(
                                (std::f64::consts::PI * fc / 48_000.0).tan() as f32,
                                1.0 / (0.5 + v as f32 * 0.3),
                                1.0 / 64.0,
                            );
                        }
                        bank.apply_patch(&patches);
                        bank
                    })
                    .collect();
                Kernel {
                    banks,
                    input: input(),
                    left: vec![0.0; FRAMES],
                    right: vec![0.0; FRAMES],
                }
            }

            /// `blocks` 64-voice blocks; returns a checksum of the bus.
            pub fn render(k: &mut Kernel, blocks: usize) -> u64 {
                let mut sum = 0u64;
                for _ in 0..blocks {
                    k.left.fill(0.0);
                    k.right.fill(0.0);
                    for bank in &mut k.banks {
                        bank.render(&k.input, &mut k.left, &mut k.right, 0, FRAMES);
                    }
                    sum = sum.wrapping_add(u64::from(k.left[FRAMES - 1].to_bits()));
                }
                sum
            }

            pub struct Tiles {
                tile: $lib::kernels::SvfVoiceTile,
                pan: $lib::simd::PanLanes,
                out: $lib::simd::LaneOut,
                ramps: $lib::events::RampTile,
                path: $lib::simd::SvfLanes,
            }

            pub fn tiles() -> Tiles {
                let mut tile = $lib::kernels::SvfVoiceTile {
                    ic1eq: [0.0; LANES],
                    ic2eq: [0.0; LANES],
                    g_coeff: [0.0; LANES],
                    k_damping: [0.0; LANES],
                    gain: [1.0 / 16.0; LANES],
                    patch_id: [0; LANES],
                };
                let mut pan = $lib::simd::PanLanes {
                    left: [0.0; LANES],
                    right: [0.0; LANES],
                };
                let mut ramps = $lib::events::RampTile::splat(0.0);
                for v in 0..LANES {
                    let fc = 60.0 * 1.3f64.powi(v as i32);
                    tile.g_coeff[v] = (std::f64::consts::PI * fc / 48_000.0).tan() as f32;
                    tile.k_damping[v] = 1.0 / (0.5 + v as f32 * 0.3);
                    (pan.left[v], pan.right[v]) = SvfBank::pan(v);
                    ramps.set_lane(v, 1.0 + v as f32, 1 << (v + 4));
                }
                Tiles {
                    tile,
                    pan,
                    out: $lib::simd::LaneOut::ZERO,
                    ramps,
                    path: match std::env::var("ABBA_SIMD_PATH").as_deref() {
                        Ok("scalar") => $lib::simd::SvfLanes::Scalar,
                        Ok("sse2") => $lib::simd::SvfLanes::Sse2,
                        _ => $lib::simd::SvfLanes::best(),
                    },
                }
            }

            /// `ticks` samples of one 16-voice tile and its ramps on the
            /// widest path; returns a checksum.
            pub fn tick(t: &mut Tiles, ticks: usize) -> u64 {
                let input = input();
                let mut sum = 0u64;
                for i in 0..ticks {
                    $lib::simd::ramp_tick(t.path, &mut t.ramps);
                    $lib::simd::svf_tick(
                        t.path,
                        &mut t.tile,
                        &t.pan,
                        input[i % FRAMES],
                        &mut t.out,
                    );
                    let (l, _) = t.out.sum();
                    sum = sum.wrapping_add(
                        u64::from(l.to_bits()) ^ u64::from(t.ramps.current[3].to_bits()),
                    );
                }
                sum
            }

            /// `n` schedules of the 32-node graph (`pdc::compile_schedule`).
            pub fn schedule(g: &Graph, n: usize) -> u64 {
                let mut sum = 0u64;
                for _ in 0..n {
                    let s = $lib::pdc::compile_schedule(&g.nodes).unwrap();
                    sum = sum.wrapping_add(
                        (s.ops.len() + s.lines.len() + s.buffer_count) as u64
                            + s.ops[3].output_latency,
                    );
                }
                sum
            }

            /// `n` rounds of 32 events through the wire codec, then 16
            /// parameter ramps over a 64-sample block (`events`).
            pub fn events(n: usize) -> u64 {
                use $lib::events::{EVENT_WIRE_BYTES, ParamRamp, RtParamEvent, decode_events};
                let events: Vec<RtParamEvent> = (0..32u32)
                    .map(|i| RtParamEvent {
                        sample_offset: (i * 2) as u16,
                        node_id: (i % 4) as u16,
                        param_id: i % 3,
                        target: 0.25 * i as f32,
                        ramp_samples: i * 3,
                    })
                    .collect();
                let mut bytes = vec![0u8; events.len() * EVENT_WIRE_BYTES];
                let mut out = vec![events[0]; events.len()];
                let mut ramps = [ParamRamp::new(0.0); 16];
                let mut sum = 0u64;
                for _ in 0..n {
                    for (k, e) in events.iter().enumerate() {
                        bytes[k * EVENT_WIRE_BYTES..(k + 1) * EVENT_WIRE_BYTES]
                            .copy_from_slice(&e.encode());
                    }
                    let got = decode_events(&bytes, &mut out).unwrap();
                    for (k, e) in got.iter().enumerate() {
                        ramps[k % 16].set(e.target, e.ramp_samples);
                    }
                    for _ in 0..FRAMES {
                        for r in &mut ramps {
                            sum = sum.wrapping_add(u64::from(r.next_value().to_bits()));
                        }
                    }
                }
                sum
            }

            /// `n` blocks through eight delay lines of 0..56 samples
            /// (`slab::DelaySlab`).
            pub fn slab(n: usize) -> u64 {
                let delays: Vec<u32> = (0..8).map(|i| i * 8).collect();
                let mut s = $lib::slab::DelaySlab::new(&delays, FRAMES).unwrap();
                let input = input();
                let mut out = vec![0.0f32; FRAMES];
                let mut sum = 0u64;
                for _ in 0..n {
                    for line in 0..delays.len() {
                        s.write(line, &input);
                        s.read(line, &mut out);
                        sum = sum.wrapping_add(u64::from(out[FRAMES - 1].to_bits()));
                    }
                    s.advance_all(FRAMES);
                }
                sum
            }

            /// `n` 1 kHz PID steps (`kernels::PidController`).
            pub fn pid(n: usize) -> u64 {
                let mut c = $lib::kernels::PidController::new($lib::kernels::PidGains {
                    kp: 2.0,
                    ki: 0.5,
                    kd: 0.01,
                    dt: 0.001,
                    torque_limit: 10.0,
                    integral_limit: 2.0,
                })
                .unwrap();
                let mut x = 0.0f32;
                let mut sum = 0u64;
                for i in 0..n {
                    let u = c
                        .step(if (i / 500) % 2 == 0 { 1.0 } else { -1.0 }, x)
                        .unwrap();
                    x += 0.001 * u;
                    sum = sum.wrapping_add(u64::from(u.to_bits()));
                }
                sum
            }

            /// `n` verifications of a 64-function call graph with one
            /// realtime root (`confine::verify_realtime`).
            pub fn confine(n: usize) -> u64 {
                use $lib::confine::{
                    CallGraph, Effect, FnId, FnNode, RealtimePolicy, verify_realtime,
                };
                let mut g = CallGraph::default();
                for i in 0..64u32 {
                    let effects = match i % 9 {
                        0 => vec![Effect::Realtime],
                        4 => vec![Effect::Isa],
                        8 if i > 40 => vec![Effect::Alloc],
                        _ => vec![],
                    };
                    g.add(FnNode::new(format!("f{i}"), effects, 64 + u64::from(i)))
                        .unwrap();
                }
                for i in 0..63u32 {
                    g.call(FnId(i), FnId(i + 1));
                    if i % 3 == 0 && i + 5 < 64 {
                        g.call(FnId(i), FnId(i + 5));
                    }
                }
                let policy = RealtimePolicy::root(FnId(0));
                let mut sum = 0u64;
                for _ in 0..n {
                    let f = verify_realtime(&g, &policy).unwrap();
                    sum = sum.wrapping_add(
                        f.len() as u64 + f.iter().map(|x| x.message.len() as u64).sum::<u64>(),
                    );
                }
                sum
            }

            pub struct Graph {
                nodes: Vec<NodeSpec>,
                kinds: Vec<BuiltinKind>,
            }

            pub fn graph() -> Graph {
                let g = super::graph();
                Graph {
                    nodes: g
                        .iter()
                        .map(|(id, latency, inputs)| NodeSpec {
                            id: NodeId(*id),
                            latency: *latency,
                            inputs: inputs.iter().map(|&i| Input::Forward(NodeId(i))).collect(),
                        })
                        .collect(),
                    kinds: g
                        .iter()
                        .map(|(id, _, _)| match kind(*id) {
                            0 => BuiltinKind::Source(*id),
                            1 => BuiltinKind::Latent,
                            _ => BuiltinKind::Gain(0.5 + *id as f32 / 64.0),
                        })
                        .collect(),
                }
            }

            /// `calls` renders of `blocks` 64-frame blocks; returns a checksum.
            pub fn run(g: &Graph, calls: usize, blocks: usize) -> u64 {
                let mut sum = 0u64;
                for _ in 0..calls {
                    let out = $lib::pdc::render_builtin_graph(
                        &g.nodes,
                        &g.kinds,
                        FRAMES,
                        blocks,
                        NodeId(31),
                    )
                    .unwrap();
                    sum = sum.wrapping_add(u64::from(out[out.len() - 1].to_bits()));
                }
                sum
            }
        }
    };
}

arm!(r, rust);
arm!(p, port);

fn time(f: &mut dyn FnMut() -> u64) -> (f64, u64) {
    let t = Instant::now();
    let v = black_box(f());
    (t.elapsed().as_secs_f64(), v)
}

fn abba(name: &str, epochs: usize, a: &mut dyn FnMut() -> u64, b: &mut dyn FnMut() -> u64) {
    // Warm both arms and check they agree (bit-exact samples).
    let (va, vb) = (a(), b());
    assert_eq!(va, vb, "{name}: the builds' samples differ");
    let mut ratios = Vec::new();
    let (mut ta, mut tb) = (0.0, 0.0);
    for _ in 0..epochs {
        let (a1, _) = time(a);
        let (b1, _) = time(b);
        let (b2, _) = time(b);
        let (a2, _) = time(a);
        ta += a1 + a2;
        tb += b1 + b2;
        ratios.push((b1 + b2) / (a1 + a2));
    }
    ratios.sort_by(f64::total_cmp);
    println!(
        "{name}: port/rust median {:.3}x (range {:.3}x-{:.3}x over {epochs} epochs); rust {:.2} ms, port {:.2} ms per window",
        ratios[ratios.len() / 2],
        ratios[0],
        ratios[ratios.len() - 1],
        ta * 1e3 / (2 * epochs) as f64,
        tb * 1e3 / (2 * epochs) as f64,
    );
}

fn main() {
    println!(
        "Resonance ABBA: A = {} build, B = {} build",
        rust::IMPLEMENTATION,
        port::IMPLEMENTATION
    );
    let kernel_blocks = env("ABBA_KERNEL_BLOCKS", 20_000);
    let graph_blocks = env("ABBA_GRAPH_BLOCKS", 2_000);
    let graph_calls = env("ABBA_GRAPH_CALLS", 10);
    let mut rk = r::kernel();
    let mut pk = p::kernel();
    let (rg, pg) = (r::graph(), p::graph());
    let simd_ticks = env("ABBA_SIMD_TICKS", 200_000);
    let (mut rt, mut pt) = (r::tiles(), p::tiles());
    let small = env("ABBA_MODULE_ITERS", 2_000);
    if let Ok(only) = std::env::var("ABBA_ONLY") {
        let workload = std::env::var("ABBA_WORKLOAD").unwrap_or_else(|_| "kernel".into());
        let v = match (only.as_str(), workload.as_str()) {
            ("rust", "kernel") => r::render(&mut rk, kernel_blocks),
            ("port", "kernel") => p::render(&mut pk, kernel_blocks),
            ("rust", "graph") => r::run(&rg, graph_calls, graph_blocks),
            ("port", "graph") => p::run(&pg, graph_calls, graph_blocks),
            ("rust", "schedule") => r::schedule(&rg, small),
            ("port", "schedule") => p::schedule(&pg, small),
            ("rust", "events") => r::events(small),
            ("port", "events") => p::events(small),
            ("rust", "slab") => r::slab(small * 10),
            ("port", "slab") => p::slab(small * 10),
            ("rust", "pid") => r::pid(small * 100),
            ("port", "pid") => p::pid(small * 100),
            ("rust", "confine") => r::confine(small),
            ("port", "confine") => p::confine(small),
            ("rust", "simd") => r::tick(&mut rt, simd_ticks),
            ("port", "simd") => p::tick(&mut pt, simd_ticks),
            _ => panic!(
                "ABBA_ONLY=rust|port, ABBA_WORKLOAD=kernel|graph|simd|schedule|events|slab|pid|confine"
            ),
        };
        println!("{only} {workload}: {v}");
        return;
    }
    let epochs = env("ABBA_EPOCHS", 9);
    let only = std::env::var("ABBA_WORKLOADS").ok();
    let on = |w: &str| only.as_deref().is_none_or(|o| o.split(',').any(|x| x == w));
    if on("kernel") {
        abba(
            "kernel (64 voices x 64 frames)",
            epochs,
            &mut || r::render(&mut rk, kernel_blocks),
            &mut || p::render(&mut pk, kernel_blocks),
        );
    }
    if on("graph") {
        abba(
            "graph (32 nodes, 64-frame blocks)",
            epochs,
            &mut || r::run(&rg, graph_calls, graph_blocks),
            &mut || p::run(&pg, graph_calls, graph_blocks),
        );
    }
    if on("schedule") {
        abba(
            "schedule (32-node graph)",
            epochs,
            &mut || r::schedule(&rg, small),
            &mut || p::schedule(&pg, small),
        );
    }
    if on("events") {
        abba(
            "events (32-event wire round trip, 16 ramps)",
            epochs,
            &mut || r::events(small),
            &mut || p::events(small),
        );
    }
    if on("slab") {
        abba(
            "slab (8 delay lines)",
            epochs,
            &mut || r::slab(small * 10),
            &mut || p::slab(small * 10),
        );
    }
    if on("pid") {
        abba(
            "pid (1 kHz steps)",
            epochs,
            &mut || r::pid(small * 100),
            &mut || p::pid(small * 100),
        );
    }
    if on("confine") {
        abba(
            "confine (64-function call graph)",
            epochs,
            &mut || r::confine(small),
            &mut || p::confine(small),
        );
    }
    if on("simd") {
        abba(
            "simd (one 16-voice tile and its ramps, widest path)",
            epochs,
            &mut || r::tick(&mut rt, simd_ticks),
            &mut || p::tick(&mut pt, simd_ticks),
        );
    }
}
