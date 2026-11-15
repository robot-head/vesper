//! Exact bounded Walsh pseudomedians for paired integer differences.
use std::fmt;

pub const MAX_WALSH_PAIRS: usize = 32;
const MAX_WALSH_AVERAGES: usize = MAX_WALSH_PAIRS * (MAX_WALSH_PAIRS + 1) / 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalshEstimateError {
    EmptyEvidence,
    TooManyPairs,
}
impl fmt::Display for WalshEstimateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Walsh estimate: {self:?}")
    }
}
impl std::error::Error for WalshEstimateError {}

/// Four times the median Walsh average, with no rounding even when both the
/// average and the median fall between integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalshEstimate {
    pub pairs: usize,
    pub walsh_averages: usize,
    pub estimate_times_four: i128,
}

/// Compute the Hodges–Lehmann point estimate from caller-supplied paired
/// differences, including zeros and ties with their full multiplicity. This
/// bounded calculation allocates no heap storage. It supplies no confidence
/// interval, gate decision, or evidence of measurement provenance.
pub fn walsh_pseudomedian(differences: &[i64]) -> Result<WalshEstimate, WalshEstimateError> {
    let n = differences.len();
    if n == 0 {
        return Err(WalshEstimateError::EmptyEvidence);
    }
    if n > MAX_WALSH_PAIRS {
        return Err(WalshEstimateError::TooManyPairs);
    }
    let mut sums = [0i128; MAX_WALSH_AVERAGES];
    let mut count = 0;
    for i in 0..n {
        for j in i..n {
            sums[count] = i128::from(differences[i]) + i128::from(differences[j]);
            count += 1;
        }
    }
    sums[..count].sort_unstable();
    let estimate_times_four = if count.is_multiple_of(2) {
        sums[count / 2 - 1] + sums[count / 2]
    } else {
        2 * sums[count / 2]
    };
    Ok(WalshEstimate {
        pairs: n,
        walsh_averages: count,
        estimate_times_four,
    })
}
