//! virtio-net device model (virtio-v1.2 §5.1) with an in-memory backend.
//!
//! Queues: receive (0) and transmit (1); no control queue, no multiqueue.
//! Offered features: `VIRTIO_NET_F_MTU`, `VIRTIO_NET_F_MAC` and
//! `VIRTIO_NET_F_STATUS`. No checksum or segmentation offload is offered, so a
//! transmitted header must have `flags == 0` and `gso_type == GSO_NONE`;
//! frames that break that rule, are shorter than an Ethernet header or longer
//! than MTU + 14 are dropped and counted (and their buffers returned), as a
//! NIC drops bad frames. Without `VIRTIO_NET_F_MRG_RXBUF` each receive buffer
//! holds one whole frame; a buffer too small for the next frame is returned
//! empty and counted. Received frames wait in a bounded queue until the driver
//! posts buffers; overflow drops the newest frame and counts it.
#![forbid(unsafe_code)]

use super::mmio::{DeviceError, QueueIo, VirtioDevice};
use std::collections::VecDeque;

pub const VIRTIO_ID_NET: u32 = 1;
pub const VIRTIO_NET_F_MTU: u64 = 1 << 3;
pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;
pub const VIRTIO_NET_F_STATUS: u64 = 1 << 16;
pub const VIRTIO_NET_S_LINK_UP: u16 = 1;
/// `struct virtio_net_hdr` with `num_buffers` (always present under VERSION_1).
pub const NET_HDR_BYTES: usize = 12;
pub const VIRTIO_NET_HDR_GSO_NONE: u8 = 0;
pub const ETHERNET_HEADER_BYTES: usize = 14;
pub const RX_QUEUE: u16 = 0;
pub const TX_QUEUE: u16 = 1;
/// Frames held for the guest before drops start.
pub const RX_BACKLOG: usize = 256;
const QUEUE_MAX: [u16; 2] = [256, 256];
const CONFIG_BYTES: usize = 12;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetStats {
    pub tx_frames: u64,
    pub tx_dropped: u64,
    pub rx_frames: u64,
    pub rx_dropped_backlog: u64,
    pub rx_dropped_small_buffer: u64,
}

#[derive(Debug)]
pub struct NetDevice {
    mac: [u8; 6],
    mtu: u16,
    link_up: bool,
    config_changed: bool,
    features: u64,
    rx_backlog: VecDeque<Vec<u8>>,
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
            rx_backlog: VecDeque::new(),
            transmitted: VecDeque::new(),
            stats: NetStats::default(),
        })
    }
    pub fn stats(&self) -> NetStats {
        self.stats
    }
    fn max_frame(&self) -> usize {
        usize::from(self.mtu) + ETHERNET_HEADER_BYTES
    }
    /// Queues a frame for the guest; returns false (and counts) if dropped.
    pub fn inject_rx(&mut self, frame: &[u8]) -> bool {
        if frame.len() < ETHERNET_HEADER_BYTES
            || frame.len() > self.max_frame()
            || self.rx_backlog.len() >= RX_BACKLOG
        {
            self.stats.rx_dropped_backlog += 1;
            return false;
        }
        self.rx_backlog.push_back(frame.to_vec());
        true
    }
    pub fn rx_backlog(&self) -> usize {
        self.rx_backlog.len()
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

    fn transmit(&mut self, readable: &[u8]) {
        let ok = readable.len() >= NET_HDR_BYTES + ETHERNET_HEADER_BYTES
            && readable[0] == 0
            && readable[1] == VIRTIO_NET_HDR_GSO_NONE
            && readable.len() - NET_HDR_BYTES <= self.max_frame()
            && self.link_up;
        if ok {
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
        VIRTIO_NET_F_MTU | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS
    }
    fn queue_max_sizes(&self) -> &[u16] {
        &QUEUE_MAX
    }
    fn config_len(&self) -> usize {
        CONFIG_BYTES
    }
    fn read_config(&self, offset: usize, out: &mut [u8]) {
        let mut cfg = [0u8; CONFIG_BYTES];
        cfg[0..6].copy_from_slice(&self.mac);
        let status = if self.link_up {
            VIRTIO_NET_S_LINK_UP
        } else {
            0
        };
        cfg[6..8].copy_from_slice(&status.to_le_bytes());
        cfg[8..10].copy_from_slice(&1u16.to_le_bytes());
        cfg[10..12].copy_from_slice(&self.mtu.to_le_bytes());
        out.copy_from_slice(&cfg[offset..offset + out.len()]);
    }
    fn activate(&mut self, features: u64) {
        self.features = features;
    }
    fn reset(&mut self) {
        self.features = 0;
        self.rx_backlog.clear();
    }
    fn take_config_change(&mut self) -> bool {
        std::mem::take(&mut self.config_changed)
    }
    fn process(&mut self, queue: u16, io: &mut QueueIo<'_>) -> Result<(), DeviceError> {
        match queue {
            TX_QUEUE => {
                while let Some(chain) = io.pop() {
                    self.transmit(chain.readable());
                    io.complete(chain, &[])?;
                }
            }
            RX_QUEUE => {
                while let Some(frame) = self.rx_backlog.front() {
                    let Some(chain) = io.pop() else { break };
                    let need = (NET_HDR_BYTES + frame.len()) as u64;
                    if chain.writable_len() < need || !chain.readable().is_empty() {
                        self.stats.rx_dropped_small_buffer += 1;
                        io.complete(chain, &[])?;
                        continue;
                    }
                    let mut out = vec![0u8; NET_HDR_BYTES];
                    out[10..12].copy_from_slice(&1u16.to_le_bytes()); // num_buffers
                    out.extend_from_slice(frame);
                    io.complete(chain, &out)?;
                    self.rx_backlog.pop_front();
                    self.stats.rx_frames += 1;
                }
            }
            _ => return Err(DeviceError::Protocol("virtio-net has no such queue")),
        }
        Ok(())
    }
}
