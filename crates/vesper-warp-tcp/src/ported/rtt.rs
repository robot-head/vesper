//! [`crate::rtt`] forwarded to the Vesper build.

use super::binding as b;
use crate::rtt::{RtoConfig, RttEstimator};

impl RttEstimator {
    /// A fresh estimator.
    #[must_use]
    pub fn new(cfg: RtoConfig) -> RttEstimator {
        b::rtt_new(cfg)
    }

    /// Feed a sample (RFC 6298 §2.2/§2.3). Karn's rule is the caller's job:
    /// only segments never retransmitted may be sampled.
    pub fn sample(&mut self, r_ns: u64) {
        b::rtt_sample(self, r_ns);
    }

    /// Double the RTO (RFC 6298 §5.5), bounded by the maximum.
    pub fn backoff(&mut self) {
        b::rtt_backoff(self);
    }

    /// Clear backoff (e.g. after new data is acknowledged).
    pub fn reset_backoff(&mut self) {
        b::rtt_reset_backoff(self);
    }

    /// Current RTO including backoff.
    #[must_use]
    pub fn rto_ns(&self) -> u64 {
        b::rtt_rto_ns(*self)
    }
}
