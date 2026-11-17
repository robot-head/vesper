//! The Rust reference implementation of [`crate::cc`], compiled without
//! `vesper-port` (the Vesper build is `vesper/cc.ves`).

use crate::cc::{CongestionController, CongestionEvent, NewReno, RttSample, SentPacket};

impl NewReno {
    /// A controller for sender MSS `mss` with an initial window of
    /// `initial_segments` segments (RFC 6928 allows 10).
    #[must_use]
    pub fn new(mss: u32, initial_segments: u32) -> NewReno {
        let mss = mss.max(1);
        NewReno {
            mss,
            cwnd: mss.saturating_mul(initial_segments.max(1)),
            ssthresh: u32::MAX,
            ca_acked: 0,
            recovery_start_ns: None,
            srtt_ns: 0,
        }
    }

    /// Set the sender MSS (called once the peer's MSS is known).
    pub fn set_mss(&mut self, mss: u32, initial_segments: u32) {
        self.mss = mss.max(1);
        self.cwnd = self.mss.saturating_mul(initial_segments.max(1));
    }

    fn in_recovery(&self, sent_ns: u64) -> bool {
        self.recovery_start_ns.is_some_and(|r| sent_ns <= r)
    }

    fn reduce(&mut self, now_ns: u64) {
        self.recovery_start_ns = Some(now_ns);
        self.ssthresh = (self.cwnd / 2).max(2 * self.mss);
        self.cwnd = self.ssthresh;
        self.ca_acked = 0;
    }
}

impl CongestionController for NewReno {
    fn on_mss(&mut self, smss: u32) {
        let segments = (self.cwnd / self.mss).max(1);
        self.set_mss(smss, segments);
    }

    fn on_sent(&mut self, _packet: SentPacket) {}

    fn on_rtt_sample(&mut self, sample: RttSample) {
        self.srtt_ns = sample.smoothed_ns;
    }

    fn on_event(&mut self, event: CongestionEvent, now_ns: u64) {
        match event {
            CongestionEvent::Acked { packet, .. } => {
                if self.in_recovery(packet.sent_ns) {
                    return;
                }
                if self.cwnd < self.ssthresh {
                    // RFC 3465 appropriate byte counting with L = 2*SMSS.
                    self.cwnd = self.cwnd.saturating_add(packet.bytes.min(2 * self.mss));
                } else {
                    self.ca_acked = self.ca_acked.saturating_add(packet.bytes);
                    if self.ca_acked >= self.cwnd {
                        self.ca_acked -= self.cwnd;
                        self.cwnd = self.cwnd.saturating_add(self.mss);
                    }
                }
            }
            CongestionEvent::Lost { packet, .. } => {
                if !self.in_recovery(packet.sent_ns) {
                    self.reduce(now_ns);
                }
            }
            CongestionEvent::EcnCe { .. } => {
                if self
                    .recovery_start_ns
                    .is_none_or(|r| now_ns > r.saturating_add(self.srtt_ns))
                {
                    self.reduce(now_ns);
                }
            }
            CongestionEvent::PersistentCongestion => {
                // RFC 5681 §3.1 eq. (4) and the loss window of one segment.
                if self.recovery_start_ns.is_none_or(|r| now_ns > r) {
                    self.ssthresh = (self.cwnd / 2).max(2 * self.mss);
                }
                self.cwnd = self.mss;
                self.ca_acked = 0;
                self.recovery_start_ns = Some(now_ns);
            }
            CongestionEvent::AppLimited => {}
        }
    }

    fn cwnd_bytes(&self) -> u32 {
        self.cwnd
    }

    fn pacing_rate_bytes_per_sec(&self) -> u64 {
        u64::MAX
    }

    fn schedule_edt_ns(&mut self, _bytes: u32, now_ns: u64) -> u64 {
        now_ns
    }
}
