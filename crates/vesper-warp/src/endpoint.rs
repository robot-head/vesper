//! The end-to-end §35.3 Warp data path (wave-four integration, issue #45).
//!
//! [`WarpEndpoint`] wires every wave-four lane into one core-local,
//! sans-IO, allocation-free endpoint over an AF_XDP-shaped UMEM driver:
//!
//! ```text
//! harvest (lane 3) → header split (lane 1) → CID lookup in the shard
//! (lane 6) → header-protection removal (lane 5) → full PN decode from the
//! tile's largest received PN (lane 1) → AEAD open with the full-PN nonce
//! (lane 5) → frames, ACK processing, RFC 9002 events (lane 1) → BBRv3 via
//! the event adapter (lane 2, [`crate::cc`]) → turn-around in place (lane 3)
//! → seal under a linear nonce ticket (lane 5) → BBRv3 EDT schedule
//! (lane 2) → software EDT launch queue (lane 4) → paced submit (lane 3) →
//! lease parked until ACK or PTO (lane 4).
//! ```
//!
//! [`WarpEndpoint::poll_and_step_quic_rx_one`] is the §35.3 function. The
//! §35.3 sketch's literal choices are generalised where the sketch is only
//! illustrative: the reply carries the connection's next packet number
//! (from the tile and the nonce sealer) instead of `full_pn + 1`, its
//! length is whatever the ACK plus the application's reply frames need
//! instead of a fixed 64 bytes, and the reply is sent only when the
//! received packet was ack-eliciting.
//!
//! Linearity follows the §35.3 `[T-Let]` rule in host-Rust form: once a
//! received frame is harvested, every exit hands it back (recycle,
//! turn-around) before an error is returned, so no `?` runs while it is
//! live. Leases are retired only on acknowledgement or loss.
//!
//! Streams (wave five, issue #133): in [`StreamMode::Reliable`] (the
//! default) every connection has a [`StreamSet`]. Received STREAM,
//! RESET_STREAM, STOP_SENDING, MAX_*, *_BLOCKED and CRYPTO frames go to
//! it; the application writes and reads through
//! [`WarpEndpoint::stream_write`] and [`WarpEndpoint::stream_read`];
//! [`WarpEndpoint::poll`] (or [`WarpEndpoint::flush`]) packs pending
//! control frames and stream data into paced packets under the congestion
//! window, both flow-control credits and the launch queue, and replies to
//! received packets carry them too. RFC 9002 acknowledgements and losses
//! are fed back to the stream layer's per-packet frame record, so lost
//! stream ranges are retransmitted (RFC 9000 §13.3) and acknowledged ones
//! free buffer space. PTO probes carry stream data when there is some. A
//! peer violation closes the connection with CONNECTION_CLOSE carrying the
//! RFC 9000 error code. [`StreamMode::Raw`] keeps the wave-four behaviour:
//! STREAM frames go to the [`Responder`] and [`WarpEndpoint::send_stream`]
//! sends unreliable raw frames.
//!
//! Handshake (wave five, issue #135): [`WarpEndpoint::connect`] (client)
//! and [`WarpEndpoint::listen`] (server) open connections with lane 12's
//! TLS 1.3 handshake instead of caller-supplied secrets. Initial and
//! Handshake packets use long headers with their own packet-number
//! spaces, keys, ACKs, loss detection and PTO probes in the connection's
//! RFC 9002 recovery; their CRYPTO data travels through lane 13's
//! per-space CRYPTO buffers, so lost handshake bytes are retransmitted as
//! ranges. Keys are installed and discarded per space (RFC 9001 §4.9),
//! the server enforces the 3× anti-amplification limit until the client's
//! address is validated (RFC 9000 §8.1), client Initials are padded to
//! 1200 bytes, the negotiated transport parameters replace the stream
//! layer's defaults, the server sends HANDSHAKE_DONE (retransmitted until
//! acknowledged), and key updates wait for confirmation. Long-header
//! packets are copied out of the UMEM frame and answered from fresh
//! frames: the zero-copy turn-around is the 1-RTT data path only.
//! [`WarpEndpoint::open_connection`] still opens a connection from
//! caller-supplied 1-RTT keys.
//!
//! What this is not: no Retry, 0-RTT, Version Negotiation, stateless
//! reset or NEW_TOKEN; a short-header packet coalesced after long-header
//! packets in one received datagram is dropped (ours never coalesce);
//! Initial and Handshake packets are recovered but not fed to BBRv3
//! (whose snapshots are keyed by application packet number) and not
//! paced; no connection migration; and no hardware EDT, RSS or crypto
//! offload. With [`SoftNic`](vesper_warp_umem::SoftNic) the
//! "NIC" is lane 3's deterministic model; with lane 3's gated AF_XDP
//! backend it is a real socket, which this crate's tests do not exercise.

use std::fmt;

use vesper_warp_bbr::BbrConfig;
use vesper_warp_crypto::{CryptoError, OneRttKeys, TAG_LEN, Unprotected};
use vesper_warp_shard::{ConnState, ConnTable, ConnectionId, SlotHandle, TableConfig};
use vesper_warp_tls::{Event as TlsEvent, QuicTls, ServerCids, Space, TransportParameters};
use vesper_warp_umem::{
    DescError, InFlightQuicPkt, RxHarvested, TxStaged, UmemError, UmemFrame, XdpNic, Xsk,
    XskBackend,
};
use vesper_warp_wheel::{
    EdtLaunchQueue, PacketNumberSpace, RetransmitLeases, TimerHandle, Wheel, WheelConfig,
};

use crate::ack::{AckError, Ecn, ReceivedPackets};
use crate::cc::{CcError, QuicBbr};
use crate::frame::{Frame, FrameError, StreamFrame, scan_payload};
use crate::handshake::{
    ClientSetup, ConnectionEvent, HandshakePhase, Hs, Listener, MIN_INITIAL_DATAGRAM,
    MIN_ORIGINAL_DCID, NO_CREDIT, TlsError, peer_params, pn_space,
};
use crate::header::{
    HeaderError, HeaderParser, LongHeader, LongPacketType, ParsedPacket, ProtectedHeader,
    write_long,
};
use crate::packet_number::{PnLen, TruncatedPn, encoded_len};
use crate::recovery::{
    self, CongestionEvent, HandshakeStatus, LossRecovery, RecoveryConfig, RecoveryError,
    TimeoutAction,
};
use crate::stream::{
    PacketFrames, PeerParams, ReadOutcome, Role, StreamConfig, StreamError, StreamEvent, StreamSet,
    TransportError,
};
use crate::{PacketType, PnSpace};

/// Bits of a connection's packet number kept in a lease key. The lease
/// table is shared by all connections of the endpoint and keyed by
/// `(space, pn)`, so the key packs `slot_index << LEASE_PN_BITS | pn`.
pub const LEASE_PN_BITS: u32 = 40;
/// Largest connection packet number this endpoint sends, plus one.
pub const MAX_SEND_PN: u64 = 1 << LEASE_PN_BITS;
/// Largest number of connections (slot indices) a lease key can name.
pub const MAX_CONNECTIONS: u32 = 1 << (62 - LEASE_PN_BITS);

/// Largest short header: first byte, 20-byte DCID, 4-byte packet number.
const MAX_SHORT_HEADER: usize = 1 + 20 + 4;
/// Largest long header we write: first byte, version, two 20-byte CIDs
/// with their lengths, an empty token's length, a 2-byte Length and a
/// 4-byte packet number.
const MAX_LONG_HEADER: usize = 1 + 4 + 1 + 20 + 1 + 20 + 1 + 2 + 4;
/// ACK ranges kept per connection and space.
pub(crate) const ACK_RANGES: usize = 32;
/// CONNECTION_CLOSE frame type for transport errors.
const CLOSE_FRAME_CRYPTO: u64 = 0x06;

/// Which §35.3 CID lookup to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CidLookup {
    /// Lane 6's open-addressed hash index (O(1) average).
    Index,
    /// Lane 6's `lookup_cid_simd` 16-lane tile scan (O(tiles)).
    TileScan,
}

/// How STREAM frames are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamMode {
    /// RFC 9000 streams: reassembly, flow control and retransmission.
    #[default]
    Reliable,
    /// Wave-four behaviour: received STREAM frames go to the [`Responder`]
    /// and [`WarpEndpoint::send_stream`] sends one raw, unreliable frame.
    Raw,
}

/// Endpoint parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointConfig {
    /// Client or server: decides which stream IDs are ours.
    pub role: Role,
    pub stream_mode: StreamMode,
    /// Per-connection stream limits and buffers (also assumed to be the
    /// peer's transport parameters unless given to
    /// [`WarpEndpoint::open_connection_with_params`]).
    pub streams: StreamConfig,
    /// Length of every connection ID, ours and the peer's (1..=20).
    pub cid_len: u8,
    /// Connection slots, at most [`MAX_CONNECTIONS`].
    pub max_connections: u32,
    pub cid_lookup: CidLookup,
    /// Maximum datagram size (BBR's SMSS), and the largest packet sent.
    pub max_datagram: u16,
    /// Sent packets tracked per connection (recovery, BBR snapshots).
    pub max_tracked_per_conn: usize,
    /// Leases that can be parked at once (all connections).
    pub lease_capacity: u32,
    /// Packets that can wait in the EDT launch queue at once.
    pub launch_capacity: u32,
    /// Launch-queue horizon, a model of `SO_TXTIME`'s horizon.
    pub launch_horizon_ns: u64,
    /// Timing-wheel tick for leases and launches.
    pub tick_ns: u64,
    /// Our `max_ack_delay` (and the peer's, assumed equal).
    pub max_ack_delay_ns: u64,
    /// Our `ack_delay_exponent` (and the peer's, assumed equal).
    pub ack_delay_exponent: u8,
    /// Idle timeout written to the connection tile on every packet.
    pub idle_timeout_ns: u64,
    /// RTT assumed before the first sample (RFC 9002 `kInitialRtt`).
    pub initial_rtt_ns: u64,
    /// Fill-ring entries replenished per [`WarpEndpoint::poll`].
    pub fill_batch: u32,
    /// Packets sealed under one send key before the endpoint initiates a
    /// key update (RFC 9001 §6), kept well below the cipher's §6.6
    /// confidentiality limit (2^23 for AES-GCM).
    pub key_update_interval: u64,
    /// Frames kept off the fill ring for fresh transmissions (sends that
    /// do not turn a received frame around: stream data, probes).
    pub tx_reserve: u32,
    /// Completion-ring entries harvested per [`WarpEndpoint::poll`].
    pub completion_batch: u32,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            role: Role::Client,
            stream_mode: StreamMode::Reliable,
            streams: StreamConfig::default(),
            cid_len: 8,
            max_connections: 64,
            cid_lookup: CidLookup::Index,
            max_datagram: 1200,
            max_tracked_per_conn: 256,
            lease_capacity: 4096,
            launch_capacity: 4096,
            launch_horizon_ns: 1_000_000_000,
            tick_ns: 1_000,
            max_ack_delay_ns: recovery::DEFAULT_MAX_ACK_DELAY_NS,
            ack_delay_exponent: crate::ack::DEFAULT_ACK_DELAY_EXPONENT,
            idle_timeout_ns: 30_000_000_000,
            initial_rtt_ns: recovery::K_INITIAL_RTT_NS,
            fill_batch: 64,
            tx_reserve: 64,
            key_update_interval: 1 << 22,
            completion_batch: 64,
        }
    }
}

/// Why a received packet was dropped. Every drop recycled its frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropReason {
    Header(HeaderError),
    /// A Retry, Version Negotiation or 0-RTT packet (not supported), or a
    /// long-header packet for a connection opened without a handshake.
    NotOneRtt,
    /// The keys for the packet's space are not installed (yet, or any
    /// more).
    KeysUnavailable,
    /// A client Initial in a datagram shorter than 1200 bytes, or with a
    /// Destination Connection ID shorter than 8 (RFC 9000 §14.1, §7.2).
    BadInitial,
    /// No listener, the connection table or the handshake refused a new
    /// connection.
    AcceptRefused,
    /// TLS rejected the handshake; the connection is closed with this
    /// QUIC error code.
    Handshake(u64),
    /// A short-header packet after long-header packets in one datagram.
    CoalescedOneRtt,
    UnknownCid,
    /// Header-protection removal, packet-number reconstruction or AEAD
    /// open failed; the buffer was restored.
    Crypto(CryptoError),
    /// Reserved header bits set after a successful open.
    ReservedBits,
    Duplicate(AckError),
    Frame(FrameError),
    Recovery(RecoveryError),
    /// The frame's bytes could not be read or written.
    Umem(UmemError),
    /// The peer violated the stream protocol; the connection is closed.
    Transport(TransportError),
    /// The connection is closed or closing.
    Closed,
}

/// Why a connection was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionError {
    /// We detected a peer violation and sent CONNECTION_CLOSE with it.
    Local(TransportError),
    /// The handshake failed (TLS alert, transport parameters, protocol
    /// violation); CONNECTION_CLOSE carried this code.
    Handshake { error_code: u64 },
    /// The peer sent CONNECTION_CLOSE.
    Peer {
        error_code: u64,
        /// `Some` for a transport close, `None` for an application close.
        frame_type: Option<u64>,
    },
}

/// Result of one [`WarpEndpoint::poll_and_step_quic_rx_one`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RxOutcome {
    /// The rx ring was empty.
    Idle,
    /// The driver rejected a malformed descriptor (its chunk, when known,
    /// went back to the fill ring).
    Rejected(DescError),
    /// The packet was dropped; its frame was recycled.
    Dropped(DropReason),
    /// The packet was authenticated and processed.
    Processed {
        conn: SlotHandle,
        pn: u64,
        /// A reply was turned around in the received frame.
        replied: bool,
    },
}

/// Endpoint errors. A received frame is never live when one is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarpError {
    Config(&'static str),
    Umem(UmemError),
    Crypto(CryptoError),
    Header(HeaderError),
    Frame(FrameError),
    Recovery(RecoveryError),
    Cc(CcError),
    /// The connection table refused the operation.
    Shard(&'static str),
    /// The handle does not name a live connection.
    StaleConnection,
    /// The congestion window is full.
    CwndLimited,
    /// The launch queue is full, or the connection's pacing schedule runs
    /// past the launch horizon: try again after [`WarpEndpoint::poll`].
    LaunchBackpressure,
    /// The packet does not fit in one datagram or frame.
    PacketTooLarge {
        len: usize,
    },
    /// The connection used up [`MAX_SEND_PN`] packet numbers.
    PacketNumbersExhausted,
    /// A stream API error.
    Stream(StreamError),
    /// The operation needs the other [`StreamMode`].
    StreamMode,
    /// The connection was closed.
    ConnectionClosed(ConnectionError),
    /// The TLS driver refused a setup.
    Tls(TlsError),
    /// The operation needs the handshake to have progressed further
    /// (peer transport parameters, 1-RTT keys or confirmation).
    Handshaking,
    /// The keys for this packet's space are not installed.
    KeysUnavailable,
    /// The server may not send more before the client's address is
    /// validated (RFC 9000 §8.1).
    AmplificationLimited,
}

impl fmt::Display for WarpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WarpError {}

macro_rules! warp_from {
    ($($variant:ident($ty:ty)),*) => {$(
        impl From<$ty> for WarpError {
            fn from(e: $ty) -> Self { Self::$variant(e) }
        }
    )*};
}
warp_from!(
    Umem(UmemError),
    Crypto(CryptoError),
    Header(HeaderError),
    Frame(FrameError),
    Recovery(RecoveryError),
    Cc(CcError),
    Stream(StreamError),
    Tls(TlsError)
);

/// The application side: what to send back for received stream data.
pub trait Responder {
    /// Called for each STREAM frame of an authenticated packet, before the
    /// reply is built. Writes reply frames into `out` and returns their
    /// length (0 for none). Frames that do not fit must not be written.
    fn on_stream(&mut self, conn: SlotHandle, frame: &StreamFrame<'_>, out: &mut [u8]) -> usize;

    /// A packet carrying this connection's data was declared lost. In
    /// [`StreamMode::Raw`] retransmitting is the application's choice; in
    /// [`StreamMode::Reliable`] the stream layer already requeued it.
    fn on_lost(&mut self, _conn: SlotHandle, _pn: u64) {}
}

/// Echoes every STREAM frame back on the same stream and offset.
#[derive(Debug, Clone, Copy, Default)]
pub struct Echo {
    pub echoed_bytes: u64,
    pub lost_packets: u64,
}

impl Responder for Echo {
    fn on_stream(&mut self, _conn: SlotHandle, frame: &StreamFrame<'_>, out: &mut [u8]) -> usize {
        let reply = Frame::Stream(StreamFrame {
            has_length: true,
            ..*frame
        });
        match reply.encode(out) {
            Ok(n) => {
                self.echoed_bytes += frame.data.len() as u64;
                n
            }
            Err(_) => 0,
        }
    }
    fn on_lost(&mut self, _conn: SlotHandle, _pn: u64) {
        self.lost_packets += 1;
    }
}

/// Counts received stream bytes and sends nothing but ACKs.
#[derive(Debug, Clone, Copy, Default)]
pub struct Discard {
    pub received_bytes: u64,
    pub stream_frames: u64,
    pub lost_packets: u64,
}

impl Responder for Discard {
    fn on_stream(&mut self, _conn: SlotHandle, frame: &StreamFrame<'_>, _out: &mut [u8]) -> usize {
        self.received_bytes += frame.data.len() as u64;
        self.stream_frames += 1;
        0
    }
    fn on_lost(&mut self, _conn: SlotHandle, _pn: u64) {
        self.lost_packets += 1;
    }
}

/// Observes every protected datagram just before it is handed to the
/// NIC's tx ring (a capture point for tests and simulators).
pub trait TxTap {
    fn on_transmit(&mut self, conn: SlotHandle, pn: u64, edt_ns: u64, datagram: &[u8]);
}

impl TxTap for () {
    fn on_transmit(&mut self, _: SlotHandle, _: u64, _: u64, _: &[u8]) {}
}

/// Endpoint counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EndpointStats {
    pub rx_processed: u64,
    pub rx_rejected: u64,
    pub rx_dropped: u64,
    pub rx_dropped_crypto: u64,
    pub rx_dropped_unknown_cid: u64,
    pub replies: u64,
    pub packets_sent: u64,
    pub ack_only_sent: u64,
    pub probes_sent: u64,
    pub launch_queued: u64,
    pub launch_released: u64,
    pub leases_parked: u64,
    pub leases_retired_acked: u64,
    pub leases_retired_lost: u64,
    pub pto_fired: u64,
    /// Connection recovery timers (loss or PTO) that fired.
    pub recovery_timers_fired: u64,
    /// Key updates this endpoint initiated.
    pub key_updates: u64,
    /// Replies whose application frames did not fit one datagram; the ACK
    /// was sent alone.
    pub oversize_replies: u64,
    /// Reply data dropped because the launch queue had no room.
    pub backpressured_replies: u64,
    /// ACK-only replies deferred because the launch queue was full.
    pub deferred_acks: u64,
    /// PTO probes that could not be sent.
    pub probes_failed: u64,
    /// Packets the launch queue refused after sealing (abandoned, never
    /// sent early).
    pub launch_refused: u64,
    /// Expired leases that recovery no longer tracked (expected 0).
    pub leases_orphaned: u64,
    pub recovery_timeouts: u64,
    pub packets_acked: u64,
    pub packets_lost: u64,
    /// Reply data dropped because the congestion window was full (the ACK
    /// was still sent).
    pub cwnd_limited_replies: u64,
    /// Packets that could be neither submitted nor queued and were
    /// abandoned after sealing (recovery later declares them lost).
    pub tx_abandoned: u64,
    pub completions_leased: u64,
    pub completions_freed: u64,
    /// Packets carrying stream-layer frames (data or control).
    pub stream_packets_sent: u64,
    /// PTO probes that carried stream data instead of a PING.
    pub data_probes_sent: u64,
    /// Peer violations that closed a connection.
    pub transport_errors: u64,
    /// CONNECTION_CLOSE frames sent and received.
    pub closes_sent: u64,
    pub closes_received: u64,
    /// Received packets dropped because their connection was closed.
    pub rx_dropped_closed: u64,
    /// Queued packets abandoned because their connection closed first.
    pub tx_suppressed_closed: u64,
    /// Initial and Handshake packets sent and processed.
    pub handshake_packets_sent: u64,
    pub handshake_packets_received: u64,
    /// Connections a server accepted.
    pub accepted: u64,
    /// Handshakes that failed and closed their connection.
    pub handshake_failures: u64,
    /// Sends a server held back at the anti-amplification limit.
    pub amplification_blocked: u64,
    /// Connection events dropped because the queue was full.
    pub connection_events_dropped: u64,
}

/// A connection's non-tile state. The tile (lane 6) holds the hot scalars:
/// largest received PN, next PN, BBR bandwidth/min-RTT, cwnd, bytes in
/// flight, next EDT, idle deadline and the peer CID.
struct Conn {
    handle: SlotHandle,
    /// 1-RTT keys: supplied by the caller, or installed by the handshake.
    keys: Option<OneRttKeys>,
    /// The TLS handshake and long-header spaces; `None` for a connection
    /// opened with caller-supplied keys.
    hs: Option<Box<Hs>>,
    /// The peer's ack_delay_exponent, for decoding its ACK frames.
    peer_ack_exponent: u8,
    recovery: LossRecovery,
    received: ReceivedPackets<ACK_RANGES>,
    cc: QuicBbr,
    /// Last `sent_ns` given to recovery (recovery requires it monotone).
    last_sent_ns: u64,
    /// Latest departure time scheduled on this connection.
    last_edt_ns: u64,
    /// The recovery timer as armed in the endpoint's timer wheel.
    timer: Option<(TimerHandle, u64)>,
    /// Packets sealed under the current send key, and its generation.
    sent_under_key: u64,
    send_generation: u64,
    /// Listed in `deferred_acks`.
    ack_deferred: bool,
    streams: StreamSet,
    error: Option<ConnectionError>,
    /// When CONNECTION_CLOSE was last sent (closing state, RFC 9000 §10.2.1).
    close_sent_ns: Option<u64>,
}

/// A packet waiting for its earliest departure time.
struct Staged {
    conn: SlotHandle,
    frame: UmemFrame<TxStaged>,
    pn: u64,
    edt_ns: u64,
    /// In flight: park the lease until ACK or PTO.
    park: bool,
    /// The tap already saw it (a resubmission after a full tx ring).
    tapped: bool,
    /// The application/stream frames made it into the packet (a reply
    /// whose frames did not fit is sent ACK-only).
    carried: bool,
    /// A CONNECTION_CLOSE packet: the only one sent once a connection is
    /// closing.
    close: bool,
}

/// A parked in-flight packet (§35.2 `park_for_ack_or_pto`).
struct Lease {
    conn: SlotHandle,
    pkt: InFlightQuicPkt,
}

/// Read-only view of one connection.
pub struct ConnectionView<'a> {
    pub tile: ConnState,
    pub recovery: &'a LossRecovery,
    pub cc: &'a QuicBbr,
    pub received: &'a ReceivedPackets<ACK_RANGES>,
    /// `None` until the handshake installs 1-RTT keys.
    pub keys: Option<&'a OneRttKeys>,
    pub streams: &'a StreamSet,
    pub error: Option<ConnectionError>,
    pub phase: HandshakePhase,
    /// The TLS driver (ALPN, peer certificates, key states), when the
    /// connection was opened with a handshake.
    pub tls: Option<&'a QuicTls>,
}

/// What [`WarpEndpoint::poll`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PollReport {
    pub launched: u32,
    pub completions: u32,
    pub replenished: u32,
    pub ptos: u32,
}

/// One core's Warp endpoint. See the module docs.
pub struct WarpEndpoint<B: XskBackend, R: Responder, T: TxTap = ()> {
    config: EndpointConfig,
    xsk: Xsk<B>,
    parser: HeaderParser,
    table: ConnTable,
    conns: Vec<Option<Conn>>,
    leases: RetransmitLeases<Lease>,
    launch: EdtLaunchQueue<Staged>,
    /// Each connection's RFC 9002 loss-detection timer.
    timers: Wheel<SlotHandle>,
    /// Connections whose ACK-only reply found the launch queue full; the
    /// ACK stays owed and [`WarpEndpoint::poll`] sends it once there is room.
    deferred_acks: Vec<SlotHandle>,
    /// Acknowledgements and losses of the packet being processed.
    events: Vec<CongestionEvent>,
    /// Plaintext of the packet being built.
    scratch: Box<[u8]>,
    /// The application's reply frames.
    app_out: Box<[u8]>,
    /// Frames of the packet being built from the stream layer.
    record: PacketFrames,
    app: R,
    tap: T,
    stats: EndpointStats,
    /// Accepts client Initials (server endpoints).
    listener: Option<Listener>,
    /// Client-chosen Destination Connection IDs that route Initials to a
    /// server connection until its Initial keys are discarded.
    initial_dcids: Vec<(ConnectionId, SlotHandle)>,
    conn_events: std::collections::VecDeque<ConnectionEvent>,
    /// A received long-header datagram, copied out of its UMEM frame.
    rx_copy: Box<[u8]>,
    /// In-order CRYPTO bytes on their way to TLS.
    tls_buf: Box<[u8]>,
}

fn lease_key(conn: SlotHandle, pn: u64) -> u64 {
    ((conn.index() as u64) << LEASE_PN_BITS) | pn
}

const ESTABLISHED: HandshakeStatus = HandshakeStatus {
    confirmed: true,
    peer_completed_address_validation: true,
    has_handshake_keys: true,
    at_amplification_limit: false,
};

impl<B: XskBackend, R: Responder, T: TxTap> WarpEndpoint<B, R, T> {
    /// An endpoint over `xsk`. Fills the fill ring.
    pub fn new(config: EndpointConfig, xsk: Xsk<B>, app: R, tap: T) -> Result<Self, WarpError> {
        if config.max_connections == 0 || config.max_connections > MAX_CONNECTIONS {
            return Err(WarpError::Config("max_connections out of range"));
        }
        if config.max_tracked_per_conn == 0 {
            return Err(WarpError::Config("max_tracked_per_conn must be positive"));
        }
        let frame_room = xsk.config().frame_size - xsk.config().headroom;
        if u32::from(config.max_datagram) > frame_room {
            return Err(WarpError::Config("max_datagram exceeds the UMEM frame"));
        }
        config.streams.validate().map_err(WarpError::Config)?;
        let parser = HeaderParser::new(usize::from(config.cid_len))?;
        let table = ConnTable::new(TableConfig::new(config.max_connections, config.cid_len))
            .map_err(|_| WarpError::Shard("invalid table configuration"))?;
        let wheel = |capacity| WheelConfig {
            tick_ns: config.tick_ns,
            ..WheelConfig::with_capacity(capacity)
        };
        let leases = RetransmitLeases::new(wheel(config.lease_capacity))
            .map_err(|_| WarpError::Config("invalid lease wheel"))?;
        let launch = EdtLaunchQueue::new(wheel(config.launch_capacity), config.launch_horizon_ns)
            .map_err(|_| WarpError::Config("invalid launch wheel"))?;
        let timers = Wheel::new(wheel(config.max_connections))
            .map_err(|_| WarpError::Config("invalid timer wheel"))?;
        let mut conns = Vec::with_capacity(config.max_connections as usize);
        conns.resize_with(config.max_connections as usize, || None);
        let buf = usize::from(config.max_datagram) + TAG_LEN;
        let rx_frame = xsk.config().frame_size as usize;
        let mut endpoint = Self {
            config,
            xsk,
            parser,
            table,
            conns,
            leases,
            launch,
            timers,
            deferred_acks: Vec::with_capacity(config.max_connections as usize),
            events: Vec::with_capacity(config.max_tracked_per_conn + 8),
            scratch: vec![0u8; buf].into_boxed_slice(),
            app_out: vec![0u8; buf].into_boxed_slice(),
            record: PacketFrames::default(),
            app,
            tap,
            stats: EndpointStats::default(),
            listener: None,
            initial_dcids: Vec::with_capacity(config.max_connections as usize),
            conn_events: std::collections::VecDeque::with_capacity(
                4 * config.max_connections as usize,
            ),
            rx_copy: vec![0u8; rx_frame].into_boxed_slice(),
            tls_buf: vec![0u8; config.streams.crypto_buffer as usize].into_boxed_slice(),
        };
        if config.tx_reserve >= endpoint.xsk.config().frame_count {
            return Err(WarpError::Config("tx_reserve leaves no receive frames"));
        }
        endpoint.replenish(u32::MAX);
        Ok(endpoint)
    }

    pub const fn config(&self) -> &EndpointConfig {
        &self.config
    }
    pub const fn stats(&self) -> EndpointStats {
        self.stats
    }
    pub fn xsk(&self) -> &Xsk<B> {
        &self.xsk
    }
    /// The driver, e.g. to deliver packets into a software NIC.
    pub fn xsk_mut(&mut self) -> &mut Xsk<B> {
        &mut self.xsk
    }
    pub const fn app(&self) -> &R {
        &self.app
    }
    pub fn app_mut(&mut self) -> &mut R {
        &mut self.app
    }
    pub const fn tap(&self) -> &T {
        &self.tap
    }
    pub fn tap_mut(&mut self) -> &mut T {
        &mut self.tap
    }
    pub const fn table(&self) -> &ConnTable {
        &self.table
    }
    /// Leases parked until ACK or PTO.
    pub fn parked_leases(&self) -> usize {
        self.leases.len()
    }
    /// Packets waiting for their departure time.
    pub fn queued_launches(&self) -> usize {
        self.launch.len()
    }

    /// Opens an established 1-RTT connection: we receive on `local_cid`,
    /// send to `peer_cid`, and protect with `keys` (from a TLS stack; no
    /// handshake is implemented). The peer's stream transport parameters
    /// are assumed equal to ours.
    pub fn open_connection(
        &mut self,
        local_cid: &[u8],
        peer_cid: &[u8],
        keys: OneRttKeys,
        now_ns: u64,
    ) -> Result<SlotHandle, WarpError> {
        let peer = self.config.streams.local_params();
        self.open_connection_with_params(local_cid, peer_cid, keys, peer, now_ns)
    }

    /// [`WarpEndpoint::open_connection`] with the peer's stream transport
    /// parameters (as a handshake would learn them).
    pub fn open_connection_with_params(
        &mut self,
        local_cid: &[u8],
        peer_cid: &[u8],
        keys: OneRttKeys,
        peer: PeerParams,
        now_ns: u64,
    ) -> Result<SlotHandle, WarpError> {
        let len = usize::from(self.config.cid_len);
        if local_cid.len() != len || peer_cid.len() != len {
            return Err(WarpError::Config("connection IDs must have cid_len bytes"));
        }
        self.insert_conn(
            local_cid,
            peer_cid,
            Some(keys),
            None,
            peer,
            ESTABLISHED,
            now_ns,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_conn(
        &mut self,
        local_cid: &[u8],
        peer_cid: &[u8],
        keys: Option<OneRttKeys>,
        hs: Option<Box<Hs>>,
        peer: PeerParams,
        status: HandshakeStatus,
        now_ns: u64,
    ) -> Result<SlotHandle, WarpError> {
        if peer.initial_max_streams_bidi > crate::frame::MAX_STREAM_COUNT
            || peer.initial_max_streams_uni > crate::frame::MAX_STREAM_COUNT
        {
            return Err(WarpError::Config("peer stream limits out of range"));
        }
        let streams = StreamSet::new(self.config.role, self.config.streams, peer)
            .map_err(WarpError::Config)?;
        if local_cid.len() != usize::from(self.config.cid_len) {
            return Err(WarpError::Config("connection IDs must have cid_len bytes"));
        }
        let local = ConnectionId::from_slice(local_cid)
            .map_err(|_| WarpError::Config("connection ID too long"))?;
        let peer = ConnectionId::from_slice(peer_cid)
            .map_err(|_| WarpError::Config("connection ID too long"))?;
        let cc = QuicBbr::new(
            BbrConfig::quic(u32::from(self.config.max_datagram)),
            now_ns,
            self.config.max_tracked_per_conn,
        )?;
        let state = ConnState::new(
            peer,
            cc.cwnd_bytes(),
            now_ns.saturating_add(self.config.idle_timeout_ns),
        );
        let handle = self.table.insert(&local, &state).map_err(|e| {
            WarpError::Shard(match e {
                vesper_warp_shard::InsertError::Full => "connection table full",
                vesper_warp_shard::InsertError::DuplicateCid => "duplicate connection ID",
                _ => "connection table refused the connection",
            })
        })?;
        let recovery = LossRecovery::new(
            RecoveryConfig {
                max_ack_delay_ns: self.config.max_ack_delay_ns,
                initial_rtt_ns: self.config.initial_rtt_ns,
                max_tracked_per_space: self.config.max_tracked_per_conn,
            },
            status,
        );
        self.conns[handle.index() as usize] = Some(Conn {
            handle,
            keys,
            hs,
            peer_ack_exponent: self.config.ack_delay_exponent,
            recovery,
            received: ReceivedPackets::new(PnSpace::ApplicationData, self.config.max_ack_delay_ns),
            cc,
            last_sent_ns: now_ns,
            last_edt_ns: now_ns,
            timer: None,
            sent_under_key: 0,
            send_generation: 0,
            ack_deferred: false,
            streams,
            error: None,
            close_sent_ns: None,
        });
        self.write_tile(handle);
        Ok(handle)
    }

    // ----- handshake (issue #135) ---------------------------------------

    /// Opens a client connection with a TLS 1.3 handshake (RFC 9001): we
    /// receive on `local_cid` (`cid_len` bytes) and send the first Initial
    /// to `original_dcid` (8 to 20 bytes, unpredictable per RFC 9000
    /// §7.2). The ClientHello leaves at once; the connection carries
    /// streams once [`ConnectionEvent::HandshakeComplete`] is reported.
    pub fn connect(
        &mut self,
        setup: &ClientSetup,
        server_name: &str,
        local_cid: &[u8],
        original_dcid: &[u8],
        now_ns: u64,
    ) -> Result<SlotHandle, WarpError> {
        if self.config.role != Role::Client {
            return Err(WarpError::Config("connect needs a client endpoint"));
        }
        self.check_handshake_config()?;
        if original_dcid.len() < MIN_ORIGINAL_DCID || original_dcid.len() > 20 {
            return Err(WarpError::Config(
                "the original destination connection ID must have 8 to 20 bytes",
            ));
        }
        let params = local_transport_params(&self.config, local_cid);
        let tls = QuicTls::client(setup, server_name, &params, original_dcid)?;
        let hs = self.new_hs(tls, local_cid);
        let status = HandshakeStatus {
            confirmed: false,
            peer_completed_address_validation: false,
            has_handshake_keys: false,
            at_amplification_limit: false,
        };
        let slot = self.insert_conn(
            local_cid,
            original_dcid,
            None,
            Some(hs),
            NO_CREDIT,
            status,
            now_ns,
        )?;
        if let Err(code) = self.apply_tls_events(slot, now_ns) {
            return Err(WarpError::ConnectionClosed(ConnectionError::Handshake {
                error_code: code,
            }));
        }
        self.flush_handshake(slot, now_ns)?;
        Ok(slot)
    }

    /// Makes a server endpoint accept connections: a client Initial for an
    /// unknown connection ID starts a TLS handshake with `listener`'s
    /// certificate, under a connection ID from its [`CidSource`](crate::handshake::CidSource).
    /// New connections are reported as [`ConnectionEvent::Accepted`].
    pub fn listen(&mut self, listener: Listener) -> Result<(), WarpError> {
        if self.config.role != Role::Server {
            return Err(WarpError::Config("listen needs a server endpoint"));
        }
        self.check_handshake_config()?;
        self.listener = Some(listener);
        Ok(())
    }

    /// The next connection event (accepted, handshake complete or
    /// confirmed), oldest first.
    pub fn poll_connection_event(&mut self) -> Option<ConnectionEvent> {
        self.conn_events.pop_front()
    }

    /// Where a connection's handshake stands.
    pub fn handshake_phase(&self, conn: SlotHandle) -> Option<HandshakePhase> {
        let c = self.conn(conn).ok()?;
        Some(
            c.hs.as_ref()
                .map_or(HandshakePhase::External, |h| h.phase()),
        )
    }

    fn check_handshake_config(&self) -> Result<(), WarpError> {
        if self.config.stream_mode != StreamMode::Reliable {
            return Err(WarpError::StreamMode);
        }
        if usize::from(self.config.max_datagram) < MIN_INITIAL_DATAGRAM {
            return Err(WarpError::Config(
                "a handshake needs max_datagram of at least 1200 (RFC 9000 §14.1)",
            ));
        }
        Ok(())
    }

    fn new_hs(&self, tls: QuicTls, local_cid: &[u8]) -> Box<Hs> {
        Hs::new(
            tls,
            local_cid,
            self.config.max_ack_delay_ns,
            self.config.max_tracked_per_conn,
            self.config.streams.crypto_buffer as usize,
        )
    }

    fn push_conn_event(&mut self, ev: ConnectionEvent) {
        if self.conn_events.len() < self.conn_events.capacity() {
            self.conn_events.push_back(ev);
        } else {
            self.stats.connection_events_dropped += 1;
        }
    }

    /// A server's new connection for a client Initial with an unknown
    /// Destination Connection ID.
    fn accept(
        &mut self,
        pkt: &[u8],
        header: &ProtectedHeader,
        datagram_len: usize,
        now_ns: u64,
    ) -> Result<SlotHandle, DropReason> {
        if self.listener.is_none() {
            return Err(DropReason::UnknownCid);
        }
        let odcid = header.dcid(pkt);
        if datagram_len < MIN_INITIAL_DATAGRAM || odcid.len() < MIN_ORIGINAL_DCID {
            return Err(DropReason::BadInitial);
        }
        if self.initial_dcids.len() >= self.initial_dcids.capacity() {
            return Err(DropReason::AcceptRefused);
        }
        let n = usize::from(self.config.cid_len);
        let mut local = [0u8; 20];
        let listener = self.listener.as_mut().expect("checked");
        let mut fresh = false;
        for _ in 0..8 {
            listener.cids.next_cid(&mut local[..n]);
            if self.table.lookup(&local[..n]).ok().flatten().is_none() {
                fresh = true;
                break;
            }
        }
        if !fresh {
            return Err(DropReason::AcceptRefused);
        }
        let params = local_transport_params(&self.config, &local[..n]);
        let cids = ServerCids {
            original_dcid: odcid,
            client_scid: header.scid(pkt),
            retry_scid: None,
        };
        let tls = QuicTls::server(&listener.setup, &params, cids)
            .map_err(|_| DropReason::AcceptRefused)?;
        let hs = self.new_hs(tls, &local[..n]);
        let status = HandshakeStatus {
            confirmed: false,
            // RFC 9002: a server assumes the peer validated its address.
            peer_completed_address_validation: true,
            has_handshake_keys: false,
            at_amplification_limit: false,
        };
        let slot = self
            .insert_conn(
                &local[..n],
                header.scid(pkt),
                None,
                Some(hs),
                NO_CREDIT,
                status,
                now_ns,
            )
            .map_err(|_| DropReason::AcceptRefused)?;
        let odcid = ConnectionId::from_slice(odcid).map_err(|_| DropReason::BadInitial)?;
        self.initial_dcids.push((odcid, slot));
        self.apply_tls_events(slot, now_ns)
            .map_err(DropReason::Handshake)?;
        Ok(slot)
    }

    /// A received datagram that starts with a long header: copied out of
    /// its frame (which goes straight back to the fill ring), then every
    /// coalesced Initial and Handshake packet in it is processed.
    fn on_long_datagram(
        &mut self,
        frame: UmemFrame<RxHarvested>,
        now_ns: u64,
    ) -> Result<RxOutcome, WarpError> {
        let mut buf = std::mem::take(&mut self.rx_copy);
        let len = match self.xsk.view_frame_bytes(&frame) {
            Ok(bytes) => {
                let n = bytes.len().min(buf.len());
                buf[..n].copy_from_slice(&bytes[..n]);
                n
            }
            Err(e) => {
                self.rx_copy = buf;
                return self.drop_rx(frame, DropReason::Umem(e));
            }
        };
        if let Err(refused) = self.xsk.recycle_to_fill_ring(frame) {
            self.rx_copy = buf;
            return Err(WarpError::Umem(refused.error));
        }
        let out = self.process_long_datagram(&mut buf[..len], now_ns);
        self.rx_copy = buf;
        out
    }

    fn count_drop(&mut self, reason: &DropReason) {
        self.stats.rx_dropped += 1;
        match reason {
            DropReason::Crypto(_) => self.stats.rx_dropped_crypto += 1,
            DropReason::UnknownCid => self.stats.rx_dropped_unknown_cid += 1,
            DropReason::Closed => self.stats.rx_dropped_closed += 1,
            _ => {}
        }
    }

    fn process_long_datagram(
        &mut self,
        dgram: &mut [u8],
        now_ns: u64,
    ) -> Result<RxOutcome, WarpError> {
        let total = dgram.len();
        let mut off = 0;
        let mut processed: Option<(SlotHandle, u64)> = None;
        let mut dropped: Option<DropReason> = None;
        let mut first = true;
        while off < total {
            let rest = &mut dgram[off..];
            let header = match self.parser.parse(rest) {
                Ok(ParsedPacket::Protected(h)) => h,
                Ok(_) => {
                    dropped.get_or_insert(DropReason::NotOneRtt);
                    break;
                }
                Err(e) => {
                    dropped.get_or_insert(DropReason::Header(e));
                    break;
                }
            };
            let end = header.packet_end;
            match header.packet_type {
                PacketType::OneRtt => {
                    dropped.get_or_insert(DropReason::CoalescedOneRtt);
                    break;
                }
                PacketType::ZeroRtt => {
                    dropped.get_or_insert(DropReason::NotOneRtt);
                }
                PacketType::Initial | PacketType::Handshake => {
                    let datagram = if first { total } else { 0 };
                    match self.process_long_packet(&mut rest[..end], &header, datagram, now_ns) {
                        Ok(v) => processed = Some(v),
                        Err((slot, reason)) => {
                            if let (Some(slot), DropReason::Transport(e)) = (slot, &reason) {
                                self.stats.transport_errors += 1;
                                if let Ok(c) = self.conn_mut(slot) {
                                    c.error = Some(ConnectionError::Local(*e));
                                }
                                let _ =
                                    self.send_close_code(slot, e.code.code(), e.frame_type, now_ns);
                            }
                            dropped.get_or_insert(reason);
                        }
                    }
                }
            }
            first = false;
            off += end;
        }
        if let Some(reason) = &dropped {
            self.count_drop(reason);
        }
        let Some((slot, pn)) = processed else {
            return Ok(RxOutcome::Dropped(dropped.unwrap_or(DropReason::NotOneRtt)));
        };
        self.stats.rx_processed += 1;
        let live = self.conn(slot).is_ok_and(|c| c.error.is_none());
        if live {
            if let Ok(mut st) = self.table.get(slot) {
                st.idle_deadline_ns = now_ns.saturating_add(self.config.idle_timeout_ns);
                let _ = self.table.set(slot, &st);
            }
            if self.drive_tls(slot, now_ns).is_ok() {
                self.flush_handshake(slot, now_ns)?;
                if self.conn(slot).is_ok_and(|c| c.error.is_none()) {
                    self.flush(slot, now_ns)?;
                }
            }
        } else if self
            .conn(slot)
            .is_ok_and(|c| matches!(c.error, Some(ConnectionError::Peer { .. })))
        {
            self.stats.closes_received += 1;
        }
        self.write_tile(slot);
        Ok(RxOutcome::Processed {
            conn: slot,
            pn,
            replied: false,
        })
    }

    /// One Initial or Handshake packet: route, open, frames. `datagram` is
    /// the datagram's length for its first packet (amplification credit),
    /// else 0. On error, names the connection when it was found.
    fn process_long_packet(
        &mut self,
        pkt: &mut [u8],
        header: &ProtectedHeader,
        datagram: usize,
        now_ns: u64,
    ) -> Result<(SlotHandle, u64), (Option<SlotHandle>, DropReason)> {
        let dcid = header.dcid(pkt);
        let routed = self.lookup(dcid).or_else(|| {
            self.initial_dcids
                .iter()
                .find(|(c, _)| c.as_bytes() == dcid)
                .map(|(_, h)| *h)
        });
        let slot = match routed {
            Some(slot) => slot,
            None if header.packet_type == PacketType::Initial => {
                let slot = self
                    .accept(pkt, header, datagram, now_ns)
                    .map_err(|r| (None, r))?;
                // The connection is kept only if its first packet opens.
                return match self.process_routed(slot, pkt, header, datagram, now_ns) {
                    Ok(v) => {
                        self.stats.accepted += 1;
                        self.push_conn_event(ConnectionEvent::Accepted(slot));
                        Ok(v)
                    }
                    Err((_, reason)) => {
                        let _ = self.close_connection(slot, now_ns);
                        Err((None, reason))
                    }
                };
            }
            None => return Err((None, DropReason::UnknownCid)),
        };
        self.process_routed(slot, pkt, header, datagram, now_ns)
    }

    fn process_routed(
        &mut self,
        slot: SlotHandle,
        pkt: &mut [u8],
        header: &ProtectedHeader,
        datagram: usize,
        now_ns: u64,
    ) -> Result<(SlotHandle, u64), (Option<SlotHandle>, DropReason)> {
        let fail = |r| (Some(slot), r);
        let space = header.packet_type.space();
        let idx = slot.index() as usize;
        let Some(Some(conn)) = self.conns.get_mut(idx) else {
            return Err(fail(DropReason::UnknownCid));
        };
        if conn.error.is_some() {
            return Err(fail(DropReason::Closed));
        }
        let Some(hs) = conn.hs.as_mut() else {
            return Err(fail(DropReason::NotOneRtt));
        };
        if let Some(amp) = hs.amp.as_mut() {
            amp.received += datagram as u64;
        }
        let client = !hs.is_server();
        let ls = hs.long(space).expect("long-header space");
        let Some(opener) = ls.opener.as_mut() else {
            return Err(fail(DropReason::KeysUnavailable));
        };
        let largest = ls.received.largest();
        let opened = opener
            .open_packet(&mut pkt[..header.packet_end], header.pn_offset, |u| {
                reconstruct(u, largest)
            })
            .map_err(|e| fail(DropReason::Crypto(e)))?;
        let u = header
            .packet_number_field(pkt)
            .map_err(|e| fail(DropReason::Header(e)))?;
        u.check_reserved_bits()
            .map_err(|_| fail(DropReason::ReservedBits))?;
        let pn = opened.packet_number;
        ls.received
            .check(pn)
            .map_err(|e| fail(DropReason::Duplicate(e)))?;
        self.stats.handshake_packets_received += 1;
        // RFC 9000 §7.2: the client adopts the server's first Initial SCID
        // as its Destination Connection ID.
        if client && space == PnSpace::Initial {
            let scid = header.scid(pkt);
            if hs.tls.set_peer_initial_scid(scid).is_err() {
                return Err(fail(DropReason::Handshake(0x0a)));
            }
            if !hs.peer_scid_known {
                hs.peer_scid_known = true;
                if let (Ok(mut st), Ok(cid)) =
                    (self.table.get(slot), ConnectionId::from_slice(scid))
                {
                    st.peer_cid = cid;
                    let _ = self.table.set(slot, &st);
                }
            }
        }
        let conn = self.conns[idx].as_mut().expect("live");
        let payload = &pkt[opened.header_len..opened.header_len + opened.payload_len];
        let exponent = conn.peer_ack_exponent;
        self.events.clear();
        let events = &mut self.events;
        let mut failure: Option<DropReason> = None;
        let mut peer_close = None;
        let mut handshake_acked = false;
        let Conn {
            recovery, streams, ..
        } = conn;
        let info = scan_payload(payload, header.packet_type, |f| {
            if failure.is_some() {
                return;
            }
            match f {
                Frame::Ack(ack) => {
                    let delay = match recovery::ack_delay_ns(ack.ack_delay, exponent) {
                        Ok(d) => d,
                        Err(e) => {
                            failure = Some(DropReason::Recovery(e));
                            return;
                        }
                    };
                    match recovery.on_ack_received(space, ack, delay, now_ns, &mut |ev| {
                        record_long_event(events, ev)
                    }) {
                        Ok(_) => handshake_acked |= space == PnSpace::Handshake,
                        Err(e) => failure = Some(DropReason::Recovery(e)),
                    }
                }
                Frame::Crypto { offset, data } => {
                    if let Err(e) = streams.on_crypto_frame(space, *offset, data) {
                        failure = Some(DropReason::Transport(e));
                    }
                }
                Frame::ConnectionClose(c) => {
                    peer_close = Some(ConnectionError::Peer {
                        error_code: c.error_code,
                        frame_type: c.frame_type,
                    });
                }
                _ => {}
            }
        })
        .map_err(|e| fail(DropReason::Frame(e)))?;
        // Resolve what the ACKs settled before any error return.
        self.resolve_long_events(slot, space);
        if let Some(reason) = failure {
            return Err(fail(reason));
        }
        let conn = self.conns[idx].as_mut().expect("live");
        let hs = conn.hs.as_mut().expect("checked");
        let ls = hs.long(space).expect("long-header space");
        ls.received
            .on_packet_received(pn, now_ns, info.ack_eliciting, Ecn::NotEct)
            .map_err(|e| fail(DropReason::Duplicate(e)))?;
        if handshake_acked && client {
            hs.handshake_acked = true;
        }
        if !client && space == PnSpace::Handshake && !hs.handshake_received {
            // RFC 9000 §8.1: a Handshake packet validates the address;
            // RFC 9001 §4.9.1: the server discards Initial keys.
            hs.handshake_received = true;
            if let Some(amp) = hs.amp.as_mut() {
                amp.validated = true;
            }
            let _ = hs.tls.on_handshake_packet_received();
        }
        if let Some(close) = peer_close {
            conn.error = Some(close);
        }
        if self.apply_tls_events(slot, now_ns).is_err() {
            return Err(fail(DropReason::Closed));
        }
        Ok((slot, pn))
    }

    /// Acknowledgements and losses in `self.events` for a long-header
    /// space reach its CRYPTO send buffer.
    fn resolve_long_events(&mut self, slot: SlotHandle, space: PnSpace) {
        let Some(Some(c)) = self.conns.get_mut(slot.index() as usize) else {
            return;
        };
        let Some(hs) = c.hs.as_mut() else {
            return;
        };
        let Some(ls) = hs.long(space) else {
            return;
        };
        for ev in &self.events {
            let (pn, acked) = match ev {
                CongestionEvent::Acked { packet } => (packet.pn, true),
                CongestionEvent::Lost { packet } => (packet.pn, false),
                _ => continue,
            };
            if acked {
                self.stats.packets_acked += 1;
            } else {
                self.stats.packets_lost += 1;
            }
            if let Some((off, len)) = ls.take(pn) {
                let crypto = c.streams.crypto_mut(space);
                if acked {
                    crypto.on_ack(off, len as usize);
                } else {
                    crypto.on_lost(off, len as usize);
                }
            }
        }
    }

    /// After a 1-RTT packet of a handshake connection: HANDSHAKE_DONE,
    /// 1-RTT acknowledgement confirmation, and 1-RTT CRYPTO data (session
    /// tickets) for TLS. On failure the connection is closed and the error
    /// code returned.
    fn after_one_rtt(&mut self, slot: SlotHandle, acked: bool, now_ns: u64) -> Result<(), u64> {
        let Some(Some(c)) = self.conns.get_mut(slot.index() as usize) else {
            return Ok(());
        };
        let Some(hs) = c.hs.as_mut() else {
            return Ok(());
        };
        let mut result = Ok(());
        if hs.done_received {
            hs.done_received = false;
            result = hs.tls.on_handshake_done_received();
        }
        if result.is_ok() && acked && !hs.is_server() {
            hs.tls.on_one_rtt_ack_received();
        }
        if let Err(e) = result {
            return Err(self.fail_handshake(slot, e.quic_error_code(), now_ns));
        }
        self.drive_tls(slot, now_ns)
    }

    /// Hands newly in-order CRYPTO bytes of every space to TLS, then
    /// applies TLS's events.
    fn drive_tls(&mut self, slot: SlotHandle, now_ns: u64) -> Result<(), u64> {
        let Some(Some(c)) = self.conns.get_mut(slot.index() as usize) else {
            return Ok(());
        };
        let Some(hs) = c.hs.as_mut() else {
            return Ok(());
        };
        let mut failed = None;
        'spaces: for space in Space::ALL {
            if hs.tls.key_state(space) != vesper_warp_tls::KeyState::Installed {
                continue;
            }
            loop {
                let n = c
                    .streams
                    .crypto_mut(pn_space(space))
                    .read(&mut self.tls_buf);
                if n == 0 {
                    break;
                }
                let off = hs.rx_offset[space.index()];
                hs.rx_offset[space.index()] += n as u64;
                if let Err(e) = hs.tls.on_crypto_frame(space, off, &self.tls_buf[..n]) {
                    failed = Some(e.quic_error_code());
                    break 'spaces;
                }
            }
        }
        if let Some(code) = failed {
            return Err(self.fail_handshake(slot, code, now_ns));
        }
        self.apply_tls_events(slot, now_ns)
    }

    /// Applies the TLS driver's events: keys per space, CRYPTO output,
    /// transport parameters, completion, HANDSHAKE_DONE, confirmation and
    /// key discard.
    fn apply_tls_events(&mut self, slot: SlotHandle, now_ns: u64) -> Result<(), u64> {
        let idx = slot.index() as usize;
        loop {
            let Some(Some(c)) = self.conns.get_mut(idx) else {
                return Ok(());
            };
            let Some(hs) = c.hs.as_mut() else {
                return Ok(());
            };
            let Some(ev) = hs.tls.poll_event() else {
                break;
            };
            match ev {
                TlsEvent::KeysReady(k) => match k.space() {
                    Space::Data => {
                        let retire = c.recovery.rtt().pto_base_ns().saturating_mul(3);
                        match k.into_one_rtt(retire) {
                            Ok(keys) => c.keys = Some(keys),
                            Err(e) => {
                                return Err(self.fail_handshake(slot, e.quic_error_code(), now_ns));
                            }
                        }
                    }
                    sp => match k.into_sealer_opener() {
                        Ok((sealer, opener)) => {
                            let ls = hs.long(pn_space(sp)).expect("long space");
                            ls.sealer = Some(sealer);
                            ls.opener = Some(opener);
                        }
                        Err(e) => {
                            return Err(self.fail_handshake(slot, e.quic_error_code(), now_ns));
                        }
                    },
                },
                TlsEvent::Crypto {
                    space,
                    offset,
                    data,
                } => {
                    let i = space.index();
                    debug_assert_eq!(offset, hs.tx_offset[i]);
                    hs.tx_offset[i] += data.len() as u64;
                    hs.pending[i].extend(data.iter());
                }
                TlsEvent::PeerTransportParameters(p) => {
                    // RFC 9000 §18.2: TRANSPORT_PARAMETER_ERROR.
                    if c.streams.set_peer_params(peer_params(&p)).is_err() {
                        return Err(self.fail_handshake(slot, 0x08, now_ns));
                    }
                    // Values above 20 are rejected by lane 12's decoder.
                    c.peer_ack_exponent = u8::try_from(p.ack_delay_exponent).unwrap_or(20);
                    hs.peer_params_applied = true;
                }
                TlsEvent::HandshakeComplete => {
                    self.push_conn_event(ConnectionEvent::HandshakeComplete(slot));
                }
                TlsEvent::SendHandshakeDone => c.streams.queue_handshake_done(),
                TlsEvent::HandshakeConfirmed => {
                    self.push_conn_event(ConnectionEvent::HandshakeConfirmed(slot));
                }
                TlsEvent::DiscardKeys(space) => {
                    let pns = pn_space(space);
                    if let Some(ls) = hs.long(pns) {
                        ls.sealer = None;
                        ls.opener = None;
                        ls.sent.clear();
                        ls.discarded = true;
                    }
                    if pns != PnSpace::ApplicationData {
                        c.recovery.discard_space(pns, now_ns);
                    }
                    if space == Space::Initial {
                        self.initial_dcids.retain(|(_, h)| *h != slot);
                    }
                }
            }
        }
        self.move_pending(slot);
        self.sync_handshake_status(slot, now_ns);
        Ok(())
    }

    /// Moves TLS output into the CRYPTO send buffers as room allows.
    fn move_pending(&mut self, slot: SlotHandle) {
        let Some(Some(c)) = self.conns.get_mut(slot.index() as usize) else {
            return;
        };
        let Some(hs) = c.hs.as_mut() else {
            return;
        };
        for space in Space::ALL {
            let pending = &mut hs.pending[space.index()];
            while !pending.is_empty() {
                let (a, _) = pending.as_slices();
                let want = a.len();
                let n = c.streams.crypto_mut(pn_space(space)).write(a);
                pending.drain(..n);
                if n < want {
                    break;
                }
            }
        }
    }

    /// Keeps recovery's view of the handshake current (RFC 9002 A.2).
    fn sync_handshake_status(&mut self, slot: SlotHandle, now_ns: u64) {
        let max = u64::from(self.config.max_datagram);
        let Some(Some(c)) = self.conns.get_mut(slot.index() as usize) else {
            return;
        };
        let Some(hs) = c.hs.as_ref() else {
            return;
        };
        let confirmed = hs.tls.is_handshake_confirmed();
        let status = HandshakeStatus {
            confirmed,
            peer_completed_address_validation: hs.is_server() || confirmed || hs.handshake_acked,
            has_handshake_keys: hs.spaces[1].sealer.is_some() || hs.spaces[1].discarded,
            at_amplification_limit: hs.amp.is_some_and(|a| !a.allows(max)),
        };
        c.recovery.set_handshake_status(status, now_ns);
        self.sync_timer(slot);
    }

    /// The TLS handshake failed: close with its code in every space we
    /// still have keys for.
    fn fail_handshake(&mut self, slot: SlotHandle, code: u64, now_ns: u64) -> u64 {
        self.stats.handshake_failures += 1;
        if let Ok(c) = self.conn_mut(slot) {
            if c.error.is_some() {
                return code;
            }
            c.error = Some(ConnectionError::Handshake { error_code: code });
        }
        let _ = self.send_close_code(slot, code, CLOSE_FRAME_CRYPTO, now_ns);
        code
    }

    /// Sends owed Initial/Handshake ACKs and pending CRYPTO data.
    fn flush_handshake(&mut self, slot: SlotHandle, now_ns: u64) -> Result<u32, WarpError> {
        let mut sent = 0;
        if self.conn(slot)?.error.is_some() || self.conn(slot)?.hs.is_none() {
            return Ok(sent);
        }
        self.move_pending(slot);
        for space in [PnSpace::Initial, PnSpace::Handshake] {
            for _ in 0..self.config.max_tracked_per_conn {
                let c = self.conns[slot.index() as usize].as_mut().expect("live");
                if c.error.is_some() {
                    return Ok(sent);
                }
                let has_data = c.streams.crypto(space).has_data();
                let Some(ls) = c.hs.as_mut().and_then(|h| h.long(space)) else {
                    break;
                };
                if ls.sealer.is_none() {
                    break;
                }
                let ack = ls.received.ack_deadline().is_some();
                if !ack && !has_data {
                    break;
                }
                match self.send_long(slot, space, LongKind::Normal, now_ns) {
                    Ok(_) => sent += 1,
                    Err(WarpError::AmplificationLimited) => return Ok(sent),
                    Err(WarpError::Umem(_)) => return Ok(sent),
                    Err(e) => return Err(e),
                }
                self.move_pending(slot);
            }
        }
        Ok(sent)
    }

    /// An RFC 9002 PTO probe in a long-header space: every unacknowledged
    /// CRYPTO range is queued again, and the probe carries it (or a PING).
    fn send_long_probe(
        &mut self,
        slot: SlotHandle,
        space: PnSpace,
        now_ns: u64,
    ) -> Result<(), WarpError> {
        let c = self.conn_mut(slot)?;
        let Conn { hs, streams, .. } = c;
        let ls = hs
            .as_mut()
            .and_then(|h| h.long(space))
            .ok_or(WarpError::KeysUnavailable)?;
        for &(_, off, len) in &ls.sent {
            streams.crypto_mut(space).on_lost(off, len as usize);
        }
        self.send_long(slot, space, LongKind::Probe, now_ns)?;
        self.stats.probes_sent += 1;
        Ok(())
    }

    /// Builds, seals and sends one Initial or Handshake packet in its own
    /// datagram: an owed ACK, then CRYPTO data (lost ranges first), then
    /// PING for a probe or CONNECTION_CLOSE for a close; ack-eliciting
    /// Initials (all client Initials) are padded to 1200 bytes. Returns the
    /// packet number.
    fn send_long(
        &mut self,
        slot: SlotHandle,
        space: PnSpace,
        kind: LongKind,
        now_ns: u64,
    ) -> Result<u64, WarpError> {
        let max = usize::from(self.config.max_datagram);
        let exponent = self.config.ack_delay_exponent;
        let tile = self
            .table
            .get(slot)
            .map_err(|_| WarpError::StaleConnection)?;
        let idx = slot.index() as usize;
        let c = self.conns[idx].as_mut().ok_or(WarpError::StaleConnection)?;
        let largest_acked = c.recovery.largest_acked(space);
        let Conn { hs, streams, .. } = c;
        let hs = hs.as_mut().ok_or(WarpError::KeysUnavailable)?;
        if let Some(amp) = hs.amp
            && !amp.allows(max as u64)
        {
            self.stats.amplification_blocked += 1;
            return Err(WarpError::AmplificationLimited);
        }
        let client = !hs.is_server();
        let mut scid = [0u8; 20];
        let scid_len = hs.local_cid().len();
        scid[..scid_len].copy_from_slice(hs.local_cid());
        let ls = hs.long(space).ok_or(WarpError::KeysUnavailable)?;
        let Some(sealer) = ls.sealer.as_ref() else {
            return Err(WarpError::KeysUnavailable);
        };
        let pn = sealer.next_packet_number();

        // Frames after a maximal header, moved once its length is known.
        let scratch = &mut self.scratch;
        let base = MAX_LONG_HEADER;
        let limit = max.saturating_sub(TAG_LEN).min(scratch.len());
        let mut at = base;
        let mut has_ack = false;
        let mut crypto = None;
        let ack_eliciting;
        match kind {
            LongKind::Close { code, frame_type } => {
                at += Frame::ConnectionClose(crate::frame::ConnectionClose {
                    error_code: code,
                    frame_type: Some(frame_type),
                    reason: b"",
                })
                .encode(&mut scratch[at..limit])?;
                ack_eliciting = false;
            }
            LongKind::Normal | LongKind::Probe => {
                if let Some(ack) = ls
                    .received
                    .ack_frame(now_ns, exponent)
                    .map_err(|_| WarpError::Config("bad ack_delay_exponent"))?
                {
                    at += Frame::Ack(ack).encode(&mut scratch[at..limit])?;
                    has_ack = true;
                }
                // Room for a PING after the CRYPTO frame.
                let room = limit.saturating_sub(1);
                if room > at
                    && let Some((n, off, len)) = streams
                        .crypto_mut(space)
                        .write_frame(&mut scratch[at..room])
                {
                    at += n;
                    crypto = Some((off, len as u32));
                }
                if kind == LongKind::Probe && crypto.is_none() {
                    at += Frame::Ping.encode(&mut scratch[at..limit])?;
                }
                ack_eliciting = crypto.is_some() || kind == LongKind::Probe;
            }
        }
        let frames_len = at - base;
        if frames_len == 0 {
            return Err(WarpError::Config("empty packet"));
        }
        let ty = if space == PnSpace::Initial {
            LongPacketType::Initial
        } else {
            LongPacketType::Handshake
        };
        let shortest = encoded_len(pn, largest_acked)
            .map_err(|_| WarpError::PacketNumbersExhausted)?
            .bytes();
        let pad_initial = space == PnSpace::Initial && (client || ack_eliciting);
        let dcid = tile.peer_cid;
        let header = |payload_len: usize, pn_bytes: usize, out: &mut [u8]| {
            write_long(
                out,
                &LongHeader {
                    ty,
                    dcid: dcid.as_bytes(),
                    scid: &scid[..scid_len],
                    token: &[],
                    full_pn: pn,
                    pn_len: PnLen::new(pn_bytes as u8).expect("1..=4 bytes"),
                    payload_len,
                },
            )
        };
        // The header-protection sample needs pn_len + payload >= 4.
        let mut pn_bytes = shortest.max(4usize.saturating_sub(frames_len));
        let mut pad = 0;
        if pad_initial {
            pn_bytes = shortest;
            let probe = header(MIN_INITIAL_DATAGRAM, pn_bytes, &mut scratch[..base])?;
            pad = MIN_INITIAL_DATAGRAM.saturating_sub(probe.header_len + frames_len + TAG_LEN);
        }
        if at + pad > scratch.len() {
            return Err(WarpError::PacketTooLarge { len: at + pad });
        }
        scratch[at..at + pad].fill(0); // PADDING frames
        let payload = frames_len + pad;
        let written = header(payload + TAG_LEN, pn_bytes, &mut scratch[..base])?;
        scratch.copy_within(base..base + payload, written.header_len);
        let plain_len = written.header_len + payload;
        let total = plain_len + TAG_LEN;
        if total > max {
            return Err(WarpError::PacketTooLarge { len: total });
        }

        let requeue = |streams: &mut StreamSet| {
            if let Some((off, len)) = crypto {
                streams.crypto_mut(space).on_lost(off, len as usize);
            }
        };
        let mut frame = match self.xsk.alloc_tx(total as u16) {
            Ok(f) => f,
            Err(e) => {
                requeue(streams);
                return Err(WarpError::Umem(e));
            }
        };
        let sealer = ls.sealer.as_mut().expect("checked");
        let sealed = match sealer.ticket(pn) {
            Err(e) => Err(WarpError::Crypto(e)),
            Ok(ticket) => match self.xsk.tx_bytes_mut(&mut frame) {
                Err(e) => {
                    let _ = sealer.abandon(ticket);
                    Err(WarpError::Umem(e))
                }
                Ok(bytes) => {
                    bytes[..plain_len].copy_from_slice(&scratch[..plain_len]);
                    match sealer.seal(ticket, bytes, written.pn_offset, payload) {
                        Ok(_) => Ok(()),
                        Err(err) => {
                            let _ = sealer.abandon(err.ticket);
                            Err(WarpError::Crypto(err.error))
                        }
                    }
                }
            },
        };
        if let Err(e) = sealed {
            requeue(streams);
            let _ = self.xsk.abandon_tx(frame);
            return Err(e);
        }
        let in_flight = !matches!(kind, LongKind::Close { .. }) && (ack_eliciting || pad > 0);
        let sent = recovery::SentPacket {
            pn,
            sent_ns: now_ns.max(self.conns[idx].as_ref().expect("live").last_sent_ns),
            bytes: total as u32,
            ack_eliciting,
            in_flight,
        };
        let c = self.conns[idx].as_mut().expect("live");
        if let Err(e) = c.recovery.on_packet_sent(space, sent) {
            if let Some((off, len)) = crypto {
                c.streams.crypto_mut(space).on_lost(off, len as usize);
            }
            let _ = self.xsk.abandon_tx(frame);
            return Err(WarpError::Recovery(e));
        }
        c.last_sent_ns = sent.sent_ns;
        let hs = c.hs.as_mut().expect("checked");
        if let Some(amp) = hs.amp.as_mut() {
            amp.sent += total as u64;
        }
        let ls = hs.long(space).expect("long space");
        if has_ack {
            ls.received.on_ack_sent();
        }
        if let Some((off, len)) = crypto {
            ls.sent.push_back((pn, off, len));
        }
        // RFC 9001 §4.9.1: a client discards Initial keys once it sends
        // its first Handshake packet.
        let discard_initial = client && space == PnSpace::Handshake && !hs.handshake_sent;
        if discard_initial {
            hs.handshake_sent = true;
            let _ = hs.tls.on_handshake_packet_sent();
        }
        self.stats.packets_sent += 1;
        self.stats.handshake_packets_sent += 1;
        if let Ok(bytes) = self.xsk.tx_bytes_mut(&mut frame) {
            self.tap.on_transmit(slot, pn, now_ns, bytes);
        }
        // Not leased: lost CRYPTO data is resent from the CRYPTO buffer.
        match self.xsk.submit_paced_tx(frame, pn, now_ns) {
            Ok(pkt) => {
                let _ = self.xsk.retire(pkt);
            }
            Err(refused) => {
                self.stats.tx_abandoned += 1;
                let _ = self.xsk.abandon_tx(refused.into_owner());
            }
        }
        if discard_initial {
            let _ = self.apply_tls_events(slot, now_ns);
        }
        self.sync_handshake_status(slot, now_ns);
        Ok(pn)
    }

    /// CONNECTION_CLOSE with `code` in every space we hold send keys for
    /// (Initial, Handshake, 1-RTT): the peer may not have the later ones
    /// yet (RFC 9000 §10.2.3).
    fn send_close_code(
        &mut self,
        slot: SlotHandle,
        code: u64,
        frame_type: u64,
        now_ns: u64,
    ) -> Result<(), WarpError> {
        let mut any = false;
        for space in [PnSpace::Initial, PnSpace::Handshake] {
            let has = self
                .conns
                .get_mut(slot.index() as usize)
                .and_then(Option::as_mut)
                .and_then(|c| c.hs.as_mut())
                .and_then(|h| h.long(space))
                .is_some_and(|ls| ls.sealer.is_some());
            if has
                && self
                    .send_long(slot, space, LongKind::Close { code, frame_type }, now_ns)
                    .is_ok()
            {
                any = true;
            }
        }
        if self.conn(slot)?.keys.is_some() {
            let n = Frame::ConnectionClose(crate::frame::ConnectionClose {
                error_code: code,
                frame_type: Some(frame_type),
                reason: b"",
            })
            .encode(&mut self.app_out)?;
            if self
                .send(slot, Source::Fresh, false, n, Kind::Close, now_ns)
                .is_ok()
            {
                any = true;
            }
        }
        if any {
            self.stats.closes_sent += 1;
            self.conn_mut(slot)?.close_sent_ns = Some(now_ns);
        }
        Ok(())
    }

    /// Closes a connection and frees its slot. Its parked leases are
    /// retired and its queued launches abandoned. This purges the shared
    /// lease table and allocates; it is not a data-path operation.
    pub fn close_connection(&mut self, conn: SlotHandle, now_ns: u64) -> Result<(), WarpError> {
        self.conn(conn)?;
        let mut parked = Vec::with_capacity(self.leases.len());
        self.leases
            .discard_space(PacketNumberSpace::ApplicationData, |key, lease| {
                parked.push((key, lease));
            });
        for (key, lease) in parked {
            if lease.conn == conn {
                let _ = self.xsk.retire(lease.pkt);
            } else {
                // Other connections' leases come back due now; the next poll
                // re-parks them at their recovery deadline.
                self.park_key(key.pn, now_ns, lease);
            }
        }
        self.conns[conn.index() as usize] = None;
        self.initial_dcids.retain(|(_, h)| *h != conn);
        self.table
            .remove(conn)
            .map_err(|_| WarpError::StaleConnection)?;
        Ok(())
    }

    /// A connection's tile and state.
    pub fn connection(&self, conn: SlotHandle) -> Option<ConnectionView<'_>> {
        let c = self.conns.get(conn.index() as usize)?.as_ref()?;
        if c.handle != conn {
            return None;
        }
        Some(ConnectionView {
            tile: self.table.get(conn).ok()?,
            recovery: &c.recovery,
            cc: &c.cc,
            received: &c.received,
            keys: c.keys.as_ref(),
            streams: &c.streams,
            error: c.error,
            phase: c
                .hs
                .as_ref()
                .map_or(HandshakePhase::External, |h| h.phase()),
            tls: c.hs.as_ref().map(|h| &h.tls),
        })
    }

    fn conn_mut(&mut self, h: SlotHandle) -> Result<&mut Conn, WarpError> {
        match self.conns.get_mut(h.index() as usize) {
            Some(Some(c)) if c.handle == h => Ok(c),
            _ => Err(WarpError::StaleConnection),
        }
    }

    /// The connection's stream layer, for a reliable-mode connection that
    /// is not closed.
    pub fn streams_mut(&mut self, conn: SlotHandle) -> Result<&mut StreamSet, WarpError> {
        if self.config.stream_mode != StreamMode::Reliable {
            return Err(WarpError::StreamMode);
        }
        let c = self.conn_mut(conn)?;
        if let Some(e) = c.error {
            return Err(WarpError::ConnectionClosed(e));
        }
        if c.hs.as_ref().is_some_and(|h| !h.peer_params_applied) {
            return Err(WarpError::Handshaking);
        }
        Ok(&mut c.streams)
    }

    /// Opens a stream of ours; returns its ID (RFC 9000 §2.1).
    pub fn open_stream(&mut self, conn: SlotHandle, uni: bool) -> Result<u64, WarpError> {
        Ok(self.streams_mut(conn)?.open(uni)?)
    }

    /// Buffers stream data; returns the bytes accepted (up to the send
    /// buffer's free space). [`WarpEndpoint::poll`] sends it.
    pub fn stream_write(
        &mut self,
        conn: SlotHandle,
        stream_id: u64,
        data: &[u8],
    ) -> Result<usize, WarpError> {
        Ok(self.streams_mut(conn)?.write(stream_id, data)?)
    }

    /// Ends a stream's sending part (FIN after the data written).
    pub fn stream_finish(&mut self, conn: SlotHandle, stream_id: u64) -> Result<(), WarpError> {
        Ok(self.streams_mut(conn)?.finish(stream_id)?)
    }

    /// Reads in-order stream data.
    pub fn stream_read(
        &mut self,
        conn: SlotHandle,
        stream_id: u64,
        out: &mut [u8],
    ) -> Result<ReadOutcome, WarpError> {
        Ok(self.streams_mut(conn)?.read(stream_id, out)?)
    }

    /// Abandons a stream's sending part (RESET_STREAM).
    pub fn stream_reset(
        &mut self,
        conn: SlotHandle,
        stream_id: u64,
        error_code: u64,
    ) -> Result<(), WarpError> {
        Ok(self.streams_mut(conn)?.reset(stream_id, error_code)?)
    }

    /// Asks the peer to stop sending on a stream (STOP_SENDING).
    pub fn stream_stop_sending(
        &mut self,
        conn: SlotHandle,
        stream_id: u64,
        error_code: u64,
    ) -> Result<(), WarpError> {
        Ok(self
            .streams_mut(conn)?
            .stop_sending(stream_id, error_code)?)
    }

    /// The next stream event of a connection.
    pub fn poll_stream_event(&mut self, conn: SlotHandle) -> Option<StreamEvent> {
        self.conn_mut(conn).ok()?.streams.poll_event()
    }

    /// Why a connection was closed, if it was.
    pub fn connection_error(&self, conn: SlotHandle) -> Option<ConnectionError> {
        self.conn(conn).ok()?.error
    }

    fn conn(&self, h: SlotHandle) -> Result<&Conn, WarpError> {
        match self.conns.get(h.index() as usize) {
            Some(Some(c)) if c.handle == h => Ok(c),
            _ => Err(WarpError::StaleConnection),
        }
    }

    /// Writes the connection's receive, recovery and BBR scalars back to
    /// its tile (lane 6's hot fields).
    fn write_tile(&mut self, h: SlotHandle) {
        let Some(Some(c)) = self.conns.get(h.index() as usize) else {
            return;
        };
        let Ok(mut st) = self.table.get(h) else {
            return;
        };
        let bbr = c.cc.controller();
        st.largest_rx_pn = c.received.largest();
        st.bbr_bw_bytes_per_sec = bbr.max_bw();
        st.bbr_min_rtt_ns = bbr.min_rtt_ns().unwrap_or(u64::MAX);
        st.cwnd_bytes = bbr.cwnd_bytes();
        st.bytes_in_flight = u32::try_from(c.recovery.bytes_in_flight()).unwrap_or(u32::MAX);
        st.next_edt_ns = bbr.next_edt_ns();
        // Every field came from a valid state and the handle is live.
        let _ = self.table.set(h, &st);
        self.sync_timer(h);
    }

    /// Re-arms the connection's loss-detection timer in the timer wheel
    /// whenever recovery moved it (an ACK, a send or a timeout).
    fn sync_timer(&mut self, h: SlotHandle) {
        let Some(Some(c)) = self.conns.get_mut(h.index() as usize) else {
            return;
        };
        let want = c.recovery.timer().map(|t| t.deadline_ns);
        if c.timer.as_ref().map(|(_, d)| *d) == want {
            return;
        }
        if let Some((handle, _)) = c.timer.take() {
            let _ = self.timers.cancel(handle);
        }
        if let Some(deadline) = want {
            // Capacity is one entry per connection slot.
            if let Ok(handle) = self.timers.insert(deadline, h) {
                c.timer = Some((handle, deadline));
            }
        }
    }

    fn lookup(&self, dcid: &[u8]) -> Option<SlotHandle> {
        let found = match self.config.cid_lookup {
            CidLookup::Index => self.table.lookup(dcid),
            CidLookup::TileScan => self.table.lookup_cid_simd(dcid),
        };
        found.ok().flatten()
    }

    /// Returns a received frame to the fill ring and reports the drop.
    fn drop_rx(
        &mut self,
        frame: UmemFrame<RxHarvested>,
        reason: DropReason,
    ) -> Result<RxOutcome, WarpError> {
        self.stats.rx_dropped += 1;
        match reason {
            DropReason::Crypto(_) => self.stats.rx_dropped_crypto += 1,
            DropReason::UnknownCid => self.stats.rx_dropped_unknown_cid += 1,
            _ => {}
        }
        match self.xsk.recycle_to_fill_ring(frame) {
            Ok(()) => Ok(RxOutcome::Dropped(reason)),
            // The refused owner is dropped here; the driver counts and
            // quarantines it as a leak instead of recycling it.
            Err(refused) => Err(WarpError::Umem(refused.error)),
        }
    }

    /// §35.3 `poll_and_step_quic_rx_one`: harvest one frame and run it
    /// through the whole receive path, replying in place when the packet
    /// was ack-eliciting.
    pub fn poll_and_step_quic_rx_one(&mut self, now_ns: u64) -> Result<RxOutcome, WarpError> {
        let mut frame = match self.xsk.harvest_rx() {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(RxOutcome::Idle),
            Err(rejected) => {
                self.stats.rx_rejected += 1;
                return Ok(RxOutcome::Rejected(rejected.reason));
            }
        };

        // 1. Header split (lane 1), up to the protected PN boundary.
        let split = match self.xsk.view_frame_bytes(&frame) {
            Err(e) => Err(DropReason::Umem(e)),
            Ok(bytes) => match self.parser.parse(bytes) {
                Ok(ParsedPacket::Protected(h)) if h.packet_type == PacketType::OneRtt => {
                    let mut dcid = [0u8; 20];
                    let d = h.dcid(bytes);
                    dcid[..d.len()].copy_from_slice(d);
                    Ok(Some((h, dcid, d.len())))
                }
                Ok(ParsedPacket::Protected(h))
                    if matches!(h.packet_type, PacketType::Initial | PacketType::Handshake) =>
                {
                    Ok(None)
                }
                Ok(_) => Err(DropReason::NotOneRtt),
                Err(e) => Err(DropReason::Header(e)),
            },
        };
        let (header, dcid, dcid_len) = match split {
            Ok(Some(v)) => v,
            Ok(None) => return self.on_long_datagram(frame, now_ns),
            Err(reason) => return self.drop_rx(frame, reason),
        };

        // 2. CID lookup in the shard's tiles (lane 6).
        let Some(slot) = self.lookup(&dcid[..dcid_len]) else {
            return self.drop_rx(frame, DropReason::UnknownCid);
        };
        let (Ok(tile), Ok(_)) = (self.table.get(slot), self.conn(slot)) else {
            return self.drop_rx(frame, DropReason::UnknownCid);
        };
        if let Some(error) = self.conn(slot)?.error {
            // Closing (RFC 9000 §10.2.1): answer with CONNECTION_CLOSE again,
            // at most once per PTO; draining: stay silent.
            self.stats.rx_dropped_closed += 1;
            let out = self.drop_rx(frame, DropReason::Closed);
            if let ConnectionError::Local(e) = error {
                let c = self.conn(slot)?;
                let gap = c.recovery.rtt().pto_base_ns();
                if c.close_sent_ns
                    .is_none_or(|t| now_ns >= t.saturating_add(gap))
                {
                    let _ = self.send_close(slot, e, now_ns);
                }
            }
            return out;
        }

        // 3-5. Header-protection removal (lane 5), full PN decode from the
        // tile's largest received PN (lane 1), AEAD open with the full-PN
        // nonce (lane 5).
        let conn = self.conns[slot.index() as usize]
            .as_mut()
            .expect("checked above");
        let opened = open_in_place(
            &mut self.xsk,
            conn,
            &mut frame,
            &header,
            tile.largest_rx_pn,
            now_ns,
        );
        let (opened_header_len, payload_len, pn) = match opened {
            Ok(v) => v,
            Err(reason) => return self.drop_rx(frame, reason),
        };

        // 6. Frames (lane 1): ACKs drive RFC 9002 recovery, whose events
        // pass through the BBRv3 adapter (lane 2); STREAM frames go to the
        // application, whose reply frames collect in `app_out`.
        self.events.clear();
        let conn = self.conns[slot.index() as usize]
            .as_mut()
            .expect("checked above");
        let scanned = scan_in_place(
            &mut self.xsk,
            conn,
            &mut self.events,
            &mut self.app,
            &mut self.app_out,
            &mut frame,
            slot,
            opened_header_len..opened_header_len + payload_len,
            pn,
            now_ns,
            self.config.stream_mode,
        );
        // Retire the leases recovery resolved (lanes 3, 4).
        let acked_any = self
            .events
            .iter()
            .any(|e| matches!(e, CongestionEvent::Acked { .. }));
        self.resolve_events(slot, PnSpace::ApplicationData, None);
        let (ack_eliciting, app_len) = match scanned {
            Ok(v) => v,
            Err(DropReason::Transport(e)) => {
                // A peer violation: close with its error code.
                self.write_tile(slot);
                self.stats.transport_errors += 1;
                self.conn_mut(slot)?.error = Some(ConnectionError::Local(e));
                let out = self.drop_rx(frame, DropReason::Transport(e));
                let _ = self.send_close(slot, e, now_ns);
                return out;
            }
            Err(reason) => {
                self.write_tile(slot);
                return self.drop_rx(frame, reason);
            }
        };
        if self.conn(slot)?.hs.is_some()
            && self.conn(slot)?.error.is_none()
            && let Err(code) = self.after_one_rtt(slot, acked_any, now_ns)
        {
            self.write_tile(slot);
            return self.drop_rx(frame, DropReason::Handshake(code));
        }
        if self.conn(slot)?.error.is_some() {
            // The peer closed the connection (draining): no reply.
            self.stats.closes_received += 1;
            self.stats.rx_processed += 1;
            self.write_tile(slot);
            return match self.xsk.recycle_to_fill_ring(frame) {
                Ok(()) => Ok(RxOutcome::Processed {
                    conn: slot,
                    pn,
                    replied: false,
                }),
                Err(refused) => Err(WarpError::Umem(refused.error)),
            };
        }
        self.stats.rx_processed += 1;
        if let Ok(mut st) = self.table.get(slot) {
            st.idle_deadline_ns = now_ns.saturating_add(self.config.idle_timeout_ns);
            let _ = self.table.set(slot, &st);
        }
        self.write_tile(slot);

        if !ack_eliciting {
            return match self.xsk.recycle_to_fill_ring(frame) {
                Ok(()) => Ok(RxOutcome::Processed {
                    conn: slot,
                    pn,
                    replied: false,
                }),
                Err(refused) => Err(WarpError::Umem(refused.error)),
            };
        }

        // 7. Reply in the received frame: ACK plus the application's frames,
        // sealed, EDT-scheduled, submitted and parked (lanes 2-5).
        let mut data_len = app_len;
        if data_len > 0 && !self.conn(slot)?.cc.can_send() {
            self.stats.cwnd_limited_replies += 1;
            data_len = 0;
        }
        if data_len > 0 && !self.launch_has_room(slot, now_ns) {
            self.stats.backpressured_replies += 1;
            data_len = 0;
        }
        // Reliable streams: the reply also carries pending stream frames.
        let mut from_streams = false;
        if self.config.stream_mode == StreamMode::Reliable
            && self.conn(slot)?.cc.can_send()
            && self.launch_has_room(slot, now_ns)
            && self.conns[slot.index() as usize]
                .as_mut()
                .is_some_and(|c| c.streams.has_pending())
        {
            let budget = self.frame_budget(slot, true, now_ns);
            let c = self.conns[slot.index() as usize].as_mut().expect("live");
            data_len = c
                .streams
                .fill(&mut self.app_out[..budget], &mut self.record);
            from_streams = data_len > 0;
        }
        // An ACK-only reply waits behind the connection's queued paced
        // packets (see `stage`). With the launch queue full it cannot, and
        // sending it now would overtake them, so the ACK is deferred.
        if data_len == 0 && self.ack_must_wait(slot, now_ns) {
            self.defer_ack(slot);
            return match self.xsk.recycle_to_fill_ring(frame) {
                Ok(()) => Ok(RxOutcome::Processed {
                    conn: slot,
                    pn,
                    replied: false,
                }),
                Err(refused) => Err(WarpError::Umem(refused.error)),
            };
        }
        self.stats.replies += 1;
        let sent = self.send_carrying(
            slot,
            Source::TurnAround(frame),
            true,
            data_len,
            Kind::Normal,
            now_ns,
        );
        if from_streams {
            self.settle_record(slot, &sent);
        }
        sent?;
        Ok(RxOutcome::Processed {
            conn: slot,
            pn,
            replied: true,
        })
    }

    /// Sends STREAM data on a connection in a fresh frame, with an owed ACK
    /// piggybacked. Returns the packet number.
    pub fn send_stream(
        &mut self,
        conn: SlotHandle,
        stream_id: u64,
        offset: u64,
        data: &[u8],
        fin: bool,
        now_ns: u64,
    ) -> Result<u64, WarpError> {
        if self.config.stream_mode != StreamMode::Raw {
            return Err(WarpError::StreamMode);
        }
        if let Some(e) = self.conn(conn)?.error {
            return Err(WarpError::ConnectionClosed(e));
        }
        if !self.conn(conn)?.cc.can_send() {
            return Err(WarpError::CwndLimited);
        }
        if !self.launch_has_room(conn, now_ns) {
            return Err(WarpError::LaunchBackpressure);
        }
        let frame = Frame::Stream(StreamFrame {
            stream_id,
            offset,
            data,
            fin,
            has_length: true,
        });
        let len = frame
            .encode(&mut self.app_out)
            .map_err(|_| WarpError::PacketTooLarge {
                len: data.len() + 16,
            })?;
        let ack = self.conn(conn)?.received.ack_deadline().is_some();
        self.send(conn, Source::Fresh, ack, len, Kind::Normal, now_ns)
    }

    /// Room for frames in a fresh packet after the short header, the
    /// connection's owed ACK (when `with_ack`) and the AEAD tag.
    fn frame_budget(&mut self, slot: SlotHandle, with_ack: bool, now_ns: u64) -> usize {
        let exponent = self.config.ack_delay_exponent;
        let peer_cid = self
            .table
            .get(slot)
            .map_or(usize::from(self.config.cid_len), |t| {
                t.peer_cid.as_bytes().len()
            });
        let fixed = 1 + peer_cid + 4 + TAG_LEN;
        let mut ack_len = 0;
        if with_ack
            && let Some(Some(c)) = self.conns.get(slot.index() as usize)
            && let Ok(Some(ack)) = c.received.ack_frame(now_ns, exponent)
        {
            ack_len = Frame::Ack(ack).encode(&mut self.scratch).unwrap_or(0);
        }
        usize::from(self.config.max_datagram)
            .saturating_sub(fixed + ack_len)
            .min(self.app_out.len())
    }

    /// Commits the stream frames in `self.record` to the packet that
    /// carried them, or requeues them when they were not sent.
    fn settle_record(&mut self, slot: SlotHandle, sent: &Result<(u64, bool), WarpError>) {
        let Some(Some(c)) = self.conns.get_mut(slot.index() as usize) else {
            return;
        };
        match sent {
            Ok((pn, true)) => {
                c.streams.commit(*pn, &self.record);
                self.stats.stream_packets_sent += 1;
            }
            _ => c.streams.requeue(&self.record),
        }
        self.record.clear();
    }

    /// Sends the stream layer's pending frames on one connection in fresh
    /// packets while the congestion window, the launch queue and the
    /// transmit frames allow. Returns the packets sent.
    pub fn flush(&mut self, conn: SlotHandle, now_ns: u64) -> Result<u32, WarpError> {
        if self.config.stream_mode != StreamMode::Reliable {
            return Err(WarpError::StreamMode);
        }
        if let Some(e) = self.conn(conn)?.error {
            return Err(WarpError::ConnectionClosed(e));
        }
        let mut sent = 0;
        if self.conn(conn)?.keys.is_none() {
            // Handshaking: 1-RTT data waits for its keys.
            return Ok(sent);
        }
        // Bounded per call; congestion control normally stops it first.
        for _ in 0..self.config.max_tracked_per_conn {
            let c = self.conn(conn)?;
            if !c.cc.can_send() || !self.launch_has_room(conn, now_ns) {
                break;
            }
            if self.xsk.free_list().is_empty() {
                break;
            }
            let ack = c.received.ack_deadline().is_some();
            let c = self.conns[conn.index() as usize].as_mut().expect("live");
            if !c.streams.has_pending() {
                break;
            }
            let budget = self.frame_budget(conn, ack, now_ns);
            let c = self.conns[conn.index() as usize].as_mut().expect("live");
            let len = c
                .streams
                .fill(&mut self.app_out[..budget], &mut self.record);
            if len == 0 {
                self.record.clear();
                break;
            }
            let result = self.send_carrying(conn, Source::Fresh, ack, len, Kind::Normal, now_ns);
            self.settle_record(conn, &result);
            if result.is_err() {
                break;
            }
            sent += 1;
        }
        Ok(sent)
    }

    fn flush_all(&mut self, now_ns: u64) -> Result<(), WarpError> {
        if self.config.stream_mode != StreamMode::Reliable {
            return Ok(());
        }
        for i in 0..self.conns.len() {
            let Some(c) = self.conns[i].as_ref() else {
                continue;
            };
            if c.error.is_some() {
                continue;
            }
            let h = c.handle;
            self.flush(h, now_ns)?;
        }
        Ok(())
    }

    /// Sends pending handshake data and owed Initial/Handshake ACKs on
    /// every connection that is handshaking.
    fn flush_handshakes(&mut self, now_ns: u64) -> Result<(), WarpError> {
        for i in 0..self.conns.len() {
            let Some(c) = self.conns[i].as_ref() else {
                continue;
            };
            if c.error.is_some() || c.hs.is_none() {
                continue;
            }
            let h = c.handle;
            self.flush_handshake(h, now_ns)?;
        }
        Ok(())
    }

    /// Sends CONNECTION_CLOSE for a peer violation (not ack-eliciting, so
    /// never retransmitted; the closing state answers later packets).
    fn send_close(
        &mut self,
        slot: SlotHandle,
        e: TransportError,
        now_ns: u64,
    ) -> Result<(), WarpError> {
        self.send_close_code(slot, e.code.code(), e.frame_type, now_ns)
    }

    /// Releases due launches, harvests completions, replenishes the fill
    /// ring and services expired leases (PTO).
    pub fn poll(&mut self, now_ns: u64) -> Result<PollReport, WarpError> {
        let mut report = PollReport {
            launched: self.release_launches(now_ns),
            ..PollReport::default()
        };
        let done = self.xsk.harvest_completions(self.config.completion_batch);
        self.stats.completions_leased += u64::from(done.completed_leased);
        self.stats.completions_freed += u64::from(done.freed);
        report.completions = done.completed_leased + done.freed + done.leaked;
        report.replenished = self.replenish(self.config.fill_batch);
        loop {
            let fired = match self.leases.poll_pto(now_ns) {
                Ok(Some(fired)) => fired,
                Ok(None) => break,
                Err(_) => return Err(WarpError::Config("clock went backwards")),
            };
            report.ptos += 1;
            self.stats.pto_fired += 1;
            self.on_lease_expired(fired.lease, now_ns)?;
        }
        loop {
            let fired = match self.timers.poll(now_ns) {
                Ok(Some(fired)) => fired,
                Ok(None) => break,
                Err(_) => return Err(WarpError::Config("clock went backwards")),
            };
            let slot = fired.payload;
            match self.conns.get_mut(slot.index() as usize) {
                Some(Some(c)) if c.handle == slot => c.timer = None,
                _ => continue,
            }
            self.stats.recovery_timers_fired += 1;
            self.run_recovery_timeout(slot, now_ns, None)?;
            // Not due after all (the deadline moved): re-arm.
            self.sync_timer(slot);
        }
        self.flush_deferred_acks(now_ns)?;
        self.flush_handshakes(now_ns)?;
        self.flush_all(now_ns)?;
        Ok(report)
    }

    /// Whether an ACK-only packet on `conn` would need the launch queue
    /// and find it full.
    fn ack_must_wait(&self, conn: SlotHandle, now_ns: u64) -> bool {
        let Ok(c) = self.conn(conn) else {
            return false;
        };
        c.last_edt_ns > now_ns && self.launch.len() >= self.config.launch_capacity as usize
    }

    fn defer_ack(&mut self, slot: SlotHandle) {
        self.stats.deferred_acks += 1;
        let room = self.deferred_acks.len() < self.deferred_acks.capacity();
        if let Some(Some(c)) = self.conns.get_mut(slot.index() as usize)
            && !c.ack_deferred
            && room
        {
            c.ack_deferred = true;
            self.deferred_acks.push(slot);
        }
    }

    /// Sends the ACKs deferred by a full launch queue, oldest first, while
    /// there is room.
    fn flush_deferred_acks(&mut self, now_ns: u64) -> Result<(), WarpError> {
        while let Some(&slot) = self.deferred_acks.first() {
            if self.ack_must_wait(slot, now_ns) {
                break;
            }
            self.deferred_acks.remove(0);
            let owed = match self.conns.get_mut(slot.index() as usize) {
                Some(Some(c)) if c.handle == slot => {
                    c.ack_deferred = false;
                    // No ACKs once closing or draining.
                    c.error.is_none() && c.received.ack_deadline().is_some()
                }
                _ => continue,
            };
            // Out of tx frames: keep it owed and try on a later poll.
            if owed
                && self
                    .send(slot, Source::Fresh, true, 0, Kind::Normal, now_ns)
                    .is_err()
            {
                self.defer_ack(slot);
                break;
            }
        }
        Ok(())
    }

    /// Posts up to `max` free frames to the fill ring, keeping
    /// `tx_reserve` free for fresh transmissions.
    fn replenish(&mut self, max: u32) -> u32 {
        let free = u32::try_from(self.xsk.free_list().len()).unwrap_or(u32::MAX);
        let spare = free.saturating_sub(self.config.tx_reserve);
        self.xsk.replenish_fill(max.min(spare))
    }

    /// A lower bound on when [`WarpEndpoint::poll`] next has timer work.
    pub fn next_wake_ns(&self) -> Option<u64> {
        [
            self.leases.next_wake_hint_ns(),
            self.launch.next_wake_hint_ns(),
            self.timers.next_wake_hint_ns(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// A parked packet's PTO deadline passed without an ACK. Runs the
    /// connection's RFC 9002 timer if it is due (time-threshold loss, or
    /// PTO with probes), then re-parks the lease unless recovery resolved
    /// it.
    fn on_lease_expired(&mut self, lease: Lease, now_ns: u64) -> Result<(), WarpError> {
        let slot = lease.conn;
        let Ok(conn) = self.conn(slot) else {
            let _ = self.xsk.retire(lease.pkt);
            return Ok(());
        };
        let due = conn
            .recovery
            .timer()
            .is_some_and(|t| t.deadline_ns <= now_ns);
        let mut lease = Some(lease);
        if due {
            self.run_recovery_timeout(slot, now_ns, Some(&mut lease))?;
        }
        if let Some(lease) = lease {
            match self.conn(slot)?.recovery.timer() {
                Some(timer) => {
                    let pn = lease.pkt.pkt_num_full();
                    let deadline = timer
                        .deadline_ns
                        .max(now_ns.saturating_add(self.config.tick_ns));
                    self.park_key(lease_key(slot, pn), deadline, lease);
                }
                // Recovery has nothing ack-eliciting in flight, so nothing
                // will resolve this lease; it is retired instead.
                None => {
                    self.stats.leases_orphaned += 1;
                    let _ = self.xsk.retire(lease.pkt);
                }
            }
        }
        Ok(())
    }

    /// RFC 9002 `OnLossDetectionTimeout` for one connection if its timer is
    /// due: losses go to BBR, leases and the application; PTO sends probes.
    /// `fired` holds a lease already taken out of the lease wheel.
    fn run_recovery_timeout(
        &mut self,
        slot: SlotHandle,
        now_ns: u64,
        fired: Option<&mut Option<Lease>>,
    ) -> Result<(), WarpError> {
        let Some(Some(conn)) = self.conns.get_mut(slot.index() as usize) else {
            return Ok(());
        };
        let due = conn
            .recovery
            .timer()
            .is_some_and(|t| t.deadline_ns <= now_ns);
        if conn.handle != slot || !due {
            return Ok(());
        }
        let closed = conn.error.is_some();
        self.stats.recovery_timeouts += 1;
        self.events.clear();
        let space = match conn.recovery.timer().map(|t| t.kind) {
            Some(recovery::TimerKind::Loss(sp) | recovery::TimerKind::Pto(sp)) => sp,
            None => PnSpace::ApplicationData,
        };
        let Conn { recovery, cc, .. } = conn;
        let events = &mut self.events;
        let action = if space == PnSpace::ApplicationData {
            recovery.on_timeout(now_ns, &mut |ev| record_event(cc, events, ev, now_ns))
        } else {
            // Long-header packets are not in BBR's flight (see the module
            // docs): their losses only reach the CRYPTO buffers.
            recovery.on_timeout(now_ns, &mut |ev| record_long_event(events, ev))
        };
        self.resolve_events(slot, space, fired);
        if let TimeoutAction::SendProbe { space, packets, .. } = action
            && !closed
            && space != PnSpace::ApplicationData
        {
            for _ in 0..packets {
                if self.send_long_probe(slot, space, now_ns).is_err() {
                    self.stats.probes_failed += 1;
                    break;
                }
            }
        } else if let TimeoutAction::SendProbe { packets, .. } = action
            && !closed
        {
            // A probe that cannot be built (no tx frame, recovery full) is
            // counted, not returned: the fired lease must still be re-parked
            // and recovery has already backed its PTO off.
            for _ in 0..packets {
                if self.send_probe(slot, now_ns).is_err() {
                    self.stats.probes_failed += 1;
                    break;
                }
            }
        }
        self.write_tile(slot);
        Ok(())
    }

    /// Applies the acknowledgements and losses in `self.events` to leases,
    /// keys and the application. `fired` holds a lease already taken out
    /// of the wheel.
    fn resolve_events(
        &mut self,
        slot: SlotHandle,
        space: PnSpace,
        mut fired: Option<&mut Option<Lease>>,
    ) {
        if space != PnSpace::ApplicationData {
            self.resolve_long_events(slot, space);
            return;
        }
        for i in 0..self.events.len() {
            let (pn, acked) = match self.events[i] {
                CongestionEvent::Acked { packet } => (packet.pn, true),
                CongestionEvent::Lost { packet } => (packet.pn, false),
                _ => continue,
            };
            if acked {
                self.stats.packets_acked += 1;
                if let Some(Some(c)) = self.conns.get_mut(slot.index() as usize)
                    && let Some(keys) = c.keys.as_mut()
                {
                    keys.on_packet_acked(pn);
                }
            } else {
                self.stats.packets_lost += 1;
                self.app.on_lost(slot, pn);
            }
            if let Some(Some(c)) = self.conns.get_mut(slot.index() as usize) {
                if acked {
                    c.streams.on_packet_acked(pn);
                } else {
                    c.streams.on_packet_lost(pn);
                }
            }
            let held = fired
                .as_deref_mut()
                .filter(|l| l.as_ref().is_some_and(|l| l.pkt.pkt_num_full() == pn))
                .and_then(Option::take);
            let taken = held.or_else(|| {
                self.leases
                    .on_ack(PacketNumberSpace::ApplicationData, lease_key(slot, pn))
                    .ok()
            });
            if let Some(lease) = taken {
                debug_assert_eq!(lease.conn, slot);
                if acked {
                    self.stats.leases_retired_acked += 1;
                } else {
                    self.stats.leases_retired_lost += 1;
                }
                let _ = self.xsk.retire(lease.pkt);
            }
        }
    }

    /// A PTO probe (RFC 9002 §6.2.4): stream data when there is some
    /// to send, else a PING.
    fn send_probe(&mut self, slot: SlotHandle, now_ns: u64) -> Result<(), WarpError> {
        let ack = self.conn(slot)?.received.largest().is_some();
        // A server's unacknowledged HANDSHAKE_DONE rides on every probe:
        // until it arrives the client keeps probing in a space the server
        // has discarded (RFC 9002 §6.2.4 prefers data in probes).
        if let Ok(c) = self.conn_mut(slot)
            && c.hs
                .as_ref()
                .is_some_and(|h| h.is_server() && h.tls.is_handshake_complete())
            && !c.streams.handshake_done_acked()
        {
            c.streams.queue_handshake_done();
        }
        if self.config.stream_mode == StreamMode::Reliable {
            let budget = self.frame_budget(slot, ack, now_ns);
            let c = self.conns[slot.index() as usize]
                .as_mut()
                .ok_or(WarpError::StaleConnection)?;
            if c.streams.has_pending() {
                let len = c
                    .streams
                    .fill(&mut self.app_out[..budget], &mut self.record);
                if len > 0 {
                    let sent =
                        self.send_carrying(slot, Source::Fresh, ack, len, Kind::Probe, now_ns);
                    self.settle_record(slot, &sent);
                    sent?;
                    self.stats.probes_sent += 1;
                    self.stats.data_probes_sent += 1;
                    return Ok(());
                }
                self.record.clear();
            }
        }
        let n = Frame::Ping.encode(&mut self.app_out)?;
        self.send(slot, Source::Fresh, ack, n, Kind::Probe, now_ns)?;
        self.stats.probes_sent += 1;
        Ok(())
    }

    /// Builds, seals, schedules and launches one packet whose frames are an
    /// optional ACK followed by `app_out[..data_len]`. A turned-around rx
    /// frame is consumed on every path.
    fn send(
        &mut self,
        slot: SlotHandle,
        source: Source,
        with_ack: bool,
        data_len: usize,
        kind: Kind,
        now_ns: u64,
    ) -> Result<u64, WarpError> {
        self.send_carrying(slot, source, with_ack, data_len, kind, now_ns)
            .map(|(pn, _)| pn)
    }

    /// [`Self::send`], also reporting whether `app_out[..data_len]` made it
    /// into the packet.
    fn send_carrying(
        &mut self,
        slot: SlotHandle,
        source: Source,
        with_ack: bool,
        data_len: usize,
        kind: Kind,
        now_ns: u64,
    ) -> Result<(u64, bool), WarpError> {
        match self.build(slot, source, with_ack, data_len, kind, now_ns) {
            Ok(staged) => {
                let pn = staged.pn;
                let carried = staged.carried;
                self.launch_or_queue(staged, now_ns);
                Ok((pn, carried))
            }
            Err((Some(rx), error)) => match self.xsk.recycle_to_fill_ring(rx) {
                Ok(()) => Err(error),
                Err(refused) => Err(WarpError::Umem(refused.error)),
            },
            Err((None, error)) => Err(error),
        }
    }

    /// Starts a key update once `key_update_interval` packets were sealed
    /// under the current send key and RFC 9001 §6 permits one. A
    /// peer-initiated update also resets the count.
    fn maybe_update_keys(&mut self, slot: SlotHandle) {
        let Some(Some(c)) = self.conns.get_mut(slot.index() as usize) else {
            return;
        };
        // RFC 9001 §6: no key update before the handshake is confirmed.
        let confirmed = c.hs.as_ref().is_none_or(|h| h.tls.is_handshake_confirmed());
        let Some(keys) = c.keys.as_mut() else {
            return;
        };
        if keys.send_generation() != c.send_generation {
            c.send_generation = keys.send_generation();
            c.sent_under_key = 0;
        }
        if c.sent_under_key >= self.config.key_update_interval
            && confirmed
            && keys.key_update_permitted()
            && keys.initiate_key_update().is_ok()
        {
            self.stats.key_updates += 1;
            c.send_generation = keys.send_generation();
            c.sent_under_key = 0;
        }
    }

    /// Starts a key update now (RFC 9001 §6.1), e.g. on local policy.
    pub fn initiate_key_update(&mut self, conn: SlotHandle) -> Result<(), WarpError> {
        self.conn(conn)?;
        let c = self.conns[conn.index() as usize].as_mut().expect("checked");
        if c.hs
            .as_ref()
            .is_some_and(|h| !h.tls.is_handshake_confirmed())
        {
            return Err(WarpError::Handshaking);
        }
        let keys = c.keys.as_mut().ok_or(WarpError::KeysUnavailable)?;
        keys.initiate_key_update()?;
        self.stats.key_updates += 1;
        c.send_generation = keys.send_generation();
        c.sent_under_key = 0;
        Ok(())
    }

    /// Writes the plaintext packet into `scratch`; returns its length and
    /// header length, and whether it carries an ACK.
    fn write_plaintext(
        &mut self,
        slot: SlotHandle,
        tile: &ConnState,
        with_ack: bool,
        data_len: usize,
        now_ns: u64,
    ) -> Result<(usize, usize, bool), WarpError> {
        let exponent = self.config.ack_delay_exponent;
        let conn = self.conns[slot.index() as usize]
            .as_mut()
            .ok_or(WarpError::StaleConnection)?;
        let scratch = &mut self.scratch;
        // Frames go after a maximal header and move once the PN length is known.
        let mut at = MAX_SHORT_HEADER;
        let mut has_ack = false;
        if with_ack {
            let ack = conn
                .received
                .ack_frame(now_ns, exponent)
                .map_err(|_| WarpError::Config("bad ack_delay_exponent"))?;
            if let Some(ack) = ack {
                at += Frame::Ack(ack).encode(&mut scratch[at..])?;
                has_ack = true;
            }
        }
        if at + data_len > scratch.len() {
            return Err(WarpError::PacketTooLarge { len: at + data_len });
        }
        scratch[at..at + data_len].copy_from_slice(&self.app_out[..data_len]);
        let payload_len = at + data_len - MAX_SHORT_HEADER;
        if payload_len == 0 {
            return Err(WarpError::Config("empty packet"));
        }
        let pn = tile.next_tx_pn;
        let shortest = encoded_len(pn, conn.recovery.largest_acked(PnSpace::ApplicationData))
            .map_err(|_| WarpError::PacketNumbersExhausted)?
            .bytes();
        // The header-protection sample needs pn_len + payload >= 4: use a
        // longer PN encoding rather than padding (padding would put
        // ACK-only packets in flight).
        let pn_bytes = shortest.max(4usize.saturating_sub(payload_len));
        let pn_len = PnLen::new(pn_bytes as u8).expect("1..=4 bytes");
        let header_len = self.parser.write_short(
            &mut scratch[..MAX_SHORT_HEADER],
            tile.peer_cid.as_bytes(),
            false,
            conn.keys
                .as_ref()
                .ok_or(WarpError::KeysUnavailable)?
                .send_key_phase(),
            pn,
            pn_len,
        )?;
        scratch.copy_within(MAX_SHORT_HEADER..MAX_SHORT_HEADER + payload_len, header_len);
        let plain_len = header_len + payload_len;
        if plain_len + TAG_LEN > usize::from(self.config.max_datagram) {
            return Err(WarpError::PacketTooLarge {
                len: plain_len + TAG_LEN,
            });
        }
        Ok((plain_len, header_len, has_ack))
    }

    /// Steps 7a-7d: frame (turn-around or fresh), seal, schedule, record.
    #[allow(clippy::type_complexity)]
    fn build(
        &mut self,
        slot: SlotHandle,
        source: Source,
        with_ack: bool,
        data_len: usize,
        kind: Kind,
        now_ns: u64,
    ) -> Result<Staged, (Option<UmemFrame<RxHarvested>>, WarpError)> {
        let probe = kind == Kind::Probe;
        let rx = match source {
            Source::TurnAround(rx) => Some(rx),
            Source::Fresh => None,
        };
        let tile = match self.table.get(slot) {
            Ok(tile) => tile,
            Err(_) => return Err((rx, WarpError::StaleConnection)),
        };
        let pn = tile.next_tx_pn;
        if pn >= MAX_SEND_PN {
            return Err((rx, WarpError::PacketNumbersExhausted));
        }
        self.maybe_update_keys(slot);
        let (plain_len, header_len, has_ack, data_len) =
            match self.write_plaintext(slot, &tile, with_ack, data_len, now_ns) {
                Ok((p, h, a)) => (p, h, a, data_len),
                // A reply whose application frames do not fit still carries
                // its ACK.
                Err(WarpError::PacketTooLarge { .. }) if data_len > 0 && rx.is_some() => {
                    self.stats.oversize_replies += 1;
                    match self.write_plaintext(slot, &tile, with_ack, 0, now_ns) {
                        Ok((p, h, a)) => (p, h, a, 0),
                        Err(e) => return Err((rx, e)),
                    }
                }
                Err(e) => return Err((rx, e)),
            };
        let total = plain_len + TAG_LEN;
        let ack_eliciting = probe || (data_len > 0 && kind != Kind::Close);
        if let Some(Some(c)) = self.conns.get(slot.index() as usize)
            && let Some(amp) = c.hs.as_ref().and_then(|h| h.amp)
            && !amp.allows(total as u64)
        {
            self.stats.amplification_blocked += 1;
            return Err((rx, WarpError::AmplificationLimited));
        }

        // The received frame turned around in place (same chunk and data
        // offset), or a fresh frame (lane 3).
        let mut frame = match rx {
            Some(rx) => match self.xsk.turn_around_tx_in_place(rx, total as u16) {
                Ok(tx) => tx,
                Err(refused) => {
                    let e = refused.error;
                    return Err((Some(refused.into_owner()), WarpError::Umem(e)));
                }
            },
            None => match self.xsk.alloc_tx(total as u16) {
                Ok(tx) => tx,
                Err(e) => return Err((None, WarpError::Umem(e))),
            },
        };

        // Seal under a linear nonce ticket for exactly this PN (lane 5).
        let conn = self.conns[slot.index() as usize]
            .as_mut()
            .expect("live: checked by write_plaintext");
        let pn_offset = 1 + tile.peer_cid.as_bytes().len();
        let sealed = match conn.keys.as_mut() {
            None => Err(WarpError::KeysUnavailable),
            Some(keys) => match keys.ticket(pn) {
                Err(e) => Err(WarpError::Crypto(e)),
                Ok(ticket) => match self.xsk.tx_bytes_mut(&mut frame) {
                    Err(e) => {
                        let _ = keys.abandon(ticket);
                        Err(WarpError::Umem(e))
                    }
                    Ok(bytes) => {
                        bytes[..plain_len].copy_from_slice(&self.scratch[..plain_len]);
                        match keys.seal(ticket, bytes, pn_offset, plain_len - header_len) {
                            Ok(n) => {
                                debug_assert_eq!(n, total);
                                conn.sent_under_key += 1;
                                Ok(())
                            }
                            Err(err) => {
                                let _ = keys.abandon(err.ticket);
                                Err(WarpError::Crypto(err.error))
                            }
                        }
                    }
                },
            },
        };
        if let Err(e) = sealed {
            let _ = self.xsk.abandon_tx(frame);
            return Err((None, e));
        }

        // EDT schedule (lane 2) and the send record (lanes 1, 2). ACK-only
        // packets are not paced and not in flight.
        // ACK-only packets are not paced, but they keep wire order behind
        // the connection's queued packets: overtaking them would make the
        // peer acknowledge a higher packet number first, and the
        // packet-threshold rule would then declare the queued ones lost.
        let edt_ns = if probe {
            // A PTO probe leaves now (RFC 9002 §6.2.4 sends probes even
            // when the congestion window is full). Pacing it could push
            // it past the launch horizon after loss has collapsed the
            // pacing rate, and a refused probe backs the PTO off with
            // nothing sent.
            now_ns
        } else if ack_eliciting {
            conn.cc.schedule_edt_ns(total as u32, now_ns)
        } else if kind == Kind::Close {
            // Closing: queued packets are suppressed, so CONNECTION_CLOSE
            // leaves now rather than behind them.
            now_ns
        } else {
            now_ns.max(conn.last_edt_ns)
        };
        conn.last_edt_ns = conn.last_edt_ns.max(edt_ns);
        let sent = recovery::SentPacket {
            pn,
            // Recovery needs monotone send times; an unpaced ACK may leave
            // before a paced packet's EDT, so it is stamped no earlier.
            sent_ns: edt_ns.max(conn.last_sent_ns),
            bytes: total as u32,
            ack_eliciting,
            in_flight: ack_eliciting,
        };
        let recorded = conn
            .recovery
            .on_packet_sent(PnSpace::ApplicationData, sent)
            .map_err(WarpError::Recovery)
            .and_then(|()| {
                conn.cc
                    .on_packet_sent(now_ns, sent, probe)
                    .map_err(WarpError::Cc)
            });
        conn.last_sent_ns = sent.sent_ns;
        if has_ack {
            conn.received.on_ack_sent();
        }
        // The packet number is burned whatever happens next.
        if let Ok(mut st) = self.table.get(slot) {
            st.next_tx_pn = pn + 1;
            let _ = self.table.set(slot, &st);
        }
        if let Err(e) = recorded {
            let _ = self.xsk.abandon_tx(frame);
            return Err((None, e));
        }
        if let Some(amp) = conn.hs.as_mut().and_then(|h| h.amp.as_mut()) {
            amp.sent += total as u64;
        }
        self.write_tile(slot);
        self.stats.packets_sent += 1;
        if !ack_eliciting {
            self.stats.ack_only_sent += 1;
        }
        Ok(Staged {
            conn: slot,
            frame,
            pn,
            edt_ns,
            park: ack_eliciting,
            tapped: false,
            carried: data_len > 0,
            close: kind == Kind::Close,
        })
    }

    /// Submits now if the departure time has come, else waits in the EDT
    /// launch queue (lane 4), the software stand-in for NIC launch time.
    fn launch_or_queue(&mut self, staged: Staged, now_ns: u64) {
        self.release_launches(now_ns);
        if staged.edt_ns <= now_ns {
            self.submit(staged, now_ns);
            return;
        }
        match self.launch.submit(now_ns, staged.edt_ns, staged) {
            Ok(()) => self.stats.launch_queued += 1,
            // Full or beyond the horizon. The NIC does not enforce launch
            // times, so submitting now would send early; the packet is
            // dropped instead and recovery later declares it lost.
            // `send_stream` and replies check for room first.
            Err(refused) => {
                self.stats.launch_refused += 1;
                self.stats.tx_abandoned += 1;
                let _ = self.xsk.abandon_tx(refused.into_packet().frame);
            }
        }
    }

    /// Whether a paced packet on `conn` would find room in the launch queue.
    fn launch_has_room(&self, conn: SlotHandle, now_ns: u64) -> bool {
        let Ok(c) = self.conn(conn) else {
            return false;
        };
        self.launch.len() < self.config.launch_capacity as usize
            && c.cc.controller().next_edt_ns()
                <= now_ns.saturating_add(self.config.launch_horizon_ns)
    }

    fn release_launches(&mut self, now_ns: u64) -> u32 {
        let mut n = 0;
        while let Ok(Some(launch)) = self.launch.poll_release(now_ns) {
            n += 1;
            self.stats.launch_released += 1;
            self.submit(launch.packet, now_ns);
        }
        n
    }

    /// `submit_paced_tx` (lane 3) then `park_for_ack_or_pto` (lane 4).
    fn submit(&mut self, staged: Staged, now_ns: u64) {
        let Staged {
            conn,
            mut frame,
            pn,
            edt_ns,
            park,
            tapped,
            close,
            ..
        } = staged;
        // Closing or draining (RFC 9000 §10.2): nothing but CONNECTION_CLOSE
        // leaves, including packets queued before the close.
        let suppressed = match self.conn(conn) {
            Err(_) => true,
            Ok(c) => c.error.is_some() && !close,
        };
        if suppressed {
            if self.conn(conn).is_ok() {
                self.stats.tx_suppressed_closed += 1;
            }
            let _ = self.xsk.abandon_tx(frame);
            return;
        }
        if !tapped && let Ok(bytes) = self.xsk.tx_bytes_mut(&mut frame) {
            self.tap.on_transmit(conn, pn, edt_ns, bytes);
        }
        match self.xsk.submit_paced_tx(frame, pn, edt_ns) {
            Ok(pkt) if park => {
                let deadline = self.pto_deadline(conn, edt_ns);
                self.park_key(lease_key(conn, pn), deadline, Lease { conn, pkt });
            }
            // Not in flight: nothing will acknowledge it; the chunk returns
            // when its transmission completes.
            Ok(pkt) => {
                let _ = self.xsk.retire(pkt);
            }
            Err(refused) => {
                let frame = refused.into_owner();
                let retry = Staged {
                    conn,
                    frame,
                    pn,
                    edt_ns: edt_ns.max(now_ns.saturating_add(self.config.tick_ns)),
                    park,
                    tapped: true,
                    carried: true,
                    close,
                };
                let at = retry.edt_ns;
                if let Err(refused) = self.launch.submit(now_ns, at, retry) {
                    // Recovery declares it lost later.
                    self.stats.tx_abandoned += 1;
                    let _ = self.xsk.abandon_tx(refused.into_packet().frame);
                }
            }
        }
    }

    /// RFC 9002 §6.2.1 PTO for a packet departing at `edt_ns`.
    fn pto_deadline(&self, conn: SlotHandle, edt_ns: u64) -> u64 {
        let Ok(c) = self.conn(conn) else {
            return edt_ns;
        };
        let base = c
            .recovery
            .rtt()
            .pto_base_ns()
            .saturating_add(self.config.max_ack_delay_ns);
        let backoff = base
            .checked_shl(c.recovery.pto_count().min(30))
            .unwrap_or(u64::MAX);
        edt_ns.saturating_add(backoff)
    }

    fn park_key(&mut self, key: u64, deadline_ns: u64, lease: Lease) {
        match self
            .leases
            .park(PacketNumberSpace::ApplicationData, key, deadline_ns, lease)
        {
            Ok(()) => self.stats.leases_parked += 1,
            // Full or duplicate: the lease cannot be tracked, so the chunk
            // is retired now (its ACK or loss then finds no lease).
            Err(refused) => {
                let _ = self.xsk.retire(refused.into_lease().pkt);
            }
        }
    }
}

impl<B: XskBackend, R: Responder, T: TxTap> Drop for WarpEndpoint<B, R, T> {
    /// Returns every queued frame and parked lease to the driver, so the
    /// wheels are empty (lane 4 counts entries dropped in a non-empty
    /// wheel as leaks).
    fn drop(&mut self) {
        let Self {
            xsk,
            leases,
            launch,
            timers,
            ..
        } = self;
        timers.drain(|_, _| {});
        launch.drain(|_, staged| {
            let _ = xsk.abandon_tx(staged.frame);
        });
        leases.discard_space(PacketNumberSpace::ApplicationData, |_, lease| {
            let _ = xsk.retire(lease.pkt);
        });
    }
}

/// What a packet is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Normal,
    /// An RFC 9002 PTO probe: ack-eliciting, bypasses the congestion window.
    Probe,
    /// CONNECTION_CLOSE: not ack-eliciting.
    Close,
}

/// What a long-header packet is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LongKind {
    Normal,
    Probe,
    Close { code: u64, frame_type: u64 },
}

/// Where a packet's frame comes from.
enum Source {
    TurnAround(UmemFrame<RxHarvested>),
    Fresh,
}

/// Steps 3-5 on the received frame's bytes; returns (header length,
/// payload length, full packet number).
fn open_in_place<B: XskBackend>(
    xsk: &mut Xsk<B>,
    conn: &mut Conn,
    frame: &mut UmemFrame<RxHarvested>,
    header: &crate::header::ProtectedHeader,
    largest_rx_pn: Option<u64>,
    now_ns: u64,
) -> Result<(usize, usize, u64), DropReason> {
    let buf = xsk.rx_bytes_mut(frame).map_err(DropReason::Umem)?;
    let packet =
        buf.get_mut(..header.packet_end)
            .ok_or(DropReason::Header(HeaderError::Truncated {
                needed: header.packet_end,
                available: 0,
            }))?;
    let opened = conn
        .keys
        .as_mut()
        .ok_or(DropReason::KeysUnavailable)?
        .open_packet(packet, header.pn_offset, now_ns, |u| {
            reconstruct(u, largest_rx_pn)
        })
        .map_err(DropReason::Crypto)?;
    // Lane 1 reads the now-unprotected PN field; it must agree with lane 5.
    let u = header
        .packet_number_field(packet)
        .map_err(DropReason::Header)?;
    debug_assert_eq!(u.header_len, opened.header_len);
    u.check_reserved_bits()
        .map_err(|_| DropReason::ReservedBits)?;
    conn.received
        .check(opened.packet_number)
        .map_err(DropReason::Duplicate)?;
    Ok((opened.header_len, opened.payload_len, opened.packet_number))
}

/// Step 6 on the opened payload; returns (ack-eliciting, reply length).
#[allow(clippy::too_many_arguments)]
fn scan_in_place<B: XskBackend, R: Responder>(
    xsk: &mut Xsk<B>,
    conn: &mut Conn,
    events: &mut Vec<CongestionEvent>,
    app: &mut R,
    app_out: &mut [u8],
    frame: &mut UmemFrame<RxHarvested>,
    slot: SlotHandle,
    payload: std::ops::Range<usize>,
    pn: u64,
    now_ns: u64,
    mode: StreamMode,
) -> Result<(bool, usize), DropReason> {
    let exponent = conn.peer_ack_exponent;
    let buf = xsk.rx_bytes_mut(frame).map_err(DropReason::Umem)?;
    let payload = &buf[payload];
    let mut app_len = 0usize;
    let mut failure: Option<DropReason> = None;
    let mut peer_close = None;
    let info = scan_payload(payload, PacketType::OneRtt, |f| {
        if failure.is_some() {
            return;
        }
        match f {
            Frame::Ack(ack) => {
                if let Err(reason) = on_ack_frame(conn, events, ack, now_ns, exponent) {
                    failure = Some(reason);
                }
            }
            Frame::Stream(s) if mode == StreamMode::Raw => {
                // At most what one datagram can carry after a header and
                // the tag; a reply that still does not fit with its ACK
                // falls back to the ACK alone.
                let budget = app_out.len().saturating_sub(TAG_LEN + MAX_SHORT_HEADER);
                if app_len < budget {
                    app_len += app.on_stream(slot, s, &mut app_out[app_len..budget]);
                }
            }
            Frame::ConnectionClose(c) => {
                peer_close = Some(ConnectionError::Peer {
                    error_code: c.error_code,
                    frame_type: c.frame_type,
                });
            }
            Frame::Stream(_)
            | Frame::ResetStream { .. }
            | Frame::StopSending { .. }
            | Frame::MaxData(_)
            | Frame::MaxStreamData { .. }
            | Frame::MaxStreams { .. }
            | Frame::DataBlocked(_)
            | Frame::StreamDataBlocked { .. }
            | Frame::StreamsBlocked { .. }
            | Frame::Crypto { .. }
                if mode == StreamMode::Reliable =>
            {
                if let Err(e) = conn.streams.on_frame(f) {
                    failure = Some(DropReason::Transport(e));
                }
            }
            Frame::HandshakeDone => {
                if let Some(hs) = conn.hs.as_mut() {
                    hs.done_received = true;
                }
            }
            _ => {}
        }
    })
    .map_err(DropReason::Frame)?;
    if let Some(reason) = failure {
        return Err(reason);
    }
    if let Some(close) = peer_close {
        conn.error = Some(close);
    }
    conn.received
        .on_packet_received(pn, now_ns, info.ack_eliciting, Ecn::NotEct)
        .map_err(DropReason::Duplicate)?;
    Ok((info.ack_eliciting, app_len))
}

/// One ACK frame: RFC 9002 recovery (lane 1) and its events through the
/// BBRv3 adapter (lane 2). Events are kept in `events` for lease retirement.
fn on_ack_frame(
    conn: &mut Conn,
    events: &mut Vec<CongestionEvent>,
    ack: &crate::frame::AckFrame<'_>,
    now_ns: u64,
    exponent: u8,
) -> Result<(), DropReason> {
    let delay = recovery::ack_delay_ns(ack.ack_delay, exponent).map_err(DropReason::Recovery)?;
    let Conn { recovery, cc, .. } = conn;
    cc.begin_ack();
    let outcome = recovery
        .on_ack_received(PnSpace::ApplicationData, ack, delay, now_ns, &mut |ev| {
            record_event(cc, events, ev, now_ns);
        })
        .map_err(DropReason::Recovery)?;
    if let Err(e) = cc.end_ack(now_ns, outcome.rtt_sample) {
        debug_assert!(false, "BBR rejected an ACK: {e:?}");
    }
    Ok(())
}

/// A recovery event goes straight to the BBR adapter (which batches
/// `Acked` until the ACK ends); acknowledgements and losses are also kept
/// for lease retirement. Each sent packet is resolved at most once, so
/// those fit in `max_tracked_per_conn` however many ACK frames a packet
/// carries.
fn record_event(
    cc: &mut QuicBbr,
    events: &mut Vec<CongestionEvent>,
    ev: CongestionEvent,
    now_ns: u64,
) {
    if let Err(e) = cc.on_event(now_ns, ev) {
        debug_assert!(false, "BBR rejected an event: {e:?}");
    }
    if matches!(
        ev,
        CongestionEvent::Acked { .. } | CongestionEvent::Lost { .. }
    ) {
        debug_assert!(events.len() < events.capacity(), "sized to max_tracked");
        if events.len() < events.capacity() {
            events.push(ev);
        }
    }
}

/// A long-header space's recovery event: acknowledgements and losses are
/// kept for the CRYPTO buffers; nothing reaches BBR.
fn record_long_event(events: &mut Vec<CongestionEvent>, ev: CongestionEvent) {
    if matches!(
        ev,
        CongestionEvent::Acked { .. } | CongestionEvent::Lost { .. }
    ) && events.len() < events.capacity()
    {
        events.push(ev);
    }
}

/// The transport parameters an endpoint advertises (RFC 9000 §18.2).
fn local_transport_params(config: &EndpointConfig, local_cid: &[u8]) -> TransportParameters {
    let w = u64::from(config.streams.stream_window);
    TransportParameters {
        initial_source_connection_id: vesper_warp_tls::ConnectionId::new(local_cid).ok(),
        max_idle_timeout_ms: config.idle_timeout_ns / 1_000_000,
        max_udp_payload_size: u64::from(config.max_datagram),
        initial_max_data: config.streams.conn_window,
        initial_max_stream_data_bidi_local: w,
        initial_max_stream_data_bidi_remote: w,
        initial_max_stream_data_uni: w,
        initial_max_streams_bidi: config.streams.max_streams_bidi,
        initial_max_streams_uni: config.streams.max_streams_uni,
        ack_delay_exponent: u64::from(config.ack_delay_exponent),
        max_ack_delay_ms: config.max_ack_delay_ns.div_ceil(1_000_000),
        // No connection migration (see the module docs).
        disable_active_migration: true,
        ..TransportParameters::default()
    }
}

/// Packet-number reconstruction for lane 5's opener: lane 1's RFC 9000 A.3
/// decode from the tile's largest received packet number.
fn reconstruct(u: &Unprotected, largest: Option<u64>) -> Option<u64> {
    let len = PnLen::new(u8::try_from(u.pn_len).ok()?)?;
    let pn = TruncatedPn {
        len,
        value: u32::try_from(u.truncated_pn).ok()?,
    };
    pn.decode(largest).ok()
}
