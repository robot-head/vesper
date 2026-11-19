//! Divergence checks of the Vesper port (port wave P3b lane 9, #1071): every
//! ported module against the frozen Rust seed copy, `vesper-resonance-seed`,
//! sample for sample and bit for bit. In the default (Rust) build both sides
//! are the same code, so these tests pass trivially there; under
//! `vesper-port` they compare the port with the Rust build.

use vesper_resonance as port;
use vesper_resonance_rust as rust;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn bits(s: &[f32]) -> Vec<u32> {
    s.iter().map(|x| x.to_bits()).collect()
}

#[test]
fn the_build_is_the_one_the_feature_selects() {
    let expected = if cfg!(feature = "vesper-port") {
        "vesper"
    } else {
        "rust"
    };
    assert_eq!(port::IMPLEMENTATION, expected);
    assert_eq!(rust::IMPLEMENTATION, "rust");
}

/// kernels.rs: `SvfBank::render` and `render_reference` over the stability
/// test's coefficient range, with sample-accurate events.
#[test]
fn svf_bank_samples_match_the_rust_build() {
    let mut rng = Rng(0x5eed_0001);
    for trial in 0..24 {
        let g = rng.range(0.0, [0.0013f32, 0.05, 0.5, 3.0, 40.0][trial % 5]);
        let k = rng.range(0.02, 1.98);
        let gain = rng.range(0.1, 2.0);
        let pp = port::kernels::VoicePatch {
            g,
            k,
            gain,
            patch_id: trial as u32,
        };
        let rp = rust::kernels::VoicePatch {
            g,
            k,
            gain,
            patch_id: trial as u32,
        };
        let mut pb = port::kernels::SvfBank::new(pp);
        let mut rb = rust::kernels::SvfBank::new(rp);
        let reference = trial % 2 == 1;
        for block in 0..40 {
            let input: Vec<f32> = (0..64)
                .map(|n| {
                    let x = if ((block * 64 + n) / 37) % 2 == 0 {
                        1.0
                    } else {
                        -1.0
                    };
                    x * rng.range(0.5, 1.0)
                })
                .collect();
            // A few ramps per block, through the parameter table.
            let mut cuts = vec![0usize];
            for _ in 0..3 {
                let lane = rng.below(16) as u16;
                let param = rng.below(3) as u32;
                let target = match param {
                    0 => rng.range(0.0, 2.0),
                    1 => rng.range(0.0, 3.0),
                    _ => rng.range(0.05, 1.9),
                };
                let ramp = rng.below(80) as u32;
                use port::events::ParamTable as _;
                use rust::events::ParamTable as _;
                assert!(pb.set_ramp(lane, param, target, ramp));
                assert!(rb.set_ramp(lane, param, target, ramp));
                cuts.push(rng.below(64) as usize);
            }
            cuts.push(64);
            cuts.sort();
            let (mut pl, mut pr) = (vec![0.0f32; 64], vec![0.0f32; 64]);
            let (mut rl, mut rr) = (vec![0.0f32; 64], vec![0.0f32; 64]);
            for w in cuts.windows(2) {
                if reference {
                    pb.render_reference(&input, &mut pl, &mut pr, w[0], w[1]);
                    rb.render_reference(&input, &mut rl, &mut rr, w[0], w[1]);
                } else {
                    pb.render(&input, &mut pl, &mut pr, w[0], w[1]);
                    rb.render(&input, &mut rl, &mut rr, w[0], w[1]);
                }
            }
            assert_eq!(bits(&pl), bits(&rl), "trial {trial} block {block} left");
            assert_eq!(bits(&pr), bits(&rr), "trial {trial} block {block} right");
            assert_eq!(bits(&pb.tile.ic1eq), bits(&rb.tile.ic1eq));
            assert_eq!(bits(&pb.tile.ic2eq), bits(&rb.tile.ic2eq));
        }
    }
}

/// kernels.rs: the PID controller over the stability test's adversarial
/// inputs (NaN, infinities, overflowing values, random bit patterns).
#[test]
fn pid_commands_match_the_rust_build() {
    let pg = port::kernels::PidGains {
        kp: 50.0,
        ki: 400.0,
        kd: 2.0,
        dt: 0.001,
        torque_limit: 1.5,
        integral_limit: 0.75,
    };
    let rg = rust::kernels::PidGains {
        kp: pg.kp,
        ki: pg.ki,
        kd: pg.kd,
        dt: pg.dt,
        torque_limit: pg.torque_limit,
        integral_limit: pg.integral_limit,
    };
    let mut p = port::kernels::PidController::new(pg).unwrap();
    let mut r = rust::kernels::PidController::new(rg).unwrap();
    let mut rng = Rng(0x7091_0006);
    let specials = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        f32::MIN,
        f32::MIN_POSITIVE,
        0.0,
    ];
    for _ in 0..50_000 {
        let mut pick = || match rng.next() % 10 {
            0 => specials[(rng.next() % specials.len() as u64) as usize],
            1 => f32::from_bits(rng.next() as u32),
            _ => rng.range(-10.0, 10.0),
        };
        let (sp, m) = (pick(), pick());
        let a = p
            .step(sp, m)
            .map(f32::to_bits)
            .map_err(|e| format!("{e:?}"));
        let b = r
            .step(sp, m)
            .map(f32::to_bits)
            .map_err(|e| format!("{e:?}"));
        assert_eq!(a, b, "setpoint {sp} measurement {m}");
        assert_eq!(p.integral().to_bits(), r.integral().to_bits());
    }
    for bad in [-1.0, f32::NAN, f32::INFINITY] {
        let mut g = pg;
        g.ki = bad;
        let mut h = rg;
        h.ki = bad;
        assert_eq!(
            port::kernels::PidController::new(g)
                .map(|_| ())
                .map_err(|e| format!("{e:?}")),
            rust::kernels::PidController::new(h)
                .map(|_| ())
                .map_err(|e| format!("{e:?}"))
        );
    }
}

/// events.rs: the wire codec, stream decoding and validation, and ramps.
#[test]
fn events_and_ramps_match_the_rust_build() {
    let mut rng = Rng(0xe0e0_0002);
    for _ in 0..2000 {
        let mut bytes = vec![0u8; 16 * rng.below(5) as usize + rng.below(3) as usize * 7];
        for b in bytes.iter_mut() {
            *b = rng.next() as u8;
        }
        // Mostly finite targets.
        for f in bytes.as_chunks_mut::<16>().0 {
            if rng.below(4) != 0 {
                f[8..12].copy_from_slice(&rng.range(-4.0, 4.0).to_bits().to_le_bytes());
            }
            if rng.below(2) == 0 {
                f[0..2].copy_from_slice(&(rng.below(64) as u16).to_le_bytes());
            }
        }
        let budget = rng.below(6) as usize;
        let mut po = vec![
            port::events::RtParamEvent {
                sample_offset: 0,
                node_id: 0,
                param_id: 0,
                target: 0.0,
                ramp_samples: 0,
            };
            budget
        ];
        let mut ro = vec![
            rust::events::RtParamEvent {
                sample_offset: 0,
                node_id: 0,
                param_id: 0,
                target: 0.0,
                ramp_samples: 0,
            };
            budget
        ];
        let a = port::events::decode_events(&bytes, &mut po).map(|e| format!("{e:?}"));
        let b = rust::events::decode_events(&bytes, &mut ro).map(|e| format!("{e:?}"));
        assert_eq!(
            a.map_err(|e| format!("{e:?}")),
            b.map_err(|e| format!("{e:?}"))
        );
        for (p, r) in po.iter().zip(&ro) {
            assert_eq!(p.encode(), r.encode());
        }
    }
    let mut pr = port::events::ParamRamp::new(0.25);
    let mut rr = rust::events::ParamRamp::new(0.25);
    for _ in 0..5000 {
        if rng.below(20) == 0 {
            let t = rng.range(-3.0, 3.0);
            let n = rng.below(50) as u32;
            pr.set(t, n);
            rr.set(t, n);
        }
        assert_eq!(pr.next_value().to_bits(), rr.next_value().to_bits());
        assert_eq!(pr.remaining(), rr.remaining());
    }
}

/// Nodes as (id, latency, inputs), each input (feedback, node, delay).
type Graph = Vec<(u32, u32, Vec<(bool, u32, u32)>)>;

fn random_graph(rng: &mut Rng, n: u32) -> Graph {
    // (id, latency, inputs (feedback, source, delay)); forward edges go to
    // earlier ids, feedback edges anywhere, with a block-long delay or more.
    (0..n)
        .map(|i| {
            let lat = if rng.below(3) == 0 {
                rng.below(40) as u32
            } else {
                0
            };
            let mut ins = Vec::new();
            if i > 0 {
                for _ in 0..rng.below(4) {
                    ins.push((false, rng.below(u64::from(i)) as u32, 0));
                }
            }
            if rng.below(6) == 0 {
                ins.push((
                    true,
                    rng.below(u64::from(n)) as u32,
                    16 + rng.below(50) as u32,
                ));
            }
            (i * 3 + 1, lat, ins)
        })
        .collect()
}

fn port_nodes(g: &Graph) -> Vec<port::pdc::NodeSpec> {
    use port::pdc::{Input, NodeId, NodeSpec};
    g.iter()
        .map(|(id, lat, ins)| NodeSpec {
            id: NodeId(*id),
            latency: *lat,
            inputs: ins
                .iter()
                .map(|&(fb, s, d)| {
                    if fb {
                        Input::Feedback {
                            from: NodeId(s * 3 + 1),
                            delay: d,
                        }
                    } else {
                        Input::Forward(NodeId(s * 3 + 1))
                    }
                })
                .collect(),
        })
        .collect()
}

fn rust_nodes(g: &Graph) -> Vec<rust::pdc::NodeSpec> {
    use rust::pdc::{Input, NodeId, NodeSpec};
    g.iter()
        .map(|(id, lat, ins)| NodeSpec {
            id: NodeId(*id),
            latency: *lat,
            inputs: ins
                .iter()
                .map(|&(fb, s, d)| {
                    if fb {
                        Input::Feedback {
                            from: NodeId(s * 3 + 1),
                            delay: d,
                        }
                    } else {
                        Input::Forward(NodeId(s * 3 + 1))
                    }
                })
                .collect(),
        })
        .collect()
}

/// pdc.rs: schedules (and their errors) of random graphs, including
/// duplicate and unknown ids, zero-delay feedback and forward cycles.
#[test]
fn schedules_match_the_rust_build() {
    let mut rng = Rng(0x0dc0_0003);
    for _ in 0..400 {
        let n = 1 + rng.below(24) as u32;
        let mut g = random_graph(&mut rng, n);
        match rng.below(8) {
            0 => g[rng.below(u64::from(n)) as usize].0 = g[0].0,
            1 => g[rng.below(u64::from(n)) as usize].2.push((false, 999, 0)),
            2 => g[rng.below(u64::from(n)) as usize].2.push((true, 0, 0)),
            3 => {
                let a = rng.below(u64::from(n)) as usize;
                g[0].2.push((false, a as u32, 0));
            }
            _ => {}
        }
        let a = port::pdc::compile_schedule(&port_nodes(&g));
        let b = rust::pdc::compile_schedule(&rust_nodes(&g));
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }
}

/// Over-long latent processors are typed errors in both builds, before any
/// delay ring is allocated (a `u32::MAX` latency would be 16 GiB).
#[test]
fn over_long_latent_processors_are_typed_errors() {
    for (latency, block) in [
        (u32::MAX, 16),
        (port::pdc::MAX_DELAY_SAMPLES + 1, 16),
        (5, 0),
    ] {
        let pn = [port::pdc::NodeSpec {
            id: port::pdc::NodeId(0),
            latency,
            inputs: vec![],
        }];
        let rn = [rust::pdc::NodeSpec {
            id: rust::pdc::NodeId(0),
            latency,
            inputs: vec![],
        }];
        let a = port::pdc::render_builtin_graph(
            &pn,
            &[port::pdc::BuiltinKind::Latent],
            block,
            1,
            port::pdc::NodeId(0),
        );
        let b = rust::pdc::render_builtin_graph(
            &rn,
            &[rust::pdc::BuiltinKind::Latent],
            block,
            1,
            rust::pdc::NodeId(0),
        );
        assert!(a.is_err());
        assert_eq!(
            format!("{a:?}"),
            format!("{b:?}"),
            "latency {latency}, block {block}"
        );
    }
}

/// pdc.rs and slab.rs: a `CompiledGraph` of the built-in processors (the
/// ported `MixGain`, `LatentMix`, `DelayLine` and `DelaySlab` kernels), and
/// the all-Vesper `Box[dyn DspProcessor]` graph, against the Rust build.
#[test]
fn graph_outputs_match_the_rust_build() {
    let mut rng = Rng(0x9a9a_0004);
    let mut rendered = 0;
    for trial in 0..60 {
        let n = 2 + rng.below(30) as u32;
        let g = random_graph(&mut rng, n);
        let kinds: Vec<u64> = (0..n)
            .map(|i| if i == 0 { 0 } else { rng.below(3) })
            .collect();
        let gains: Vec<f32> = (0..n).map(|_| rng.range(-1.5, 1.5)).collect();
        let pk: Vec<port::pdc::BuiltinKind> = kinds
            .iter()
            .zip(&gains)
            .enumerate()
            .map(|(i, (&k, &gain))| match k {
                0 => port::pdc::BuiltinKind::Source(i as u32),
                1 => port::pdc::BuiltinKind::Latent,
                _ => port::pdc::BuiltinKind::Gain(gain),
            })
            .collect();
        let rk: Vec<rust::pdc::BuiltinKind> = kinds
            .iter()
            .zip(&gains)
            .enumerate()
            .map(|(i, (&k, &gain))| match k {
                0 => rust::pdc::BuiltinKind::Source(i as u32),
                1 => rust::pdc::BuiltinKind::Latent,
                _ => rust::pdc::BuiltinKind::Gain(gain),
            })
            .collect();
        let sink = g.last().unwrap().0;
        let blocks = 1 + rng.below(12) as usize;
        let a = port::pdc::render_builtin_graph(
            &port_nodes(&g),
            &pk,
            16,
            blocks,
            port::pdc::NodeId(sink),
        );
        let b = rust::pdc::render_builtin_graph(
            &rust_nodes(&g),
            &rk,
            16,
            blocks,
            rust::pdc::NodeId(sink),
        );
        match (a, b) {
            (Ok(a), Ok(b)) => {
                assert_eq!(bits(&a), bits(&b), "trial {trial}");
                rendered += usize::from(a.iter().any(|&x| x != 0.0));
            }
            (a, b) => assert_eq!(
                format!("{:?}", a.map(|v| bits(&v))),
                format!("{:?}", b.map(|v| bits(&v))),
                "trial {trial}"
            ),
        }
    }
    assert!(
        rendered >= 30,
        "only {rendered} graphs rendered a non-silent sink"
    );
}

/// slab.rs: delay-slab reads and writes of every length, against the Rust
/// build's ring arithmetic.
#[test]
fn delay_slabs_match_the_rust_build() {
    let mut rng = Rng(0x51ab_0005);
    for _ in 0..200 {
        let block = 1 + rng.below(40) as usize;
        let delays: Vec<u32> = (0..1 + rng.below(5))
            .map(|_| rng.below(90) as u32)
            .collect();
        let mut p = port::slab::DelaySlab::new(&delays, block).unwrap();
        let mut r = rust::slab::DelaySlab::new(&delays, block).unwrap();
        assert_eq!(p.working_set_bytes(), r.working_set_bytes());
        for _ in 0..30 {
            let line = rng.below(delays.len() as u64 + 1) as usize;
            let len = rng.below(3 * block as u64 + 1) as usize;
            let input: Vec<f32> = (0..len).map(|_| rng.range(-1.0, 1.0)).collect();
            p.write(line, &input);
            r.write(line, &input);
            let (mut po, mut ro) = (vec![0.0f32; len], vec![0.0f32; len]);
            p.read(line, &mut po);
            r.read(line, &mut ro);
            assert_eq!(bits(&po), bits(&ro));
            p.advance_all(block);
            r.advance_all(block);
        }
    }
    for (b, l) in [(usize::MAX, 64), (3, usize::MAX), (0, 0)] {
        assert_eq!(
            format!("{:?}", port::slab::TiledSlab::new(b, l).map(|s| s.stride())),
            format!("{:?}", rust::slab::TiledSlab::new(b, l).map(|s| s.stride()))
        );
    }
}

/// confine.rs: findings (rules, instructions and messages with call paths)
/// of random call graphs and policies.
#[test]
fn confinement_findings_match_the_rust_build() {
    let mut rng = Rng(0xc0f1_0006);
    let mut found = 0;
    for _ in 0..300 {
        let n = 1 + rng.below(12) as u32;
        let mut pg = port::confine::CallGraph::default();
        let mut rg = rust::confine::CallGraph::default();
        let effects = ["Alloc", "Isa", "Realtime", "Net", "Custom"];
        for i in 0..n {
            let known = rng.below(8) != 0;
            let frame = rng.below(256);
            let picks: Vec<usize> = (0..rng.below(3)).map(|_| rng.below(5) as usize).collect();
            let pe = |k: usize| match k {
                0 => port::confine::Effect::Alloc,
                1 => port::confine::Effect::Isa,
                2 => port::confine::Effect::Realtime,
                3 => port::confine::Effect::Net,
                _ => port::confine::Effect::Other(effects[4].into()),
            };
            let re = |k: usize| match k {
                0 => rust::confine::Effect::Alloc,
                1 => rust::confine::Effect::Isa,
                2 => rust::confine::Effect::Realtime,
                3 => rust::confine::Effect::Net,
                _ => rust::confine::Effect::Other(effects[4].into()),
            };
            let loops = rng.below(3);
            let bound = rng.below(2000);
            let name = format!("f{i}");
            let mut p = if known {
                port::confine::FnNode::new(&name, picks.iter().map(|&k| pe(k)).collect(), frame)
            } else {
                port::confine::FnNode::unknown(&name)
            };
            let mut r = if known {
                rust::confine::FnNode::new(&name, picks.iter().map(|&k| re(k)).collect(), frame)
            } else {
                rust::confine::FnNode::unknown(&name)
            };
            p = p.with_loops(match loops {
                0 => port::confine::LoopBound::NoLoops,
                1 => port::confine::LoopBound::Bounded(bound),
                _ => port::confine::LoopBound::Unbounded,
            });
            r = r.with_loops(match loops {
                0 => rust::confine::LoopBound::NoLoops,
                1 => rust::confine::LoopBound::Bounded(bound),
                _ => rust::confine::LoopBound::Unbounded,
            });
            for _ in 0..rng.below(3) {
                let c = rng.below(u64::from(n)) as u32;
                let inst = rng.below(100) as u32;
                p.call_sites.push((port::confine::FnId(c), inst));
                r.call_sites.push((rust::confine::FnId(c), inst));
            }
            pg.add(p).unwrap();
            rg.add(r).unwrap();
        }
        for _ in 0..rng.below(u64::from(2 * n) + 1) {
            // Mostly forward edges, so most graphs are acyclic.
            let a = rng.below(u64::from(n)) as u32;
            let b = if rng.below(5) == 0 {
                rng.below(u64::from(n) + 1) as u32
            } else {
                a + rng.below(u64::from(n - a)) as u32
            };
            if a == b && rng.below(2) == 0 {
                continue;
            }
            pg.call(port::confine::FnId(a), port::confine::FnId(b));
            rg.call(rust::confine::FnId(a), rust::confine::FnId(b));
        }
        let mut pp = port::confine::RealtimePolicy::new(vec![]);
        let mut rp = rust::confine::RealtimePolicy::new(vec![]);
        for _ in 0..1 + rng.below(2) {
            let f = rng.below(u64::from(n)) as u32;
            let budget = (rng.below(2) == 0).then(|| rng.below(600));
            pp.roots.push(port::confine::RealtimeRoot {
                function: port::confine::FnId(f),
                stack_budget_bytes: budget,
            });
            rp.roots.push(rust::confine::RealtimeRoot {
                function: rust::confine::FnId(f),
                stack_budget_bytes: budget,
            });
        }
        for _ in 0..rng.below(3) {
            let caller = rng.below(u64::from(n)) as u32;
            let site = rng.below(4) as u32;
            pg.indirect_calls.push(port::confine::IndirectCall {
                caller: port::confine::FnId(caller),
                site,
            });
            rg.indirect_calls.push(rust::confine::IndirectCall {
                caller: rust::confine::FnId(caller),
                site,
            });
            if rng.below(2) == 0 {
                let targets: Vec<u32> = (0..rng.below(3))
                    .map(|_| rng.below(u64::from(n)) as u32)
                    .collect();
                let s2 = if rng.below(6) == 0 { site + 9 } else { site };
                pp.indirect_targets.insert(
                    (port::confine::FnId(caller), s2),
                    targets.iter().map(|&t| port::confine::FnId(t)).collect(),
                );
                rp.indirect_targets.insert(
                    (rust::confine::FnId(caller), s2),
                    targets.iter().map(|&t| rust::confine::FnId(t)).collect(),
                );
            }
        }
        if rng.below(2) == 0 {
            pp.reject_unbounded_loops = false;
            rp.reject_unbounded_loops = false;
        }
        if rng.below(2) == 0 {
            pp.max_loop_iterations = Some(1000);
            rp.max_loop_iterations = Some(1000);
        }
        if rng.below(3) == 0 {
            pp.allowed_effects
                .insert(port::confine::Effect::Other("Custom".into()));
            rp.allowed_effects
                .insert(rust::confine::Effect::Other("Custom".into()));
        }
        let a = port::confine::verify_realtime(&pg, &pp);
        let b = rust::confine::verify_realtime(&rg, &rp);
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
        found += usize::from(b.is_ok_and(|f| !f.is_empty()));
    }
    assert!(found >= 100, "only {found} graphs had findings");
}

/// lease.rs: frame writes, commits, emergency silence and driver copies.
#[test]
fn leases_match_the_rust_build() {
    let mut rng = Rng(0x1ea5_0007);
    for _ in 0..200 {
        let (ch, fr) = (1 + rng.below(3) as usize, 1 + rng.below(6) as usize);
        let mut pport = port::lease::RtOutputPort::new(ch, fr, 2).unwrap();
        let mut rport = rust::lease::RtOutputPort::new(ch, fr, 2).unwrap();
        let mut pl = pport.lease().unwrap();
        let mut rl = rport.lease().unwrap();
        for _ in 0..rng.below(10) {
            let frame = rng.below(fr as u64 + 1) as usize;
            let n = if rng.below(5) == 0 { ch + 1 } else { ch };
            let s: Vec<f32> = (0..n)
                .map(|_| {
                    if rng.below(20) == 0 {
                        f32::NAN
                    } else {
                        rng.range(-1.0, 1.0)
                    }
                })
                .collect();
            assert_eq!(
                format!("{:?}", pl.write_frame(frame, &s)),
                format!("{:?}", rl.write_frame(frame, &s))
            );
        }
        let silence = rng.below(2) == 0;
        if silence {
            assert!(pport.emergency_silence_and_commit(pl).is_ok());
            assert!(rport.emergency_silence_and_commit(rl).is_ok());
        } else {
            match (pport.commit(pl), rport.commit(rl)) {
                (Ok(()), Ok(())) => {}
                (Err(p), Err(r)) => {
                    assert!(pport.emergency_silence_and_commit(p).is_ok());
                    assert!(rport.emergency_silence_and_commit(r).is_ok());
                }
                _ => panic!("commit outcomes differ"),
            }
        }
        let (mut po, mut ro) = (vec![9.0f32; ch * fr], vec![9.0f32; ch * fr]);
        assert_eq!(
            format!("{:?}", pport.complete(&mut po)),
            format!("{:?}", rport.complete(&mut ro))
        );
        assert_eq!(bits(&po), bits(&ro));
    }
}
