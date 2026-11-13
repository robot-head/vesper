//! "0 Alloc" evidence: a counting global allocator over the warmed-up
//! steady state of the realtime paths. Counting is per thread (only the
//! thread under test is counted), so the test harness's other threads do
//! not interfere.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use vesper_resonance::events::RtParamEvent;
use vesper_resonance::kernels::{
    LANES, PidController, PidGains, QUANTUM_FRAMES, QuantumOutcome, SynthEngine, VoicePatch,
    VoicePatchBank, param,
};
use vesper_resonance::lease::RtOutputPort;
use vesper_resonance::pdc::{
    CompiledGraph, DspProcessor, Input, LatentMix, MixGain, NodeId, NodeSpec,
};
use vesper_resonance::spsc::{mailbox, retirement_channel};

struct Counting;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static DEALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn bump(c: &'static std::thread::LocalKey<Cell<u64>>) {
    // `try_with`: thread-locals may be gone during thread teardown.
    let _ = ACTIVE.try_with(|a| {
        if a.get() {
            let _ = c.try_with(|n| n.set(n.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump(&ALLOCS);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        bump(&DEALLOCS);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump(&ALLOCS);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        bump(&ALLOCS);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Run `f` with counting on; returns (allocations, deallocations).
fn count<R>(f: impl FnOnce() -> R) -> (R, u64, u64) {
    ALLOCS.with(|n| n.set(0));
    DEALLOCS.with(|n| n.set(0));
    ACTIVE.with(|a| a.set(true));
    let r = f();
    ACTIVE.with(|a| a.set(false));
    (r, ALLOCS.with(Cell::get), DEALLOCS.with(Cell::get))
}

#[test]
fn counter_detects_allocation() {
    let (_, a, d) = count(|| drop(std::hint::black_box(vec![1u8; 10])));
    assert_eq!((a, d), (1, 1));
}

fn bank(id: u32) -> VoicePatchBank {
    let mut voices = [VoicePatch {
        g: 0.05,
        k: 1.2,
        gain: 0.1,
        patch_id: id,
    }; LANES];
    for (v, p) in voices.iter_mut().enumerate() {
        p.g = 0.02 + v as f32 * 0.01;
    }
    VoicePatchBank { voices }
}

/// The full §32.3 quantum (patch swap + wait-free retirement + event decode
/// and splitting + SVF render + lease commit) allocates and frees nothing,
/// including on quanta that swap in a new patch bank.
#[test]
fn synth_quantum_steady_state_allocates_nothing() {
    let (mut tx, rx) = mailbox::<Box<VoicePatchBank>>(16).unwrap();
    let (sink, mut janitor) = retirement_channel::<Box<VoicePatchBank>>(16).unwrap();
    let mut engine = SynthEngine::new(Box::new(bank(0)), rx, sink);
    let mut port = RtOutputPort::new(2, QUANTUM_FRAMES, 4).unwrap();
    let input = [0.25f32; QUANTUM_FRAMES];
    let events: Vec<u8> = [
        RtParamEvent {
            sample_offset: 5,
            node_id: 2,
            param_id: param::G,
            target: 0.2,
            ramp_samples: 30,
        },
        RtParamEvent {
            sample_offset: 40,
            node_id: 9,
            param_id: param::GAIN,
            target: 0.0,
            ramp_samples: 0,
        },
    ]
    .iter()
    .flat_map(|e| e.encode())
    .collect();
    let mut out = vec![0.0f32; 2 * QUANTUM_FRAMES];
    // Warm up.
    for _ in 0..8 {
        engine.process_quantum(&input, &events, &mut port);
        port.complete(&mut out);
    }
    for round in 0..50u32 {
        // Non-realtime side: allocate the next patch bank, drain retirements.
        tx.send(Box::new(bank(round + 1))).unwrap();
        janitor.drain();
        let (rendered, a, d) = count(|| {
            let mut ok = 0;
            for q in 0..20 {
                let evs: &[u8] = if q % 2 == 0 { &events } else { &[] };
                if matches!(
                    engine.process_quantum(&input, evs, &mut port),
                    QuantumOutcome::Rendered { .. }
                ) {
                    ok += 1;
                }
                port.complete(&mut out);
            }
            ok
        });
        assert_eq!(
            (a, d),
            (0, 0),
            "round {round}: allocations in the realtime quantum"
        );
        assert_eq!(rendered, 20);
        assert_eq!(engine.current_patch().voices[0].patch_id, round + 1);
    }
    assert_eq!(port.auditor().leaked(), 0);
}

#[test]
fn pid_steady_state_allocates_nothing() {
    let mut pid = PidController::new(PidGains {
        kp: 8.0,
        ki: 20.0,
        kd: 0.4,
        dt: 0.001,
        torque_limit: 2.0,
        integral_limit: 1.0,
    })
    .unwrap();
    let (sum, a, d) = count(|| {
        let mut theta = 0.0f32;
        let mut w = 0.0f32;
        let mut sum = 0.0f32;
        for _ in 0..10_000 {
            let tau = pid.step(1.0, theta).unwrap();
            w += (tau - 0.1 * w) / 0.01 * 0.001;
            theta += w * 0.001;
            sum += tau;
        }
        sum
    });
    assert!(sum.is_finite());
    assert_eq!((a, d), (0, 0));
}

#[test]
fn compiled_graph_blocks_allocate_nothing() {
    let block = 64;
    let nodes = vec![
        NodeSpec {
            id: NodeId(0),
            latency: 0,
            inputs: vec![],
        },
        NodeSpec {
            id: NodeId(1),
            latency: 23,
            inputs: vec![
                Input::Forward(NodeId(0)),
                Input::Feedback {
                    from: NodeId(2),
                    delay: 64,
                },
            ],
        },
        NodeSpec {
            id: NodeId(2),
            latency: 0,
            inputs: vec![Input::Forward(NodeId(0)), Input::Forward(NodeId(1))],
        },
    ];
    struct Ramp(f32);
    impl DspProcessor for Ramp {
        fn process(&mut self, _: &vesper_resonance::pdc::Inputs<'_>, out: &mut [f32]) {
            for y in out {
                self.0 = (self.0 + 0.01) % 1.0;
                *y = self.0;
            }
        }
    }
    let procs: Vec<Box<dyn DspProcessor>> = vec![
        Box::new(Ramp(0.0)),
        Box::new(LatentMix::new(23, block).unwrap()),
        Box::new(MixGain(0.5)),
    ];
    let mut g = CompiledGraph::new(&nodes, procs, block).unwrap();
    g.process_block();
    let (_, a, d) = count(|| {
        for _ in 0..1000 {
            g.process_block();
        }
    });
    assert_eq!((a, d), (0, 0));
    assert!(g.output(NodeId(2)).unwrap().iter().all(|y| y.is_finite()));
}

#[test]
fn retirement_on_the_realtime_side_allocates_and_frees_nothing() {
    let (mut sink, mut janitor) = retirement_channel::<Box<[u8; 4096]>>(8).unwrap();
    for _ in 0..3 {
        let batch: Vec<Box<[u8; 4096]>> = (0..8).map(|_| Box::new([7u8; 4096])).collect();
        let mut batch = batch.into_iter();
        let (_, a, d) = count(|| {
            for b in batch.by_ref() {
                sink.retire(b).unwrap();
            }
        });
        // The 4 KiB boxes were moved, not freed, on this side.
        assert_eq!((a, d), (0, 0));
        drop(batch);
        let (n, _, d) = count(|| janitor.drain());
        assert_eq!(n, 8);
        assert_eq!(d, 8, "the janitor is where the boxes are freed");
    }
}
