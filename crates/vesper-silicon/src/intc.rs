//! RISC-V interrupt controllers driving the IrqGuard [`Cpu`] model (§26.1
//! item 3), configured from devicetree numbers: PLICs
//! ([`crate::dtb::Platform::plics`]), CLINT / ACLINT blocks
//! ([`crate::dtb::Platform::clints`]) and the AIA APLIC domains and IMSICs
//! ([`crate::dtb::Platform::aplics`], [`crate::dtb::Platform::imsics`],
//! register models in [`crate::aia`]).
//!
//! [`RiscvIntc`] owns one [`Cpu`] per enabled hart and a software model of
//! the platform's interrupt blocks:
//!
//! * every PLIC as the RISC-V PLIC specification (1.0.0) lays it out:
//!   per-source priority at `base + 4*src`, pending bits at `0x1000`,
//!   per-context enables at `0x2000 + 0x80*ctx`, threshold at
//!   `0x20_0000 + 0x1000*ctx` and claim/complete one word above it.
//!   Gateways are level-triggered (a source is forwarded while its line is
//!   high and it is not in service; a completion of an enabled, in-service
//!   source re-forwards a line that is still high) or, on the two-cell PLIC
//!   bindings (T-Head C900, Andes NCEPLIC100) for sources the devicetree
//!   marks rising-edge, edge-triggered (a rising edge forwards a request; one
//!   edge that arrives while the source is in service is held and forwarded
//!   at completion). A claim returns the highest-priority pending source
//!   enabled for the context (ties to the lowest id) and puts it in service.
//!   Priorities and thresholds are 3 bits wide (values 0..=7, as QEMU's
//!   `sifive_plic` on `virt` uses). A platform may have several PLICs (QEMU
//!   `virt` with several sockets has one per socket): each has its own
//!   sources and contexts, and a hart's external level is the OR of its
//!   contexts on all of them;
//! * the APLIC domains and IMSIC interrupt files of the AIA (see
//!   [`crate::aia`]). Wires enter the root domain and reach the domain the
//!   source is delegated to; MSIs that MSI-mode domains forward (and that
//!   devices write, [`RiscvIntc::msi_write`]) set pending bits in the IMSIC
//!   file at the written address. When the modelled software runs in
//!   S-mode, the M-mode firmware's part is done at construction, as OpenSBI
//!   does: the MSI address configuration is programmed from the IMSIC nodes
//!   and locked, and the sources the devicetree's `riscv,delegation` names
//!   are delegated;
//! * the SiFive CLINT (MSIP at `4*k`, MTIMECMP at `0x4000 + 8*k`, MTIME at
//!   `0xbff8`) and the ACLINT MSWI, SSWI and MTIMER devices (MTIMER `reg`
//!   is MTIME then the MTIMECMP array, as QEMU emits it). MTIP is the level
//!   `mtime >= mtimecmp`, against the MTIME of the hart's own block (blocks
//!   that name the same MTIME address share one counter); MTIMECMP starts at
//!   `u64::MAX` (its reset value is unspecified);
//! * the hart side: a cause is enabled (`mie`) once a handler is installed
//!   for it; the controller's output levels are re-evaluated after every
//!   operation ([`RiscvIntc::sync`]) and a rising level raises the cause on
//!   that hart's [`Cpu`], where `mstatus.MIE` (the [`crate::irq::IrqGuard`])
//!   decides whether it runs now or stays pending. External-interrupt
//!   delivery claims, runs the ISR and completes, until nothing is left: it
//!   claims from the hart's IMSIC file (`*topei`), then its APLIC IDCs
//!   (`claimi`), then its PLIC contexts. A level that stays high after
//!   [`STORM_BUDGET`] handler runs is counted as a storm and left asserted.
//!
//! An IMSIC file whose hart and level a PLIC context or a direct APLIC IDC
//! also reaches supports `eidelivery` = 0x40000000 (and resets to it): the
//! hart's external interrupt at that level then comes from the PLIC/APLIC
//! instead of the file (AIA §3.8.1); with 1 it comes only from the file.
//! Routing a wired source selects 0x40000000 (refused with
//! [`IntcError::ImsicOwnsLevel`] while the file delivers its own MSIs);
//! routing an MSI selects 1.
//!
//! Hypervisor level (H extension): each S-level file is followed by guest
//! files `1..=GEILEN` (`2^riscv,guest-index-bits - 1` unless
//! [`RiscvIntc::limit_guest_files`] says fewer, as QEMU's `aia-guests`
//! does). `hgeip` bit `g` is guest file `g`'s output; `hgeie` has writable
//! bits `GEILEN:1`; SGEIP (12) is `hgeip & hgeie != 0` and VSEIP (10) is
//! `hgeip[VGEIN]`; `vsiselect`/`vsireg` and `vstopei` reach the file
//! `hstatus.VGEIN` names, else [`IntcError::NoGuestFile`] (the
//! illegal-instruction trap). [`RiscvIntc::install_guest_msi`] gives HS-mode
//! software a VS-level handler: on SGEIP it switches VGEIN to each guest
//! with a live bit and claims through `vstopei`. `hvip`, `hvictl`,
//! `vsie` and the virtual hart itself are not modelled.
//!
//! PCI MSIs ([`RiscvIntc::pci_msi_write`]) take the controller and device
//! ID their requester ID maps to through `msi-map`/`msi-parent`, and the
//! host's `dma-ranges`; with no IOMMU the write lands wherever its CPU
//! address decodes, and the record says whether that was the routed IMSIC.
//!
//! Machine-mode software ([`Privilege::Machine`]) receives MEIP (11) from
//! its M-level PLIC contexts, APLIC IDCs or IMSIC file, MSIP (3) and MTIP
//! (7); supervisor-mode software ([`Privilege::Supervisor`]) receives SEIP
//! (9) and SSIP (1) from the ACLINT SSWI. The supervisor timer comes from SBI
//! or Sstc, which a devicetree does not describe, so it is not modelled.
//!
//! This is a software model of the controllers' register protocol; it is
//! not a cycle model and runs no code on a RISC-V core.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::Arch;
use crate::aia::{AiaError, Msi, MsiAddrCfg, SourceMode};
use crate::dtb::{
    AplicInfo, ClintInfo, ClintKind, ControllerKind, DtbError, HartTarget, ImsicInfo, IrqLine,
    Platform, PlicInfo, Trigger,
};
use crate::irq::{Cpu, IsrCtx};
use crate::ledger::Ledger;

/// Supervisor software interrupt.
pub const SSIP: u32 = 1;
/// Machine software interrupt.
pub const MSIP: u32 = 3;
/// Machine timer interrupt.
pub const MTIP: u32 = 7;
/// Supervisor external interrupt.
pub const SEIP: u32 = 9;
/// Machine external interrupt.
pub const MEIP: u32 = 11;
/// Virtual supervisor external interrupt (`mip.VSEIP`, H extension).
pub const VSEIP: u32 = 10;
/// Supervisor guest external interrupt (`mip.SGEIP`, H extension).
pub const SGEIP: u32 = 12;
/// Largest GEILEN on RV64 (`hgeip` bits 63:1).
pub const MAX_GEILEN: u32 = 63;

/// Most handler runs for one delivery before the level is declared a storm.
pub const STORM_BUDGET: usize = 64;
/// Most PLIC contexts the specification's register map has room for.
pub const PLIC_MAX_CONTEXTS: usize = 15_872;
/// Largest PLIC priority / threshold value (3 implemented bits).
pub const PLIC_MAX_PRIORITY: u32 = 7;

const PLIC_PENDING: u64 = 0x1000;
const PLIC_ENABLE: u64 = 0x2000;
const PLIC_ENABLE_STRIDE: u64 = 0x80;
const PLIC_CONTEXT: u64 = 0x20_0000;
const PLIC_CONTEXT_STRIDE: u64 = 0x1000;
const CLINT_MTIMECMP: u64 = 0x4000;
const CLINT_MTIME: u64 = 0xbff8;
const APLIC_DOMAINCFG_IE: u32 = 1 << 8;
const APLIC_DOMAINCFG_DM: u32 = 1 << 2;

/// The privilege the modelled software runs at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Privilege {
    Machine,
    Supervisor,
}

impl Privilege {
    fn external(self) -> u32 {
        match self {
            Privilege::Machine => MEIP,
            Privilege::Supervisor => SEIP,
        }
    }

    fn software(self) -> u32 {
        match self {
            Privilege::Machine => MSIP,
            Privilege::Supervisor => SSIP,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntcError {
    /// The platform is not RISC-V.
    NotRiscv(Arch),
    /// Kept for compatibility: several PLICs are now modelled and this is
    /// no longer returned.
    MultiplePlics,
    NoPlic,
    NoSuchHart(u32),
    /// The hart has no PLIC context (or APLIC IDC) for this privilege's
    /// external cause on the device's controller.
    NoContext {
        hart: u32,
        cause: u32,
    },
    /// No CLINT/ACLINT output reaches this hart with this cause.
    NoLocalSource {
        hart: u32,
        cause: u32,
    },
    /// The supervisor timer is not described by a devicetree.
    TimerNeedsMachineMode,
    NoSuchSource(u32),
    PriorityTooHigh(u32),
    /// More contexts than [`PLIC_MAX_CONTEXTS`].
    TooManyContexts(usize),
    /// The PLIC `reg` window is smaller than its contexts need.
    WindowTooSmall {
        size: u64,
        needed: u64,
    },
    /// No modelled register at this address (or not for this width).
    Unmapped {
        addr: u64,
    },
    Misaligned {
        addr: u64,
    },
    /// A write to a read-only register (PLIC pending bits).
    ReadOnly {
        addr: u64,
    },
    /// A device is not wired to a PLIC or APLIC.
    NotOnPlic {
        device: String,
    },
    /// The controller cannot take this trigger type (an edge on a one-cell
    /// PLIC, both edges, or an unspecified type on an APLIC source).
    UnsupportedTrigger {
        device: String,
        trigger: Trigger,
    },
    /// The source is not active in the APLIC domain this privilege drives
    /// (an S-mode domain the firmware did not delegate it to).
    NotDelegated {
        domain: String,
        source: u32,
    },
    /// The hart has no IMSIC interrupt file at this privilege.
    NoImsic {
        hart: u32,
        cause: u32,
    },
    /// Every identity of the hart's IMSIC file is allocated.
    NoFreeIdentity {
        hart: u32,
    },
    /// The IMSIC files' addresses cannot be expressed with the binding's
    /// index fields (as the APLIC's MSI address configuration needs).
    ImsicLayout {
        imsic: String,
        detail: String,
    },
    /// `hstatus.VGEIN` is not the number of an implemented guest file of
    /// the hart, so `vsiselect`/`vsireg` (0x70..=0xff) and `vstopei` are
    /// inaccessible: an illegal-instruction exception from M or HS mode
    /// (AIA §2.3, §3.9).
    NoGuestFile {
        hart: u32,
        vgein: u32,
    },
    /// A guest file number outside `1..=GEILEN`.
    NoSuchGuest {
        hart: u32,
        guest: u32,
    },
    /// The hart's IMSIC file at this level delivers its own interrupts
    /// (`eidelivery` = 1), so a direct APLIC IDC or PLIC context cannot
    /// reach the hart until software selects 0x40000000.
    ImsicOwnsLevel {
        hart: u32,
        cause: u32,
    },
    /// Guest external interrupts are an HS-mode facility: the modelled
    /// software must run at [`Privilege::Supervisor`].
    GuestNeedsSupervisor,
    /// A PCI MSI resolved (through `msi-map` or `msi-parent`) to a
    /// controller that is not one of the platform's IMSICs.
    MsiControllerNotModelled {
        controller: String,
    },
    Aia(AiaError),
    Dtb(DtbError),
}

impl std::fmt::Display for IntcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for IntcError {}

impl From<DtbError> for IntcError {
    fn from(e: DtbError) -> Self {
        IntcError::Dtb(e)
    }
}

impl From<AiaError> for IntcError {
    fn from(e: AiaError) -> Self {
        match e {
            AiaError::Unmapped { addr } => IntcError::Unmapped { addr },
            AiaError::Misaligned { addr } => IntcError::Misaligned { addr },
            e => IntcError::Aia(e),
        }
    }
}

/// The PLIC register file and gateways.
#[derive(Clone, Debug)]
pub struct Plic {
    base: u64,
    ndev: u32,
    contexts: Vec<HartTarget>,
    priority: Vec<u32>,
    level: Vec<bool>,
    edge: Vec<bool>,
    /// An edge that arrived while the source was in service.
    held: Vec<bool>,
    pending: Vec<bool>,
    in_service: Vec<bool>,
    enable: Vec<Vec<bool>>,
    threshold: Vec<u32>,
}

impl Plic {
    pub fn new(info: &PlicInfo) -> Result<Self, IntcError> {
        if info.contexts.len() > PLIC_MAX_CONTEXTS {
            return Err(IntcError::TooManyContexts(info.contexts.len()));
        }
        let n = info.ndev as usize + 1;
        let nctx = info.contexts.len() as u64;
        let needed = PLIC_CONTEXT + PLIC_CONTEXT_STRIDE * nctx;
        if info.size < needed {
            return Err(IntcError::WindowTooSmall {
                size: info.size,
                needed,
            });
        }
        Ok(Self {
            base: info.base,
            ndev: info.ndev,
            contexts: info.contexts.clone(),
            priority: vec![0; n],
            level: vec![false; n],
            edge: vec![false; n],
            held: vec![false; n],
            pending: vec![false; n],
            in_service: vec![false; n],
            enable: vec![vec![false; n]; info.contexts.len()],
            threshold: vec![0; info.contexts.len()],
        })
    }

    pub fn base(&self) -> u64 {
        self.base
    }

    pub fn contexts(&self) -> &[HartTarget] {
        &self.contexts
    }

    fn src(&self, src: u32) -> Result<usize, IntcError> {
        if src == 0 || src > self.ndev {
            return Err(IntcError::NoSuchSource(src));
        }
        Ok(src as usize)
    }

    /// Makes source `src`'s gateway edge-triggered (rising edge) or
    /// level-triggered. Only the two-cell PLIC bindings have edge gateways;
    /// [`RiscvIntc`] checks that.
    pub fn set_edge(&mut self, src: u32, edge: bool) -> Result<(), IntcError> {
        let s = self.src(src)?;
        self.edge[s] = edge;
        self.held[s] = false;
        Ok(())
    }

    pub fn is_edge(&self, src: u32) -> bool {
        self.src(src).is_ok_and(|s| self.edge[s])
    }

    /// The device drives source `src`'s line.
    pub fn set_level(&mut self, src: u32, high: bool) -> Result<(), IntcError> {
        let s = self.src(src)?;
        let rising = high && !self.level[s];
        self.level[s] = high;
        if self.edge[s] {
            if rising {
                if self.in_service[s] {
                    self.held[s] = true;
                } else {
                    self.pending[s] = true;
                }
            }
        } else if high && !self.in_service[s] {
            self.pending[s] = true;
        }
        Ok(())
    }

    pub fn is_pending(&self, src: u32) -> bool {
        self.src(src).is_ok_and(|s| self.pending[s])
    }

    /// The highest-priority pending source enabled for `ctx` (ties to the
    /// lowest id) and its priority; priority 0 never qualifies.
    fn best(&self, ctx: usize) -> (u32, u32) {
        let mut best = (0u32, 0u32);
        for s in 1..=self.ndev as usize {
            let p = self.priority[s];
            if self.pending[s] && self.enable[ctx][s] && p > best.1 {
                best = (s as u32, p);
            }
        }
        best
    }

    /// The context's interrupt output (EIP).
    pub fn eip(&self, ctx: usize) -> bool {
        ctx < self.contexts.len() && {
            let (src, p) = self.best(ctx);
            src != 0 && p > self.threshold[ctx]
        }
    }

    /// Claim: the best eligible source (0 if none), now in service.
    pub fn claim(&mut self, ctx: usize) -> u32 {
        if ctx >= self.contexts.len() {
            return 0;
        }
        // The threshold gates notification only; a claim is not affected by
        // it (PLIC specification, "Interrupt Claim Process"), so a driver
        // that masks notifications with the threshold can still poll.
        let (s, _) = self.best(ctx);
        if s != 0 {
            self.pending[s as usize] = false;
            self.in_service[s as usize] = true;
        }
        s
    }

    /// Completion. Ignored unless `src` is enabled for `ctx` (as the
    /// specification says) and in service.
    pub fn complete(&mut self, ctx: usize, src: u32) {
        let Ok(s) = self.src(src) else { return };
        if ctx >= self.contexts.len() || !self.enable[ctx][s] || !self.in_service[s] {
            return;
        }
        self.in_service[s] = false;
        if self.edge[s] {
            if self.held[s] {
                self.held[s] = false;
                self.pending[s] = true;
            }
        } else if self.level[s] {
            self.pending[s] = true;
        }
    }

    fn words(&self) -> u64 {
        u64::from(self.ndev) / 32 + 1
    }

    fn decode(&self, addr: u64) -> Result<PlicReg, IntcError> {
        let off = addr.wrapping_sub(self.base);
        if !addr.is_multiple_of(4) {
            return Err(IntcError::Misaligned { addr });
        }
        let nctx = self.contexts.len() as u64;
        let unmapped = Err(IntcError::Unmapped { addr });
        if off < PLIC_PENDING {
            let s = off / 4;
            return if s <= u64::from(self.ndev) {
                Ok(PlicReg::Priority(s as usize))
            } else {
                unmapped
            };
        }
        if off < PLIC_ENABLE {
            let w = (off - PLIC_PENDING) / 4;
            return if w < self.words() {
                Ok(PlicReg::Pending(w))
            } else {
                unmapped
            };
        }
        if off < PLIC_CONTEXT {
            let rel = off - PLIC_ENABLE;
            let (ctx, w) = (rel / PLIC_ENABLE_STRIDE, (rel % PLIC_ENABLE_STRIDE) / 4);
            return if ctx < nctx && w < self.words() {
                Ok(PlicReg::Enable(ctx as usize, w))
            } else {
                unmapped
            };
        }
        let rel = off - PLIC_CONTEXT;
        let (ctx, r) = (rel / PLIC_CONTEXT_STRIDE, rel % PLIC_CONTEXT_STRIDE);
        match (ctx < nctx, r) {
            (true, 0) => Ok(PlicReg::Threshold(ctx as usize)),
            (true, 4) => Ok(PlicReg::Claim(ctx as usize)),
            _ => unmapped,
        }
    }

    fn bits(&self, v: &[bool], w: u64) -> u32 {
        (0..32)
            .filter(|b| v.get((w * 32 + b) as usize).copied().unwrap_or(false))
            .fold(0, |acc, b| acc | (1 << b))
    }

    /// A 32-bit MMIO read. A read of claim/complete claims.
    pub fn read32(&mut self, addr: u64) -> Result<u32, IntcError> {
        Ok(match self.decode(addr)? {
            PlicReg::Priority(s) => self.priority[s],
            PlicReg::Pending(w) => self.bits(&self.pending, w),
            PlicReg::Enable(c, w) => self.bits(&self.enable[c], w),
            PlicReg::Threshold(c) => self.threshold[c],
            PlicReg::Claim(c) => self.claim(c),
        })
    }

    /// A 32-bit MMIO write. Priority and threshold keep their 3 implemented
    /// bits; source 0's priority and enable bit are hard-wired to 0.
    pub fn write32(&mut self, addr: u64, v: u32) -> Result<(), IntcError> {
        match self.decode(addr)? {
            PlicReg::Priority(0) => {}
            PlicReg::Priority(s) => self.priority[s] = v & PLIC_MAX_PRIORITY,
            PlicReg::Pending(_) => return Err(IntcError::ReadOnly { addr }),
            PlicReg::Enable(c, w) => {
                for b in 0..32u64 {
                    let s = (w * 32 + b) as usize;
                    if s != 0 && s <= self.ndev as usize {
                        self.enable[c][s] = v & (1 << b) != 0;
                    }
                }
            }
            PlicReg::Threshold(c) => self.threshold[c] = v & PLIC_MAX_PRIORITY,
            PlicReg::Claim(c) => self.complete(c, v),
        }
        Ok(())
    }

    #[cfg_attr(vesper_port, allow(dead_code))]
    fn contains(&self, addr: u64) -> bool {
        let end = self.base + PLIC_CONTEXT + PLIC_CONTEXT_STRIDE * self.contexts.len() as u64;
        (self.base..end).contains(&addr)
    }
}

enum PlicReg {
    Priority(usize),
    Pending(u64),
    Enable(usize, u64),
    Threshold(usize),
    Claim(usize),
}

/// One delivered interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub hart: u32,
    pub cause: u32,
    /// The PLIC or APLIC source for external interrupts (the IMSIC identity
    /// for an MSI that no APLIC source sent), else the cause.
    pub source: u32,
}

/// Which controller produced an external interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ctl {
    /// Index into [`Platform::plics`].
    Plic(usize),
    /// Index into [`Platform::aplics`] (direct delivery through an IDC).
    Aplic(usize),
    /// IMSIC index into [`Platform::imsics`] and file (hart) index.
    Imsic(usize, usize),
}

/// One external-interrupt delivery, with the controller and number it was
/// claimed from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalDelivery {
    pub hart: u32,
    pub cause: u32,
    /// Devicetree path of the PLIC, APLIC domain or IMSIC.
    pub controller: String,
    /// The PLIC/APLIC source, or the IMSIC identity.
    pub number: u32,
    /// For an IMSIC identity that an APLIC source forwards: that source.
    pub source: Option<u32>,
}

/// Counters of deliveries that did nothing useful.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IntcStats {
    /// A delivery found the level already low (or a claim returned 0).
    pub spurious: u64,
    /// The level stayed high for [`STORM_BUDGET`] handler runs.
    pub storms: u64,
    /// A claimed source had no ISR (it was completed unhandled).
    pub unhandled: u64,
}

/// One VS-level external interrupt claimed through `vstopei`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestDelivery {
    pub hart: u32,
    /// The guest interrupt file (`hstatus.VGEIN` during the claim).
    pub guest: u32,
    pub id: u32,
}

/// One PCI MSI write and where it went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PciMsi {
    /// Requester ID (`bus << 8 | device << 3 | function`).
    pub rid: u32,
    /// The ID the MSI controller sees (`msi-map` output).
    pub device_id: u32,
    pub controller: String,
    /// The bus address the function wrote and the CPU address it reached.
    pub bus_addr: u64,
    pub cpu_addr: u64,
    pub data: u32,
    /// The IMSIC, hart and guest file (0 = the hart's own) it landed in
    /// (`None` for an APLIC `setipnum_le`).
    pub imsic: Option<String>,
    pub hart: Option<u32>,
    pub guest: Option<u32>,
    /// Whether it landed in a file of `controller`. Without an IOMMU a
    /// function can reach any interrupt file; `false` flags a write that
    /// bypassed its routed controller.
    pub in_parent: bool,
}

/// Software slots of a CLINT/ACLINT block: MSIP or SSIP bits and MTIMECMP
/// registers, one per index.
fn clint_slots(info: &ClintInfo) -> usize {
    match info.kind {
        ClintKind::Clint => info.targets.len().div_ceil(2),
        _ => info.targets.len(),
    }
}

/// Software-interrupt target of index `k`.
fn sw_target(info: &ClintInfo, k: usize) -> Option<HartTarget> {
    match info.kind {
        ClintKind::Clint => info.targets.get(2 * k).copied(),
        ClintKind::AclintMswi | ClintKind::AclintSswi => info.targets.get(k).copied(),
        ClintKind::AclintMtimer => None,
    }
}

fn timer_target(info: &ClintInfo, k: usize) -> Option<HartTarget> {
    match info.kind {
        ClintKind::Clint => info.targets.get(2 * k + 1).copied(),
        ClintKind::AclintMtimer => info.targets.get(k).copied(),
        _ => None,
    }
}

/// Physical address of the MTIME register a block's MTIMECMPs compare
/// against. Blocks naming the same address share one counter.
fn mtime_addr_of(info: &ClintInfo) -> Option<u64> {
    match info.kind {
        ClintKind::Clint => Some(info.regs[0].base + CLINT_MTIME),
        ClintKind::AclintMtimer => Some(info.regs[0].base),
        _ => None,
    }
}

#[cfg(not(vesper_port))]
#[path = "intc/rust.rs"]
mod imp;
#[cfg(vesper_port)]
#[path = "intc/port.rs"]
mod imp;

/// The fabric: the controller state ([`imp::Core`]) and what the dispatch
/// loops and routing keep beside it.
#[derive(Debug)]
struct State {
    core: imp::Core,
    aplic_info: Vec<AplicInfo>,
    imsic_info: Vec<ImsicInfo>,
    clint_info: Vec<ClintInfo>,
    /// Allocated identities per IMSIC and file.
    used: Vec<Vec<BTreeSet<u32>>>,
    /// Allocated identities per IMSIC and guest file `(file, guest)`.
    guest_used: Vec<BTreeMap<(usize, usize), BTreeSet<u32>>>,
    /// `(hart, cause)` raised on the Cpu and not yet finished.
    asserted: BTreeSet<(u32, u32)>,
    /// Causes with an installed handler (`mie`), per hart.
    mie: BTreeSet<(u32, u32)>,
    /// IMSIC identities that an APLIC source forwards: `(imsic, file, id)`
    /// to `(domain, source)`.
    msi_sources: BTreeMap<(usize, usize, u32), (usize, u32)>,
    log: Vec<Delivery>,
    ext_log: Vec<ExternalDelivery>,
    stats: IntcStats,
    guest_log: Vec<GuestDelivery>,
    pci_log: Vec<PciMsi>,
}

impl State {
    fn complete_external(&mut self, hart: u32, cause: u32, ctl: Ctl, number: u32) {
        let forwarded = match ctl {
            Ctl::Imsic(b, k) => self.msi_sources.get(&(b, k, number)).copied(),
            _ => None,
        };
        self.core
            .complete_external(hart, cause, ctl, number, forwarded);
    }

    fn controller_path(&self, ctl: Ctl) -> String {
        match ctl {
            Ctl::Plic(_) => String::new(),
            Ctl::Aplic(d) => self.aplic_info[d].path.clone(),
            Ctl::Imsic(b, _) => self.imsic_info[b].path.clone(),
        }
    }

    /// The IMSIC and file of `hart` at `level`.
    fn file_of(&self, hart: u32, level: Privilege) -> Result<(usize, usize), IntcError> {
        let cause = level.external();
        self.core
            .imsic_file(hart, cause)
            .ok_or(IntcError::NoImsic { hart, cause })
    }

    /// Enables identity `id` of file `(b, k, g)` and the file's MSI
    /// delivery.
    fn enable_identity(&mut self, b: usize, k: usize, g: usize, id: u32) -> Result<(), IntcError> {
        self.core
            .file_csr_write(b, k, g, crate::aia::ISEL_EIDELIVERY, 1)?;
        let isel = crate::aia::ISEL_EIE0 + 2 * (id / 64);
        let old = self.core.file_csr_read(b, k, g, isel)?;
        self.core
            .file_csr_write(b, k, g, isel, old | 1 << (id % 64))
    }
}

/// What an interrupt handler receives.
pub struct IrqEvent<'a> {
    pub ctx: &'a IsrCtx,
    pub hart: u32,
    pub cause: u32,
    /// The claimed PLIC or APLIC source for external interrupts (for an MSI
    /// that no APLIC source sent, the IMSIC identity), else the cause.
    pub source: u32,
    pub intc: &'a IntcHandle,
}

type Handler = Box<dyn FnMut(&IrqEvent<'_>)>;
type GuestIsrs = Rc<RefCell<BTreeMap<(u32, u32, u32), Handler>>>;

/// Shared access to the controller registers from inside handlers (an ISR
/// acknowledging its device, a timer handler moving MTIMECMP). Changes made
/// through it are seen by the Cpus at the next [`RiscvIntc`] operation.
#[derive(Clone, Debug)]
pub struct IntcHandle {
    state: Rc<RefCell<State>>,
}

impl IntcHandle {
    pub fn read32(&self, addr: u64) -> Result<u32, IntcError> {
        self.state.borrow_mut().core.read32(addr)
    }

    pub fn write32(&self, addr: u64, v: u32) -> Result<(), IntcError> {
        self.state.borrow_mut().core.write32(addr, v)
    }

    /// A 64-bit access as two 32-bit ones, low word first.
    pub fn write64(&self, addr: u64, v: u64) -> Result<(), IntcError> {
        self.write32(addr, v as u32)?;
        self.write32(addr + 4, (v >> 32) as u32)
    }

    pub fn read64(&self, addr: u64) -> Result<u64, IntcError> {
        let lo = self.read32(addr)?;
        let hi = self.read32(addr + 4)?;
        Ok(u64::from(lo) | (u64::from(hi) << 32))
    }

    /// The device drives source `src` of the platform's first PLIC (or,
    /// without a PLIC, of its first APLIC domain tree).
    pub fn set_line(&self, src: u32, high: bool) -> Result<(), IntcError> {
        let mut st = self.state.borrow_mut();
        let ctl = if st.core.plic_count() != 0 {
            Ctl::Plic(0)
        } else if st.core.aplic_count() != 0 {
            Ctl::Aplic(0)
        } else {
            return Err(IntcError::NoPlic);
        };
        st.core.set_wire(ctl, src, high)
    }

    /// The device drives source `src` of controller `ctl`.
    pub fn set_controller_line(&self, ctl: Ctl, src: u32, high: bool) -> Result<(), IntcError> {
        let mut st = self.state.borrow_mut();
        match ctl {
            Ctl::Plic(p) if p >= st.core.plic_count() => return Err(IntcError::NoPlic),
            Ctl::Aplic(d) if d >= st.core.aplic_count() => return Err(IntcError::NoPlic),
            _ => {}
        }
        st.core.set_wire(ctl, src, high)
    }

    /// A bus master writes `data` to `addr` (an MSI to an IMSIC file or an
    /// APLIC's `setipnum_le`).
    pub fn msi_write(&self, addr: u64, data: u32) -> Result<(), IntcError> {
        let mut st = self.state.borrow_mut();
        if !addr.is_multiple_of(4) {
            return Err(IntcError::Misaligned { addr });
        }
        st.core.deliver_msi(addr, data)?;
        st.core.settle();
        Ok(())
    }

    /// The MTIME that `hart`'s timer compares against (0 if it has none).
    pub fn mtime(&self, hart: u32) -> u64 {
        self.state.borrow().core.hart_mtime(hart)
    }

    /// Clears a pending SSIP (software writing `sip.SSIP = 0`).
    pub fn clear_ssip(&self, hart: u32) {
        self.state.borrow_mut().core.clear_ssip(hart);
    }

    /// `*ireg` read of `hart`'s IMSIC file at `level` (`miselect`/`mireg`
    /// for M, `siselect`/`sireg` for S).
    pub fn imsic_csr_read(&self, hart: u32, level: Privilege, isel: u32) -> Result<u64, IntcError> {
        let st = self.state.borrow();
        let (b, k) = st.file_of(hart, level)?;
        st.core.file_csr_read(b, k, 0, isel)
    }

    /// `*ireg` write.
    pub fn imsic_csr_write(
        &self,
        hart: u32,
        level: Privilege,
        isel: u32,
        v: u64,
    ) -> Result<(), IntcError> {
        let mut st = self.state.borrow_mut();
        let (b, k) = st.file_of(hart, level)?;
        st.core.file_csr_write(b, k, 0, isel, v)
    }

    /// `*topei` read.
    pub fn topei(&self, hart: u32, level: Privilege) -> Result<u32, IntcError> {
        let st = self.state.borrow();
        let (b, k) = st.file_of(hart, level)?;
        Ok(st.core.file_topei(b, k, 0))
    }

    /// `csrrw rd, *topei, x0`: claims the top identity.
    pub fn claim_topei(&self, hart: u32, level: Privilege) -> Result<u32, IntcError> {
        let mut st = self.state.borrow_mut();
        let (b, k) = st.file_of(hart, level)?;
        Ok(st.core.file_claim(b, k, 0))
    }

    /// GEILEN of `hart`: the number of guest interrupt files after its
    /// S-level IMSIC file (`2^riscv,guest-index-bits - 1`, at most 63), or 0.
    pub fn geilen(&self, hart: u32) -> u32 {
        self.state.borrow().core.geilen(hart)
    }

    /// `hstatus.VGEIN` (a 6-bit field, stored as written; accesses check
    /// that it names an implemented guest file).
    pub fn vgein(&self, hart: u32) -> u32 {
        self.state.borrow().core.vgein(hart)
    }

    /// A write of `hstatus.VGEIN`.
    pub fn set_vgein(&self, hart: u32, vgein: u32) {
        self.state.borrow_mut().core.set_vgein(hart, vgein & 0x3f);
    }

    /// `hgeie`: bits `GEILEN:1` are writable, bit 0 and the rest read zero.
    pub fn hgeie(&self, hart: u32) -> u64 {
        self.state.borrow().core.hgeie(hart)
    }

    pub fn set_hgeie(&self, hart: u32, v: u64) {
        let mut st = self.state.borrow_mut();
        let n = st.core.geilen(hart);
        let mask = ((1u64 << n) - 1) << 1;
        st.core.set_hgeie(hart, v & mask);
    }

    /// `hgeip` (read-only): bit `g` is set while guest file `g` signals an
    /// interrupt (`eidelivery` = 1 and a pending, enabled identity below
    /// its threshold).
    pub fn hgeip(&self, hart: u32) -> u64 {
        self.state.borrow().core.hgeip(hart)
    }

    /// The H-extension bits of `mip`: SGEIP (`hgeip & hgeie != 0`) and
    /// VSEIP (`hgeip` bit VGEIN). `hvip.VSEIP` is not modelled.
    pub fn hyp_pending(&self, hart: u32) -> u64 {
        let st = self.state.borrow();
        (u64::from(st.core.level(hart, SGEIP)) << SGEIP)
            | (u64::from(st.core.level(hart, VSEIP)) << VSEIP)
    }

    /// `vsiselect = isel; vsireg` read: the guest file VGEIN selects.
    pub fn vs_imsic_csr_read(&self, hart: u32, isel: u32) -> Result<u64, IntcError> {
        self.state.borrow_mut().core.vs_csr_read(hart, isel)
    }

    /// `vsiselect = isel; vsireg = v`.
    pub fn vs_imsic_csr_write(&self, hart: u32, isel: u32, v: u64) -> Result<(), IntcError> {
        self.state.borrow_mut().core.vs_csr_write(hart, isel, v)
    }

    /// `vstopei` read (from M or HS mode).
    pub fn vstopei(&self, hart: u32) -> Result<u32, IntcError> {
        self.state.borrow_mut().core.vstopei(hart)
    }

    /// `csrrw rd, vstopei, x0`: the VS-level claim.
    pub fn claim_vstopei(&self, hart: u32) -> Result<u32, IntcError> {
        self.state.borrow_mut().core.claim_vstopei(hart)
    }

    /// Whether identity `id` is pending in guest file `guest` (1 and up)
    /// of `hart`'s S-level IMSIC.
    pub fn guest_eip(&self, hart: u32, guest: u32, id: u32) -> Result<bool, IntcError> {
        let st = self.state.borrow();
        let none = IntcError::NoImsic { hart, cause: SEIP };
        let (b, k) = st.core.imsic_file(hart, SEIP).ok_or(none.clone())?;
        let g = guest as usize;
        if guest == 0 || g >= st.core.files_len(b, k) {
            return Err(none);
        }
        Ok(st.core.file_is_pending(b, k, g, id))
    }
}

/// The interrupt fabric of a RISC-V devicetree platform.
pub struct RiscvIntc {
    privilege: Privilege,
    harts: Vec<(u32, Cpu)>,
    handle: IntcHandle,
    isrs: Rc<RefCell<BTreeMap<(Ctl, u32), Handler>>>,
    /// VS-level handlers by `(hart, guest, identity)`.
    guest_isrs: GuestIsrs,
    plic_paths: Vec<String>,
}

impl std::fmt::Debug for RiscvIntc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RiscvIntc")
            .field("privilege", &self.privilege)
            .field("harts", &self.harts)
            .finish()
    }
}

/// The APLIC MSI address configuration that points domains at `m` (and
/// `s`) IMSIC files, as firmware derives it (the fields Linux's
/// `aplic_msi_setup` reads back), with a check that every file address is
/// expressible.
pub fn msi_addr_cfg(m: &ImsicInfo, s: Option<&ImsicInfo>) -> Result<MsiAddrCfg, IntcError> {
    let layout = |imsic: &ImsicInfo, detail: String| IntcError::ImsicLayout {
        imsic: imsic.path.clone(),
        detail,
    };
    let hhxs = m.group_index_shift - 24;
    if hhxs > 31 || m.hart_index_bits > 15 || m.group_index_bits > 7 {
        return Err(layout(m, "index fields too wide".into()));
    }
    let mask = |w: u32| (1u64 << w) - 1;
    let base_ppn = |i: &ImsicInfo| {
        let ppn = i.files[0] >> 12;
        ppn & !(mask(i.hart_index_bits) << i.guest_index_bits)
            & !(mask(m.group_index_bits) << (hhxs + 12))
    };
    let mut cfg = MsiAddrCfg {
        m_ppn: base_ppn(m),
        s_ppn: 0,
        hhxs,
        lhxs_m: m.guest_index_bits,
        lhxs_s: 0,
        hhxw: m.group_index_bits,
        lhxw: m.hart_index_bits,
        locked: false,
    };
    if let Some(s) = s {
        cfg.s_ppn = base_ppn(s);
        cfg.lhxs_s = s.guest_index_bits;
    }
    let harts = 1u32 << (m.hart_index_bits + m.group_index_bits);
    for (imsic, sup) in [(Some(m), false), (s, true)] {
        let Some(i) = imsic else { continue };
        for &f in &i.files {
            if cfg.hart_of(sup, f, harts).is_none() {
                return Err(layout(i, format!("file {f:#x} has no hart index")));
            }
        }
    }
    Ok(cfg)
}

impl RiscvIntc {
    /// Builds the controllers and one [`Cpu`] per enabled hart from
    /// `platform`. A platform may have no PLIC (only local interrupts, or
    /// AIA).
    pub fn new(
        platform: &Platform,
        privilege: Privilege,
        ledger: &Ledger,
    ) -> Result<Self, IntcError> {
        if platform.arch != Arch::Riscv64 {
            return Err(IntcError::NotRiscv(platform.arch));
        }
        let mut state = State {
            core: imp::Core::new(platform)?,
            aplic_info: platform.aplics.clone(),
            imsic_info: platform.imsics.clone(),
            clint_info: platform.clints.clone(),
            used: platform
                .imsics
                .iter()
                .map(|m| vec![BTreeSet::new(); m.files.len()])
                .collect(),
            guest_used: vec![BTreeMap::new(); platform.imsics.len()],
            asserted: BTreeSet::new(),
            mie: BTreeSet::new(),
            msi_sources: BTreeMap::new(),
            log: Vec::new(),
            ext_log: Vec::new(),
            stats: IntcStats::default(),
            guest_log: Vec::new(),
            pci_log: Vec::new(),
        };
        Self::firmware(&mut state, platform, privilege)?;
        let harts = platform
            .harts
            .iter()
            .map(|h| (h.id, Cpu::new(Arch::Riscv64, ledger)))
            .collect();
        Ok(Self {
            privilege,
            harts,
            handle: IntcHandle {
                state: Rc::new(RefCell::new(state)),
            },
            isrs: Rc::new(RefCell::new(BTreeMap::new())),
            guest_isrs: Rc::new(RefCell::new(BTreeMap::new())),
            plic_paths: platform.plics.iter().map(|p| p.path.clone()).collect(),
        })
    }

    /// What M-mode firmware does before S-mode software runs: program each
    /// root domain's MSI address configuration from the IMSIC nodes, lock
    /// it, and apply the devicetree's delegations. M-mode software does this
    /// itself, so for [`Privilege::Machine`] only the configuration is
    /// programmed (unlocked) and nothing is delegated.
    fn firmware(
        st: &mut State,
        platform: &Platform,
        privilege: Privilege,
    ) -> Result<(), IntcError> {
        for d in 0..st.core.aplic_count() {
            let info = st.aplic_info[d].clone();
            if info.parent.is_some() {
                continue;
            }
            let imsic = |path: &Option<String>| {
                path.as_ref()
                    .and_then(|p| platform.imsics.iter().find(|m| &m.path == p))
            };
            if let Some(m) = imsic(&info.msi_parent) {
                let child_s = info.children.iter().find_map(|c| {
                    platform
                        .aplics
                        .iter()
                        .find(|a| &a.path == c)
                        .and_then(|a| imsic(&a.msi_parent))
                });
                let mut cfg = msi_addr_cfg(m, child_s)?;
                cfg.locked = privilege == Privilege::Supervisor;
                let base = info.base;
                let high_m = (u32::from(cfg.locked) << 31)
                    | (cfg.hhxs << 24)
                    | (cfg.lhxs_m << 20)
                    | (cfg.hhxw << 16)
                    | (cfg.lhxw << 12)
                    | ((cfg.m_ppn >> 32) as u32 & 0xfff);
                let core = &mut st.core;
                core.aplic_write32(d, base + 0x1bc0, cfg.m_ppn as u32)?;
                core.aplic_write32(d, base + 0x1bc8, cfg.s_ppn as u32)?;
                core.aplic_write32(
                    d,
                    base + 0x1bcc,
                    (cfg.lhxs_s << 20) | ((cfg.s_ppn >> 32) as u32 & 0xfff),
                )?;
                core.aplic_write32(d, base + 0x1bc4, high_m)?;
            }
        }
        if privilege == Privilege::Supervisor {
            // Parents first, so a grandchild's delegation finds its sources
            // active in the child.
            let count = st.core.aplic_count();
            let mut order: Vec<usize> = (0..count).collect();
            let depth = |st: &State, mut d: usize| {
                let mut n = 0;
                while st.core.root_of(d) != d {
                    let p = st.aplic_info[d].parent.clone();
                    d = p
                        .and_then(|p| st.aplic_info.iter().position(|a| a.path == p))
                        .unwrap_or(d);
                    n += 1;
                    if n > count {
                        break;
                    }
                }
                n
            };
            order.sort_by_key(|&d| depth(st, d));
            for d in order {
                let info = st.aplic_info[d].clone();
                for del in &info.delegations {
                    let Some(ci) = info.children.iter().position(|c| *c == del.child) else {
                        continue;
                    };
                    let base = info.base;
                    for s in del.first..=del.last.min(st.core.aplic_sources(d)) {
                        st.core
                            .aplic_write32(d, base + 4 * u64::from(s), (1 << 10) | ci as u32)?;
                    }
                }
                st.core.settle();
            }
        }
        st.core.settle();
        Ok(())
    }

    pub fn handle(&self) -> IntcHandle {
        self.handle.clone()
    }

    pub fn privilege(&self) -> Privilege {
        self.privilege
    }

    pub fn hart_ids(&self) -> Vec<u32> {
        self.harts.iter().map(|h| h.0).collect()
    }

    /// The hart's Cpu, for `disable_local_save` / `restore_local`.
    /// Restoring delivers pending causes directly; call [`RiscvIntc::sync`]
    /// afterwards to raise levels that rose meanwhile.
    pub fn cpu(&mut self, hart: u32) -> Result<&mut Cpu, IntcError> {
        self.harts
            .iter_mut()
            .find(|h| h.0 == hart)
            .map(|h| &mut h.1)
            .ok_or(IntcError::NoSuchHart(hart))
    }

    /// The hart's context, for this privilege's external interrupt, on the
    /// first PLIC that has one.
    pub fn context_of(&self, hart: u32) -> Result<usize, IntcError> {
        self.plic_context(hart).map(|(_, c)| c)
    }

    /// The PLIC (index into [`Platform::plics`]) and context of the hart's
    /// external interrupt at this privilege, on the first PLIC that has one.
    pub fn plic_context(&self, hart: u32) -> Result<(usize, usize), IntcError> {
        let st = self.handle.state.borrow();
        if st.core.plic_count() == 0 {
            return Err(IntcError::NoPlic);
        }
        let cause = self.privilege.external();
        st.core
            .ext_context(hart, cause)
            .ok_or(IntcError::NoContext { hart, cause })
    }

    pub fn deliveries(&self) -> Vec<Delivery> {
        self.handle.state.borrow().log.clone()
    }

    /// Every external delivery with the controller it was claimed from.
    pub fn external_deliveries(&self) -> Vec<ExternalDelivery> {
        self.handle.state.borrow().ext_log.clone()
    }

    pub fn stats(&self) -> IntcStats {
        self.handle.state.borrow().stats
    }

    /// MSIs written to an address no IMSIC file or APLIC `setipnum_le`
    /// occupies (lost on hardware too).
    pub fn lost_msis(&self) -> Vec<Msi> {
        self.handle.state.borrow().core.lost_msis()
    }

    /// Base address of the first PLIC's register window.
    pub fn plic_base(&self) -> Result<u64, IntcError> {
        let st = self.handle.state.borrow();
        if st.core.plic_count() == 0 {
            return Err(IntcError::NoPlic);
        }
        Ok(st.core.plic_base(0))
    }

    /// Base addresses of every PLIC, in [`Platform::plics`] order.
    pub fn plic_bases(&self) -> Vec<u64> {
        let st = self.handle.state.borrow();
        (0..st.core.plic_count())
            .map(|p| st.core.plic_base(p))
            .collect()
    }

    pub fn read32(&mut self, addr: u64) -> Result<u32, IntcError> {
        let v = self.handle.read32(addr);
        self.sync();
        v
    }

    pub fn write32(&mut self, addr: u64, v: u32) -> Result<(), IntcError> {
        let r = self.handle.write32(addr, v);
        self.sync();
        r
    }

    pub fn write64(&mut self, addr: u64, v: u64) -> Result<(), IntcError> {
        let r = self.handle.write64(addr, v);
        self.sync();
        r
    }

    pub fn read64(&mut self, addr: u64) -> Result<u64, IntcError> {
        self.handle.read64(addr)
    }

    /// A device drives source `src` of the first PLIC (or, without one, of
    /// the first APLIC domain tree).
    pub fn set_line(&mut self, src: u32, high: bool) -> Result<(), IntcError> {
        let r = self.handle.set_line(src, high);
        self.sync();
        r
    }

    /// A device drives source `src` of controller `ctl`.
    pub fn set_controller_line(&mut self, ctl: Ctl, src: u32, high: bool) -> Result<(), IntcError> {
        let r = self.handle.set_controller_line(ctl, src, high);
        self.sync();
        r
    }

    /// A bus master writes `data` to `addr` (a device MSI).
    pub fn msi_write(&mut self, addr: u64, data: u32) -> Result<(), IntcError> {
        let r = self.handle.msi_write(addr, data);
        self.sync();
        r
    }

    /// Drives the wired line of `device` (its first PLIC or APLIC
    /// interrupt) and returns the source number.
    pub fn set_device_line(
        &mut self,
        platform: &Platform,
        device: &str,
        high: bool,
    ) -> Result<u32, IntcError> {
        let (ctl, line) = self.device_line(platform, device)?;
        self.set_controller_line(ctl, line.number, high)?;
        Ok(line.number)
    }

    /// Advances every MTIME counter by `ticks` (wrapping, as the counters
    /// do).
    pub fn advance_mtime(&mut self, ticks: u64) {
        self.handle.state.borrow_mut().core.advance_mtime(ticks);
        self.sync();
    }

    /// The first line of `device` that goes to a PLIC or an APLIC domain.
    fn device_line(&self, platform: &Platform, device: &str) -> Result<(Ctl, IrqLine), IntcError> {
        let id = platform
            .tree()
            .find(device)
            .ok_or_else(|| DtbError::NoSuchNode(device.to_string()))?;
        let path = platform.tree().path(id);
        let st = self.handle.state.borrow();
        for l in platform.irq_lines.iter().filter(|l| l.device == path) {
            if let Some(p) = self.plic_paths.iter().position(|c| *c == l.controller) {
                return Ok((Ctl::Plic(p), l.clone()));
            }
            if let Some(d) = st.aplic_info.iter().position(|a| a.path == l.controller) {
                return Ok((Ctl::Aplic(d), l.clone()));
            }
        }
        Err(IntcError::NotOnPlic { device: path })
    }

    /// Installs `handler` for `device`'s first wired interrupt and routes it
    /// to `hart` through the controller registers, returning the source
    /// number. `priority` is 1 (lowest) to 7 (highest):
    ///
    /// * PLIC: the source gets `priority` and is enabled in `hart`'s
    ///   context; a rising-edge source (two-cell bindings only) gets an edge
    ///   gateway.
    /// * APLIC (the domain this privilege drives: the root for M-mode, the
    ///   domain the device names for S-mode): the domain is enabled,
    ///   `sourcecfg` gets the devicetree trigger, and the source is enabled
    ///   and targeted at `hart`: in direct mode at its IDC with IPRIO `8 -
    ///   priority` (lower is more urgent), in MSI mode at an identity of
    ///   `hart`'s IMSIC file allocated from the band of `priority` (higher
    ///   priority, lower identity), which is enabled there.
    ///
    /// A source has one handler: installing it again replaces the handler
    /// (on a PLIC it also enables the new hart's context).
    pub fn install_isr(
        &mut self,
        platform: &Platform,
        device: &str,
        hart: u32,
        priority: u32,
        handler: impl FnMut(&IrqEvent<'_>) + 'static,
    ) -> Result<u32, IntcError> {
        if priority > PLIC_MAX_PRIORITY {
            return Err(IntcError::PriorityTooHigh(priority));
        }
        let (ctl, line) = self.device_line(platform, device)?;
        let trigger = platform.trigger(&line)?;
        let src = line.number;
        let cause = self.privilege.external();
        match ctl {
            Ctl::Plic(p) => {
                let edge_capable = platform.controllers.iter().any(|c| {
                    c.path == line.controller && matches!(c.kind, ControllerKind::Plic { .. })
                }) && line.specifier.len() == 2;
                let edge = match trigger {
                    Trigger::LevelHigh => false,
                    Trigger::EdgeRising if edge_capable => true,
                    t => {
                        return Err(IntcError::UnsupportedTrigger {
                            device: line.device.clone(),
                            trigger: t,
                        });
                    }
                };
                let (base, ctx) = {
                    let st = self.handle.state.borrow();
                    let want = HartTarget {
                        hart,
                        cause: Some(cause),
                    };
                    let ctx = st
                        .core
                        .plic_context(p, want)
                        .ok_or(IntcError::NoContext { hart, cause })?;
                    (st.core.plic_base(p), ctx)
                };
                self.select_wired_delivery(hart, cause)?;
                self.handle
                    .state
                    .borrow_mut()
                    .core
                    .plic_set_edge(p, src, edge)?;
                self.ensure_external(hart)?;
                self.isrs.borrow_mut().insert((ctl, src), Box::new(handler));
                self.handle.write32(base + 4 * u64::from(src), priority)?;
                let word =
                    base + PLIC_ENABLE + PLIC_ENABLE_STRIDE * ctx as u64 + 4 * u64::from(src / 32);
                let old = self.handle.read32(word)?;
                self.handle.write32(word, old | (1 << (src % 32)))?;
            }
            Ctl::Aplic(named) => {
                let mode = match trigger {
                    Trigger::EdgeRising => SourceMode::EdgeRising,
                    Trigger::EdgeFalling => SourceMode::EdgeFalling,
                    Trigger::LevelHigh => SourceMode::LevelHigh,
                    Trigger::LevelLow => SourceMode::LevelLow,
                    Trigger::Unspecified => {
                        return Err(IntcError::UnsupportedTrigger {
                            device: line.device.clone(),
                            trigger,
                        });
                    }
                };
                let d = {
                    let st = self.handle.state.borrow();
                    match self.privilege {
                        Privilege::Machine => st.core.root_of(named),
                        Privilege::Supervisor => named,
                    }
                };
                self.route_aplic(d, src, mode, hart, priority, handler)?;
            }
            Ctl::Imsic(..) => unreachable!("device_line returns wired controllers"),
        }
        self.sync();
        Ok(src)
    }

    fn route_aplic(
        &mut self,
        d: usize,
        src: u32,
        mode: SourceMode,
        hart: u32,
        priority: u32,
        handler: impl FnMut(&IrqEvent<'_>) + 'static,
    ) -> Result<(), IntcError> {
        let cause = self.privilege.external();
        let want = HartTarget {
            hart,
            cause: Some(cause),
        };
        let (base, info, active) = {
            let st = self.handle.state.borrow();
            (
                st.core.aplic_base(d),
                st.aplic_info[d].clone(),
                st.core.aplic_is_delegated_here(d, src),
            )
        };
        // A non-root domain must have the source delegated to it.
        if !active {
            return Err(IntcError::NotDelegated {
                domain: info.path.clone(),
                source: src,
            });
        }
        if src == 0 || src > info.sources {
            return Err(IntcError::NoSuchSource(src));
        }
        let h = self.handle.clone();
        if let Some(imsic_path) = &info.msi_parent {
            // MSI delivery through the hart's IMSIC file.
            let (b, k, file_addr, cfg, sup) = {
                let st = h.state.borrow();
                let b = st
                    .imsic_info
                    .iter()
                    .position(|m| &m.path == imsic_path)
                    .ok_or(IntcError::NoImsic { hart, cause })?;
                let k = st.imsic_info[b]
                    .targets
                    .iter()
                    .position(|t| *t == want)
                    .ok_or(IntcError::NoImsic { hart, cause })?;
                (
                    b,
                    k,
                    st.imsic_info[b].files[k],
                    st.core.aplic_msi_cfg(d),
                    st.core.aplic_is_supervisor(d),
                )
            };
            let harts = 1u32 << (cfg.lhxw + cfg.hhxw);
            let hart_index =
                cfg.hart_of(sup, file_addr, harts)
                    .ok_or_else(|| IntcError::ImsicLayout {
                        imsic: imsic_path.clone(),
                        detail: format!("file {file_addr:#x} is not reachable from {}", info.path),
                    })?;
            // Replacing a route retires the identity the source had, so
            // reinstalling never leaks identities.
            self.retire_msi_route(d, src)?;
            let id = self.allocate_identity(b, k, hart, priority)?;
            h.write32(base, APLIC_DOMAINCFG_IE | APLIC_DOMAINCFG_DM)?;
            h.write32(base + 4 * u64::from(src), mode.bits())?;
            h.write32(base + 0x3000 + 4 * u64::from(src), (hart_index << 18) | id)?;
            {
                let mut st = h.state.borrow_mut();
                st.enable_identity(b, k, 0, id)?;
                st.msi_sources.insert((b, k, id), (d, src));
            }
            self.ensure_external(hart)?;
            self.isrs
                .borrow_mut()
                .insert((Ctl::Imsic(b, k), id), Box::new(handler));
            h.write32(base + 0x1edc, src)?;
        } else {
            let idc = info
                .idcs
                .iter()
                .position(|t| *t == want)
                .ok_or(IntcError::NoContext { hart, cause })?;
            self.select_wired_delivery(hart, cause)?;
            h.write32(base, APLIC_DOMAINCFG_IE)?;
            h.write32(base + 4 * u64::from(src), mode.bits())?;
            let iprio = 8 - priority.max(1);
            h.write32(
                base + 0x3000 + 4 * u64::from(src),
                ((idc as u32) << 18) | iprio,
            )?;
            h.write32(base + crate::aia::APLIC_IDC + 32 * idc as u64, 1)?;
            self.ensure_external(hart)?;
            self.isrs
                .borrow_mut()
                .insert((Ctl::Aplic(d), src), Box::new(handler));
            h.write32(base + 0x1edc, src)?;
        }
        Ok(())
    }

    /// Before a direct APLIC IDC or PLIC context serves `(hart, cause)`:
    /// if the hart has an IMSIC file at that level, select
    /// `eidelivery` = 0x40000000, unless the file is delivering its own
    /// MSIs.
    fn select_wired_delivery(&self, hart: u32, cause: u32) -> Result<(), IntcError> {
        let mut st = self.handle.state.borrow_mut();
        let Some((b, k)) = st.core.imsic_file(hart, cause) else {
            return Ok(());
        };
        if st.core.file_eidelivery(b, k) == crate::aia::EIDELIVERY_ON
            || !st.core.file_supports_external_delivery(b, k)
        {
            return Err(IntcError::ImsicOwnsLevel { hart, cause });
        }
        st.core.file_csr_write(
            b,
            k,
            0,
            crate::aia::ISEL_EIDELIVERY,
            u64::from(crate::aia::EIDELIVERY_EXTERNAL),
        )
    }

    /// Frees the IMSIC identity (and its handler, enable and pending bits)
    /// that APLIC domain `d` source `src` forwards to, if it has one.
    fn retire_msi_route(&self, d: usize, src: u32) -> Result<(), IntcError> {
        let old: Vec<(usize, usize, u32)> = {
            let st = self.handle.state.borrow();
            st.msi_sources
                .iter()
                .filter(|(_, v)| **v == (d, src))
                .map(|(key, _)| *key)
                .collect()
        };
        for (b, k, id) in old {
            {
                let mut st = self.handle.state.borrow_mut();
                st.msi_sources.remove(&(b, k, id));
                st.used[b][k].remove(&id);
                let bit = 1u64 << (id % 64);
                for isel in [
                    crate::aia::ISEL_EIE0 + 2 * (id / 64),
                    crate::aia::ISEL_EIP0 + 2 * (id / 64),
                ] {
                    let v = st.core.file_csr_read(b, k, 0, isel)?;
                    st.core.file_csr_write(b, k, 0, isel, v & !bit)?;
                }
            }
            self.isrs.borrow_mut().remove(&(Ctl::Imsic(b, k), id));
        }
        Ok(())
    }

    /// The lowest free identity of file `(b, k)` at or above the band of
    /// `priority` (7 gets the lowest band), else the lowest free one.
    fn allocate_identity(
        &self,
        b: usize,
        k: usize,
        hart: u32,
        priority: u32,
    ) -> Result<u32, IntcError> {
        let mut st = self.handle.state.borrow_mut();
        let ids = st.imsic_info[b].ids;
        let band = (ids + 1) / 8;
        let lo = 1 + (PLIC_MAX_PRIORITY - priority.clamp(1, PLIC_MAX_PRIORITY)) * band;
        let used = &mut st.used[b][k];
        let id = (lo..=ids)
            .chain(1..lo)
            .find(|i| !used.contains(i))
            .ok_or(IntcError::NoFreeIdentity { hart })?;
        used.insert(id);
        Ok(id)
    }

    /// The address of `hart`'s IMSIC file for this privilege (what a
    /// device's MSI address register is programmed with).
    pub fn msi_target(&self, hart: u32) -> Result<u64, IntcError> {
        let cause = self.privilege.external();
        let st = self.handle.state.borrow();
        let (b, k) = st
            .core
            .imsic_file(hart, cause)
            .ok_or(IntcError::NoImsic { hart, cause })?;
        Ok(st.imsic_info[b].files[k])
    }

    /// Installs `handler` for a device MSI to `hart`: allocates an identity
    /// in the hart's IMSIC file (band of `priority`, as for
    /// [`RiscvIntc::install_isr`]), enables it and file delivery, and
    /// returns the address/data pair the device must write.
    pub fn install_msi(
        &mut self,
        hart: u32,
        priority: u32,
        handler: impl FnMut(&IrqEvent<'_>) + 'static,
    ) -> Result<Msi, IntcError> {
        if priority > PLIC_MAX_PRIORITY {
            return Err(IntcError::PriorityTooHigh(priority));
        }
        self.cpu(hart)?;
        let cause = self.privilege.external();
        let (b, k, addr) = {
            let st = self.handle.state.borrow();
            let (b, k) = st
                .core
                .imsic_file(hart, cause)
                .ok_or(IntcError::NoImsic { hart, cause })?;
            (b, k, st.imsic_info[b].files[k])
        };
        let id = self.allocate_identity(b, k, hart, priority)?;
        self.handle
            .state
            .borrow_mut()
            .enable_identity(b, k, 0, id)?;
        self.ensure_external(hart)?;
        self.isrs
            .borrow_mut()
            .insert((Ctl::Imsic(b, k), id), Box::new(handler));
        self.sync();
        Ok(Msi { addr, data: id })
    }

    /// `*ireg` read of `hart`'s IMSIC file at `level`.
    pub fn imsic_csr_read(
        &mut self,
        hart: u32,
        level: Privilege,
        isel: u32,
    ) -> Result<u64, IntcError> {
        self.handle.imsic_csr_read(hart, level, isel)
    }

    /// `*ireg` write of `hart`'s IMSIC file at `level`.
    pub fn imsic_csr_write(
        &mut self,
        hart: u32,
        level: Privilege,
        isel: u32,
        v: u64,
    ) -> Result<(), IntcError> {
        let r = self.handle.imsic_csr_write(hart, level, isel, v);
        self.sync();
        r
    }

    /// `*topei` read.
    pub fn topei(&mut self, hart: u32, level: Privilege) -> Result<u32, IntcError> {
        self.handle.topei(hart, level)
    }

    /// `csrrw rd, *topei, x0`.
    pub fn claim_topei(&mut self, hart: u32, level: Privilege) -> Result<u32, IntcError> {
        let r = self.handle.claim_topei(hart, level);
        self.sync();
        r
    }

    /// The `mip`/`sip` external-interrupt level of `hart` for `cause`
    /// (MEIP or SEIP), as the CPU would see it.
    pub fn external_level(&self, hart: u32, cause: u32) -> bool {
        self.handle.state.borrow().core.level(hart, cause)
    }

    /// Registers the external-interrupt claim/complete loop for `hart`
    /// once.
    fn ensure_external(&mut self, hart: u32) -> Result<(), IntcError> {
        let cause = self.privilege.external();
        if !self.handle.state.borrow_mut().mie.insert((hart, cause)) {
            return Ok(());
        }
        let handle = self.handle.clone();
        let isrs = self.isrs.clone();
        self.cpu(hart)?.register_isr(cause, move |ictx| {
            let mut runs = 0usize;
            loop {
                let claimed = handle.state.borrow_mut().core.claim_external(hart, cause);
                let Some((ctl, number)) = claimed else {
                    if runs == 0 {
                        handle.state.borrow_mut().stats.spurious += 1;
                    }
                    break;
                };
                if number == 0 {
                    // An APLIC iforce delivery with nothing pending (claimi
                    // cleared iforce).
                    if runs == 0 {
                        handle.state.borrow_mut().stats.spurious += 1;
                    }
                    break;
                }
                if runs == STORM_BUDGET {
                    // Complete it so the gateway can forward it again, but
                    // stop delivering.
                    let mut st = handle.state.borrow_mut();
                    st.complete_external(hart, cause, ctl, number);
                    st.stats.storms += 1;
                    break;
                }
                runs += 1;
                let source = {
                    let mut st = handle.state.borrow_mut();
                    let wired = match ctl {
                        Ctl::Imsic(b, k) => st.msi_sources.get(&(b, k, number)).map(|x| x.1),
                        _ => None,
                    };
                    let controller = match ctl {
                        Ctl::Plic(p) => st.core.plic_base(p),
                        _ => 0,
                    };
                    let path = st.controller_path(ctl);
                    st.log.push(Delivery {
                        hart,
                        cause,
                        source: wired.unwrap_or(number),
                    });
                    st.ext_log.push(ExternalDelivery {
                        hart,
                        cause,
                        controller: if path.is_empty() {
                            format!("plic@{controller:x}")
                        } else {
                            path
                        },
                        number,
                        source: wired,
                    });
                    wired.unwrap_or(number)
                };
                // Take the ISR out so it may use the handle freely.
                let isr = isrs.borrow_mut().remove(&(ctl, number));
                match isr {
                    Some(mut f) => {
                        f(&IrqEvent {
                            ctx: ictx,
                            hart,
                            cause,
                            source,
                            intc: &handle,
                        });
                        isrs.borrow_mut().entry((ctl, number)).or_insert(f);
                    }
                    None => handle.state.borrow_mut().stats.unhandled += 1,
                }
                handle
                    .state
                    .borrow_mut()
                    .complete_external(hart, cause, ctl, number);
            }
            handle.state.borrow_mut().asserted.remove(&(hart, cause));
        });
        Ok(())
    }

    fn install_local(
        &mut self,
        hart: u32,
        cause: u32,
        mut handler: impl FnMut(&IrqEvent<'_>) + 'static,
    ) -> Result<(), IntcError> {
        let reachable = self.handle.state.borrow().clint_info.iter().any(|c| {
            c.targets.contains(&HartTarget {
                hart,
                cause: Some(cause),
            })
        });
        if !reachable {
            return Err(IntcError::NoLocalSource { hart, cause });
        }
        let handle = self.handle.clone();
        self.cpu(hart)?.register_isr(cause, move |ictx| {
            let mut runs = 0usize;
            while handle.state.borrow().core.level(hart, cause) {
                if runs == STORM_BUDGET {
                    handle.state.borrow_mut().stats.storms += 1;
                    break;
                }
                runs += 1;
                handle.state.borrow_mut().log.push(Delivery {
                    hart,
                    cause,
                    source: cause,
                });
                handler(&IrqEvent {
                    ctx: ictx,
                    hart,
                    cause,
                    source: cause,
                    intc: &handle,
                });
            }
            let mut st = handle.state.borrow_mut();
            if runs == 0 {
                st.stats.spurious += 1;
            }
            st.asserted.remove(&(hart, cause));
        });
        self.handle.state.borrow_mut().mie.insert((hart, cause));
        self.sync();
        Ok(())
    }

    /// Handler for this privilege's software interrupt on `hart` (MSIP from
    /// a CLINT/MSWI, or SSIP from an SSWI). The handler must clear the
    /// source (write 0 to MSIP, or [`IntcHandle::clear_ssip`]).
    pub fn on_software(
        &mut self,
        hart: u32,
        handler: impl FnMut(&IrqEvent<'_>) + 'static,
    ) -> Result<(), IntcError> {
        self.install_local(hart, self.privilege.software(), handler)
    }

    /// Machine timer handler for `hart`; it must move MTIMECMP past MTIME.
    pub fn on_timer(
        &mut self,
        hart: u32,
        handler: impl FnMut(&IrqEvent<'_>) + 'static,
    ) -> Result<(), IntcError> {
        if self.privilege != Privilege::Machine {
            return Err(IntcError::TimerNeedsMachineMode);
        }
        self.install_local(hart, MTIP, handler)
    }

    /// Address of `hart`'s MSIP / SETSSIP register (`cause` 3 or 1) or
    /// MTIMECMP (`cause` 7).
    pub fn local_addr(&self, hart: u32, cause: u32) -> Result<u64, IntcError> {
        let st = self.handle.state.borrow();
        let want = Some(HartTarget {
            hart,
            cause: Some(cause),
        });
        for b in &st.clint_info {
            for k in 0..clint_slots(b) {
                let k64 = k as u64;
                match (b.kind, cause) {
                    (ClintKind::Clint, MSIP) if sw_target(b, k) == want => {
                        return Ok(b.regs[0].base + 4 * k64);
                    }
                    (ClintKind::Clint, MTIP) if timer_target(b, k) == want => {
                        return Ok(b.regs[0].base + CLINT_MTIMECMP + 8 * k64);
                    }
                    (ClintKind::AclintMswi | ClintKind::AclintSswi, MSIP | SSIP)
                        if sw_target(b, k) == want =>
                    {
                        return Ok(b.regs[0].base + 4 * k64);
                    }
                    (ClintKind::AclintMtimer, MTIP) if timer_target(b, k) == want => {
                        return Ok(b.regs[1].base + 8 * k64);
                    }
                    _ => {}
                }
            }
        }
        Err(IntcError::NoLocalSource { hart, cause })
    }

    /// Address of MTIME.
    pub fn mtime_addr(&self) -> Result<u64, IntcError> {
        let st = self.handle.state.borrow();
        st.clint_info
            .iter()
            .find_map(mtime_addr_of)
            .ok_or(IntcError::NoLocalSource {
                hart: 0,
                cause: MTIP,
            })
    }

    /// GEILEN of `hart` (see [`IntcHandle::geilen`]).
    pub fn geilen(&self, hart: u32) -> u32 {
        self.handle.geilen(hart)
    }

    /// Implements only guest files `1..=geilen` behind every S-level
    /// IMSIC file (GEILEN is a property of the hart, not of the
    /// devicetree: Linux probes it by writing all ones to `hgeie`; QEMU's
    /// `aia-guests=N` gives GEILEN = N inside a `2^guest-index-bits`
    /// stride). An MSI to a page past the last implemented file is lost.
    pub fn limit_guest_files(&mut self, geilen: u32) -> Result<(), IntcError> {
        {
            let mut st = self.handle.state.borrow_mut();
            let keep = geilen as usize + 1;
            let files = |m: &ImsicInfo| m.targets.len().min(m.files.len());
            // Check every file before changing any.
            for (b, m) in st.imsic_info.iter().enumerate() {
                if !m.supervisor() {
                    continue;
                }
                for (k, t) in m.targets.iter().enumerate().take(files(m)) {
                    if keep > st.core.files_len(b, k) {
                        return Err(IntcError::NoSuchGuest {
                            hart: t.hart,
                            guest: geilen,
                        });
                    }
                }
            }
            for b in 0..st.imsic_info.len() {
                if !st.imsic_info[b].supervisor() {
                    continue;
                }
                for k in 0..files(&st.imsic_info[b]) {
                    st.core.truncate_files(b, k, keep);
                }
                st.guest_used[b].retain(|&(_, g), _| g < keep);
            }
            // hgeie bits of guests that no longer exist read zero.
            for hart in st.core.hyp_harts() {
                st.core.clip_hgeie(hart);
            }
        }
        self.guest_isrs
            .borrow_mut()
            .retain(|&(_, g, _), _| g <= geilen);
        self.sync();
        Ok(())
    }

    /// A write of `hstatus.VGEIN`.
    pub fn set_vgein(&mut self, hart: u32, vgein: u32) {
        self.handle.set_vgein(hart, vgein);
        self.sync();
    }

    pub fn vgein(&self, hart: u32) -> u32 {
        self.handle.vgein(hart)
    }

    /// A write of `hgeie`.
    pub fn set_hgeie(&mut self, hart: u32, v: u64) {
        self.handle.set_hgeie(hart, v);
        self.sync();
    }

    pub fn hgeie(&self, hart: u32) -> u64 {
        self.handle.hgeie(hart)
    }

    /// `hgeip` of `hart`.
    pub fn hgeip(&self, hart: u32) -> u64 {
        self.handle.hgeip(hart)
    }

    /// `mip & (SGEIP | VSEIP)` of `hart`.
    pub fn hyp_pending(&self, hart: u32) -> u64 {
        self.handle.hyp_pending(hart)
    }

    /// `vsiselect`/`vsireg` read of the guest file VGEIN selects.
    pub fn vs_imsic_csr_read(&mut self, hart: u32, isel: u32) -> Result<u64, IntcError> {
        self.handle.vs_imsic_csr_read(hart, isel)
    }

    /// `vsiselect`/`vsireg` write.
    pub fn vs_imsic_csr_write(&mut self, hart: u32, isel: u32, v: u64) -> Result<(), IntcError> {
        let r = self.handle.vs_imsic_csr_write(hart, isel, v);
        self.sync();
        r
    }

    /// `vstopei` read.
    pub fn vstopei(&mut self, hart: u32) -> Result<u32, IntcError> {
        self.handle.vstopei(hart)
    }

    /// `csrrw rd, vstopei, x0`.
    pub fn claim_vstopei(&mut self, hart: u32) -> Result<u32, IntcError> {
        let r = self.handle.claim_vstopei(hart);
        self.sync();
        r
    }

    /// The address of guest file `guest` of `hart` (what a device assigned
    /// to that guest's virtual hart is programmed with).
    pub fn guest_msi_target(&self, hart: u32, guest: u32) -> Result<u64, IntcError> {
        let st = self.handle.state.borrow();
        let (b, k, n) = st
            .core
            .guest_files(hart)
            .ok_or(IntcError::NoImsic { hart, cause: SEIP })?;
        if guest == 0 || guest > n {
            return Err(IntcError::NoSuchGuest { hart, guest });
        }
        Ok(st.imsic_info[b].files[k] + u64::from(guest) * crate::aia::IMSIC_PAGE)
    }

    /// Installs a VS-level `handler` for an MSI to guest file `guest` of
    /// `hart`: allocates the lowest free identity in that file, enables it
    /// and the file's delivery, sets the guest's `hgeie` bit, and returns
    /// the address/data pair the device must write. The modelled HS-mode
    /// software takes SGEIP; for each guest with a set `hgeip & hgeie` bit
    /// (lowest first) it switches `hstatus.VGEIN` to that guest, the guest
    /// claims through `vstopei` and its handler runs (cause [`VSEIP`]),
    /// then VGEIN is restored.
    pub fn install_guest_msi(
        &mut self,
        hart: u32,
        guest: u32,
        handler: impl FnMut(&IrqEvent<'_>) + 'static,
    ) -> Result<Msi, IntcError> {
        if self.privilege != Privilege::Supervisor {
            return Err(IntcError::GuestNeedsSupervisor);
        }
        self.cpu(hart)?;
        let addr = self.guest_msi_target(hart, guest)?;
        let id = {
            let mut st = self.handle.state.borrow_mut();
            let (b, k, _) = st
                .core
                .guest_files(hart)
                .ok_or(IntcError::NoImsic { hart, cause: SEIP })?;
            let ids = st.imsic_info[b].ids;
            let used = st.guest_used[b].entry((k, guest as usize)).or_default();
            let id = (1..=ids)
                .find(|i| !used.contains(i))
                .ok_or(IntcError::NoFreeIdentity { hart })?;
            used.insert(id);
            st.enable_identity(b, k, guest as usize, id)?;
            id
        };
        let hgeie = self.handle.hgeie(hart);
        self.handle.set_hgeie(hart, hgeie | 1 << guest);
        self.ensure_guest(hart)?;
        self.guest_isrs
            .borrow_mut()
            .insert((hart, guest, id), Box::new(handler));
        self.sync();
        Ok(Msi { addr, data: id })
    }

    /// Every VS-level claim so far.
    pub fn guest_deliveries(&self) -> Vec<GuestDelivery> {
        self.handle.state.borrow().guest_log.clone()
    }

    /// Registers the SGEIP loop for `hart` once.
    fn ensure_guest(&mut self, hart: u32) -> Result<(), IntcError> {
        if !self.handle.state.borrow_mut().mie.insert((hart, SGEIP)) {
            return Ok(());
        }
        let handle = self.handle.clone();
        let isrs = self.guest_isrs.clone();
        self.cpu(hart)?.register_isr(SGEIP, move |ictx| {
            let mut runs = 0usize;
            loop {
                let next = {
                    let st = handle.state.borrow();
                    let live = st.core.hgeip(hart) & st.core.hgeie(hart);
                    (live != 0).then(|| live.trailing_zeros())
                };
                let Some(guest) = next else {
                    if runs == 0 {
                        handle.state.borrow_mut().stats.spurious += 1;
                    }
                    break;
                };
                if runs == STORM_BUDGET {
                    handle.state.borrow_mut().stats.storms += 1;
                    break;
                }
                runs += 1;
                // The hypervisor switches to the guest's virtual hart; the
                // guest claims through vstopei (its stopei).
                let saved = handle.vgein(hart);
                handle.set_vgein(hart, guest);
                let id = handle.claim_vstopei(hart).map_or(0, |v| v >> 16);
                if id == 0 {
                    handle.set_vgein(hart, saved);
                    break;
                }
                handle
                    .state
                    .borrow_mut()
                    .guest_log
                    .push(GuestDelivery { hart, guest, id });
                let isr = isrs.borrow_mut().remove(&(hart, guest, id));
                match isr {
                    Some(mut f) => {
                        f(&IrqEvent {
                            ctx: ictx,
                            hart,
                            cause: VSEIP,
                            source: id,
                            intc: &handle,
                        });
                        isrs.borrow_mut().entry((hart, guest, id)).or_insert(f);
                    }
                    None => handle.state.borrow_mut().stats.unhandled += 1,
                }
                handle.set_vgein(hart, saved);
            }
            handle.state.borrow_mut().asserted.remove(&(hart, SGEIP));
        });
        Ok(())
    }

    /// A PCI function below `host` with requester ID `rid` writes `data` to
    /// bus address `bus_addr` (its MSI address register; a plain MSI
    /// capability writes its 16-bit message data zero-extended). The
    /// requester ID picks the MSI controller and device ID through
    /// `msi-map`/`msi-parent` ([`Platform::pci_msi_route`]), and the host's
    /// `dma-ranges` translate the address ([`Platform::pci_cpu_address`]).
    /// There is no IOMMU here, so the write lands wherever the CPU address
    /// decodes, as on QEMU `virt`; [`PciMsi::in_parent`] says whether that
    /// is a file of the routed controller. A write that decodes to no
    /// interrupt file or `setipnum_le` is counted in
    /// [`RiscvIntc::lost_msis`] and refused.
    pub fn pci_msi_write(
        &mut self,
        platform: &Platform,
        host: &str,
        rid: u32,
        bus_addr: u64,
        data: u32,
    ) -> Result<PciMsi, IntcError> {
        let route = platform.pci_msi_route(host, rid)?;
        if !bus_addr.is_multiple_of(4) {
            return Err(IntcError::Misaligned { addr: bus_addr });
        }
        let cpu_addr = platform.pci_cpu_address(host, bus_addr, 4)?;
        let rec = {
            let mut st = self.handle.state.borrow_mut();
            if !st.imsic_info.iter().any(|m| m.path == route.controller) {
                return Err(IntcError::MsiControllerNotModelled {
                    controller: route.controller.clone(),
                });
            }
            let landed = st.core.locate_file(cpu_addr).map(|(b, k, g)| {
                let m = &st.imsic_info[b];
                (m.path.clone(), m.targets[k].hart, g as u32)
            });
            if let Err(e) = st.core.deliver_msi(cpu_addr, data) {
                st.core.push_lost_msi(Msi {
                    addr: cpu_addr,
                    data,
                });
                return Err(e);
            }
            st.core.settle();
            let rec = PciMsi {
                rid,
                device_id: route.device_id,
                in_parent: landed.as_ref().is_some_and(|l| l.0 == route.controller),
                controller: route.controller,
                bus_addr,
                cpu_addr,
                data,
                imsic: landed.as_ref().map(|l| l.0.clone()),
                hart: landed.as_ref().map(|l| l.1),
                guest: landed.as_ref().map(|l| l.2),
            };
            st.pci_log.push(rec.clone());
            rec
        };
        self.sync();
        Ok(rec)
    }

    /// Every PCI MSI written through [`RiscvIntc::pci_msi_write`].
    pub fn pci_msis(&self) -> Vec<PciMsi> {
        self.handle.state.borrow().pci_log.clone()
    }

    /// Re-evaluates every enabled cause on every hart and raises those whose
    /// level rose on the hart's Cpu (delivered now, or held pending while
    /// the hart's interrupts are masked).
    pub fn sync(&mut self) {
        let rising: Vec<(u32, u32)> = {
            let mut st = self.handle.state.borrow_mut();
            let enabled: Vec<(u32, u32)> = st.mie.iter().copied().collect();
            let mut out = Vec::new();
            for (hart, cause) in enabled {
                // A raised cause stays asserted until its handler has run
                // (it may be pending on a masked hart meanwhile).
                if st.core.level(hart, cause) && st.asserted.insert((hart, cause)) {
                    out.push((hart, cause));
                }
            }
            out
        };
        for (hart, cause) in rising {
            if let Some((_, cpu)) = self.harts.iter_mut().find(|h| h.0 == hart) {
                cpu.raise(cause);
            }
        }
    }
}

/// The local causes the platform's PLIC, APLIC, IMSIC and CLINT/ACLINT blocks wire to
/// `hart` (hidden outputs excluded).
pub fn local_causes(platform: &Platform, hart: u32) -> Vec<u32> {
    let mut v: Vec<u32> = platform
        .clints
        .iter()
        .flat_map(|c| c.targets.iter())
        .chain(platform.plics.iter().flat_map(|p| p.contexts.iter()))
        .chain(platform.aplics.iter().flat_map(|a| a.idcs.iter()))
        .chain(platform.imsics.iter().flat_map(|m| m.targets.iter()))
        .filter(|t| t.hart == hart)
        .filter_map(|t| t.cause)
        .collect();
    v.sort();
    v.dedup();
    v
}
