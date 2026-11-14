//! virtio-vsock device model (virtio-v1.2 §5.10) with a host-side socket
//! table, stream sockets only.
//!
//! Queues: receive (0), transmit (1) and event (2). No feature bits are
//! offered, so only `VIRTIO_VSOCK_TYPE_STREAM` is valid. The host is CID 2.
//! Guest packets are validated before any effect: the header must be 44
//! bytes, `len` must equal the payload length and be at most
//! [`MAX_PAYLOAD`], `src_cid` must be the guest's CID and `dst_cid` the host's
//! (spoofed or misaddressed packets are dropped and counted). Malformed packets
//! are dropped; a well-formed packet for an unknown connection, of another
//! socket type or with an unknown op gets `RST`.
//!
//! Credit-based flow control (§5.10.6.3) is enforced in both directions: a
//! guest `RW` packet larger than the free space it was granted resets the
//! connection, and the host never sends more than the guest's
//! `buf_alloc - (tx_cnt - peer_fwd_cnt)`. Replies wait in a bounded outbox;
//! when it is full the device stops taking transmit chains until the guest
//! posts receive buffers (back-pressure, never loss of control packets).
//! [`VsockDevice::transport_reset`] drops every connection and posts
//! `VIRTIO_VSOCK_EVENT_TRANSPORT_RESET`, which a restored snapshot clone must
//! send so that the guest forgets connections that belonged to its parent.
#![forbid(unsafe_code)]

use super::mmio::{DeviceError, QueueIo, VirtioDevice};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const VIRTIO_ID_VSOCK: u32 = 19;
pub const HOST_CID: u64 = 2;
pub const HEADER_BYTES: usize = 44;
pub const MAX_PAYLOAD: usize = 64 * 1024;
pub const TYPE_STREAM: u16 = 1;
pub const OP_REQUEST: u16 = 1;
pub const OP_RESPONSE: u16 = 2;
pub const OP_RST: u16 = 3;
pub const OP_SHUTDOWN: u16 = 4;
pub const OP_RW: u16 = 5;
pub const OP_CREDIT_UPDATE: u16 = 6;
pub const OP_CREDIT_REQUEST: u16 = 7;
pub const SHUTDOWN_RCV: u32 = 1;
pub const SHUTDOWN_SEND: u32 = 2;
pub const EVENT_TRANSPORT_RESET: u32 = 0;
pub const RX_QUEUE: u16 = 0;
pub const TX_QUEUE: u16 = 1;
pub const EVENT_QUEUE: u16 = 2;
/// Receive buffer the host grants each connection.
pub const HOST_BUF_ALLOC: u32 = 64 * 1024;
/// Packets waiting for guest receive buffers before back-pressure starts.
pub const OUTBOX_LIMIT: usize = 512;
/// Connections per device.
pub const MAX_CONNECTIONS: usize = 1024;
const QUEUE_MAX: [u16; 3] = [256, 256, 64];
const CONFIG_BYTES: usize = 8;

/// `struct virtio_vsock_hdr`, little endian, 44 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VsockHeader {
    pub src_cid: u64,
    pub dst_cid: u64,
    pub src_port: u32,
    pub dst_port: u32,
    pub len: u32,
    pub kind: u16,
    pub op: u16,
    pub flags: u32,
    pub buf_alloc: u32,
    pub fwd_cnt: u32,
}
impl VsockHeader {
    pub fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut b = [0u8; HEADER_BYTES];
        b[0..8].copy_from_slice(&self.src_cid.to_le_bytes());
        b[8..16].copy_from_slice(&self.dst_cid.to_le_bytes());
        b[16..20].copy_from_slice(&self.src_port.to_le_bytes());
        b[20..24].copy_from_slice(&self.dst_port.to_le_bytes());
        b[24..28].copy_from_slice(&self.len.to_le_bytes());
        b[28..30].copy_from_slice(&self.kind.to_le_bytes());
        b[30..32].copy_from_slice(&self.op.to_le_bytes());
        b[32..36].copy_from_slice(&self.flags.to_le_bytes());
        b[36..40].copy_from_slice(&self.buf_alloc.to_le_bytes());
        b[40..44].copy_from_slice(&self.fwd_cnt.to_le_bytes());
        b
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let b: &[u8; HEADER_BYTES] = b.get(..HEADER_BYTES)?.try_into().ok()?;
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().expect("8"));
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().expect("4"));
        let u16_at = |i: usize| u16::from_le_bytes(b[i..i + 2].try_into().expect("2"));
        Some(Self {
            src_cid: u64_at(0),
            dst_cid: u64_at(8),
            src_port: u32_at(16),
            dst_port: u32_at(20),
            len: u32_at(24),
            kind: u16_at(28),
            op: u16_at(30),
            flags: u32_at(32),
            buf_alloc: u32_at(36),
            fwd_cnt: u32_at(40),
        })
    }
}

/// `(guest_port, host_port)`.
pub type ConnKey = (u32, u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnState {
    /// Host sent REQUEST, waiting for the guest's RESPONSE.
    Connecting,
    Connected,
}

#[derive(Debug)]
struct Conn {
    state: ConnState,
    peer_buf_alloc: u32,
    peer_fwd_cnt: u32,
    tx_cnt: u32,
    rx_cnt: u32,
    fwd_cnt: u32,
    rx: VecDeque<u8>,
    peer_shutdown: u32,
}
impl Conn {
    fn new(state: ConnState) -> Self {
        Self {
            state,
            peer_buf_alloc: 0,
            peer_fwd_cnt: 0,
            tx_cnt: 0,
            rx_cnt: 0,
            fwd_cnt: 0,
            rx: VecDeque::new(),
            peer_shutdown: 0,
        }
    }
    /// Bytes the guest may still send us.
    fn host_free(&self) -> u32 {
        HOST_BUF_ALLOC.wrapping_sub(self.rx_cnt.wrapping_sub(self.fwd_cnt))
    }
    /// Bytes we may still send the guest.
    fn guest_credit(&self) -> u32 {
        // The in-flight count wraps (sequence counters); the subtraction from
        // the advertised allocation saturates, so a shrunken allocation grants
        // no credit until the guest forwards enough bytes.
        self.peer_buf_alloc
            .saturating_sub(self.tx_cnt.wrapping_sub(self.peer_fwd_cnt))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VsockError {
    InvalidCid,
    PortInUse,
    NoSuchConnection,
    NotConnected,
    TooManyConnections,
    OutboxFull,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VsockStats {
    pub malformed: u64,
    pub spoofed: u64,
    pub resets_sent: u64,
    pub credit_violations: u64,
    pub packets_rx: u64,
    pub packets_tx: u64,
}

#[derive(Debug)]
pub struct VsockDevice {
    guest_cid: u64,
    listeners: BTreeSet<u32>,
    conns: BTreeMap<ConnKey, Conn>,
    outbox: VecDeque<(VsockHeader, Vec<u8>)>,
    events: VecDeque<u32>,
    stats: VsockStats,
}

impl VsockDevice {
    /// Guest CIDs 0, 1, 2 and `u32::MAX` are reserved; the upper 32 bits must be 0.
    pub fn new(guest_cid: u64) -> Result<Self, VsockError> {
        if guest_cid <= HOST_CID || guest_cid >= u64::from(u32::MAX) {
            return Err(VsockError::InvalidCid);
        }
        Ok(Self {
            guest_cid,
            listeners: BTreeSet::new(),
            conns: BTreeMap::new(),
            outbox: VecDeque::new(),
            events: VecDeque::new(),
            stats: VsockStats::default(),
        })
    }
    pub fn stats(&self) -> VsockStats {
        self.stats
    }
    pub fn listen(&mut self, host_port: u32) -> Result<(), VsockError> {
        if !self.listeners.insert(host_port) {
            return Err(VsockError::PortInUse);
        }
        Ok(())
    }
    pub fn connection_state(&self, key: ConnKey) -> Option<ConnState> {
        self.conns.get(&key).map(|c| c.state)
    }
    pub fn connections(&self) -> Vec<ConnKey> {
        self.conns.keys().copied().collect()
    }
    pub fn outbox_len(&self) -> usize {
        self.outbox.len()
    }

    fn header(&self, key: ConnKey, op: u16, len: u32, flags: u32) -> VsockHeader {
        VsockHeader {
            src_cid: HOST_CID,
            dst_cid: self.guest_cid,
            src_port: key.1,
            dst_port: key.0,
            len,
            kind: TYPE_STREAM,
            op,
            flags,
            buf_alloc: HOST_BUF_ALLOC,
            fwd_cnt: self.conns.get(&key).map_or(0, |c| c.fwd_cnt),
        }
    }
    fn push(&mut self, key: ConnKey, op: u16, flags: u32, payload: Vec<u8>) {
        let h = self.header(key, op, payload.len() as u32, flags);
        if op == OP_RST {
            self.stats.resets_sent += 1;
        }
        self.outbox.push_back((h, payload));
    }

    /// Host-initiated connection to a guest listener on `guest_port`.
    pub fn connect(&mut self, host_port: u32, guest_port: u32) -> Result<ConnKey, VsockError> {
        let key = (guest_port, host_port);
        if self.conns.contains_key(&key) {
            return Err(VsockError::PortInUse);
        }
        if self.conns.len() >= MAX_CONNECTIONS {
            return Err(VsockError::TooManyConnections);
        }
        if self.outbox.len() >= OUTBOX_LIMIT {
            return Err(VsockError::OutboxFull);
        }
        self.conns.insert(key, Conn::new(ConnState::Connecting));
        self.push(key, OP_REQUEST, 0, Vec::new());
        Ok(key)
    }
    /// Takes everything the guest sent on `key` and returns credit.
    pub fn recv(&mut self, key: ConnKey) -> Result<Vec<u8>, VsockError> {
        let conn = self
            .conns
            .get_mut(&key)
            .ok_or(VsockError::NoSuchConnection)?;
        let data: Vec<u8> = conn.rx.drain(..).collect();
        conn.fwd_cnt = conn.fwd_cnt.wrapping_add(data.len() as u32);
        if !data.is_empty() && self.outbox.len() < OUTBOX_LIMIT {
            self.push(key, OP_CREDIT_UPDATE, 0, Vec::new());
        }
        Ok(data)
    }
    /// Sends up to the guest's credit; returns the number of bytes queued.
    pub fn send(&mut self, key: ConnKey, data: &[u8]) -> Result<usize, VsockError> {
        let conn = self.conns.get(&key).ok_or(VsockError::NoSuchConnection)?;
        if conn.state != ConnState::Connected || conn.peer_shutdown & SHUTDOWN_RCV != 0 {
            return Err(VsockError::NotConnected);
        }
        let mut sent = 0usize;
        while sent < data.len() && self.outbox.len() < OUTBOX_LIMIT {
            let credit = self.conns[&key].guest_credit() as usize;
            let n = credit.min(data.len() - sent).min(4096);
            if n == 0 {
                break;
            }
            let conn = self.conns.get_mut(&key).expect("checked above");
            conn.tx_cnt = conn.tx_cnt.wrapping_add(n as u32);
            self.push(key, OP_RW, 0, data[sent..sent + n].to_vec());
            sent += n;
        }
        Ok(sent)
    }
    /// Shuts both directions down; the guest answers with RST.
    pub fn shutdown(&mut self, key: ConnKey) -> Result<(), VsockError> {
        if !self.conns.contains_key(&key) {
            return Err(VsockError::NoSuchConnection);
        }
        self.push(key, OP_SHUTDOWN, SHUTDOWN_RCV | SHUTDOWN_SEND, Vec::new());
        Ok(())
    }
    /// After a snapshot restore or fork: drop every connection and pending
    /// packet and tell the guest (`VIRTIO_VSOCK_EVENT_TRANSPORT_RESET`).
    pub fn transport_reset(&mut self) {
        self.conns.clear();
        self.outbox.clear();
        self.events.push_back(EVENT_TRANSPORT_RESET);
    }

    fn rst_reply(&mut self, h: &VsockHeader) {
        if h.op == OP_RST {
            return;
        }
        let mut r = VsockHeader {
            src_cid: HOST_CID,
            dst_cid: self.guest_cid,
            src_port: h.dst_port,
            dst_port: h.src_port,
            kind: h.kind,
            op: OP_RST,
            ..VsockHeader::default()
        };
        r.buf_alloc = 0;
        self.stats.resets_sent += 1;
        self.outbox.push_back((r, Vec::new()));
    }

    /// One guest packet. Returns false if malformed (dropped).
    fn on_guest_packet(&mut self, bytes: &[u8]) -> bool {
        let Some(h) = VsockHeader::decode(bytes) else {
            self.stats.malformed += 1;
            return false;
        };
        let payload = &bytes[HEADER_BYTES..];
        if h.len as usize != payload.len() || payload.len() > MAX_PAYLOAD {
            self.stats.malformed += 1;
            return false;
        }
        if h.src_cid != self.guest_cid || h.dst_cid != HOST_CID {
            self.stats.spoofed += 1;
            return false;
        }
        self.stats.packets_tx += 1;
        if h.kind != TYPE_STREAM {
            self.rst_reply(&h);
            return true;
        }
        let key = (h.src_port, h.dst_port);
        if let Some(c) = self.conns.get_mut(&key) {
            c.peer_buf_alloc = h.buf_alloc;
            c.peer_fwd_cnt = h.fwd_cnt;
        }
        let state = self.conns.get(&key).map(|c| c.state);
        match (h.op, state) {
            (OP_REQUEST, None)
                if self.listeners.contains(&h.dst_port) && self.conns.len() < MAX_CONNECTIONS =>
            {
                let mut c = Conn::new(ConnState::Connected);
                c.peer_buf_alloc = h.buf_alloc;
                c.peer_fwd_cnt = h.fwd_cnt;
                self.conns.insert(key, c);
                self.push(key, OP_RESPONSE, 0, Vec::new());
            }
            (OP_RESPONSE, Some(ConnState::Connecting)) => {
                self.conns.get_mut(&key).expect("present").state = ConnState::Connected;
            }
            (OP_RW, Some(ConnState::Connected)) => {
                let c = self.conns.get_mut(&key).expect("present");
                if payload.len() as u64 > u64::from(c.host_free()) {
                    self.stats.credit_violations += 1;
                    self.conns.remove(&key);
                    self.rst_reply(&h);
                } else {
                    c.rx_cnt = c.rx_cnt.wrapping_add(payload.len() as u32);
                    c.rx.extend(payload);
                }
            }
            (OP_CREDIT_UPDATE, Some(_)) => {}
            (OP_CREDIT_REQUEST, Some(_)) => self.push(key, OP_CREDIT_UPDATE, 0, Vec::new()),
            (OP_SHUTDOWN, Some(_)) => {
                let c = self.conns.get_mut(&key).expect("present");
                c.peer_shutdown |= h.flags & (SHUTDOWN_RCV | SHUTDOWN_SEND);
                if c.peer_shutdown == SHUTDOWN_RCV | SHUTDOWN_SEND {
                    self.conns.remove(&key);
                    self.rst_reply(&h);
                }
            }
            (OP_RST, _) => {
                self.conns.remove(&key);
            }
            _ => {
                self.conns.remove(&key);
                self.rst_reply(&h);
            }
        }
        true
    }
}

impl VirtioDevice for VsockDevice {
    fn device_type(&self) -> u32 {
        VIRTIO_ID_VSOCK
    }
    fn device_features(&self) -> u64 {
        0
    }
    fn queue_max_sizes(&self) -> &[u16] {
        &QUEUE_MAX
    }
    fn config_len(&self) -> usize {
        CONFIG_BYTES
    }
    fn read_config(&self, offset: usize, out: &mut [u8]) {
        let cfg = self.guest_cid.to_le_bytes();
        out.copy_from_slice(&cfg[offset..offset + out.len()]);
    }
    fn activate(&mut self, _features: u64) {}
    fn reset(&mut self) {
        self.conns.clear();
        self.outbox.clear();
        self.events.clear();
    }
    fn process(&mut self, queue: u16, io: &mut QueueIo<'_>) -> Result<(), DeviceError> {
        match queue {
            TX_QUEUE => {
                while self.outbox.len() < OUTBOX_LIMIT {
                    let Some(chain) = io.pop() else { break };
                    self.on_guest_packet(chain.readable());
                    io.complete(chain, &[])?;
                }
            }
            RX_QUEUE => {
                while let Some((h, payload)) = self.outbox.pop_front() {
                    let Some(chain) = io.pop() else {
                        self.outbox.push_front((h, payload));
                        break;
                    };
                    let room = chain.writable_len() as usize;
                    if room < HEADER_BYTES || !chain.readable().is_empty() {
                        io.unpop(chain);
                        self.outbox.push_front((h, payload));
                        return Err(DeviceError::Protocol("vsock rx buffer below 44 bytes"));
                    }
                    let n = payload.len().min(room - HEADER_BYTES);
                    let mut head = h;
                    head.len = n as u32;
                    let mut out = head.encode().to_vec();
                    out.extend_from_slice(&payload[..n]);
                    io.complete(chain, &out)?;
                    self.stats.packets_rx += 1;
                    if n < payload.len() {
                        self.outbox.push_front((h, payload[n..].to_vec()));
                    }
                }
            }
            EVENT_QUEUE => {
                while let Some(&event) = self.events.front() {
                    let Some(chain) = io.pop() else { break };
                    if chain.writable_len() < 4 {
                        io.unpop(chain);
                        return Err(DeviceError::Protocol("vsock event buffer below 4 bytes"));
                    }
                    io.complete(chain, &event.to_le_bytes())?;
                    self.events.pop_front();
                }
            }
            _ => return Err(DeviceError::Protocol("virtio-vsock has no such queue")),
        }
        Ok(())
    }
}
