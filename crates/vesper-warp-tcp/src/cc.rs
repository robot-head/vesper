//! Congestion-control boundary shared with QUIC and BBRv3.
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
    pub(crate) mss: u32,
    pub(crate) cwnd: u32,
    pub(crate) ssthresh: u32,
    pub(crate) ca_acked: u32,
    pub(crate) recovery_start_ns: Option<u64>,
    pub(crate) srtt_ns: u64,
}

impl NewReno {
    /// Slow-start threshold.
    #[must_use]
    pub fn ssthresh(&self) -> u32 {
        self.ssthresh
    }
}
