//! The pilot's export ABI v3 gauge (#934), run unchanged against the Rust
//! and the Vesper build: every mirror cell round-trips through Vesper, slices
//! of mirrors are read and updated in place, and no call allocates. The
//! Vesper-only tests at the end look at the mirrors themselves.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use vesper_port_pilot::gauge::*;

/// Counts allocations, to show the calls lend storage instead of copying it.
struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The expected state, written out independently of both builds.
fn model(g: &mut Gauge, s: Sample) {
    let level = i64::from(s.level);
    g.phase = match s.level.cmp(&g.last) {
        std::cmp::Ordering::Greater => Phase::Rising,
        std::cmp::Ordering::Less => Phase::Falling,
        std::cmp::Ordering::Equal => Phase::Idle,
    };
    g.count += 1;
    g.total = g.total.wrapping_add(level);
    g.last = s.level;
    g.small = g.small.wrapping_add(s.weight as i8);
    g.wide = g.wide.wrapping_add(s.at as u16);
    g.flags = g.flags.wrapping_add(u32::from(s.weight));
    g.seen = true;
    g.peak = Some(g.peak.map_or(s.at, |p| p.max(s.at)));
    g.span.lo = g.span.lo.min(level);
    g.span.hi = g.span.hi.max(level);
    g.ratio *= 2.0;
    g.index += 1;
    g.delta = (level - 1) as isize;
}

fn start(origin: i64) -> Gauge {
    Gauge {
        total: origin,
        span: Span {
            lo: origin,
            hi: origin,
        },
        ratio: 0.5,
        ..Gauge::default()
    }
}

/// Readings at every width's edges, then a deterministic stream.
fn readings(n: usize) -> Vec<Sample> {
    let mut out = vec![
        Sample {
            at: u64::MAX,
            level: i32::MAX,
            weight: u8::MAX,
        },
        Sample {
            at: 0,
            level: i32::MIN,
            weight: 0,
        },
        Sample {
            at: 1 << 63,
            level: -1,
            weight: 128,
        },
        Sample {
            at: 65_535,
            level: 0,
            weight: 127,
        },
    ];
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    while out.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.push(Sample {
            at: x,
            level: (x >> 16) as i32,
            weight: (x >> 3) as u8,
        });
    }
    out
}

#[test]
fn every_cell_round_trips() {
    let mut meter = Meter::new(-7);
    let mut expected = start(-7);
    assert_eq!(meter.snapshot(), expected);
    for s in readings(40) {
        meter.record(s);
        model(&mut expected, s);
        assert_eq!(meter.snapshot(), expected, "after {s:?}");
        assert_eq!(meter.phase(), expected.phase);
        assert_eq!(meter.count(), expected.count);
    }
}

#[test]
fn slices_are_read_in_place() {
    let all = readings(64);
    let mut samples = Samples::with_capacity(all.len());
    for s in &all {
        samples.push(*s);
    }
    let mut meter = Meter::new(0);
    let mut expected = start(0);
    for s in &all {
        model(&mut expected, *s);
    }
    assert_eq!(meter.scan(&samples), 64);
    assert_eq!(meter.snapshot(), expected);
    assert_eq!(meter.scan(&Samples::new()), 64);
}

#[test]
fn slices_are_updated_in_place() {
    let all = readings(16);
    let mut samples = Samples::with_capacity(all.len());
    for s in &all {
        samples.push(*s);
    }
    let buffer = samples.as_ptr();
    samples.shift(-3);
    assert_eq!(samples.as_ptr(), buffer);
    for (i, s) in all.iter().enumerate() {
        let got = samples.get(i).unwrap();
        assert_eq!(got.at, s.at);
        assert_eq!(got.level, s.level.wrapping_sub(3));
        assert_eq!(got.weight, s.weight.wrapping_add(1));
    }
}

#[test]
fn in_and_sink_parameters_leave_the_caller_alone() {
    let s = Sample {
        at: 9,
        level: i32::MAX,
        weight: 200,
    };
    assert_eq!(sample_score(s), i64::from(i32::MAX.wrapping_mul(3)) + 200);
    assert_eq!(sample_score(s), sample_score(s));
    assert_eq!(span_width(Span { lo: -5, hi: 7 }), 12);
    assert_eq!(
        span_width(Span {
            lo: i64::MIN,
            hi: 0
        }),
        i64::MIN
    );
}

#[test]
fn calls_do_not_allocate() {
    let all = readings(32);
    let mut samples = Samples::with_capacity(all.len());
    for s in &all {
        samples.push(*s);
    }
    let mut meter = Meter::new(1);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for round in 0..1000 {
        meter.record(all[round % all.len()]);
        let _ = meter.scan(&samples);
        samples.shift(1);
        let _ = meter.phase();
    }
    // Other tests may run on other threads, so allow their noise but not
    // one allocation per call.
    let during = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert!(during < 100, "{during} allocations over 4000 calls");
}

/// The Vesper build's mirrors: canonical narrow words written by Vesper,
/// and storage updated where it lives.
#[cfg(feature = "vesper-port")]
mod mirrors {
    use super::*;
    use vesper_port_pilot::gauge::binding::{self, repr};

    #[test]
    fn vesper_writes_canonical_words() {
        let mut meter = Meter::new(0);
        meter.record(Sample {
            at: 3,
            level: -2,
            weight: 200,
        });
        let state = meter.raw();
        assert_eq!(state.last.get(), -2);
        assert_eq!(state.last.bits(), (-2i64) as u64);
        assert_eq!(state.small.get(), 200u8 as i8);
        assert_eq!(state.small.bits(), i64::from(200u8 as i8) as u64);
        assert_eq!(state.seen.bits(), 1);
        assert_eq!(state.phase.get(), Phase::Falling);
        assert_eq!(state.peak.get(), Some(3));
        assert_eq!(state.span.lo, -2);
        assert_eq!(state.delta, -3);
    }

    #[test]
    fn rust_writes_reach_vesper_in_place() {
        let mut state = binding::gauge_new(0);
        state.last.set(i32::MAX);
        state.count = 41;
        state.span.hi = 1000;
        state.peak.set(Some(u64::MAX));
        binding::gauge_record(
            &mut state,
            &repr::Sample::from_value(&Sample {
                at: 1,
                level: 5,
                weight: 1,
            }),
        );
        assert_eq!(state.count, 42);
        assert_eq!(state.phase.get(), Phase::Falling);
        assert_eq!(state.span.hi, 1000);
        assert_eq!(state.peak.get(), Some(u64::MAX));
    }

    #[test]
    fn in_mirrors_are_not_written() {
        // `local.level = ..` in `sample_score` works on a copy: the caller's
        // mirror, lent in place, is unchanged.
        let s = repr::Sample::from_value(&Sample {
            at: 9,
            level: -4,
            weight: 7,
        });
        let before = s;
        assert_eq!(binding::sample_score(&s), -12 + 7);
        assert_eq!(s, before);
        let all = [s, s, s];
        let mut state = binding::gauge_new(0);
        assert_eq!(binding::gauge_scan(&mut state, &all), 3);
        assert_eq!(all, [before; 3]);
    }

    #[test]
    fn mirrors_convert_both_ways() {
        let g = Gauge {
            count: 1,
            total: -1,
            last: i32::MIN,
            small: i8::MIN,
            wide: u16::MAX,
            flags: u32::MAX,
            seen: true,
            phase: Phase::Rising,
            peak: None,
            span: Span { lo: -1, hi: 1 },
            ratio: -0.25,
            index: usize::MAX,
            delta: isize::MIN,
        };
        let m = repr::Gauge::from_value(&g);
        assert_eq!(m.to_value(), g);
        assert_eq!(binding::gauge_phase(&m), Phase::Rising);
        assert_eq!(
            vesper_port_abi::mirror_words(&m)[3],
            i64::from(i8::MIN) as u64
        );
    }
}
