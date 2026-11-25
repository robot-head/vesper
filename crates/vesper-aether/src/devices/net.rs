//! virtio-net device model (virtio-v1.2 §5.1) with an in-memory backend.
//!
//! Queues: receive (0) and transmit (1), or with [`NetDevice::with_queue_pairs`]
//! up to [`MAX_QUEUE_PAIRS`] receive/transmit pairs (`receiveqN` = 2(N-1),
//! `transmitqN` = 2(N-1)+1) plus the control queue (§5.1.2). Offered
//! features: `VIRTIO_NET_F_MTU`, `VIRTIO_NET_F_MAC`, `VIRTIO_NET_F_STATUS`,
//! `VIRTIO_NET_F_MRG_RXBUF`, and with more than one pair `VIRTIO_NET_F_MQ` and
//! `VIRTIO_NET_F_CTRL_VQ`.
//!
//! Multiqueue (§5.1.6.5.5): after `DRIVER_OK` only pair 1 is active; the
//! driver enables more with `VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET` on the control
//! queue (index 2N with `MQ`, 2 without), which the device acknowledges `OK`
//! for 1..=`max_virtqueue_pairs` and `ERR` otherwise (as for every command
//! class it does not implement). The host steers received frames to a pair
//! explicitly ([`NetDevice::inject_rx_on`]); frames for an inactive pair are
//! dropped and counted, and shrinking the active set drops the backlog of the
//! pairs it disables. Automatic flow steering is not implemented.
//!
//! Mergeable receive buffers (§5.1.6.3.1): with `MRG_RXBUF` a frame larger
//! than one receive chain is spread over as many chains as it needs, the
//! first carrying the header with `num_buffers` set to that count; if fewer
//! chains are posted than the frame needs, they are left posted and the frame
//! waits.
//!
//! No checksum or segmentation offload is offered, so a
//! transmitted header must have `flags == 0` and `gso_type == GSO_NONE`;
//! frames that break that rule, are shorter than an Ethernet header or longer
//! than MTU + 14 are dropped and counted (and their buffers returned), as a
//! NIC drops bad frames. Without `VIRTIO_NET_F_MRG_RXBUF` each receive chain
//! holds one whole frame; a buffer too small for the next frame is returned
//! empty and counted. Received frames wait in a bounded queue until the driver
//! posts buffers; overflow drops the newest frame and counts it.
#![forbid(unsafe_code)]

use super::mmio::{DeviceError, QueueIo, VirtioDevice};
use std::collections::VecDeque;

pub const VIRTIO_ID_NET: u32 = 1;
pub const VIRTIO_NET_F_MTU: u64 = 1 << 3;
pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;
pub const VIRTIO_NET_F_MRG_RXBUF: u64 = 1 << 15;
pub const VIRTIO_NET_F_STATUS: u64 = 1 << 16;
pub const VIRTIO_NET_F_CTRL_VQ: u64 = 1 << 17;
pub const VIRTIO_NET_F_MQ: u64 = 1 << 22;
/// Control queue command class and command for the active pair count.
pub const VIRTIO_NET_CTRL_MQ: u8 = 4;
pub const VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET: u8 = 0;
pub const VIRTIO_NET_OK: u8 = 0;
pub const VIRTIO_NET_ERR: u8 = 1;
/// Queue pairs a device can be built with.
pub const MAX_QUEUE_PAIRS: u16 = 8;
pub const VIRTIO_NET_S_LINK_UP: u16 = 1;
/// `struct virtio_net_hdr` with `num_buffers` (always present under VERSION_1).
pub const NET_HDR_BYTES: usize = 12;
pub const VIRTIO_NET_HDR_GSO_NONE: u8 = 0;
pub const ETHERNET_HEADER_BYTES: usize = 14;
pub const RX_QUEUE: u16 = 0;
pub const TX_QUEUE: u16 = 1;
/// Frames held for the guest before drops start.
pub const RX_BACKLOG: usize = 256;
/// Frames the guest may have transmitted that the host has not yet taken;
/// further frames are dropped (and counted) so a slow host consumer cannot be
/// made to grow memory without bound.
pub const TX_BACKLOG: usize = 256;
const QUEUE_SIZE_MAX: u16 = 256;
const CONFIG_BYTES: usize = 12;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetStats {
    pub tx_frames: u64,
    pub tx_dropped: u64,
    pub tx_dropped_backlog: u64,
    pub rx_frames: u64,
    pub rx_dropped_backlog: u64,
    pub rx_dropped_small_buffer: u64,
    /// Frames the host steered to a pair the driver has not enabled.
    pub rx_dropped_inactive: u64,
    /// Received frames that spanned more than one receive chain.
    pub rx_merged: u64,
    pub ctrl_ok: u64,
    pub ctrl_err: u64,
}

/// The virtio-net device. The Vesper port (`vesper/src/net.ves`) owns the
/// configuration, features, the multiqueue pairs, the control queue,
/// transmit validation, the receive plan, the backlog counts and the stats;
/// this side keeps the frame bytes and the queue loops.
#[derive(Debug)]
pub struct NetDevice {
    mtu: u16,
    state: crate::binding::NetState,
    queue_max: Vec<u16>,
    rx_backlog: Vec<VecDeque<Vec<u8>>>,
    transmitted: VecDeque<Vec<u8>>,
    /// One receive frame with its header, reused.
    scratch: Vec<u8>,
}

const TX_ACCEPT: u8 = 2;
const RX_SMALL: u8 = 0;
const RX_NO_HEADER: u8 = 1;
const RX_ONE: u8 = 2;

impl NetDevice {
    /// `mtu` is the largest payload after the Ethernet header (68..=9000).
    pub fn new(mac: [u8; 6], mtu: u16) -> Option<Self> {
        if !(68..=9000).contains(&mtu) {
            return None;
        }
        Some(Self {
            mtu,
            state: crate::binding::net_state_new(&mac, mtu, 1),
            queue_max: vec![QUEUE_SIZE_MAX; 2],
            rx_backlog: vec![VecDeque::new()],
            transmitted: VecDeque::new(),
            scratch: Vec::new(),
        })
    }
    /// A device with `pairs` receive/transmit pairs; more than one adds the
    /// control queue and offers `MQ` and `CTRL_VQ`. `None` outside
    /// 1..=[`MAX_QUEUE_PAIRS`].
    pub fn with_queue_pairs(mut self, pairs: u16) -> Option<Self> {
        if pairs == 0 || pairs > MAX_QUEUE_PAIRS {
            return None;
        }
        let mut cfg = [0u8; CONFIG_BYTES];
        crate::binding::net_config(&self.state, &mut cfg);
        self.state = crate::binding::net_state_new(&cfg[..6], self.mtu, pairs);
        let queues = if pairs > 1 { 2 * pairs + 1 } else { 2 };
        self.queue_max = vec![QUEUE_SIZE_MAX; usize::from(queues)];
        self.rx_backlog = (0..pairs).map(|_| VecDeque::new()).collect();
        Some(self)
    }
    /// Pairs the driver has enabled (1 until `VQ_PAIRS_SET`).
    pub fn active_pairs(&self) -> u16 {
        crate::binding::net_active_pairs(&self.state)
    }
    /// The control queue's index under the negotiated features, if any.
    pub fn ctrl_queue(&self) -> Option<u16> {
        u16::try_from(crate::binding::net_ctrl_queue(&self.state)).ok()
    }
    pub fn stats(&self) -> NetStats {
        crate::binding::net_stats(&self.state)
    }
    /// Queues a frame for the guest on pair 1; returns false (and counts) if
    /// dropped.
    pub fn inject_rx(&mut self, frame: &[u8]) -> bool {
        self.inject_rx_on(0, frame)
    }
    /// Queues a frame for receive queue `pair` (0-based).
    pub fn inject_rx_on(&mut self, pair: u16, frame: &[u8]) -> bool {
        if !crate::binding::net_inject(&mut self.state, pair, frame.len() as u64) {
            return false;
        }
        self.rx_backlog[usize::from(pair)].push_back(frame.to_vec());
        true
    }
    /// Frames waiting for receive buffers, over all pairs.
    pub fn rx_backlog(&self) -> usize {
        crate::binding::net_rx_backlog(&self.state) as usize
    }
    /// The oldest frame the guest transmitted.
    pub fn take_tx(&mut self) -> Option<Vec<u8>> {
        self.transmitted.pop_front()
    }
    /// Changes the link state the guest sees (raises a config interrupt).
    pub fn set_link(&mut self, up: bool) {
        crate::binding::net_set_link(&mut self.state, up);
    }

    /// One control command (§5.1.6.5): returns the ack byte.
    pub(crate) fn control(&mut self, readable: &[u8]) -> u8 {
        let got = crate::binding::net_control(&mut self.state, readable);
        let pairs = usize::from((got >> 8) as u16);
        for b in self.rx_backlog.iter_mut().skip(pairs) {
            b.clear();
        }
        got as u8
    }

    fn transmit(&mut self, readable: &[u8]) {
        let verdict =
            crate::binding::net_tx(&mut self.state, readable, self.transmitted.len() as u64);
        if verdict == TX_ACCEPT {
            self.transmitted
                .push_back(readable[NET_HDR_BYTES..].to_vec());
        }
    }

    /// Delivers pair `pair`'s backlog into posted receive chains.
    fn receive(&mut self, pair: u16, io: &mut QueueIo<'_>) -> Result<(), DeviceError> {
        let p = usize::from(pair);
        while let Some(frame) = self.rx_backlog[p].front() {
            let need = (NET_HDR_BYTES + frame.len()) as u64;
            let Some(first) = io.pop() else { break };
            let plan = crate::binding::net_rx_first(
                &mut self.state,
                pair,
                frame.len() as u64,
                first.readable().is_empty(),
                first.writable_len(),
            );
            if plan == RX_SMALL {
                io.complete(first, &[])?;
                continue;
            }
            if plan == RX_NO_HEADER {
                io.unpop(first);
                return Err(DeviceError::Protocol(
                    "virtio-net receive buffer below the header",
                ));
            }
            if plan == RX_ONE {
                let frame = self.rx_backlog[p].pop_front().expect("front exists");
                self.scratch.clear();
                self.scratch.extend_from_slice(&[0; NET_HDR_BYTES]);
                self.scratch[10..12].copy_from_slice(&1u16.to_le_bytes()); // num_buffers
                self.scratch.extend_from_slice(&frame);
                io.complete(first, &self.scratch)?;
                continue;
            }
            // Collect enough chains for the whole frame (MRG_RXBUF).
            let mut chains = vec![first];
            let mut room: u64 = chains[0].writable_len();
            while room < need {
                match io.pop() {
                    Some(c) if c.readable().is_empty() && c.writable_len() > 0 => {
                        room += c.writable_len();
                        chains.push(c);
                    }
                    Some(c) => {
                        // A receive buffer with a readable part, or none at
                        // all, cannot be completed out of order: give
                        // everything back and stop the device.
                        io.unpop(c);
                        while let Some(c) = chains.pop() {
                            io.unpop(c);
                        }
                        return Err(DeviceError::Protocol(
                            "virtio-net mergeable receive buffer is not device-writable",
                        ));
                    }
                    None => break,
                }
            }
            if room < need {
                // Not enough buffers yet: leave them posted, in order.
                while let Some(c) = chains.pop() {
                    io.unpop(c);
                }
                break;
            }
            let frame = self.rx_backlog[p].pop_front().expect("front exists");
            let count = chains.len() as u16;
            self.scratch.clear();
            self.scratch.extend_from_slice(&[0; NET_HDR_BYTES]);
            self.scratch[10..12].copy_from_slice(&count.to_le_bytes()); // num_buffers
            self.scratch.extend_from_slice(&frame);
            let mut rest: &[u8] = &self.scratch;
            for c in chains {
                let n = (c.writable_len() as usize).min(rest.len());
                io.complete(c, &rest[..n])?;
                rest = &rest[n..];
            }
            crate::binding::net_rx_merged(&mut self.state, pair, u64::from(count));
        }
        Ok(())
    }

    /// One transmitted chain, outside the queues.
    /// Exposed for `tests/net_port.rs`.
    #[doc(hidden)]
    pub fn transmit_for_port_test(&mut self, readable: &[u8]) {
        self.transmit(readable);
    }

    /// One control command, outside the queues.
    /// Exposed for `tests/net_port.rs`.
    #[doc(hidden)]
    pub fn control_for_port_test(&mut self, readable: &[u8]) -> u8 {
        self.control(readable)
    }
}

impl VirtioDevice for NetDevice {
    fn device_type(&self) -> u32 {
        VIRTIO_ID_NET
    }
    fn device_features(&self) -> u64 {
        crate::binding::net_device_features(&self.state)
    }
    fn queue_max_sizes(&self) -> &[u16] {
        &self.queue_max
    }
    fn config_len(&self) -> usize {
        CONFIG_BYTES
    }
    fn read_config(&self, offset: usize, out: &mut [u8]) {
        let mut cfg = [0u8; CONFIG_BYTES];
        crate::binding::net_config(&self.state, &mut cfg);
        out.copy_from_slice(&cfg[offset..offset + out.len()]);
    }
    fn activate(&mut self, features: u64) {
        crate::binding::net_activate(&mut self.state, features);
    }
    fn reset(&mut self) {
        crate::binding::net_reset(&mut self.state);
        for b in &mut self.rx_backlog {
            b.clear();
        }
    }
    fn take_config_change(&mut self) -> bool {
        crate::binding::net_take_config_change(&mut self.state)
    }
    fn process(&mut self, queue: u16, io: &mut QueueIo<'_>) -> Result<(), DeviceError> {
        let kind = crate::binding::net_queue(&self.state, queue);
        match kind & 0xff {
            0 => {
                while let Some(chain) = io.pop() {
                    let w = chain.writable_len();
                    if w == 0 {
                        io.unpop(chain);
                        return Err(DeviceError::Protocol("virtio-net control without ack byte"));
                    }
                    let ack = self.control(chain.readable());
                    // The ack is the last device-writable byte.
                    let mut out = vec![0u8; w as usize - 1];
                    out.push(ack);
                    io.complete(chain, &out)?;
                }
                Ok(())
            }
            1 => {
                while let Some(chain) = io.pop() {
                    self.transmit(chain.readable());
                    io.complete(chain, &[])?;
                }
                Ok(())
            }
            2 => self.receive((kind >> 8) as u16, io),
            _ => Err(DeviceError::Protocol("virtio-net has no such queue")),
        }
    }
}

#[cfg(test)]
mod reference;
#[cfg(test)]
pub(crate) use reference::NetDevice as ReferenceNetDevice;
