//! Model-based test: the wheel against a `BTreeMap` reference over random
//! insert / cancel / advance sequences, including far (overflow) deadlines,
//! slot-index wrap-around, start times near 2^64 and cascading across levels.

use std::collections::BTreeMap;

use vesper_warp_wheel::{HandleError, MAX_TIME_NS, TimerHandle, Wheel, WheelConfig};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

#[derive(Clone, Copy)]
struct Shape {
    tick_ns: u64,
    slot_bits: u8,
    levels: u8,
    capacity: u32,
    start_ns: u64,
}

fn run(shape: Shape, seed: u64, steps: usize) {
    let mut rng = Rng(seed);
    let config = WheelConfig {
        tick_ns: shape.tick_ns,
        slot_bits: shape.slot_bits,
        levels: shape.levels,
        capacity: shape.capacity,
        start_ns: shape.start_ns,
    };
    let mut wheel: Wheel<u64> = Wheel::new(config).unwrap();
    // Reference: (deadline, insertion seq) -> payload id.
    let mut model: BTreeMap<(u64, u64), u64> = BTreeMap::new();
    let mut live: Vec<(TimerHandle, (u64, u64))> = Vec::new();
    let mut stale: Vec<TimerHandle> = Vec::new();
    let mut now = shape.start_ns;
    let mut seq = 0u64;
    let span_ticks = 1u64 << (u32::from(shape.slot_bits) * u32::from(shape.levels));
    let span_ns = span_ticks.saturating_mul(shape.tick_ns);
    let room = MAX_TIME_NS - shape.start_ns;

    for _ in 0..steps {
        match rng.below(10) {
            0..=4 => {
                let offset = match rng.below(8) {
                    0 => 0,
                    1 => rng.below(shape.tick_ns * 3),
                    2 | 3 => rng.below(shape.tick_ns.saturating_mul(1 << shape.slot_bits) * 4),
                    4 => rng.below(span_ns),
                    5 => span_ns.saturating_add(rng.below(span_ns.saturating_mul(4))),
                    6 => rng.next() >> rng.below(64),
                    _ => {
                        // Past or same-tick deadline relative to the clock.
                        0u64.wrapping_sub(rng.below(shape.tick_ns * 2))
                    }
                };
                let deadline = if offset > u64::MAX / 2 {
                    now.saturating_sub(offset.wrapping_neg())
                } else {
                    now.saturating_add(offset).min(shape.start_ns + room)
                };
                let id = seq;
                match wheel.insert(deadline, id) {
                    Ok(h) => {
                        model.insert((deadline, seq), id);
                        live.push((h, (deadline, seq)));
                    }
                    Err(e) => {
                        assert_eq!(wheel.len(), shape.capacity as usize, "only Full expected");
                        assert_eq!(e.into_payload(), id);
                    }
                }
                seq += 1;
            }
            5 | 6 => {
                if !live.is_empty() {
                    let i = rng.below(live.len() as u64) as usize;
                    let (h, key) = live.swap_remove(i);
                    if model.contains_key(&key) {
                        assert_eq!(wheel.deadline(&h), Ok(key.0));
                        assert_eq!(wheel.cancel(h), Ok(model.remove(&key).unwrap()));
                    } else {
                        stale.push(h);
                    }
                }
                if let Some(h) = stale.pop() {
                    assert_eq!(wheel.cancel(h), Err(HandleError::Stale));
                }
            }
            _ => {
                let delta = match rng.below(6) {
                    0 => 0,
                    1 => rng.below(shape.tick_ns),
                    2 => rng.below(shape.tick_ns * 70),
                    3 => rng.below(span_ns / 2 + 1),
                    4 => span_ns.saturating_mul(rng.below(5) + 1),
                    _ => rng.next() >> rng.below(64).max(4),
                };
                now = now.saturating_add(delta).min(MAX_TIME_NS);
                let mut fired = Vec::new();
                while let Some(e) = wheel.poll(now).unwrap() {
                    fired.push((e.deadline_ns, e.payload));
                }
                let mut expected = Vec::new();
                while let Some((&(d, s), _)) = model.first_key_value() {
                    if d > now {
                        break;
                    }
                    let id = model.remove(&(d, s)).unwrap();
                    expected.push((d, id));
                }
                assert_eq!(fired, expected, "seed {seed} now {now}");
                // Handles of fired entries become stale.
                live.retain(|(_, k)| model.contains_key(k) || k.0 > now);
                let mut i = 0;
                while i < live.len() {
                    if !model.contains_key(&live[i].1) {
                        stale.push(live.swap_remove(i).0);
                    } else {
                        i += 1;
                    }
                }
            }
        }
        assert_eq!(wheel.len(), model.len());
        if let Some((&(d, _), _)) = model.first_key_value() {
            let hint = wheel
                .next_wake_hint_ns()
                .expect("non-empty wheel has a hint");
            assert!(hint <= d.max(now), "hint {hint} later than deadline {d}");
        } else {
            assert_eq!(wheel.next_wake_hint_ns(), None);
        }
    }
    let mut rest = Vec::new();
    wheel.drain(|d, id| rest.push((d, id)));
    rest.sort();
    let mut want: Vec<_> = model.iter().map(|(&(d, _), &id)| (d, id)).collect();
    want.sort();
    assert_eq!(rest, want);
    for (h, _) in live {
        assert_eq!(wheel.cancel(h), Err(HandleError::Stale));
    }
}

const SHAPES: &[Shape] = &[
    // Tiny rings: heavy cascading and overflow.
    Shape {
        tick_ns: 1,
        slot_bits: 2,
        levels: 3,
        capacity: 64,
        start_ns: 0,
    },
    Shape {
        tick_ns: 7,
        slot_bits: 3,
        levels: 2,
        capacity: 128,
        start_ns: 5,
    },
    Shape {
        tick_ns: 1_000,
        slot_bits: 6,
        levels: 4,
        capacity: 256,
        start_ns: 123_456_789,
    },
    Shape {
        tick_ns: 1,
        slot_bits: 1,
        levels: 1,
        capacity: 32,
        start_ns: 0,
    },
    // Start near 2^64: high-bit transitions and the top of the range.
    Shape {
        tick_ns: 1,
        slot_bits: 4,
        levels: 3,
        capacity: 128,
        start_ns: u64::MAX - (1 << 20),
    },
    Shape {
        tick_ns: 3,
        slot_bits: 6,
        levels: 10,
        capacity: 128,
        start_ns: u64::MAX - (1 << 40),
    },
    Shape {
        tick_ns: 1 << 20,
        slot_bits: 5,
        levels: 12,
        capacity: 64,
        start_ns: 1 << 63,
    },
];

#[test]
fn wheel_matches_btreemap_reference() {
    for (i, shape) in SHAPES.iter().enumerate() {
        for seed in 0..40u64 {
            run(*shape, seed * 1_000 + i as u64, 2_000);
        }
    }
}

#[test]
fn equal_deadlines_fire_fifo_across_cascades() {
    let mut wheel = Wheel::new(WheelConfig {
        tick_ns: 1,
        slot_bits: 2,
        levels: 2,
        capacity: 64,
        start_ns: 0,
    })
    .unwrap();
    // Far deadline (overflow), cascades down twice before firing.
    let mut hs = Vec::new();
    for id in 0..10u32 {
        hs.push(wheel.insert(1_000, id).unwrap());
    }
    // Interleave earlier deadlines inserted later.
    hs.push(wheel.insert(999, 100).unwrap());
    let mut out = Vec::new();
    while let Some(e) = wheel.poll(2_000).unwrap() {
        out.push((e.deadline_ns, e.payload));
    }
    let mut want = vec![(999, 100)];
    want.extend((0..10).map(|i| (1_000, i)));
    assert_eq!(out, want);
    drop(hs);
}

#[test]
fn nothing_fires_early_within_a_tick() {
    let mut wheel = Wheel::new(WheelConfig {
        tick_ns: 1_000,
        ..WheelConfig::with_capacity(8)
    })
    .unwrap();
    let _a = wheel.insert(1_500, 'a').unwrap();
    let _b = wheel.insert(1_200, 'b').unwrap();
    assert!(wheel.poll(1_199).unwrap().is_none());
    assert_eq!(wheel.poll(1_200).unwrap().unwrap().payload, 'b');
    assert!(wheel.poll(1_499).unwrap().is_none());
    // Same-tick late insert into an opened tick keeps order.
    let _c = wheel.insert(1_300, 'c').unwrap();
    assert_eq!(wheel.poll(1_499).unwrap().unwrap().payload, 'c');
    assert_eq!(wheel.poll(1_500).unwrap().unwrap().payload, 'a');
    assert!(wheel.is_empty());
}
