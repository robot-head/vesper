//! RISC-V PLIC and CLINT / ACLINT emulation driving the IrqGuard [`Cpu`]
//! model (§26.1 item 3), configured from devicetree numbers
//! ([`crate::dtb::Platform::plics`] and [`crate::dtb::Platform::clints`]).
//!
//! [`RiscvIntc`] owns one [`Cpu`] per enabled hart and a software model of
//! the platform's interrupt blocks:
//!
//! * the PLIC as the RISC-V PLIC specification (1.0.0) lays it out:
//!   per-source priority at `base + 4*src`, pending bits at `0x1000`,
//!   per-context enables at `0x2000 + 0x80*ctx`, threshold at
//!   `0x20_0000 + 0x1000*ctx` and claim/complete one word above it. A
//!   level-triggered gateway per source: a source is forwarded (pending)
//!   while its line is high and it is not in service; a claim returns the
//!   highest-priority pending source enabled for the context with priority
//!   above the threshold (ties to the lowest id) and puts it in service; a
//!   completion of a source enabled for that context ends service, and a
//!   line still high is forwarded again. Priorities and thresholds are
//!   3 bits wide (values 0..=7, as QEMU's `sifive_plic` on `virt` uses);
//! * the SiFive CLINT (MSIP at `4*k`, MTIMECMP at `0x4000 + 8*k`, MTIME at
//!   `0xbff8`) and the ACLINT MSWI, SSWI and MTIMER devices (MTIMER `reg`
//!   is MTIME then the MTIMECMP array, as QEMU emits it). MTIP is the level
//!   `mtime >= mtimecmp`, against the MTIME of the hart's own block (blocks
//!   that name the same MTIME address share one counter); MTIMECMP starts at `u64::MAX` (its reset value is
//!   unspecified);
//! * the hart side: a cause is enabled (`mie`) once a handler is installed
//!   for it; the controller's output levels are re-evaluated after every
//!   operation ([`RiscvIntc::sync`]) and a rising level raises the cause on
//!   that hart's [`Cpu`], where `mstatus.MIE` (the [`crate::irq::IrqGuard`])
//!   decides whether it runs now or stays pending. External-interrupt
//!   delivery claims from the hart's PLIC context, runs the source's ISR and
//!   completes, until the claim returns 0. A level that stays high after
//!   [`STORM_BUDGET`] handler runs is counted as a storm and left asserted.
//!
//! Machine-mode software ([`Privilege::Machine`]) receives MEIP (11) from
//! its M-mode PLIC context, MSIP (3) and MTIP (7); supervisor-mode software
//! ([`Privilege::Supervisor`]) receives SEIP (9) and SSIP (1) from the ACLINT
//! SSWI. The supervisor timer comes from SBI or Sstc, which a devicetree
//! does not describe, so it is not modelled.
//!
//! This is a software model of the controllers' register protocol; it is
//! not a cycle model, models one level per source (no edge-triggered or MSI
//! sources), and runs no code on a RISC-V core.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::Arch;
use crate::dtb::{ClintInfo, ClintKind, DtbError, HartTarget, Platform, PlicInfo};
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

const PLIC_PENDING: u64 = 0x1000;
const PLIC_ENABLE: u64 = 0x2000;
const PLIC_ENABLE_STRIDE: u64 = 0x80;
const PLIC_CONTEXT: u64 = 0x20_0000;
const PLIC_CONTEXT_STRIDE: u64 = 0x1000;
const CLINT_MTIMECMP: u64 = 0x4000;
const CLINT_MTIME: u64 = 0xbff8;

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
    /// More than one PLIC (not modelled).
    MultiplePlics,
    NoPlic,
    NoSuchHart(u32),
    /// The hart has no PLIC context for this privilege's external cause.
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
    /// A device is not wired to the PLIC.
    NotOnPlic {
        device: String,
    },
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

/// The PLIC register file and gateways.
#[derive(Clone, Debug)]
pub struct Plic {
    base: u64,
    ndev: u32,
    contexts: Vec<HartTarget>,
    priority: Vec<u32>,
    level: Vec<bool>,
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
            pending: vec![false; n],
            in_service: vec![false; n],
            enable: vec![vec![false; n]; info.contexts.len()],
            threshold: vec![0; info.contexts.len()],
        })
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

    /// The device drives source `src`'s line.
    pub fn set_level(&mut self, src: u32, high: bool) -> Result<(), IntcError> {
        let s = self.src(src)?;
        self.level[s] = high;
        if high && !self.in_service[s] {
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
        if self.level[s] {
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
    /// PLIC source for external interrupts, else the cause.
    pub source: u32,
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
struct State {
    plic: Option<Plic>,
    blocks: Vec<Block>,
    /// MTIME counters by physical address (one per distinct register).
    mtimes: BTreeMap<u64, u64>,
    /// `(hart, cause)` raised on the Cpu and not yet finished.
    asserted: BTreeSet<(u32, u32)>,
    /// Causes with an installed handler (`mie`), per hart.
    mie: BTreeSet<(u32, u32)>,
    log: Vec<Delivery>,
    stats: IntcStats,
}

impl State {
    fn ext_context(&self, hart: u32, cause: u32) -> Option<usize> {
        self.plic.as_ref()?.contexts.iter().position(|t| {
            *t == HartTarget {
                hart,
                cause: Some(cause),
            }
        })
    }

    /// The level of `cause` on `hart`.
    fn level(&self, hart: u32, cause: u32) -> bool {
        let want = HartTarget {
            hart,
            cause: Some(cause),
        };
        match cause {
            MEIP | SEIP => self
                .plic
                .as_ref()
                .is_some_and(|p| (0..p.contexts.len()).any(|c| p.contexts[c] == want && p.eip(c))),
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

    fn read32(&mut self, addr: u64) -> Result<u32, IntcError> {
        if let Some(p) = self.plic.as_mut()
            && p.contains(addr)
        {
            return p.read32(addr);
        }
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

    fn write32(&mut self, addr: u64, v: u32) -> Result<(), IntcError> {
        if let Some(p) = self.plic.as_mut()
            && p.contains(addr)
        {
            return p.write32(addr, v);
        }
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
}

/// What an interrupt handler receives.
pub struct IrqEvent<'a> {
    pub ctx: &'a IsrCtx,
    pub hart: u32,
    pub cause: u32,
    /// The claimed PLIC source for external interrupts, else the cause.
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

    /// The device drives PLIC source `src`.
    pub fn set_line(&self, src: u32, high: bool) -> Result<(), IntcError> {
        self.state
            .borrow_mut()
            .plic
            .as_mut()
            .ok_or(IntcError::NoPlic)?
            .set_level(src, high)
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
}

/// The interrupt fabric of a RISC-V devicetree platform.
pub struct RiscvIntc {
    privilege: Privilege,
    harts: Vec<(u32, Cpu)>,
    handle: IntcHandle,
    isrs: Rc<RefCell<BTreeMap<u32, Handler>>>,
    plic_path: Option<String>,
}

impl std::fmt::Debug for RiscvIntc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RiscvIntc")
            .field("privilege", &self.privilege)
            .field("harts", &self.harts)
            .finish()
    }
}

impl RiscvIntc {
    /// Builds the controllers and one [`Cpu`] per enabled hart from
    /// `platform`. A platform may have no PLIC (only local interrupts).
    pub fn new(
        platform: &Platform,
        privilege: Privilege,
        ledger: &Ledger,
    ) -> Result<Self, IntcError> {
        if platform.arch != Arch::Riscv64 {
            return Err(IntcError::NotRiscv(platform.arch));
        }
        if platform.plics.len() > 1 {
            return Err(IntcError::MultiplePlics);
        }
        let plic = platform.plics.first().map(Plic::new).transpose()?;
        let state = State {
            plic,
            blocks: platform.clints.iter().map(Block::new).collect(),
            mtimes: BTreeMap::new(),
            asserted: BTreeSet::new(),
            mie: BTreeSet::new(),
            log: Vec::new(),
            stats: IntcStats::default(),
        };
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
            plic_path: platform.plics.first().map(|p| p.path.clone()),
        })
    }

    pub fn handle(&self) -> IntcHandle {
        self.handle.clone()
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

    /// The hart's PLIC context for this privilege's external interrupt.
    pub fn context_of(&self, hart: u32) -> Result<usize, IntcError> {
        let st = self.handle.state.borrow();
        if st.plic.is_none() {
            return Err(IntcError::NoPlic);
        }
        let cause = self.privilege.external();
        st.ext_context(hart, cause)
            .ok_or(IntcError::NoContext { hart, cause })
    }

    pub fn deliveries(&self) -> Vec<Delivery> {
        self.handle.state.borrow().log.clone()
    }

    pub fn stats(&self) -> IntcStats {
        self.handle.state.borrow().stats
    }

    /// Base address of the PLIC register window.
    pub fn plic_base(&self) -> Result<u64, IntcError> {
        Ok(self
            .handle
            .state
            .borrow()
            .plic
            .as_ref()
            .ok_or(IntcError::NoPlic)?
            .base)
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

    /// A device drives PLIC source `src`.
    pub fn set_line(&mut self, src: u32, high: bool) -> Result<(), IntcError> {
        let r = self.handle.set_line(src, high);
        self.sync();
        r
    }

    /// Drives the PLIC line of `device` (its first PLIC interrupt).
    pub fn set_device_line(
        &mut self,
        platform: &Platform,
        device: &str,
        high: bool,
    ) -> Result<u32, IntcError> {
        let src = self.plic_source(platform, device)?;
        self.set_line(src, high)?;
        Ok(src)
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

    fn plic_source(&self, platform: &Platform, device: &str) -> Result<u32, IntcError> {
        let plic = self.plic_path.as_deref().ok_or(IntcError::NoPlic)?;
        let id = platform
            .tree()
            .find(device)
            .ok_or_else(|| DtbError::NoSuchNode(device.to_string()))?;
        let path = platform.tree().path(id);
        platform
            .irq_lines
            .iter()
            .find(|l| l.device == path && l.controller == plic)
            .map(|l| l.number)
            .ok_or(IntcError::NotOnPlic { device: path })
    }

    /// Installs `handler` for `device`'s PLIC source and, through the PLIC
    /// registers, gives the source `priority` and enables it in `hart`'s
    /// context. Returns the source number. A source has one handler: installing
    /// it for a second hart replaces the handler and enables both contexts.
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
        let src = self.plic_source(platform, device)?;
        let ctx = self.context_of(hart)?;
        let base = self.plic_base()?;
        self.ensure_external(hart, ctx)?;
        self.isrs.borrow_mut().insert(src, Box::new(handler));
        self.handle.write32(base + 4 * u64::from(src), priority)?;
        let word = base + PLIC_ENABLE + PLIC_ENABLE_STRIDE * ctx as u64 + 4 * u64::from(src / 32);
        let old = self.handle.read32(word)?;
        self.handle.write32(word, old | (1 << (src % 32)))?;
        self.sync();
        Ok(src)
    }

    /// Registers the claim/complete loop for `hart`'s external cause once.
    fn ensure_external(&mut self, hart: u32, ctx: usize) -> Result<(), IntcError> {
        let cause = self.privilege.external();
        if !self.handle.state.borrow_mut().mie.insert((hart, cause)) {
            return Ok(());
        }
        let handle = self.handle.clone();
        let isrs = self.isrs.clone();
        self.cpu(hart)?.register_isr(cause, move |ictx| {
            let mut runs = 0usize;
            loop {
                let src = handle
                    .state
                    .borrow_mut()
                    .plic
                    .as_mut()
                    .map_or(0, |p| p.claim(ctx));
                if src == 0 {
                    if runs == 0 {
                        handle.state.borrow_mut().stats.spurious += 1;
                    }
                    break;
                }
                if runs == STORM_BUDGET {
                    // Complete it so the gateway can forward it again, but
                    // stop delivering.
                    let mut st = handle.state.borrow_mut();
                    if let Some(p) = st.plic.as_mut() {
                        p.complete(ctx, src);
                    }
                    st.stats.storms += 1;
                    break;
                }
                runs += 1;
                handle.state.borrow_mut().log.push(Delivery {
                    hart,
                    cause,
                    source: src,
                });
                // Take the ISR out so it may use the handle freely.
                let isr = isrs.borrow_mut().remove(&src);
                match isr {
                    Some(mut f) => {
                        f(&IrqEvent {
                            ctx: ictx,
                            hart,
                            cause,
                            source: src,
                            intc: &handle,
                        });
                        isrs.borrow_mut().entry(src).or_insert(f);
                    }
                    None => handle.state.borrow_mut().stats.unhandled += 1,
                }
                if let Some(p) = handle.state.borrow_mut().plic.as_mut() {
                    p.complete(ctx, src);
                }
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

/// The local causes the platform's PLIC and CLINT/ACLINT blocks wire to
/// `hart` (hidden outputs excluded).
pub fn local_causes(platform: &Platform, hart: u32) -> Vec<u32> {
    let mut v: Vec<u32> = platform
        .clints
        .iter()
        .flat_map(|c| c.targets.iter())
        .chain(platform.plics.iter().flat_map(|p| p.contexts.iter()))
        .filter(|t| t.hart == hart)
        .filter_map(|t| t.cause)
        .collect();
    v.sort();
    v.dedup();
    v
}
