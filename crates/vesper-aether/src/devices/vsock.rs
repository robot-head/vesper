//! virtio-vsock device model (virtio-v1.2 §5.10) with a host-side socket
//! table: stream sockets, and seqpacket sockets with `VIRTIO_VSOCK_F_SEQPACKET`.
//!
//! Queues: receive (0), transmit (1) and event (2). The device offers
//! `VIRTIO_VSOCK_F_SEQPACKET`; without it only `VIRTIO_VSOCK_TYPE_STREAM` is
//! valid and a seqpacket packet gets `RST`. A connection keeps the type it was
//! opened with: a request for a listener of the other type, or a packet of the
//! other type on an open connection, is answered with `RST`. Seqpacket `RW`
//! packets carry message boundaries (§5.10.6.3.1): the device reassembles a
//! guest message until a packet with `VIRTIO_VSOCK_SEQ_EOM` and hands it to the
//! host whole ([`VsockDevice::recv_message`]), keeping `SEQ_EOR`; a host
//! message ([`VsockDevice::send_message`]) is queued only if the guest's credit
//! and the outbox take all of it, split into packets of at most 4 KiB with
//! `EOM` (and `EOR` if asked) on the last, and when a receive buffer splits a
//! packet the boundary flags stay on the final part only. The host is CID 2.
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
//!
//! The device runs in Vesper (`vesper/src/vsock.ves`): one call per guest
//! packet, receive buffer or host socket call. This side keeps the queue
//! loops and the bytes each connection received until the host takes them.
//! The Rust device it replaced is the test reference
//! (`vsock/reference.rs`, src/port_reference_tests/vsock.rs).
#![forbid(unsafe_code)]

use super::mmio::{DeviceError, QueueIo, VirtioDevice};
use std::collections::VecDeque;

pub const VIRTIO_ID_VSOCK: u32 = 19;
pub const HOST_CID: u64 = 2;
pub const HEADER_BYTES: usize = 44;
pub const MAX_PAYLOAD: usize = 64 * 1024;
pub const TYPE_STREAM: u16 = 1;
pub const TYPE_SEQPACKET: u16 = 2;
pub const VIRTIO_VSOCK_F_SEQPACKET: u64 = 1 << 1;
/// Seqpacket `RW` flags: end of message, end of record.
pub const SEQ_EOM: u32 = 1;
pub const SEQ_EOR: u32 = 2;
/// Largest packet payload the host sends.
pub const HOST_PACKET_BYTES: usize = 4096;
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
/// Complete seqpacket messages a connection holds for the host. Credit only
/// counts payload bytes, so empty messages would otherwise be unbounded; a
/// guest that goes past this is reset.
pub const MAX_QUEUED_MESSAGES: usize = 256;
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
        #[cfg(feature = "vesper-port")]
        {
            let mut b = [0u8; HEADER_BYTES];
            crate::binding::vsock_encode_hdr(*self, &mut b);
            b
        }
        #[cfg(not(feature = "vesper-port"))]
        self.encode_reference()
    }
    #[doc(hidden)]
    pub fn encode_reference(&self) -> [u8; HEADER_BYTES] {
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
        #[cfg(feature = "vesper-port")]
        return crate::binding::vsock_decode_hdr(b);
        #[cfg(not(feature = "vesper-port"))]
        Self::decode_reference(b)
    }
    #[doc(hidden)]
    pub fn decode_reference(b: &[u8]) -> Option<Self> {
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

/// One reassembled seqpacket message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeqMessage {
    pub data: Vec<u8>,
    /// The last packet carried `SEQ_EOR`.
    pub eor: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VsockError {
    InvalidCid,
    PortInUse,
    NoSuchConnection,
    NotConnected,
    TooManyConnections,
    OutboxFull,
    /// A stream call on a seqpacket connection or vice versa, or seqpacket
    /// before the driver negotiated `VIRTIO_VSOCK_F_SEQPACKET`.
    WrongType,
    /// The guest's credit or the outbox cannot take the whole message now.
    WouldBlock,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VsockStats {
    pub malformed: u64,
    pub spoofed: u64,
    pub resets_sent: u64,
    pub credit_violations: u64,
    /// Seqpacket connections reset for queueing more than
    /// [`MAX_QUEUED_MESSAGES`] unread messages.
    pub message_overflows: u64,
    pub packets_rx: u64,
    pub packets_tx: u64,
    pub messages_rx: u64,
    pub messages_tx: u64,
}

#[cfg(test)]
mod reference;
#[cfg(test)]
pub(crate) use reference::VsockDevice as ReferenceVsockDevice;

/// Bytes the guest sent one connection, kept by slot for the host.
#[derive(Debug, Default)]
struct SlotBytes {
    rx: VecDeque<u8>,
    messages: VecDeque<SeqMessage>,
    partial: Vec<u8>,
}

impl SlotBytes {
    fn clear(&mut self) {
        self.rx.clear();
        self.messages.clear();
        self.partial.clear();
    }
}

/// The virtio-vsock device. The Vesper port (`vesper/src/vsock.ves`) owns the
/// listeners, the connection table, credit, the outbox and the event queue;
/// this side keeps the bytes each connection received until the host takes
/// them.
#[derive(Debug)]
pub struct VsockDevice {
    guest_cid: u64,
    state: crate::binding::VsockState,
    slots: Vec<SlotBytes>,
    /// One receive packet as the port writes it.
    scratch: Box<[u8; HEADER_BYTES + HOST_PACKET_BYTES]>,
}

const ACT_FULL: u64 = 1 << 63;

fn vsock_error(code: u8) -> VsockError {
    match code {
        1 => VsockError::InvalidCid,
        2 => VsockError::PortInUse,
        3 => VsockError::NoSuchConnection,
        4 => VsockError::NotConnected,
        5 => VsockError::TooManyConnections,
        6 => VsockError::OutboxFull,
        7 => VsockError::WrongType,
        _ => VsockError::WouldBlock,
    }
}

fn vsock_result(code: u8) -> Result<(), VsockError> {
    if code == 0 {
        Ok(())
    } else {
        Err(vsock_error(code))
    }
}

impl VsockDevice {
    /// Guest CIDs 0, 1, 2 and `u32::MAX` are reserved; the upper 32 bits must be 0.
    pub fn new(guest_cid: u64) -> Result<Self, VsockError> {
        if guest_cid <= HOST_CID || guest_cid >= u64::from(u32::MAX) {
            return Err(VsockError::InvalidCid);
        }
        Ok(Self {
            guest_cid,
            state: crate::binding::vsock_state_new(guest_cid),
            slots: Vec::new(),
            scratch: Box::new([0; HEADER_BYTES + HOST_PACKET_BYTES]),
        })
    }
    pub fn stats(&self) -> VsockStats {
        crate::binding::vsock_stats(&self.state)
    }
    pub fn listen(&mut self, host_port: u32) -> Result<(), VsockError> {
        vsock_result(crate::binding::vsock_listen(
            &mut self.state,
            host_port,
            TYPE_STREAM,
        ))
    }
    /// A seqpacket listener (usable once the driver negotiated SEQPACKET).
    pub fn listen_seqpacket(&mut self, host_port: u32) -> Result<(), VsockError> {
        vsock_result(crate::binding::vsock_listen(
            &mut self.state,
            host_port,
            TYPE_SEQPACKET,
        ))
    }
    pub fn connection_state(&self, key: ConnKey) -> Option<ConnState> {
        match crate::binding::vsock_connection_state(&self.state, key.0, key.1) {
            1 => Some(ConnState::Connecting),
            2 => Some(ConnState::Connected),
            _ => None,
        }
    }
    pub fn connections(&self) -> Vec<ConnKey> {
        crate::binding::vsock_connections(&self.state)
            .into_iter()
            .map(|k| ((k >> 32) as u32, k as u32))
            .collect()
    }
    pub fn outbox_len(&self) -> usize {
        crate::binding::vsock_outbox_len(&self.state) as usize
    }
    fn slot(&mut self, slot: u64) -> &mut SlotBytes {
        let slot = slot as usize;
        if slot >= self.slots.len() {
            self.slots.resize_with(slot + 1, SlotBytes::default);
        }
        &mut self.slots[slot]
    }
    fn open(&mut self, host_port: u32, guest_port: u32, kind: u16) -> Result<ConnKey, VsockError> {
        let (code, slot) =
            crate::binding::vsock_connect(&mut self.state, host_port, guest_port, kind);
        vsock_result(code)?;
        self.slot(slot).clear();
        Ok((guest_port, host_port))
    }
    /// Host-initiated connection to a guest listener on `guest_port`.
    pub fn connect(&mut self, host_port: u32, guest_port: u32) -> Result<ConnKey, VsockError> {
        self.open(host_port, guest_port, TYPE_STREAM)
    }
    /// Host-initiated seqpacket connection.
    pub fn connect_seqpacket(
        &mut self,
        host_port: u32,
        guest_port: u32,
    ) -> Result<ConnKey, VsockError> {
        self.open(host_port, guest_port, TYPE_SEQPACKET)
    }
    /// Connection `key`'s slot, if it has type `kind`.
    fn typed_slot(&self, key: ConnKey, kind: u16) -> Result<u64, VsockError> {
        let found = crate::binding::vsock_lookup(&self.state, key.0, key.1);
        if found == u64::MAX {
            return Err(VsockError::NoSuchConnection);
        }
        if (found >> 32) as u16 != kind {
            return Err(VsockError::WrongType);
        }
        Ok(found & u64::from(u32::MAX))
    }
    /// Takes everything the guest sent on stream `key` and returns credit.
    pub fn recv(&mut self, key: ConnKey) -> Result<Vec<u8>, VsockError> {
        let slot = self.typed_slot(key, TYPE_STREAM)?;
        let data: Vec<u8> = self.slot(slot).rx.drain(..).collect();
        crate::binding::vsock_forwarded(
            &mut self.state,
            key.0,
            key.1,
            slot,
            data.len() as u64,
            false,
        );
        Ok(data)
    }
    /// The oldest complete message the guest sent on seqpacket `key`, which
    /// returns its bytes as credit.
    pub fn recv_message(&mut self, key: ConnKey) -> Result<Option<SeqMessage>, VsockError> {
        let slot = self.typed_slot(key, TYPE_SEQPACKET)?;
        let Some(msg) = self.slot(slot).messages.pop_front() else {
            return Ok(None);
        };
        crate::binding::vsock_forwarded(
            &mut self.state,
            key.0,
            key.1,
            slot,
            msg.data.len() as u64,
            true,
        );
        Ok(Some(msg))
    }
    /// Queues one whole message on seqpacket `key`, or nothing
    /// ([`VsockError::WouldBlock`]) if the guest's credit or the outbox
    /// cannot take all of it now.
    pub fn send_message(&mut self, key: ConnKey, data: &[u8], eor: bool) -> Result<(), VsockError> {
        vsock_result(crate::binding::vsock_send_message(
            &mut self.state,
            key.0,
            key.1,
            data,
            eor,
        ))
    }
    /// Sends up to the guest's credit; returns the number of bytes queued.
    pub fn send(&mut self, key: ConnKey, data: &[u8]) -> Result<usize, VsockError> {
        let sent = crate::binding::vsock_send(&mut self.state, key.0, key.1, data);
        if sent & ACT_FULL != 0 {
            return Err(vsock_error(sent as u8));
        }
        Ok(sent as usize)
    }
    /// Shuts both directions down; the guest answers with RST.
    pub fn shutdown(&mut self, key: ConnKey) -> Result<(), VsockError> {
        vsock_result(crate::binding::vsock_shutdown(
            &mut self.state,
            key.0,
            key.1,
        ))
    }
    /// After a snapshot restore or fork: drop every connection and pending
    /// packet and tell the guest (`VIRTIO_VSOCK_EVENT_TRANSPORT_RESET`).
    pub fn transport_reset(&mut self) {
        crate::binding::vsock_transport_reset(&mut self.state);
        self.slots.clear();
    }

    /// One guest packet; returns whether the outbox reached its limit.
    fn on_guest_packet(&mut self, bytes: &[u8]) -> bool {
        let act = crate::binding::vsock_guest_packet(&mut self.state, bytes);
        let code = act as u8;
        if code != 0 {
            let payload = &bytes[HEADER_BYTES..];
            let slot = self.slot((act & !ACT_FULL) >> 8);
            match code {
                1 => slot.rx.extend(payload),
                2 => slot.partial.extend_from_slice(payload),
                3 | 4 => {
                    slot.partial.extend_from_slice(payload);
                    let data = std::mem::take(&mut slot.partial);
                    slot.messages.push_back(SeqMessage {
                        data,
                        eor: code == 4,
                    });
                }
                _ => slot.clear(),
            }
        }
        act & ACT_FULL != 0
    }

    /// One receive-queue step outside the queues: the next packet for a
    /// buffer of `room` bytes (at least 44) in `out`; false when the outbox
    /// is empty.
    #[cfg(test)]
    pub(crate) fn rx_step_for_test(&mut self, room: usize, out: &mut Vec<u8>) -> bool {
        out.clear();
        if self.outbox_len() == 0 {
            return false;
        }
        let got =
            crate::binding::vsock_rx_fill(&mut self.state, room as u64, &mut self.scratch[..]);
        out.extend_from_slice(&self.scratch[..(got & !ACT_FULL) as usize]);
        true
    }

    /// One guest packet, outside the queues.
    /// Exposed for `tests/vsock_port.rs`.
    #[doc(hidden)]
    pub fn deliver_tx_for_port_test(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
        let _ = self.on_guest_packet(bytes);
        Ok(())
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
        crate::binding::vsock_activate(&mut self.state, features);
    }
    fn reset(&mut self) {
        crate::binding::vsock_reset(&mut self.state);
        self.slots.clear();
    }
    fn process(&mut self, queue: u16, io: &mut QueueIo<'_>) -> Result<(), DeviceError> {
        match queue {
            TX_QUEUE => {
                if self.outbox_len() >= OUTBOX_LIMIT {
                    return Ok(());
                }
                while let Some(chain) = io.pop() {
                    let full = self.on_guest_packet(chain.readable());
                    io.complete(chain, &[])?;
                    if full {
                        break;
                    }
                }
            }
            RX_QUEUE => {
                if self.outbox_len() == 0 {
                    return Ok(());
                }
                while let Some(chain) = io.pop() {
                    let room = chain.writable_len() as usize;
                    if room < HEADER_BYTES || !chain.readable().is_empty() {
                        io.unpop(chain);
                        return Err(DeviceError::Protocol("vsock rx buffer below 44 bytes"));
                    }
                    let got = crate::binding::vsock_rx_fill(
                        &mut self.state,
                        room as u64,
                        &mut self.scratch[..],
                    );
                    let n = (got & !ACT_FULL) as usize;
                    io.complete(chain, &self.scratch[..n])?;
                    if got & ACT_FULL == 0 {
                        break;
                    }
                }
            }
            EVENT_QUEUE => {
                while crate::binding::vsock_events(&self.state) > 0 {
                    let Some(chain) = io.pop() else { break };
                    if chain.writable_len() < 4 {
                        io.unpop(chain);
                        return Err(DeviceError::Protocol("vsock event buffer below 4 bytes"));
                    }
                    io.complete(chain, &EVENT_TRANSPORT_RESET.to_le_bytes())?;
                    crate::binding::vsock_event_taken(&mut self.state);
                }
            }
            _ => return Err(DeviceError::Protocol("virtio-vsock has no such queue")),
        }
        Ok(())
    }
}
