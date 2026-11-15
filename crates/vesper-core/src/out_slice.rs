//! Concrete output-slice initialization coverage (manuscript §4.4).
//!
//! This tracks constant intervals only. It does not allocate destination memory,
//! create borrows, or integrate with source checking or unwind control flow.

use core::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_intervals: usize,
    /// Maximum units of linear scan/movement per operation.
    pub max_work: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidBounds { start: u64, end: u64, len: u64 },
    AlreadyInitialized { start: u64, end: u64 },
    IntervalLimit { max: usize },
    WorkLimit { max: usize },
    AllocationFailed,
}

/// Path-local initialization state; clone it when analyzing a branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutSlice {
    len: u64,
    limits: Limits,
    // Nonempty intervals sorted by start; disjoint but not necessarily merged.
    initialized: Vec<Range<u64>>,
}

impl OutSlice {
    pub fn new(len: u64, limits: Limits) -> Self {
        Self {
            len,
            limits,
            initialized: Vec::new(),
        }
    }

    pub fn initialized(&self) -> &[Range<u64>] {
        &self.initialized
    }

    /// Record one completed write. Errors leave the state unchanged.
    pub fn initialize(&mut self, range: Range<u64>) -> Result<(), Error> {
        if range.start > range.end || range.end > self.len {
            return Err(Error::InvalidBounds {
                start: range.start,
                end: range.end,
                len: self.len,
            });
        }
        if range.start == range.end {
            return Ok(());
        }
        if self.initialized.len() >= self.limits.max_intervals {
            return Err(Error::IntervalLimit {
                max: self.limits.max_intervals,
            });
        }
        // One scan, at most one move during growth, and one during insertion.
        if self
            .initialized
            .len()
            .checked_mul(3)
            .is_none_or(|n| n > self.limits.max_work)
        {
            return Err(Error::WorkLimit {
                max: self.limits.max_work,
            });
        }
        let mut at = self.initialized.len();
        for (i, old) in self.initialized.iter().enumerate() {
            if range.start < old.end && old.start < range.end {
                return Err(Error::AlreadyInitialized {
                    start: range.start,
                    end: range.end,
                });
            }
            if at == self.initialized.len() && range.start < old.start {
                at = i;
            }
        }
        self.initialized
            .try_reserve(1)
            .map_err(|_| Error::AllocationFailed)?;
        self.initialized.insert(at, range);
        Ok(())
    }

    /// Whether every element has been initialized, as required on normal return.
    pub fn is_complete(&self) -> Result<bool, Error> {
        if self.initialized.len() > self.limits.max_work {
            return Err(Error::WorkLimit {
                max: self.limits.max_work,
            });
        }
        let mut end = 0;
        for range in &self.initialized {
            if range.start != end {
                return Ok(false);
            }
            end = range.end;
        }
        Ok(end == self.len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_order_adjacent_writes_cover_and_holes_remain() {
        let mut state = OutSlice::new(
            6,
            Limits {
                max_intervals: 4,
                max_work: 12,
            },
        );
        state.initialize(4..6).unwrap();
        state.initialize(0..2).unwrap();
        assert_eq!(state.is_complete(), Ok(false));
        state.initialize(2..4).unwrap();
        assert_eq!(state.is_complete(), Ok(true));
        let before = state.clone();
        assert_eq!(
            state.initialize(1..3),
            Err(Error::AlreadyInitialized { start: 1, end: 3 })
        );
        assert_eq!(
            state.initialize(5..7),
            Err(Error::InvalidBounds {
                start: 5,
                end: 7,
                len: 6
            })
        );
        assert_eq!(state, before);
        assert_eq!(
            OutSlice::new(
                0,
                Limits {
                    max_intervals: 0,
                    max_work: 0
                }
            )
            .is_complete(),
            Ok(true)
        );
    }

    #[test]
    fn all_small_write_sequences_match_element_oracle() {
        let limits = Limits {
            max_intervals: 3,
            max_work: 9,
        };
        for len in 0..=4u64 {
            let ranges: Vec<_> = (0..=len)
                .flat_map(|start| (start..=len).map(move |end| start..end))
                .collect();
            for a in &ranges {
                for b in &ranges {
                    for c in &ranges {
                        let mut state = OutSlice::new(len, limits);
                        let mut used = vec![false; len as usize];
                        for range in [a, b, c] {
                            let overlap = range.clone().any(|i| used[i as usize]);
                            let before = state.clone();
                            let result = state.initialize(range.clone());
                            assert_eq!(result.is_err(), overlap);
                            if overlap {
                                assert_eq!(state, before);
                            } else {
                                for i in range.clone() {
                                    used[i as usize] = true;
                                }
                            }
                            assert_eq!(state.is_complete().unwrap(), used.iter().all(|x| *x));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn resource_failures_are_transactional() {
        let mut state = OutSlice::new(
            4,
            Limits {
                max_intervals: 1,
                max_work: 1,
            },
        );
        state.initialize(0..1).unwrap();
        let before = state.clone();
        assert_eq!(state.initialize(2..3), Err(Error::IntervalLimit { max: 1 }));
        assert_eq!(state, before);
        let mut work = OutSlice::new(
            4,
            Limits {
                max_intervals: 2,
                max_work: 1,
            },
        );
        work.initialize(0..1).unwrap();
        let before = work.clone();
        assert_eq!(work.initialize(2..3), Err(Error::WorkLimit { max: 1 }));
        assert_eq!(work, before);
    }
}
