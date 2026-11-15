//! Bounded paired Hodges–Lehmann estimation from signed differences.
//!
//! Exact signed-rank coverage requires independent identically distributed,
//! continuous differences symmetric about their population location. The API
//! neither verifies these assumptions nor treats ABBA measurements as proof
//! of them. It does not alter the existing ratio-based regression gate.

use std::fmt;

/// Maximum number of paired differences accepted by this calculation.
pub const MAX_HODGES_LEHMANN_PAIRS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HodgesLehmannError {
    EmptyEvidence,
    TooManyPairs,
    ZeroDifference,
    TiedAbsoluteDifferences,
    TiedWalshAverages,
    AllocationFailed,
}
impl fmt::Display for HodgesLehmannError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hodges–Lehmann: {self:?}")
    }
}
impl std::error::Error for HodgesLehmannError {}

/// An exact interval whose endpoints are twice the Walsh-average bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedRankInterval {
    pub lower_times_two: i128,
    pub upper_times_two: i128,
    /// Inclusive signed-rank tail used for each end of the interval.
    pub critical_rank: usize,
    /// Exact nominal signed-rank coverage under the documented assumptions.
    pub covered_outcomes: u64,
    pub total_outcomes: u64,
}

/// The point estimate is four times the median of the Walsh averages, so
/// even-sized medians and half-integer Walsh averages never require rounding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HodgesLehmannEstimate {
    pub estimate_times_four: i128,
    /// None when no finite >=95% interval exists for this sample size.
    pub interval: Option<SignedRankInterval>,
}

/// Estimate the location of a paired-difference distribution and its exact
/// two-sided >=95% signed-rank interval. This bounded fragment rejects zeros,
/// tied absolute differences, and tied Walsh averages rather than applying an
/// untied distribution to ambiguous ranks. `differences` are caller-provided
/// signed observations; no source of measurements or units is inferred.
pub fn hodges_lehmann_paired(
    differences: &[i64],
) -> Result<HodgesLehmannEstimate, HodgesLehmannError> {
    let n = differences.len();
    if n == 0 {
        return Err(HodgesLehmannError::EmptyEvidence);
    }
    if n > MAX_HODGES_LEHMANN_PAIRS {
        return Err(HodgesLehmannError::TooManyPairs);
    }
    if differences.contains(&0) {
        return Err(HodgesLehmannError::ZeroDifference);
    }
    // Fixed-size scratch avoids allocation and handles i64::MIN exactly.
    let mut magnitudes = [0u64; MAX_HODGES_LEHMANN_PAIRS];
    for (slot, &difference) in magnitudes.iter_mut().zip(differences) {
        *slot = difference.unsigned_abs();
    }
    magnitudes[..n].sort_unstable();
    if magnitudes[..n].windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(HodgesLehmannError::TiedAbsoluteDifferences);
    }
    let count = n * (n + 1) / 2;
    let mut walsh_twice = Vec::new();
    walsh_twice
        .try_reserve_exact(count)
        .map_err(|_| HodgesLehmannError::AllocationFailed)?;
    for i in 0..n {
        for j in i..n {
            walsh_twice.push(i128::from(differences[i]) + i128::from(differences[j]));
        }
    }
    walsh_twice.sort_unstable();
    if walsh_twice.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(HodgesLehmannError::TiedWalshAverages);
    }
    let estimate_times_four = if count.is_multiple_of(2) {
        walsh_twice[count / 2 - 1] + walsh_twice[count / 2]
    } else {
        2 * walsh_twice[count / 2]
    };
    // Exact distribution of the sum of a subset of ranks 1..=n.
    // All counts fit u64 because n <= 32.
    let mut distribution =
        [0u64; MAX_HODGES_LEHMANN_PAIRS * (MAX_HODGES_LEHMANN_PAIRS + 1) / 2 + 1];
    distribution[0] = 1;
    let mut max_rank = 0;
    for rank in 1..=n {
        for sum in (0..=max_rank).rev() {
            distribution[sum + rank] += distribution[sum];
        }
        max_rank += rank;
    }
    let total = 1u64 << n;
    let mut tail = 0u64;
    let mut critical = None;
    for (rank, &outcomes) in distribution[..=max_rank].iter().enumerate() {
        tail += outcomes;
        if 40 * tail <= total {
            critical = Some((rank, tail));
        } else {
            break;
        }
    }
    let interval = critical.map(|(rank, tail)| SignedRankInterval {
        lower_times_two: walsh_twice[rank],
        upper_times_two: walsh_twice[count - 1 - rank],
        critical_rank: rank,
        covered_outcomes: total - 2 * tail,
        total_outcomes: total,
    });
    Ok(HodgesLehmannEstimate {
        estimate_times_four,
        interval,
    })
}
