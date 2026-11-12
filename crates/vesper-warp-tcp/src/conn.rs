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

use crate::cc::{
    AckedPacket, CongestionController, CongestionEvent, LostPacket, NewReno, RttSample, SentPacket,
};
use crate::rtt::{RtoConfig, RttEstimator};
use crate::seq::{Seq, SeqMap};
use crate::wire::{
    self, Flags, HeaderFields, IpPair, MAX_HEADER_LEN, MAX_OPTIONS_LEN, MAX_SACK_BLOCKS,
    MAX_WINDOW_SCALE, SackBlock, Segment, TcpOptions, Timestamps, WireError,
};

/// RFC 6675 DupThresh.
const DUP_THRESH: u32 = 3;
/// Smallest sender MSS accepted from a peer; smaller advertisements are
/// clamped up so that a hostile MSS option cannot stall the sender.
pub const MIN_SEND_MSS: u16 = 64;
/// Timestamp clock tick (1 ms, within RFC 7323 §5.4's 1 ms–1 s range).
const TS_TICK_NS: u64 = 1_000_000;
/// RFC 9293 §3.4.1 ISN clock: one tick per 4 µs.
const ISN_TICK_NS: u64 = 4_000;

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

impl State {
    /// States in which sequence numbers of both sides are known.
    #[must_use]
    pub const fn is_synchronized(self) -> bool {
        !matches!(self, State::Closed | State::Listen | State::SynSent)
    }
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
    chunks: [&'a [u8]; 3],
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

#[derive(Clone, Copy, Debug, Default)]
struct SentSeg {
    /// Absolute start (0 = SYN).
    start: u64,
    /// Absolute end (exclusive, includes SYN/FIN).
    end: u64,
    syn: bool,
    fin: bool,
    pn: u64,
    sent_ns: u64,
    retransmits: u32,
    sacked: bool,
    lost: bool,
    /// Retransmitted in the current recovery episode (RFC 6675 HighRxt).
    rexmit_in_recovery: bool,
}

impl SentSeg {
    fn bytes(&self) -> u32 {
        // Entries are at most one MSS (+SYN/FIN).
        (self.end - self.start) as u32
    }

    fn data_range(&self) -> (u64, u64) {
        let s = self.start.max(1);
        let e = if self.fin { self.end - 1 } else { self.end };
        (s, e.max(s))
    }
}

/// Fixed-capacity ring of sent segments; never reallocates after
/// construction.
#[derive(Debug)]
struct SentQueue {
    buf: Vec<SentSeg>,
    head: usize,
    len: usize,
}

impl SentQueue {
    fn new(cap: usize) -> SentQueue {
        SentQueue {
            buf: vec![SentSeg::default(); cap],
            head: 0,
            len: 0,
        }
    }
    fn is_full(&self) -> bool {
        self.len == self.buf.len()
    }
    fn is_empty(&self) -> bool {
        self.len == 0
    }
    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
    fn idx(&self, i: usize) -> usize {
        (self.head + i) % self.buf.len()
    }
    fn get(&self, i: usize) -> &SentSeg {
        &self.buf[self.idx(i)]
    }
    fn get_mut(&mut self, i: usize) -> &mut SentSeg {
        let k = self.idx(i);
        &mut self.buf[k]
    }
    fn push(&mut self, s: SentSeg) -> bool {
        if self.is_full() {
            return false;
        }
        let k = self.idx(self.len);
        self.buf[k] = s;
        self.len += 1;
        true
    }
    fn pop_front(&mut self) {
        if self.len > 0 {
            self.head = (self.head + 1) % self.buf.len();
            self.len -= 1;
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct OooRange {
    start: u64,
    end: u64,
    stamp: u64,
}

#[derive(Clone, Copy, Debug)]
struct RstSpec {
    ips: IpPair,
    src_port: u16,
    dst_port: u16,
    seq: Seq,
    ack: Option<Seq>,
}

const EV_CONNECTED: u8 = 1;
const EV_PEER_FIN: u8 = 2;
const EV_CLOSED: u8 = 4;

/// A single TCP connection (one TCB).
#[derive(Debug)]
pub struct Connection<C: CongestionController = NewReno> {
    cfg: Config,
    state: State,
    passive: bool,
    remote_port: u16,
    ips: Option<IpPair>,
    iss_base: u32,

    // Send side (absolute positions; 0 = SYN, data offset k at k+1).
    snd: SeqMap,
    snd_una: u64,
    snd_nxt: u64,
    snd_wnd: u32,
    max_snd_wnd: u32,
    snd_wl1: Seq,
    snd_wl2: Seq,
    snd_wscale: u8,
    rcv_wscale: u8,
    ws_ok: bool,
    sack_ok: bool,
    ts_ok: bool,
    peer_mss: u16,
    smss: u32,
    need_syn: bool,
    write_end: u64,
    fin_requested: bool,
    fin_abs: Option<u64>,
    released_off: u64,
    sent: SentQueue,
    next_pn: u64,
    rtt: RttEstimator,
    timed: Option<u64>,
    rto_deadline: Option<u64>,
    retries: u32,
    persist_deadline: Option<u64>,
    persist_backoff: u32,
    probe_due: bool,
    dupacks: u32,
    recovery_point: Option<u64>,
    force_retx: bool,
    cc: C,

    // Receive side.
    rcv: SeqMap,
    have_irs: bool,
    rcv_nxt: u64,
    rcv_adv_right: u64,
    read_off: u64,
    ooo: Vec<OooRange>,
    ooo_stamp: u64,
    ooo_fin: Option<u64>,
    fin_received: bool,
    urgent_off: Option<u64>,
    ts_recent: u32,
    ts_recent_valid: bool,
    last_ack_sent: u64,
    ack_now: bool,
    ack_pending: u32,
    delack_deadline: Option<u64>,
    challenge_window_ns: u64,
    challenge_count: u32,
    pending_rst: Option<RstSpec>,
    time_wait_deadline: Option<u64>,

    events: u8,
    close_reason: Option<CloseReason>,
    stats: Stats,
}

fn validate(cfg: &Config) -> Result<(), TcpError> {
    if cfg.mss < MIN_SEND_MSS {
        return Err(TcpError::InvalidConfig("mss below 64"));
    }
    if cfg.recv_buffer == 0 || cfg.recv_buffer > 1 << 30 {
        return Err(TcpError::InvalidConfig("recv_buffer must be in 1..=2^30"));
    }
    if cfg.window_scale.is_some_and(|s| s > MAX_WINDOW_SCALE) {
        return Err(TcpError::InvalidConfig("window_scale above 14"));
    }
    if cfg.max_in_flight_segments == 0 || cfg.max_ooo_ranges == 0 {
        return Err(TcpError::InvalidConfig("queues must be non-empty"));
    }
    if cfg.rto.min_ns == 0 || cfg.rto.min_ns > cfg.rto.max_ns {
        return Err(TcpError::InvalidConfig("rto bounds"));
    }
    Ok(())
}

impl Connection<NewReno> {
    /// A closed connection with the built-in NewReno controller.
    pub fn new(cfg: Config) -> Result<Connection<NewReno>, TcpError> {
        let cc = NewReno::new(u32::from(cfg.mss), cfg.initial_window_segments);
        Connection::with_controller(cfg, cc)
    }
}

impl<C: CongestionController> Connection<C> {
    /// A closed connection using `cc` for congestion control.
    pub fn with_controller(cfg: Config, cc: C) -> Result<Connection<C>, TcpError> {
        validate(&cfg)?;
        Ok(Connection {
            cfg,
            state: State::Closed,
            passive: false,
            remote_port: 0,
            ips: None,
            iss_base: 0,
            snd: SeqMap::default(),
            snd_una: 0,
            snd_nxt: 0,
            snd_wnd: 0,
            max_snd_wnd: 0,
            snd_wl1: Seq(0),
            snd_wl2: Seq(0),
            snd_wscale: 0,
            rcv_wscale: 0,
            ws_ok: false,
            sack_ok: false,
            ts_ok: false,
            peer_mss: 536,
            smss: 536,
            need_syn: false,
            write_end: 0,
            fin_requested: false,
            fin_abs: None,
            released_off: 0,
            sent: SentQueue::new(cfg.max_in_flight_segments as usize),
            next_pn: 0,
            rtt: RttEstimator::new(cfg.rto),
            timed: None,
            rto_deadline: None,
            retries: 0,
            persist_deadline: None,
            persist_backoff: 0,
            probe_due: false,
            dupacks: 0,
            recovery_point: None,
            force_retx: false,
            cc,
            rcv: SeqMap::default(),
            have_irs: false,
            rcv_nxt: 0,
            rcv_adv_right: 0,
            read_off: 0,
            ooo: Vec::with_capacity(cfg.max_ooo_ranges as usize),
            ooo_stamp: 0,
            ooo_fin: None,
            fin_received: false,
            urgent_off: None,
            ts_recent: 0,
            ts_recent_valid: false,
            last_ack_sent: 0,
            ack_now: false,
            ack_pending: 0,
            delack_deadline: None,
            challenge_window_ns: 0,
            challenge_count: 0,
            pending_rst: None,
            time_wait_deadline: None,
            events: 0,
            close_reason: None,
            stats: Stats::default(),
        })
    }

    // ----------------------------------------------------------------
    // Accessors
    // ----------------------------------------------------------------

    /// Current state.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// Configuration.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The congestion controller.
    #[must_use]
    pub fn controller(&self) -> &C {
        &self.cc
    }

    /// RTT estimator.
    #[must_use]
    pub fn rtt(&self) -> &RttEstimator {
        &self.rtt
    }

    /// Why the connection closed, if it closed abnormally.
    #[must_use]
    pub fn close_reason(&self) -> Option<CloseReason> {
        self.close_reason
    }

    /// Sender MSS (payload bytes per data segment).
    #[must_use]
    pub fn send_mss(&self) -> u32 {
        self.smss
    }

    /// Current peer receive window in bytes.
    #[must_use]
    pub fn send_window(&self) -> u32 {
        self.snd_wnd
    }

    /// Negotiated (send shift, receive shift) when window scaling is on.
    #[must_use]
    pub fn window_shifts(&self) -> (u8, u8) {
        (self.snd_wscale, self.rcv_wscale)
    }

    /// Whether SACK was negotiated.
    #[must_use]
    pub fn sack_enabled(&self) -> bool {
        self.sack_ok
    }

    /// Whether timestamps were negotiated.
    #[must_use]
    pub fn timestamps_enabled(&self) -> bool {
        self.ts_ok
    }

    /// Stream offset one past the highest data byte acknowledged.
    #[must_use]
    pub fn acked_offset(&self) -> u64 {
        self.snd_una.saturating_sub(1).min(self.write_end)
    }

    /// Stream offset of the next byte expected from the peer.
    #[must_use]
    pub fn recv_offset(&self) -> u64 {
        self.rcv_nxt.saturating_sub(1).min(self.data_rcv_end())
    }

    /// Bytes delivered to the application but not yet released.
    #[must_use]
    pub fn unread(&self) -> u64 {
        self.recv_offset() - self.read_off
    }

    /// Stream offset one past the last urgent byte signalled by the peer
    /// (RFC 9293 §3.1/RFC 6093 urgent-pointer semantics; the data itself is
    /// delivered inline).
    #[must_use]
    pub fn urgent_offset(&self) -> Option<u64> {
        self.urgent_off
    }

    /// Bytes in flight (RFC 6675 pipe).
    #[must_use]
    pub fn bytes_in_flight(&self) -> u32 {
        self.pipe()
    }

    /// RCV.NXT on the wire, once the peer's ISN is known.
    #[must_use]
    pub fn recv_next_seq(&self) -> Option<Seq> {
        self.have_irs.then(|| self.rcv.seq(self.rcv_nxt))
    }

    /// (SND.UNA, SND.NXT) on the wire.
    #[must_use]
    pub fn send_seqs(&self) -> (Seq, Seq) {
        (self.snd.seq(self.snd_una), self.snd.seq(self.snd_nxt))
    }

    /// Number of out-of-order ranges currently buffered.
    #[must_use]
    pub fn ooo_ranges(&self) -> usize {
        self.ooo.len()
    }

    fn data_rcv_end(&self) -> u64 {
        if self.fin_received {
            self.rcv_nxt.saturating_sub(2)
        } else {
            u64::MAX
        }
    }

    // ----------------------------------------------------------------
    // User calls (RFC 9293 §3.10.1–§3.10.6)
    // ----------------------------------------------------------------

    /// Passive OPEN. `iss_base` is the caller's per-connection ISN secret
    /// component (RFC 6528 F(); this crate adds the RFC 9293 4 µs clock).
    pub fn listen(&mut self, iss_base: u32) -> Result<(), TcpError> {
        if self.state != State::Closed {
            return Err(TcpError::InvalidState(self.state));
        }
        self.reset_tcb();
        self.iss_base = iss_base;
        self.passive = true;
        self.state = State::Listen;
        Ok(())
    }

    /// Active OPEN to `remote_port`. `ips` is oriented local → remote.
    pub fn connect(
        &mut self,
        now: u64,
        ips: IpPair,
        remote_port: u16,
        iss_base: u32,
    ) -> Result<(), TcpError> {
        if self.state != State::Closed {
            return Err(TcpError::InvalidState(self.state));
        }
        self.reset_tcb();
        self.iss_base = iss_base;
        self.passive = false;
        self.ips = Some(ips);
        self.remote_port = remote_port;
        self.peer_mss = ips.default_mss();
        self.snd.isn = self.clock_iss(now);
        self.need_syn = true;
        self.state = State::SynSent;
        Ok(())
    }

    /// Make `n` more bytes of the caller's send buffer available.
    pub fn write(&mut self, n: u64) -> Result<(), TcpError> {
        match self.state {
            State::SynSent | State::SynReceived | State::Established | State::CloseWait
                if !self.fin_requested => {}
            State::Closed | State::Listen => return Err(TcpError::InvalidState(self.state)),
            _ => return Err(TcpError::ConnectionClosing),
        }
        self.write_end = self
            .write_end
            .checked_add(n)
            .filter(|&v| v < 1 << 62)
            .ok_or(TcpError::Overflow)?;
        Ok(())
    }

    /// Stream offset one past the last byte written.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.write_end
    }

    /// CLOSE: send FIN after all written data.
    pub fn close(&mut self) -> Result<(), TcpError> {
        match self.state {
            State::Listen | State::SynSent => {
                self.enter_closed(None);
                Ok(())
            }
            State::SynReceived | State::Established | State::CloseWait if !self.fin_requested => {
                self.fin_requested = true;
                match self.state {
                    State::Established => self.state = State::FinWait1,
                    State::CloseWait => self.state = State::LastAck,
                    // RFC 9293 §3.10.4 permits queueing until ESTABLISHED.
                    _ => {}
                }
                Ok(())
            }
            State::Closed => Err(TcpError::InvalidState(self.state)),
            _ => Err(TcpError::ConnectionClosing),
        }
    }

    /// ABORT: send RST in synchronized states and close immediately.
    pub fn abort(&mut self) {
        if let (true, Some(ips)) = (
            self.state.is_synchronized() && self.state != State::TimeWait,
            self.ips,
        ) {
            self.pending_rst = Some(RstSpec {
                ips,
                src_port: self.cfg.local_port,
                dst_port: self.remote_port,
                seq: self.snd.seq(self.snd_nxt),
                ack: None,
            });
        }
        if self.state != State::Closed {
            self.enter_closed(Some(CloseReason::Aborted));
        }
    }

    /// The application finished with `n` delivered bytes, reopening the
    /// receive window and freeing their ring space.
    pub fn recv_release(&mut self, n: u64) -> Result<(), TcpError> {
        if n > self.unread() {
            return Err(TcpError::ReleaseExceedsUnread);
        }
        self.read_off += n;
        if self.state.is_synchronized() {
            let right = self.recv_right();
            if right.saturating_sub(self.rcv_adv_right) >= self.sws_threshold() {
                self.ack_now = true;
            }
        }
        Ok(())
    }

    /// Next acknowledged send-buffer range to reclaim.
    pub fn take_released(&mut self) -> Option<Released> {
        let acked = self.acked_offset();
        if acked > self.released_off {
            let r = Released {
                start: self.released_off,
                end: acked,
            };
            self.released_off = acked;
            Some(r)
        } else {
            None
        }
    }

    /// Next pending notification.
    pub fn take_event(&mut self) -> Option<Event> {
        for bit in [EV_CONNECTED, EV_PEER_FIN, EV_CLOSED] {
            if self.events & bit != 0 {
                self.events &= !bit;
                return Some(match bit {
                    EV_CONNECTED => Event::Connected,
                    EV_PEER_FIN => Event::PeerFinished,
                    _ => Event::Closed(self.close_reason),
                });
            }
        }
        None
    }

    /// Earliest timer deadline.
    #[must_use]
    pub fn poll_timeout(&self) -> Option<u64> {
        [
            self.rto_deadline,
            self.persist_deadline,
            self.delack_deadline,
            self.time_wait_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    // ----------------------------------------------------------------
    // Internal helpers
    // ----------------------------------------------------------------

    fn clock_iss(&self, now: u64) -> Seq {
        Seq(self.iss_base.wrapping_add((now / ISN_TICK_NS) as u32))
    }

    fn ts_now(now: u64) -> u32 {
        // The timestamp clock wraps by design (RFC 7323 §5.2).
        (now / TS_TICK_NS) as u32
    }

    fn reset_tcb(&mut self) {
        let cfg = self.cfg;
        self.passive = false;
        self.remote_port = 0;
        self.ips = None;
        self.snd = SeqMap::default();
        self.snd_una = 0;
        self.snd_nxt = 0;
        self.snd_wnd = 0;
        self.max_snd_wnd = 0;
        self.snd_wl1 = Seq(0);
        self.snd_wl2 = Seq(0);
        self.snd_wscale = 0;
        self.rcv_wscale = 0;
        self.ws_ok = false;
        self.sack_ok = false;
        self.ts_ok = false;
        self.peer_mss = 536;
        self.smss = 536;
        self.need_syn = false;
        self.write_end = 0;
        self.fin_requested = false;
        self.fin_abs = None;
        self.released_off = 0;
        self.sent.clear();
        self.rtt = RttEstimator::new(cfg.rto);
        self.timed = None;
        self.rto_deadline = None;
        self.retries = 0;
        self.persist_deadline = None;
        self.persist_backoff = 0;
        self.probe_due = false;
        self.dupacks = 0;
        self.recovery_point = None;
        self.force_retx = false;
        self.rcv = SeqMap::default();
        self.have_irs = false;
        self.rcv_nxt = 0;
        self.rcv_adv_right = 0;
        self.read_off = 0;
        self.ooo.clear();
        self.ooo_fin = None;
        self.fin_received = false;
        self.urgent_off = None;
        self.ts_recent = 0;
        self.ts_recent_valid = false;
        self.last_ack_sent = 0;
        self.ack_now = false;
        self.ack_pending = 0;
        self.delack_deadline = None;
        self.pending_rst = None;
        self.time_wait_deadline = None;
        self.close_reason = None;
    }

    fn enter_closed(&mut self, reason: Option<CloseReason>) {
        self.state = State::Closed;
        self.close_reason = reason;
        self.events |= EV_CLOSED;
        self.sent.clear();
        self.ooo.clear();
        self.rto_deadline = None;
        self.persist_deadline = None;
        self.delack_deadline = None;
        self.time_wait_deadline = None;
        self.need_syn = false;
        self.ack_now = false;
        self.probe_due = false;
    }

    fn enter_time_wait(&mut self, now: u64) {
        self.state = State::TimeWait;
        self.rto_deadline = None;
        self.persist_deadline = None;
        self.time_wait_deadline = Some(now.saturating_add(self.cfg.time_wait_ns));
    }

    fn enter_established(&mut self) {
        self.state = State::Established;
        self.events |= EV_CONNECTED;
        if self.fin_requested {
            self.state = State::FinWait1;
        }
    }

    /// Absolute right edge of the receive window (1 + read offset + ring).
    fn recv_right(&self) -> u64 {
        1 + self.read_off + u64::from(self.cfg.recv_buffer)
    }

    fn recv_window(&self) -> u32 {
        // Bounded by recv_buffer (<= 2^30).
        self.recv_right().saturating_sub(self.rcv_nxt) as u32
    }

    fn sws_threshold(&self) -> u64 {
        u64::from(
            (self.cfg.recv_buffer / 2)
                .min(u32::from(self.cfg.mss))
                .max(1),
        )
    }

    fn fin_acked(&self) -> bool {
        self.fin_abs.is_some_and(|f| self.snd_una > f)
    }

    fn pipe(&self) -> u32 {
        let mut p: u32 = 0;
        for i in 0..self.sent.len {
            let s = self.sent.get(i);
            if !s.sacked && !s.lost {
                p = p.saturating_add(s.bytes());
            }
        }
        if self.recovery_point.is_some() && !self.sack_ok {
            // RFC 6582: each duplicate ACK signals a segment left the
            // network (the window-inflation equivalent for pipe).
            p = p.saturating_sub(self.dupacks.saturating_mul(self.smss));
        }
        p
    }

    fn set_send_mss(&mut self) {
        let base = self.peer_mss.max(MIN_SEND_MSS).min(self.cfg.mss);
        let ts = if self.ts_ok { 12 } else { 0 };
        self.smss = u32::from(base) - ts;
        self.cc.on_mss(self.smss);
    }

    fn negotiate(&mut self, opts: &TcpOptions, ips: IpPair) {
        self.peer_mss = opts.mss.unwrap_or(ips.default_mss());
        match (self.cfg.window_scale, opts.window_scale) {
            (Some(ours), Some(theirs)) => {
                self.snd_wscale = theirs.min(MAX_WINDOW_SCALE);
                self.rcv_wscale = ours;
                self.ws_ok = true;
            }
            _ => {
                self.snd_wscale = 0;
                self.rcv_wscale = 0;
                self.ws_ok = false;
            }
        }
        self.sack_ok = self.cfg.sack && opts.sack_permitted;
        self.ts_ok = self.cfg.timestamps && opts.timestamps.is_some();
        if let (true, Some(ts)) = (self.ts_ok, opts.timestamps) {
            self.ts_recent = ts.val;
            self.ts_recent_valid = true;
        }
        self.set_send_mss();
    }

    fn queue_rst_reply(&mut self, ips_in: IpPair, seg: &Segment<'_>) {
        // RFC 9293 §3.10.7.1: <SEQ=SEG.ACK><CTL=RST> if ACK is set, else
        // <SEQ=0><ACK=SEG.SEQ+SEG.LEN><CTL=RST,ACK>.
        let (seq, ack) = if seg.flags.contains(Flags::ACK) {
            (seg.ack, None)
        } else {
            (Seq(0), Some(seg.seq.add(seg.seq_len())))
        };
        self.pending_rst = Some(RstSpec {
            ips: ips_in.reversed(),
            src_port: seg.dst_port,
            dst_port: seg.src_port,
            seq,
            ack,
        });
    }

    fn challenge_ack(&mut self, now: u64) {
        if now.saturating_sub(self.challenge_window_ns) >= 1_000_000_000 {
            self.challenge_window_ns = now;
            self.challenge_count = 0;
        }
        if self.challenge_count < self.cfg.challenge_ack_limit {
            self.challenge_count += 1;
            self.stats.challenge_acks += 1;
            self.ack_now = true;
        } else {
            self.stats.challenge_acks_suppressed += 1;
        }
    }

    // ----------------------------------------------------------------
    // Segment arrival (RFC 9293 §3.10.7)
    // ----------------------------------------------------------------

    /// Process one incoming segment. `ips` is oriented as received (peer is
    /// `src`). `ring` must be the same caller-owned receive ring of
    /// `Config::recv_buffer` bytes on every call.
    pub fn on_segment<'a>(
        &mut self,
        now: u64,
        ips: IpPair,
        bytes: &'a [u8],
        ring: &'a mut [u8],
    ) -> Result<Received<'a>, TcpError> {
        if ring.len() != self.cfg.recv_buffer as usize {
            return Err(TcpError::RingSizeMismatch);
        }
        wire::verify_checksum(ips, bytes)?;
        let seg = Segment::parse(bytes)?;
        if seg.dst_port != self.cfg.local_port {
            return Err(TcpError::NotForThisConnection);
        }
        if !matches!(self.state, State::Closed | State::Listen)
            && (seg.src_port != self.remote_port || self.ips != Some(ips.reversed()))
        {
            return Err(TcpError::NotForThisConnection);
        }
        match self.state {
            State::Closed => {
                if !seg.flags.contains(Flags::RST) {
                    self.queue_rst_reply(ips, &seg);
                }
                Ok(Received::default())
            }
            State::Listen => {
                self.on_listen(now, ips, &seg);
                Ok(Received::default())
            }
            State::SynSent => self.on_syn_sent(now, ips, seg, ring),
            _ => self.on_synchronized(now, seg, ring),
        }
    }

    fn on_listen(&mut self, now: u64, ips: IpPair, seg: &Segment<'_>) {
        let f = seg.flags;
        if f.contains(Flags::RST) {
            return;
        }
        if f.contains(Flags::ACK) {
            self.queue_rst_reply(ips, seg);
            return;
        }
        if !f.contains(Flags::SYN) {
            return;
        }
        self.ips = Some(ips.reversed());
        self.remote_port = seg.src_port;
        self.rcv.isn = seg.seq;
        self.have_irs = true;
        self.rcv_nxt = 1;
        self.rcv_adv_right = 1;
        self.last_ack_sent = 1;
        self.snd.isn = self.clock_iss(now);
        self.negotiate(&seg.options, ips);
        // The window in a SYN is never scaled (RFC 7323 §2.2).
        self.snd_wnd = u32::from(seg.window);
        self.max_snd_wnd = self.snd_wnd;
        self.snd_wl1 = seg.seq;
        self.snd_wl2 = self.snd.seq(0);
        self.need_syn = true;
        self.state = State::SynReceived;
        // Data and FIN carried on the SYN are not acknowledged and will be
        // retransmitted by the peer (RFC 9293 permits queueing; we drop).
    }

    fn on_syn_sent<'a>(
        &mut self,
        now: u64,
        ips: IpPair,
        seg: Segment<'a>,
        ring: &'a mut [u8],
    ) -> Result<Received<'a>, TcpError> {
        let f = seg.flags;
        let mut acceptable_ack = false;
        if f.contains(Flags::ACK) {
            let iss = self.snd.seq(0);
            if seg.ack.le(iss) || seg.ack.gt(self.snd.seq(self.snd_nxt)) {
                if !f.contains(Flags::RST) {
                    self.queue_rst_reply(ips, &seg);
                }
                return Ok(Received::default());
            }
            acceptable_ack = true;
        }
        if f.contains(Flags::RST) {
            if acceptable_ack {
                self.enter_closed(Some(CloseReason::Refused));
            }
            return Ok(Received::default());
        }
        if !f.contains(Flags::SYN) {
            return Ok(Received::default());
        }
        self.rcv.isn = seg.seq;
        self.have_irs = true;
        self.rcv_nxt = 1;
        self.rcv_adv_right = 1;
        self.last_ack_sent = 1;
        self.negotiate(&seg.options, ips);
        self.snd_wnd = u32::from(seg.window);
        self.max_snd_wnd = self.snd_wnd;
        self.snd_wl1 = seg.seq;
        self.snd_wl2 = seg.ack;
        if acceptable_ack {
            let ack_abs = self.snd.abs(self.snd_una, seg.ack).unwrap_or(1);
            self.advance_una(now, ack_abs);
            self.enter_established();
            self.ack_now = true;
            // Data or FIN on the SYN-ACK continue at the sixth step.
            let mut rec = self.accept_text(now, &seg, ring);
            rec.fin |= self.process_fin(now, &seg);
            Ok(rec)
        } else {
            // Simultaneous open: resend our SYN as SYN,ACK.
            self.state = State::SynReceived;
            if !self.sent.is_empty() {
                self.sent.get_mut(0).lost = true;
                self.force_retx = true;
            }
            Ok(Received::default())
        }
    }

    fn on_synchronized<'a>(
        &mut self,
        now: u64,
        mut seg: Segment<'a>,
        ring: &'a mut [u8],
    ) -> Result<Received<'a>, TcpError> {
        let f = seg.flags;
        let is_rst = f.contains(Flags::RST);

        // RFC 7323 §3.2 and §5.3 (PAWS).
        if self.ts_ok && !is_rst {
            match seg.options.timestamps {
                None => {
                    self.stats.paws_dropped += 1;
                    return Ok(Received::default());
                }
                Some(ts) if self.ts_recent_valid && Seq(ts.val).lt(Seq(self.ts_recent)) => {
                    self.stats.paws_dropped += 1;
                    self.ack_now = true;
                    return Ok(Received::default());
                }
                _ => {}
            }
        }

        // Duplicate SYN while our SYN,ACK is unacknowledged: resend it.
        if self.state == State::SynReceived
            && f.contains(Flags::SYN)
            && !f.contains(Flags::ACK)
            && !is_rst
            && seg.seq == self.rcv.isn
        {
            if !self.sent.is_empty() {
                self.sent.get_mut(0).lost = true;
                self.force_retx = true;
            }
            return Ok(Received::default());
        }

        // First: sequence-number acceptability (RFC 9293 §3.10.7.4).
        let seg_len = seg.seq_len();
        let rcv_wnd = self.recv_window();
        let nxt = self.rcv.seq(self.rcv_nxt);
        let acceptable = match (seg_len, rcv_wnd) {
            (0, 0) => seg.seq == nxt,
            (0, w) => seg.seq.in_window(nxt, nxt.add(w)),
            (_, 0) => false,
            (l, w) => {
                seg.seq.in_window(nxt, nxt.add(w)) || seg.seq.add(l - 1).in_window(nxt, nxt.add(w))
            }
        };
        if !acceptable {
            if is_rst {
                return Ok(Received::default());
            }
            self.ack_now = true;
            if self.state == State::TimeWait && f.contains(Flags::FIN) {
                self.time_wait_deadline = Some(now.saturating_add(self.cfg.time_wait_ns));
            }
            // Zero window: still honour valid ACK/RST information.
            if rcv_wnd == 0 && seg.seq == nxt {
                seg.payload = &[];
                seg.flags = Flags(seg.flags.0 & !(Flags::FIN.0 | Flags::SYN.0));
            } else {
                return Ok(Received::default());
            }
        }

        // RFC 7323 §4.3: update TS.Recent.
        if let (true, Some(ts)) = (self.ts_ok, seg.options.timestamps) {
            let last = self.rcv.seq(self.last_ack_sent);
            if seg.seq.le(last) && (!self.ts_recent_valid || Seq(ts.val).ge(Seq(self.ts_recent))) {
                self.ts_recent = ts.val;
                self.ts_recent_valid = true;
            }
        }

        // Second: RST (RFC 5961 §3.2).
        if is_rst {
            if seg.seq == nxt {
                match self.state {
                    State::SynReceived if self.passive => {
                        let iss_base = self.iss_base;
                        self.reset_tcb();
                        self.iss_base = iss_base;
                        self.passive = true;
                        self.state = State::Listen;
                    }
                    State::SynReceived => self.enter_closed(Some(CloseReason::Refused)),
                    State::Established | State::FinWait1 | State::FinWait2 | State::CloseWait => {
                        self.enter_closed(Some(CloseReason::Reset));
                    }
                    _ => self.enter_closed(None),
                }
            } else {
                self.challenge_ack(now);
            }
            return Ok(Received::default());
        }

        // Fourth: SYN in a synchronized state (RFC 5961 §4.2).
        if seg.flags.contains(Flags::SYN) {
            self.challenge_ack(now);
            return Ok(Received::default());
        }

        // Fifth: ACK.
        if !f.contains(Flags::ACK) {
            return Ok(Received::default());
        }
        let una = self.snd.seq(self.snd_una);
        let snd_nxt = self.snd.seq(self.snd_nxt);
        if seg.ack.gt(snd_nxt) {
            if self.state == State::SynReceived {
                self.pending_rst = Some(RstSpec {
                    ips: self.ips.unwrap_or(IpPair::V4 {
                        src: [0; 4],
                        dst: [0; 4],
                    }),
                    src_port: self.cfg.local_port,
                    dst_port: self.remote_port,
                    seq: seg.ack,
                    ack: None,
                });
            } else {
                self.ack_now = true;
            }
            return Ok(Received::default());
        }
        if seg.ack.lt(una.add(0u32.wrapping_sub(self.max_snd_wnd))) {
            self.challenge_ack(now);
            return Ok(Received::default());
        }
        if self.state == State::SynReceived {
            if una.lt(seg.ack) && seg.ack.le(snd_nxt) {
                self.snd_wnd = u32::from(seg.window) << self.snd_wscale;
                self.max_snd_wnd = self.max_snd_wnd.max(self.snd_wnd);
                self.snd_wl1 = seg.seq;
                self.snd_wl2 = seg.ack;
                self.enter_established();
            } else {
                self.pending_rst = Some(RstSpec {
                    ips: self.ips.unwrap_or(IpPair::V4 {
                        src: [0; 4],
                        dst: [0; 4],
                    }),
                    src_port: self.cfg.local_port,
                    dst_port: self.remote_port,
                    seq: seg.ack,
                    ack: None,
                });
                return Ok(Received::default());
            }
        }
        if self.state != State::TimeWait {
            self.handle_ack(now, &seg);
        }
        match self.state {
            State::FinWait1 if self.fin_acked() => self.state = State::FinWait2,
            State::Closing => {
                if self.fin_acked() {
                    self.enter_time_wait(now);
                } else {
                    return Ok(Received::default());
                }
            }
            State::LastAck if self.fin_acked() => {
                self.enter_closed(None);
                return Ok(Received::default());
            }
            _ => {}
        }

        // Sixth: URG (recorded; data is delivered inline).
        let urgent_ok = f.contains(Flags::URG)
            && matches!(
                self.state,
                State::Established | State::FinWait1 | State::FinWait2
            );
        if let (true, Some(abs)) = (
            urgent_ok,
            self.rcv
                .abs(self.rcv_nxt, seg.seq.add(u32::from(seg.urgent))),
        ) {
            let off = abs.saturating_sub(1);
            self.urgent_off = Some(self.urgent_off.map_or(off, |u| u.max(off)));
        }

        // Seventh and eighth: text and FIN.
        let mut rec = self.accept_text(now, &seg, ring);
        rec.fin |= self.process_fin(now, &seg);
        if !rec.fin && self.ooo_fin == Some(self.rcv_nxt) {
            rec.fin = self.consume_fin(now);
        }
        Ok(rec)
    }

    /// Cumulative acknowledgment up to `ack_abs` (> snd_una).
    fn advance_una(&mut self, now: u64, ack_abs: u64) {
        if ack_abs <= self.snd_una {
            return;
        }
        let mut sample = None;
        while !self.sent.is_empty() {
            let front = *self.sent.get(0);
            if front.end <= ack_abs {
                if !front.sacked {
                    self.report_acked(now, &front, front.bytes());
                }
                if self.timed == Some(front.pn) {
                    if front.retransmits == 0 {
                        sample = Some(now.saturating_sub(front.sent_ns));
                    }
                    self.timed = None;
                }
                self.sent.pop_front();
            } else if front.start < ack_abs {
                // Partial acknowledgment of a segment.
                let part = (ack_abs - front.start) as u32;
                if !front.sacked {
                    self.report_acked(now, &front, part);
                }
                let f = self.sent.get_mut(0);
                f.start = ack_abs;
                break;
            } else {
                break;
            }
        }
        self.snd_una = ack_abs;
        if self.timed.is_some()
            && !(0..self.sent.len).any(|i| Some(self.sent.get(i).pn) == self.timed)
        {
            self.timed = None;
        }
        if let Some(r) = sample {
            self.rtt.sample(r);
            let s = RttSample {
                latest_ns: self.rtt.latest_ns(),
                min_ns: self.rtt.min_ns(),
                smoothed_ns: self.rtt.srtt_ns(),
                ack_delay_ns: 0,
            };
            self.cc.on_rtt_sample(s);
        }
        self.retries = 0;
        self.rto_deadline = if self.sent.is_empty() {
            None
        } else {
            Some(now.saturating_add(self.rtt.rto_ns()))
        };
    }

    fn report_acked(&mut self, now: u64, s: &SentSeg, bytes: u32) {
        let packet = AckedPacket {
            pn: s.pn,
            sent_ns: s.sent_ns,
            bytes,
        };
        let bytes_in_flight = self.pipe();
        self.cc.on_event(
            CongestionEvent::Acked {
                packet,
                bytes_in_flight,
            },
            now,
        );
    }

    fn mark_lost(&mut self, now: u64, i: usize) {
        let s = *self.sent.get(i);
        if s.lost || s.sacked {
            return;
        }
        let bytes_in_flight = self.pipe();
        self.sent.get_mut(i).lost = true;
        let packet = LostPacket {
            pn: s.pn,
            sent_ns: s.sent_ns,
            bytes: s.bytes(),
        };
        self.cc.on_event(
            CongestionEvent::Lost {
                packet,
                bytes_in_flight,
            },
            now,
        );
    }

    fn enter_recovery(&mut self) {
        if self.recovery_point.is_none() {
            self.recovery_point = Some(self.snd_nxt);
            self.stats.recoveries += 1;
            self.force_retx = true;
        }
    }

    fn handle_ack(&mut self, now: u64, seg: &Segment<'_>) {
        let ack_abs = match self.snd.abs(self.snd_una, seg.ack) {
            Some(a) => a.min(self.snd_nxt),
            None => return,
        };
        let new_wnd = u32::from(seg.window) << self.snd_wscale;
        let old_wnd = self.snd_wnd;
        let prev_una = self.snd_una;

        // Duplicate-ACK test (RFC 5681 §2) before the window update.
        let is_dup = ack_abs == prev_una
            && seg.payload.is_empty()
            && !seg.flags.contains(Flags::SYN)
            && !seg.flags.contains(Flags::FIN)
            && new_wnd == old_wnd
            && !self.sent.is_empty();

        // Window update (RFC 9293 §3.10.7.4, SND.WL1/WL2).
        if self.snd_wl1.lt(seg.seq) || (self.snd_wl1 == seg.seq && self.snd_wl2.le(seg.ack)) {
            self.snd_wnd = new_wnd;
            self.max_snd_wnd = self.max_snd_wnd.max(new_wnd);
            self.snd_wl1 = seg.seq;
            self.snd_wl2 = seg.ack;
            if old_wnd == 0 && new_wnd > 0 {
                self.persist_deadline = None;
                self.persist_backoff = 0;
                self.probe_due = false;
            }
        }

        if ack_abs > prev_una {
            self.advance_una(now, ack_abs);
            if let Some(rp) = self.recovery_point {
                if self.snd_una >= rp {
                    self.recovery_point = None;
                    self.dupacks = 0;
                    for i in 0..self.sent.len {
                        self.sent.get_mut(i).rexmit_in_recovery = false;
                    }
                } else if !self.sack_ok && !self.sent.is_empty() {
                    // RFC 6582 partial ACK: retransmit the next hole.
                    self.dupacks = 0;
                    self.mark_lost(now, 0);
                    self.force_retx = true;
                }
            } else {
                self.dupacks = 0;
            }
        } else if is_dup {
            self.dupacks = self.dupacks.saturating_add(1);
            self.stats.dup_acks += 1;
        }

        if self.sack_ok {
            self.apply_sack(now, seg.options.sack());
            self.scoreboard_losses(now);
        }
        if self.dupacks >= DUP_THRESH && self.recovery_point.is_none() && !self.sent.is_empty() {
            self.mark_lost(now, 0);
            self.enter_recovery();
        }
    }

    fn apply_sack(&mut self, now: u64, blocks: &[SackBlock]) {
        for b in blocks {
            let (Some(l), Some(r)) = (
                self.snd.abs(self.snd_una, b.left),
                self.snd.abs(self.snd_una, b.right),
            ) else {
                continue;
            };
            // Ignore blocks outside SND.UNA..SND.NXT (incl. D-SACK).
            if l <= self.snd_una || r > self.snd_nxt || l >= r {
                continue;
            }
            for i in 0..self.sent.len {
                let s = *self.sent.get(i);
                if s.start >= r {
                    break;
                }
                if s.start >= l && s.end <= r && !s.sacked {
                    let e = self.sent.get_mut(i);
                    e.sacked = true;
                    e.lost = false;
                    self.report_acked(now, &s, s.bytes());
                }
            }
        }
    }

    /// RFC 6675 IsLost over the scoreboard (single reverse pass).
    fn scoreboard_losses(&mut self, now: u64) {
        let mut sacked_count = 0u32;
        let mut sacked_bytes = 0u64;
        let mut any = false;
        let limit = u64::from(DUP_THRESH - 1) * u64::from(self.smss);
        for i in (0..self.sent.len).rev() {
            let s = *self.sent.get(i);
            if s.sacked {
                sacked_count += 1;
                sacked_bytes += u64::from(s.bytes());
                continue;
            }
            if !s.lost
                && !s.rexmit_in_recovery
                && (sacked_count >= DUP_THRESH || sacked_bytes > limit)
            {
                self.mark_lost(now, i);
                any = true;
            }
        }
        if any {
            self.enter_recovery();
        }
    }

    /// Seventh step: accept segment text into the stream.
    fn accept_text<'a>(&mut self, now: u64, seg: &Segment<'a>, ring: &'a mut [u8]) -> Received<'a> {
        let mut rec = Received::default();
        if !matches!(
            self.state,
            State::Established | State::FinWait1 | State::FinWait2
        ) || seg.payload.is_empty()
        {
            return rec;
        }
        let Some(mut start) = self.rcv.abs(self.rcv_nxt, seg.seq) else {
            return rec;
        };
        if seg.flags.contains(Flags::SYN) {
            start += 1;
        }
        let mut payload = seg.payload;
        let right = self.recv_right();
        if start < self.rcv_nxt {
            let skip = self.rcv_nxt - start;
            if skip >= payload.len() as u64 {
                self.ack_now = true;
                return rec;
            }
            payload = &payload[skip as usize..];
            start = self.rcv_nxt;
        }
        if start >= right {
            self.ack_now = true;
            return rec;
        }
        let max = (right - start).min(payload.len() as u64) as usize;
        payload = &payload[..max];
        let end = start + payload.len() as u64;
        let cap = u64::from(self.cfg.recv_buffer);

        if start == self.rcv_nxt {
            rec.chunks[0] = payload;
            self.rcv_nxt = end;
            let had_ooo = !self.ooo.is_empty();
            // Pull any buffered range that now continues the stream.
            let mut pull: Option<u64> = None;
            let mut i = 0;
            while i < self.ooo.len() {
                let r = self.ooo[i];
                if r.end <= self.rcv_nxt {
                    self.ooo.swap_remove(i);
                    continue;
                }
                if r.start <= self.rcv_nxt {
                    pull = Some(r.end);
                    self.ooo.swap_remove(i);
                    continue;
                }
                i += 1;
            }
            let ring: &'a [u8] = ring;
            if let Some(pend) = pull {
                let from = self.rcv_nxt;
                let p0 = ((from - 1) % cap) as usize;
                let len = (pend - from) as usize;
                let first = len.min(ring.len() - p0);
                rec.chunks[1] = &ring[p0..p0 + first];
                rec.chunks[2] = &ring[..len - first];
                self.rcv_nxt = pend;
            }
            if self.ooo_fin.is_some_and(|f| f < self.rcv_nxt) {
                // Data beyond a recorded FIN is a protocol violation; the
                // FIN position is discarded rather than trusted.
                self.ooo_fin = None;
            }
            if had_ooo {
                self.ack_now = true;
            } else {
                self.schedule_ack(now);
            }
        } else {
            // Out of order: copy into the ring at its stream position.
            if !self.insert_ooo(start, end) {
                self.stats.ooo_dropped += 1;
            } else {
                let p0 = ((start - 1) % cap) as usize;
                let first = payload.len().min(ring.len() - p0);
                ring[p0..p0 + first].copy_from_slice(&payload[..first]);
                let rest = payload.len() - first;
                ring[..rest].copy_from_slice(&payload[first..]);
            }
            self.ack_now = true;
        }
        rec
    }

    fn schedule_ack(&mut self, now: u64) {
        self.ack_pending += 1;
        if self.cfg.delayed_ack_ns == 0 || self.ack_pending >= 2 {
            self.ack_now = true;
        } else if self.delack_deadline.is_none() {
            self.delack_deadline = Some(now.saturating_add(self.cfg.delayed_ack_ns));
        }
    }

    /// Insert `[start, end)` into the out-of-order table, merging. Returns
    /// false when the table is full and the range touches no existing one.
    fn insert_ooo(&mut self, start: u64, end: u64) -> bool {
        self.ooo_stamp += 1;
        let stamp = self.ooo_stamp;
        let mut ns = start;
        let mut ne = end;
        let mut merged = false;
        let mut i = 0;
        while i < self.ooo.len() {
            let r = self.ooo[i];
            if r.start <= ne && ns <= r.end {
                ns = ns.min(r.start);
                ne = ne.max(r.end);
                self.ooo.swap_remove(i);
                merged = true;
                continue;
            }
            i += 1;
        }
        if !merged && self.ooo.len() >= self.cfg.max_ooo_ranges as usize {
            return false;
        }
        // Capacity is reserved at construction; this push never allocates.
        self.ooo.push(OooRange {
            start: ns,
            end: ne,
            stamp,
        });
        true
    }

    /// Eighth step: FIN. Returns whether a FIN was consumed.
    fn process_fin(&mut self, now: u64, seg: &Segment<'_>) -> bool {
        if !seg.flags.contains(Flags::FIN) || !self.state.is_synchronized() {
            return false;
        }
        if !matches!(
            self.state,
            State::Established | State::FinWait1 | State::FinWait2 | State::SynReceived
        ) {
            // CLOSE-WAIT, CLOSING, LAST-ACK: remain; TIME-WAIT: restart.
            if self.state == State::TimeWait {
                self.time_wait_deadline = Some(now.saturating_add(self.cfg.time_wait_ns));
            }
            self.ack_now = true;
            return false;
        }
        let Some(mut seg_start) = self.rcv.abs(self.rcv_nxt, seg.seq) else {
            return false;
        };
        if seg.flags.contains(Flags::SYN) {
            seg_start += 1;
        }
        let fin_pos = seg_start + seg.payload.len() as u64;
        self.ack_now = true;
        if fin_pos > self.rcv_nxt {
            if fin_pos <= self.recv_right() {
                self.ooo_fin = Some(fin_pos);
            }
            return false;
        }
        if fin_pos < self.rcv_nxt {
            return false;
        }
        self.consume_fin(now)
    }

    fn consume_fin(&mut self, now: u64) -> bool {
        self.rcv_nxt += 1;
        self.fin_received = true;
        self.ooo_fin = None;
        self.events |= EV_PEER_FIN;
        self.ack_now = true;
        self.delack_deadline = None;
        match self.state {
            State::SynReceived | State::Established => self.state = State::CloseWait,
            State::FinWait1 => {
                if self.fin_acked() {
                    self.enter_time_wait(now);
                } else {
                    self.state = State::Closing;
                }
            }
            State::FinWait2 => self.enter_time_wait(now),
            _ => {}
        }
        true
    }

    // ----------------------------------------------------------------
    // Timers
    // ----------------------------------------------------------------

    /// Fire every timer whose deadline is at or before `now`.
    pub fn on_timeout(&mut self, now: u64) {
        if self.delack_deadline.is_some_and(|d| d <= now) {
            self.delack_deadline = None;
            self.ack_now = true;
        }
        if self.time_wait_deadline.is_some_and(|d| d <= now) {
            self.time_wait_deadline = None;
            if self.state == State::TimeWait {
                self.enter_closed(None);
            }
            return;
        }
        if self.persist_deadline.is_some_and(|d| d <= now) {
            self.persist_deadline = None;
            self.probe_due = true;
        }
        if self.rto_deadline.is_some_and(|d| d <= now) {
            self.on_rto(now);
        }
    }

    fn on_rto(&mut self, now: u64) {
        self.rto_deadline = None;
        if self.sent.is_empty() {
            return;
        }
        self.stats.rtos += 1;
        let probing = self.snd_wnd == 0 && self.state.is_synchronized();
        if !probing {
            self.retries += 1;
            if self.retries > self.cfg.max_retransmits {
                self.enter_closed(Some(CloseReason::TimedOut));
                return;
            }
            self.cc.on_event(CongestionEvent::PersistentCongestion, now);
        }
        // RFC 2018 §8: forget SACK state after a timeout.
        for i in 0..self.sent.len {
            let s = self.sent.get_mut(i);
            s.sacked = false;
            s.lost = true;
            s.rexmit_in_recovery = false;
        }
        self.recovery_point = Some(self.snd_nxt);
        self.dupacks = 0;
        self.timed = None;
        self.force_retx = true;
        self.rtt.backoff();
        self.rto_deadline = Some(now.saturating_add(self.rtt.rto_ns()));
    }

    // ----------------------------------------------------------------
    // Transmission
    // ----------------------------------------------------------------

    /// Write the next segment to send into `out`, if any. Call repeatedly
    /// until it returns `None`.
    pub fn poll_transmit(
        &mut self,
        now: u64,
        send: &SendView<'_>,
        out: &mut [u8],
    ) -> Result<Option<Transmit>, TcpError> {
        if out.len() < MAX_HEADER_LEN {
            return Err(TcpError::BufferTooSmall);
        }
        if let Some(rst) = self.pending_rst.take() {
            let h = HeaderFields {
                src_port: rst.src_port,
                dst_port: rst.dst_port,
                seq: rst.seq,
                ack: rst.ack.unwrap_or(Seq(0)),
                flags: if rst.ack.is_some() {
                    Flags::RST | Flags::ACK
                } else {
                    Flags::RST
                },
                window: 0,
                urgent: 0,
            };
            let n = wire::write_header(out, &h, &TcpOptions::default())?;
            wire::fill_checksum(rst.ips, &mut out[..n])?;
            self.stats.segments_sent += 1;
            return Ok(Some(Transmit {
                len: n,
                edt_ns: now,
                ips: rst.ips,
            }));
        }
        if matches!(self.state, State::Closed | State::Listen) {
            return Ok(None);
        }
        if self.need_syn {
            if !self.sent.push(SentSeg {
                start: 0,
                end: 1,
                syn: true,
                ..SentSeg::default()
            }) {
                return Ok(None);
            }
            self.need_syn = false;
            self.snd_nxt = self.snd_nxt.max(1);
            return self.emit_entry(now, 0, send, out, false).map(Some);
        }
        if self.state != State::TimeWait {
            if let Some(t) = self.try_retransmit(now, send, out)? {
                return Ok(Some(t));
            }
            if let Some(t) = self.try_new_data(now, send, out)? {
                return Ok(Some(t));
            }
        }
        if self.ack_now && self.have_irs && self.state != State::SynSent {
            return self.emit_ack(now, out).map(Some);
        }
        Ok(None)
    }

    fn try_retransmit(
        &mut self,
        now: u64,
        send: &SendView<'_>,
        out: &mut [u8],
    ) -> Result<Option<Transmit>, TcpError> {
        let Some(i) = (0..self.sent.len).find(|&i| {
            let s = self.sent.get(i);
            s.lost && !s.sacked
        }) else {
            self.force_retx = false;
            return Ok(None);
        };
        let cwnd = self.cc.cwnd_bytes();
        if !(self.force_retx || self.pipe() < cwnd || self.sent.get(i).syn) {
            return Ok(None);
        }
        self.force_retx = false;
        let t = self.emit_entry(now, i, send, out, true)?;
        Ok(Some(t))
    }

    fn try_new_data(
        &mut self,
        now: u64,
        send: &SendView<'_>,
        out: &mut [u8],
    ) -> Result<Option<Transmit>, TcpError> {
        // Data may flow once our SYN is acknowledged.
        if !matches!(
            self.state,
            State::Established
                | State::FinWait1
                | State::Closing
                | State::CloseWait
                | State::LastAck
        ) {
            return Ok(None);
        }
        if self.fin_abs.is_some() || self.sent.is_full() {
            return Ok(None);
        }
        let data_end = 1 + self.write_end;
        let unsent = data_end.saturating_sub(self.snd_nxt);
        if unsent == 0 {
            if self.fin_requested {
                return self.emit_new(now, 0, true, send, out).map(Some);
            }
            return Ok(None);
        }
        let wnd_right = self.snd_una + u64::from(self.snd_wnd);
        let usable = wnd_right.saturating_sub(self.snd_nxt);
        let pipe = self.pipe();
        let cwnd_room = u64::from(self.cc.cwnd_bytes().saturating_sub(pipe));
        let smss = u64::from(self.smss);

        if usable == 0 {
            // Zero window (or window fully used): RFC 9293 §3.8.6.1 probing.
            if self.snd_wnd == 0 && self.sent.is_empty() {
                if self.probe_due {
                    self.probe_due = false;
                    self.stats.window_probes += 1;
                    return self.emit_new(now, 1, false, send, out).map(Some);
                }
                self.arm_persist(now);
            }
            return Ok(None);
        }
        let len = unsent.min(smss).min(usable).min(cwnd_room);
        if len == 0 {
            return Ok(None);
        }
        // Sender SWS avoidance and Nagle (RFC 9293 §3.8.6.2.1).
        let full = len == smss;
        let all = len == unsent && (!self.cfg.nagle || self.sent.is_empty());
        let half_window = self.max_snd_wnd > 0 && len >= u64::from(self.max_snd_wnd / 2);
        if !(full || all || half_window) {
            // Nothing in flight will clock this out: RFC 9293 §3.8.6.2.1
            // (d) override timeout, driven by the persist timer.
            if self.sent.is_empty() {
                if self.probe_due {
                    self.probe_due = false;
                } else {
                    self.arm_persist(now);
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
        }
        let with_fin = self.fin_requested && self.snd_nxt + len == data_end;
        self.emit_new(now, len as u32, with_fin, send, out)
            .map(Some)
    }

    fn arm_persist(&mut self, now: u64) {
        if self.persist_deadline.is_none() {
            let backoff = self
                .rtt
                .rto_ns()
                .checked_shl(self.persist_backoff.min(16))
                .unwrap_or(u64::MAX);
            self.persist_backoff = self.persist_backoff.saturating_add(1);
            self.persist_deadline = Some(now.saturating_add(backoff.min(self.cfg.rto.max_ns)));
        }
    }

    fn emit_new(
        &mut self,
        now: u64,
        len: u32,
        fin: bool,
        send: &SendView<'_>,
        out: &mut [u8],
    ) -> Result<Transmit, TcpError> {
        let start = self.snd_nxt;
        let end = start + u64::from(len) + u64::from(fin);
        let entry = SentSeg {
            start,
            end,
            fin,
            ..SentSeg::default()
        };
        // Validate before committing state.
        self.check_view(&entry, send, out)?;
        if !self.sent.push(entry) {
            return Err(TcpError::Overflow);
        }
        self.snd_nxt = end;
        if fin {
            self.fin_abs = Some(end - 1);
        }
        let i = self.sent.len - 1;
        self.emit_entry(now, i, send, out, false)
    }

    fn check_view(&self, s: &SentSeg, send: &SendView<'_>, out: &[u8]) -> Result<(), TcpError> {
        let (ds, de) = s.data_range();
        if de > ds {
            let (start, end) = (ds - 1, de - 1);
            let view_end = send.base.saturating_add(send.bytes.len() as u64);
            if start < send.base || end > view_end {
                return Err(TcpError::SendViewMissing { start, end });
            }
        }
        if out.len() < MAX_HEADER_LEN + (de - ds) as usize {
            return Err(TcpError::BufferTooSmall);
        }
        Ok(())
    }

    fn base_options(&self, now: u64, syn: bool) -> TcpOptions {
        let mut o = TcpOptions::default();
        let active_syn = self.state == State::SynSent;
        if syn {
            o.mss = Some(self.cfg.mss);
            o.window_scale = if active_syn {
                self.cfg.window_scale
            } else if self.ws_ok {
                Some(self.rcv_wscale)
            } else {
                None
            };
            o.sack_permitted = if active_syn {
                self.cfg.sack
            } else {
                self.sack_ok
            };
        }
        if (syn && active_syn && self.cfg.timestamps) || self.ts_ok {
            o.timestamps = Some(Timestamps {
                val: Self::ts_now(now),
                ecr: if self.ts_ok { self.ts_recent } else { 0 },
            });
        }
        o
    }

    fn fill_sack(&self, o: &mut TcpOptions, room: usize) {
        if !self.sack_ok || self.ooo.is_empty() {
            return;
        }
        let space = (MAX_OPTIONS_LEN - o.encoded_len()).min(room);
        if space < 12 {
            return;
        }
        let max_blocks = ((space - 4) / 8).min(MAX_SACK_BLOCKS);
        // Most recently updated ranges first (RFC 2018 §4).
        let mut last_stamp = u64::MAX;
        let mut n = 0;
        while n < max_blocks {
            let mut best: Option<OooRange> = None;
            for r in &self.ooo {
                if r.stamp < last_stamp && best.is_none_or(|b| r.stamp > b.stamp) {
                    best = Some(*r);
                }
            }
            let Some(b) = best else { break };
            last_stamp = b.stamp;
            o.sack_blocks[n] = SackBlock {
                left: self.rcv.seq(b.start),
                right: self.rcv.seq(b.end),
            };
            n += 1;
        }
        o.sack_len = n as u8;
    }

    fn advertised_window(&mut self, syn: bool) -> u16 {
        let right = self.recv_right();
        let adv_right = if right.saturating_sub(self.rcv_adv_right) >= self.sws_threshold() {
            right
        } else {
            self.rcv_adv_right.max(self.rcv_nxt)
        };
        let wnd = adv_right.saturating_sub(self.rcv_nxt);
        if syn {
            let w = wnd.min(65535);
            self.rcv_adv_right = self.rcv_adv_right.max(self.rcv_nxt + w);
            return w as u16;
        }
        let scaled = (wnd >> self.rcv_wscale).min(65535);
        self.rcv_adv_right = self
            .rcv_adv_right
            .max(self.rcv_nxt + (scaled << self.rcv_wscale));
        scaled as u16
    }

    fn after_ack_sent(&mut self) {
        self.ack_now = false;
        self.ack_pending = 0;
        self.delack_deadline = None;
        self.last_ack_sent = self.rcv_nxt;
    }

    fn emit_ack(&mut self, now: u64, out: &mut [u8]) -> Result<Transmit, TcpError> {
        let ips = self.ips.ok_or(TcpError::InvalidState(self.state))?;
        let mut o = self.base_options(now, false);
        self.fill_sack(&mut o, MAX_OPTIONS_LEN);
        let window = self.advertised_window(false);
        let h = HeaderFields {
            src_port: self.cfg.local_port,
            dst_port: self.remote_port,
            seq: self.snd.seq(self.snd_nxt),
            ack: self.rcv.seq(self.rcv_nxt),
            flags: Flags::ACK,
            window,
            urgent: 0,
        };
        let n = wire::write_header(out, &h, &o)?;
        wire::fill_checksum(ips, &mut out[..n])?;
        self.after_ack_sent();
        self.stats.segments_sent += 1;
        Ok(Transmit {
            len: n,
            edt_ns: now,
            ips,
        })
    }

    fn emit_entry(
        &mut self,
        now: u64,
        i: usize,
        send: &SendView<'_>,
        out: &mut [u8],
        retransmit: bool,
    ) -> Result<Transmit, TcpError> {
        let ips = self.ips.ok_or(TcpError::InvalidState(self.state))?;
        let s = *self.sent.get(i);
        self.check_view(&s, send, out)?;
        let (ds, de) = s.data_range();
        let payload_len = (de - ds) as usize;
        let mut o = self.base_options(now, s.syn);
        if !s.syn {
            let total = u32::from(self.peer_mss.max(MIN_SEND_MSS).min(self.cfg.mss)) as usize;
            let used = payload_len + o.encoded_len();
            self.fill_sack(&mut o, total.saturating_sub(used));
        }
        let with_ack = self.have_irs;
        let window = if with_ack || s.syn {
            self.advertised_window(s.syn)
        } else {
            0
        };
        let mut flags = if s.syn { Flags::SYN } else { Flags(0) };
        if with_ack {
            flags = flags | Flags::ACK;
        }
        if s.fin {
            flags = flags | Flags::FIN;
        }
        if payload_len > 0 && de - 1 == self.write_end {
            flags = flags | Flags::PSH;
        }
        let h = HeaderFields {
            src_port: self.cfg.local_port,
            dst_port: self.remote_port,
            seq: self.snd.seq(s.start),
            ack: if with_ack {
                self.rcv.seq(self.rcv_nxt)
            } else {
                Seq(0)
            },
            flags,
            window,
            urgent: 0,
        };
        let hl = wire::write_header(out, &h, &o)?;
        if payload_len > 0 {
            let off = (ds - 1 - send.base) as usize;
            out[hl..hl + payload_len].copy_from_slice(&send.bytes[off..off + payload_len]);
        }
        let n = hl + payload_len;
        wire::fill_checksum(ips, &mut out[..n])?;

        let pn = self.next_pn;
        self.next_pn += 1;
        let e = self.sent.get_mut(i);
        e.pn = pn;
        e.sent_ns = now;
        e.lost = false;
        if retransmit {
            e.retransmits = e.retransmits.saturating_add(1);
            if self.recovery_point.is_some() {
                e.rexmit_in_recovery = true;
            }
            self.stats.retransmits += 1;
            self.stats.retransmitted_bytes += payload_len as u64;
        } else if self.timed.is_none() && !s.syn {
            self.timed = Some(pn);
        }
        if s.syn && !retransmit && self.timed.is_none() {
            self.timed = Some(pn);
        }
        let bytes = s.bytes();
        self.cc.on_sent(SentPacket {
            pn,
            sent_ns: now,
            bytes,
            ack_eliciting: true,
            in_flight: true,
        });
        if self.rto_deadline.is_none() {
            self.rto_deadline = Some(now.saturating_add(self.rtt.rto_ns()));
        }
        if with_ack {
            self.after_ack_sent();
        }
        self.stats.segments_sent += 1;
        let edt_ns = self.cc.schedule_edt_ns(bytes, now);
        Ok(Transmit {
            len: n,
            edt_ns,
            ips,
        })
    }
}
