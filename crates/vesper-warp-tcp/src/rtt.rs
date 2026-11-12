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
    cfg: RtoConfig,
    srtt_ns: u64,
    rttvar_ns: u64,
    min_ns: u64,
    latest_ns: u64,
    has_sample: bool,
    backoff_shift: u32,
}

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
        if self.backoff_shift < 32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6298_formulas_and_backoff() {
        let cfg = RtoConfig {
            initial_ns: 1000,
            min_ns: 10,
            max_ns: 100_000,
            granularity_ns: 1,
        };
        let mut e = RttEstimator::new(cfg);
        assert_eq!(e.rto_ns(), 1000);
        e.sample(800);
        assert_eq!((e.srtt_ns(), e.rttvar_ns()), (800, 400));
        assert_eq!(e.rto_ns(), 800 + 1600);
        e.sample(400);
        // RTTVAR = 300 + 100 = 400, SRTT = 700 + 50 = 750.
        assert_eq!((e.srtt_ns(), e.rttvar_ns()), (750, 400));
        assert_eq!(e.rto_ns(), 2350);
        e.backoff();
        assert_eq!(e.rto_ns(), 4700);
        for _ in 0..40 {
            e.backoff();
        }
        assert_eq!(e.rto_ns(), 100_000);
        e.sample(750);
        assert_eq!(e.min_ns(), 400);
        assert!(e.rto_ns() < 100_000);
    }
}
