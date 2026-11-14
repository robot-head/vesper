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
use crate::aia::{AiaError, AplicDomain, ImsicFile, Msi, MsiAddrCfg, SourceMode};
use crate::dtb::{
    AplicInfo, ClintInfo, ClintKind, ControllerKind, DtbError, HartTarget, ImsicInfo, IrqLine,
    MemRange, Platform, PlicInfo, Trigger,
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

/// Most handler runs for one delivery before the level is declared a storm.
pub const STORM_BUDGET: usize = 64;
/// Most PLIC contexts the specification's register map has room for.
pub const PLIC_MAX_CONTEXTS: usize = 15_872;
/// Largest PLIC priority / threshold value (3 implemented bits).
pub const PLIC_MAX_PRIORITY: u32 = 7;
/// Most MSI rounds one operation may cause (an MSI written to an APLIC's
/// `setipnum_le` can make it forward another).
const MSI_ROUNDS: usize = 64;

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
        let n = match info.kind {
            ClintKind::Clint => info.targets.len().div_ceil(2),
            _ => info.targets.len(),
        };
        Self {
            info: info.clone(),
            sip: vec![false; n],
            mtimecmp: vec![u64::MAX; n],
        }
    }

    /// Software-interrupt target of index `k`.
    fn sw_target(&self, k: usize) -> Option<HartTarget> {
        match self.info.kind {
            ClintKind::Clint => self.info.targets.get(2 * k).copied(),
            ClintKind::AclintMswi | ClintKind::AclintSswi => self.info.targets.get(k).copied(),
            ClintKind::AclintMtimer => None,
        }
    }

    fn timer_target(&self, k: usize) -> Option<HartTarget> {
        match self.info.kind {
            ClintKind::Clint => self.info.targets.get(2 * k + 1).copied(),
            ClintKind::AclintMtimer => self.info.targets.get(k).copied(),
            _ => None,
        }
    }

    fn has_timer(&self) -> bool {
        matches!(self.info.kind, ClintKind::Clint | ClintKind::AclintMtimer)
    }

    /// Physical address of the MTIME register this block's MTIMECMPs
    /// compare against. Blocks naming the same address share one counter.
    fn mtime_addr(&self) -> Option<u64> {
        match self.info.kind {
            ClintKind::Clint => Some(self.info.regs[0].base + CLINT_MTIME),
            ClintKind::AclintMtimer => Some(self.info.regs[0].base),
            _ => None,
        }
    }
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

#[derive(Debug)]
struct ImsicBlock {
    info: ImsicInfo,
    /// `[file][guest]`; guest 0 is the hart's own file.
    files: Vec<Vec<ImsicFile>>,
    /// Allocated identities per file.
    used: Vec<BTreeSet<u32>>,
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

#[derive(Debug)]
struct State {
    plics: Vec<Plic>,
    aplics: Vec<AplicDomain>,
    aplic_info: Vec<AplicInfo>,
    imsics: Vec<ImsicBlock>,
    blocks: Vec<Block>,
    /// MTIME counters by physical address (one per distinct register).
    mtimes: BTreeMap<u64, u64>,
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
    lost_msis: Vec<Msi>,
}

impl State {
    fn ext_context(&self, hart: u32, cause: u32) -> Option<(usize, usize)> {
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

    /// The IMSIC and file of `(hart, cause)`.
    fn imsic_file(&self, hart: u32, cause: u32) -> Option<(usize, usize)> {
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

    /// The level of `cause` on `hart`.
    fn level(&self, hart: u32, cause: u32) -> bool {
        let want = HartTarget {
            hart,
            cause: Some(cause),
        };
        match cause {
            MEIP | SEIP => {
                self.plics
                    .iter()
                    .any(|p| (0..p.contexts.len()).any(|c| p.contexts[c] == want && p.eip(c)))
                    || self.aplics.iter().zip(&self.aplic_info).any(|(d, info)| {
                        info.idcs
                            .iter()
                            .enumerate()
                            .any(|(i, t)| *t == want && d.idc_asserted(i))
                    })
                    || self.imsics.iter().any(|m| {
                        m.info
                            .targets
                            .iter()
                            .enumerate()
                            .any(|(k, t)| *t == want && m.files[k][0].asserted())
                    })
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

    /// The root of `d`'s domain tree.
    fn root_of(&self, d: usize) -> usize {
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

    /// Delegation and the root's MSI configuration flow from parents to
    /// children; then MSI-mode domains forward, and MSIs land in IMSIC
    /// files (or APLIC `setipnum_le` registers).
    fn settle(&mut self) {
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
    fn deliver_msi(&mut self, addr: u64, data: u32) -> Result<(), IntcError> {
        for m in &mut self.imsics {
            if let Some((k, g, off)) = m.locate(addr) {
                m.files[k][g].mmio_write32(off, data);
                return Ok(());
            }
        }
        for d in &mut self.aplics {
            if d.contains(addr) && addr - d.base() == 0x2000 {
                return d.write32(addr, data).map_err(IntcError::from);
            }
        }
        Err(IntcError::Unmapped { addr })
    }

    fn read32(&mut self, addr: u64) -> Result<u32, IntcError> {
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

    fn write32(&mut self, addr: u64, v: u32) -> Result<(), IntcError> {
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

    fn mtime_at(&self, addr: u64) -> u64 {
        self.mtimes.get(&addr).copied().unwrap_or(0)
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

    /// Drives a wire into `ctl` (a PLIC, or any domain of an APLIC tree:
    /// the wire enters the root and every domain sees it).
    fn set_wire(&mut self, ctl: Ctl, src: u32, high: bool) -> Result<(), IntcError> {
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
    fn claim_external(&mut self, hart: u32, cause: u32) -> Option<(Ctl, u32)> {
        let want = HartTarget {
            hart,
            cause: Some(cause),
        };
        if let Some((b, k)) = self.imsic_file(hart, cause) {
            let f = &mut self.imsics[b].files[k][0];
            if f.asserted() {
                return Some((Ctl::Imsic(b, k), f.claim() >> 16));
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

    fn complete_external(&mut self, hart: u32, cause: u32, ctl: Ctl, number: u32) {
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
            Ctl::Imsic(b, k) => {
                // Linux's APLIC-MSI EOI: a level source still asserted after
                // its handler is re-triggered through setipnum (AIA §4.9.2).
                if let Some(&(d, s)) = self.msi_sources.get(&(b, k, number))
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

    fn controller_path(&self, ctl: Ctl) -> String {
        match ctl {
            Ctl::Plic(_) => String::new(),
            Ctl::Aplic(d) => self.aplic_info[d].path.clone(),
            Ctl::Imsic(b, _) => self.imsics[b].info.path.clone(),
        }
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

/// Shared access to the controller registers from inside handlers (an ISR
/// acknowledging its device, a timer handler moving MTIMECMP). Changes made
/// through it are seen by the Cpus at the next [`RiscvIntc`] operation.
#[derive(Clone, Debug)]
pub struct IntcHandle {
    state: Rc<RefCell<State>>,
}

impl IntcHandle {
    pub fn read32(&self, addr: u64) -> Result<u32, IntcError> {
        self.state.borrow_mut().read32(addr)
    }

    pub fn write32(&self, addr: u64, v: u32) -> Result<(), IntcError> {
        self.state.borrow_mut().write32(addr, v)
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
        let ctl = if !st.plics.is_empty() {
            Ctl::Plic(0)
        } else if !st.aplics.is_empty() {
            Ctl::Aplic(0)
        } else {
            return Err(IntcError::NoPlic);
        };
        st.set_wire(ctl, src, high)
    }

    /// The device drives source `src` of controller `ctl`.
    pub fn set_controller_line(&self, ctl: Ctl, src: u32, high: bool) -> Result<(), IntcError> {
        let mut st = self.state.borrow_mut();
        match ctl {
            Ctl::Plic(p) if p >= st.plics.len() => return Err(IntcError::NoPlic),
            Ctl::Aplic(d) if d >= st.aplics.len() => return Err(IntcError::NoPlic),
            _ => {}
        }
        st.set_wire(ctl, src, high)
    }

    /// A bus master writes `data` to `addr` (an MSI to an IMSIC file or an
    /// APLIC's `setipnum_le`).
    pub fn msi_write(&self, addr: u64, data: u32) -> Result<(), IntcError> {
        let mut st = self.state.borrow_mut();
        if !addr.is_multiple_of(4) {
            return Err(IntcError::Misaligned { addr });
        }
        st.deliver_msi(addr, data)?;
        st.settle();
        Ok(())
    }

    /// The MTIME that `hart`'s timer compares against (0 if it has none).
    pub fn mtime(&self, hart: u32) -> u64 {
        let st = self.state.borrow();
        let want = Some(HartTarget {
            hart,
            cause: Some(MTIP),
        });
        st.blocks
            .iter()
            .find(|b| (0..b.mtimecmp.len()).any(|k| b.timer_target(k) == want))
            .and_then(Block::mtime_addr)
            .map_or(0, |a| st.mtime_at(a))
    }

    /// Clears a pending SSIP (software writing `sip.SSIP = 0`).
    pub fn clear_ssip(&self, hart: u32) {
        let mut st = self.state.borrow_mut();
        let want = Some(HartTarget {
            hart,
            cause: Some(SSIP),
        });
        for b in &mut st.blocks {
            for k in 0..b.sip.len() {
                if b.info.kind == ClintKind::AclintSswi && b.sw_target(k) == want {
                    b.sip[k] = false;
                }
            }
        }
    }

    fn with_file<T>(
        &self,
        hart: u32,
        level: Privilege,
        f: impl FnOnce(&mut ImsicFile) -> T,
    ) -> Result<T, IntcError> {
        let cause = level.external();
        let mut st = self.state.borrow_mut();
        let (b, k) = st
            .imsic_file(hart, cause)
            .ok_or(IntcError::NoImsic { hart, cause })?;
        Ok(f(&mut st.imsics[b].files[k][0]))
    }

    /// `*ireg` read of `hart`'s IMSIC file at `level` (`miselect`/`mireg`
    /// for M, `siselect`/`sireg` for S).
    pub fn imsic_csr_read(&self, hart: u32, level: Privilege, isel: u32) -> Result<u64, IntcError> {
        self.with_file(hart, level, |f| f.csr_read(isel))?
            .map_err(IntcError::from)
    }

    /// `*ireg` write.
    pub fn imsic_csr_write(
        &self,
        hart: u32,
        level: Privilege,
        isel: u32,
        v: u64,
    ) -> Result<(), IntcError> {
        self.with_file(hart, level, |f| f.csr_write(isel, v))?
            .map_err(IntcError::from)
    }

    /// `*topei` read.
    pub fn topei(&self, hart: u32, level: Privilege) -> Result<u32, IntcError> {
        self.with_file(hart, level, |f| f.topei())
    }

    /// `csrrw rd, *topei, x0`: claims the top identity.
    pub fn claim_topei(&self, hart: u32, level: Privilege) -> Result<u32, IntcError> {
        self.with_file(hart, level, |f| f.claim())
    }

    /// Whether identity `id` is pending in guest file `guest` (1 and up)
    /// of `hart`'s S-level IMSIC. Guest files take MSIs but raise no hart
    /// cause here (there is no hypervisor model); this is what `hgeip`
    /// and the guest's `vstopei` would build on.
    pub fn guest_eip(&self, hart: u32, guest: u32, id: u32) -> Result<bool, IntcError> {
        let st = self.state.borrow();
        let none = IntcError::NoImsic { hart, cause: SEIP };
        let (b, k) = st.imsic_file(hart, SEIP).ok_or(none.clone())?;
        st.imsics[b].files[k]
            .get(guest as usize)
            .filter(|_| guest != 0)
            .map(|f| f.is_pending(id))
            .ok_or(none)
    }
}

/// The interrupt fabric of a RISC-V devicetree platform.
pub struct RiscvIntc {
    privilege: Privilege,
    harts: Vec<(u32, Cpu)>,
    handle: IntcHandle,
    isrs: Rc<RefCell<BTreeMap<(Ctl, u32), Handler>>>,
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
        let plics = platform
            .plics
            .iter()
            .map(Plic::new)
            .collect::<Result<Vec<_>, _>>()?;
        let mut imsics = Vec::new();
        for info in &platform.imsics {
            let guests = 1usize << info.guest_index_bits;
            let files = (0..info.files.len())
                .map(|_| (0..guests).map(|_| ImsicFile::new(info.ids)).collect())
                .collect::<Result<Vec<Vec<_>>, _>>()?;
            imsics.push(ImsicBlock {
                info: info.clone(),
                files,
                used: vec![BTreeSet::new(); info.files.len()],
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
        let mut state = State {
            plics,
            aplics,
            aplic_info: platform.aplics.clone(),
            imsics,
            blocks: platform.clints.iter().map(Block::new).collect(),
            mtimes: BTreeMap::new(),
            asserted: BTreeSet::new(),
            mie: BTreeSet::new(),
            msi_sources: BTreeMap::new(),
            log: Vec::new(),
            ext_log: Vec::new(),
            stats: IntcStats::default(),
            lost_msis: Vec::new(),
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
        for d in 0..st.aplics.len() {
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
                let d0 = &mut st.aplics[d];
                d0.write32(base + 0x1bc0, cfg.m_ppn as u32)?;
                d0.write32(base + 0x1bc8, cfg.s_ppn as u32)?;
                d0.write32(
                    base + 0x1bcc,
                    (cfg.lhxs_s << 20) | ((cfg.s_ppn >> 32) as u32 & 0xfff),
                )?;
                d0.write32(base + 0x1bc4, high_m)?;
            }
        }
        if privilege == Privilege::Supervisor {
            // Parents first, so a grandchild's delegation finds its sources
            // active in the child.
            let mut order: Vec<usize> = (0..st.aplics.len()).collect();
            let depth = |st: &State, mut d: usize| {
                let mut n = 0;
                while st.root_of(d) != d {
                    let p = st.aplic_info[d].parent.clone();
                    d = p
                        .and_then(|p| st.aplic_info.iter().position(|a| a.path == p))
                        .unwrap_or(d);
                    n += 1;
                    if n > st.aplics.len() {
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
                    for s in del.first..=del.last.min(st.aplics[d].sources()) {
                        st.aplics[d].write32(base + 4 * u64::from(s), (1 << 10) | ci as u32)?;
                    }
                }
                st.settle();
            }
        }
        st.settle();
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
        if st.plics.is_empty() {
            return Err(IntcError::NoPlic);
        }
        let cause = self.privilege.external();
        st.ext_context(hart, cause)
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
        self.handle.state.borrow().lost_msis.clone()
    }

    /// Base address of the first PLIC's register window.
    pub fn plic_base(&self) -> Result<u64, IntcError> {
        Ok(self
            .handle
            .state
            .borrow()
            .plics
            .first()
            .ok_or(IntcError::NoPlic)?
            .base)
    }

    /// Base addresses of every PLIC, in [`Platform::plics`] order.
    pub fn plic_bases(&self) -> Vec<u64> {
        self.handle
            .state
            .borrow()
            .plics
            .iter()
            .map(|p| p.base)
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
        {
            let mut st = self.handle.state.borrow_mut();
            let addrs: Vec<u64> = st.blocks.iter().filter_map(Block::mtime_addr).collect();
            for a in addrs {
                st.mtimes.entry(a).or_insert(0);
            }
            for v in st.mtimes.values_mut() {
                *v = v.wrapping_add(ticks);
            }
        }
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
                    let ctx = st.plics[p]
                        .contexts
                        .iter()
                        .position(|t| *t == want)
                        .ok_or(IntcError::NoContext { hart, cause })?;
                    (st.plics[p].base, ctx)
                };
                self.handle.state.borrow_mut().plics[p].set_edge(src, edge)?;
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
                        Privilege::Machine => st.root_of(named),
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
            let dom = &st.aplics[d];
            (
                dom.base(),
                st.aplic_info[d].clone(),
                dom.is_delegated_here(src),
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
                    .imsics
                    .iter()
                    .position(|m| &m.info.path == imsic_path)
                    .ok_or(IntcError::NoImsic { hart, cause })?;
                let k = st.imsics[b]
                    .info
                    .targets
                    .iter()
                    .position(|t| *t == want)
                    .ok_or(IntcError::NoImsic { hart, cause })?;
                (
                    b,
                    k,
                    st.imsics[b].info.files[k],
                    st.aplics[d].msi_cfg(),
                    st.aplics[d].is_supervisor(),
                )
            };
            let harts = 1u32 << (cfg.lhxw + cfg.hhxw);
            let hart_index =
                cfg.hart_of(sup, file_addr, harts)
                    .ok_or_else(|| IntcError::ImsicLayout {
                        imsic: imsic_path.clone(),
                        detail: format!("file {file_addr:#x} is not reachable from {}", info.path),
                    })?;
            let id = self.allocate_identity(b, k, hart, priority)?;
            h.write32(base, APLIC_DOMAINCFG_IE | APLIC_DOMAINCFG_DM)?;
            h.write32(base + 4 * u64::from(src), mode.bits())?;
            h.write32(base + 0x3000 + 4 * u64::from(src), (hart_index << 18) | id)?;
            {
                let mut st = h.state.borrow_mut();
                let f = &mut st.imsics[b].files[k][0];
                f.csr_write(crate::aia::ISEL_EIDELIVERY, 1)?;
                let isel = crate::aia::ISEL_EIE0 + 2 * (id / 64);
                let old = f.csr_read(isel)?;
                f.csr_write(isel, old | 1 << (id % 64))?;
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
        let ids = st.imsics[b].info.ids;
        let band = (ids + 1) / 8;
        let lo = 1 + (PLIC_MAX_PRIORITY - priority.clamp(1, PLIC_MAX_PRIORITY)) * band;
        let used = &mut st.imsics[b].used[k];
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
            .imsic_file(hart, cause)
            .ok_or(IntcError::NoImsic { hart, cause })?;
        Ok(st.imsics[b].info.files[k])
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
                .imsic_file(hart, cause)
                .ok_or(IntcError::NoImsic { hart, cause })?;
            (b, k, st.imsics[b].info.files[k])
        };
        let id = self.allocate_identity(b, k, hart, priority)?;
        {
            let mut st = self.handle.state.borrow_mut();
            let f = &mut st.imsics[b].files[k][0];
            f.csr_write(crate::aia::ISEL_EIDELIVERY, 1)?;
            let isel = crate::aia::ISEL_EIE0 + 2 * (id / 64);
            let old = f.csr_read(isel)?;
            f.csr_write(isel, old | 1 << (id % 64))?;
        }
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
        self.handle.state.borrow().level(hart, cause)
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
                let claimed = handle.state.borrow_mut().claim_external(hart, cause);
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
                        Ctl::Plic(p) => st.plics[p].base,
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
        let reachable = self.handle.state.borrow().blocks.iter().any(|b| {
            b.info.targets.contains(&HartTarget {
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
            while handle.state.borrow().level(hart, cause) {
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
        for b in &st.blocks {
            for k in 0..b.sip.len() {
                let k64 = k as u64;
                match (b.info.kind, cause) {
                    (ClintKind::Clint, MSIP) if b.sw_target(k) == want => {
                        return Ok(b.info.regs[0].base + 4 * k64);
                    }
                    (ClintKind::Clint, MTIP) if b.timer_target(k) == want => {
                        return Ok(b.info.regs[0].base + CLINT_MTIMECMP + 8 * k64);
                    }
                    (ClintKind::AclintMswi | ClintKind::AclintSswi, MSIP | SSIP)
                        if b.sw_target(k) == want =>
                    {
                        return Ok(b.info.regs[0].base + 4 * k64);
                    }
                    (ClintKind::AclintMtimer, MTIP) if b.timer_target(k) == want => {
                        return Ok(b.info.regs[1].base + 8 * k64);
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
        st.blocks
            .iter()
            .find_map(|b| match b.info.kind {
                ClintKind::Clint => Some(b.info.regs[0].base + CLINT_MTIME),
                ClintKind::AclintMtimer => Some(b.info.regs[0].base),
                _ => None,
            })
            .ok_or(IntcError::NoLocalSource {
                hart: 0,
                cause: MTIP,
            })
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
                if st.level(hart, cause) && st.asserted.insert((hart, cause)) {
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
