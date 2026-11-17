//! The sans-IO RFC 9293 connection: state machine, windows, reassembly,
//! retransmission and SACK loss recovery.
//!
//! The caller owns all I/O and all payload memory:
//! - incoming segments are passed in as byte slices; in-order payload is
//!   returned as borrowed slices of that segment, and out-of-order payload
//!   is placed in a caller-owned receive ring and later returned as borrowed
//!   slices of the ring;
//! - outgoing segments are written into a caller-provided buffer; payload is
//!   read from a caller-owned send buffer described by a [`SendView`]. The
//!   retransmission queue stores only sequence ranges into that buffer, never
//!   copies, and the caller learns via [`Connection::take_released`] when a
//!   range is acknowledged and may be reused;
//! - time is a caller-supplied virtual nanosecond clock; timers are
//!   surfaced as deadlines by [`Connection::poll_timeout`].

use crate::rtt::RtoConfig;
use crate::wire::{IpPair, WireError};

#[cfg(feature = "vesper-port")]
pub use crate::ported::conn::Connection;
#[cfg(not(feature = "vesper-port"))]
pub use crate::reference::conn::Connection;

/// Smallest sender MSS accepted from a peer; smaller advertisements are
/// clamped up so that a hostile MSS option cannot stall the sender.
pub const MIN_SEND_MSS: u16 = 64;

/// Connection states (RFC 9293 §3.3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    /// No connection.
    Closed,
    /// Waiting for a SYN.
    Listen,
    /// SYN sent, waiting for a matching SYN.
    SynSent,
    /// SYN received and SYN sent, waiting for ACK of our SYN.
    SynReceived,
    /// Data transfer.
    Established,
    /// Our FIN sent (or queued), not yet acknowledged.
    FinWait1,
    /// Our FIN acknowledged, waiting for the peer's FIN.
    FinWait2,
    /// Peer's FIN received, waiting for the local close.
    CloseWait,
    /// FINs crossed (simultaneous close), waiting for ACK of ours.
    Closing,
    /// Peer closed first; our FIN sent, waiting for its ACK.
    LastAck,
    /// Waiting 2·MSL before reuse.
    TimeWait,
}

/// Endpoint configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Local port.
    pub local_port: u16,
    /// MSS advertised to the peer and the upper bound for our segments
    /// (payload bytes, RFC 9293 §3.7.1).
    pub mss: u16,
    /// Receive ring capacity in bytes; also the maximum receive window.
    /// Must be in `1..=2^30`.
    pub recv_buffer: u32,
    /// Window shift to offer (RFC 7323); `None` disables window scaling.
    pub window_scale: Option<u8>,
    /// Offer SACK (RFC 2018).
    pub sack: bool,
    /// Offer timestamps (RFC 7323).
    pub timestamps: bool,
    /// RFC 6298 parameters.
    pub rto: RtoConfig,
    /// Retransmissions of one segment before the connection is aborted
    /// (RFC 9293 §3.8.3 R2).
    pub max_retransmits: u32,
    /// TIME-WAIT duration (2·MSL; RFC 9293 suggests MSL = 2 minutes).
    pub time_wait_ns: u64,
    /// Delayed-ACK timeout; 0 acknowledges every segment immediately
    /// (RFC 9293 §3.8.6.3 requires < 0.5 s).
    pub delayed_ack_ns: u64,
    /// Capacity of the retransmission queue/scoreboard in segments.
    pub max_in_flight_segments: u32,
    /// Maximum disjoint out-of-order ranges remembered by the receiver.
    pub max_ooo_ranges: u32,
    /// Challenge ACKs allowed per second of virtual time (RFC 5961 §7).
    pub challenge_ack_limit: u32,
    /// Initial window in segments for the built-in controller.
    pub initial_window_segments: u32,
    /// Nagle's algorithm (RFC 9293 §3.7.4).
    pub nagle: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            local_port: 0,
            mss: 1460,
            recv_buffer: 256 * 1024,
            window_scale: Some(7),
            sack: true,
            timestamps: true,
            rto: RtoConfig::default(),
            max_retransmits: 15,
            time_wait_ns: 240_000_000_000,
            delayed_ack_ns: 40_000_000,
            max_in_flight_segments: 1024,
            max_ooo_ranges: 32,
            challenge_ack_limit: 1000,
            initial_window_segments: 10,
            nagle: false,
        }
    }
}

/// Errors returned by the endpoint. Each is a typed rejection; the
/// connection state is unchanged unless documented otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpError {
    /// Malformed or corrupted segment.
    Wire(WireError),
    /// The segment's ports or addresses do not belong to this connection.
    NotForThisConnection,
    /// The receive ring passed in does not have `Config::recv_buffer` bytes.
    RingSizeMismatch,
    /// The operation is not valid in the current state.
    InvalidState(State),
    /// The connection is closing; no more data may be written.
    ConnectionClosing,
    /// The send view does not cover the bytes that must be transmitted.
    SendViewMissing {
        /// First stream offset needed.
        start: u64,
        /// One past the last stream offset needed.
        end: u64,
    },
    /// Output buffer shorter than 60 bytes plus the payload to send.
    BufferTooSmall,
    /// Configuration rejected.
    InvalidConfig(&'static str),
    /// `recv_release` released more than was delivered.
    ReleaseExceedsUnread,
    /// A counter would overflow.
    Overflow,
}

impl From<WireError> for TcpError {
    fn from(e: WireError) -> Self {
        TcpError::Wire(e)
    }
}

impl core::fmt::Display for TcpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TcpError::Wire(e) => write!(f, "{e}"),
            TcpError::NotForThisConnection => f.write_str("segment not for this connection"),
            TcpError::RingSizeMismatch => f.write_str("receive ring size mismatch"),
            TcpError::InvalidState(s) => write!(f, "operation invalid in state {s:?}"),
            TcpError::ConnectionClosing => f.write_str("connection closing"),
            TcpError::SendViewMissing { start, end } => {
                write!(f, "send view does not cover {start}..{end}")
            }
            TcpError::BufferTooSmall => f.write_str("output buffer too small"),
            TcpError::InvalidConfig(why) => write!(f, "invalid configuration: {why}"),
            TcpError::ReleaseExceedsUnread => f.write_str("release exceeds delivered bytes"),
            TcpError::Overflow => f.write_str("counter overflow"),
        }
    }
}

impl std::error::Error for TcpError {}

/// Why a connection ended abnormally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// RST received in a synchronized state.
    Reset,
    /// RST received in SYN-SENT/active SYN-RECEIVED.
    Refused,
    /// Retransmission limit reached.
    TimedOut,
    /// Local abort.
    Aborted,
}

/// Notifications for the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Reached ESTABLISHED.
    Connected,
    /// The peer's FIN was received in order: no more data will arrive.
    PeerFinished,
    /// The connection reached CLOSED.
    Closed(Option<CloseReason>),
}

/// The caller's send buffer: `bytes[i]` is stream offset `base + i`.
#[derive(Clone, Copy, Debug)]
pub struct SendView<'a> {
    /// Stream offset of `bytes[0]`.
    pub base: u64,
    /// Buffer contents.
    pub bytes: &'a [u8],
}

impl SendView<'_> {
    /// An empty view.
    pub const EMPTY: SendView<'static> = SendView {
        base: 0,
        bytes: &[],
    };
}

/// A fully acknowledged range of the send stream. The caller may reuse the
/// buffer bytes for these offsets once it has taken this value.
#[must_use = "acknowledged send-buffer ranges must be reclaimed by the caller"]
#[derive(Debug, PartialEq, Eq)]
pub struct Released {
    /// First released stream offset.
    pub start: u64,
    /// One past the last released stream offset.
    pub end: u64,
}

/// In-order payload surfaced by one incoming segment, as up to three
/// borrowed slices (the segment itself, then the receive ring, possibly
/// wrapped). The bytes stay valid in the ring until released with
/// [`Connection::recv_release`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Received<'a> {
    pub(crate) chunks: [&'a [u8]; 3],
    /// The peer's FIN was processed by this segment.
    pub fin: bool,
}

impl<'a> Received<'a> {
    /// The data chunks in stream order (empty chunks skipped).
    pub fn chunks(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.chunks.iter().copied().filter(|c| !c.is_empty())
    }

    /// Total delivered bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.chunks.iter().map(|c| c.len()).sum()
    }

    /// Whether no data was delivered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One segment written by [`Connection::poll_transmit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transmit {
    /// Bytes written to the output buffer.
    pub len: usize,
    /// Earliest departure time from the congestion controller.
    pub edt_ns: u64,
    /// Destination/source pair (local is `src`).
    pub ips: IpPair,
}

/// Counters for tests and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Segments emitted.
    pub segments_sent: u64,
    /// Data segments retransmitted.
    pub retransmits: u64,
    /// Payload bytes retransmitted.
    pub retransmitted_bytes: u64,
    /// Fast-retransmit/SACK recovery episodes entered.
    pub recoveries: u64,
    /// Retransmission timeouts.
    pub rtos: u64,
    /// Zero-window probes sent.
    pub window_probes: u64,
    /// Challenge ACKs sent (RFC 5961).
    pub challenge_acks: u64,
    /// Challenge ACKs suppressed by the rate limit.
    pub challenge_acks_suppressed: u64,
    /// Out-of-order segments dropped because the range table was full.
    pub ooo_dropped: u64,
    /// Segments dropped by PAWS or missing timestamps.
    pub paws_dropped: u64,
    /// Duplicate ACKs received.
    pub dup_acks: u64,
}
