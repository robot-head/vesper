//! Windowed max filters and the probe-wait PRNG.

/// Kathleen Nichols' windowed running maximum (three samples), as in Linux
/// `lib/minmax.c`. Time is a caller-chosen virtual clock (here: packet-timed
/// round count). Samples older than `window` time units expire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WindowedMax {
    samples: [(u64, u64); 3], // (time, value)
}

impl WindowedMax {
    /// A filter holding `value` at `time`.
    #[must_use]
    pub const fn new(time: u64, value: u64) -> Self {
        Self {
            samples: [(time, value); 3],
        }
    }

    /// Current maximum.
    #[must_use]
    pub const fn get(&self) -> u64 {
        self.samples[0].1
    }

    /// Add a sample and return the windowed maximum.
    pub fn update(&mut self, window: u64, time: u64, value: u64) -> u64 {
        let sample = (time, value);
        if value >= self.samples[0].1 || time.saturating_sub(self.samples[2].0) > window {
            *self = Self::new(time, value);
            return value;
        }
        if value >= self.samples[1].1 {
            self.samples[1] = sample;
            self.samples[2] = sample;
        } else if value >= self.samples[2].1 {
            self.samples[2] = sample;
        }
        let dt = time.saturating_sub(self.samples[0].0);
        if dt > window {
            self.samples[0] = self.samples[1];
            self.samples[1] = self.samples[2];
            self.samples[2] = sample;
            if time.saturating_sub(self.samples[0].0) > window {
                self.samples[0] = self.samples[1];
                self.samples[1] = self.samples[2];
                self.samples[2] = sample;
            }
        } else if self.samples[1].0 == self.samples[0].0 && dt > window / 4 {
            self.samples[1] = sample;
            self.samples[2] = sample;
        } else if self.samples[2].0 == self.samples[1].0 && dt > window / 2 {
            self.samples[2] = sample;
        }
        self.samples[0].1
    }
}

/// The two-slot bandwidth filter of draft-ietf-ccwg-bbr-07 §"BBR.max_bw Max
/// Filter": the max over the current and previous ProbeBW cycles, keyed by
/// `BBR.cycle_count`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CycleMax {
    previous: u64,
    current: u64,
}

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

/// SplitMix64: a tiny deterministic PRNG for the randomized probe wait.
/// Not cryptographic; seeded from the configuration so runs replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    /// Next 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..bound` (`bound > 0`), by rejection-free multiply-high.
    pub fn below(&mut self, bound: u64) -> u64 {
        ((self.next_u64() as u128 * bound as u128) >> 64) as u64
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
