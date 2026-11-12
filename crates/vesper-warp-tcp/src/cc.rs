//! Congestion-control boundary shared with the QUIC and BBRv3 lanes
//! (docs/rfcs/wave-four-contracts.md, "Shared vocabulary").
//!
//! TCP has no packet numbers; this crate assigns every transmission a fresh,
//! strictly increasing `pn` (a transmission id), so a retransmission of the
//! same bytes is a new `pn`. That is the identity BBR-style rate sampling
//! needs and mirrors QUIC packet numbers.

/// A transmission handed to the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SentPacket {
    /// Transmission id.
    pub pn: u64,
    /// Send time (virtual ns).
    pub sent_ns: u64,
    /// Bytes counted against the congestion window (sequence-space length).
    pub bytes: u32,
    /// Whether the segment elicits an acknowledgment.
    pub ack_eliciting: bool,
    /// Whether it counts toward bytes in flight.
    pub in_flight: bool,
}

/// A transmission newly acknowledged (cumulatively or by SACK).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AckedPacket {
    /// Transmission id of the most recent transmission of these bytes.
    pub pn: u64,
    /// Send time of that transmission.
    pub sent_ns: u64,
    /// Bytes acknowledged.
    pub bytes: u32,
}

/// A transmission declared lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LostPacket {
    /// Transmission id.
    pub pn: u64,
    /// Send time.
    pub sent_ns: u64,
    /// Bytes declared lost.
    pub bytes: u32,
}

/// An RTT measurement (RFC 6298 via Karn's rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RttSample {
    /// Latest sample.
    pub latest_ns: u64,
    /// Minimum over the connection.
    pub min_ns: u64,
    /// Smoothed RTT after this sample.
    pub smoothed_ns: u64,
    /// Peer-reported ack delay (TCP has none: always 0).
    pub ack_delay_ns: u64,
}

/// Events delivered to a controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CongestionEvent {
    /// Bytes acknowledged.
    Acked {
        /// The acknowledged transmission.
        packet: AckedPacket,
        /// Bytes still in flight after processing the acknowledgment.
        bytes_in_flight: u32,
    },
    /// Bytes declared lost (dup-ACK, SACK scoreboard).
    Lost {
        /// The lost transmission.
        packet: LostPacket,
        /// Bytes in flight before removing it.
        bytes_in_flight: u32,
    },
    /// ECN congestion experienced (unused: this endpoint does not negotiate
    /// ECN, but the variant is part of the shared contract).
    EcnCe {
        /// Cumulative CE count.
        count: u64,
    },
    /// Retransmission timeout: TCP's analogue of QUIC persistent congestion.
    PersistentCongestion,
    /// The sender had nothing to send while below cwnd.
    AppLimited,
}

/// A congestion controller. The wave-four integration wires BBRv3 behind this
/// trait; [`NewReno`] is the standalone RFC 5681 controller.
pub trait CongestionController {
    /// The sender MSS became known (after the handshake). Default: ignore.
    fn on_mss(&mut self, _smss: u32) {}
    /// A transmission left the endpoint.
    fn on_sent(&mut self, packet: SentPacket);
    /// An RTT sample was taken.
    fn on_rtt_sample(&mut self, sample: RttSample);
    /// Acknowledgment, loss or other congestion signal.
    fn on_event(&mut self, event: CongestionEvent, now_ns: u64);
    /// Current congestion window.
    fn cwnd_bytes(&self) -> u32;
    /// Current pacing rate (`u64::MAX` means unpaced).
    fn pacing_rate_bytes_per_sec(&self) -> u64;
    /// Earliest departure time for `bytes` sent at `now_ns`; never earlier
    /// than `now_ns`.
    fn schedule_edt_ns(&mut self, bytes: u32, now_ns: u64) -> u64;
}

/// RFC 5681 slow start / congestion avoidance with a single window
/// reduction per loss episode (episodes are delimited by send time, as in
/// RFC 9002 §7.3.2, so it composes with RFC 6675 pipe-based recovery instead
/// of RFC 5681 window inflation). Window-based and unpaced: EDT is `now`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewReno {
    mss: u32,
    cwnd: u32,
    ssthresh: u32,
    ca_acked: u32,
    recovery_start_ns: Option<u64>,
    srtt_ns: u64,
}

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

    /// Slow-start threshold.
    #[must_use]
    pub fn ssthresh(&self) -> u32 {
        self.ssthresh
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

#[cfg(test)]
mod tests {
    use super::*;

    fn acked(sent_ns: u64, bytes: u32) -> CongestionEvent {
        CongestionEvent::Acked {
            packet: AckedPacket {
                pn: 0,
                sent_ns,
                bytes,
            },
            bytes_in_flight: 0,
        }
    }

    #[test]
    fn slow_start_then_one_reduction_per_episode() {
        let mut cc = NewReno::new(1000, 2);
        cc.on_event(acked(1, 1000), 2);
        assert_eq!(cc.cwnd_bytes(), 3000);
        let lost = |sent_ns| CongestionEvent::Lost {
            packet: LostPacket {
                pn: 0,
                sent_ns,
                bytes: 1000,
            },
            bytes_in_flight: 3000,
        };
        cc.on_event(lost(5), 10);
        assert_eq!(cc.cwnd_bytes(), 2000);
        cc.on_event(lost(6), 11); // same episode
        assert_eq!(cc.cwnd_bytes(), 2000);
        cc.on_event(acked(9, 1000), 12); // sent before recovery: no growth
        assert_eq!(cc.cwnd_bytes(), 2000);
        cc.on_event(acked(20, 2000), 30); // congestion avoidance
        assert_eq!(cc.cwnd_bytes(), 3000);
        cc.on_event(CongestionEvent::PersistentCongestion, 40);
        assert_eq!(cc.cwnd_bytes(), 1000);
        assert_eq!(cc.ssthresh(), 2000);
        assert_eq!(cc.schedule_edt_ns(100, 77), 77);
    }
}
