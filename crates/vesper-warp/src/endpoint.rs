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
//! What this is not: there is no TLS handshake (connections are opened
//! with application traffic secrets the caller supplies), no stream
//! reassembly or retransmission of lost stream data (loss is reported to
//! the [`Responder`]), no connection migration, and no hardware EDT, RSS
//! or crypto offload. With [`SoftNic`](vesper_warp_umem::SoftNic) the
//! "NIC" is lane 3's deterministic model; with lane 3's gated AF_XDP
//! backend it is a real socket, which this crate's tests do not exercise.

use std::fmt;

use vesper_warp_bbr::BbrConfig;
use vesper_warp_crypto::{CryptoError, OneRttKeys, TAG_LEN, Unprotected};
use vesper_warp_shard::{ConnState, ConnTable, ConnectionId, SlotHandle, TableConfig};
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
use crate::header::{HeaderError, HeaderParser, ParsedPacket};
use crate::packet_number::{PnLen, TruncatedPn, encoded_len};
use crate::recovery::{
    self, CongestionEvent, HandshakeStatus, LossRecovery, RecoveryConfig, RecoveryError,
    TimeoutAction,
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
/// ACK ranges kept per connection.
const ACK_RANGES: usize = 32;

/// Which §35.3 CID lookup to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CidLookup {
    /// Lane 6's open-addressed hash index (O(1) average).
    Index,
    /// Lane 6's `lookup_cid_simd` 16-lane tile scan (O(tiles)).
    TileScan,
}

/// Endpoint parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointConfig {
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
    /// A long-header, Retry or Version Negotiation packet: there is no
    /// handshake here, only established 1-RTT connections.
    NotOneRtt,
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
    Cc(CcError)
);

/// The application side: what to send back for received stream data.
pub trait Responder {
    /// Called for each STREAM frame of an authenticated packet, before the
    /// reply is built. Writes reply frames into `out` and returns their
    /// length (0 for none). Frames that do not fit must not be written.
    fn on_stream(&mut self, conn: SlotHandle, frame: &StreamFrame<'_>, out: &mut [u8]) -> usize;

    /// A packet carrying this connection's data was declared lost. The
    /// endpoint has no stream layer, so retransmitting is the
    /// application's choice.
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
}

/// A connection's non-tile state. The tile (lane 6) holds the hot scalars:
/// largest received PN, next PN, BBR bandwidth/min-RTT, cwnd, bytes in
/// flight, next EDT, idle deadline and the peer CID.
struct Conn {
    handle: SlotHandle,
    keys: OneRttKeys,
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
    pub keys: &'a OneRttKeys,
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
    app: R,
    tap: T,
    stats: EndpointStats,
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
            app,
            tap,
            stats: EndpointStats::default(),
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
    /// handshake is implemented).
    pub fn open_connection(
        &mut self,
        local_cid: &[u8],
        peer_cid: &[u8],
        keys: OneRttKeys,
        now_ns: u64,
    ) -> Result<SlotHandle, WarpError> {
        let len = usize::from(self.config.cid_len);
        if local_cid.len() != len || peer_cid.len() != len {
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
            ESTABLISHED,
        );
        self.conns[handle.index() as usize] = Some(Conn {
            handle,
            keys,
            recovery,
            received: ReceivedPackets::new(PnSpace::ApplicationData, self.config.max_ack_delay_ns),
            cc,
            last_sent_ns: now_ns,
            last_edt_ns: now_ns,
            timer: None,
            sent_under_key: 0,
            send_generation: 0,
            ack_deferred: false,
        });
        self.write_tile(handle);
        Ok(handle)
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
            keys: &c.keys,
        })
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
                    Ok((h, dcid, d.len()))
                }
                Ok(_) => Err(DropReason::NotOneRtt),
                Err(e) => Err(DropReason::Header(e)),
            },
        };
        let (header, dcid, dcid_len) = match split {
            Ok(v) => v,
            Err(reason) => return self.drop_rx(frame, reason),
        };

        // 2. CID lookup in the shard's tiles (lane 6).
        let Some(slot) = self.lookup(&dcid[..dcid_len]) else {
            return self.drop_rx(frame, DropReason::UnknownCid);
        };
        let (Ok(tile), Ok(_)) = (self.table.get(slot), self.conn(slot)) else {
            return self.drop_rx(frame, DropReason::UnknownCid);
        };

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
            self.config.ack_delay_exponent,
        );
        // Retire the leases recovery resolved (lanes 3, 4).
        self.resolve_events(slot, None);
        let (ack_eliciting, app_len) = match scanned {
            Ok(v) => v,
            Err(reason) => {
                self.write_tile(slot);
                return self.drop_rx(frame, reason);
            }
        };
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
        self.send(
            slot,
            Source::TurnAround(frame),
            true,
            data_len,
            false,
            now_ns,
        )?;
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
        self.send(conn, Source::Fresh, ack, len, false, now_ns)
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
                    c.received.ack_deadline().is_some()
                }
                _ => continue,
            };
            // Out of tx frames: keep it owed and try on a later poll.
            if owed
                && self
                    .send(slot, Source::Fresh, true, 0, false, now_ns)
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
        self.stats.recovery_timeouts += 1;
        self.events.clear();
        let Conn { recovery, cc, .. } = conn;
        let events = &mut self.events;
        let action = recovery.on_timeout(now_ns, &mut |ev| record_event(cc, events, ev, now_ns));
        self.resolve_events(slot, fired);
        if let TimeoutAction::SendProbe { packets, .. } = action {
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
    fn resolve_events(&mut self, slot: SlotHandle, mut fired: Option<&mut Option<Lease>>) {
        for i in 0..self.events.len() {
            let (pn, acked) = match self.events[i] {
                CongestionEvent::Acked { packet } => (packet.pn, true),
                CongestionEvent::Lost { packet } => (packet.pn, false),
                _ => continue,
            };
            if acked {
                self.stats.packets_acked += 1;
                if let Some(Some(c)) = self.conns.get_mut(slot.index() as usize) {
                    c.keys.on_packet_acked(pn);
                }
            } else {
                self.stats.packets_lost += 1;
                self.app.on_lost(slot, pn);
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

    fn send_probe(&mut self, slot: SlotHandle, now_ns: u64) -> Result<(), WarpError> {
        let n = Frame::Ping.encode(&mut self.app_out)?;
        let ack = self.conn(slot)?.received.largest().is_some();
        self.send(slot, Source::Fresh, ack, n, true, now_ns)?;
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
        probe: bool,
        now_ns: u64,
    ) -> Result<u64, WarpError> {
        match self.build(slot, source, with_ack, data_len, probe, now_ns) {
            Ok(staged) => {
                let pn = staged.pn;
                self.launch_or_queue(staged, now_ns);
                Ok(pn)
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
        if c.keys.send_generation() != c.send_generation {
            c.send_generation = c.keys.send_generation();
            c.sent_under_key = 0;
        }
        if c.sent_under_key >= self.config.key_update_interval
            && c.keys.key_update_permitted()
            && c.keys.initiate_key_update().is_ok()
        {
            self.stats.key_updates += 1;
            c.send_generation = c.keys.send_generation();
            c.sent_under_key = 0;
        }
    }

    /// Starts a key update now (RFC 9001 §6.1), e.g. on local policy.
    pub fn initiate_key_update(&mut self, conn: SlotHandle) -> Result<(), WarpError> {
        self.conn(conn)?;
        let c = self.conns[conn.index() as usize].as_mut().expect("checked");
        c.keys.initiate_key_update()?;
        self.stats.key_updates += 1;
        c.send_generation = c.keys.send_generation();
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
            conn.keys.send_key_phase(),
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
        probe: bool,
        now_ns: u64,
    ) -> Result<Staged, (Option<UmemFrame<RxHarvested>>, WarpError)> {
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
        let ack_eliciting = data_len > 0 || probe;

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
        let sealed = match conn.keys.ticket(pn) {
            Err(e) => Err(WarpError::Crypto(e)),
            Ok(ticket) => match self.xsk.tx_bytes_mut(&mut frame) {
                Err(e) => {
                    let _ = conn.keys.abandon(ticket);
                    Err(WarpError::Umem(e))
                }
                Ok(bytes) => {
                    bytes[..plain_len].copy_from_slice(&self.scratch[..plain_len]);
                    let pn_offset = 1 + usize::from(self.config.cid_len);
                    match conn
                        .keys
                        .seal(ticket, bytes, pn_offset, plain_len - header_len)
                    {
                        Ok(n) => {
                            debug_assert_eq!(n, total);
                            conn.sent_under_key += 1;
                            Ok(())
                        }
                        Err(err) => {
                            let _ = conn.keys.abandon(err.ticket);
                            Err(WarpError::Crypto(err.error))
                        }
                    }
                }
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
        let edt_ns = if ack_eliciting {
            conn.cc.schedule_edt_ns(total as u32, now_ns)
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
        } = staged;
        if self.conn(conn).is_err() {
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
    exponent: u8,
) -> Result<(bool, usize), DropReason> {
    let buf = xsk.rx_bytes_mut(frame).map_err(DropReason::Umem)?;
    let payload = &buf[payload];
    let mut app_len = 0usize;
    let mut failure: Option<DropReason> = None;
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
            Frame::Stream(s) => {
                // At most what one datagram can carry after a header and
                // the tag; a reply that still does not fit with its ACK
                // falls back to the ACK alone.
                let budget = app_out.len().saturating_sub(TAG_LEN + MAX_SHORT_HEADER);
                if app_len < budget {
                    app_len += app.on_stream(slot, s, &mut app_out[app_len..budget]);
                }
            }
            _ => {}
        }
    })
    .map_err(DropReason::Frame)?;
    if let Some(reason) = failure {
        return Err(reason);
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
