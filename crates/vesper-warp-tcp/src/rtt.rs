//! RFC 6298 retransmission-timeout estimation.

/// RTO parameters. RFC 6298 recommends a 1 s initial and minimum RTO and
/// permits a maximum of at least 60 s; tests shorten them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtoConfig {
    /// RTO before the first sample (RFC 6298 §2.1).
    pub initial_ns: u64,
    /// Lower bound (RFC 6298 §2.4).
    pub min_ns: u64,
    /// Upper bound (RFC 6298 §2.5).
    pub max_ns: u64,
    /// Clock granularity G (RFC 6298 §2.3).
    pub granularity_ns: u64,
}

impl Default for RtoConfig {
    fn default() -> Self {
        RtoConfig {
            initial_ns: 1_000_000_000,
            min_ns: 1_000_000_000,
            max_ns: 60_000_000_000,
            granularity_ns: 1_000_000,
        }
    }
}

/// SRTT/RTTVAR estimator with exponential backoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RttEstimator {
    pub(crate) cfg: RtoConfig,
    pub(crate) srtt_ns: u64,
    pub(crate) rttvar_ns: u64,
    pub(crate) min_ns: u64,
    pub(crate) latest_ns: u64,
    pub(crate) has_sample: bool,
    pub(crate) backoff_shift: u32,
}

impl RttEstimator {
    /// Smoothed RTT (0 before the first sample).
    #[must_use]
    pub fn srtt_ns(&self) -> u64 {
        self.srtt_ns
    }

    /// RTT variance.
    #[must_use]
    pub fn rttvar_ns(&self) -> u64 {
        self.rttvar_ns
    }

    /// Minimum observed sample (`u64::MAX` before any).
    #[must_use]
    pub fn min_ns(&self) -> u64 {
        self.min_ns
    }

    /// Latest sample.
    #[must_use]
    pub fn latest_ns(&self) -> u64 {
        self.latest_ns
    }

    /// Whether any sample has been taken.
    #[must_use]
    pub fn has_sample(&self) -> bool {
        self.has_sample
    }
}
