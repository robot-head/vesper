//! Concrete half-open slice-projection checking from manuscript §4.3.
//!
//! This decides only supplied constant endpoints. It does not prove symbolic
//! bounds, create borrows, or integrate with the source checker.

/// An interval `[start, end)` in the same slice as every other input interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Interval {
    pub start: u64,
    pub end: u64,
}

/// Logical limits on input traversal and pairwise comparisons.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_intervals: usize,
    pub max_comparisons: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    IntervalLimit {
        count: usize,
        max: usize,
    },
    /// The pair count overflowed or exceeded the supplied comparison budget.
    ComparisonLimit {
        max: usize,
    },
    InvalidBounds {
        index: usize,
        interval: Interval,
        len: u64,
    },
    Overlap {
        first: usize,
        second: usize,
    },
}

/// Check bounds first, then disjointness in original pair order.
///
/// Empty ranges are disjoint from every range, including themselves. The
/// complete pairwise work is prepaid before examining any endpoint, so input
/// length alone cannot trigger an unbounded quadratic scan.
pub fn check_disjoint(len: u64, intervals: &[Interval], limits: Limits) -> Result<(), Error> {
    let n = intervals.len();
    if n > limits.max_intervals {
        return Err(Error::IntervalLimit {
            count: n,
            max: limits.max_intervals,
        });
    }
    // Divide before multiplying to avoid rejecting representable pair counts.
    let (a, b) = if n.is_multiple_of(2) {
        (n / 2, n.saturating_sub(1))
    } else {
        (n, n / 2)
    };
    if a.checked_mul(b)
        .is_none_or(|pairs| pairs > limits.max_comparisons)
    {
        return Err(Error::ComparisonLimit {
            max: limits.max_comparisons,
        });
    }
    for (index, &interval) in intervals.iter().enumerate() {
        if interval.start > interval.end || interval.end > len {
            return Err(Error::InvalidBounds {
                index,
                interval,
                len,
            });
        }
    }
    for (first, a) in intervals.iter().enumerate() {
        for (second, b) in intervals.iter().enumerate().skip(first + 1) {
            if a.start < a.end && b.start < b.end && a.start < b.end && b.start < a.end {
                return Err(Error::Overlap { first, second });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_element_set_oracle_on_small_domain() {
        let limits = Limits {
            max_intervals: 3,
            max_comparisons: 3,
        };
        for len in 0..=4 {
            let ranges: Vec<_> = (0..=len)
                .flat_map(|start| (start..=len).map(move |end| Interval { start, end }))
                .collect();
            for &a in &ranges {
                for &b in &ranges {
                    for &c in &ranges {
                        let items = [a, b, c];
                        let expected = (0..len).all(|i| {
                            items.iter().filter(|x| x.start <= i && i < x.end).count() <= 1
                        });
                        assert_eq!(
                            check_disjoint(len, &items, limits).is_ok(),
                            expected,
                            "{items:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn bounds_limits_and_pair_indices_are_explicit() {
        let limits = Limits {
            max_intervals: 3,
            max_comparisons: 3,
        };
        let r = |start, end| Interval { start, end };
        assert_eq!(
            check_disjoint(10, &[r(0, 4), r(4, 4), r(3, 8)], limits),
            Err(Error::Overlap {
                first: 0,
                second: 2
            })
        );
        assert_eq!(
            check_disjoint(10, &[r(0, 4), r(9, 8)], limits),
            Err(Error::InvalidBounds {
                index: 1,
                interval: r(9, 8),
                len: 10
            })
        );
        assert_eq!(
            check_disjoint(10, &[r(0, 11)], limits),
            Err(Error::InvalidBounds {
                index: 0,
                interval: r(0, 11),
                len: 10
            })
        );
        assert_eq!(
            check_disjoint(u64::MAX, &[r(u64::MAX, u64::MAX)], limits),
            Ok(())
        );
        assert_eq!(
            check_disjoint(0, &[r(0, 0); 4], limits),
            Err(Error::IntervalLimit { count: 4, max: 3 })
        );
        assert_eq!(
            check_disjoint(
                0,
                &[r(0, 0); 3],
                Limits {
                    max_intervals: 3,
                    max_comparisons: 2
                }
            ),
            Err(Error::ComparisonLimit { max: 2 })
        );
        assert_eq!(
            check_disjoint(
                0,
                &[],
                Limits {
                    max_intervals: 0,
                    max_comparisons: 0
                }
            ),
            Ok(())
        );
    }
}
