//! Session channels over Vesper Warp QUIC streams (design §25.2 on §35).
//!
//! [`WarpNet`] places `nodes` simulated hosts on lane 3's software NIC
//! (`SoftNic`) and joins every pair of hosts with one Warp QUIC connection
//! (two [`WarpEndpoint`]s, reliable RFC 9000 streams, RFC 9002 recovery,
//! BBRv3, 1-RTT packet protection). The wire between the two NICs is a
//! seeded lossy link: each datagram can be dropped, held back behind newer
//! ones (reordering) or duplicated, and a link can be partitioned.
//!
//! A [`WarpTransport`] is the [`Transport`] of one session endpoint. Every
//! `(channel, from-role, to-role)` direction of a session is one
//! unidirectional QUIC stream, opened by the sending host on the connection
//! to the receiving host. The stream starts with a 16-byte header naming
//! the channel and the two roles, followed by length-delimited records: a
//! session frame, or an abandon marker. Closing a session at `End` sends
//! FIN on its streams; dropping or faulting it sends the abandon marker and
//! FIN, which the peer's endpoint observes as `NetFail::PeerGone`. The
//! typestate and monitor checks are exactly those of the in-memory
//! channels: [`Session::bind`](crate::session::Session::bind) checks the
//! protocol type against the projection, and every received frame is checked
//! again by the receiving endpoint's monitor.
//!
//! Untrusted bytes: a stream whose header or records are malformed
//! (unknown tag, oversized label, payload type or body, invalid UTF-8, bad
//! magic) is stopped with `STOP_SENDING` and counted; if its header was
//! valid, receives on that channel direction then fail with
//! [`NetFail::Malformed`]. Nothing in this module panics on received bytes.
//!
//! Blocking receives drive the simulated network forward (one
//! [`WarpNet::step`] per wait) up to [`WarpNetConfig::max_wait_steps`] and
//! then fail with [`NetFail::Timeout`]. Non-blocking drivers call
//! [`WarpNet::step`] themselves.
//!
//! What this is not: keys come from fixed per-link secrets
//! ([`WarpEndpoint::open_connection`], "keys from a TLS stack") rather than
//! from Warp's TLS 1.3 handshake, there is no real NIC or kernel, and the
//! QUIC stack is exercised only on this software link.

use crate::chor::RoleId;
use crate::session::{Frame, NetFail, Transport};
use crate::simnet::SplitMix;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::mpsc;
use vesper_warp::endpoint::{Discard, EndpointConfig, RxOutcome, TxTap, WarpEndpoint};
use vesper_warp::stream::{Role, StreamConfig, StreamEvent};
use vesper_warp_crypto::{Cipher, OneRttKeys, TrafficSecret};
use vesper_warp_shard::SlotHandle;
use vesper_warp_umem::{SoftNic, SoftNicMode, UmemConfig};

/// Stream header magic: "Vesper Chorus Session", version 1.
pub const STREAM_MAGIC: [u8; 4] = *b"VCS1";
/// Header length: magic, channel (u64), from role (u16), to role (u16).
pub const STREAM_HEADER_LEN: usize = 16;
const TAG_FRAME: u8 = 1;
const TAG_ABANDON: u8 = 2;
/// Longest label and payload-type name on the wire.
pub const MAX_NAME_LEN: usize = 64;
/// STOP_SENDING / RESET_STREAM application error code for malformed input.
pub const ERR_MALFORMED: u64 = 0x5643_0001;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WarpNetConfig {
    /// Hosts, `2..=16`.
    pub nodes: u32,
    pub seed: u64,
    /// Datagram loss probability, per million.
    pub loss_ppm: u32,
    /// Probability that a datagram is held back one step, per million.
    pub reorder_ppm: u32,
    /// Probability that a datagram is delivered twice, per million.
    pub duplicate_ppm: u32,
    /// Virtual time one [`WarpNet::step`] advances (the one-way wire delay).
    pub step_ns: u64,
    /// Steps a blocking receive waits before `NetFail::Timeout`.
    pub max_wait_steps: u32,
    /// Concurrent unidirectional streams each side of a connection may
    /// open; further session directions wait for a stream to retire.
    pub max_streams_uni: u64,
    /// Largest frame body accepted from the wire.
    pub max_body: u32,
}

impl Default for WarpNetConfig {
    fn default() -> Self {
        Self {
            nodes: 2,
            seed: 1,
            loss_ppm: 0,
            reorder_ppm: 0,
            duplicate_ppm: 0,
            step_ns: 1_000_000,
            max_wait_steps: 20_000,
            max_streams_uni: 64,
            max_body: 64 * 1024,
        }
    }
}

/// Counters of one network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WarpNetStats {
    pub steps: u64,
    pub datagrams: u64,
    pub dropped: u64,
    pub partitioned: u64,
    pub reordered: u64,
    pub duplicated: u64,
    /// Datagrams the receiving NIC had no buffer or ring slot for.
    pub nic_dropped: u64,
    pub frames_sent: u64,
    pub frames_delivered: u64,
    pub local_frames: u64,
    pub streams_opened: u64,
    pub streams_finished: u64,
    pub streams_received: u64,
    pub abandons_sent: u64,
    pub abandons_received: u64,
    pub malformed_streams: u64,
    /// Stream bytes the QUIC stack retransmitted (sum over endpoints).
    pub stream_bytes_retransmitted: u64,
    /// Stream events the QUIC stack dropped (must stay 0).
    pub events_dropped: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WarpNetError {
    Config(&'static str),
    /// A Warp endpoint failed to build.
    Warp(String),
}

impl std::fmt::Display for WarpNetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(m) => write!(f, "invalid WarpNet configuration: {m}"),
            Self::Warp(m) => write!(f, "Warp endpoint: {m}"),
        }
    }
}

impl std::error::Error for WarpNetError {}

/// Datagrams captured from one endpoint's transmit path.
#[derive(Default)]
struct Tap {
    out: VecDeque<Vec<u8>>,
}

impl TxTap for Tap {
    fn on_transmit(&mut self, _conn: SlotHandle, _pn: u64, _edt_ns: u64, datagram: &[u8]) {
        self.out.push_back(datagram.to_vec());
    }
}

type Ep = WarpEndpoint<SoftNic, Discard, Tap>;

/// Key of one session direction.
type DirKey = (u64, RoleId, RoleId);

#[derive(Default)]
struct SendDir {
    id: Option<u64>,
    pending: VecDeque<u8>,
    fin: bool,
    /// The peer stopped the stream; further data is discarded.
    dead: bool,
}

struct RecvDir {
    buf: Vec<u8>,
    header: Option<DirKey>,
    /// Stopped after malformed input: further bytes are discarded.
    stopped: bool,
}

/// One side (host) of a link: its endpoint and the streams it carries.
struct Side {
    node: u32,
    /// The host at the other end of this link: every frame read here was
    /// sent by it.
    peer: u32,
    ep: Ep,
    conn: SlotHandle,
    send: BTreeMap<DirKey, SendDir>,
    /// Directions waiting for a stream (stream limit reached), in order.
    waiting: VecDeque<DirKey>,
    recv: BTreeMap<u64, RecvDir>,
    /// Next peer-initiated unidirectional stream index to look for.
    next_remote_uni: u64,
    peer_uni_base: u64,
}

struct Link {
    sides: [Side; 2],
    /// Datagrams held back one step, per direction (0 → 1, 1 → 0).
    held: [Vec<Vec<u8>>; 2],
    blocked: [bool; 2],
    failed: Option<String>,
}

#[derive(Default)]
struct Mail {
    /// `(node, source host, direction)` → frames received at `node` from
    /// `source host`. The source is the link the frame arrived on, so a
    /// receiver only accepts frames from the host its placement names.
    frames: HashMap<(u32, u32, DirKey), VecDeque<Frame>>,
    /// `(node, source host, channel, role)`: that role abandoned the
    /// channel, as reported to `node` by `source host`.
    abandoned: BTreeSet<(u32, u32, u64, RoleId)>,
    /// `(node, source host, direction)` whose stream was malformed.
    malformed: BTreeSet<(u32, u32, DirKey)>,
}

struct Core {
    cfg: WarpNetConfig,
    now: u64,
    rng: SplitMix,
    links: Vec<Link>,
    /// `(lo, hi)` node pair → link index.
    index: BTreeMap<(u32, u32), usize>,
    mail: Mail,
    stats: WarpNetStats,
    scratch: Vec<u8>,
}

type Job = Box<dyn FnOnce(&mut Core) + std::marker::Send>;

/// A simulated network of hosts joined pairwise by Warp QUIC connections.
///
/// Warp endpoints are single-threaded (they are not `Send`), while session
/// transports must be. The network therefore lives on one dedicated thread
/// that owns every endpoint; [`WarpNet`] handles and [`WarpTransport`]s send
/// it requests and wait for the answer. Requests from one driving thread are
/// served in order, so a single-threaded driver is deterministic for a seed.
/// The thread exits when the last handle is dropped.
#[derive(Clone)]
pub struct WarpNet {
    tx: mpsc::Sender<Job>,
    nodes: u32,
}

fn secret(seed: u64, link: usize, side: u8) -> Result<TrafficSecret, WarpNetError> {
    let mut rng = SplitMix(seed ^ (link as u64).wrapping_mul(0x9e37) ^ (u64::from(side) << 56));
    let mut bytes = [0u8; 32];
    for chunk in bytes.chunks_mut(8) {
        chunk.copy_from_slice(&rng.next_u64().to_le_bytes());
    }
    TrafficSecret::new(Cipher::Aes128Gcm, &bytes).map_err(|e| WarpNetError::Warp(format!("{e:?}")))
}

fn validate(cfg: &WarpNetConfig) -> Result<(), WarpNetError> {
    if !(2..=16).contains(&cfg.nodes) {
        return Err(WarpNetError::Config("nodes must be in 2..=16"));
    }
    if cfg.loss_ppm >= 1_000_000 || cfg.reorder_ppm > 1_000_000 || cfg.duplicate_ppm > 1_000_000 {
        return Err(WarpNetError::Config(
            "probabilities must be below one million",
        ));
    }
    if cfg.step_ns == 0 || cfg.max_wait_steps == 0 {
        return Err(WarpNetError::Config(
            "step_ns and max_wait_steps must be positive",
        ));
    }
    if !(1..=1024).contains(&cfg.max_streams_uni) {
        return Err(WarpNetError::Config("max_streams_uni must be in 1..=1024"));
    }
    if cfg.max_body == 0 || cfg.max_body > 1 << 24 {
        return Err(WarpNetError::Config("max_body must be in 1..=16 MiB"));
    }
    Ok(())
}

fn build_core(cfg: WarpNetConfig) -> Result<Core, WarpNetError> {
    let streams = StreamConfig {
        max_streams_bidi: 0,
        max_streams_uni: cfg.max_streams_uni,
        event_capacity: 4 * cfg.max_streams_uni as usize + 64,
        ..StreamConfig::default()
    };
    let base = EndpointConfig {
        streams,
        initial_rtt_ns: 4 * cfg.step_ns,
        max_connections: 4,
        // Virtual time races ahead while a blocked receiver steps the
        // network; idleness is not a failure in this simulation.
        idle_timeout_ns: u64::MAX / 4,
        ..EndpointConfig::default()
    };
    let warp = |e: vesper_warp::endpoint::WarpError| WarpNetError::Warp(e.to_string());
    let mut links = Vec::new();
    let mut index = BTreeMap::new();
    for a in 0..cfg.nodes {
        for b in a + 1..cfg.nodes {
            let li = links.len();
            let mk = |role: Role| -> Result<Ep, WarpNetError> {
                let xsk = SoftNic::xsk(UmemConfig::with_frames(256), SoftNicMode::Scripted)
                    .map_err(|e| WarpNetError::Warp(format!("{e:?}")))?;
                WarpEndpoint::new(
                    EndpointConfig { role, ..base },
                    xsk,
                    Discard::default(),
                    Tap::default(),
                )
                .map_err(warp)
            };
            let mut client = mk(Role::Client)?;
            let mut server = mk(Role::Server)?;
            let ccid = [b'c', b'h', b'o', b'r', 0, a as u8, b as u8, 1];
            let scid = [b'c', b'h', b'o', b'r', 0, a as u8, b as u8, 2];
            let keys = |l, r| -> Result<OneRttKeys, WarpNetError> {
                OneRttKeys::new(
                    secret(cfg.seed, li, l)?,
                    secret(cfg.seed, li, r)?,
                    100_000_000,
                )
                .map_err(|e| WarpNetError::Warp(format!("{e:?}")))
            };
            let c_conn = client
                .open_connection(&ccid, &scid, keys(0, 1)?, 0)
                .map_err(warp)?;
            let s_conn = server
                .open_connection(&scid, &ccid, keys(1, 0)?, 0)
                .map_err(warp)?;
            let side = |node, peer, ep, conn, peer_uni_base| Side {
                node,
                peer,
                ep,
                conn,
                send: BTreeMap::new(),
                waiting: VecDeque::new(),
                recv: BTreeMap::new(),
                next_remote_uni: 0,
                peer_uni_base,
            };
            // Uni stream IDs: client-initiated 2 + 4i, server 3 + 4i.
            links.push(Link {
                sides: [side(a, b, client, c_conn, 3), side(b, a, server, s_conn, 2)],
                held: [Vec::new(), Vec::new()],
                blocked: [false; 2],
                failed: None,
            });
            index.insert((a, b), li);
        }
    }
    Ok(Core {
        cfg,
        now: 0,
        rng: SplitMix(cfg.seed ^ 0x7761_7270_6e65_7421),
        links,
        index,
        mail: Mail::default(),
        stats: WarpNetStats::default(),
        scratch: vec![0; 64 * 1024],
    })
}

impl WarpNet {
    pub fn new(cfg: WarpNetConfig) -> Result<Self, WarpNetError> {
        validate(&cfg)?;
        let (tx, rx) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("chorus-warpnet".into())
            .spawn(move || {
                let mut core = match build_core(cfg) {
                    Ok(c) => {
                        let _ = ready_tx.send(Ok(()));
                        c
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                while let Ok(job) = rx.recv() {
                    job(&mut core);
                }
            })
            .map_err(|e| WarpNetError::Warp(format!("cannot start the network thread: {e}")))?;
        ready_rx
            .recv()
            .map_err(|_| WarpNetError::Warp("network thread exited".into()))??;
        Ok(Self {
            tx,
            nodes: cfg.nodes,
        })
    }

    /// Run `f` on the network thread. `None` if the thread is gone.
    fn call<R: std::marker::Send + 'static>(
        &self,
        f: impl FnOnce(&mut Core) -> R + std::marker::Send + 'static,
    ) -> Option<R> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Box::new(move |c: &mut Core| {
                let _ = tx.send(f(c));
            }))
            .ok()?;
        rx.recv().ok()
    }

    /// The transport for one session endpoint on host `node`. `placement`
    /// maps each role of the session's choreography (by [`RoleId`] index)
    /// to the host that runs it.
    pub fn transport(
        &self,
        node: u32,
        placement: &[u32],
    ) -> Result<Box<dyn Transport>, WarpNetError> {
        let n = self.nodes;
        if node >= n || placement.iter().any(|p| *p >= n) {
            return Err(WarpNetError::Config("node out of range"));
        }
        if placement.len() > usize::from(u16::MAX) {
            return Err(WarpNetError::Config("too many roles"));
        }
        Ok(Box::new(WarpTransport {
            net: self.clone(),
            node,
            placement: placement.to_vec(),
        }))
    }

    /// Advance virtual time by one wire delay: write queued session bytes
    /// into streams, carry datagrams across the lossy links, run every
    /// endpoint's receive path, NIC and timers, then read arrived records.
    pub fn step(&self) {
        self.call(Core::step);
    }

    /// Step until no datagram is in flight and every session stream has
    /// ended (sent its FIN and had it read), or `max_steps` pass. Returns
    /// whether it settled.
    pub fn settle(&self, max_steps: u32) -> bool {
        self.call(move |g| {
            for _ in 0..max_steps {
                g.step();
                if g.quiet() {
                    return true;
                }
            }
            false
        })
        .unwrap_or(false)
    }

    pub fn now_ns(&self) -> u64 {
        self.call(|g| g.now).unwrap_or(0)
    }

    pub fn stats(&self) -> WarpNetStats {
        self.call(|g| {
            let mut s = g.stats;
            for l in &g.links {
                for side in &l.sides {
                    if let Some(c) = side.ep.connection(side.conn) {
                        let st = c.streams.stats();
                        s.stream_bytes_retransmitted += st.stream_bytes_retransmitted;
                        s.events_dropped += st.events_dropped;
                    }
                }
            }
            s
        })
        .unwrap_or_default()
    }

    /// Block (or unblock) the directed link `src → dst`: every datagram on
    /// it is dropped while blocked.
    pub fn set_blocked(&self, src: u32, dst: u32, blocked: bool) {
        self.call(move |g| {
            if let Some(&li) = g.index.get(&(src.min(dst), src.max(dst))) {
                g.links[li].blocked[usize::from(src > dst)] = blocked;
            }
        });
    }

    /// Frames delivered to a host but not yet received by a session.
    pub fn undelivered(&self) -> usize {
        self.call(|g| g.mail.frames.values().map(VecDeque::len).sum())
            .unwrap_or(0)
    }

    /// Session stream directions still open (sending or receiving) over all
    /// links.
    pub fn open_streams(&self) -> usize {
        self.call(|g| {
            g.links
                .iter()
                .flat_map(|l| l.sides.iter())
                .map(|s| s.send.len() + s.recv.len())
                .sum()
        })
        .unwrap_or(0)
    }

    /// Describes every open session stream direction (for diagnostics).
    pub fn describe_open_streams(&self) -> Vec<String> {
        self.call(|g| {
            let mut v = Vec::new();
            for l in &g.links {
                for s in &l.sides {
                    for (k, d) in &s.send {
                        v.push(format!(
                            "host {} send {k:?} stream {:?} pending {} fin {} dead {}",
                            s.node,
                            d.id,
                            d.pending.len(),
                            d.fin,
                            d.dead
                        ));
                    }
                    for (id, r) in &s.recv {
                        let st =
                            s.ep.connection(s.conn)
                                .and_then(|c| c.streams.recv_state(*id));
                        v.push(format!(
                            "host {} recv stream {id} {:?} buffered {} stopped {} state {st:?}",
                            s.node,
                            r.header,
                            r.buf.len(),
                            r.stopped
                        ));
                    }
                }
            }
            v
        })
        .unwrap_or_default()
    }

    /// A link that failed (its QUIC connection reported an error).
    pub fn link_failure(&self) -> Option<String> {
        self.call(|g| g.links.iter().find_map(|l| l.failed.clone()))
            .unwrap_or_else(|| Some("network thread exited".into()))
    }

    /// Write raw bytes as a new stream from host `src` to host `dst`
    /// (models a peer that does not use the session API; the bytes are
    /// untrusted at the receiver). The stream is finished after the bytes.
    pub fn inject_stream(&self, src: u32, dst: u32, bytes: &[u8]) -> Result<(), WarpNetError> {
        let bytes = bytes.to_vec();
        self.call(move |g| {
            let (li, si) = g
                .side_of(src, dst)
                .ok_or(WarpNetError::Config("no such link"))?;
            let side = &mut g.links[li].sides[si];
            let id = side
                .ep
                .open_stream(side.conn, true)
                .map_err(|e| WarpNetError::Warp(e.to_string()))?;
            // A key no session uses.
            let key = (u64::MAX - id, RoleId(u16::MAX), RoleId(u16::MAX));
            side.send.insert(
                key,
                SendDir {
                    id: Some(id),
                    pending: bytes.into_iter().collect(),
                    fin: true,
                    dead: false,
                },
            );
            g.stats.streams_opened += 1;
            Ok(())
        })
        .unwrap_or(Err(WarpNetError::Warp("network thread exited".into())))
    }
}

fn encode_header(key: DirKey, out: &mut VecDeque<u8>) {
    out.extend(STREAM_MAGIC);
    out.extend(key.0.to_be_bytes());
    out.extend(key.1.0.to_be_bytes());
    out.extend(key.2.0.to_be_bytes());
}

fn encode_frame(f: &Frame, out: &mut VecDeque<u8>) {
    out.push_back(TAG_FRAME);
    out.extend(f.seq.to_be_bytes());
    out.push_back(f.label.len() as u8);
    out.extend(f.label.as_bytes());
    out.push_back(f.payload_ty.len() as u8);
    out.extend(f.payload_ty.as_bytes());
    out.extend((f.body.len() as u32).to_be_bytes());
    out.extend(&f.body);
}

/// One parsed record.
enum Record {
    Frame(Frame),
    Abandon,
}

/// Parse one record from the front of `b`. `Ok(None)`: incomplete.
fn parse_record(b: &[u8], key: DirKey, max_body: u32) -> Result<Option<(Record, usize)>, ()> {
    let Some(&tag) = b.first() else {
        return Ok(None);
    };
    match tag {
        TAG_ABANDON => Ok(Some((Record::Abandon, 1))),
        TAG_FRAME => {
            let mut at = 1;
            let need = |at: usize, n: usize| b.len() >= at + n;
            if !need(at, 8) {
                return Ok(None);
            }
            let seq = u64::from_be_bytes(b[at..at + 8].try_into().map_err(|_| ())?);
            at += 8;
            let mut names = [String::new(), String::new()];
            for name in names.iter_mut() {
                if !need(at, 1) {
                    return Ok(None);
                }
                let n = usize::from(b[at]);
                at += 1;
                if n > MAX_NAME_LEN {
                    return Err(());
                }
                if !need(at, n) {
                    return Ok(None);
                }
                *name = std::str::from_utf8(&b[at..at + n])
                    .map_err(|_| ())?
                    .to_owned();
                at += n;
            }
            if !need(at, 4) {
                return Ok(None);
            }
            let len = u32::from_be_bytes(b[at..at + 4].try_into().map_err(|_| ())?);
            at += 4;
            if len > max_body {
                return Err(());
            }
            let len = len as usize;
            if !need(at, len) {
                return Ok(None);
            }
            let body = b[at..at + len].to_vec();
            at += len;
            let [label, payload_ty] = names;
            Ok(Some((
                Record::Frame(Frame {
                    channel: key.0,
                    from: key.1,
                    to: key.2,
                    seq,
                    label,
                    payload_ty,
                    body,
                }),
                at,
            )))
        }
        _ => Err(()),
    }
}

fn parse_header(b: &[u8]) -> Result<Option<DirKey>, ()> {
    if b.len() < STREAM_HEADER_LEN {
        // A prefix that already disagrees with the magic is malformed.
        let n = b.len().min(4);
        if b[..n] != STREAM_MAGIC[..n] {
            return Err(());
        }
        return Ok(None);
    }
    if b[..4] != STREAM_MAGIC {
        return Err(());
    }
    let ch = u64::from_be_bytes(b[4..12].try_into().map_err(|_| ())?);
    let from = u16::from_be_bytes([b[12], b[13]]);
    let to = u16::from_be_bytes([b[14], b[15]]);
    if from == to {
        return Err(());
    }
    Ok(Some((ch, RoleId(from), RoleId(to))))
}

impl Core {
    fn side_of(&self, src: u32, dst: u32) -> Option<(usize, usize)> {
        let li = *self.index.get(&(src.min(dst), src.max(dst)))?;
        Some((li, usize::from(src > dst)))
    }

    fn quiet(&self) -> bool {
        self.links.iter().all(|l| {
            l.held.iter().all(Vec::is_empty)
                && l.sides.iter().all(|s| {
                    s.ep.tap().out.is_empty()
                        && s.ep.queued_launches() == 0
                        && s.ep.parked_leases() == 0
                        && s.send.is_empty()
                        && s.recv.is_empty()
                })
        })
    }

    /// Queue bytes for direction `key` from host `src` to host `dst`.
    fn queue(
        &mut self,
        src: u32,
        dst: u32,
        key: DirKey,
        write: impl FnOnce(&mut VecDeque<u8>),
        fin: bool,
    ) -> Result<(), NetFail> {
        let (li, si) = self.side_of(src, dst).ok_or(NetFail::Closed)?;
        let link = &mut self.links[li];
        if link.failed.is_some() {
            return Err(NetFail::Closed);
        }
        let side = &mut link.sides[si];
        let fresh = !side.send.contains_key(&key);
        let d = side.send.entry(key).or_default();
        if d.dead {
            return Err(NetFail::PeerGone);
        }
        if d.fin {
            return Err(NetFail::Closed);
        }
        if fresh {
            encode_header(key, &mut d.pending);
            side.waiting.push_back(key);
        }
        write(&mut d.pending);
        d.fin |= fin;
        Ok(())
    }

    fn finish_dir(&mut self, src: u32, dst: u32, key: DirKey) {
        if let Some((li, si)) = self.side_of(src, dst)
            && let Some(d) = self.links[li].sides[si].send.get_mut(&key)
        {
            d.fin = true;
        }
    }

    fn step(&mut self) {
        let now = self.now;
        self.stats.steps += 1;
        self.flush_writes();
        // Carry datagrams across each link.
        for li in 0..self.links.len() {
            if self.links[li].failed.is_some() {
                continue;
            }
            for dir in 0..2 {
                let fresh: Vec<Vec<u8>> = self.links[li].sides[dir]
                    .ep
                    .tap_mut()
                    .out
                    .drain(..)
                    .collect();
                let old = std::mem::take(&mut self.links[li].held[dir]);
                let mut now_out = Vec::new();
                for d in fresh {
                    self.stats.datagrams += 1;
                    if self.links[li].blocked[dir] {
                        self.stats.partitioned += 1;
                    } else if self.chance(self.cfg.loss_ppm) {
                        self.stats.dropped += 1;
                    } else if self.chance(self.cfg.reorder_ppm) {
                        self.stats.reordered += 1;
                        self.links[li].held[dir].push(d);
                    } else {
                        if self.chance(self.cfg.duplicate_ppm) {
                            self.stats.duplicated += 1;
                            now_out.push(d.clone());
                        }
                        now_out.push(d);
                    }
                }
                now_out.extend(old);
                let rx = &mut self.links[li].sides[1 - dir];
                for d in &now_out {
                    match rx.ep.xsk_mut().backend_mut().deliver(d) {
                        vesper_warp_umem::DeliverOutcome::Delivered { .. } => {}
                        _ => self.stats.nic_dropped += 1,
                    }
                }
            }
            let link = &mut self.links[li];
            let r = (|| -> Result<(), vesper_warp::endpoint::WarpError> {
                for _ in 0..4096 {
                    let a = link.sides[0].ep.poll_and_step_quic_rx_one(now)?;
                    let b = link.sides[1].ep.poll_and_step_quic_rx_one(now)?;
                    if a == RxOutcome::Idle && b == RxOutcome::Idle {
                        break;
                    }
                }
                for s in link.sides.iter_mut() {
                    s.ep.xsk_mut().backend_mut().step();
                    s.ep.poll(now)?;
                    if let Some(e) = s.ep.connection_error(s.conn) {
                        return Err(vesper_warp::endpoint::WarpError::ConnectionClosed(e));
                    }
                }
                Ok(())
            })();
            if let Err(e) = r {
                link.failed = Some(e.to_string());
            }
        }
        self.flush_reads();
        self.now = self.now.saturating_add(self.cfg.step_ns);
    }

    fn chance(&mut self, ppm: u32) -> bool {
        ppm > 0 && self.rng.next_u64() % 1_000_000 < u64::from(ppm)
    }

    /// Open waiting streams, write pending bytes, send FINs.
    fn flush_writes(&mut self) {
        let stats = &mut self.stats;
        for link in self.links.iter_mut() {
            if link.failed.is_some() {
                continue;
            }
            for side in link.sides.iter_mut() {
                while let Some(&key) = side.waiting.front() {
                    match side.ep.open_stream(side.conn, true) {
                        Ok(id) => {
                            side.waiting.pop_front();
                            stats.streams_opened += 1;
                            if let Some(d) = side.send.get_mut(&key) {
                                d.id = Some(id);
                            }
                        }
                        Err(_) => break, // stream limit: wait for MAX_STREAMS
                    }
                }
                let mut done = Vec::new();
                for (key, d) in side.send.iter_mut() {
                    let Some(id) = d.id else { continue };
                    if d.dead {
                        done.push(*key);
                        continue;
                    }
                    while !d.pending.is_empty() {
                        let (a, _) = d.pending.as_slices();
                        match side.ep.stream_write(side.conn, id, a) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                d.pending.drain(..n);
                            }
                        }
                    }
                    if d.pending.is_empty() && d.fin {
                        if side.ep.stream_finish(side.conn, id).is_ok() {
                            stats.streams_finished += 1;
                        }
                        done.push(*key);
                    }
                }
                for k in done {
                    side.send.remove(&k);
                }
            }
        }
    }

    /// Read every readable stream and turn its records into mailbox frames.
    fn flush_reads(&mut self) {
        let max_body = self.cfg.max_body;
        let Core {
            links,
            mail,
            stats,
            scratch,
            ..
        } = self;
        for link in links.iter_mut() {
            if link.failed.is_some() {
                continue;
            }
            for side in link.sides.iter_mut() {
                // Discover peer-opened streams in ID order.
                loop {
                    let id = side.peer_uni_base + 4 * side.next_remote_uni;
                    let known = side
                        .ep
                        .connection(side.conn)
                        .is_some_and(|c| c.streams.recv_state(id).is_some());
                    if !known {
                        break;
                    }
                    side.next_remote_uni += 1;
                    stats.streams_received += 1;
                    side.recv.insert(
                        id,
                        RecvDir {
                            buf: Vec::new(),
                            header: None,
                            stopped: false,
                        },
                    );
                }
                while let Some(ev) = side.ep.poll_stream_event(side.conn) {
                    match ev {
                        StreamEvent::StopSending { stream_id, .. } => {
                            for d in side.send.values_mut() {
                                if d.id == Some(stream_id) {
                                    d.dead = true;
                                    d.pending.clear();
                                }
                            }
                        }
                        StreamEvent::Reset { stream_id, .. } => {
                            if let Some(r) = side.recv.remove(&stream_id)
                                && let Some(key) = r.header
                            {
                                mail.abandoned.insert((side.node, side.peer, key.0, key.1));
                            }
                        }
                        _ => {}
                    }
                }
                let mut finished = Vec::new();
                for (&id, r) in side.recv.iter_mut() {
                    let mut fin = false;
                    loop {
                        match side.ep.stream_read(side.conn, id, &mut scratch[..]) {
                            Ok(o) => {
                                if !r.stopped {
                                    r.buf.extend_from_slice(&scratch[..o.len]);
                                }
                                if o.fin {
                                    fin = true;
                                    break;
                                }
                                if o.len == 0 {
                                    break;
                                }
                            }
                            Err(_) => {
                                fin = true;
                                break;
                            }
                        }
                    }
                    if !r.stopped
                        && let Err(()) =
                            Self::parse_stream((side.node, side.peer), r, mail, stats, max_body)
                    {
                        r.stopped = true;
                        r.buf.clear();
                        stats.malformed_streams += 1;
                        if let Some(key) = r.header {
                            mail.malformed.insert((side.node, side.peer, key));
                        }
                        let _ = side.ep.stream_stop_sending(side.conn, id, ERR_MALFORMED);
                    }
                    if fin {
                        finished.push(id);
                    }
                }
                for id in finished {
                    if let Some(r) = side.recv.remove(&id)
                        && !r.stopped
                        && !r.buf.is_empty()
                    {
                        // Truncated record at FIN.
                        stats.malformed_streams += 1;
                        if let Some(key) = r.header {
                            mail.malformed.insert((side.node, side.peer, key));
                        }
                    }
                }
            }
        }
    }

    fn parse_stream(
        (node, src): (u32, u32),
        r: &mut RecvDir,
        mail: &mut Mail,
        stats: &mut WarpNetStats,
        max_body: u32,
    ) -> Result<(), ()> {
        let mut at = 0;
        if r.header.is_none() {
            match parse_header(&r.buf)? {
                None => return Ok(()),
                Some(k) => {
                    r.header = Some(k);
                    at = STREAM_HEADER_LEN;
                }
            }
        }
        let key = r.header.expect("header parsed");
        loop {
            match parse_record(&r.buf[at..], key, max_body)? {
                None => break,
                Some((Record::Frame(f), n)) => {
                    at += n;
                    stats.frames_delivered += 1;
                    mail.frames
                        .entry((node, src, key))
                        .or_default()
                        .push_back(f);
                }
                Some((Record::Abandon, n)) => {
                    at += n;
                    stats.abandons_received += 1;
                    mail.abandoned.insert((node, src, key.0, key.1));
                }
            }
        }
        r.buf.drain(..at);
        Ok(())
    }

    /// Next frame for `key` at `node` that arrived from host `src` (the
    /// host the receiver's placement assigns to the sending role).
    fn try_recv(&mut self, node: u32, src: u32, key: DirKey) -> Result<Option<Frame>, NetFail> {
        if let Some(f) = self
            .mail
            .frames
            .get_mut(&(node, src, key))
            .and_then(VecDeque::pop_front)
        {
            return Ok(Some(f));
        }
        if self.mail.malformed.contains(&(node, src, key)) {
            return Err(NetFail::Malformed);
        }
        if self.mail.abandoned.contains(&(node, src, key.0, key.1)) {
            return Err(NetFail::PeerGone);
        }
        Ok(None)
    }
}

/// The [`Transport`] of one session endpoint on a [`WarpNet`] host.
pub struct WarpTransport {
    net: WarpNet,
    node: u32,
    placement: Vec<u32>,
}

impl WarpTransport {
    fn host(&self, r: RoleId) -> Result<u32, NetFail> {
        self.placement
            .get(usize::from(r.0))
            .copied()
            .ok_or(NetFail::Closed)
    }
}

impl Transport for WarpTransport {
    fn send(&mut self, frame: Frame) -> Result<(), NetFail> {
        let dst = self.host(frame.to)?;
        let node = self.node;
        self.net
            .call(move |g| {
                if g.mail
                    .abandoned
                    .contains(&(node, dst, frame.channel, frame.to))
                {
                    return Err(NetFail::PeerGone);
                }
                if frame.label.len() > MAX_NAME_LEN
                    || frame.payload_ty.len() > MAX_NAME_LEN
                    || frame.body.len() > g.cfg.max_body as usize
                {
                    return Err(NetFail::Malformed);
                }
                g.stats.frames_sent += 1;
                let key = (frame.channel, frame.from, frame.to);
                if dst == node {
                    g.stats.local_frames += 1;
                    g.mail
                        .frames
                        .entry((dst, node, key))
                        .or_default()
                        .push_back(frame);
                    return Ok(());
                }
                g.queue(node, dst, key, |out| encode_frame(&frame, out), false)
            })
            .unwrap_or(Err(NetFail::Closed))
    }

    fn try_recv(
        &mut self,
        channel: u64,
        from: RoleId,
        to: RoleId,
    ) -> Result<Option<Frame>, NetFail> {
        let node = self.node;
        let src = self.host(from)?;
        self.net
            .call(move |g| g.try_recv(node, src, (channel, from, to)))
            .unwrap_or(Err(NetFail::Closed))
    }

    /// Blocking receive: one network step per wait, released between
    /// steps so that other threads' sessions can send meanwhile.
    fn recv(&mut self, channel: u64, from: RoleId, to: RoleId) -> Result<Frame, NetFail> {
        let node = self.node;
        let src = self.host(from)?;
        let wait = self
            .net
            .call(|g| g.cfg.max_wait_steps)
            .ok_or(NetFail::Closed)?;
        for _ in 0..=wait {
            let r = self
                .net
                .call(move |g| {
                    if let Some(f) = g.try_recv(node, src, (channel, from, to))? {
                        return Ok(Some(f));
                    }
                    if let Some((li, _)) = g.side_of(node, src)
                        && g.links[li].failed.is_some()
                    {
                        return Err(NetFail::Closed);
                    }
                    g.step();
                    Ok(None)
                })
                .unwrap_or(Err(NetFail::Closed))?;
            if let Some(f) = r {
                return Ok(f);
            }
        }
        Err(NetFail::Timeout)
    }

    fn abandon(&mut self, channel: u64, me: RoleId) {
        let node = self.node;
        let placement = self.placement.clone();
        self.net.call(move |g| {
            for (i, &host) in placement.iter().enumerate() {
                let peer = RoleId(i as u16);
                if peer == me {
                    continue;
                }
                if host == node {
                    g.mail.abandoned.insert((host, node, channel, me));
                    continue;
                }
                let key = (channel, me, peer);
                if g.queue(node, host, key, |out| out.push_back(TAG_ABANDON), true)
                    .is_ok()
                {
                    g.stats.abandons_sent += 1;
                }
            }
        });
    }

    fn finish(&mut self, channel: u64, me: RoleId) {
        let node = self.node;
        let placement = self.placement.clone();
        self.net.call(move |g| {
            for (i, &host) in placement.iter().enumerate() {
                let peer = RoleId(i as u16);
                if peer != me && host != node {
                    g.finish_dir(node, host, (channel, me, peer));
                }
            }
        });
    }
}
