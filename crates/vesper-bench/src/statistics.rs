use crate::Error;
use std::cmp::Ordering;
pub const MAX_EPOCHS: usize = 64;
/// Exact Mann–Whitney rank counts, multiplied by two so ties remain integral.
/// `twice_u_first` counts pairs where the first observation exceeds the second;
/// `twice_u_second` counts the reverse. Each tie contributes one to both.
/// These descriptive counts do not supply a p-value or confidence interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MannWhitneyRanks {
    pub first_count: usize,
    pub second_count: usize,
    pub twice_u_first: u64,
    pub twice_u_second: u64,
    pub tied_pairs: u64,
}

/// Compute rank counts for two nonempty samples, each bounded by `MAX_EPOCHS`.
/// This performs at most 4096 comparisons and does not allocate. A subsequent
/// independent-sample inference would need assumptions about independence and
/// distributions; in particular, these ranks do not make paired ABBA epochs
/// independent, and this function does not participate in the regression gate.
pub fn mann_whitney_ranks(first: &[u64], second: &[u64]) -> Result<MannWhitneyRanks, Error> {
    if first.is_empty() || second.is_empty() {
        return Err(Error::EmptyEvidence);
    }
    if first.len() > MAX_EPOCHS || second.len() > MAX_EPOCHS {
        return Err(Error::Limit("rank sample size"));
    }
    let mut result = MannWhitneyRanks {
        first_count: first.len(),
        second_count: second.len(),
        twice_u_first: 0,
        twice_u_second: 0,
        tied_pairs: 0,
    };
    for &x in first {
        for &y in second {
            match x.cmp(&y) {
                Ordering::Greater => result.twice_u_first += 2,
                Ordering::Less => result.twice_u_second += 2,
                Ordering::Equal => {
                    result.twice_u_first += 1;
                    result.twice_u_second += 1;
                    result.tied_pairs += 1;
                }
            }
        }
    }
    Ok(result)
}

/// Exact two-sided permutation probability as an unreduced fraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactMannWhitney {
    pub u_first: u64,
    pub numerator: u128,
    pub denominator: u128,
}

/// Exact no-ties Mann–Whitney permutation probability, with a two-sided
/// probability defined as twice the smaller inclusive tail, capped at one.
/// Each sample has at most 32 observations. Any repeated observation, including
/// repetitions within one sample, is rejected: this distribution is untied.
/// Inference assumes observations are exchangeable under the null hypothesis;
/// paired ABBA epochs do not automatically meet that assumption. This function
/// does not run the paired regression gate or produce a confidence interval.
pub fn mann_whitney_exact(first: &[u64], second: &[u64]) -> Result<ExactMannWhitney, Error> {
    if first.is_empty() || second.is_empty() {
        return Err(Error::EmptyEvidence);
    }
    if first.len() > 32 || second.len() > 32 {
        return Err(Error::Limit("exact rank sample size"));
    }
    let (n, m) = (first.len(), second.len());
    let mut observations = [0u64; 64];
    observations[..n].copy_from_slice(first);
    observations[n..n + m].copy_from_slice(second);
    observations[..n + m].sort_unstable();
    if observations[..n + m]
        .windows(2)
        .any(|pair| pair[0] == pair[1])
    {
        return Err(Error::TiedObservations);
    }

    let ranks = mann_whitney_ranks(first, second)?;
    let observed =
        usize::try_from(ranks.twice_u_first / 2).map_err(|_| Error::ArithmeticOverflow)?;
    let max_u = n * m;
    let width = max_u + 1;
    let cells = (m + 1) * width;
    let mut previous = Vec::new();
    previous
        .try_reserve_exact(cells)
        .map_err(|_| Error::AllocationFailed)?;
    previous.resize(cells, 0u128);
    let mut current = Vec::new();
    current
        .try_reserve_exact(cells)
        .map_err(|_| Error::AllocationFailed)?;
    current.resize(cells, 0u128);
    // For zero first-sample observations, each second-sample prefix has U=0.
    for j in 0..=m {
        previous[j * width] = 1;
    }
    for i in 1..=n {
        current.fill(0);
        current[0] = 1;
        for j in 1..=m {
            for u in 0..=i * j {
                let from_second = current[(j - 1) * width + u];
                let from_first = if u >= j {
                    previous[j * width + u - j]
                } else {
                    0
                };
                current[j * width + u] = from_second
                    .checked_add(from_first)
                    .ok_or(Error::ArithmeticOverflow)?;
            }
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let distribution = &previous[m * width..(m + 1) * width];
    let mut denominator = 0u128;
    let mut tail = 0u128;
    let tail_limit = observed.min(max_u - observed);
    for (u, &count) in distribution.iter().enumerate() {
        denominator = denominator
            .checked_add(count)
            .ok_or(Error::ArithmeticOverflow)?;
        if u <= tail_limit {
            tail = tail.checked_add(count).ok_or(Error::ArithmeticOverflow)?;
        }
    }
    let numerator = tail
        .checked_mul(2)
        .ok_or(Error::ArithmeticOverflow)?
        .min(denominator);
    Ok(ExactMannWhitney {
        u_first: observed as u64,
        numerator,
        denominator,
    })
}
/// Positive elapsed totals, with equal operation counts for the two arms.
/// This may also represent synthetic statistical test data, not measured cycles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairedSample {
    baseline: u64,
    candidate: u64,
    operations: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BasisPointBounds {
    pub floor: i128,
    pub ceil: i128,
}
impl PairedSample {
    pub fn new(
        baseline_total: u64,
        candidate_total: u64,
        operations_per_arm: u64,
    ) -> Result<Self, Error> {
        if baseline_total == 0 || candidate_total == 0 || operations_per_arm == 0 {
            return Err(Error::NonPositiveEvidence);
        }
        Ok(Self {
            baseline: baseline_total,
            candidate: candidate_total,
            operations: operations_per_arm,
        })
    }
    pub fn baseline_total(self) -> u64 {
        self.baseline
    }
    pub fn candidate_total(self) -> u64 {
        self.candidate
    }
    pub fn operations_per_arm(self) -> u64 {
        self.operations
    }
    /// Directed rounding is presentation only. Gates compare exact ratios.
    pub fn regression_bp_bounds(self) -> BasisPointBounds {
        // Difference of u64s and scaling by 10,000 fit strictly inside i128.
        let scaled = (i128::from(self.candidate) - i128::from(self.baseline)) * 10_000;
        let denominator = i128::from(self.baseline);
        let floor = scaled.div_euclid(denominator);
        BasisPointBounds {
            floor,
            ceil: floor + i128::from(scaled.rem_euclid(denominator) != 0),
        }
    }
    fn compare_ratio(self, other: Self) -> Ordering {
        // u64 * u64 fits u128, including maximal values: no narrow product.
        (u128::from(self.candidate) * u128::from(other.baseline))
            .cmp(&(u128::from(other.candidate) * u128::from(self.baseline)))
    }
    fn compare_threshold(self, bp: i64) -> Result<Ordering, Error> {
        if bp < -9999 {
            return Err(Error::InvalidThreshold);
        }
        let factor =
            u128::try_from(i128::from(bp) + 10_000).map_err(|_| Error::ArithmeticOverflow)?;
        let left = u128::from(self.candidate)
            .checked_mul(10_000)
            .ok_or(Error::ArithmeticOverflow)?;
        let right = u128::from(self.baseline)
            .checked_mul(factor)
            .ok_or(Error::ArithmeticOverflow)?;
        Ok(left.cmp(&right))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Pass,
    Regression,
    Inconclusive,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InconclusiveReason {
    InsufficientEpochs,
    IntervalCrossesThreshold,
    UnavailableEvidence,
}
/// Exact nonparametric population-median order-statistic interval. Ranks are
/// 1-based; confidence numerator/denominator are exact binomial outcome counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MedianInterval {
    pub lower: PairedSample,
    pub upper: PairedSample,
    pub lower_rank: usize,
    pub upper_rank: usize,
    pub covered_outcomes: u128,
    pub total_outcomes: u128,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Analysis {
    pub decision: Decision,
    pub reason: Option<InconclusiveReason>,
    pub median_lower: PairedSample,
    pub median_upper: PairedSample,
    pub interval: Option<MedianInterval>,
}
/// Exact two-sided >=95% median interval using Binomial(n,1/2) order ranks.
/// This is NOT Mann-Whitney or Hodges-Lehmann. Coverage assumes independent,
/// identically distributed epoch ratios and a fixed population median. Ties
/// are retained; discrete distributions make coverage conservative. Adaptive
/// selection, correlated epochs, order effects and cherry-picking can invalidate
/// inference. This is per-comparison coverage, without multiplicity correction.
/// ABBA may mitigate drift; additive common drift does not cancel ratio
/// denominators, so this method assumes no automatic noise cancellation.
///
/// A finite interval needs at least six epochs. An upper bound <= threshold
/// passes; a lower bound > threshold detects regression; overlap is inconclusive.
/// No threshold is applied by the measurement runner itself.
pub fn analyze_pairs(samples: &[PairedSample], max_regression_bp: i64) -> Result<Analysis, Error> {
    if samples.is_empty() {
        return Err(Error::EmptyEvidence);
    }
    if samples.len() > MAX_EPOCHS {
        return Err(Error::Limit("epochs"));
    }
    if max_regression_bp < -9999 {
        return Err(Error::InvalidThreshold);
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.compare_ratio(*b));
    let n = sorted.len();
    let median_lower = sorted[(n - 1) / 2];
    let median_upper = sorted[n / 2];
    let total = 1u128
        .checked_shl(u32::try_from(n).map_err(|_| Error::ArithmeticOverflow)?)
        .ok_or(Error::ArithmeticOverflow)?;
    let mut coefficient = 1u128;
    let mut tail = 0u128;
    let mut chosen = None;
    for j in 0..n / 2 {
        tail = tail
            .checked_add(coefficient)
            .ok_or(Error::ArithmeticOverflow)?;
        if tail.checked_mul(200).ok_or(Error::ArithmeticOverflow)?
            <= total.checked_mul(5).ok_or(Error::ArithmeticOverflow)?
        {
            chosen = Some((j + 1, tail));
        } else {
            break;
        }
        coefficient = coefficient
            .checked_mul(u128::try_from(n - j).map_err(|_| Error::ArithmeticOverflow)?)
            .ok_or(Error::ArithmeticOverflow)?
            / u128::try_from(j + 1).map_err(|_| Error::ArithmeticOverflow)?;
    }
    let Some((rank, tail)) = chosen else {
        return Ok(Analysis {
            decision: Decision::Inconclusive,
            reason: Some(InconclusiveReason::InsufficientEpochs),
            median_lower,
            median_upper,
            interval: None,
        });
    };
    let interval = MedianInterval {
        lower: sorted[rank - 1],
        upper: sorted[n - rank],
        lower_rank: rank,
        upper_rank: n - rank + 1,
        covered_outcomes: total
            .checked_sub(tail.checked_mul(2).ok_or(Error::ArithmeticOverflow)?)
            .ok_or(Error::ArithmeticOverflow)?,
        total_outcomes: total,
    };
    let (decision, reason) =
        if interval.upper.compare_threshold(max_regression_bp)? != Ordering::Greater {
            (Decision::Pass, None)
        } else if interval.lower.compare_threshold(max_regression_bp)? == Ordering::Greater {
            (Decision::Regression, None)
        } else {
            (
                Decision::Inconclusive,
                Some(InconclusiveReason::IntervalCrossesThreshold),
            )
        };
    Ok(Analysis {
        decision,
        reason,
        median_lower,
        median_upper,
        interval: Some(interval),
    })
}
