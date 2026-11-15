//! Exact bounded paired signed-rank inference.
//!
//! The null interpretation requires independent, equiprobable signs
//! conditional on the nonzero magnitudes and zero pattern. No measurement
//! provenance, confidence interval, or regression-gate decision is inferred.

use crate::PairedSample;
use std::{cmp::Ordering, fmt};

/// Maximum number of paired differences, including zeros, accepted at once.
pub const MAX_SIGNED_RANK_PAIRS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignedRankError {
    EmptyEvidence,
    TooManyPairs,
    NoNonzeroDifferences,
}
impl fmt::Display for SignedRankError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "signed-rank: {self:?}")
    }
}
impl std::error::Error for SignedRankError {}

/// Exact sign-randomization result; doubled ranks represent midranks exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedRankProbability {
    pub nonzero_pairs: usize,
    pub discarded_zero_pairs: usize,
    pub tied_magnitude_groups: usize,
    pub twice_w_positive: u64,
    /// Twice the smaller inclusive tail, capped at one, as an unreduced fraction.
    pub numerator: u64,
    pub denominator: u64,
}

/// Exact two-sided Wilcoxon signed-rank permutation probability for caller-
/// supplied paired differences. Zeros are discarded (the Wilcoxon convention),
/// and tied absolute differences receive doubled midranks. All scratch storage
/// and work are bounded; no allocation or floating-point rounding is used.
///
/// The null interpretation assumes independent, equiprobable signs conditional
/// on the observed nonzero magnitudes and zero pattern. Paired ABBA measurements
/// do not automatically establish those assumptions. This function does not
/// produce a confidence interval or change the existing regression gate.
pub fn signed_rank_exact(differences: &[i64]) -> Result<SignedRankProbability, SignedRankError> {
    validate_len(differences.len())?;
    let mut observations = [Observation::EMPTY; MAX_SIGNED_RANK_PAIRS];
    let mut n = 0;
    for &difference in differences {
        if difference != 0 {
            observations[n] = Observation {
                magnitude: difference.unsigned_abs(),
                denominator: 1,
                positive: difference > 0,
            };
            n += 1;
        }
    }
    rank_observations(&mut observations, n, differences.len())
}

/// Exact signed-rank probability for per-operation candidate-minus-baseline
/// differences in existing paired samples. Fractional magnitudes are compared
/// as rational values using u128 cross-products; no conversion or rounding is
/// required. Zero differences are discarded, and equal fractions share ranks.
/// The caller must establish common units, provenance, and the conditional
/// sign assumptions documented for [`signed_rank_exact`]. This does not change
/// the existing ratio-based regression gate or produce a confidence interval.
pub fn signed_rank_exact_paired_samples(
    samples: &[PairedSample],
) -> Result<SignedRankProbability, SignedRankError> {
    validate_len(samples.len())?;
    let mut observations = [Observation::EMPTY; MAX_SIGNED_RANK_PAIRS];
    let mut n = 0;
    for &sample in samples {
        let baseline = sample.baseline_total();
        let candidate = sample.candidate_total();
        if baseline != candidate {
            observations[n] = Observation {
                magnitude: candidate.abs_diff(baseline),
                denominator: sample.operations_per_arm(),
                positive: candidate > baseline,
            };
            n += 1;
        }
    }
    rank_observations(&mut observations, n, samples.len())
}

fn validate_len(len: usize) -> Result<(), SignedRankError> {
    if len == 0 {
        Err(SignedRankError::EmptyEvidence)
    } else if len > MAX_SIGNED_RANK_PAIRS {
        Err(SignedRankError::TooManyPairs)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Observation {
    magnitude: u64,
    denominator: u64,
    positive: bool,
}
impl Observation {
    const EMPTY: Self = Self {
        magnitude: 0,
        denominator: 1,
        positive: false,
    };
    fn magnitude_cmp(self, other: Self) -> Ordering {
        // Each factor is u64, so both products fit in u128 exactly.
        (u128::from(self.magnitude) * u128::from(other.denominator))
            .cmp(&(u128::from(other.magnitude) * u128::from(self.denominator)))
    }
}

fn rank_observations(
    observations: &mut [Observation; MAX_SIGNED_RANK_PAIRS],
    n: usize,
    input_len: usize,
) -> Result<SignedRankProbability, SignedRankError> {
    if n == 0 {
        return Err(SignedRankError::NoNonzeroDifferences);
    }
    observations[..n].sort_unstable_by(|a, b| a.magnitude_cmp(*b));
    // Doubled ranks sum to n*(n+1), at most 1056. Counts are bounded by 2^32.
    let mut distribution = [0u64; MAX_SIGNED_RANK_PAIRS * (MAX_SIGNED_RANK_PAIRS + 1) + 1];
    distribution[0] = 1;
    let mut max_sum = 0usize;
    let mut positive_sum = 0usize;
    let mut tied_groups = 0usize;
    let mut start = 0;
    while start < n {
        let mut end = start + 1;
        while end < n && observations[end].magnitude_cmp(observations[start]) == Ordering::Equal {
            end += 1;
        }
        if end - start > 1 {
            tied_groups += 1;
        }
        let doubled_rank = start + 1 + end;
        for observation in &observations[start..end] {
            if observation.positive {
                positive_sum += doubled_rank;
            }
            for sum in (0..=max_sum).rev() {
                distribution[sum + doubled_rank] += distribution[sum];
            }
            max_sum += doubled_rank;
        }
        start = end;
    }
    let total = 1u64 << n;
    let tail_limit = positive_sum.min(max_sum - positive_sum);
    let tail: u64 = distribution[..=tail_limit].iter().sum();
    Ok(SignedRankProbability {
        nonzero_pairs: n,
        discarded_zero_pairs: input_len - n,
        tied_magnitude_groups: tied_groups,
        twice_w_positive: positive_sum as u64,
        numerator: (2 * tail).min(total),
        denominator: total,
    })
}
