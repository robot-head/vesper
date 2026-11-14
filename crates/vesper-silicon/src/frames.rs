//! Physical 4 KiB page frames, page-table unmap with pending TLB shootdown,
//! and a frame-conservation auditor (§26.1 item 5, §26.3).
//!
//! [`PhysFrame4K`] is minted only by [`FrameAllocator::alloc`] and is
//! non-`Clone`, so the same frame cannot be owned twice. Unmapping yields a
//! linear [`PendingTlbShootdown`] that holds the frame; the frame comes back
//! only through [`PendingTlbShootdown::flush_ipi_and_reclaim`], which records
//! the per-architecture invalidation sequence on the [`SmpIpi`] model. A
//! dropped frame or shootdown is counted as a leak on the [`Ledger`] and the
//! [`FrameAuditor`] reports it. The page tables and IPIs are software models.

use std::collections::BTreeMap;

use crate::Arch;
use crate::ledger::{Ledger, OwnerKind, Token};

pub const PAGE_BYTES: u64 = 4096;

/// A physical 4 KiB frame. Linear: its exits are [`FrameAllocator::free`],
/// [`PageTable::map`] (which stores it) and DMA submission.
#[must_use = "a PhysFrame4K must be freed, mapped or handed to DMA"]
#[derive(Debug)]
pub struct PhysFrame4K {
    pfn: u64,
    token: Token,
}

impl PhysFrame4K {
    pub fn pfn(&self) -> u64 {
        self.pfn
    }

    pub fn phys_addr(&self) -> u64 {
        self.pfn * PAGE_BYTES
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    OutOfRange { pfn: u64 },
    NotAllocated { pfn: u64 },
    ForeignLedger,
    RangeOverflow,
}

/// A simple free-list frame allocator over `[base_pfn, base_pfn + count)`.
#[derive(Debug)]
pub struct FrameAllocator {
    base_pfn: u64,
    count: u64,
    free: Vec<u64>,
    out: Vec<bool>,
    ledger: Ledger,
}

impl FrameAllocator {
    pub fn new(base_pfn: u64, count: u64, ledger: &Ledger) -> Result<Self, FrameError> {
        let end = base_pfn
            .checked_add(count)
            .ok_or(FrameError::RangeOverflow)?;
        end.checked_mul(PAGE_BYTES)
            .ok_or(FrameError::RangeOverflow)?;
        let n = usize::try_from(count).map_err(|_| FrameError::RangeOverflow)?;
        Ok(Self {
            base_pfn,
            count,
            free: (base_pfn..end).rev().collect(),
            out: vec![false; n],
            ledger: ledger.clone(),
        })
    }

    pub fn total(&self) -> u64 {
        self.count
    }

    pub fn free_count(&self) -> u64 {
        self.free.len() as u64
    }

    pub fn allocated_count(&self) -> u64 {
        self.count - self.free_count()
    }

    pub fn alloc(&mut self) -> Option<PhysFrame4K> {
        let pfn = self.free.pop()?;
        self.out[(pfn - self.base_pfn) as usize] = true;
        Some(PhysFrame4K {
            pfn,
            token: self.ledger.open(OwnerKind::PhysFrame4K),
        })
    }

    /// The frame's consuming exit back to the allocator.
    pub fn free(&mut self, frame: PhysFrame4K) -> Result<(), (PhysFrame4K, FrameError)> {
        if !frame.token.ledger().same(&self.ledger) {
            return Err((frame, FrameError::ForeignLedger));
        }
        let Some(i) = frame
            .pfn
            .checked_sub(self.base_pfn)
            .filter(|&i| i < self.count)
            .map(|i| i as usize)
        else {
            let pfn = frame.pfn;
            return Err((frame, FrameError::OutOfRange { pfn }));
        };
        if !self.out[i] {
            let pfn = frame.pfn;
            return Err((frame, FrameError::NotAllocated { pfn }));
        }
        self.out[i] = false;
        self.free.push(frame.pfn);
        frame.token.discharge();
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VmError {
    Unaligned { vaddr: u64 },
    AlreadyMapped { vaddr: u64 },
    NotMapped { vaddr: u64 },
}

/// One address space's leaf 4 KiB mappings.
#[derive(Debug)]
pub struct PageTable {
    asid: u16,
    leaves: BTreeMap<u64, PhysFrame4K>,
    ledger: Ledger,
}

impl PageTable {
    pub fn new(asid: u16, ledger: &Ledger) -> Self {
        Self {
            asid,
            leaves: BTreeMap::new(),
            ledger: ledger.clone(),
        }
    }

    pub fn asid(&self) -> u16 {
        self.asid
    }

    pub fn mapped_count(&self) -> u64 {
        self.leaves.len() as u64
    }

    pub fn translate(&self, vaddr: u64) -> Option<u64> {
        let page = vaddr & !(PAGE_BYTES - 1);
        self.leaves
            .get(&page)
            .map(|f| f.phys_addr() + (vaddr - page))
    }

    pub fn mapped_pfns(&self) -> impl Iterator<Item = u64> + '_ {
        self.leaves.values().map(|f| f.pfn)
    }

    pub fn map(&mut self, vaddr: u64, frame: PhysFrame4K) -> Result<(), (PhysFrame4K, VmError)> {
        if !vaddr.is_multiple_of(PAGE_BYTES) {
            return Err((frame, VmError::Unaligned { vaddr }));
        }
        if self.leaves.contains_key(&vaddr) {
            return Err((frame, VmError::AlreadyMapped { vaddr }));
        }
        self.leaves.insert(vaddr, frame);
        Ok(())
    }

    /// Removes the leaf. The frame is not reusable until the returned
    /// shootdown is completed on every core.
    pub fn unmap_leaf_4k(&mut self, vaddr: u64) -> Result<PendingTlbShootdown, VmError> {
        if !vaddr.is_multiple_of(PAGE_BYTES) {
            return Err(VmError::Unaligned { vaddr });
        }
        let frame = self
            .leaves
            .remove(&vaddr)
            .ok_or(VmError::NotMapped { vaddr })?;
        Ok(PendingTlbShootdown {
            vaddr,
            asid: self.asid,
            reclaimed_frame: frame,
            token: self.ledger.open(OwnerKind::PendingTlbShootdown),
        })
    }
}

/// A frame whose stale translations may still be cached in some TLB.
#[must_use = "a PendingTlbShootdown must be completed with flush_ipi_and_reclaim"]
#[derive(Debug)]
pub struct PendingTlbShootdown {
    vaddr: u64,
    asid: u16,
    reclaimed_frame: PhysFrame4K,
    token: Token,
}

impl PendingTlbShootdown {
    pub fn vaddr(&self) -> u64 {
        self.vaddr
    }

    pub fn asid(&self) -> u16 {
        self.asid
    }

    pub fn pfn(&self) -> u64 {
        self.reclaimed_frame.pfn
    }

    /// The only exit: broadcast the invalidation, then hand back the frame.
    pub fn flush_ipi_and_reclaim(self, smp: &mut SmpIpi) -> PhysFrame4K {
        let PendingTlbShootdown {
            vaddr,
            asid,
            reclaimed_frame,
            token,
        } = self;
        smp.broadcast_tlb_invalidate(vaddr, asid);
        token.discharge();
        reclaimed_frame
    }
}

/// One TLB-maintenance instruction (rendered as assembly text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlbInst {
    /// AArch64 `DSB ISHST`.
    DsbIshst,
    /// AArch64 `TLBI VAE1IS, Xt` with `Xt = asid << 48 | va >> 12`.
    TlbiVae1is(u64),
    DsbIsh,
    Isb,
    /// x86_64 `INVLPG` on each core (remote cores via IPI), PCID ignored.
    Invlpg(u64),
    /// RISC-V `SFENCE.VMA vaddr, asid` on each hart (remote via IPI/SBI).
    SfenceVma {
        vaddr: u64,
        asid: u16,
    },
}

/// Invalidation sequence one core executes for a 4 KiB leaf.
pub fn tlb_invalidate_sequence(arch: Arch, vaddr: u64, asid: u16) -> Vec<TlbInst> {
    match arch {
        Arch::Aarch64 => {
            let operand = (u64::from(asid) << 48) | ((vaddr >> 12) & ((1u64 << 44) - 1));
            vec![
                TlbInst::DsbIshst,
                TlbInst::TlbiVae1is(operand),
                TlbInst::DsbIsh,
                TlbInst::Isb,
            ]
        }
        Arch::X86_64 => vec![TlbInst::Invlpg(vaddr)],
        Arch::Riscv64 => vec![TlbInst::SfenceVma { vaddr, asid }],
    }
}

/// Model of inter-processor TLB shootdown.
#[derive(Debug)]
pub struct SmpIpi {
    arch: Arch,
    cores: u32,
    /// Per completed shootdown: (vaddr, asid, instructions executed per core).
    log: Vec<(u64, u16, Vec<TlbInst>)>,
    /// Stale translations still cached: (asid, vaddr) per core.
    stale: Vec<BTreeMap<(u16, u64), ()>>,
}

impl SmpIpi {
    pub fn new(arch: Arch, cores: u32) -> Self {
        Self {
            arch,
            cores,
            log: Vec::new(),
            stale: vec![BTreeMap::new(); cores as usize],
        }
    }

    /// A core cached a translation (models TLB fill).
    pub fn note_cached(&mut self, core: u32, asid: u16, vaddr: u64) {
        if let Some(t) = self.stale.get_mut(core as usize) {
            t.insert((asid, vaddr), ());
        }
    }

    pub fn is_cached_anywhere(&self, asid: u16, vaddr: u64) -> bool {
        self.stale.iter().any(|t| t.contains_key(&(asid, vaddr)))
    }

    fn broadcast_tlb_invalidate(&mut self, vaddr: u64, asid: u16) {
        // AArch64's broadcast TLBI reaches every core in the inner-shareable
        // domain from one core; x86/RISC-V need the instruction on each core.
        let seq = tlb_invalidate_sequence(self.arch, vaddr, asid);
        for t in &mut self.stale {
            t.remove(&(asid, vaddr));
        }
        self.log.push((vaddr, asid, seq));
    }

    pub fn cores(&self) -> u32 {
        self.cores
    }

    pub fn log(&self) -> &[(u64, u16, Vec<TlbInst>)] {
        &self.log
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameAudit {
    pub total: u64,
    pub free: u64,
    pub mapped: u64,
    pub pending_shootdown: u64,
    pub in_dma: u64,
    /// Frames owned by the caller outside any table, shootdown or transfer.
    pub held: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditError {
    Leaked {
        frames: u64,
        shootdowns: u64,
        transfers: u64,
    },
    /// Allocated frames disagree with live frame owners.
    Unaccounted {
        allocated: u64,
        live_owners: u64,
    },
    DuplicateMapping {
        pfn: u64,
    },
}

/// Checks `total = free + mapped + pending + in_dma + held` and no leaks.
pub struct FrameAuditor;

impl FrameAuditor {
    /// `in_dma` is the number of in-flight transfers that own a frame.
    pub fn audit(
        allocator: &FrameAllocator,
        tables: &[&PageTable],
        in_dma_frames: u64,
    ) -> Result<FrameAudit, AuditError> {
        let ledger = &allocator.ledger;
        let leaked = (
            ledger.leaked(OwnerKind::PhysFrame4K),
            ledger.leaked(OwnerKind::PendingTlbShootdown),
            ledger.leaked(OwnerKind::DmaTransfer),
        );
        if leaked != (0, 0, 0) {
            return Err(AuditError::Leaked {
                frames: leaked.0,
                shootdowns: leaked.1,
                transfers: leaked.2,
            });
        }
        let live = ledger.live(OwnerKind::PhysFrame4K);
        let allocated = allocator.allocated_count();
        if live != allocated {
            return Err(AuditError::Unaccounted {
                allocated,
                live_owners: live,
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        for t in tables {
            for pfn in t.mapped_pfns() {
                if !seen.insert(pfn) {
                    return Err(AuditError::DuplicateMapping { pfn });
                }
            }
        }
        let mapped = seen.len() as u64;
        let pending = ledger.live(OwnerKind::PendingTlbShootdown);
        let accounted = mapped + pending + in_dma_frames;
        let held = allocated
            .checked_sub(accounted)
            .ok_or(AuditError::Unaccounted {
                allocated,
                live_owners: accounted,
            })?;
        Ok(FrameAudit {
            total: allocator.total(),
            free: allocator.free_count(),
            mapped,
            pending_shootdown: pending,
            in_dma: in_dma_frames,
            held,
        })
    }
}
