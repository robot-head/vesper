//! Deterministic two-endpoint test harness with a seeded lossy, reordering
//! and duplicating link. Virtual time only; no I/O.
//!
//! The harness allocates freely (packet copies, logs); it is a test driver,
//! not part of the zero-allocation steady-state path.

use crate::cc::CongestionController;
use crate::conn::{Connection, Event, SendView, State, TcpError};
use crate::wire::{IpPair, Segment};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Which endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// The active opener.
    A,
    /// The passive opener.
    B,
}

/// Link impairments. Probabilities are parts per million.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkConfig {
    /// One-way propagation delay.
    pub delay_ns: u64,
    /// Uniform extra delay in `0..jitter_ns`; nonzero jitter reorders.
    pub jitter_ns: u64,
    /// Drop probability.
    pub loss_ppm: u32,
    /// Duplication probability.
    pub dup_ppm: u32,
    /// Serialization rate per direction in bytes/s (0 = infinite).
    pub rate_bytes_per_sec: u64,
    /// PRNG seed.
    pub seed: u64,
}

impl Default for LinkConfig {
    fn default() -> Self {
        LinkConfig {
            delay_ns: 5_000_000,
            jitter_ns: 0,
            loss_ppm: 0,
            dup_ppm: 0,
            rate_bytes_per_sec: 12_500_000,
            seed: 1,
        }
    }
}

/// How the receiving application consumes delivered bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadPolicy {
    /// Release everything as soon as it is delivered.
    Immediate,
    /// Release nothing before the given time, then behave as `Immediate`.
    StallUntil(u64),
    /// Release at most `chunk` bytes every `interval_ns`.
    Periodic {
        /// Period.
        interval_ns: u64,
        /// Bytes per period.
        chunk: u64,
    },
}

/// SplitMix64: a small deterministic PRNG for the link model (not for
/// anything security-relevant).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// Seeded generator.
    #[must_use]
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (0 when `n == 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// True with probability `ppm / 10^6`.
    pub fn chance(&mut self, ppm: u32) -> bool {
        self.below(1_000_000) < u64::from(ppm)
    }
}

/// One endpoint and its application-side buffers.
#[derive(Debug)]
pub struct Peer<C: CongestionController> {
    /// The connection.
    pub conn: Connection<C>,
    /// Addresses oriented local → remote.
    pub ips: IpPair,
    /// Unacknowledged part of the send stream (caller-owned).
    pub send_buf: Vec<u8>,
    /// Stream offset of `send_buf[0]`.
    pub send_base: u64,
    /// Receive ring (caller-owned).
    pub ring: Vec<u8>,
    /// Everything delivered so far, in order.
    pub received: Vec<u8>,
    /// Peer FIN delivered.
    pub peer_finished: bool,
    /// Application read policy.
    pub read: ReadPolicy,
    /// Events observed.
    pub events: Vec<Event>,
    /// Errors returned by `on_segment`.
    pub errors: Vec<TcpError>,
    /// Segments emitted (including dropped ones).
    pub transmitted: u64,
    next_read_ns: u64,
}

impl<C: CongestionController> Peer<C> {
    fn new(conn: Connection<C>, ips: IpPair) -> Peer<C> {
        let ring = vec![0u8; conn.config().recv_buffer as usize];
        Peer {
            conn,
            ips,
            send_buf: Vec::new(),
            send_base: 0,
            ring,
            received: Vec::new(),
            peer_finished: false,
            read: ReadPolicy::Immediate,
            events: Vec::new(),
            errors: Vec::new(),
            transmitted: 0,
            next_read_ns: 0,
        }
    }

    /// Append application data to the send stream.
    pub fn write(&mut self, data: &[u8]) -> Result<(), TcpError> {
        self.conn.write(data.len() as u64)?;
        self.send_buf.extend_from_slice(data);
        Ok(())
    }

    fn reclaim(&mut self) {
        while let Some(r) = self.conn.take_released() {
            let n = (r.end - self.send_base) as usize;
            self.send_buf.drain(..n.min(self.send_buf.len()));
            self.send_base = r.end;
        }
        while let Some(e) = self.conn.take_event() {
            self.events.push(e);
        }
    }

    fn app_read(&mut self, now: u64) {
        let unread = self.conn.unread();
        if unread == 0 {
            return;
        }
        let n = match self.read {
            ReadPolicy::Immediate => unread,
            ReadPolicy::StallUntil(t) if now >= t => unread,
            ReadPolicy::StallUntil(_) => 0,
            ReadPolicy::Periodic { interval_ns, chunk } => {
                if now >= self.next_read_ns {
                    self.next_read_ns = now + interval_ns;
                    unread.min(chunk)
                } else {
                    0
                }
            }
        };
        if n > 0 {
            self.conn.recv_release(n).expect("release within unread");
        }
    }

    fn app_deadline(&self) -> Option<u64> {
        if self.conn.unread() == 0 {
            return None;
        }
        match self.read {
            ReadPolicy::Immediate => None,
            ReadPolicy::StallUntil(t) => Some(t),
            ReadPolicy::Periodic { .. } => Some(self.next_read_ns),
        }
    }
}

/// A drop filter: return `true` to drop. Arguments are the sending side,
/// the parsed segment and that side's transmission index.
pub type DropFilter = Box<dyn FnMut(Side, &Segment<'_>, u64) -> bool>;

/// A packet on the link: (arrival ns, tie-break order, sent by A, bytes).
type InFlight = (u64, u64, bool, Vec<u8>);

/// The two-endpoint harness.
pub struct Harness<C: CongestionController> {
    /// Virtual time.
    pub now: u64,
    /// Active opener.
    pub a: Peer<C>,
    /// Passive opener.
    pub b: Peer<C>,
    /// Link model.
    pub link: LinkConfig,
    /// Optional targeted drops (applied before random loss).
    pub filter: Option<DropFilter>,
    /// Packets dropped by the filter or random loss.
    pub dropped: u64,
    /// Packets duplicated.
    pub duplicated: u64,
    rng: Rng,
    queue: BinaryHeap<Reverse<InFlight>>,
    order: u64,
    busy_until: [u64; 2],
    out: Vec<u8>,
}

impl<C: CongestionController> Harness<C> {
    /// Build a harness: `b` listens on its configured port and `a`
    /// connects to it at time 0.
    pub fn new(
        a: Connection<C>,
        b: Connection<C>,
        link: LinkConfig,
    ) -> Result<Harness<C>, TcpError> {
        Harness::with_ips(
            a,
            b,
            link,
            IpPair::V4 {
                src: [10, 0, 0, 1],
                dst: [10, 0, 0, 2],
            },
        )
    }

    /// As [`Harness::new`] with explicit addresses (oriented A → B).
    pub fn with_ips(
        mut a: Connection<C>,
        mut b: Connection<C>,
        link: LinkConfig,
        ips_a: IpPair,
    ) -> Result<Harness<C>, TcpError> {
        b.listen(0x5eed_0000 ^ link.seed as u32)?;
        let b_port = b.config().local_port;
        a.connect(0, ips_a, b_port, 0x1234_5678 ^ (link.seed >> 32) as u32)?;
        let mtu = 60 + usize::from(a.config().mss.max(b.config().mss));
        Ok(Harness {
            now: 0,
            a: Peer::new(a, ips_a),
            b: Peer::new(b, ips_a.reversed()),
            link,
            filter: None,
            dropped: 0,
            duplicated: 0,
            rng: Rng::new(link.seed),
            queue: BinaryHeap::new(),
            order: 0,
            busy_until: [0; 2],
            out: vec![0; mtu],
        })
    }

    /// Mutable access to one side.
    pub fn peer(&mut self, side: Side) -> &mut Peer<C> {
        match side {
            Side::A => &mut self.a,
            Side::B => &mut self.b,
        }
    }

    fn flush(&mut self, side: Side) -> Result<(), TcpError> {
        loop {
            let now = self.now;
            let peer = match side {
                Side::A => &mut self.a,
                Side::B => &mut self.b,
            };
            let view = SendView {
                base: peer.send_base,
                bytes: &peer.send_buf,
            };
            let Some(t) = peer.conn.poll_transmit(now, &view, &mut self.out)? else {
                peer.reclaim();
                return Ok(());
            };
            let idx = peer.transmitted;
            peer.transmitted += 1;
            let pkt = self.out[..t.len].to_vec();
            let drop = match (&mut self.filter, Segment::parse(&pkt)) {
                (Some(f), Ok(seg)) => f(side, &seg, idx),
                _ => false,
            };
            let dir = usize::from(side == Side::B);
            let depart = t.edt_ns.max(now).max(self.busy_until[dir]);
            let tx_ns = match self.link.rate_bytes_per_sec {
                0 => 0,
                r => (pkt.len() as u64).saturating_mul(1_000_000_000) / r,
            };
            self.busy_until[dir] = depart + tx_ns;
            if drop || self.rng.chance(self.link.loss_ppm) {
                self.dropped += 1;
                continue;
            }
            let copies = if self.rng.chance(self.link.dup_ppm) {
                self.duplicated += 1;
                2
            } else {
                1
            };
            for _ in 0..copies {
                let at = depart + tx_ns + self.link.delay_ns + self.rng.below(self.link.jitter_ns);
                self.order += 1;
                self.queue
                    .push(Reverse((at, self.order, side == Side::A, pkt.clone())));
            }
        }
    }

    fn deliver(&mut self, to: Side, pkt: &[u8]) {
        let now = self.now;
        let peer = self.peer(to);
        let ips_in = peer.ips.reversed();
        match peer.conn.on_segment(now, ips_in, pkt, &mut peer.ring) {
            Ok(rec) => {
                for c in rec.chunks() {
                    peer.received.extend_from_slice(c);
                }
                if rec.fin {
                    peer.peer_finished = true;
                }
            }
            Err(e) => peer.errors.push(e),
        }
        peer.app_read(now);
        peer.reclaim();
    }

    /// Advance to the next event. Returns false when nothing is pending.
    pub fn step(&mut self) -> Result<bool, TcpError> {
        self.flush(Side::A)?;
        self.flush(Side::B)?;
        let next = [
            self.queue.peek().map(|Reverse((t, ..))| *t),
            self.a.conn.poll_timeout(),
            self.b.conn.poll_timeout(),
            self.a.app_deadline(),
            self.b.app_deadline(),
        ]
        .into_iter()
        .flatten()
        .min();
        let Some(t) = next else { return Ok(false) };
        self.now = self.now.max(t);
        while let Some(Reverse((at, ..))) = self.queue.peek() {
            if *at > self.now {
                break;
            }
            let Some(Reverse((_, _, from_a, pkt))) = self.queue.pop() else {
                break;
            };
            let to = if from_a { Side::B } else { Side::A };
            self.deliver(to, &pkt);
            self.flush(to)?;
        }
        for side in [Side::A, Side::B] {
            let now = self.now;
            let p = self.peer(side);
            if p.conn.poll_timeout().is_some_and(|d| d <= now) {
                p.conn.on_timeout(now);
            }
            p.app_read(now);
            p.reclaim();
        }
        Ok(true)
    }

    /// Step until `done` holds or virtual time passes `deadline_ns`.
    /// Returns whether `done` held.
    pub fn run_until(
        &mut self,
        deadline_ns: u64,
        mut done: impl FnMut(&Self) -> bool,
    ) -> Result<bool, TcpError> {
        while !done(self) {
            if self.now > deadline_ns || !self.step()? {
                return Ok(done(self));
            }
        }
        Ok(true)
    }

    /// Both sides are in `state`.
    #[must_use]
    pub fn both(&self, state: State) -> bool {
        self.a.conn.state() == state && self.b.conn.state() == state
    }
}
