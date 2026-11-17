//! [`crate::cc`]'s NewReno forwarded to the Vesper build.

use super::binding as b;
use crate::cc::{CongestionController, CongestionEvent, NewReno, RttSample, SentPacket};

impl NewReno {
    /// A controller for sender MSS `mss` with an initial window of
    /// `initial_segments` segments (RFC 6928 allows 10).
    #[must_use]
    pub fn new(mss: u32, initial_segments: u32) -> NewReno {
        b::newreno_new(mss, initial_segments)
    }

    /// Set the sender MSS (called once the peer's MSS is known).
    pub fn set_mss(&mut self, mss: u32, initial_segments: u32) {
        b::newreno_set_mss(self, mss, initial_segments);
    }
}

impl CongestionController for NewReno {
    fn on_mss(&mut self, smss: u32) {
        b::newreno_on_mss(self, smss);
    }

    // NewReno ignores sends; the Vesper build has no counterpart.
    fn on_sent(&mut self, _packet: SentPacket) {}

    fn on_rtt_sample(&mut self, sample: RttSample) {
        b::newreno_on_rtt_sample(self, sample);
    }

    fn on_event(&mut self, event: CongestionEvent, now_ns: u64) {
        b::newreno_on_event(self, event, now_ns);
    }

    fn cwnd_bytes(&self) -> u32 {
        self.cwnd
    }

    fn pacing_rate_bytes_per_sec(&self) -> u64 {
        b::newreno_pacing_rate(self.clone())
    }

    fn schedule_edt_ns(&mut self, bytes: u32, now_ns: u64) -> u64 {
        b::newreno_schedule_edt(self.clone(), bytes, now_ns)
    }
}
