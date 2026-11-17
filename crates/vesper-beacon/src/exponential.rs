//! A bounded base-2 exponential histogram (the OTLP
//! `ExponentialHistogramDataPoint` bucket layout) over `u64` observations.
//!
//! Bucket `i` at scale `s` holds values in `(2^(i / 2^s), 2^((i + 1) / 2^s)]`;
//! zero goes to the zero bucket (threshold 0). At most
//! [`EXP_HISTOGRAM_BUCKETS`] buckets are kept, in a fixed array (no heap).
//! When an observation would need more, the histogram halves its
//! resolution (scale minus one, merging adjacent bucket pairs) as often as
//! needed, as the OpenTelemetry SDK aggregator does. It starts at
//! [`EXP_HISTOGRAM_MAX_SCALE`].
//!
//! Index mapping: exact for scale <= 0 and for powers of two at every
//! scale. For positive scales the fractional part of `log2(v)` is computed
//! with a 63-bit fixed-point squaring loop rather than floating point (no
//! `std` here), so a value within about 2^-60 (relative) of a bucket
//! boundary may land in the neighbouring bucket; integers are never exactly
//! on such a boundary except at powers of two.

use crate::HistogramError;

/// Buckets kept (the OpenTelemetry SDK default maximum size).
pub const EXP_HISTOGRAM_BUCKETS: usize = 160;
/// Starting (finest) scale.
pub const EXP_HISTOGRAM_MAX_SCALE: i32 = 20;
/// Coarsest scale; at -10 every `u64` maps to bucket 0 or -1.
pub const EXP_HISTOGRAM_MIN_SCALE: i32 = -10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExponentialHistogram {
    scale: i32,
    /// Index of `counts[0]`.
    offset: i32,
    len: usize,
    counts: [u64; EXP_HISTOGRAM_BUCKETS],
    zero_count: u64,
    total_count: u64,
    sum: u128,
    min: u64,
    max: u64,
}
impl Default for ExponentialHistogram {
    fn default() -> Self {
        Self {
            scale: EXP_HISTOGRAM_MAX_SCALE,
            offset: 0,
            len: 0,
            counts: [0; EXP_HISTOGRAM_BUCKETS],
            zero_count: 0,
            total_count: 0,
            sum: 0,
            min: u64::MAX,
            max: 0,
        }
    }
}

/// `floor(2^scale * log2(v))` for `v >= 1` and `0 < scale <= 20`, by the
/// binary logarithm squaring method on a Q1.63 mantissa.
fn log2_fixed(v: u64, scale: i32) -> i64 {
    let k = 63 - v.leading_zeros() as i64;
    let mut x = u128::from(v) << (63 - k);
    let mut frac = 0i64;
    for _ in 0..scale {
        x = (x * x) >> 63;
        frac <<= 1;
        if x >= 1 << 64 {
            frac |= 1;
            x >>= 1;
        }
    }
    (k << scale) + frac
}

/// Bucket index of `v >= 1` at `scale`.
pub fn exp_bucket_index(v: u64, scale: i32) -> i32 {
    debug_assert!(v >= 1);
    debug_assert!((EXP_HISTOGRAM_MIN_SCALE..=EXP_HISTOGRAM_MAX_SCALE).contains(&scale));
    if scale <= 0 {
        // ceil(log2 v) - 1, then coarsen.
        let ceil = 64 - (v - 1).leading_zeros() as i32;
        return (ceil - 1) >> (-scale);
    }
    if v.is_power_of_two() {
        let k = 63 - v.leading_zeros() as i32;
        return (k << scale) - 1;
    }
    // No integer other than a power of two lies on a boundary, so
    // ceil(x) - 1 = floor(x).
    log2_fixed(v, scale) as i32
}

impl ExponentialHistogram {
    pub const FOOTPRINT_BYTES: usize = core::mem::size_of::<Self>();
    pub fn scale(&self) -> i32 {
        self.scale
    }
    /// Index of the first positive bucket in [`Self::positive_counts`].
    pub fn offset(&self) -> i32 {
        self.offset
    }
    pub fn positive_counts(&self) -> &[u64] {
        &self.counts[..self.len]
    }
    pub fn zero_count(&self) -> u64 {
        self.zero_count
    }
    pub fn total_count(&self) -> u64 {
        self.total_count
    }
    pub fn sum(&self) -> u128 {
        self.sum
    }
    pub fn min(&self) -> Option<u64> {
        (self.total_count != 0).then_some(self.min)
    }
    pub fn max(&self) -> Option<u64> {
        (self.total_count != 0).then_some(self.max)
    }

    /// The downscale (`scale` reduction) needed so that `index` fits the
    /// window alongside the occupied buckets.
    fn needed_shift(&self, index: i32) -> u32 {
        if self.len == 0 {
            return 0;
        }
        let lo = i64::from(self.offset.min(index));
        let hi = i64::from((self.offset + self.len as i32 - 1).max(index));
        let mut shift = 0;
        while ((hi >> shift) - (lo >> shift) + 1) as usize > EXP_HISTOGRAM_BUCKETS {
            shift += 1;
        }
        shift
    }

    fn downscale(&mut self, shift: u32) {
        if shift == 0 {
            return;
        }
        let base = self.offset >> shift;
        for j in 0..self.len {
            let c = self.counts[j];
            self.counts[j] = 0;
            let to = ((self.offset + j as i32) >> shift) - base;
            self.counts[to as usize] += c;
        }
        let last = (self.offset + self.len as i32 - 1) >> shift;
        self.len = (last - base + 1) as usize;
        self.offset = base;
        self.scale -= shift as i32;
    }

    /// Records one observation. All checks precede mutation: on error the
    /// histogram is unchanged.
    pub fn record(&mut self, value: u64) -> Result<(), HistogramError> {
        let total = self
            .total_count
            .checked_add(1)
            .ok_or(HistogramError::TotalOverflow)?;
        let sum = self
            .sum
            .checked_add(u128::from(value))
            .ok_or(HistogramError::TotalOverflow)?;
        if value == 0 {
            self.zero_count = self
                .zero_count
                .checked_add(1)
                .ok_or(HistogramError::TotalOverflow)?;
        } else {
            let mut index = exp_bucket_index(value, self.scale);
            let shift = self.needed_shift(index);
            // At the minimum scale every u64 fits in two buckets, so the
            // window never needs to go below it.
            debug_assert!(self.scale - shift as i32 >= EXP_HISTOGRAM_MIN_SCALE);
            self.downscale(shift);
            index >>= shift;
            if self.len == 0 {
                self.offset = index;
                self.len = 1;
            } else if index < self.offset {
                let grow = (self.offset - index) as usize;
                self.counts.copy_within(0..self.len, grow);
                self.counts[..grow].fill(0);
                self.offset = index;
                self.len += grow;
            } else if index >= self.offset + self.len as i32 {
                self.len = (index - self.offset) as usize + 1;
            }
            // Bucket counts are bounded by the checked total.
            self.counts[(index - self.offset) as usize] += 1;
        }
        self.total_count = total;
        self.sum = sum;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        Ok(())
    }
}
