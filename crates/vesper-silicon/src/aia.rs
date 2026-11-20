//! RISC-V Advanced Interrupt Architecture (AIA 1.0) register models: the
//! APLIC (one interrupt domain per [`AplicDomain`]) and IMSIC interrupt
//! files ([`ImsicFile`]), configured from devicetree numbers
//! ([`crate::dtb::AplicInfo`], [`crate::dtb::ImsicInfo`]). [`crate::intc`]
//! wires them to harts.
//!
//! What is modelled, following the AIA specification 1.0 (ratified June 2023):
//!
//! * APLIC `domaincfg` (IE, DM; DM is read-only when the domain supports one
//!   delivery mode, little-endian only), `sourcecfg` with delegation (D and
//!   child index) and the six source modes, the pending and enable arrays
//!   and their `*num` registers, `setipnum_le`/`setipnum_be`, `target` in
//!   both formats, `genmsi`, the root domain's `mmsiaddrcfg[h]` /
//!   `smsiaddrcfg[h]` with the lock bit, and the direct-mode IDCs
//!   (`idelivery`, `iforce`, `ithreshold`, `topi`, `claimi`).
//! * Pending-bit rules of §4.7: edge modes latch a rising edge of the
//!   rectified input; level modes in direct delivery track the rectified
//!   input; level modes in MSI delivery latch a rising edge, accept a
//!   `setip`/`setipnum` only while the rectified input is high and clear when
//!   it falls; a forwarded MSI or a claim clears the bit.
//! * MSI addresses as §4.9.1 computes them from the hart, guest and the
//!   `msiaddrcfg` fields.
//! * IMSIC interrupt files (§3): `seteipnum_le` / `seteipnum_be` MMIO, and
//!   the indirect CSRs `eidelivery`, `eithreshold`, `eip*`, `eie*` (RV64
//!   numbering: only even-numbered `eip`/`eie` registers exist) and
//!   `*topei` (read and claim). A lower identity has the higher priority.
//! * `eidelivery` value 0x40000000 (§3.8.1): a file built with
//!   [`ImsicFile::with_external_delivery`] accepts it (and resets to it); the
//!   file then behaves as though `eidelivery` were 0 and the hart's
//!   external interrupt at that level comes from the APLIC or PLIC instead
//!   ([`crate::intc`] does the switching). Other files, including every
//!   guest file, keep bit 0 of a write, as QEMU 8.2.2 does.
//!
//! Model choices where the specification leaves room or the hardware
//! varies: IPRIOLEN is 8 (QEMU's choice); a reserved source mode (2 or 3)
//! is stored as Inactive; a Detached or Inactive source's rectified input
//! is 0. QEMU 8.2.2 differs from the specification
//! in four places that the cross-check test records (`tests/aia_qemu.rs`).
//!
//! This is a software model of the register protocol; there is no RISC-V
//! core and no bus timing. Guest files raise their bit of `hgeip`;
//! [`crate::intc`] models `hstatus.VGEIN`, `hgeie`, `hgeip`, `vsiselect` /
//! `vsireg` and `vstopei` over them.

/// IMSIC interrupt-file page size.
pub const IMSIC_PAGE: u64 = 0x1000;
/// `*iselect` numbers of the IMSIC indirect registers.
pub const ISEL_EIDELIVERY: u32 = 0x70;
pub const ISEL_EITHRESHOLD: u32 = 0x72;
pub const ISEL_EIP0: u32 = 0x80;
pub const ISEL_EIE0: u32 = 0xc0;
/// `eidelivery` values (§3.8.1).
pub const EIDELIVERY_OFF: u32 = 0;
pub const EIDELIVERY_ON: u32 = 1;
/// Delivery from a PLIC or APLIC through this file's position (optional).
pub const EIDELIVERY_EXTERNAL: u32 = 0x4000_0000;
/// Smallest and largest IMSIC identity count the binding allows.
pub const IMSIC_MIN_IDS: u32 = 63;
pub const IMSIC_MAX_IDS: u32 = 2047;
/// Largest APLIC source count.
pub const APLIC_MAX_SOURCES: u32 = 1023;
/// Implemented interrupt-priority bits of the APLIC IDCs.
pub const APLIC_IPRIOLEN: u32 = 8;

/// First IDC; each is 32 bytes.
pub const APLIC_IDC: u64 = 0x4000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AiaError {
    /// Not a register of this block (or not for this width).
    Unmapped {
        addr: u64,
    },
    Misaligned {
        addr: u64,
    },
    /// An `*iselect` value that names no IMSIC register (an illegal
    /// instruction on a real hart).
    BadIselect(u32),
    /// `riscv,num-ids` outside 63..=2047 or not one below a multiple of 64.
    BadIdCount(u32),
    /// `riscv,num-sources` outside 1..=1023.
    BadSourceCount(u32),
    /// The `reg` window is smaller than the registers need.
    WindowTooSmall {
        size: u64,
        needed: u64,
    },
    /// A domain with neither `msi-parent` nor `interrupts-extended`.
    NoDeliveryMode {
        domain: String,
    },
}

impl std::fmt::Display for AiaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for AiaError {}

/// An APLIC source mode (`sourcecfg.SM`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMode {
    Inactive,
    Detached,
    EdgeRising,
    EdgeFalling,
    LevelHigh,
    LevelLow,
}

impl SourceMode {
    pub fn bits(self) -> u32 {
        match self {
            SourceMode::Inactive => 0,
            SourceMode::Detached => 1,
            SourceMode::EdgeRising => 4,
            SourceMode::EdgeFalling => 5,
            SourceMode::LevelHigh => 6,
            SourceMode::LevelLow => 7,
        }
    }

    /// The mode a `sourcecfg.SM` value selects (reserved 2 and 3 are stored
    /// as Inactive).
    pub fn of(sm: u32) -> Self {
        match sm & 7 {
            1 => SourceMode::Detached,
            4 => SourceMode::EdgeRising,
            5 => SourceMode::EdgeFalling,
            6 => SourceMode::LevelHigh,
            7 => SourceMode::LevelLow,
            _ => SourceMode::Inactive,
        }
    }

    #[cfg_attr(vesper_port, allow(dead_code))]
    fn rectify(self, raw: bool) -> bool {
        match self {
            SourceMode::EdgeRising | SourceMode::LevelHigh => raw,
            SourceMode::EdgeFalling | SourceMode::LevelLow => !raw,
            SourceMode::Inactive | SourceMode::Detached => false,
        }
    }

    #[cfg_attr(vesper_port, allow(dead_code))]
    fn level(self) -> bool {
        matches!(self, SourceMode::LevelHigh | SourceMode::LevelLow)
    }
}

// `mmsiaddrcfgh`/`smsiaddrcfgh` fields (§4.5.4, §4.5.5): the PPN's bits
// 43:32, LHXW, HHXW, LHXS and HHXS, and the Lock bit.
#[cfg_attr(vesper_port, allow(dead_code))]
const MSIADDRCFGH_PPN: u32 = 0xfff;
#[cfg_attr(vesper_port, allow(dead_code))]
const MSIADDRCFGH_LHXW_SHIFT: u32 = 12;
#[cfg_attr(vesper_port, allow(dead_code))]
const MSIADDRCFGH_LHXW: u32 = 0xf;
#[cfg_attr(vesper_port, allow(dead_code))]
const MSIADDRCFGH_HHXW_SHIFT: u32 = 16;
#[cfg_attr(vesper_port, allow(dead_code))]
const MSIADDRCFGH_HHXW: u32 = 0x7;
#[cfg_attr(vesper_port, allow(dead_code))]
const MSIADDRCFGH_LHXS_SHIFT: u32 = 20;
#[cfg_attr(vesper_port, allow(dead_code))]
const MSIADDRCFGH_LHXS: u32 = 0x7;
#[cfg_attr(vesper_port, allow(dead_code))]
const MSIADDRCFGH_HHXS_SHIFT: u32 = 24;
#[cfg_attr(vesper_port, allow(dead_code))]
const MSIADDRCFGH_HHXS: u32 = 0x1f;
#[cfg_attr(vesper_port, allow(dead_code))]
const MSIADDRCFGH_L_SHIFT: u32 = 31;
// An MSI target address is a page number shifted by the 4 KiB page size.
#[cfg_attr(vesper_port, allow(dead_code))]
const PAGE_SHIFT: u32 = 12;

/// The `mmsiaddrcfg[h]` / `smsiaddrcfg[h]` fields (§4.5.3–4.5.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MsiAddrCfg {
    pub m_ppn: u64,
    pub s_ppn: u64,
    pub hhxs: u32,
    pub lhxs_m: u32,
    pub lhxs_s: u32,
    pub hhxw: u32,
    pub lhxw: u32,
    pub locked: bool,
}

impl MsiAddrCfg {
    /// The MSI target address of `hart` (and `guest`, S level only) as
    /// §4.9.1 computes it.
    pub fn address(&self, supervisor: bool, hart: u32, guest: u32) -> u64 {
        #[cfg(vesper_port)]
        {
            imp::msi_address(self, supervisor, hart, guest)
        }
        #[cfg(not(vesper_port))]
        {
            let mask = |w: u32| (1u64 << w) - 1;
            let g = (u64::from(hart) >> self.lhxw) & mask(self.hhxw);
            let h = u64::from(hart) & mask(self.lhxw);
            let (ppn, lhxs, guest) = if supervisor {
                (self.s_ppn, self.lhxs_s, u64::from(guest))
            } else {
                (self.m_ppn, self.lhxs_m, 0)
            };
            (ppn | (g << (self.hhxs + PAGE_SHIFT)) | (h << lhxs) | guest) << PAGE_SHIFT
        }
    }

    /// The hart index whose M-level (or S-level guest 0) file is at `addr`,
    /// if the address has that form.
    pub fn hart_of(&self, supervisor: bool, addr: u64, harts: u32) -> Option<u32> {
        (0..harts).find(|&h| self.address(supervisor, h, 0) == addr)
    }

    #[cfg_attr(vesper_port, allow(dead_code))]
    fn high_m(&self) -> u32 {
        (u32::from(self.locked) << MSIADDRCFGH_L_SHIFT)
            | (self.hhxs << MSIADDRCFGH_HHXS_SHIFT)
            | (self.lhxs_m << MSIADDRCFGH_LHXS_SHIFT)
            | (self.hhxw << MSIADDRCFGH_HHXW_SHIFT)
            | (self.lhxw << MSIADDRCFGH_LHXW_SHIFT)
            | ((self.m_ppn >> 32) as u32 & MSIADDRCFGH_PPN)
    }

    #[cfg_attr(vesper_port, allow(dead_code))]
    fn high_s(&self) -> u32 {
        (self.lhxs_s << MSIADDRCFGH_LHXS_SHIFT) | ((self.s_ppn >> 32) as u32 & MSIADDRCFGH_PPN)
    }
}

/// One message-signalled interrupt write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Msi {
    pub addr: u64,
    pub data: u32,
}

// The register models: the Rust build, or the forwarding twins of the
// port-backed package (port wave P3a lane 7, #1042).
#[cfg(not(vesper_port))]
#[path = "aia/rust.rs"]
mod imp;
#[cfg(vesper_port)]
#[path = "aia/port.rs"]
mod imp;
pub use imp::{AplicDomain, ImsicFile};
#[cfg(vesper_port)]
pub(crate) use imp::{cfg_from_port, fault as port_fault};
