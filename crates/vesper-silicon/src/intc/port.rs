//! The port-backed build of the interrupt fabric's controller state: one
//! Vesper `Fabric` handle (vesper/src/intc.ves) holds every PLIC, APLIC
//! domain tree, IMSIC file, CLINT/ACLINT block, MTIME counter and the
//! hypervisor CSRs, and each [`Core`] method is one call into it. The
//! adapter in `intc.rs` keeps the handler table and the dispatch loops and
//! asks for one claim at a time. The methods match `intc/rust.rs`.

use super::*;
use crate::aia::{cfg_from_port, port_fault};
use crate::binding;

/// No index (the port's `none()`).
const NONE: u64 = u64::MAX;

/// A hart target as the port's word: `hart << 32 | cause`, cause
/// `0xffffffff` when the tree marks the output unused.
fn enc(t: &HartTarget) -> u64 {
    (u64::from(t.hart) << 32) | u64::from(t.cause.unwrap_or(u32::MAX))
}

fn want(hart: u32, cause: u32) -> u64 {
    (u64::from(hart) << 32) | u64::from(cause)
}

fn fault(f: binding::IntcFault) -> IntcError {
    match f {
        binding::IntcFault::Unmapped(addr) => IntcError::Unmapped { addr },
        binding::IntcFault::Misaligned(addr) => IntcError::Misaligned { addr },
        binding::IntcFault::ReadOnly(addr) => IntcError::ReadOnly { addr },
        binding::IntcFault::NoSuchSource(s) => IntcError::NoSuchSource(s as u32),
        binding::IntcFault::TooManyContexts(n) => IntcError::TooManyContexts(n as usize),
        binding::IntcFault::WindowTooSmall { size, needed } => {
            IntcError::WindowTooSmall { size, needed }
        }
        binding::IntcFault::NoGuestFile { hart, vgein } => IntcError::NoGuestFile {
            hart: hart as u32,
            vgein: vgein as u32,
        },
        binding::IntcFault::Aia(e) => IntcError::from(port_fault(e)),
    }
}

fn clint_kind(k: ClintKind) -> u64 {
    match k {
        ClintKind::Clint => 0,
        ClintKind::AclintMswi => 1,
        ClintKind::AclintSswi => 2,
        ClintKind::AclintMtimer => 3,
    }
}

pub(super) struct Core(binding::Fabric);

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core")
            .field("plics", &binding::fabric_plic_count(&self.0))
            .field("aplics", &binding::fabric_aplic_count(&self.0))
            .field("lost_msis", &self.lost_msis())
            .finish_non_exhaustive()
    }
}

impl Core {
    pub(super) fn new(platform: &Platform) -> Result<Self, IntcError> {
        let mut f = binding::fabric_new();
        for p in &platform.plics {
            let ctx: Vec<u64> = p.contexts.iter().map(enc).collect();
            binding::fabric_add_plic(&mut f, p.base, p.size, u64::from(p.ndev), &ctx)
                .map_err(fault)?;
        }
        // A file whose hart and level a PLIC context or a direct APLIC IDC
        // also reaches supports eidelivery = 0x40000000; guest files never
        // do (AIA §3.8.1).
        let wired = |t: &HartTarget| {
            platform.plics.iter().any(|p| p.contexts.contains(t))
                || platform.aplics.iter().any(|a| a.idcs.contains(t))
        };
        for info in &platform.imsics {
            let targets: Vec<u64> = info.targets.iter().map(enc).collect();
            let ext: Vec<bool> = info.targets.iter().map(wired).collect();
            binding::fabric_add_imsic(
                &mut f,
                u64::from(info.guest_index_bits),
                u64::from(info.ids),
                &targets,
                &info.files,
                &ext,
            )
            .map_err(fault)?;
        }
        for a in &platform.aplics {
            let idcs: Vec<u64> = a.idcs.iter().map(enc).collect();
            let root = a.parent.is_none();
            // The root domain is M-level; its descendants are S-level.
            binding::fabric_add_aplic(
                &mut f,
                a.base,
                a.size,
                u64::from(a.sources),
                &idcs,
                a.msi_parent.is_some(),
                root,
                !root,
                a.children.len() as u64,
            )
            .map_err(|e| match fault(e) {
                IntcError::Aia(AiaError::NoDeliveryMode { .. }) => {
                    IntcError::Aia(AiaError::NoDeliveryMode {
                        domain: a.path.clone(),
                    })
                }
                e => e,
            })?;
        }
        let index = |path: &String| platform.aplics.iter().position(|a| &a.path == path);
        for (d, a) in platform.aplics.iter().enumerate() {
            let parent = a.parent.as_ref().and_then(index).map_or(NONE, |i| i as u64);
            let children: Vec<u64> = a
                .children
                .iter()
                .filter_map(index)
                .map(|i| i as u64)
                .collect();
            binding::fabric_link_aplic(&mut f, d as u64, parent, &children);
        }
        for c in &platform.clints {
            let bases: Vec<u64> = c.regs.iter().map(|r| r.base).collect();
            let sizes: Vec<u64> = c.regs.iter().map(|r| r.size).collect();
            let targets: Vec<u64> = c.targets.iter().map(enc).collect();
            binding::fabric_add_clint(&mut f, clint_kind(c.kind), &bases, &sizes, &targets);
        }
        Ok(Self(f))
    }

    // ------------------------------------------------------------ PLICs

    pub(super) fn plic_count(&self) -> usize {
        binding::fabric_plic_count(&self.0) as usize
    }

    pub(super) fn plic_base(&self, p: usize) -> u64 {
        binding::fabric_plic_base(&self.0, p as u64)
    }

    /// The context of `want` on PLIC `p`.
    pub(super) fn plic_context(&self, p: usize, want: HartTarget) -> Option<usize> {
        let c = binding::fabric_plic_context(&self.0, p as u64, enc(&want));
        (c != NONE).then_some(c as usize)
    }

    pub(super) fn plic_set_edge(
        &mut self,
        p: usize,
        src: u32,
        edge: bool,
    ) -> Result<(), IntcError> {
        binding::fabric_plic_set_edge(&mut self.0, p as u64, u64::from(src), edge).map_err(fault)
    }

    pub(super) fn ext_context(&self, hart: u32, cause: u32) -> Option<(usize, usize)> {
        let w = want(hart, cause);
        (0..self.plic_count()).find_map(|p| {
            let c = binding::fabric_plic_context(&self.0, p as u64, w);
            (c != NONE).then_some((p, c as usize))
        })
    }

    // ------------------------------------------------------------ APLICs

    pub(super) fn aplic_count(&self) -> usize {
        binding::fabric_aplic_count(&self.0) as usize
    }

    fn aplic_header(&self, d: usize) -> binding::AplicHdr {
        binding::fabric_aplic_header(&self.0, d as u64).to_value()
    }

    pub(super) fn aplic_base(&self, d: usize) -> u64 {
        self.aplic_header(d).base
    }

    pub(super) fn aplic_sources(&self, d: usize) -> u32 {
        self.aplic_header(d).sources as u32
    }

    pub(super) fn aplic_is_delegated_here(&self, d: usize, src: u32) -> bool {
        binding::fabric_aplic_is_delegated_here(&self.0, d as u64, u64::from(src))
    }

    pub(super) fn aplic_msi_cfg(&self, d: usize) -> MsiAddrCfg {
        cfg_from_port(&self.aplic_header(d).msicfg)
    }

    pub(super) fn aplic_is_supervisor(&self, d: usize) -> bool {
        self.aplic_header(d).supervisor
    }

    /// A register write to domain `d` without settling the fabric.
    pub(super) fn aplic_write32(&mut self, d: usize, addr: u64, v: u32) -> Result<(), IntcError> {
        binding::fabric_aplic_write32(&mut self.0, d as u64, addr, u64::from(v)).map_err(fault)
    }

    /// The root of `d`'s domain tree.
    pub(super) fn root_of(&self, d: usize) -> usize {
        binding::fabric_root_of(&self.0, d as u64) as usize
    }

    // ------------------------------------------------------------ IMSIC files

    /// The IMSIC and file of `(hart, cause)`.
    pub(super) fn imsic_file(&self, hart: u32, cause: u32) -> Option<(usize, usize)> {
        let bk = binding::fabric_imsic_file(&self.0, want(hart, cause));
        (bk != NONE).then_some(((bk >> 32) as usize, (bk & 0xffff_ffff) as usize))
    }

    /// The implemented files (guest 0 and the guest files) behind file `k`
    /// of IMSIC `b`.
    pub(super) fn files_len(&self, b: usize, k: usize) -> usize {
        binding::fabric_files_len(&self.0, b as u64, k as u64) as usize
    }

    pub(super) fn truncate_files(&mut self, b: usize, k: usize, keep: usize) {
        binding::fabric_truncate_files(&mut self.0, b as u64, k as u64, keep as u64)
    }

    pub(super) fn file_csr_read(
        &self,
        b: usize,
        k: usize,
        g: usize,
        isel: u32,
    ) -> Result<u64, IntcError> {
        binding::fabric_file_csr_read(&self.0, b as u64, k as u64, g as u64, u64::from(isel))
            .map_err(fault)
    }

    pub(super) fn file_csr_write(
        &mut self,
        b: usize,
        k: usize,
        g: usize,
        isel: u32,
        v: u64,
    ) -> Result<(), IntcError> {
        binding::fabric_file_csr_write(
            &mut self.0,
            b as u64,
            k as u64,
            g as u64,
            u64::from(isel),
            v,
        )
        .map_err(fault)
    }

    pub(super) fn file_topei(&self, b: usize, k: usize, g: usize) -> u32 {
        binding::fabric_file_topei(&self.0, b as u64, k as u64, g as u64) as u32
    }

    pub(super) fn file_claim(&mut self, b: usize, k: usize, g: usize) -> u32 {
        binding::fabric_file_claim(&mut self.0, b as u64, k as u64, g as u64) as u32
    }

    pub(super) fn file_is_pending(&self, b: usize, k: usize, g: usize, id: u32) -> bool {
        binding::fabric_file_is_pending(&self.0, b as u64, k as u64, g as u64, u64::from(id))
    }

    pub(super) fn file_eidelivery(&self, b: usize, k: usize) -> u32 {
        binding::fabric_file_eidelivery(&self.0, b as u64, k as u64) as u32
    }

    pub(super) fn file_supports_external_delivery(&self, b: usize, k: usize) -> bool {
        binding::fabric_file_external(&self.0, b as u64, k as u64)
    }

    /// The IMSIC file `(b, k, g)` whose page holds `addr`, if implemented.
    pub(super) fn locate_file(&self, addr: u64) -> Option<(usize, usize, usize)> {
        let l = binding::fabric_locate_file(&self.0, addr).to_value();
        (l.k != NONE).then_some((
            (l.k >> 32) as usize,
            (l.k & 0xffff_ffff) as usize,
            l.g as usize,
        ))
    }

    // ------------------------------------------------------------ levels

    /// The level of `cause` on `hart`.
    pub(super) fn level(&self, hart: u32, cause: u32) -> bool {
        binding::fabric_level(&self.0, u64::from(hart), u64::from(cause))
    }

    // ------------------------------------------------------------ hypervisor

    /// `hart`'s S-level IMSIC file and its GEILEN (guest files
    /// `1..=GEILEN`).
    pub(super) fn guest_files(&self, hart: u32) -> Option<(usize, usize, u32)> {
        let l = binding::fabric_guest_files(&self.0, u64::from(hart)).to_value();
        (l.k != NONE).then_some((
            (l.k >> 32) as usize,
            (l.k & 0xffff_ffff) as usize,
            l.g as u32,
        ))
    }

    pub(super) fn geilen(&self, hart: u32) -> u32 {
        binding::fabric_geilen(&self.0, u64::from(hart)) as u32
    }

    /// `hgeip`: bit `g` is guest file `g`'s interrupt output.
    pub(super) fn hgeip(&self, hart: u32) -> u64 {
        binding::fabric_hgeip(&self.0, u64::from(hart))
    }

    pub(super) fn vgein(&self, hart: u32) -> u32 {
        binding::fabric_vgein(&self.0, u64::from(hart)) as u32
    }

    pub(super) fn set_vgein(&mut self, hart: u32, vgein: u32) {
        binding::fabric_set_vgein(&mut self.0, u64::from(hart), u64::from(vgein))
    }

    pub(super) fn hgeie(&self, hart: u32) -> u64 {
        binding::fabric_hgeie(&self.0, u64::from(hart))
    }

    pub(super) fn set_hgeie(&mut self, hart: u32, v: u64) {
        binding::fabric_set_hgeie(&mut self.0, u64::from(hart), v)
    }

    /// The harts with hypervisor CSR state, in order.
    pub(super) fn hyp_harts(&self) -> Vec<u32> {
        let mut harts: Vec<u32> = binding::fabric_hyp_harts(&self.0)
            .into_iter()
            .map(|h| h as u32)
            .collect();
        harts.sort_unstable();
        harts
    }

    /// Masks a hart's `hgeie` to its current guest files (after
    /// [`Core::truncate_files`]).
    pub(super) fn clip_hgeie(&mut self, hart: u32) {
        binding::fabric_clip_hgeie(&mut self.0, u64::from(hart))
    }

    pub(super) fn vs_csr_read(&mut self, hart: u32, isel: u32) -> Result<u64, IntcError> {
        binding::fabric_vs_csr_read(&self.0, u64::from(hart), u64::from(isel)).map_err(fault)
    }

    pub(super) fn vs_csr_write(&mut self, hart: u32, isel: u32, v: u64) -> Result<(), IntcError> {
        binding::fabric_vs_csr_write(&mut self.0, u64::from(hart), u64::from(isel), v)
            .map_err(fault)
    }

    pub(super) fn vstopei(&mut self, hart: u32) -> Result<u32, IntcError> {
        binding::fabric_vstopei(&self.0, u64::from(hart))
            .map(|v| v as u32)
            .map_err(fault)
    }

    pub(super) fn claim_vstopei(&mut self, hart: u32) -> Result<u32, IntcError> {
        binding::fabric_claim_vstopei(&mut self.0, u64::from(hart))
            .map(|v| v as u32)
            .map_err(fault)
    }

    // ------------------------------------------------------------ MSIs

    /// Delegation and the root's MSI configuration flow from parents to
    /// children; then MSI-mode domains forward, and MSIs land in IMSIC
    /// files (or APLIC `setipnum_le` registers).
    pub(super) fn settle(&mut self) {
        binding::fabric_settle(&mut self.0)
    }

    /// A 32-bit write of `data` to `addr` by a bus master.
    pub(super) fn deliver_msi(&mut self, addr: u64, data: u32) -> Result<(), IntcError> {
        binding::fabric_deliver_msi(&mut self.0, addr, u64::from(data)).map_err(fault)
    }

    pub(super) fn lost_msis(&self) -> Vec<Msi> {
        binding::fabric_lost_msis(&self.0)
            .into_iter()
            .map(|m| Msi {
                addr: m.addr,
                data: m.data as u32,
            })
            .collect()
    }

    pub(super) fn push_lost_msi(&mut self, m: Msi) {
        let m = binding::repr::Msi::from_value(&binding::Msi {
            addr: m.addr,
            data: u64::from(m.data),
        });
        binding::fabric_push_lost_msi(&mut self.0, &m)
    }

    // ------------------------------------------------------------ registers

    pub(super) fn read32(&mut self, addr: u64) -> Result<u32, IntcError> {
        binding::fabric_read32(&mut self.0, addr)
            .map(|v| v as u32)
            .map_err(fault)
    }

    pub(super) fn write32(&mut self, addr: u64, v: u32) -> Result<(), IntcError> {
        binding::fabric_write32(&mut self.0, addr, u64::from(v)).map_err(fault)
    }

    /// The MTIME that `hart`'s timer compares against (0 if it has none).
    pub(super) fn hart_mtime(&self, hart: u32) -> u64 {
        binding::fabric_hart_mtime(&self.0, u64::from(hart))
    }

    /// Advances every MTIME counter by `ticks` (wrapping).
    pub(super) fn advance_mtime(&mut self, ticks: u64) {
        binding::fabric_advance_mtime(&mut self.0, ticks)
    }

    /// Clears `hart`'s SSIP on every SSWI.
    pub(super) fn clear_ssip(&mut self, hart: u32) {
        binding::fabric_clear_ssip(&mut self.0, u64::from(hart))
    }

    // ------------------------------------------------------------ wires and claims

    /// Drives a wire into `ctl` (a PLIC, or any domain of an APLIC tree:
    /// the wire enters the root and every domain sees it).
    pub(super) fn set_wire(&mut self, ctl: Ctl, src: u32, high: bool) -> Result<(), IntcError> {
        let (kind, a) = match ctl {
            Ctl::Plic(p) => (0, p),
            Ctl::Aplic(d) => (1, d),
            Ctl::Imsic(..) => return Err(IntcError::NoSuchSource(src)),
        };
        binding::fabric_set_wire(&mut self.0, kind, a as u64, u64::from(src), high).map_err(fault)
    }

    /// Claims one external interrupt for `(hart, cause)`: the IMSIC file
    /// first, then APLIC IDCs, then PLIC contexts. `Some((ctl, number))`, or
    /// `None` when nothing is claimable (an APLIC `iforce` claim is
    /// reported as `Some((Aplic, 0))`).
    pub(super) fn claim_external(&mut self, hart: u32, cause: u32) -> Option<(Ctl, u32)> {
        let c = binding::fabric_claim_external(&mut self.0, u64::from(hart), u64::from(cause))
            .to_value();
        if !c.found {
            return None;
        }
        let ctl = match c.kind {
            0 => Ctl::Plic(c.a as usize),
            1 => Ctl::Aplic(c.a as usize),
            _ => Ctl::Imsic(c.a as usize, c.b as usize),
        };
        Some((ctl, c.number as u32))
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
        let (kind, a) = match ctl {
            Ctl::Plic(p) => (0, p),
            Ctl::Aplic(d) => (1, d),
            Ctl::Imsic(b, _) => (2, b),
        };
        let (fd, fs) = forwarded.map_or((NONE, 0), |(d, s)| (d as u64, u64::from(s)));
        binding::fabric_complete_external(
            &mut self.0,
            u64::from(hart),
            u64::from(cause),
            kind,
            a as u64,
            u64::from(number),
            fd,
            fs,
        )
    }
}
