//! The Rust build of the interrupt fabric's controller state: the PLICs,
//! APLIC domain trees, IMSIC files, CLINT/ACLINT blocks, MTIME counters and
//! hypervisor CSRs behind [`super::RiscvIntc`]. The port build
//! (`intc/port.rs`) keeps the same state in the Vesper library and exposes
//! the same methods.

use std::collections::BTreeMap;

use super::*;
use crate::aia::{AplicDomain, ImsicFile};
use crate::dtb::MemRange;

/// Most MSI rounds one operation may cause (an MSI written to an APLIC's
/// `setipnum_le` can make it forward another).
const MSI_ROUNDS: usize = 64;

/// A CLINT or ACLINT block's state.
#[derive(Clone, Debug)]
struct Block {
    info: ClintInfo,
    /// MSIP (CLINT, MSWI) or SSIP (SSWI) per index.
    sip: Vec<bool>,
    mtimecmp: Vec<u64>,
}

impl Block {
    fn new(info: &ClintInfo) -> Self {
        let n = clint_slots(info);
        Self {
            info: info.clone(),
            sip: vec![false; n],
            mtimecmp: vec![u64::MAX; n],
        }
    }

    fn sw_target(&self, k: usize) -> Option<HartTarget> {
        sw_target(&self.info, k)
    }

    fn timer_target(&self, k: usize) -> Option<HartTarget> {
        timer_target(&self.info, k)
    }

    fn has_timer(&self) -> bool {
        matches!(self.info.kind, ClintKind::Clint | ClintKind::AclintMtimer)
    }

    fn mtime_addr(&self) -> Option<u64> {
        mtime_addr_of(&self.info)
    }
}

#[derive(Debug)]
struct ImsicBlock {
    info: ImsicInfo,
    /// `[file][guest]`; guest 0 is the hart's own file.
    files: Vec<Vec<ImsicFile>>,
}

impl ImsicBlock {
    fn locate(&self, addr: u64) -> Option<(usize, usize, u64)> {
        let stride = 0x1000u64 << self.info.guest_index_bits;
        self.info.files.iter().enumerate().find_map(|(k, &f)| {
            (addr >= f && addr - f < stride).then(|| {
                let rel = addr - f;
                (k, (rel / 0x1000) as usize, rel % 0x1000)
            })
        })
    }
}

/// The hypervisor CSR state of one hart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Hyp {
    vgein: u32,
    hgeie: u64,
}

#[derive(Debug)]
pub(super) struct Core {
    plics: Vec<Plic>,
    aplics: Vec<AplicDomain>,
    aplic_info: Vec<AplicInfo>,
    imsics: Vec<ImsicBlock>,
    blocks: Vec<Block>,
    /// MTIME counters by physical address (one per distinct register).
    mtimes: BTreeMap<u64, u64>,
    lost_msis: Vec<Msi>,
    /// `hstatus.VGEIN` and `hgeie` per hart.
    hyp: BTreeMap<u32, Hyp>,
}

impl Core {
    pub(super) fn new(platform: &Platform) -> Result<Self, IntcError> {
        let plics = platform
            .plics
            .iter()
            .map(Plic::new)
            .collect::<Result<Vec<_>, _>>()?;
        let mut imsics = Vec::new();
        // A file whose hart and level a PLIC context or a direct APLIC IDC
        // also reaches supports eidelivery = 0x40000000; guest files never
        // do (AIA §3.8.1).
        let wired = |t: &HartTarget| {
            platform.plics.iter().any(|p| p.contexts.contains(t))
                || platform.aplics.iter().any(|a| a.idcs.contains(t))
        };
        for info in &platform.imsics {
            let guests = 1usize << info.guest_index_bits;
            let files = info
                .targets
                .iter()
                .take(info.files.len())
                .map(|t| {
                    (0..guests)
                        .map(|g| {
                            if g == 0 && wired(t) {
                                ImsicFile::with_external_delivery(info.ids)
                            } else {
                                ImsicFile::new(info.ids)
                            }
                        })
                        .collect()
                })
                .collect::<Result<Vec<Vec<_>>, _>>()?;
            imsics.push(ImsicBlock {
                info: info.clone(),
                files,
            });
        }
        let mut aplics = Vec::new();
        for a in &platform.aplics {
            let window = MemRange {
                base: a.base,
                size: a.size,
            };
            let root = a.parent.is_none();
            // The root domain is M-level; its descendants are S-level.
            aplics.push(AplicDomain::new(
                &a.path,
                window,
                a.sources,
                a.idcs.len(),
                a.msi_parent.is_some(),
                root,
                !root,
                a.children.len() as u32,
            )?);
        }
        Ok(Self {
            plics,
            aplics,
            aplic_info: platform.aplics.clone(),
            imsics,
            blocks: platform.clints.iter().map(Block::new).collect(),
            mtimes: BTreeMap::new(),
            lost_msis: Vec::new(),
            hyp: BTreeMap::new(),
        })
    }

    // ------------------------------------------------------------ PLICs

    pub(super) fn plic_count(&self) -> usize {
        self.plics.len()
    }

    pub(super) fn plic_base(&self, p: usize) -> u64 {
        self.plics[p].base
    }

    /// The context of `want` on PLIC `p`.
    pub(super) fn plic_context(&self, p: usize, want: HartTarget) -> Option<usize> {
        self.plics[p].contexts.iter().position(|t| *t == want)
    }

    pub(super) fn plic_set_edge(
        &mut self,
        p: usize,
        src: u32,
        edge: bool,
    ) -> Result<(), IntcError> {
        self.plics[p].set_edge(src, edge)
    }

    pub(super) fn ext_context(&self, hart: u32, cause: u32) -> Option<(usize, usize)> {
        let want = HartTarget {
            hart,
            cause: Some(cause),
        };
        self.plics.iter().enumerate().find_map(|(p, plic)| {
            plic.contexts
                .iter()
                .position(|t| *t == want)
                .map(|c| (p, c))
        })
    }

    // ------------------------------------------------------------ APLICs

    pub(super) fn aplic_count(&self) -> usize {
        self.aplics.len()
    }

    pub(super) fn aplic_base(&self, d: usize) -> u64 {
        self.aplics[d].base()
    }

    pub(super) fn aplic_sources(&self, d: usize) -> u32 {
        self.aplics[d].sources()
    }

    pub(super) fn aplic_is_delegated_here(&self, d: usize, src: u32) -> bool {
        self.aplics[d].is_delegated_here(src)
    }

    pub(super) fn aplic_msi_cfg(&self, d: usize) -> MsiAddrCfg {
        self.aplics[d].msi_cfg()
    }

    pub(super) fn aplic_is_supervisor(&self, d: usize) -> bool {
        self.aplics[d].is_supervisor()
    }

    /// A register write to domain `d` without settling the fabric.
    pub(super) fn aplic_write32(&mut self, d: usize, addr: u64, v: u32) -> Result<(), IntcError> {
        self.aplics[d].write32(addr, v).map_err(IntcError::from)
    }

    /// The root of `d`'s domain tree.
    pub(super) fn root_of(&self, d: usize) -> usize {
        let mut cur = d;
        for _ in 0..self.aplic_info.len() {
            match &self.aplic_info[cur].parent {
                Some(p) => match self.aplic_info.iter().position(|a| &a.path == p) {
                    Some(i) => cur = i,
                    None => break,
                },
                None => break,
            }
        }
        cur
    }

    // ------------------------------------------------------------ IMSIC files

    /// The IMSIC and file of `(hart, cause)`.
    pub(super) fn imsic_file(&self, hart: u32, cause: u32) -> Option<(usize, usize)> {
        let want = HartTarget {
            hart,
            cause: Some(cause),
        };
        self.imsics.iter().enumerate().find_map(|(b, m)| {
            m.info
                .targets
                .iter()
                .position(|t| *t == want)
                .map(|k| (b, k))
        })
    }

    /// The implemented files (guest 0 and the guest files) behind file `k`
    /// of IMSIC `b`.
    pub(super) fn files_len(&self, b: usize, k: usize) -> usize {
        self.imsics[b].files[k].len()
    }

    pub(super) fn truncate_files(&mut self, b: usize, k: usize, keep: usize) {
        self.imsics[b].files[k].truncate(keep);
    }

    pub(super) fn file_csr_read(
        &self,
        b: usize,
        k: usize,
        g: usize,
        isel: u32,
    ) -> Result<u64, IntcError> {
        self.imsics[b].files[k][g]
            .csr_read(isel)
            .map_err(IntcError::from)
    }

    pub(super) fn file_csr_write(
        &mut self,
        b: usize,
        k: usize,
        g: usize,
        isel: u32,
        v: u64,
    ) -> Result<(), IntcError> {
        self.imsics[b].files[k][g]
            .csr_write(isel, v)
            .map_err(IntcError::from)
    }

    pub(super) fn file_topei(&self, b: usize, k: usize, g: usize) -> u32 {
        self.imsics[b].files[k][g].topei()
    }

    pub(super) fn file_claim(&mut self, b: usize, k: usize, g: usize) -> u32 {
        self.imsics[b].files[k][g].claim()
    }

    pub(super) fn file_is_pending(&self, b: usize, k: usize, g: usize, id: u32) -> bool {
        self.imsics[b].files[k][g].is_pending(id)
    }

    pub(super) fn file_eidelivery(&self, b: usize, k: usize) -> u32 {
        self.imsics[b].files[k][0].eidelivery()
    }

    pub(super) fn file_supports_external_delivery(&self, b: usize, k: usize) -> bool {
        self.imsics[b].files[k][0].supports_external_delivery()
    }

    /// The IMSIC file `(b, k, g)` whose page holds `addr`, if implemented.
    pub(super) fn locate_file(&self, addr: u64) -> Option<(usize, usize, usize)> {
        self.imsics.iter().enumerate().find_map(|(b, m)| {
            m.locate(addr)
                .filter(|&(k, g, _)| g < m.files[k].len())
                .map(|(k, g, _)| (b, k, g))
        })
    }

    // ------------------------------------------------------------ levels

    /// The level of `cause` on `hart`.
    pub(super) fn level(&self, hart: u32, cause: u32) -> bool {
        let want = HartTarget {
            hart,
            cause: Some(cause),
        };
        match cause {
            MEIP | SEIP => {
                let wired = self
                    .plics
                    .iter()
                    .any(|p| (0..p.contexts.len()).any(|c| p.contexts[c] == want && p.eip(c)))
                    || self.aplics.iter().zip(&self.aplic_info).any(|(d, info)| {
                        info.idcs
                            .iter()
                            .enumerate()
                            .any(|(i, t)| *t == want && d.idc_asserted(i))
                    });
                // With an interrupt file at this level, `eidelivery` picks
                // the file (1) or the PLIC/APLIC (0x40000000) (AIA §3.8.1).
                match self.imsic_file(hart, cause) {
                    Some((b, k)) => {
                        let f = &self.imsics[b].files[k][0];
                        f.asserted() || (f.delivers_external() && wired)
                    }
                    None => wired,
                }
            }
            SGEIP => {
                let h = self.hyp.get(&hart).copied().unwrap_or_default();
                self.hgeip(hart) & h.hgeie != 0
            }
            VSEIP => {
                let h = self.hyp.get(&hart).copied().unwrap_or_default();
                h.vgein < 64 && self.hgeip(hart) >> h.vgein & 1 != 0
            }
            MSIP | SSIP => self
                .blocks
                .iter()
                .any(|b| (0..b.sip.len()).any(|k| b.sip[k] && b.sw_target(k) == Some(want))),
            MTIP => self.blocks.iter().any(|b| {
                let now = b.mtime_addr().map_or(0, |a| self.mtime_at(a));
                b.has_timer()
                    && (0..b.mtimecmp.len())
                        .any(|k| now >= b.mtimecmp[k] && b.timer_target(k) == Some(want))
            }),
            _ => false,
        }
    }

    // ------------------------------------------------------------ hypervisor

    /// `hart`'s S-level IMSIC file and its GEILEN (guest files
    /// `1..=GEILEN`).
    pub(super) fn guest_files(&self, hart: u32) -> Option<(usize, usize, u32)> {
        let (b, k) = self.imsic_file(hart, SEIP)?;
        let n = (self.imsics[b].files[k].len() as u32).saturating_sub(1);
        Some((b, k, n.min(MAX_GEILEN)))
    }

    pub(super) fn geilen(&self, hart: u32) -> u32 {
        self.guest_files(hart).map_or(0, |g| g.2)
    }

    /// `hgeip`: bit `g` is guest file `g`'s interrupt output.
    pub(super) fn hgeip(&self, hart: u32) -> u64 {
        let Some((b, k, n)) = self.guest_files(hart) else {
            return 0;
        };
        (1..=n)
            .filter(|&g| self.imsics[b].files[k][g as usize].asserted())
            .fold(0, |acc, g| acc | 1 << g)
    }

    pub(super) fn vgein(&self, hart: u32) -> u32 {
        self.hyp.get(&hart).map_or(0, |h| h.vgein)
    }

    pub(super) fn set_vgein(&mut self, hart: u32, vgein: u32) {
        self.hyp.entry(hart).or_default().vgein = vgein;
    }

    pub(super) fn hgeie(&self, hart: u32) -> u64 {
        self.hyp.get(&hart).map_or(0, |h| h.hgeie)
    }

    pub(super) fn set_hgeie(&mut self, hart: u32, v: u64) {
        self.hyp.entry(hart).or_default().hgeie = v;
    }

    /// The harts with hypervisor CSR state, in order.
    pub(super) fn hyp_harts(&self) -> Vec<u32> {
        self.hyp.keys().copied().collect()
    }

    /// Masks a hart's `hgeie` to its current guest files (after
    /// [`Core::truncate_files`]).
    pub(super) fn clip_hgeie(&mut self, hart: u32) {
        let n = self.geilen(hart);
        if let Some(h) = self.hyp.get_mut(&hart) {
            h.hgeie &= ((1u64 << n) - 1) << 1;
        }
    }

    /// The guest file `hstatus.VGEIN` selects, or the typed exception.
    fn vs_file(&mut self, hart: u32) -> Result<&mut ImsicFile, IntcError> {
        let vgein = self.hyp.get(&hart).map_or(0, |h| h.vgein);
        match self.guest_files(hart) {
            Some((b, k, n)) if vgein != 0 && vgein <= n => {
                Ok(&mut self.imsics[b].files[k][vgein as usize])
            }
            _ => Err(IntcError::NoGuestFile { hart, vgein }),
        }
    }

    pub(super) fn vs_csr_read(&mut self, hart: u32, isel: u32) -> Result<u64, IntcError> {
        self.vs_file(hart)?.csr_read(isel).map_err(IntcError::from)
    }

    pub(super) fn vs_csr_write(&mut self, hart: u32, isel: u32, v: u64) -> Result<(), IntcError> {
        self.vs_file(hart)?
            .csr_write(isel, v)
            .map_err(IntcError::from)
    }

    pub(super) fn vstopei(&mut self, hart: u32) -> Result<u32, IntcError> {
        Ok(self.vs_file(hart)?.topei())
    }

    pub(super) fn claim_vstopei(&mut self, hart: u32) -> Result<u32, IntcError> {
        Ok(self.vs_file(hart)?.claim())
    }

    // ------------------------------------------------------------ MSIs

    /// Delegation and the root's MSI configuration flow from parents to
    /// children; then MSI-mode domains forward, and MSIs land in IMSIC
    /// files (or APLIC `setipnum_le` registers).
    pub(super) fn settle(&mut self) {
        for _ in 0..=self.aplics.len() {
            let mut changed = false;
            for d in 0..self.aplics.len() {
                let children: Vec<usize> = self.aplic_info[d]
                    .children
                    .iter()
                    .filter_map(|c| self.aplic_info.iter().position(|a| &a.path == c))
                    .collect();
                let root_cfg = self.aplics[self.root_of(d)].msi_cfg();
                for (ci, &c) in children.iter().enumerate() {
                    if self.aplics[c].msi_cfg() != root_cfg {
                        self.aplics[c].set_msi_cfg(root_cfg);
                    }
                    for s in 1..=self.aplics[c].sources() {
                        let here = self.aplics[d].delegated_to(s) == Some(ci as u32);
                        let before = self.aplics[c].mode(s) != SourceMode::Inactive
                            || self.aplics[c].delegated_to(s).is_some();
                        self.aplics[c].set_active_here(s, here);
                        let after = self.aplics[c].mode(s) != SourceMode::Inactive
                            || self.aplics[c].delegated_to(s).is_some();
                        changed |= before != after;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for _ in 0..MSI_ROUNDS {
            let msis: Vec<Msi> = self.aplics.iter_mut().flat_map(|d| d.take_msis()).collect();
            if msis.is_empty() {
                break;
            }
            for m in msis {
                if self.deliver_msi(m.addr, m.data).is_err() {
                    self.lost_msis.push(m);
                }
            }
        }
    }

    /// A 32-bit write of `data` to `addr` by a bus master.
    pub(super) fn deliver_msi(&mut self, addr: u64, data: u32) -> Result<(), IntcError> {
        for m in &mut self.imsics {
            if let Some((k, g, off)) = m.locate(addr) {
                return match m.files[k].get_mut(g) {
                    Some(f) => {
                        f.mmio_write32(off, data);
                        Ok(())
                    }
                    None => Err(IntcError::Unmapped { addr }),
                };
            }
        }
        for d in &mut self.aplics {
            if d.contains(addr) && addr - d.base() == 0x2000 {
                return d.write32(addr, data).map_err(IntcError::from);
            }
        }
        Err(IntcError::Unmapped { addr })
    }

    pub(super) fn lost_msis(&self) -> Vec<Msi> {
        self.lost_msis.clone()
    }

    pub(super) fn push_lost_msi(&mut self, m: Msi) {
        self.lost_msis.push(m);
    }

    // ------------------------------------------------------------ registers

    pub(super) fn read32(&mut self, addr: u64) -> Result<u32, IntcError> {
        if let Some(p) = self.plics.iter_mut().find(|p| p.contains(addr)) {
            return p.read32(addr);
        }
        if let Some(d) = self.aplics.iter_mut().find(|d| d.contains(addr)) {
            let v = d.read32(addr)?;
            self.settle();
            return Ok(v);
        }
        if self.imsics.iter().any(|m| m.locate(addr).is_some()) {
            if !addr.is_multiple_of(4) {
                return Err(IntcError::Misaligned { addr });
            }
            // seteipnum_le/_be read as zero.
            return Ok(0);
        }
        self.clint_read32(addr)
    }

    pub(super) fn write32(&mut self, addr: u64, v: u32) -> Result<(), IntcError> {
        if let Some(p) = self.plics.iter_mut().find(|p| p.contains(addr)) {
            return p.write32(addr, v);
        }
        if let Some(d) = self.aplics.iter_mut().find(|d| d.contains(addr)) {
            d.write32(addr, v)?;
            self.settle();
            return Ok(());
        }
        if self.imsics.iter().any(|m| m.locate(addr).is_some()) {
            if !addr.is_multiple_of(4) {
                return Err(IntcError::Misaligned { addr });
            }
            self.deliver_msi(addr, v)?;
            return Ok(());
        }
        self.clint_write32(addr, v)
    }

    fn clint_read32(&mut self, addr: u64) -> Result<u32, IntcError> {
        let (b, r, off) = self.block_at(addr)?;
        let blk = &self.blocks[b];
        let mtime = blk.mtime_addr().map_or(0, |a| self.mtime_at(a));
        let half = |v: u64, hi: bool| if hi { (v >> 32) as u32 } else { v as u32 };
        let unmapped = Err(IntcError::Unmapped { addr });
        match (blk.info.kind, r) {
            (ClintKind::Clint, _) if off < CLINT_MTIMECMP => {
                match blk.sip.get((off / 4) as usize) {
                    Some(&s) => Ok(u32::from(s)),
                    None => unmapped,
                }
            }
            (ClintKind::Clint, _) if off >= CLINT_MTIME => match off - CLINT_MTIME {
                0 => Ok(half(mtime, false)),
                4 => Ok(half(mtime, true)),
                _ => unmapped,
            },
            (ClintKind::Clint, _) => {
                let rel = off - CLINT_MTIMECMP;
                match blk.mtimecmp.get((rel / 8) as usize) {
                    Some(&v) => Ok(half(v, rel % 8 == 4)),
                    None => unmapped,
                }
            }
            (ClintKind::AclintMswi, _) => match blk.sip.get((off / 4) as usize) {
                Some(&s) => Ok(u32::from(s)),
                None => unmapped,
            },
            (ClintKind::AclintSswi, _) => match blk.sip.get((off / 4) as usize) {
                // SETSSIP reads as 0.
                Some(_) => Ok(0),
                None => unmapped,
            },
            (ClintKind::AclintMtimer, 0) => match off {
                0 => Ok(half(mtime, false)),
                4 => Ok(half(mtime, true)),
                _ => unmapped,
            },
            (ClintKind::AclintMtimer, _) => match blk.mtimecmp.get((off / 8) as usize) {
                Some(&v) => Ok(half(v, off % 8 == 4)),
                None => unmapped,
            },
        }
    }

    fn clint_write32(&mut self, addr: u64, v: u32) -> Result<(), IntcError> {
        let (b, r, off) = self.block_at(addr)?;
        let set_half = |old: u64, hi: bool| {
            if hi {
                (old & 0xffff_ffff) | (u64::from(v) << 32)
            } else {
                (old & !0xffff_ffff) | u64::from(v)
            }
        };
        let unmapped = Err(IntcError::Unmapped { addr });
        let kind = self.blocks[b].info.kind;
        let mut no_counter = 0u64;
        let mtime: &mut u64 = match self.blocks[b].mtime_addr() {
            Some(a) => self.mtimes.entry(a).or_insert(0),
            None => &mut no_counter,
        };
        let blk = &mut self.blocks[b];
        match (kind, r) {
            (ClintKind::Clint | ClintKind::AclintMswi, _)
                if kind == ClintKind::AclintMswi || off < CLINT_MTIMECMP =>
            {
                match blk.sip.get_mut((off / 4) as usize) {
                    Some(s) => *s = v & 1 != 0,
                    None => return unmapped,
                }
            }
            (ClintKind::Clint, _) if off >= CLINT_MTIME => match off - CLINT_MTIME {
                0 => *mtime = set_half(*mtime, false),
                4 => *mtime = set_half(*mtime, true),
                _ => return unmapped,
            },
            (ClintKind::Clint, _) => {
                let rel = off - CLINT_MTIMECMP;
                match blk.mtimecmp.get_mut((rel / 8) as usize) {
                    Some(c) => *c = set_half(*c, rel % 8 == 4),
                    None => return unmapped,
                }
            }
            (ClintKind::AclintSswi, _) => match blk.sip.get_mut((off / 4) as usize) {
                // Writing 1 sets SSIP; writing 0 does nothing.
                Some(s) => *s |= v & 1 != 0,
                None => return unmapped,
            },
            (ClintKind::AclintMtimer, 0) => match off {
                0 => *mtime = set_half(*mtime, false),
                4 => *mtime = set_half(*mtime, true),
                _ => return unmapped,
            },
            (ClintKind::AclintMtimer, _) => match blk.mtimecmp.get_mut((off / 8) as usize) {
                Some(c) => *c = set_half(*c, off % 8 == 4),
                None => return unmapped,
            },
            (ClintKind::AclintMswi, _) => return unmapped,
        }
        Ok(())
    }

    pub(super) fn mtime_at(&self, addr: u64) -> u64 {
        self.mtimes.get(&addr).copied().unwrap_or(0)
    }

    /// The MTIME that `hart`'s timer compares against (0 if it has none).
    pub(super) fn hart_mtime(&self, hart: u32) -> u64 {
        let want = Some(HartTarget {
            hart,
            cause: Some(MTIP),
        });
        self.blocks
            .iter()
            .find(|b| (0..b.mtimecmp.len()).any(|k| b.timer_target(k) == want))
            .and_then(Block::mtime_addr)
            .map_or(0, |a| self.mtime_at(a))
    }

    /// Advances every MTIME counter by `ticks` (wrapping).
    pub(super) fn advance_mtime(&mut self, ticks: u64) {
        let addrs: Vec<u64> = self.blocks.iter().filter_map(Block::mtime_addr).collect();
        for a in addrs {
            self.mtimes.entry(a).or_insert(0);
        }
        for v in self.mtimes.values_mut() {
            *v = v.wrapping_add(ticks);
        }
    }

    /// Clears `hart`'s SSIP on every SSWI.
    pub(super) fn clear_ssip(&mut self, hart: u32) {
        let want = Some(HartTarget {
            hart,
            cause: Some(SSIP),
        });
        for b in &mut self.blocks {
            for k in 0..b.sip.len() {
                if b.info.kind == ClintKind::AclintSswi && b.sw_target(k) == want {
                    b.sip[k] = false;
                }
            }
        }
    }

    /// The block, region index and offset holding `addr`.
    fn block_at(&self, addr: u64) -> Result<(usize, usize, u64), IntcError> {
        if !addr.is_multiple_of(4) {
            return Err(IntcError::Misaligned { addr });
        }
        for (b, blk) in self.blocks.iter().enumerate() {
            for (r, m) in blk.info.regs.iter().enumerate() {
                if addr >= m.base && addr - m.base < m.size {
                    return Ok((b, r, addr - m.base));
                }
            }
        }
        Err(IntcError::Unmapped { addr })
    }

    // ------------------------------------------------------------ wires and claims

    /// Drives a wire into `ctl` (a PLIC, or any domain of an APLIC tree:
    /// the wire enters the root and every domain sees it).
    pub(super) fn set_wire(&mut self, ctl: Ctl, src: u32, high: bool) -> Result<(), IntcError> {
        match ctl {
            Ctl::Plic(p) => self.plics[p].set_level(src, high)?,
            Ctl::Aplic(d) => {
                let root = self.root_of(d);
                if src == 0 || src > self.aplics[root].sources() {
                    return Err(IntcError::NoSuchSource(src));
                }
                for i in 0..self.aplics.len() {
                    if self.root_of(i) == root {
                        self.aplics[i].set_wire(src, high);
                    }
                }
                self.settle();
            }
            Ctl::Imsic(..) => return Err(IntcError::NoSuchSource(src)),
        }
        Ok(())
    }

    /// Claims one external interrupt for `(hart, cause)`: the IMSIC file
    /// first, then APLIC IDCs, then PLIC contexts. `Some((ctl, number))`, or
    /// `None` when nothing is claimable (an APLIC `iforce` claim is
    /// reported as `Some((Aplic, 0))`).
    pub(super) fn claim_external(&mut self, hart: u32, cause: u32) -> Option<(Ctl, u32)> {
        let want = HartTarget {
            hart,
            cause: Some(cause),
        };
        if let Some((b, k)) = self.imsic_file(hart, cause) {
            let f = &mut self.imsics[b].files[k][0];
            if f.asserted() {
                return Some((Ctl::Imsic(b, k), f.claim() >> 16));
            }
            if !f.delivers_external() {
                // The file, not the PLIC/APLIC, supplies this level.
                return None;
            }
        }
        for d in 0..self.aplics.len() {
            for (i, t) in self.aplic_info[d].idcs.iter().enumerate() {
                if *t == want && self.aplics[d].idc_asserted(i) {
                    let v = self.aplics[d].claimi(i);
                    return Some((Ctl::Aplic(d), v >> 16));
                }
            }
        }
        for p in 0..self.plics.len() {
            for c in 0..self.plics[p].contexts.len() {
                if self.plics[p].contexts[c] == want {
                    let s = self.plics[p].claim(c);
                    if s != 0 {
                        return Some((Ctl::Plic(p), s));
                    }
                }
            }
        }
        None
    }

    /// Completes `number` on `ctl`. `forwarded` is the APLIC domain and
    /// source that forwards an IMSIC identity, if one does.
    pub(super) fn complete_external(
        &mut self,
        hart: u32,
        cause: u32,
        ctl: Ctl,
        number: u32,
        forwarded: Option<(usize, u32)>,
    ) {
        match ctl {
            Ctl::Plic(p) => {
                let want = HartTarget {
                    hart,
                    cause: Some(cause),
                };
                if let Some(c) = self.plics[p].contexts.iter().position(|t| *t == want) {
                    self.plics[p].complete(c, number);
                }
            }
            Ctl::Imsic(..) => {
                // Linux's APLIC-MSI EOI: a level source still asserted after
                // its handler is re-triggered through setipnum (AIA §4.9.2).
                if let Some((d, s)) = forwarded
                    && matches!(
                        self.aplics[d].mode(s),
                        SourceMode::LevelHigh | SourceMode::LevelLow
                    )
                {
                    let base = self.aplics[d].base();
                    let _ = self.aplics[d].write32(base + 0x1cdc, s);
                    self.settle();
                }
            }
            Ctl::Aplic(_) => {}
        }
    }
}
