//! Windowed max filters and the probe-wait PRNG.

// Under export ABI v3 a filter crosses as its Vesper-layout mirror; the
// filters inside `Bbrv3` are lent in place with the controller.
#[cfg(feature = "vesper-port")]
use crate::port::repr::{CycleMax as CycleMirror, WindowedMax as Mirror};

/// Kathleen Nichols' windowed running maximum (three samples), as in Linux
/// `lib/minmax.c`. Time is a caller-chosen virtual clock (here: packet-timed
/// round count). Samples older than `window` time units expire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
///
/// The three `(time, value)` samples are named fields (`t0`/`v0` is the
/// best), so that the Vesper port can share the layout.
pub struct WindowedMax {
    pub(crate) t0: u64,
    pub(crate) v0: u64,
    pub(crate) t1: u64,
    pub(crate) v1: u64,
    pub(crate) t2: u64,
    pub(crate) v2: u64,
}

impl WindowedMax {
    /// Current maximum.
    #[must_use]
    pub const fn get(&self) -> u64 {
        self.v0
    }
}

#[cfg(not(feature = "vesper-port"))]
impl WindowedMax {
    /// A filter holding `value` at `time`.
    #[must_use]
    pub const fn new(time: u64, value: u64) -> Self {
        Self {
            t0: time,
            v0: value,
            t1: time,
            v1: value,
            t2: time,
            v2: value,
        }
    }

    const fn sample(&self, i: usize) -> (u64, u64) {
        match i {
            0 => (self.t0, self.v0),
            1 => (self.t1, self.v1),
            _ => (self.t2, self.v2),
        }
    }

    fn set(&mut self, i: usize, (t, v): (u64, u64)) {
        match i {
            0 => (self.t0, self.v0) = (t, v),
            1 => (self.t1, self.v1) = (t, v),
            _ => (self.t2, self.v2) = (t, v),
        }
    }

    /// Add a sample and return the windowed maximum.
    pub fn update(&mut self, window: u64, time: u64, value: u64) -> u64 {
        let sample = (time, value);
        if value >= self.sample(0).1 || time.saturating_sub(self.sample(2).0) > window {
            *self = Self::new(time, value);
            return value;
        }
        if value >= self.sample(1).1 {
            self.set(1, sample);
            self.set(2, sample);
        } else if value >= self.sample(2).1 {
            self.set(2, sample);
        }
        let dt = time.saturating_sub(self.sample(0).0);
        if dt > window {
            self.set(0, self.sample(1));
            self.set(1, self.sample(2));
            self.set(2, sample);
            if time.saturating_sub(self.sample(0).0) > window {
                self.set(0, self.sample(1));
                self.set(1, self.sample(2));
                self.set(2, sample);
            }
        } else if self.sample(1).0 == self.sample(0).0 && dt > window / 4 {
            self.set(1, sample);
            self.set(2, sample);
        } else if self.sample(2).0 == self.sample(1).0 && dt > window / 2 {
            self.set(2, sample);
        }
        self.sample(0).1
    }
}

#[cfg(feature = "vesper-port")]
impl WindowedMax {
    /// A filter holding `value` at `time`.
    #[must_use]
    pub fn new(time: u64, value: u64) -> Self {
        crate::port::windowed_max_new(time, value).to_value()
    }

    /// Add a sample and return the windowed maximum.
    pub fn update(&mut self, window: u64, time: u64, value: u64) -> u64 {
        let mut m = Mirror::from_value(self);
        let max = crate::port::windowed_max_update(&mut m, window, time, value);
        *self = m.to_value();
        max
    }
}

/// The two-slot bandwidth filter of draft-ietf-ccwg-bbr-07 §"BBR.max_bw Max
/// Filter": the max over the current and previous ProbeBW cycles, keyed by
/// `BBR.cycle_count`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CycleMax {
    pub(crate) previous: u64,
    pub(crate) current: u64,
}

#[cfg(not(feature = "vesper-port"))]
impl CycleMax {
    /// Current maximum over both slots.
    #[must_use]
    pub const fn get(&self) -> u64 {
        if self.previous > self.current {
            self.previous
        } else {
            self.current
        }
    }

    /// Record a sample in the current cycle.
    pub fn update(&mut self, value: u64) -> u64 {
        if value > self.current {
            self.current = value;
        }
        self.get()
    }

    /// Start a new cycle: the current slot becomes the previous one.
    pub fn advance(&mut self) -> u64 {
        self.previous = self.current;
        self.current = 0;
        self.get()
    }
}

#[cfg(feature = "vesper-port")]
impl CycleMax {
    /// Current maximum over both slots.
    #[must_use]
    pub fn get(&self) -> u64 {
        crate::port::cycle_max_get(&CycleMirror::from_value(self))
    }

    /// Record a sample in the current cycle.
    pub fn update(&mut self, value: u64) -> u64 {
        let mut m = CycleMirror::from_value(self);
        let max = crate::port::cycle_max_update(&mut m, value);
        *self = m.to_value();
        max
    }

    /// Start a new cycle: the current slot becomes the previous one.
    pub fn advance(&mut self) -> u64 {
        let mut m = CycleMirror::from_value(self);
        let max = crate::port::cycle_max_advance(&mut m);
        *self = m.to_value();
        max
    }
}

/// SplitMix64: a tiny deterministic PRNG for the randomized probe wait.
/// Not cryptographic; seeded from the configuration so runs replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitMix64(pub u64);

/// SplitMix64 increment (the golden-ratio "gamma").
#[cfg(not(feature = "vesper-port"))]
const SPLITMIX_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
/// SplitMix64 finalizer multipliers.
#[cfg(not(feature = "vesper-port"))]
const SPLITMIX_MUL_1: u64 = 0xbf58_476d_1ce4_e5b9;
#[cfg(not(feature = "vesper-port"))]
const SPLITMIX_MUL_2: u64 = 0x94d0_49bb_1331_11eb;

#[cfg(not(feature = "vesper-port"))]
impl SplitMix64 {
    /// Next 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(SPLITMIX_GAMMA);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(SPLITMIX_MUL_1);
        z = (z ^ (z >> 27)).wrapping_mul(SPLITMIX_MUL_2);
        z ^ (z >> 31)
    }

    /// Uniform in `0..bound` (`bound > 0`), by rejection-free multiply-high.
    pub fn below(&mut self, bound: u64) -> u64 {
        ((self.next_u64() as u128 * bound as u128) >> 64) as u64
    }
}

#[cfg(feature = "vesper-port")]
impl SplitMix64 {
    /// Next 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        crate::port::splitmix_next(&mut self.0)
    }

    /// Uniform in `0..bound` (`bound > 0`), by rejection-free multiply-high.
    pub fn below(&mut self, bound: u64) -> u64 {
        crate::port::splitmix_below(&mut self.0, bound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windowed_max_expires_old_peaks() {
        let mut f = WindowedMax::new(0, 0);
        assert_eq!(f.update(10, 0, 100), 100);
        assert_eq!(f.update(10, 3, 50), 100);
        assert_eq!(f.update(10, 8, 70), 100);
        // The 100 sample (time 0) is older than the window at time 11.
        assert_eq!(f.update(10, 11, 40), 70);
        assert_eq!(f.update(10, 30, 5), 5);
        // Window 1 (Startup) forgets after one round.
        let mut g = WindowedMax::new(0, 0);
        g.update(1, 0, 9);
        assert_eq!(g.update(1, 1, 3), 9);
        assert_eq!(g.update(1, 2, 3), 3);
    }

    #[test]
    fn cycle_max_keeps_two_cycles() {
        let mut f = CycleMax::default();
        f.update(10);
        f.update(7);
        assert_eq!(f.get(), 10);
        assert_eq!(f.advance(), 10);
        f.update(4);
        assert_eq!(f.get(), 10);
        assert_eq!(f.advance(), 4);
        assert_eq!(f.advance(), 0);
    }

    #[test]
    fn prng_is_deterministic_and_bounded() {
        let mut a = SplitMix64(7);
        let mut b = SplitMix64(7);
        for _ in 0..1000 {
            let x = a.below(2);
            assert_eq!(x, b.below(2));
            assert!(x < 2);
        }
    }
}
