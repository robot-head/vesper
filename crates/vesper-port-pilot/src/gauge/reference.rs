//! The Rust build of the gauge: the same functions as the generated
//! binding, on plain Rust values.
use super::{Gauge, Phase, Sample, Span};

pub fn gauge_new(origin: i64) -> Gauge {
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

pub fn gauge_record(g: &mut Gauge, s: &Sample) {
    let level = s.level;
    let wide_level = i64::from(level);
    g.phase = if level > g.last {
        Phase::Rising
    } else if level < g.last {
        Phase::Falling
    } else {
        Phase::Idle
    };
    g.count = g.count.wrapping_add(1);
    g.total = g.total.wrapping_add(wide_level);
    g.last = level;
    g.small = i64::from(g.small).wrapping_add(i64::from(s.weight)) as i8;
    g.wide = u64::from(g.wide).wrapping_add(s.at) as u16;
    g.flags = u64::from(g.flags).wrapping_add(u64::from(s.weight)) as u32;
    g.seen = true;
    g.peak = Some(match g.peak {
        Some(p) if p >= s.at => p,
        _ => s.at,
    });
    g.span.lo = g.span.lo.min(wide_level);
    g.span.hi = g.span.hi.max(wide_level);
    g.ratio *= 2.0;
    g.index = g.index.wrapping_add(1);
    g.delta = i64::from(g.last).wrapping_sub(1) as isize;
}

pub fn gauge_scan(g: &mut Gauge, samples: &[Sample]) -> u64 {
    for s in samples {
        gauge_record(g, s);
    }
    g.count
}

pub fn sample_score(s: &Sample) -> i64 {
    let level = i64::from(s.level).wrapping_mul(3) as i32;
    i64::from(level).wrapping_add(i64::from(s.weight))
}

pub fn gauge_phase(g: &Gauge) -> Phase {
    g.phase
}

pub fn shift_samples(samples: &mut [Sample], by: i32) {
    for s in samples {
        s.level = i64::from(s.level).wrapping_add(i64::from(by)) as i32;
        s.weight = u64::from(s.weight).wrapping_add(1) as u8;
    }
}

pub fn span_width(s: Span) -> i64 {
    s.hi.wrapping_sub(s.lo)
}
