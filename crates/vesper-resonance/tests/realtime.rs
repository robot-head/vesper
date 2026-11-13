//! Leases, sample-accurate events and the §32.3 kernels against references.

use vesper_resonance::events::{
    EVENT_WIRE_BYTES, EventError, ParamRamp, ParamTable, RtParamEvent, decode_events,
    process_with_events,
};
use vesper_resonance::kernels::{
    LANES, MAX_EVENTS, PidController, PidFault, PidGains, QUANTUM_FRAMES, QuantumOutcome, SvfBank,
    SynthEngine, VoicePatch, VoicePatchBank, param,
};
use vesper_resonance::lease::{CommitKind, LeaseError, RtOutputPort};
use vesper_resonance::spsc::{mailbox, retirement_channel};

fn ev(offset: u16, node: u16, param: u32, target: f32, ramp: u32) -> RtParamEvent {
    RtParamEvent {
        sample_offset: offset,
        node_id: node,
        param_id: param,
        target,
        ramp_samples: ramp,
    }
}

// ---------------------------------------------------------------- leases

#[test]
fn lease_must_be_fully_written_to_commit() {
    let mut port = RtOutputPort::new(2, 4, 2).unwrap();
    let mut lease = port.lease().unwrap();
    lease.write_frame(0, &[1.0, 2.0]).unwrap();
    assert_eq!(
        lease.write_frame(2, &[0.0, 0.0]),
        Err(LeaseError::OutOfOrder {
            expected: 1,
            got: 2
        })
    );
    assert_eq!(
        lease.write_frame(1, &[0.0]),
        Err(LeaseError::ChannelCount {
            expected: 2,
            got: 1
        })
    );
    assert_eq!(
        lease.write_frame(1, &[f32::NAN, 0.0]),
        Err(LeaseError::NonFinite)
    );
    let mut lease = port
        .commit(lease)
        .expect_err("incomplete lease is handed back");
    assert_eq!(lease.written(), 1);
    for f in 1..4 {
        lease.write_frame(f, &[f as f32, -(f as f32)]).unwrap();
    }
    assert_eq!(lease.write_frame(4, &[0.0, 0.0]), Err(LeaseError::Full));
    assert!(port.commit(lease).is_ok());
    let mut out = [9.0f32; 8];
    assert_eq!(port.complete(&mut out), Some(CommitKind::Rendered));
    assert_eq!(out, [1.0, 2.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    assert_eq!(port.auditor().committed(), 1);
}

#[test]
fn emergency_silence_zeroes_the_rest_and_drops_are_counted() {
    let mut port = RtOutputPort::new(1, 4, 2).unwrap();
    let mut lease = port.lease().unwrap();
    lease.write_frame(0, &[0.5]).unwrap();
    port.emergency_silence_and_commit(lease).unwrap();
    let mut out = [9.0f32; 4];
    assert_eq!(
        port.complete(&mut out),
        Some(CommitKind::Silenced { written_frames: 1 })
    );
    assert_eq!(out, [0.5, 0.0, 0.0, 0.0]);

    // A lease from another port is refused, never silently accepted.
    let mut other = RtOutputPort::new(1, 4, 1).unwrap();
    let foreign = other.lease().unwrap();
    let foreign = port.emergency_silence_and_commit(foreign).unwrap_err();
    other.emergency_silence_and_commit(foreign).unwrap();

    let leaked = port.lease().unwrap();
    drop(leaked);
    assert_eq!(port.auditor().leaked(), 1);
    assert_eq!(port.auditor().issued(), 2);
    assert_eq!(port.auditor().silenced(), 1);
    assert!(RtOutputPort::new(0, 4, 1).is_err());
    assert!(RtOutputPort::new(usize::MAX, 2, 1).is_err());
}

#[test]
fn port_runs_out_of_periods_until_the_driver_completes() {
    let mut port = RtOutputPort::new(1, 1, 1).unwrap();
    let mut l = port.lease().unwrap();
    l.write_frame(0, &[1.0]).unwrap();
    port.commit(l).unwrap();
    assert!(port.lease().is_none());
    let mut out = [0.0];
    port.complete(&mut out).unwrap();
    let l = port.lease().unwrap();
    port.emergency_silence_and_commit(l).unwrap();
}

// ---------------------------------------------------------------- events

#[test]
fn wire_event_is_16_bytes_little_endian() {
    let e = ev(0x0102, 0x0304, 0x0506_0708, 1.0, 0x0a0b_0c0d);
    let b = e.encode();
    assert_eq!(b.len(), EVENT_WIRE_BYTES);
    assert_eq!(
        b,
        [
            0x02, 0x01, 0x04, 0x03, 0x08, 0x07, 0x06, 0x05, 0x00, 0x00, 0x80, 0x3f, 0x0d, 0x0c,
            0x0b, 0x0a
        ]
    );
    assert_eq!(RtParamEvent::decode(&b), Ok(e));
}

#[test]
fn malformed_event_streams_are_typed_errors() {
    let mut out = [ev(0, 0, 0, 0.0, 0); 2];
    assert_eq!(
        decode_events(&[0; 17], &mut out),
        Err(EventError::TrailingBytes(1))
    );
    assert_eq!(
        decode_events(&[0; 48], &mut out),
        Err(EventError::OverBudget { max: 2 })
    );
    let mut nan = ev(0, 0, 0, 0.0, 0).encode();
    nan[8..12].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
    assert_eq!(
        decode_events(&nan, &mut out),
        Err(EventError::NonFiniteTarget)
    );
    assert_eq!(decode_events(&[], &mut out).unwrap().len(), 0);

    let mut bank = SvfBank::new(patch(0.1, 0.7, 1.0));
    let render = |_: &mut SvfBank, _: usize, _: usize| {};
    assert_eq!(
        process_with_events(&[ev(64, 0, 0, 1.0, 0)], 64, &mut bank, render),
        Err(EventError::OffsetOutOfBlock {
            offset: 64,
            block: 64
        })
    );
    assert_eq!(
        process_with_events(
            &[ev(9, 0, 0, 1.0, 0), ev(3, 0, 0, 1.0, 0)],
            64,
            &mut bank,
            render
        ),
        Err(EventError::Unsorted { index: 1 })
    );
    assert_eq!(
        process_with_events(&[ev(0, LANES as u16, 0, 1.0, 0)], 64, &mut bank, render),
        Err(EventError::UnknownParam {
            node_id: LANES as u16,
            param_id: 0
        })
    );
    assert_eq!(
        process_with_events(&[ev(0, 0, 3, 1.0, 0)], 64, &mut bank, render),
        Err(EventError::UnknownParam {
            node_id: 0,
            param_id: 3
        })
    );
}

#[test]
fn ramps_land_exactly_on_target() {
    let mut r = ParamRamp::new(0.0);
    r.set(1.0, 3);
    let vals: Vec<f32> = (0..5).map(|_| r.next_value()).collect();
    assert_eq!(vals[2], 1.0);
    assert_eq!(&vals[3..], &[1.0, 1.0]);
    assert!(vals[0] > 0.0 && vals[0] < vals[1] && vals[1] < 1.0);
    r.set(-2.0, 0);
    assert_eq!(r.next_value(), -2.0);
}

/// A simple per-sample table: one ramp per (node, param).
struct Table([[ParamRamp; 2]; 2]);
impl ParamTable for Table {
    fn ramp(&mut self, node: u16, param: u32) -> Option<&mut ParamRamp> {
        self.0.get_mut(node as usize)?.get_mut(param as usize)
    }
}

/// Split processing equals a per-sample reference that applies events at
/// exactly their sample offsets, for random event sets.
#[test]
fn split_blocks_match_per_sample_reference() {
    let mut seed = 0x1234_5678u64;
    let mut rnd = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    for _ in 0..500 {
        let block = 1 + rnd(128) as usize;
        let count = rnd(12) as usize;
        let mut events: Vec<RtParamEvent> = (0..count)
            .map(|_| {
                ev(
                    rnd(block as u64) as u16,
                    rnd(2) as u16,
                    rnd(2) as u32,
                    rnd(1000) as f32 / 100.0 - 5.0,
                    rnd(40) as u32,
                )
            })
            .collect();
        events.sort_by_key(|e| e.sample_offset);

        let mut split = Table([[ParamRamp::new(0.5); 2]; 2]);
        let mut got = vec![0.0f32; block];
        let segments = process_with_events(&events, block, &mut split, |t, s, e| {
            for y in &mut got[s..e] {
                let mut acc = 0.0;
                for n in &mut t.0 {
                    for p in n.iter_mut() {
                        acc += p.next_value();
                    }
                }
                *y = acc;
            }
        })
        .unwrap();

        let mut reference = Table([[ParamRamp::new(0.5); 2]; 2]);
        let mut want = vec![0.0f32; block];
        for (t, y) in want.iter_mut().enumerate() {
            for e in events.iter().filter(|e| usize::from(e.sample_offset) == t) {
                reference.0[e.node_id as usize][e.param_id as usize].set(e.target, e.ramp_samples);
            }
            let mut acc = 0.0;
            for n in &mut reference.0 {
                for p in n.iter_mut() {
                    acc += p.next_value();
                }
            }
            *y = acc;
        }
        assert_eq!(got, want);
        let mut offsets: Vec<u16> = events
            .iter()
            .map(|e| e.sample_offset)
            .filter(|&o| o > 0)
            .collect();
        offsets.dedup();
        assert_eq!(segments, 1 + offsets.len());
    }
}

// ---------------------------------------------------------------- SVF

fn patch(g: f32, k: f32, gain: f32) -> VoicePatch {
    VoicePatch {
        g,
        k,
        gain,
        patch_id: 7,
    }
}

/// Straightforward f64 per-voice TPT SVF low-pass with the same per-sample
/// parameter ramps.
fn svf_reference(
    input: &[f32],
    voices: &[(f64, f64, f64)],
    events: &[RtParamEvent],
) -> (Vec<f64>, Vec<f64>) {
    let mut left = vec![0.0; input.len()];
    let mut right = vec![0.0; input.len()];
    for (v, &(g0, k0, gain0)) in voices.iter().enumerate() {
        let mut ramps = [
            ParamRamp::new(gain0 as f32),
            ParamRamp::new(g0 as f32),
            ParamRamp::new(k0 as f32),
        ];
        let (mut ic1, mut ic2) = (0.0f64, 0.0f64);
        let p = v as f64 / (LANES - 1) as f64;
        for (i, &x) in input.iter().enumerate() {
            for e in events
                .iter()
                .filter(|e| e.sample_offset as usize == i && e.node_id as usize == v)
            {
                ramps[e.param_id as usize].set(e.target, e.ramp_samples);
            }
            let gain = f64::from(ramps[0].next_value());
            let g = f64::from(ramps[1].next_value());
            let k = f64::from(ramps[2].next_value());
            let a1 = 1.0 / (1.0 + g * (g + k));
            let a2 = g * a1;
            let a3 = g * a2;
            let v3 = f64::from(x) - ic2;
            let v1 = a1 * ic1 + a2 * v3;
            let v2 = ic2 + a2 * ic1 + a3 * v3;
            ic1 = 2.0 * v1 - ic1;
            ic2 = 2.0 * v2 - ic2;
            left[i] += gain * v2 * (1.0 - p);
            right[i] += gain * v2 * p;
        }
    }
    (left, right)
}

fn voices() -> Vec<(f64, f64, f64)> {
    (0..LANES)
        .map(|v| {
            let fc = 100.0 * (1.3f64).powi(v as i32);
            (
                (std::f64::consts::PI * fc / 48_000.0).tan(),
                1.0 / (0.5 + v as f64 * 0.3),
                1.0 / LANES as f64,
            )
        })
        .collect()
}

fn bank_from(voices: &[(f64, f64, f64)]) -> VoicePatchBank {
    let mut b = VoicePatchBank {
        voices: [patch(0.0, 0.0, 0.0); LANES],
    };
    for (v, &(g, k, gain)) in voices.iter().enumerate() {
        b.voices[v] = patch(g as f32, k as f32, gain as f32);
    }
    b
}

fn test_input(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| ((i as f32 * 0.07).sin() + if i % 50 == 0 { 0.8 } else { 0.0 }) * 0.5)
        .collect()
}

#[test]
fn svf_bank_matches_f64_reference_with_events() {
    let v = voices();
    let input = test_input(4096);
    let events = [
        ev(10, 3, param::G, 0.3, 100),
        ev(10, 4, param::GAIN, 0.0, 0),
        ev(500, 15, param::K, 0.2, 2000),
        ev(2048, 0, param::GAIN, 0.25, 7),
    ];
    let (rl, rr) = svf_reference(&input, &v, &events);
    let mut bank = SvfBank::new(patch(0.0, 1.0, 0.0));
    bank.apply_patch(&bank_from(&v));
    let mut left = vec![0.0f32; input.len()];
    let mut right = vec![0.0f32; input.len()];
    process_with_events(&events, input.len(), &mut bank, |b, s, e| {
        b.render(&input, &mut left, &mut right, s, e)
    })
    .unwrap();
    let mut max_err = 0.0f64;
    for i in 0..input.len() {
        max_err = max_err.max((f64::from(left[i]) - rl[i]).abs());
        max_err = max_err.max((f64::from(right[i]) - rr[i]).abs());
    }
    assert!(max_err < 1e-4, "max error {max_err}");
    // The filter actually filters: DC passes (low-pass), so a constant
    // input settles to its gain-weighted sum.
    let dc = vec![1.0f32; 48_000];
    let mut bank = SvfBank::new(patch(0.0, 1.0, 0.0));
    bank.apply_patch(&bank_from(&v));
    let mut l = vec![0.0; dc.len()];
    let mut r = vec![0.0; dc.len()];
    bank.render(&dc, &mut l, &mut r, 0, dc.len());
    assert!((l[dc.len() - 1] + r[dc.len() - 1] - 1.0).abs() < 1e-3);
}

// ---------------------------------------------------------------- quantum

#[test]
fn realtime_quantum_swaps_patches_splits_events_and_commits() {
    let v = voices();
    let (mut tx, rx) = mailbox::<Box<VoicePatchBank>>(4).unwrap();
    let (sink, mut janitor) = retirement_channel::<Box<VoicePatchBank>>(4).unwrap();
    let mut engine = SynthEngine::new(Box::new(bank_from(&v)), rx, sink);
    let mut port = RtOutputPort::new(2, QUANTUM_FRAMES, 2).unwrap();
    let input: [f32; QUANTUM_FRAMES] = test_input(QUANTUM_FRAMES).try_into().unwrap();

    let events: Vec<u8> = [
        ev(0, 1, param::GAIN, 0.5, 0),
        ev(17, 2, param::G, 0.4, 10),
        ev(17, 3, param::K, 1.0, 0),
    ]
    .iter()
    .flat_map(|e| e.encode())
    .collect();
    let out = engine.process_quantum(&input, &events, &mut port);
    assert!(
        matches!(out, QuantumOutcome::Rendered { segments: 2, .. }),
        "{out:?}"
    );
    let mut buf = [0.0f32; 2 * QUANTUM_FRAMES];
    assert_eq!(port.complete(&mut buf), Some(CommitKind::Rendered));
    assert!(buf.iter().any(|&s| s != 0.0));

    // A new patch bank is swapped in; the old one is retired, not dropped.
    let mut newer = bank_from(&v);
    newer.voices[0].patch_id = 99;
    tx.send(Box::new(newer)).unwrap();
    let out = engine.process_quantum(&input, &[], &mut port);
    assert!(matches!(out, QuantumOutcome::Rendered { swap, .. } if swap.swapped == 1));
    assert_eq!(engine.current_patch().voices[0].patch_id, 99);
    assert_eq!(engine.bank().tile.patch_id[0], 99);
    assert_eq!(janitor.pop().map(|b| b.voices[0].patch_id), Some(7));
    port.complete(&mut buf).unwrap();

    // Malformed events: silenced, committed, typed error.
    let out = engine.process_quantum(&input, &[1, 2, 3], &mut port);
    assert_eq!(out, QuantumOutcome::Silenced(EventError::TrailingBytes(3)));
    assert_eq!(
        port.complete(&mut buf),
        Some(CommitKind::Silenced { written_frames: 0 })
    );
    assert!(buf.iter().all(|&s| s == 0.0));

    let too_many: Vec<u8> = (0..=MAX_EVENTS)
        .flat_map(|_| ev(0, 0, 0, 0.0, 0).encode())
        .collect();
    assert_eq!(
        engine.process_quantum(&input, &too_many, &mut port),
        QuantumOutcome::Silenced(EventError::OverBudget { max: MAX_EVENTS })
    );

    // No free period: nothing rendered.
    let out = engine.process_quantum(&input, &[], &mut port);
    assert!(matches!(out, QuantumOutcome::Rendered { .. }));
    assert_eq!(
        engine.process_quantum(&input, &[], &mut port),
        QuantumOutcome::NoPeriod
    );
    assert_eq!(port.auditor().leaked(), 0);
}

// ---------------------------------------------------------------- PID

fn gains() -> PidGains {
    PidGains {
        kp: 8.0,
        ki: 20.0,
        kd: 0.4,
        dt: 0.001,
        torque_limit: 2.0,
        integral_limit: 1.0,
    }
}

/// f64 reference PID with the same structure.
struct RefPid {
    g: PidGains,
    i: f64,
    prev: Option<f64>,
}

impl RefPid {
    fn step(&mut self, sp: f64, m: f64) -> f64 {
        let g = self.g;
        let e = sp - m;
        let il = f64::from(g.integral_limit);
        self.i = (self.i + f64::from(g.ki) * e * f64::from(g.dt)).clamp(-il, il);
        let d = self.prev.map_or(0.0, |p| -(m - p) / f64::from(g.dt));
        self.prev = Some(m);
        let tl = f64::from(g.torque_limit);
        (f64::from(g.kp) * e + self.i + f64::from(g.kd) * d).clamp(-tl, tl)
    }
}

/// Motor + load: J dw/dt = tau - b w, dtheta/dt = w.
fn plant(theta: &mut f64, w: &mut f64, tau: f64, dt: f64) {
    let (j, b) = (0.01, 0.1);
    *w += (tau - b * *w) / j * dt;
    *theta += *w * dt;
}

#[test]
fn pid_matches_reference_and_tracks_setpoint_at_1khz() {
    let mut pid = PidController::new(gains()).unwrap();
    let mut rpid = RefPid {
        g: gains(),
        i: 0.0,
        prev: None,
    };
    let (mut theta, mut w) = (0.0f64, 0.0f64);
    let mut max_dev = 0.0f64;
    for step in 0..3000 {
        let sp = if step < 1500 { 1.0 } else { -0.5 };
        let m = theta;
        let tau = pid.step(sp as f32, m as f32).unwrap();
        let rtau = rpid.step(sp, f64::from(m as f32));
        max_dev = max_dev.max((f64::from(tau) - rtau).abs());
        assert!(tau.abs() <= gains().torque_limit);
        assert!(pid.integral().abs() <= gains().integral_limit);
        plant(&mut theta, &mut w, f64::from(tau), 0.001);
    }
    assert!(
        max_dev < 1e-3,
        "controller vs reference deviation {max_dev}"
    );
    assert!((theta + 0.5).abs() < 0.01, "final position {theta}");
}

#[test]
fn pid_rejects_bad_inputs_and_gains() {
    let mut pid = PidController::new(gains()).unwrap();
    assert_eq!(pid.step(f32::NAN, 0.0), Err(PidFault::NonFiniteInput));
    assert_eq!(pid.step(0.0, f32::INFINITY), Err(PidFault::NonFiniteInput));
    // Finite inputs whose difference overflows: a fault, and the state is
    // not poisoned for later steps.
    let mut p = PidController::new(PidGains {
        kp: 1.0,
        ki: 0.0,
        kd: 0.0,
        dt: 0.001,
        torque_limit: 1.0,
        integral_limit: 1.0,
    })
    .unwrap();
    assert_eq!(
        p.step(f32::MAX, -f32::MAX),
        Err(PidFault::NonFiniteIntermediate)
    );
    assert_eq!(p.integral(), 0.0);
    assert_eq!(p.step(0.5, 0.0), Ok(0.5));
    let mut bad = gains();
    bad.dt = 0.0;
    assert_eq!(PidController::new(bad).err(), Some(PidFault::BadGains));
    bad = gains();
    bad.kp = -1.0;
    assert_eq!(PidController::new(bad).err(), Some(PidFault::BadGains));
}
