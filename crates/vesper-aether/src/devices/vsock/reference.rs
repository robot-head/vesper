//! The Rust virtio-vsock device, kept only as the reference the Vesper
//! device (`super::VsockDevice`, `vesper/src/vsock.ves`) is checked against
//! (src/port_reference_tests/vsock.rs).
#![allow(dead_code)]

use super::*;
use crate::devices::mmio::{DeviceError, QueueIo, VirtioDevice};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug)]
struct Conn {
    kind: u16,
    state: ConnState,
    peer_buf_alloc: u32,
    peer_fwd_cnt: u32,
    tx_cnt: u32,
    rx_cnt: u32,
    fwd_cnt: u32,
    rx: VecDeque<u8>,
    /// Seqpacket: complete messages, and the one being reassembled.
    messages: VecDeque<SeqMessage>,
    partial: Vec<u8>,
    peer_shutdown: u32,
}
impl Conn {
    fn new(kind: u16, state: ConnState) -> Self {
        Self {
            kind,
            state,
            peer_buf_alloc: 0,
            peer_fwd_cnt: 0,
            tx_cnt: 0,
            rx_cnt: 0,
            fwd_cnt: 0,
            rx: VecDeque::new(),
            messages: VecDeque::new(),
            partial: Vec::new(),
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

#[derive(Debug)]
pub struct VsockDevice {
    guest_cid: u64,
    features: u64,
    /// Host port -> socket type.
    listeners: BTreeMap<u32, u16>,
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
            features: 0,
            listeners: BTreeMap::new(),
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
        self.listen_typed(host_port, TYPE_STREAM)
    }
    /// A seqpacket listener (usable once the driver negotiated SEQPACKET).
    pub fn listen_seqpacket(&mut self, host_port: u32) -> Result<(), VsockError> {
        self.listen_typed(host_port, TYPE_SEQPACKET)
    }
    fn listen_typed(&mut self, host_port: u32, kind: u16) -> Result<(), VsockError> {
        if self.listeners.contains_key(&host_port) {
            return Err(VsockError::PortInUse);
        }
        self.listeners.insert(host_port, kind);
        Ok(())
    }
    fn seqpacket(&self) -> bool {
        self.features & VIRTIO_VSOCK_F_SEQPACKET != 0
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
            kind: self.conns.get(&key).map_or(TYPE_STREAM, |c| c.kind),
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
        self.connect_typed(host_port, guest_port, TYPE_STREAM)
    }
    /// Host-initiated seqpacket connection.
    pub fn connect_seqpacket(
        &mut self,
        host_port: u32,
        guest_port: u32,
    ) -> Result<ConnKey, VsockError> {
        if !self.seqpacket() {
            return Err(VsockError::WrongType);
        }
        self.connect_typed(host_port, guest_port, TYPE_SEQPACKET)
    }
    fn connect_typed(
        &mut self,
        host_port: u32,
        guest_port: u32,
        kind: u16,
    ) -> Result<ConnKey, VsockError> {
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
        self.conns
            .insert(key, Conn::new(kind, ConnState::Connecting));
        self.push(key, OP_REQUEST, 0, Vec::new());
        Ok(key)
    }
    /// Takes everything the guest sent on stream `key` and returns credit.
    pub fn recv(&mut self, key: ConnKey) -> Result<Vec<u8>, VsockError> {
        let conn = self
            .conns
            .get_mut(&key)
            .ok_or(VsockError::NoSuchConnection)?;
        if conn.kind != TYPE_STREAM {
            return Err(VsockError::WrongType);
        }
        let data: Vec<u8> = conn.rx.drain(..).collect();
        conn.fwd_cnt = conn.fwd_cnt.wrapping_add(data.len() as u32);
        if !data.is_empty() && self.outbox.len() < OUTBOX_LIMIT {
            self.push(key, OP_CREDIT_UPDATE, 0, Vec::new());
        }
        Ok(data)
    }
    /// The oldest complete message the guest sent on seqpacket `key`, which
    /// returns its bytes as credit.
    pub fn recv_message(&mut self, key: ConnKey) -> Result<Option<SeqMessage>, VsockError> {
        let conn = self
            .conns
            .get_mut(&key)
            .ok_or(VsockError::NoSuchConnection)?;
        if conn.kind != TYPE_SEQPACKET {
            return Err(VsockError::WrongType);
        }
        let Some(msg) = conn.messages.pop_front() else {
            return Ok(None);
        };
        conn.fwd_cnt = conn.fwd_cnt.wrapping_add(msg.data.len() as u32);
        if !msg.data.is_empty() && self.outbox.len() < OUTBOX_LIMIT {
            self.push(key, OP_CREDIT_UPDATE, 0, Vec::new());
        }
        Ok(Some(msg))
    }
    /// Queues one whole message on seqpacket `key`, or nothing
    /// ([`VsockError::WouldBlock`]) if the guest's credit or the outbox
    /// cannot take all of it now.
    pub fn send_message(&mut self, key: ConnKey, data: &[u8], eor: bool) -> Result<(), VsockError> {
        let conn = self.conns.get(&key).ok_or(VsockError::NoSuchConnection)?;
        if conn.kind != TYPE_SEQPACKET {
            return Err(VsockError::WrongType);
        }
        if conn.state != ConnState::Connected || conn.peer_shutdown & SHUTDOWN_RCV != 0 {
            return Err(VsockError::NotConnected);
        }
        let packets = data.len().div_ceil(HOST_PACKET_BYTES).max(1);
        if (conn.guest_credit() as usize) < data.len() || self.outbox.len() + packets > OUTBOX_LIMIT
        {
            return Err(VsockError::WouldBlock);
        }
        let conn = self.conns.get_mut(&key).expect("checked above");
        conn.tx_cnt = conn.tx_cnt.wrapping_add(data.len() as u32);
        let mut parts: Vec<&[u8]> = data.chunks(HOST_PACKET_BYTES).collect();
        if parts.is_empty() {
            parts.push(&[]);
        }
        let last = parts.len() - 1;
        for (i, part) in parts.into_iter().enumerate() {
            let flags = if i == last {
                SEQ_EOM | if eor { SEQ_EOR } else { 0 }
            } else {
                0
            };
            self.push(key, OP_RW, flags, part.to_vec());
        }
        self.stats.messages_tx += 1;
        Ok(())
    }
    /// Sends up to the guest's credit; returns the number of bytes queued.
    pub fn send(&mut self, key: ConnKey, data: &[u8]) -> Result<usize, VsockError> {
        let conn = self.conns.get(&key).ok_or(VsockError::NoSuchConnection)?;
        if conn.kind != TYPE_STREAM {
            return Err(VsockError::WrongType);
        }
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

    /// One receive-queue step outside the queues, as `process` does it.
    #[cfg(test)]
    pub(crate) fn rx_step_for_test(&mut self, room: usize, out: &mut Vec<u8>) -> bool {
        out.clear();
        let Some((h, payload)) = self.outbox.pop_front() else {
            return false;
        };
        let n = payload.len().min(room - HEADER_BYTES);
        let mut head = h;
        head.len = n as u32;
        if n < payload.len() {
            head.flags &= !(SEQ_EOM | SEQ_EOR);
        }
        out.extend_from_slice(&head.encode_reference());
        out.extend_from_slice(&payload[..n]);
        self.stats.packets_rx += 1;
        if n < payload.len() {
            self.outbox.push_front((h, payload[n..].to_vec()));
        }
        true
    }

    /// One guest packet. Returns false if malformed (dropped).
    /// Exposed for `tests/vsock_port.rs`.
    #[doc(hidden)]
    pub fn deliver_tx_for_port_test(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
        let _ = self.on_guest_packet(bytes);
        Ok(())
    }

    fn on_guest_packet(&mut self, bytes: &[u8]) -> bool {
        let Some(h) = VsockHeader::decode_reference(bytes) else {
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
        if h.kind != TYPE_STREAM && !(h.kind == TYPE_SEQPACKET && self.seqpacket()) {
            self.rst_reply(&h);
            return true;
        }
        let key = (h.src_port, h.dst_port);
        if self.conns.get(&key).is_some_and(|c| c.kind != h.kind) {
            self.conns.remove(&key);
            self.rst_reply(&h);
            return true;
        }
        if let Some(c) = self.conns.get_mut(&key) {
            c.peer_buf_alloc = h.buf_alloc;
            c.peer_fwd_cnt = h.fwd_cnt;
        }
        let state = self.conns.get(&key).map(|c| c.state);
        match (h.op, state) {
            (OP_REQUEST, None)
                if self.listeners.get(&h.dst_port) == Some(&h.kind)
                    && self.conns.len() < MAX_CONNECTIONS =>
            {
                let mut c = Conn::new(h.kind, ConnState::Connected);
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
                } else if c.kind == TYPE_SEQPACKET
                    && h.flags & SEQ_EOM != 0
                    && c.messages.len() >= MAX_QUEUED_MESSAGES
                {
                    self.stats.message_overflows += 1;
                    self.conns.remove(&key);
                    self.rst_reply(&h);
                } else {
                    c.rx_cnt = c.rx_cnt.wrapping_add(payload.len() as u32);
                    if c.kind == TYPE_SEQPACKET {
                        c.partial.extend_from_slice(payload);
                        if h.flags & SEQ_EOM != 0 {
                            c.messages.push_back(SeqMessage {
                                data: std::mem::take(&mut c.partial),
                                eor: h.flags & SEQ_EOR != 0,
                            });
                            self.stats.messages_rx += 1;
                        }
                    } else {
                        c.rx.extend(payload);
                    }
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
        VIRTIO_VSOCK_F_SEQPACKET
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
    fn activate(&mut self, features: u64) {
        self.features = features;
    }
    fn reset(&mut self) {
        self.features = 0;
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
                    if n < payload.len() {
                        // Message boundaries belong to the final part only.
                        head.flags &= !(SEQ_EOM | SEQ_EOR);
                    }
                    let mut out = head.encode_reference().to_vec();
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
