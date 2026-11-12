//! Congestion-control input events, named exactly as
//! `docs/rfcs/wave-four-contracts.md` names them, plus the per-packet
//! delivery-rate snapshot this lane owns.

/// Largest valid packet number (`2^62 - 1`, RFC 9000 §12.3).
pub const MAX_PACKET_NUMBER: u64 = (1 << 62) - 1;

/// A packet handed to the network.
///
/// `sent_ns` is the departure time the sender chose (with EDT pacing, the
/// value [`crate::Bbrv3::schedule_edt_ns`] returned).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SentPacket {
    /// Packet number, `< 2^62`.
    pub pn: u64,
    /// Departure time in nanoseconds.
    pub sent_ns: u64,
    /// Bytes counted for congestion control (QUIC: datagram payload).
    pub bytes: u32,
    /// Whether the packet elicits an acknowledgement.
    pub ack_eliciting: bool,
    /// Whether the packet counts toward bytes in flight. Packets that do not
    /// (for example ACK-only packets) never contribute to delivery or loss.
    pub in_flight: bool,
}

/// A packet newly acknowledged by the peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AckedPacket {
    /// Packet number.
    pub pn: u64,
    /// The departure time recorded when it was sent.
    pub sent_ns: u64,
    /// Bytes, as sent.
    pub bytes: u32,
}

/// A packet declared lost by loss detection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LostPacket {
    /// Packet number.
    pub pn: u64,
    /// The departure time recorded when it was sent.
    pub sent_ns: u64,
    /// Bytes, as sent.
    pub bytes: u32,
}

/// RTT estimator output for the ACK being processed (RFC 9002 §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RttSample {
    /// Latest RTT sample: `now - sent_ns` of the newest acknowledged packet.
    pub latest_ns: u64,
    /// Minimum RTT over the connection lifetime.
    pub min_ns: u64,
    /// Smoothed RTT.
    pub smoothed_ns: u64,
    /// Peer-reported ACK delay (not used by BBR; kept for the contract).
    pub ack_delay_ns: u64,
}

/// Delivery-rate state snapshotted when a packet is sent
/// (draft-ietf-ccwg-bbr-07 §"Per-packet (P) state").
///
/// The transport stores this beside its sent-packet record and hands it back
/// when the packet is acknowledged or declared lost. It is plain copyable
/// data: it carries no ownership obligation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacketDeliveryState {
    /// `P.delivered`: connection delivered bytes at send time.
    pub delivered: u64,
    /// `P.delivered_time`.
    pub delivered_ns: u64,
    /// `P.first_send_time`.
    pub first_sent_ns: u64,
    /// `P.is_app_limited`.
    pub is_app_limited: bool,
    /// `P.tx_in_flight`: bytes in flight right after this packet was sent.
    pub tx_in_flight: u64,
    /// `P.lost`: connection congested bytes (lost plus CE-marked) at send
    /// time.
    pub lost: u64,
    /// Whether the packet was counted in bytes in flight. Uncounted packets
    /// are ignored on acknowledgement and loss.
    pub counted: bool,
}

/// One acknowledged packet together with its send-time snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivered {
    /// The packet.
    pub packet: AckedPacket,
    /// Its snapshot from [`crate::Bbrv3::on_packet_sent`].
    pub state: PacketDeliveryState,
}

/// A CC-agnostic congestion event (contract vocabulary).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CongestionEvent<'a> {
    /// One ACK frame: every newly acknowledged packet, plus the RTT
    /// estimator's sample if the ACK produced one.
    Acked {
        /// Newly acknowledged packets, each reported at most once.
        packets: &'a [Delivered],
        /// RTT sample for this ACK.
        rtt: Option<RttSample>,
    },
    /// A packet declared lost.
    Lost {
        /// The packet.
        packet: LostPacket,
        /// Its send-time snapshot.
        state: PacketDeliveryState,
    },
    /// Newly reported ECN-CE marks (the increase in the peer's CE count).
    /// Deliver it before the `Acked` event of the same ACK frame so the
    /// ACK's rate sample sees the marks.
    EcnCe {
        /// Number of new CE marks.
        count: u64,
    },
    /// RFC 9002 §7.6 persistent congestion (treated as an RTO).
    PersistentCongestion,
    /// The application has no data to send right now.
    AppLimited,
}

/// Rejected input. A rejected call leaves the controller unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BbrError {
    /// Configuration rejected; the string names the field.
    InvalidConfig(&'static str),
    /// Packet number `>= 2^62`.
    PacketNumberOutOfRange {
        /// The packet number.
        pn: u64,
    },
    /// An in-flight packet with zero bytes.
    EmptyInFlightPacket {
        /// The packet number.
        pn: u64,
    },
    /// The send guard refused: `bytes_in_flight >= cwnd`.
    CwndFull {
        /// Bytes in flight.
        bytes_in_flight: u64,
        /// Congestion window.
        cwnd: u64,
    },
    /// Acknowledged or lost bytes exceed the bytes in flight, so a packet
    /// was reported twice or does not belong to this controller.
    InflightUnderflow {
        /// Bytes in flight.
        bytes_in_flight: u64,
        /// Bytes the event tried to remove.
        released: u64,
    },
    /// A snapshot claims more delivered or congested bytes than this
    /// controller has counted, so it was not produced by this controller.
    ForeignSnapshot {
        /// The packet number.
        pn: u64,
    },
    /// The event time is earlier than an earlier event's time.
    TimeWentBackwards {
        /// Latest time seen.
        last_ns: u64,
        /// The rejected time.
        now_ns: u64,
    },
    /// An acknowledged packet's departure time is after the ACK time.
    AckedBeforeSent {
        /// The packet number.
        pn: u64,
    },
    /// A cumulative byte counter would overflow `u64`.
    CounterOverflow,
}

impl core::fmt::Display for BbrError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidConfig(field) => write!(f, "invalid BBR configuration: {field}"),
            Self::PacketNumberOutOfRange { pn } => write!(f, "packet number {pn} >= 2^62"),
            Self::EmptyInFlightPacket { pn } => write!(f, "in-flight packet {pn} has no bytes"),
            Self::CwndFull {
                bytes_in_flight,
                cwnd,
            } => write!(
                f,
                "send refused: {bytes_in_flight} bytes in flight >= cwnd {cwnd}"
            ),
            Self::InflightUnderflow {
                bytes_in_flight,
                released,
            } => write!(
                f,
                "{released} bytes released but only {bytes_in_flight} in flight"
            ),
            Self::ForeignSnapshot { pn } => {
                write!(
                    f,
                    "packet {pn} carries a snapshot this controller did not issue"
                )
            }
            Self::TimeWentBackwards { last_ns, now_ns } => {
                write!(f, "time went backwards: {now_ns} < {last_ns}")
            }
            Self::AckedBeforeSent { pn } => {
                write!(f, "packet {pn} acknowledged before it was sent")
            }
            Self::CounterOverflow => write!(f, "byte counter overflow"),
        }
    }
}

impl std::error::Error for BbrError {}
