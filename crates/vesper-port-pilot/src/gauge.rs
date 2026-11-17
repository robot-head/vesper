//! The pilot's export ABI v3 library (`vesper/gauge.ves`, Stage 1 perf
//! lane 4, #934): state kept in Rust storage with the Vesper layout and lent
//! to Vesper in place.
//!
//! [`Meter`] and [`Samples`] are the pattern a port follows to drop the
//! boundary copies: in the Vesper build they hold the binding's `#[repr(C)]`
//! mirrors (`binding::repr::Gauge`, `binding::repr::Sample`), so every call
//! passes a pointer to the caller's own storage. In the Rust build they hold
//! the plain Rust values. `tests/gauge.rs` runs unchanged against both.

/// The direction of the last reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    Rising,
    Falling,
}

/// One reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sample {
    pub at: u64,
    pub level: i32,
    pub weight: u8,
}

/// The range of levels seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub lo: i64,
    pub hi: i64,
}

/// A snapshot of a [`Meter`]'s state.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Gauge {
    pub count: u64,
    pub total: i64,
    pub last: i32,
    pub small: i8,
    pub wide: u16,
    pub flags: u32,
    pub seen: bool,
    pub phase: Phase,
    pub peak: Option<u64>,
    pub span: Span,
    pub ratio: f64,
    pub index: usize,
    pub delta: isize,
}

#[cfg(not(feature = "vesper-port"))]
mod reference;
#[cfg(not(feature = "vesper-port"))]
use reference as imp;

// Generated and committed, outside src/ (see the porting recipe).
#[cfg(feature = "vesper-port")]
#[rustfmt::skip]
#[path = "../port/gauge.rs"]
pub mod binding;
#[cfg(feature = "vesper-port")]
use binding as imp;

#[cfg(feature = "vesper-port")]
type State = binding::repr::Gauge;
#[cfg(not(feature = "vesper-port"))]
type State = Gauge;

#[cfg(feature = "vesper-port")]
type Item = binding::repr::Sample;
#[cfg(not(feature = "vesper-port"))]
type Item = Sample;

#[cfg(feature = "vesper-port")]
fn item(s: &Sample) -> Item {
    Item::from_value(s)
}
#[cfg(not(feature = "vesper-port"))]
fn item(s: &Sample) -> Item {
    *s
}

#[cfg(feature = "vesper-port")]
fn sample(i: &Item) -> Sample {
    i.to_value()
}
#[cfg(not(feature = "vesper-port"))]
fn sample(i: &Item) -> Sample {
    *i
}

/// A gauge whose state stays in place across calls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meter {
    state: State,
}

impl Meter {
    pub fn new(origin: i64) -> Self {
        Self {
            state: imp::gauge_new(origin),
        }
    }

    /// Record one reading.
    pub fn record(&mut self, s: Sample) {
        imp::gauge_record(&mut self.state, &item(&s));
    }

    /// Record every reading of `samples`; returns the new count.
    pub fn scan(&mut self, samples: &Samples) -> u64 {
        imp::gauge_scan(&mut self.state, &samples.items)
    }

    /// The number of readings, read straight from the state.
    pub fn count(&self) -> u64 {
        self.state.count
    }

    /// The direction of the last reading.
    pub fn phase(&self) -> Phase {
        imp::gauge_phase(&self.state)
    }

    /// The whole state as a Rust value.
    #[cfg(feature = "vesper-port")]
    pub fn snapshot(&self) -> Gauge {
        self.state.to_value()
    }

    /// The whole state as a Rust value.
    #[cfg(not(feature = "vesper-port"))]
    pub fn snapshot(&self) -> Gauge {
        self.state
    }

    /// The storage Vesper updates in place (the mirror in the Vesper build).
    #[cfg(feature = "vesper-port")]
    pub fn raw(&mut self) -> &mut binding::repr::Gauge {
        &mut self.state
    }
}

/// Readings kept in the Vesper layout, lent to Vesper as a slice in place.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Samples {
    items: Vec<Item>,
}

impl Samples {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            items: Vec::with_capacity(n),
        }
    }

    pub fn push(&mut self, s: Sample) {
        self.items.push(item(&s));
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, i: usize) -> Option<Sample> {
        self.items.get(i).map(sample)
    }

    /// Add `by` to every level and 1 to every weight, in place.
    pub fn shift(&mut self, by: i32) {
        imp::shift_samples(&mut self.items, by);
    }

    /// The element buffer (for tests that check nothing was reallocated).
    pub fn as_ptr(&self) -> *const u8 {
        self.items.as_ptr().cast()
    }
}

/// A score of one reading (the callee works on a copy of its `in` mirror).
pub fn sample_score(s: Sample) -> i64 {
    imp::sample_score(&item(&s))
}

/// `hi - lo`, wrapping.
#[cfg(feature = "vesper-port")]
pub fn span_width(s: Span) -> i64 {
    imp::span_width(binding::repr::Span::from_value(&s))
}

/// `hi - lo`, wrapping.
#[cfg(not(feature = "vesper-port"))]
pub fn span_width(s: Span) -> i64 {
    imp::span_width(s)
}
