//! The port-backed build of [`ImsicFile`] and [`AplicDomain`]: each method
//! forwards to the Vesper port (vesper/src/aia.ves) through the generated
//! binding. Plain fields are read in place through the handle's view; every
//! decision (delivery, priorities, pending-bit rules, register decoding)
//! runs in Vesper. `Clone`, `PartialEq` and `Debug` keep the Rust build's
//! behavior.

use super::*;
use crate::binding;
use crate::dtb::MemRange;

pub(crate) fn fault(f: binding::AiaFault) -> AiaError {
    match f {
        binding::AiaFault::Unmapped(addr) => AiaError::Unmapped { addr },
        binding::AiaFault::Misaligned(addr) => AiaError::Misaligned { addr },
        binding::AiaFault::BadIselect(i) => AiaError::BadIselect(i as u32),
        binding::AiaFault::BadIdCount(n) => AiaError::BadIdCount(n as u32),
        binding::AiaFault::BadSourceCount(n) => AiaError::BadSourceCount(n as u32),
        binding::AiaFault::WindowTooSmall { size, needed } => {
            AiaError::WindowTooSmall { size, needed }
        }
        // `AplicDomain::new` adds the domain's path.
        binding::AiaFault::NoDeliveryMode => AiaError::NoDeliveryMode {
            domain: String::new(),
        },
    }
}

pub(super) fn cfg_to_port(c: &MsiAddrCfg) -> binding::repr::MsiCfg {
    binding::repr::MsiCfg::from_value(&binding::MsiCfg {
        m_ppn: c.m_ppn,
        s_ppn: c.s_ppn,
        hhxs: u64::from(c.hhxs),
        lhxs_m: u64::from(c.lhxs_m),
        lhxs_s: u64::from(c.lhxs_s),
        hhxw: u64::from(c.hhxw),
        lhxw: u64::from(c.lhxw),
        locked: c.locked,
    })
}

pub(crate) fn cfg_from_port(c: &binding::MsiCfg) -> MsiAddrCfg {
    // The port keeps each field within its register width.
    MsiAddrCfg {
        m_ppn: c.m_ppn,
        s_ppn: c.s_ppn,
        hhxs: c.hhxs as u32,
        lhxs_m: c.lhxs_m as u32,
        lhxs_s: c.lhxs_s as u32,
        hhxw: c.hhxw as u32,
        lhxw: c.lhxw as u32,
        locked: c.locked,
    }
}

/// One IMSIC interrupt file.
pub struct ImsicFile(binding::Imsic);

impl Clone for ImsicFile {
    fn clone(&self) -> Self {
        Self(binding::imsic_clone(&self.0))
    }
}

impl PartialEq for ImsicFile {
    fn eq(&self, other: &Self) -> bool {
        binding::imsic_eq(&self.0, &other.0)
    }
}

impl Eq for ImsicFile {}

impl std::fmt::Debug for ImsicFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let v = self.0.as_ref();
        let h = self.header();
        f.debug_struct("ImsicFile")
            .field("ids", &(h.ids as u32))
            .field("delivery", &(h.delivery as u32))
            .field("external", &h.external)
            .field("threshold", &(h.threshold as u32))
            .field("eip", &v.eip().to_vec())
            .field("eie", &v.eie().to_vec())
            .finish()
    }
}

impl ImsicFile {
    fn header(&self) -> binding::ImsicHdr {
        binding::imsic_header(&self.0).to_value()
    }

    /// A file with identities `1..=ids` (`riscv,num-ids`).
    pub fn new(ids: u32) -> Result<Self, AiaError> {
        binding::imsic_new(u64::from(ids), false)
            .map(Self)
            .map_err(fault)
    }

    /// A file whose `eidelivery` also supports [`EIDELIVERY_EXTERNAL`] and
    /// resets to it.
    pub fn with_external_delivery(ids: u32) -> Result<Self, AiaError> {
        binding::imsic_new(u64::from(ids), true)
            .map(Self)
            .map_err(fault)
    }

    pub fn ids(&self) -> u32 {
        self.header().ids as u32
    }

    /// The `eidelivery` value.
    pub fn eidelivery(&self) -> u32 {
        self.header().delivery as u32
    }

    /// Whether `eidelivery` selects the PLIC or APLIC
    /// ([`EIDELIVERY_EXTERNAL`]).
    pub fn delivers_external(&self) -> bool {
        self.eidelivery() == EIDELIVERY_EXTERNAL
    }

    pub fn supports_external_delivery(&self) -> bool {
        self.header().external
    }

    /// An MSI with data `id` arrived (a write to `seteipnum_le`).
    pub fn seteipnum(&mut self, id: u32) {
        binding::imsic_seteipnum(&mut self.0, u64::from(id))
    }

    pub fn is_pending(&self, id: u32) -> bool {
        binding::imsic_is_pending(&self.0, u64::from(id))
    }

    pub fn is_enabled(&self, id: u32) -> bool {
        binding::imsic_is_enabled(&self.0, u64::from(id))
    }

    /// The 32-bit MMIO write at offset `off` in the file's page.
    pub fn mmio_write32(&mut self, off: u64, v: u32) {
        binding::imsic_mmio_write32(&mut self.0, off, u64::from(v))
    }

    /// `*ireg` read with `*iselect = isel`.
    pub fn csr_read(&self, isel: u32) -> Result<u64, AiaError> {
        binding::imsic_csr_read(&self.0, u64::from(isel)).map_err(fault)
    }

    /// `*ireg` write.
    pub fn csr_write(&mut self, isel: u32, v: u64) -> Result<(), AiaError> {
        binding::imsic_csr_write(&mut self.0, u64::from(isel), v).map_err(fault)
    }

    /// The highest-priority (lowest) identity that is pending and enabled
    /// and below a nonzero `eithreshold`, or 0.
    pub fn top(&self) -> u32 {
        binding::imsic_top(&self.0) as u32
    }

    /// `*topei` read.
    pub fn topei(&self) -> u32 {
        binding::imsic_topei(&self.0) as u32
    }

    /// A write to `*topei`: claims the identity a read would return.
    pub fn claim(&mut self) -> u32 {
        binding::imsic_claim(&mut self.0) as u32
    }

    /// The file's interrupt output to its hart (or its `hgeip` bit).
    pub fn asserted(&self) -> bool {
        binding::imsic_asserted(&self.0)
    }
}

/// The MSI target address of `hart` (and `guest`) as §4.9.1 computes it.
pub(super) fn msi_address(c: &MsiAddrCfg, supervisor: bool, hart: u32, guest: u32) -> u64 {
    binding::msi_address(
        &cfg_to_port(c),
        supervisor,
        u64::from(hart),
        u64::from(guest),
    )
}

fn mode_of_code(code: u64) -> SourceMode {
    SourceMode::of(code as u32)
}

/// One APLIC interrupt domain.
pub struct AplicDomain(binding::Aplic);

impl Clone for AplicDomain {
    fn clone(&self) -> Self {
        Self(binding::aplic_clone(&self.0))
    }
}

impl std::fmt::Debug for AplicDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let v = self.0.as_ref();
        let h = self.header();
        f.debug_struct("AplicDomain")
            .field("base", &h.base)
            .field("sources", &(h.sources as u32))
            .field("supervisor", &h.supervisor)
            .field("root", &h.root)
            .field("msi_capable", &h.msi_capable)
            .field("direct_capable", &h.direct_capable)
            .field("children", &(h.children as u32))
            .field("ie", &h.ie)
            .field("dm", &h.dm)
            .field("sourcecfg", &v.sourcecfg().to_vec())
            .field("active_here", &v.active_here().to_vec())
            .field("raw", &v.raw().to_vec())
            .field("pending", &v.pending().to_vec())
            .field("enabled", &v.enabled().to_vec())
            .field("target", &v.target().to_vec())
            .field("idc_delivery", &v.idc_delivery().to_vec())
            .field("idc_force", &v.idc_force().to_vec())
            .field("idc_threshold", &v.idc_threshold().to_vec())
            .field("msicfg", &cfg_from_port(&h.msicfg))
            .field("outbox", &v.outbox().to_vec())
            .finish()
    }
}

impl AplicDomain {
    fn header(&self) -> binding::AplicHdr {
        binding::aplic_header(&self.0).to_value()
    }

    /// A domain at `window` with `sources` sources and `idcs` interrupt
    /// delivery controls (direct mode; 0 when the domain has no
    /// `interrupts-extended`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        path: &str,
        window: MemRange,
        sources: u32,
        idcs: usize,
        msi_capable: bool,
        root: bool,
        supervisor: bool,
        children: u32,
    ) -> Result<Self, AiaError> {
        binding::aplic_new(
            window.base,
            window.size,
            u64::from(sources),
            idcs as u64,
            msi_capable,
            root,
            supervisor,
            u64::from(children),
        )
        .map(Self)
        .map_err(|f| match fault(f) {
            AiaError::NoDeliveryMode { .. } => AiaError::NoDeliveryMode {
                domain: path.to_string(),
            },
            e => e,
        })
    }

    pub fn base(&self) -> u64 {
        self.header().base
    }

    pub fn sources(&self) -> u32 {
        self.header().sources as u32
    }

    pub fn is_root(&self) -> bool {
        self.header().root
    }

    pub fn is_supervisor(&self) -> bool {
        self.header().supervisor
    }

    /// MSI delivery mode (`domaincfg.DM`).
    pub fn msi_mode(&self) -> bool {
        self.header().dm
    }

    pub fn idc_count(&self) -> usize {
        self.0.as_ref().idc_threshold().len()
    }

    /// The window the registers occupy.
    pub fn contains(&self, addr: u64) -> bool {
        binding::aplic_contains(&self.0, addr)
    }

    /// The source mode in this domain.
    pub fn mode(&self, src: u32) -> SourceMode {
        mode_of_code(binding::aplic_mode(&self.0, u64::from(src)))
    }

    /// The child index source `src` is delegated to, if any.
    pub fn delegated_to(&self, src: u32) -> Option<u32> {
        binding::aplic_delegated_to(&self.0, u64::from(src)).map(|c| c as u32)
    }

    /// Whether `src` belongs to this domain.
    pub fn is_delegated_here(&self, src: u32) -> bool {
        binding::aplic_is_delegated_here(&self.0, u64::from(src))
    }

    /// Whether the parent delegates `src` to this domain (fabric use).
    pub fn set_active_here(&mut self, src: u32, active: bool) {
        binding::aplic_set_active_here(&mut self.0, u64::from(src), active)
    }

    pub fn is_pending(&self, src: u32) -> bool {
        binding::aplic_is_pending(&self.0, u64::from(src))
    }

    pub fn rectified(&self, src: u32) -> bool {
        binding::aplic_rectified(&self.0, u64::from(src))
    }

    /// The wire of source `src` changed to `high` (fabric use).
    pub fn set_wire(&mut self, src: u32, high: bool) {
        binding::aplic_set_wire(&mut self.0, u64::from(src), high)
    }

    /// The root's MSI address configuration.
    pub fn msi_cfg(&self) -> MsiAddrCfg {
        cfg_from_port(&self.header().msicfg)
    }

    /// Fabric use: a child computes its MSI addresses with the root's
    /// configuration.
    pub fn set_msi_cfg(&mut self, cfg: MsiAddrCfg) {
        binding::aplic_set_msi_cfg(&mut self.0, &cfg_to_port(&cfg))
    }

    /// The direct-mode IDC `topi` value.
    pub fn topi(&self, idc: usize) -> u32 {
        binding::aplic_topi(&self.0, idc as u64) as u32
    }

    /// `claimi` read.
    pub fn claimi(&mut self, idc: usize) -> u32 {
        binding::aplic_claimi(&mut self.0, idc as u64) as u32
    }

    /// The IDC's interrupt output.
    pub fn idc_asserted(&self, idc: usize) -> bool {
        binding::aplic_idc_asserted(&self.0, idc as u64)
    }

    /// Forwards every pending, enabled source as an MSI and returns the
    /// MSIs produced since the last call (including `genmsi`).
    pub fn take_msis(&mut self) -> Vec<Msi> {
        binding::aplic_take_msis(&mut self.0)
            .into_iter()
            .map(|m| Msi {
                addr: m.addr,
                data: m.data as u32,
            })
            .collect()
    }

    /// A 32-bit MMIO read.
    pub fn read32(&mut self, addr: u64) -> Result<u32, AiaError> {
        binding::aplic_read32(&mut self.0, addr)
            .map(|v| v as u32)
            .map_err(fault)
    }

    /// A 32-bit MMIO write.
    pub fn write32(&mut self, addr: u64, v: u32) -> Result<(), AiaError> {
        binding::aplic_write32(&mut self.0, addr, u64::from(v)).map_err(fault)
    }
}
