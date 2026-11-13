//! Linear DMA transfers, the 16-byte scatter-gather descriptor and the
//! per-architecture cache-maintenance sequence (§26.1 item 5, §26.2).
//!
//! [`DmaEngine::submit_rx`]/[`DmaEngine::submit_tx`] take the buffer by value
//! and return a linear [`DmaTransfer`]; the CPU cannot touch the buffer
//! until [`DmaEngine::poll_complete`] hands it back after the (modelled)
//! device signalled completion. Failed submissions return the buffer. The
//! engine is a software model: it records the descriptor bytes it would
//! write and the barrier instructions it would execute, which
//! [`dma_barriers`] computes per architecture:
//!
//! * AArch64 (non-coherent): `DC CVAC` per cache line + `DSB SY` before the
//!   device reads or writes the buffer; after a device write, `DC IVAC` per
//!   line + `DSB SY` before the CPU reads it.
//! * RISC-V, coherent DMA: `FENCE IORW, IORW` at both points.
//! * RISC-V, non-coherent DMA ([`DmaCoherence::Zicbom`], selected from the
//!   devicetree by [`crate::dtb::Platform::dma_coherence`]): `CBO.CLEAN` per
//!   cache block + `FENCE IORW, IORW` before the device touches the buffer;
//!   after a device write, `FENCE IORW, IORW` + `CBO.INVAL` per block before
//!   the CPU reads (the order Linux's `arch_sync_dma_for_{device,cpu}` uses
//!   on RISC-V). A receive buffer that does not cover whole cache blocks is
//!   rejected: invalidating a shared block would discard the CPU's writes to
//!   the other bytes of that block ([`NonCoherentCache`] demonstrates it).
//! * x86_64: none; DMA is cache-coherent.
//!
//! [`NonCoherentCache`] is a software model of one write-back cache in front
//! of memory that a bus master accesses directly. It executes the
//! cache-maintenance instructions above, so tests can check that a sequence
//! makes CPU and device views agree and that leaving any operation out
//! exposes stale data. It models data, not timing or ordering: fences are
//! no-ops in it.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::Arch;
use crate::frames::{PAGE_BYTES, PhysFrame4K};
use crate::ledger::{Ledger, OwnerKind, Token};

/// A buffer a bus master may own: physical start and length in bytes.
pub trait DmaBuffer {
    fn dma_region(&self) -> (u64, u32);
}

impl DmaBuffer for PhysFrame4K {
    fn dma_region(&self) -> (u64, u32) {
        (self.phys_addr(), PAGE_BYTES as u32)
    }
}

/// A statically reserved SRAM buffer at a fixed physical address.
#[derive(Debug)]
pub struct StaticDmaBuf<const N: usize> {
    phys: u64,
    pub data: [u8; N],
}

impl<const N: usize> StaticDmaBuf<N> {
    pub fn new(phys: u64) -> Self {
        Self { phys, data: [0; N] }
    }

    /// The first `n` received bytes (clamped to the buffer).
    pub fn prefix(&self, n: u32) -> &[u8] {
        &self.data[..(n as usize).min(N)]
    }
}

impl<const N: usize> DmaBuffer for StaticDmaBuf<N> {
    fn dma_region(&self) -> (u64, u32) {
        (self.phys, u32::try_from(N).unwrap_or(u32::MAX))
    }
}

/// §26.2 `SgDmaDescWire`: exactly 16 little-endian bytes, no padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SgDmaDescWire {
    pub phys_addr: u64,
    pub byte_len: u32,
    pub flags: u16,
    pub next_idx: u16,
}

impl SgDmaDescWire {
    pub const SIZE: usize = 16;
    pub const FLAG_DEVICE_WRITES: u16 = 1 << 1;
    pub const FLAG_NEXT: u16 = 1 << 0;

    pub fn to_bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..8].copy_from_slice(&self.phys_addr.to_le_bytes());
        b[8..12].copy_from_slice(&self.byte_len.to_le_bytes());
        b[12..14].copy_from_slice(&self.flags.to_le_bytes());
        b[14..16].copy_from_slice(&self.next_idx.to_le_bytes());
        b
    }

    /// Parses exactly 16 bytes; any other length is an error.
    pub fn from_bytes(b: &[u8]) -> Result<Self, DmaError> {
        let b: &[u8; 16] = b
            .try_into()
            .map_err(|_| DmaError::BadDescriptorLength(b.len()))?;
        Ok(Self {
            phys_addr: u64::from_le_bytes(b[0..8].try_into().expect("8")),
            byte_len: u32::from_le_bytes(b[8..12].try_into().expect("4")),
            flags: u16::from_le_bytes(b[12..14].try_into().expect("2")),
            next_idx: u16::from_le_bytes(b[14..16].try_into().expect("2")),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DmaError {
    NoSuchChannel(u8),
    ChannelBusy(u8),
    EmptyBuffer,
    RegionOverflow,
    BadLineSize(u32),
    BadDescriptorLength(usize),
    /// Device reported more bytes than the buffer holds.
    DeviceOverrun {
        reported: u32,
        capacity: u32,
    },
    DeviceFault(u8),
    NotInFlight(u8),
    /// Every descriptor slot is owned by an in-flight transfer.
    RingFull,
    /// The device has not completed this transfer yet.
    NotComplete,
    /// The transfer was submitted to a different engine.
    ForeignTransfer,
    /// A non-coherent receive buffer that does not start and end on cache
    /// block boundaries.
    PartialCacheBlock {
        phys: u64,
        len: u32,
        block: u32,
    },
    /// The coherence mode does not exist on the engine's architecture.
    CoherenceMismatch {
        arch: Arch,
        coherence: DmaCoherence,
    },
    /// An access outside a [`NonCoherentCache`]'s memory window.
    OutOfWindow {
        addr: u64,
    },
}

/// How a device's DMA interacts with the CPU caches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaCoherence {
    /// Hardware keeps caches coherent; only ordering barriers are needed.
    Coherent,
    /// RISC-V non-coherent DMA maintained with Zicbom `cbo.*` instructions
    /// on blocks of this size (`riscv,cbom-block-size`).
    Zicbom { block_bytes: u32 },
    /// Arm non-coherent DMA (`DC CVAC` / `DC IVAC`); the line size comes
    /// from `CTR_EL0`, which a devicetree does not state.
    ArmNonCoherent,
}

static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);

/// One barrier / cache-maintenance instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BarrierInst {
    /// AArch64 `DC CVAC, Xt` (clean to point of coherency) for one line.
    DcCvac(u64),
    /// AArch64 `DC IVAC, Xt` (invalidate to point of coherency) for one line.
    DcIvac(u64),
    /// AArch64 `DSB SY`.
    DsbSy,
    /// RISC-V `FENCE IORW, IORW`.
    FenceIorwIorw,
    /// RISC-V Zicbom `CBO.CLEAN`: write the block back if dirty, keep it.
    CboClean(u64),
    /// RISC-V Zicbom `CBO.FLUSH`: write back if dirty, then invalidate.
    CboFlush(u64),
    /// RISC-V Zicbom `CBO.INVAL`: discard the block without writing back
    /// (with `menvcfg.CBIE = 11`; `01` executes it as a flush).
    CboInval(u64),
}

impl std::fmt::Display for BarrierInst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BarrierInst::DcCvac(a) => write!(f, "dc cvac, {a:#x}"),
            BarrierInst::DcIvac(a) => write!(f, "dc ivac, {a:#x}"),
            BarrierInst::DsbSy => f.write_str("dsb sy"),
            BarrierInst::FenceIorwIorw => f.write_str("fence iorw, iorw"),
            BarrierInst::CboClean(a) => write!(f, "cbo.clean ({a:#x})"),
            BarrierInst::CboFlush(a) => write!(f, "cbo.flush ({a:#x})"),
            BarrierInst::CboInval(a) => write!(f, "cbo.inval ({a:#x})"),
        }
    }
}

/// When the barrier sequence runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaPhase {
    /// Before handing the buffer to the device (both directions).
    BeforeDevice,
    /// After a device write completed, before the CPU reads.
    AfterDeviceWrite,
}

/// The barrier sequence for `[phys, phys + len)` with `line`-byte cache lines.
pub fn dma_barriers(
    arch: Arch,
    phase: DmaPhase,
    phys: u64,
    len: u32,
    line: u32,
) -> Result<Vec<BarrierInst>, DmaError> {
    if line == 0 || !line.is_power_of_two() {
        return Err(DmaError::BadLineSize(line));
    }
    if len == 0 {
        return Err(DmaError::EmptyBuffer);
    }
    let end = phys
        .checked_add(u64::from(len))
        .ok_or(DmaError::RegionOverflow)?;
    Ok(match arch {
        Arch::X86_64 => Vec::new(),
        Arch::Riscv64 => vec![BarrierInst::FenceIorwIorw],
        Arch::Aarch64 => {
            let line = u64::from(line);
            let mut a = phys & !(line - 1);
            let mut out = Vec::new();
            while a < end {
                out.push(match phase {
                    DmaPhase::BeforeDevice => BarrierInst::DcCvac(a),
                    DmaPhase::AfterDeviceWrite => BarrierInst::DcIvac(a),
                });
                a = a.checked_add(line).ok_or(DmaError::RegionOverflow)?;
            }
            out.push(BarrierInst::DsbSy);
            out
        }
    })
}

/// Block-aligned addresses of every `block`-byte cache block that overlaps
/// `[phys, phys + len)`.
fn blocks(phys: u64, len: u32, block: u32) -> Result<Vec<u64>, DmaError> {
    if block == 0 || !block.is_power_of_two() {
        return Err(DmaError::BadLineSize(block));
    }
    if len == 0 {
        return Err(DmaError::EmptyBuffer);
    }
    let end = phys
        .checked_add(u64::from(len))
        .ok_or(DmaError::RegionOverflow)?;
    let block = u64::from(block);
    let mut a = phys & !(block - 1);
    let mut out = Vec::new();
    while a < end {
        out.push(a);
        a = a.checked_add(block).ok_or(DmaError::RegionOverflow)?;
    }
    Ok(out)
}

/// The RISC-V Zicbom sequence for non-coherent DMA on `[phys, phys + len)`
/// with `block`-byte cache blocks. After a device write the range must cover
/// whole blocks ([`DmaError::PartialCacheBlock`] otherwise).
pub fn zicbom_barriers(
    phase: DmaPhase,
    phys: u64,
    len: u32,
    block: u32,
) -> Result<Vec<BarrierInst>, DmaError> {
    let bs = blocks(phys, len, block)?;
    Ok(match phase {
        DmaPhase::BeforeDevice => {
            let mut out: Vec<_> = bs.into_iter().map(BarrierInst::CboClean).collect();
            out.push(BarrierInst::FenceIorwIorw);
            out
        }
        DmaPhase::AfterDeviceWrite => {
            let mask = u64::from(block) - 1;
            if phys & mask != 0 || u64::from(len) & mask != 0 {
                return Err(DmaError::PartialCacheBlock { phys, len, block });
            }
            let mut out = vec![BarrierInst::FenceIorwIorw];
            out.extend(bs.into_iter().map(BarrierInst::CboInval));
            out
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Device writes memory (receive).
    FromDevice,
    /// Device reads memory (transmit).
    ToDevice,
}

/// An in-flight bus-master transfer owning `B`.
#[must_use = "a DmaTransfer must be completed with DmaEngine::poll_complete"]
#[derive(Debug)]
pub struct DmaTransfer<B> {
    engine: u64,
    channel: u8,
    desc_slot: u16,
    owned_buf: B,
    direction: Direction,
    token: Token,
}

impl<B> DmaTransfer<B> {
    pub fn channel(&self) -> u8 {
        self.channel
    }

    pub fn desc_slot(&self) -> u16 {
        self.desc_slot
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Channel {
    Idle,
    Busy {
        slot: u16,
        capacity: u32,
    },
    Done {
        slot: u16,
        result: Result<u32, DmaError>,
    },
}

/// Model of a DMA engine with a descriptor ring.
#[derive(Debug)]
pub struct DmaEngine {
    id: u64,
    arch: Arch,
    line: u32,
    coherence: DmaCoherence,
    channels: Vec<Channel>,
    ring: Vec<[u8; 16]>,
    /// Slot owned by an in-flight (or completed, unpolled) transfer.
    slot_busy: Vec<bool>,
    next_slot: u16,
    barriers: Vec<BarrierInst>,
    ledger: Ledger,
}

impl DmaEngine {
    pub fn new(
        arch: Arch,
        channels: u8,
        ring_slots: u16,
        line: u32,
        ledger: &Ledger,
    ) -> Result<Self, DmaError> {
        let coherence = match arch {
            Arch::Aarch64 => DmaCoherence::ArmNonCoherent,
            Arch::X86_64 | Arch::Riscv64 => DmaCoherence::Coherent,
        };
        Self::with_coherence(arch, coherence, channels, ring_slots, line, ledger)
    }

    /// An engine for a device with the given coherence (for example from
    /// [`crate::dtb::Platform::dma_coherence`]). `line` is the Arm cache-line
    /// size for [`DmaCoherence::ArmNonCoherent`]; for
    /// [`DmaCoherence::Zicbom`] the block size comes from the variant.
    pub fn with_coherence(
        arch: Arch,
        coherence: DmaCoherence,
        channels: u8,
        ring_slots: u16,
        line: u32,
        ledger: &Ledger,
    ) -> Result<Self, DmaError> {
        let line = match (arch, coherence) {
            (Arch::Riscv64, DmaCoherence::Zicbom { block_bytes }) => block_bytes,
            (Arch::Aarch64, DmaCoherence::ArmNonCoherent) | (_, DmaCoherence::Coherent) => line,
            _ => return Err(DmaError::CoherenceMismatch { arch, coherence }),
        };
        if line == 0 || !line.is_power_of_two() {
            return Err(DmaError::BadLineSize(line));
        }
        let slots = ring_slots.max(1) as usize;
        Ok(Self {
            id: NEXT_ENGINE.fetch_add(1, Ordering::Relaxed),
            arch,
            line,
            coherence,
            channels: vec![Channel::Idle; channels as usize],
            ring: vec![[0; 16]; slots],
            slot_busy: vec![false; slots],
            next_slot: 0,
            barriers: Vec::new(),
            ledger: ledger.clone(),
        })
    }

    /// Barrier instructions executed so far, in order.
    pub fn barriers(&self) -> &[BarrierInst] {
        &self.barriers
    }

    pub fn coherence(&self) -> DmaCoherence {
        self.coherence
    }

    fn sequence(&self, phase: DmaPhase, phys: u64, len: u32) -> Result<Vec<BarrierInst>, DmaError> {
        match self.coherence {
            DmaCoherence::Zicbom { block_bytes } => zicbom_barriers(phase, phys, len, block_bytes),
            DmaCoherence::ArmNonCoherent => dma_barriers(self.arch, phase, phys, len, self.line),
            DmaCoherence::Coherent => {
                let seq = dma_barriers(self.arch, phase, phys, len, self.line)?;
                // Coherent Arm DMA needs ordering but no cache maintenance.
                Ok(seq
                    .into_iter()
                    .filter(|b| !matches!(b, BarrierInst::DcCvac(_) | BarrierInst::DcIvac(_)))
                    .collect())
            }
        }
    }

    pub fn descriptor(&self, slot: u16) -> Option<SgDmaDescWire> {
        self.ring
            .get(slot as usize)
            .and_then(|b| SgDmaDescWire::from_bytes(b).ok())
    }

    pub fn in_flight(&self) -> usize {
        self.channels
            .iter()
            .filter(|c| !matches!(c, Channel::Idle))
            .count()
    }

    pub fn submit_rx<B: DmaBuffer>(
        &mut self,
        channel: u8,
        buf: B,
    ) -> Result<DmaTransfer<B>, (B, DmaError)> {
        self.submit(channel, buf, Direction::FromDevice)
    }

    pub fn submit_tx<B: DmaBuffer>(
        &mut self,
        channel: u8,
        buf: B,
    ) -> Result<DmaTransfer<B>, (B, DmaError)> {
        self.submit(channel, buf, Direction::ToDevice)
    }

    fn submit<B: DmaBuffer>(
        &mut self,
        channel: u8,
        buf: B,
        direction: Direction,
    ) -> Result<DmaTransfer<B>, (B, DmaError)> {
        match self.channels.get(channel as usize) {
            None => return Err((buf, DmaError::NoSuchChannel(channel))),
            Some(Channel::Idle) => {}
            Some(_) => return Err((buf, DmaError::ChannelBusy(channel))),
        }
        let (phys, len) = buf.dma_region();
        let seq = match self.sequence(DmaPhase::BeforeDevice, phys, len) {
            Ok(s) => s,
            Err(e) => return Err((buf, e)),
        };
        // Check the post-receive sequence now, so a buffer that cannot be
        // invalidated safely is refused before the device owns it.
        if direction == Direction::FromDevice
            && let Err(e) = self.sequence(DmaPhase::AfterDeviceWrite, phys, len)
        {
            return Err((buf, e));
        }
        let n = self.ring.len();
        let Some(slot) = (0..n)
            .map(|k| (self.next_slot as usize + k) % n)
            .find(|&i| !self.slot_busy[i])
        else {
            return Err((buf, DmaError::RingFull));
        };
        self.slot_busy[slot] = true;
        let slot = slot as u16;
        self.next_slot = ((slot as usize + 1) % n) as u16;
        let flags = match direction {
            Direction::FromDevice => SgDmaDescWire::FLAG_DEVICE_WRITES,
            Direction::ToDevice => 0,
        };
        self.ring[slot as usize] = SgDmaDescWire {
            phys_addr: phys,
            byte_len: len,
            flags,
            next_idx: 0,
        }
        .to_bytes();
        // The descriptor and buffer must be visible before the doorbell.
        self.barriers.extend(seq);
        self.channels[channel as usize] = Channel::Busy {
            slot,
            capacity: len,
        };
        Ok(DmaTransfer {
            engine: self.id,
            channel,
            desc_slot: slot,
            owned_buf: buf,
            direction,
            token: self.ledger.open(OwnerKind::DmaTransfer),
        })
    }

    /// The modelled device finishes `channel` having moved `bytes`, or
    /// faulting with `fault`.
    pub fn device_complete(
        &mut self,
        channel: u8,
        bytes: u32,
        fault: Option<u8>,
    ) -> Result<(), DmaError> {
        let Some(Channel::Busy { slot, capacity }) = self.channels.get(channel as usize).cloned()
        else {
            return Err(DmaError::NotInFlight(channel));
        };
        let result = match fault {
            Some(code) => Err(DmaError::DeviceFault(code)),
            None if bytes > capacity => Err(DmaError::DeviceOverrun {
                reported: bytes,
                capacity,
            }),
            None => Ok(bytes),
        };
        self.channels[channel as usize] = Channel::Done { slot, result };
        Ok(())
    }

    /// `DmaEngine::await_complete`: if the device finished, returns the
    /// status and the buffer (after the post-DMA invalidation for receives);
    /// otherwise gives the transfer back with [`DmaError::NotComplete`] (a
    /// fiber would suspend here), or with [`DmaError::ForeignTransfer`] if it
    /// belongs to another engine.
    #[allow(clippy::type_complexity)]
    pub fn poll_complete<B: DmaBuffer>(
        &mut self,
        xfer: DmaTransfer<B>,
    ) -> Result<(Result<u32, DmaError>, B), (DmaTransfer<B>, DmaError)> {
        if xfer.engine != self.id {
            return Err((xfer, DmaError::ForeignTransfer));
        }
        let result = match self.channels.get(xfer.channel as usize) {
            Some(Channel::Done { slot, result }) if *slot == xfer.desc_slot => result.clone(),
            _ => return Err((xfer, DmaError::NotComplete)),
        };
        self.slot_busy[xfer.desc_slot as usize] = false;
        if xfer.direction == Direction::FromDevice {
            let (phys, len) = xfer.owned_buf.dma_region();
            // Validated at submit; recomputing cannot fail.
            if let Ok(seq) = self.sequence(DmaPhase::AfterDeviceWrite, phys, len) {
                self.barriers.extend(seq);
            }
        }
        self.channels[xfer.channel as usize] = Channel::Idle;
        let DmaTransfer {
            owned_buf, token, ..
        } = xfer;
        token.discharge();
        Ok((result, owned_buf))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    data: Vec<u8>,
    dirty: bool,
}

/// A write-back cache with `block`-byte lines in front of a memory window
/// `[base, base + size)` that a bus master reads and writes directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonCoherentCache {
    base: u64,
    block: u64,
    memory: Vec<u8>,
    lines: std::collections::BTreeMap<u64, Line>,
}

impl NonCoherentCache {
    pub fn new(base: u64, size: usize, block: u32) -> Result<Self, DmaError> {
        if block == 0 || !block.is_power_of_two() || !base.is_multiple_of(u64::from(block)) {
            return Err(DmaError::BadLineSize(block));
        }
        base.checked_add(size as u64)
            .ok_or(DmaError::RegionOverflow)?;
        Ok(Self {
            base,
            block: u64::from(block),
            memory: vec![0; size],
            lines: Default::default(),
        })
    }

    fn index(&self, addr: u64) -> Result<usize, DmaError> {
        addr.checked_sub(self.base)
            .and_then(|o| usize::try_from(o).ok())
            .filter(|&o| o < self.memory.len())
            .ok_or(DmaError::OutOfWindow { addr })
    }

    fn line_of(&mut self, addr: u64) -> Result<(&mut Line, usize), DmaError> {
        self.index(addr)?;
        let tag = addr & !(self.block - 1);
        let start = self.index(tag)?;
        let end = (start + self.block as usize).min(self.memory.len());
        let fill = self.memory[start..end].to_vec();
        let line = self.lines.entry(tag).or_insert(Line {
            data: fill,
            dirty: false,
        });
        Ok((line, (addr - tag) as usize))
    }

    /// A CPU store (allocates the line, marks it dirty).
    pub fn cpu_write(&mut self, addr: u64, bytes: &[u8]) -> Result<(), DmaError> {
        for (k, &b) in bytes.iter().enumerate() {
            let (line, off) = self.line_of(addr + k as u64)?;
            line.data[off] = b;
            line.dirty = true;
        }
        Ok(())
    }

    /// A CPU load (allocates the line from memory on a miss).
    pub fn cpu_read(&mut self, addr: u64, len: usize) -> Result<Vec<u8>, DmaError> {
        (0..len)
            .map(|k| {
                let (line, off) = self.line_of(addr + k as u64)?;
                Ok(line.data[off])
            })
            .collect()
    }

    /// The bus master reads memory directly, bypassing the cache.
    pub fn device_read(&self, addr: u64, len: usize) -> Result<Vec<u8>, DmaError> {
        let i = self.index(addr)?;
        let j = i.checked_add(len).filter(|&j| j <= self.memory.len());
        let j = j.ok_or(DmaError::OutOfWindow { addr })?;
        Ok(self.memory[i..j].to_vec())
    }

    /// The bus master writes memory directly; cached copies become stale.
    pub fn device_write(&mut self, addr: u64, bytes: &[u8]) -> Result<(), DmaError> {
        let i = self.index(addr)?;
        let j = i
            .checked_add(bytes.len())
            .filter(|&j| j <= self.memory.len());
        let j = j.ok_or(DmaError::OutOfWindow { addr })?;
        self.memory[i..j].copy_from_slice(bytes);
        Ok(())
    }

    fn writeback(&mut self, tag: u64) {
        if let Some(line) = self.lines.get_mut(&tag)
            && line.dirty
        {
            let start = (tag - self.base) as usize;
            self.memory[start..start + line.data.len()].copy_from_slice(&line.data);
            line.dirty = false;
        }
    }

    /// Executes one barrier or cache-maintenance instruction. Fences and
    /// `DSB` order accesses, which this data-only model does not track.
    pub fn execute(&mut self, inst: &BarrierInst) -> Result<(), DmaError> {
        let mask = !(self.block - 1);
        let tag = |a: u64| a & mask;
        match *inst {
            BarrierInst::CboClean(a) | BarrierInst::DcCvac(a) => {
                self.index(a)?;
                self.writeback(tag(a));
            }
            BarrierInst::CboFlush(a) => {
                self.index(a)?;
                self.writeback(tag(a));
                self.lines.remove(&tag(a));
            }
            BarrierInst::CboInval(a) | BarrierInst::DcIvac(a) => {
                self.index(a)?;
                self.lines.remove(&tag(a));
            }
            BarrierInst::DsbSy | BarrierInst::FenceIorwIorw => {}
        }
        Ok(())
    }

    pub fn execute_all(&mut self, insts: &[BarrierInst]) -> Result<(), DmaError> {
        insts.iter().try_for_each(|i| self.execute(i))
    }

    /// Evicts every line (writing dirty ones back), as cache pressure may at
    /// any time.
    pub fn evict_all(&mut self) {
        let tags: Vec<u64> = self.lines.keys().copied().collect();
        for t in tags {
            self.writeback(t);
        }
        self.lines.clear();
    }
}
