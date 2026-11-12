/// Exact bins for 0..=15, followed by eight bins per power-of-two octave,
/// through u64::MAX. No clamping, collision across octaves, or out-of-range input.
pub const HISTOGRAM_BINS: usize = 496;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistogramError {
    BinOverflow { index: usize },
    TotalOverflow,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinRange {
    pub lower: u64,
    pub upper: u64,
}
impl BinRange {
    /// Lower endpoint estimate. Zero is exact; for nonzero observations v in this bin,
    /// (v - lower) / v < 1/8. This is a conservative 12.5%, not a <2% claim.
    pub const fn estimate(self) -> u64 {
        self.lower
    }
}
/// 7,944 bytes: two 496-element u64 arrays plus one u64 total (no heap).
/// Exemplar IDs are caller-supplied opaque nonzero span IDs, not context owners.
#[derive(Debug)]
pub struct FastLogHistogram {
    buckets: [u64; HISTOGRAM_BINS],
    exemplars: [Option<core::num::NonZeroU64>; HISTOGRAM_BINS],
    total_count: u64,
}
impl Default for FastLogHistogram {
    fn default() -> Self {
        Self {
            buckets: [0; HISTOGRAM_BINS],
            exemplars: [None; HISTOGRAM_BINS],
            total_count: 0,
        }
    }
}
impl FastLogHistogram {
    pub const FOOTPRINT_BYTES: usize = core::mem::size_of::<Self>();
    pub fn bucket_index(value: u64) -> usize {
        if value < 16 {
            return value as usize;
        }
        let octave = 63 - value.leading_zeros() as usize;
        16 + (octave - 4) * 8 + ((value >> (octave - 3)) as usize - 8)
    }
    pub fn bin_range(index: usize) -> Option<BinRange> {
        if index >= HISTOGRAM_BINS {
            return None;
        }
        if index < 16 {
            return Some(BinRange {
                lower: index as u64,
                upper: index as u64,
            });
        }
        let octave = (index - 16) / 8 + 4;
        let sub = (index - 16) % 8;
        let width = 1_u64 << (octave - 3);
        let lower = (8 + sub as u64) * width;
        Some(BinRange {
            lower,
            upper: lower + (width - 1),
        })
    }
    /// All checks precede mutation: a failed record leaves counters/exemplars unchanged.
    pub fn record_with_exemplar(
        &mut self,
        value: u64,
        span_id: core::num::NonZeroU64,
    ) -> Result<(), HistogramError> {
        let index = Self::bucket_index(value);
        let count = self.buckets[index]
            .checked_add(1)
            .ok_or(HistogramError::BinOverflow { index })?;
        let total = self
            .total_count
            .checked_add(1)
            .ok_or(HistogramError::TotalOverflow)?;
        self.buckets[index] = count;
        self.exemplars[index] = Some(span_id);
        self.total_count = total;
        Ok(())
    }
    pub const fn total_count(&self) -> u64 {
        self.total_count
    }
    pub fn count(&self, index: usize) -> Option<u64> {
        self.buckets.get(index).copied()
    }
    pub fn exemplar(&self, index: usize) -> Option<core::num::NonZeroU64> {
        self.exemplars.get(index).copied().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_ranges_partition_u64_and_bound_error() {
        assert_eq!(FastLogHistogram::FOOTPRINT_BYTES, 7944);
        let mut next = 0_u128;
        for i in 0..HISTOGRAM_BINS {
            let r = FastLogHistogram::bin_range(i).unwrap();
            assert_eq!(u128::from(r.lower), next);
            assert!(r.lower <= r.upper);
            for value in [r.lower, r.lower + (r.upper - r.lower) / 2, r.upper] {
                assert_eq!(FastLogHistogram::bucket_index(value), i);
                if value != 0 {
                    assert!(u128::from(value - r.estimate()) * 8 < u128::from(value));
                }
            }
            next = u128::from(r.upper) + 1;
        }
        assert_eq!(next, 1_u128 << 64);
        assert_eq!(FastLogHistogram::bin_range(HISTOGRAM_BINS), None);
        assert_eq!(FastLogHistogram::bucket_index(8), 8);
        assert_eq!(FastLogHistogram::bucket_index(16), 16);
        assert_eq!(FastLogHistogram::bucket_index(u64::MAX), 495);
    }
    #[test]
    fn checked_counters_are_transactional() {
        let id = core::num::NonZeroU64::new(7).unwrap();
        let mut h = FastLogHistogram::default();
        h.record_with_exemplar(u64::MAX, id).unwrap();
        assert_eq!(h.exemplar(495), Some(id));
        h.total_count = u64::MAX;
        assert_eq!(
            h.record_with_exemplar(0, id),
            Err(HistogramError::TotalOverflow)
        );
        assert_eq!(h.count(0), Some(0));
        assert_eq!(h.exemplar(0), None);
        h.total_count = 0;
        h.buckets[1] = u64::MAX;
        assert_eq!(
            h.record_with_exemplar(1, id),
            Err(HistogramError::BinOverflow { index: 1 })
        );
        assert_eq!(h.total_count(), 0);
        assert_eq!(h.exemplar(1), None);
    }
}
