//! The Rust MMIO transport the Vesper register file replaced (SH6 lane 14,
//! #1409), kept only as the reference of src/port_reference_tests/mmio.rs.
#![allow(dead_code)]

use super::*;

#[derive(Debug)]
struct ActiveQueue {
    queue: VirtQueue,
    pending: VecDeque<Chain>,
}

#[derive(Debug)]
struct QueueSlot {
    max: u16,
    num: u16,
    ready: bool,
    desc: u64,
    driver: u64,
    device: u64,
    active: Option<ActiveQueue>,
}
impl QueueSlot {
    fn new(max: u16) -> Self {
        Self {
            max,
            num: 0,
            ready: false,
            desc: 0,
            driver: 0,
            device: 0,
            active: None,
        }
    }
}

/// The Rust transport: the register file plus the device model.
#[derive(Debug)]
pub struct MmioTransport<D: VirtioDevice> {
    device: D,
    vm_id: u64,
    limits: QueueLimits,
    ring_features: u64,
    status: u32,
    device_features_sel: u32,
    driver_features_sel: u32,
    driver_features: u64,
    driver_accepted_unknown: bool,
    negotiated: u64,
    queue_sel: u32,
    queues: Vec<QueueSlot>,
    interrupt_status: u32,
    config_generation: u32,
    stats: TransportStats,
}

impl<D: VirtioDevice> MmioTransport<D> {
    pub fn new(device: D, vm_id: u64) -> Self {
        Self::with_limits(device, vm_id, QueueLimits::default())
    }
    /// Offers only the ring features in `ring_features` (a subset of
    /// [`RING_FEATURES`]; other bits are ignored).
    pub fn with_ring_features(device: D, vm_id: u64, ring_features: u64) -> Self {
        let mut t = Self::new(device, vm_id);
        t.ring_features = ring_features & RING_FEATURES;
        t
    }
    pub fn with_limits(device: D, vm_id: u64, limits: QueueLimits) -> Self {
        let queues = device
            .queue_max_sizes()
            .iter()
            .map(|&m| QueueSlot::new(m))
            .collect();
        Self {
            device,
            vm_id,
            limits,
            ring_features: RING_FEATURES,
            status: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            driver_features: 0,
            driver_accepted_unknown: false,
            negotiated: 0,
            queue_sel: 0,
            queues,
            interrupt_status: 0,
            config_generation: 0,
            stats: TransportStats::default(),
        }
    }
    pub fn device(&self) -> &D {
        &self.device
    }
    /// Host-side access to the device (e.g. injecting a received frame); a
    /// configuration change the device reports raises a config interrupt.
    pub fn with_device<R>(&mut self, f: impl FnOnce(&mut D) -> R) -> R {
        let r = f(&mut self.device);
        self.check_config_change();
        r
    }
    pub fn status(&self) -> u32 {
        self.status
    }
    /// The VM this device belongs to.
    pub fn vm_id(&self) -> u64 {
        self.vm_id
    }
    pub fn negotiated_features(&self) -> u64 {
        self.negotiated
    }
    pub fn offered_features(&self) -> u64 {
        self.device.device_features() | VIRTIO_F_VERSION_1 | self.ring_features
    }
    /// The ring format negotiated queues use.
    pub fn ring_format(&self) -> RingFormat {
        if self.negotiated & VIRTIO_F_RING_PACKED != 0 {
            RingFormat::Packed
        } else {
            RingFormat::Split
        }
    }
    /// Counters of queue `index`, once it is ready.
    pub fn queue_stats(&self, index: u32) -> Option<crate::devices::queue::QueueStats> {
        self.queues
            .get(index as usize)?
            .active
            .as_ref()
            .map(|a| a.queue.stats())
    }
    pub fn interrupt_pending(&self) -> bool {
        self.interrupt_status != 0
    }
    pub fn stats(&self) -> TransportStats {
        self.stats
    }

    fn check_config_change(&mut self) {
        if self.device.take_config_change() {
            self.config_generation = self.config_generation.wrapping_add(1);
            self.raise(INT_CONFIG_CHANGE);
        }
    }
    fn raise(&mut self, bits: u32) {
        self.interrupt_status |= bits;
        self.stats.interrupts += 1;
    }
    fn needs_reset(&mut self) {
        self.status |= status::DEVICE_NEEDS_RESET;
        self.raise(INT_CONFIG_CHANGE);
    }
    fn reset(&mut self) {
        self.device.reset();
        self.status = 0;
        self.device_features_sel = 0;
        self.driver_features_sel = 0;
        self.driver_features = 0;
        self.driver_accepted_unknown = false;
        self.negotiated = 0;
        self.queue_sel = 0;
        for q in &mut self.queues {
            *q = QueueSlot::new(q.max);
        }
        self.interrupt_status = 0;
    }
    fn selected(&self) -> Result<&QueueSlot, MmioError> {
        self.queues
            .get(self.queue_sel as usize)
            .ok_or(MmioError::InvalidQueue {
                index: self.queue_sel,
            })
    }
    fn selected_mut(&mut self) -> Result<&mut QueueSlot, MmioError> {
        let index = self.queue_sel;
        let q = self
            .queues
            .get_mut(index as usize)
            .ok_or(MmioError::InvalidQueue { index })?;
        if q.ready {
            return Err(MmioError::QueueConfiguredWhileReady);
        }
        Ok(q)
    }

    /// A driver read of `data.len()` bytes at `offset` in the register window.
    pub fn read(&mut self, offset: u64, data: &mut [u8]) -> Result<(), MmioError> {
        if offset >= MMIO_WINDOW_BYTES {
            return Err(MmioError::OutOfWindow { offset });
        }
        if offset >= reg::CONFIG {
            return self.read_config(offset, data);
        }
        if data.len() != 4 || !offset.is_multiple_of(4) {
            return Err(MmioError::Width {
                offset,
                len: data.len(),
            });
        }
        let value = match offset {
            reg::MAGIC_VALUE => MMIO_MAGIC,
            reg::VERSION => MMIO_VERSION,
            reg::DEVICE_ID => self.device.device_type(),
            reg::VENDOR_ID => AETHER_VENDOR_ID,
            reg::DEVICE_FEATURES => match self.device_features_sel {
                0 => self.offered_features() as u32,
                1 => (self.offered_features() >> 32) as u32,
                _ => 0,
            },
            reg::QUEUE_NUM_MAX => self
                .queues
                .get(self.queue_sel as usize)
                .map_or(0, |q| u32::from(q.max)),
            reg::QUEUE_READY => u32::from(self.selected()?.ready),
            reg::INTERRUPT_STATUS => self.interrupt_status,
            reg::STATUS => self.status,
            reg::SHM_LEN_LOW | reg::SHM_LEN_HIGH | reg::SHM_BASE_LOW | reg::SHM_BASE_HIGH => {
                // No shared memory regions: length reads as -1 (§4.2.2).
                u32::MAX
            }
            reg::CONFIG_GENERATION => self.config_generation,
            reg::QUEUE_RESET => return Err(MmioError::UnsupportedRegister { offset }),
            reg::DEVICE_FEATURES_SEL
            | reg::DRIVER_FEATURES
            | reg::DRIVER_FEATURES_SEL
            | reg::QUEUE_SEL
            | reg::QUEUE_NUM
            | reg::QUEUE_NOTIFY
            | reg::INTERRUPT_ACK
            | reg::QUEUE_DESC_LOW
            | reg::QUEUE_DESC_HIGH
            | reg::QUEUE_DRIVER_LOW
            | reg::QUEUE_DRIVER_HIGH
            | reg::QUEUE_DEVICE_LOW
            | reg::QUEUE_DEVICE_HIGH
            | reg::SHM_SEL => return Err(MmioError::WriteOnlyRegister { offset }),
            _ => return Err(MmioError::UnsupportedRegister { offset }),
        };
        data.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) -> Result<(), MmioError> {
        let len = data.len();
        let off = (offset - reg::CONFIG) as usize;
        if !matches!(len, 1 | 2 | 4 | 8) {
            return Err(MmioError::Width { offset, len });
        }
        if off
            .checked_add(len)
            .is_none_or(|e| e > self.device.config_len())
        {
            return Err(MmioError::ConfigOutOfRange { offset, len });
        }
        self.device.read_config(off, data);
        Ok(())
    }

    /// A driver write. `memory` is the guest RAM (needed when a queue becomes
    /// ready and on notification).
    pub fn write(
        &mut self,
        memory: &mut GuestMemory,
        offset: u64,
        data: &[u8],
    ) -> Result<(), MmioError> {
        if offset >= MMIO_WINDOW_BYTES {
            return Err(MmioError::OutOfWindow { offset });
        }
        if offset >= reg::CONFIG {
            let len = data.len();
            let off = (offset - reg::CONFIG) as usize;
            if !matches!(len, 1 | 2 | 4 | 8) {
                return Err(MmioError::Width { offset, len });
            }
            if off
                .checked_add(len)
                .is_none_or(|e| e > self.device.config_len())
            {
                return Err(MmioError::ConfigOutOfRange { offset, len });
            }
            self.device.write_config(off, data)?;
            self.check_config_change();
            return Ok(());
        }
        let value = match <[u8; 4]>::try_from(data) {
            Ok(b) if offset.is_multiple_of(4) => u32::from_le_bytes(b),
            _ => {
                return Err(MmioError::Width {
                    offset,
                    len: data.len(),
                });
            }
        };
        match offset {
            reg::DEVICE_FEATURES_SEL => self.device_features_sel = value,
            reg::DRIVER_FEATURES_SEL => self.driver_features_sel = value,
            reg::DRIVER_FEATURES => {
                if self.status & status::FEATURES_OK != 0 {
                    return Err(MmioError::FeaturesAfterFeaturesOk);
                }
                match self.driver_features_sel {
                    0 => {
                        self.driver_features =
                            (self.driver_features & !0xffff_ffff) | u64::from(value)
                    }
                    1 => {
                        self.driver_features =
                            (self.driver_features & 0xffff_ffff) | (u64::from(value) << 32)
                    }
                    // Bits above 63 are never offered: accepting any of them
                    // makes the feature set invalid at FEATURES_OK.
                    _ if value != 0 => self.driver_accepted_unknown = true,
                    _ => {}
                }
            }
            reg::QUEUE_SEL => self.queue_sel = value,
            reg::QUEUE_NUM => {
                let q = self.selected_mut()?;
                if value == 0 || value > u32::from(q.max) {
                    return Err(MmioError::InvalidQueueSize {
                        size: value,
                        max: q.max,
                    });
                }
                q.num = value as u16;
            }
            reg::QUEUE_DESC_LOW => set_low(&mut self.selected_mut()?.desc, value),
            reg::QUEUE_DESC_HIGH => set_high(&mut self.selected_mut()?.desc, value),
            reg::QUEUE_DRIVER_LOW => set_low(&mut self.selected_mut()?.driver, value),
            reg::QUEUE_DRIVER_HIGH => set_high(&mut self.selected_mut()?.driver, value),
            reg::QUEUE_DEVICE_LOW => set_low(&mut self.selected_mut()?.device, value),
            reg::QUEUE_DEVICE_HIGH => set_high(&mut self.selected_mut()?.device, value),
            reg::QUEUE_READY => self.set_queue_ready(memory, value != 0)?,
            reg::QUEUE_NOTIFY => self.notify(memory, value)?,
            reg::INTERRUPT_ACK => self.interrupt_status &= !value,
            reg::STATUS => self.write_status(value)?,
            reg::SHM_SEL => {}
            reg::QUEUE_RESET => return Err(MmioError::UnsupportedRegister { offset }),
            reg::MAGIC_VALUE
            | reg::VERSION
            | reg::DEVICE_ID
            | reg::VENDOR_ID
            | reg::DEVICE_FEATURES
            | reg::QUEUE_NUM_MAX
            | reg::INTERRUPT_STATUS
            | reg::SHM_LEN_LOW
            | reg::SHM_LEN_HIGH
            | reg::SHM_BASE_LOW
            | reg::SHM_BASE_HIGH
            | reg::CONFIG_GENERATION => return Err(MmioError::ReadOnlyRegister { offset }),
            _ => return Err(MmioError::UnsupportedRegister { offset }),
        }
        Ok(())
    }

    pub(crate) fn write_status(&mut self, new: u32) -> Result<(), MmioError> {
        if new == 0 {
            self.reset();
            return Ok(());
        }
        let old = self.status;
        let added = new & !old;
        // DEVICE_NEEDS_RESET is device-owned; a driver may write it back
        // unchanged but may not set it.
        if added & status::DEVICE_NEEDS_RESET != 0 {
            return Err(MmioError::DriverSetNeedsReset);
        }
        if old & !new & !status::DEVICE_NEEDS_RESET != 0 {
            return Err(MmioError::StatusRegression { from: old, to: new });
        }
        let mut next = new | (old & status::DEVICE_NEEDS_RESET);
        if added & status::FEATURES_OK != 0 {
            let offered = self.offered_features();
            let required = VIRTIO_F_VERSION_1;
            if self.driver_accepted_unknown
                || self.driver_features & !offered != 0
                || self.driver_features & required != required
            {
                // §3.1.1: the device leaves FEATURES_OK clear; the driver
                // re-reads status and gives up on the device.
                next &= !status::FEATURES_OK;
                self.stats.rejected_feature_sets += 1;
            } else {
                self.negotiated = self.driver_features;
            }
        }
        if added & status::DRIVER_OK != 0 {
            if next & status::FEATURES_OK == 0 {
                return Err(MmioError::DriverOkWithoutFeaturesOk);
            }
            self.device.activate(self.negotiated);
        }
        self.status = next;
        Ok(())
    }

    fn set_queue_ready(&mut self, memory: &GuestMemory, ready: bool) -> Result<(), MmioError> {
        let index = self.queue_sel;
        let limits = self.limits;
        let q = self
            .queues
            .get_mut(index as usize)
            .ok_or(MmioError::InvalidQueue { index })?;
        if !ready {
            q.ready = false;
            q.active = None;
            return Ok(());
        }
        if q.ready {
            return Ok(());
        }
        if q.num == 0 {
            return Err(MmioError::InvalidQueueSize {
                size: 0,
                max: q.max,
            });
        }
        if self.status & status::FEATURES_OK == 0 {
            return Err(MmioError::QueueReadyBeforeFeaturesOk);
        }
        let format = if self.negotiated & VIRTIO_F_RING_PACKED != 0 {
            RingFormat::Packed
        } else {
            RingFormat::Split
        };
        let layout = RingLayout {
            format,
            size: q.num,
            desc: q.desc,
            driver: q.driver,
            device: q.device,
        };
        let queue = VirtQueue::new(
            layout,
            RingFeatures::from_bits(self.negotiated),
            limits,
            memory,
        )
        .map_err(|e| match e {
            QueueError::InvalidQueueSize => MmioError::InvalidQueueSize {
                size: u32::from(q.num),
                max: q.max,
            },
            _ => MmioError::RingNotInRam,
        })?;
        q.active = Some(ActiveQueue {
            queue,
            pending: VecDeque::new(),
        });
        q.ready = true;
        Ok(())
    }

    fn notify(&mut self, memory: &mut GuestMemory, value: u32) -> Result<(), MmioError> {
        self.stats.notifications += 1;
        if self.negotiated & VIRTIO_F_NOTIFICATION_DATA == 0 {
            return self.run_queue(memory, value, None);
        }
        // §2.9: vqn in bits 0..16, then next_idx (split) or next_off and
        // next_wrap (packed).
        let index = value & 0xffff;
        let hint = self
            .queues
            .get(index as usize)
            .and_then(|q| q.active.as_ref())
            .map(|a| a.queue.hint_from_data((value >> 16) as u16));
        match hint {
            Some(Ok(h)) => self.run_queue(memory, index, Some(h)),
            Some(Err(_)) => Err(MmioError::InvalidNotification { value }),
            None => self.run_queue(memory, index, None),
        }
    }

    /// Host-side poll: process queue `index` again (e.g. after the backend has
    /// data for a receive queue). Captures newly available chains first.
    pub fn poll(&mut self, memory: &mut GuestMemory, index: u32) -> Result<(), MmioError> {
        self.run_queue(memory, index, None)
    }

    fn run_queue(
        &mut self,
        memory: &mut GuestMemory,
        index: u32,
        hint: Option<AvailHint>,
    ) -> Result<(), MmioError> {
        if self.status & status::DEVICE_NEEDS_RESET != 0 {
            return Err(MmioError::DeviceNeedsReset);
        }
        if self.status & status::FAILED != 0 {
            return Err(MmioError::DriverFailed);
        }
        if self.status & status::DRIVER_OK == 0 {
            return Err(MmioError::NotifyBeforeDriverOk);
        }
        let q = self
            .queues
            .get_mut(index as usize)
            .ok_or(MmioError::InvalidQueue { index })?;
        let Some(active) = q.active.as_mut() else {
            return Err(MmioError::QueueNotReady { index });
        };
        let mut captured = 0u64;
        let mut failure = None;
        loop {
            match active.queue.capture(memory, hint) {
                Ok(Some(chain)) => {
                    active.pending.push_back(chain);
                    captured += 1;
                }
                Ok(None) | Err(QueueError::Full) => break,
                Err(e) => {
                    failure = Some(MmioError::Queue(e));
                    break;
                }
            }
        }
        self.stats.chains_captured += captured;
        if let Some(e) = failure {
            self.needs_reset();
            return Err(e);
        }
        let mut io = QueueIo {
            queue: &mut active.queue,
            pending: &mut active.pending,
            memory,
            completed: 0,
        };
        let result = self.device.process(index as u16, &mut io);
        let completed = io.completed;
        let flushed = active.queue.flush(memory);
        self.stats.chains_completed += completed as u64;
        match flushed {
            Ok(true) => self.raise(INT_USED_BUFFER),
            Ok(false) if completed > 0 => self.stats.interrupts_suppressed += 1,
            Ok(false) => {}
            Err(e) => {
                self.needs_reset();
                return Err(device_error(e));
            }
        }
        self.check_config_change();
        match result {
            Ok(()) => Ok(()),
            Err(e) => {
                self.needs_reset();
                Err(device_error(e))
            }
        }
    }
}

fn set_low(field: &mut u64, value: u32) {
    *field = (*field & !0xffff_ffff) | u64::from(value);
}
fn set_high(field: &mut u64, value: u32) {
    *field = (*field & 0xffff_ffff) | (u64::from(value) << 32);
}
