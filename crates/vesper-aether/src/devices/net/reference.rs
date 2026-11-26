//! The Rust virtio-net device, kept only as the reference the Vesper
//! device (`super::NetDevice`, `vesper/src/net.ves`) is checked against
//! (src/port_reference_tests/net.rs).
#![allow(dead_code)]

use super::*;
use crate::devices::mmio::{DeviceError, QueueIo, VirtioDevice};
use std::collections::VecDeque;

#[derive(Debug)]
pub struct NetDevice {
    mac: [u8; 6],
    mtu: u16,
    link_up: bool,
    config_changed: bool,
    features: u64,
    max_pairs: u16,
    active_pairs: u16,
    queue_max: Vec<u16>,
    rx_backlog: Vec<VecDeque<Vec<u8>>>,
    transmitted: VecDeque<Vec<u8>>,
    stats: NetStats,
}

impl NetDevice {
    /// `mtu` is the largest payload after the Ethernet header (68..=9000).
    pub fn new(mac: [u8; 6], mtu: u16) -> Option<Self> {
        if !(68..=9000).contains(&mtu) {
            return None;
        }
        Some(Self {
            mac,
            mtu,
            link_up: true,
            config_changed: false,
            features: 0,
            max_pairs: 1,
            active_pairs: 1,
            queue_max: vec![QUEUE_SIZE_MAX; 2],
            rx_backlog: vec![VecDeque::new()],
            transmitted: VecDeque::new(),
            stats: NetStats::default(),
        })
    }
    /// A device with `pairs` receive/transmit pairs; more than one adds the
    /// control queue and offers `MQ` and `CTRL_VQ`. `None` outside
    /// 1..=[`MAX_QUEUE_PAIRS`].
    pub fn with_queue_pairs(mut self, pairs: u16) -> Option<Self> {
        if pairs == 0 || pairs > MAX_QUEUE_PAIRS {
            return None;
        }
        self.max_pairs = pairs;
        let queues = if pairs > 1 { 2 * pairs + 1 } else { 2 };
        self.queue_max = vec![QUEUE_SIZE_MAX; usize::from(queues)];
        self.rx_backlog = (0..pairs).map(|_| VecDeque::new()).collect();
        Some(self)
    }
    /// Pairs the driver has enabled (1 until `VQ_PAIRS_SET`).
    pub fn active_pairs(&self) -> u16 {
        self.active_pairs
    }
    /// The control queue's index under the negotiated features, if any.
    pub fn ctrl_queue(&self) -> Option<u16> {
        if self.features & VIRTIO_NET_F_CTRL_VQ == 0 {
            None
        } else if self.features & VIRTIO_NET_F_MQ != 0 {
            Some(2 * self.max_pairs)
        } else {
            Some(2)
        }
    }
    pub fn stats(&self) -> NetStats {
        self.stats
    }
    pub(crate) fn max_frame(&self) -> usize {
        usize::from(self.mtu) + ETHERNET_HEADER_BYTES
    }
    /// Queues a frame for the guest on pair 1; returns false (and counts) if
    /// dropped.
    pub fn inject_rx(&mut self, frame: &[u8]) -> bool {
        self.inject_rx_on(0, frame)
    }
    /// Queues a frame for receive queue `pair` (0-based).
    pub fn inject_rx_on(&mut self, pair: u16, frame: &[u8]) -> bool {
        if pair >= self.active_pairs {
            self.stats.rx_dropped_inactive += 1;
            return false;
        }
        let backlog = &mut self.rx_backlog[usize::from(pair)];
        if frame.len() < ETHERNET_HEADER_BYTES
            || frame.len() > usize::from(self.mtu) + ETHERNET_HEADER_BYTES
            || backlog.len() >= RX_BACKLOG
        {
            self.stats.rx_dropped_backlog += 1;
            return false;
        }
        backlog.push_back(frame.to_vec());
        true
    }
    /// Frames waiting for receive buffers, over all pairs.
    pub fn rx_backlog(&self) -> usize {
        self.rx_backlog.iter().map(VecDeque::len).sum()
    }
    /// The oldest frame the guest transmitted.
    pub fn take_tx(&mut self) -> Option<Vec<u8>> {
        self.transmitted.pop_front()
    }
    /// Changes the link state the guest sees (raises a config interrupt).
    pub fn set_link(&mut self, up: bool) {
        if self.link_up != up {
            self.link_up = up;
            self.config_changed = true;
        }
    }

    fn set_pairs(&mut self, pairs: u16) {
        self.active_pairs = pairs;
        for (i, b) in self.rx_backlog.iter_mut().enumerate() {
            if i >= usize::from(pairs) {
                self.stats.rx_dropped_inactive += b.len() as u64;
                b.clear();
            }
        }
    }

    /// One control command (§5.1.6.5): returns the ack byte.
    pub(crate) fn control(&mut self, readable: &[u8]) -> u8 {
        self.control_reference(readable)
    }

    /// The Rust implementation of `control`, exposed for differential tests.
    #[doc(hidden)]
    pub fn control_reference(&mut self, readable: &[u8]) -> u8 {
        let ok = match readable {
            [VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, lo, hi]
                if self.features & VIRTIO_NET_F_MQ != 0 =>
            {
                let pairs = u16::from_le_bytes([*lo, *hi]);
                if (1..=self.max_pairs).contains(&pairs) {
                    self.set_pairs(pairs);
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if ok {
            self.stats.ctrl_ok += 1;
            VIRTIO_NET_OK
        } else {
            self.stats.ctrl_err += 1;
            VIRTIO_NET_ERR
        }
    }

    /// Delivers pair `pair`'s backlog into posted receive chains.
    fn receive(&mut self, pair: usize, io: &mut QueueIo<'_>) -> Result<(), DeviceError> {
        let mergeable = self.features & VIRTIO_NET_F_MRG_RXBUF != 0;
        while let Some(frame) = self.rx_backlog[pair].front() {
            let need = (NET_HDR_BYTES + frame.len()) as u64;
            let Some(first) = io.pop() else { break };
            if !first.readable().is_empty() || (!mergeable && first.writable_len() < need) {
                self.stats.rx_dropped_small_buffer += 1;
                io.complete(first, &[])?;
                continue;
            }
            if first.writable_len() < NET_HDR_BYTES as u64 {
                io.unpop(first);
                return Err(DeviceError::Protocol(
                    "virtio-net receive buffer below the header",
                ));
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
            let frame = self.rx_backlog[pair].pop_front().expect("front exists");
            let mut out = vec![0u8; NET_HDR_BYTES];
            let count = chains.len() as u16;
            out[10..12].copy_from_slice(&count.to_le_bytes()); // num_buffers
            out.extend_from_slice(&frame);
            let mut rest: &[u8] = &out;
            for c in chains {
                let n = (c.writable_len() as usize).min(rest.len());
                io.complete(c, &rest[..n])?;
                rest = &rest[n..];
            }
            self.stats.rx_frames += 1;
            if count > 1 {
                self.stats.rx_merged += 1;
            }
        }
        Ok(())
    }

    /// Exposed for `tests/net_port.rs`.
    #[doc(hidden)]
    pub fn transmit_valid_reference(&self, readable: &[u8]) -> bool {
        readable.len() >= NET_HDR_BYTES + ETHERNET_HEADER_BYTES
            && readable[0] == 0
            && readable[1] == VIRTIO_NET_HDR_GSO_NONE
            && readable.len() - NET_HDR_BYTES <= self.max_frame()
            && self.link_up
    }

    /// Exposed for `tests/net_port.rs`.
    #[doc(hidden)]
    pub fn transmit_for_port_test(&mut self, readable: &[u8]) {
        self.transmit(readable);
    }

    /// Exposed for `tests/net_port.rs`.
    #[doc(hidden)]
    pub fn control_for_port_test(&mut self, readable: &[u8]) -> u8 {
        self.control(readable)
    }

    /// Exposed for `tests/net_port.rs`.
    #[doc(hidden)]
    pub fn read_config_reference(&self, offset: usize, out: &mut [u8]) {
        let mut cfg = [0u8; CONFIG_BYTES];
        cfg[0..6].copy_from_slice(&self.mac);
        let status = if self.link_up {
            VIRTIO_NET_S_LINK_UP
        } else {
            0
        };
        cfg[6..8].copy_from_slice(&status.to_le_bytes());
        cfg[8..10].copy_from_slice(&self.max_pairs.to_le_bytes());
        cfg[10..12].copy_from_slice(&self.mtu.to_le_bytes());
        out.copy_from_slice(&cfg[offset..offset + out.len()]);
    }

    fn transmit(&mut self, readable: &[u8]) {
        let ok = self.transmit_valid_reference(readable);
        if ok && self.transmitted.len() >= TX_BACKLOG {
            self.stats.tx_dropped_backlog += 1;
        } else if ok {
            self.transmitted
                .push_back(readable[NET_HDR_BYTES..].to_vec());
            self.stats.tx_frames += 1;
        } else {
            self.stats.tx_dropped += 1;
        }
    }
}

impl VirtioDevice for NetDevice {
    fn device_type(&self) -> u32 {
        VIRTIO_ID_NET
    }
    fn device_features(&self) -> u64 {
        VIRTIO_NET_F_MTU
            | VIRTIO_NET_F_MAC
            | VIRTIO_NET_F_STATUS
            | VIRTIO_NET_F_MRG_RXBUF
            | if self.max_pairs > 1 {
                VIRTIO_NET_F_MQ | VIRTIO_NET_F_CTRL_VQ
            } else {
                0
            }
    }
    fn queue_max_sizes(&self) -> &[u16] {
        &self.queue_max
    }
    fn config_len(&self) -> usize {
        CONFIG_BYTES
    }
    fn read_config(&self, offset: usize, out: &mut [u8]) {
        self.read_config_reference(offset, out);
    }

    fn activate(&mut self, features: u64) {
        self.features = features;
        self.active_pairs = 1;
    }
    fn reset(&mut self) {
        self.features = 0;
        self.active_pairs = 1;
        for b in &mut self.rx_backlog {
            b.clear();
        }
    }
    fn take_config_change(&mut self) -> bool {
        std::mem::take(&mut self.config_changed)
    }
    fn process(&mut self, queue: u16, io: &mut QueueIo<'_>) -> Result<(), DeviceError> {
        if Some(queue) == self.ctrl_queue() {
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
            return Ok(());
        }
        let pairs = if self.features & VIRTIO_NET_F_MQ != 0 {
            self.max_pairs
        } else {
            1
        };
        let pair = usize::from(queue / 2);
        if pair >= usize::from(pairs) {
            return Err(DeviceError::Protocol("virtio-net has no such queue"));
        }
        if queue % 2 == 1 {
            while let Some(chain) = io.pop() {
                self.transmit(chain.readable());
                io.complete(chain, &[])?;
            }
            Ok(())
        } else {
            self.receive(pair, io)
        }
    }
}
