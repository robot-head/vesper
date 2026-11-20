//! The Rust reference implementation of [`crate::rtt`], compiled without
//! `vesper-port` (the Vesper build is `vesper/rtt.ves`).

use crate::rtt::{RtoConfig, RttEstimator};

/// Cap on the RTO backoff shift (RFC 6298 §5.5 doubling; the maximum RTO
/// bounds the value long before).
const MAX_BACKOFF_SHIFT: u32 = 32;

impl RttEstimator {
    /// A fresh estimator.
    #[must_use]
    pub fn new(cfg: RtoConfig) -> RttEstimator {
        RttEstimator {
            cfg,
            srtt_ns: 0,
            rttvar_ns: 0,
            min_ns: u64::MAX,
            latest_ns: 0,
            has_sample: false,
            backoff_shift: 0,
        }
    }

    /// Feed a sample (RFC 6298 §2.2/§2.3). Karn's rule is the caller's job:
    /// only segments never retransmitted may be sampled.
    pub fn sample(&mut self, r_ns: u64) {
        self.latest_ns = r_ns;
        self.min_ns = self.min_ns.min(r_ns);
        if self.has_sample {
            let err = self.srtt_ns.abs_diff(r_ns);
            // RTTVAR = 3/4 RTTVAR + 1/4 |SRTT - R'|; SRTT = 7/8 SRTT + 1/8 R'.
            self.rttvar_ns = self.rttvar_ns - self.rttvar_ns / 4 + err / 4;
            self.srtt_ns = self.srtt_ns - self.srtt_ns / 8 + r_ns / 8;
        } else {
            self.srtt_ns = r_ns;
            self.rttvar_ns = r_ns / 2;
            self.has_sample = true;
        }
        // A new sample after backoff restores the computed RTO (§5.7).
        self.backoff_shift = 0;
    }

    /// Double the RTO (RFC 6298 §5.5), bounded by the maximum.
    pub fn backoff(&mut self) {
        if self.backoff_shift < MAX_BACKOFF_SHIFT {
            self.backoff_shift += 1;
        }
    }

    /// Clear backoff (e.g. after new data is acknowledged).
    pub fn reset_backoff(&mut self) {
        self.backoff_shift = 0;
    }

    /// Current RTO including backoff.
    #[must_use]
    pub fn rto_ns(&self) -> u64 {
        let base = if self.has_sample {
            self.srtt_ns.saturating_add(
                self.cfg
                    .granularity_ns
                    .max(self.rttvar_ns.saturating_mul(4)),
            )
        } else {
            self.cfg.initial_ns
        };
        let base = base.clamp(self.cfg.min_ns, self.cfg.max_ns);
        base.checked_shl(self.backoff_shift)
            .filter(|v| v >> self.backoff_shift == base)
            .unwrap_or(u64::MAX)
            .min(self.cfg.max_ns)
    }
}
