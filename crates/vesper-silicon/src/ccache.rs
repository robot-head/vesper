//! SiFive composable cache controller (L2) register model: the `FLUSH64`
//! and `FLUSH32` registers that the StarFive JH7100 relies on for
//! non-coherent DMA (§26.1 item 5), configured from
//! [`crate::dtb::CcacheInfo`].
//!
//! Register map (SiFive FU540/FU740 manuals, Linux
//! `drivers/cache/sifive_ccache.c`): `Config` at `0x000` (banks, ways,
//! log2 sets, log2 block bytes, one byte each), `WayEnable` at `0x008`,
//! `FLUSH64` at `0x200` (a 64-bit store of a physical address writes the
//! line holding it back and invalidates it) and `FLUSH32` at `0x240` (a
//! 32-bit store of the address shifted right by 4). The cache contents are a
//! [`NonCoherentCache`]; the flush acts on it. Error-correction, way
//! masking and the performance counters are not modelled.

use crate::dma::{BarrierInst, DmaError, NonCoherentCache};
use crate::dtb::CcacheInfo;

pub const CCACHE_CONFIG: u64 = 0x000;
pub const CCACHE_WAY_ENABLE: u64 = 0x008;
pub const CCACHE_FLUSH64: u64 = 0x200;
pub const CCACHE_FLUSH32: u64 = 0x240;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CcacheError {
    Unmapped {
        addr: u64,
    },
    Misaligned {
        addr: u64,
    },
    /// The flushed address is outside the modelled memory.
    Dma(DmaError),
    /// The cache model's line size differs from `cache-block-size`.
    LineMismatch {
        controller: u32,
        cache: u32,
    },
}

impl std::fmt::Display for CcacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CcacheError {}

/// A SiFive cache controller in front of the memory a [`NonCoherentCache`]
/// models.
#[derive(Clone, Debug)]
pub struct SifiveCcache {
    info: CcacheInfo,
    ways: u32,
    way_enable: u64,
    flushes: u64,
}

impl SifiveCcache {
    /// `ways` is the implemented way count (16 on the FU740 and JH7100; a
    /// devicetree does not state it).
    pub fn new(info: &CcacheInfo, ways: u32) -> Self {
        Self {
            info: info.clone(),
            ways,
            way_enable: 0,
            flushes: 0,
        }
    }

    pub fn info(&self) -> &CcacheInfo {
        &self.info
    }

    /// Lines flushed so far.
    pub fn flushes(&self) -> u64 {
        self.flushes
    }

    fn off(&self, addr: u64, align: u64) -> Result<u64, CcacheError> {
        if !addr.is_multiple_of(align) {
            return Err(CcacheError::Misaligned { addr });
        }
        addr.checked_sub(self.info.base)
            .filter(|&o| o < self.info.size)
            .ok_or(CcacheError::Unmapped { addr })
    }

    pub fn read64(&self, addr: u64) -> Result<u64, CcacheError> {
        let log2 = |v: Option<u32>| v.map_or(0, |v| u64::from(v.trailing_zeros()));
        match self.off(addr, 8)? {
            CCACHE_CONFIG => Ok(1
                | (u64::from(self.ways) << 8)
                | (log2(self.info.sets) << 16)
                | (u64::from(self.info.line_bytes.trailing_zeros()) << 24)),
            CCACHE_WAY_ENABLE => Ok(self.way_enable),
            _ => Err(CcacheError::Unmapped { addr }),
        }
    }

    fn flush(&mut self, cache: &mut NonCoherentCache, phys: u64) -> Result<(), CcacheError> {
        let reg = self.info.base + CCACHE_FLUSH64;
        cache
            .execute(&BarrierInst::CcacheFlush64 { reg, line: phys })
            .map_err(CcacheError::Dma)?;
        self.flushes += 1;
        Ok(())
    }

    /// A 64-bit store: `FLUSH64` flushes the line holding `v`; `WayEnable`
    /// keeps the index of the last enabled way (it only grows on hardware).
    pub fn write64(
        &mut self,
        cache: &mut NonCoherentCache,
        addr: u64,
        v: u64,
    ) -> Result<(), CcacheError> {
        if cache.block_bytes() != self.info.line_bytes {
            return Err(CcacheError::LineMismatch {
                controller: self.info.line_bytes,
                cache: cache.block_bytes(),
            });
        }
        match self.off(addr, 8)? {
            CCACHE_FLUSH64 => self.flush(cache, v),
            CCACHE_WAY_ENABLE => {
                self.way_enable = self.way_enable.max(v.min(u64::from(self.ways) - 1));
                Ok(())
            }
            _ => Err(CcacheError::Unmapped { addr }),
        }
    }

    /// A 32-bit store: `FLUSH32` flushes the line holding `v << 4`.
    pub fn write32(
        &mut self,
        cache: &mut NonCoherentCache,
        addr: u64,
        v: u32,
    ) -> Result<(), CcacheError> {
        match self.off(addr, 4)? {
            CCACHE_FLUSH32 => self.flush(cache, u64::from(v) << 4),
            _ => Err(CcacheError::Unmapped { addr }),
        }
    }

    /// Runs a barrier sequence the way the CPU would: `CcacheFlush64` stores
    /// go through this controller's register; other instructions go to the
    /// cache model directly.
    pub fn run(
        &mut self,
        cache: &mut NonCoherentCache,
        seq: &[BarrierInst],
    ) -> Result<(), CcacheError> {
        for inst in seq {
            match *inst {
                BarrierInst::CcacheFlush64 { reg, line } => self.write64(cache, reg, line)?,
                ref other => cache.execute(other).map_err(CcacheError::Dma)?,
            }
        }
        Ok(())
    }
}
