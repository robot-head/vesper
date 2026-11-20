//! The Rust build of [`ImsicFile`] and [`AplicDomain`] (the seed copy's,
//! and the default). The port-backed package uses `port.rs` instead, which
//! forwards to the Vesper port (vesper/src/aia.ves).

use super::*;
use crate::dtb::MemRange;

// Register offsets and bits of the APLIC (AIA §4.5).
const DOMAINCFG: u64 = 0x0000;
const SOURCECFG: u64 = 0x0004;
const MMSIADDRCFG: u64 = 0x1bc0;
const MMSIADDRCFGH: u64 = 0x1bc4;
const SMSIADDRCFG: u64 = 0x1bc8;
const SMSIADDRCFGH: u64 = 0x1bcc;
const SETIP: u64 = 0x1c00;
const SETIPNUM: u64 = 0x1cdc;
const IN_CLRIP: u64 = 0x1d00;
const CLRIPNUM: u64 = 0x1ddc;
const SETIE: u64 = 0x1e00;
const SETIENUM: u64 = 0x1edc;
const CLRIE: u64 = 0x1f00;
const CLRIENUM: u64 = 0x1fdc;
const SETIPNUM_LE: u64 = 0x2000;
const SETIPNUM_BE: u64 = 0x2004;
const GENMSI: u64 = 0x3000;
const TARGET: u64 = 0x3004;
const IDC_BYTES: u64 = 32;
const SOURCECFG_D: u32 = 1 << 10;
const DOMAINCFG_IE: u32 = 1 << 8;
const DOMAINCFG_DM: u32 = 1 << 2;
const MSIADDRCFGH_L: u32 = 1 << MSIADDRCFGH_L_SHIFT;
// `domaincfg` bits 31:24 read as 0x80 (§4.5.1).
const DOMAINCFG_READ_ONE: u32 = 0x8000_0000;
// `sourcecfg[i]` Child Index field when D = 1 (§4.5.2).
const SOURCECFG_CHILD: u32 = 0x3ff;
// Last `*iselect` of the `eip` and `eie` arrays (eip63, eie63; §3.8).
const ISEL_EIP63: u32 = 0xbf;
const ISEL_EIE63: u32 = 0xff;
// `topei`/`topi`/`claimi`: the identity sits at bit 16 (§3.9, §4.8.1.4).
const TOP_ID_SHIFT: u32 = 16;
// `target[i]` fields (§4.5.16): Hart Index 31:18, Guest Index 17:12, EIID
// 10:0 in MSI mode, IPRIO 7:0 in direct mode.
const TARGET_HART_SHIFT: u32 = 18;
const TARGET_HART: u32 = 0xfffc_0000;
const TARGET_GUEST_SHIFT: u32 = 12;
const TARGET_GUEST: u32 = 0x3f;
const TARGET_GUEST_FIELD: u32 = TARGET_GUEST << TARGET_GUEST_SHIFT;
const TARGET_EIID: u32 = 0x7ff;
const TARGET_IPRIO: u32 = 0xff;
// IDC register offsets (§4.8.1).
const IDC_IDELIVERY: u64 = 0x00;
const IDC_IFORCE: u64 = 0x04;
const IDC_ITHRESHOLD: u64 = 0x08;
const IDC_TOPI: u64 = 0x18;
const IDC_CLAIMI: u64 = 0x1c;

/// One IMSIC interrupt file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImsicFile {
    ids: u32,
    /// `eidelivery`: [`EIDELIVERY_OFF`], [`EIDELIVERY_ON`] or
    /// [`EIDELIVERY_EXTERNAL`].
    delivery: u32,
    /// Whether `eidelivery` supports [`EIDELIVERY_EXTERNAL`].
    external: bool,
    threshold: u32,
    eip: Vec<u64>,
    eie: Vec<u64>,
}

impl ImsicFile {
    /// A file with identities `1..=ids` (`riscv,num-ids`).
    pub fn new(ids: u32) -> Result<Self, AiaError> {
        if !(IMSIC_MIN_IDS..=IMSIC_MAX_IDS).contains(&ids) || !(ids + 1).is_multiple_of(64) {
            return Err(AiaError::BadIdCount(ids));
        }
        let words = (ids as usize + 1) / 64;
        Ok(Self {
            ids,
            delivery: EIDELIVERY_OFF,
            external: false,
            threshold: 0,
            eip: vec![0; words],
            eie: vec![0; words],
        })
    }

    /// A file whose `eidelivery` also supports [`EIDELIVERY_EXTERNAL`] (a
    /// machine- or supervisor-level file of a hart that a PLIC or a direct
    /// APLIC IDC also reaches; never a guest file, §3.8.1). It resets to
    /// that value, as the specification requires.
    pub fn with_external_delivery(ids: u32) -> Result<Self, AiaError> {
        let mut f = Self::new(ids)?;
        f.external = true;
        f.delivery = EIDELIVERY_EXTERNAL;
        Ok(f)
    }

    pub fn ids(&self) -> u32 {
        self.ids
    }

    /// The `eidelivery` value.
    pub fn eidelivery(&self) -> u32 {
        self.delivery
    }

    /// Whether `eidelivery` selects the PLIC or APLIC
    /// ([`EIDELIVERY_EXTERNAL`]).
    pub fn delivers_external(&self) -> bool {
        self.delivery == EIDELIVERY_EXTERNAL
    }

    pub fn supports_external_delivery(&self) -> bool {
        self.external
    }

    fn bit(v: &[u64], id: u32) -> bool {
        v.get(id as usize / 64)
            .is_some_and(|w| w >> (id % 64) & 1 != 0)
    }

    fn set(v: &mut [u64], id: u32, on: bool) {
        if let Some(w) = v.get_mut(id as usize / 64) {
            if on {
                *w |= 1 << (id % 64);
            } else {
                *w &= !(1 << (id % 64));
            }
        }
    }

    /// An MSI with data `id` arrived (a write to `seteipnum_le`). Identity 0
    /// and identities above `ids` are ignored, as the specification says.
    pub fn seteipnum(&mut self, id: u32) {
        if id != 0 && id <= self.ids {
            Self::set(&mut self.eip, id, true);
        }
    }

    pub fn is_pending(&self, id: u32) -> bool {
        Self::bit(&self.eip, id)
    }

    pub fn is_enabled(&self, id: u32) -> bool {
        Self::bit(&self.eie, id)
    }

    /// The 32-bit MMIO write of a device or APLIC at offset `off` in the
    /// file's page: `seteipnum_le` (0) or `seteipnum_be` (4). Other offsets
    /// are reserved: writes are ignored, as on hardware.
    pub fn mmio_write32(&mut self, off: u64, v: u32) {
        match off {
            0 => self.seteipnum(v),
            4 => self.seteipnum(v.swap_bytes()),
            _ => {}
        }
    }

    fn word(&self, isel: u32, base: u32) -> Result<usize, AiaError> {
        let k = isel - base;
        // RV64: only even-numbered eip/eie registers exist.
        if !k.is_multiple_of(2) {
            return Err(AiaError::BadIselect(isel));
        }
        Ok((k / 2) as usize)
    }

    /// `*ireg` read with `*iselect = isel`.
    pub fn csr_read(&self, isel: u32) -> Result<u64, AiaError> {
        match isel {
            ISEL_EIDELIVERY => Ok(u64::from(self.delivery)),
            ISEL_EITHRESHOLD => Ok(u64::from(self.threshold)),
            ISEL_EIP0..=ISEL_EIP63 => {
                let w = self.word(isel, ISEL_EIP0)?;
                Ok(self.eip.get(w).copied().unwrap_or(0))
            }
            ISEL_EIE0..=ISEL_EIE63 => {
                let w = self.word(isel, ISEL_EIE0)?;
                Ok(self.eie.get(w).copied().unwrap_or(0))
            }
            _ => Err(AiaError::BadIselect(isel)),
        }
    }

    /// `*ireg` write. `eidelivery` (WARL) takes 0x40000000 exactly when the
    /// file supports it and otherwise keeps bit 0; `eithreshold` keeps the bits
    /// needed for `ids`; bit 0 of `eip0`/`eie0` (identity 0) is read-only 0;
    /// registers beyond `ids` are read-only zero.
    pub fn csr_write(&mut self, isel: u32, v: u64) -> Result<(), AiaError> {
        match isel {
            ISEL_EIDELIVERY => {
                self.delivery = if self.external && v == u64::from(EIDELIVERY_EXTERNAL) {
                    EIDELIVERY_EXTERNAL
                } else {
                    (v & 1) as u32
                }
            }
            ISEL_EITHRESHOLD => {
                let bits = 32 - self.ids.leading_zeros();
                self.threshold = (v as u32) & ((1u32 << bits) - 1);
            }
            ISEL_EIP0..=ISEL_EIP63 => {
                let w = self.word(isel, ISEL_EIP0)?;
                if let Some(r) = self.eip.get_mut(w) {
                    *r = if w == 0 { v & !1 } else { v };
                }
            }
            ISEL_EIE0..=ISEL_EIE63 => {
                let w = self.word(isel, ISEL_EIE0)?;
                if let Some(r) = self.eie.get_mut(w) {
                    *r = if w == 0 { v & !1 } else { v };
                }
            }
            _ => return Err(AiaError::BadIselect(isel)),
        }
        Ok(())
    }

    /// The highest-priority (lowest) identity that is pending and enabled
    /// and below a nonzero `eithreshold`, or 0.
    pub fn top(&self) -> u32 {
        for (w, (&p, &e)) in self.eip.iter().zip(&self.eie).enumerate() {
            let m = p & e;
            if m != 0 {
                let id = w as u32 * 64 + m.trailing_zeros();
                return if self.threshold == 0 || id < self.threshold {
                    id
                } else {
                    0
                };
            }
        }
        0
    }

    /// `*topei` read: identity in bits 26:16 and again as the priority in
    /// bits 10:0, or 0.
    pub fn topei(&self) -> u32 {
        let id = self.top();
        (id << TOP_ID_SHIFT) | id
    }

    /// A write to `*topei` (any value): clears the pending bit of the
    /// identity a read would return and returns that read value (the
    /// `csrrw rd, *topei, x0` claim idiom).
    pub fn claim(&mut self) -> u32 {
        let v = self.topei();
        let id = v >> TOP_ID_SHIFT;
        if id != 0 {
            Self::set(&mut self.eip, id, false);
        }
        v
    }

    /// The file's interrupt output to its hart (or its `hgeip` bit). With
    /// `eidelivery` = 0x40000000 the file acts as though it were 0.
    pub fn asserted(&self) -> bool {
        self.delivery == EIDELIVERY_ON && self.top() != 0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Idc {
    delivery: bool,
    force: bool,
    threshold: u32,
}

/// One APLIC interrupt domain.
#[derive(Clone, Debug)]
pub struct AplicDomain {
    base: u64,
    sources: u32,
    supervisor: bool,
    root: bool,
    msi_capable: bool,
    direct_capable: bool,
    children: u32,
    ie: bool,
    dm: bool,
    sourcecfg: Vec<u32>,
    /// The parent delegates the source here (always true in the root).
    active_here: Vec<bool>,
    raw: Vec<bool>,
    pending: Vec<bool>,
    enabled: Vec<bool>,
    target: Vec<u32>,
    idcs: Vec<Idc>,
    msicfg: MsiAddrCfg,
    outbox: Vec<Msi>,
}

impl AplicDomain {
    /// A domain at `window` with `sources` sources and `idcs` interrupt
    /// delivery controls (direct mode; 0 when the domain has no
    /// `interrupts-extended`). `root` domains hold the MSI address
    /// configuration; `supervisor` domains deliver to S-level files.
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
        if sources == 0 || sources > APLIC_MAX_SOURCES {
            return Err(AiaError::BadSourceCount(sources));
        }
        if idcs == 0 && !msi_capable {
            return Err(AiaError::NoDeliveryMode {
                domain: path.to_string(),
            });
        }
        let needed = APLIC_IDC + IDC_BYTES * idcs as u64;
        if window.size < needed {
            return Err(AiaError::WindowTooSmall {
                size: window.size,
                needed,
            });
        }
        let n = sources as usize + 1;
        Ok(Self {
            base: window.base,
            sources,
            supervisor,
            root,
            msi_capable,
            direct_capable: idcs > 0,
            children,
            ie: false,
            dm: msi_capable && idcs == 0,
            sourcecfg: vec![0; n],
            active_here: vec![root; n],
            raw: vec![false; n],
            pending: vec![false; n],
            enabled: vec![false; n],
            target: vec![0; n],
            idcs: vec![Idc::default(); idcs],
            msicfg: MsiAddrCfg::default(),
            outbox: Vec::new(),
        })
    }

    pub fn base(&self) -> u64 {
        self.base
    }

    pub fn sources(&self) -> u32 {
        self.sources
    }

    pub fn is_root(&self) -> bool {
        self.root
    }

    pub fn is_supervisor(&self) -> bool {
        self.supervisor
    }

    /// MSI delivery mode (`domaincfg.DM`).
    pub fn msi_mode(&self) -> bool {
        self.dm
    }

    pub fn idc_count(&self) -> usize {
        self.idcs.len()
    }

    /// The window the registers occupy.
    pub fn contains(&self, addr: u64) -> bool {
        let end = APLIC_IDC + IDC_BYTES * self.idcs.len() as u64;
        addr >= self.base && addr - self.base < end.max(APLIC_IDC)
    }

    /// The source mode in this domain (Inactive when delegated onwards or
    /// not delegated here).
    pub fn mode(&self, src: u32) -> SourceMode {
        let s = src as usize;
        if src == 0 || src > self.sources || !self.active_here[s] {
            return SourceMode::Inactive;
        }
        let cfg = self.sourcecfg[s];
        if cfg & SOURCECFG_D != 0 {
            SourceMode::Inactive
        } else {
            SourceMode::of(cfg)
        }
    }

    /// The child index source `src` is delegated to, if any.
    pub fn delegated_to(&self, src: u32) -> Option<u32> {
        let s = src as usize;
        if src == 0 || src > self.sources || !self.active_here[s] {
            return None;
        }
        let cfg = self.sourcecfg[s];
        (cfg & SOURCECFG_D != 0).then_some(cfg & SOURCECFG_CHILD)
    }

    /// Whether `src` belongs to this domain: always in the root, else when
    /// the parent delegates it here.
    pub fn is_delegated_here(&self, src: u32) -> bool {
        src != 0 && src <= self.sources && self.active_here[src as usize]
    }

    /// Whether the parent delegates `src` to this domain (fabric use).
    pub fn set_active_here(&mut self, src: u32, active: bool) {
        if let Some(a) = self.active_here.get_mut(src as usize)
            && src != 0
            && *a != active
        {
            *a = active;
            if !active {
                self.sourcecfg[src as usize] = 0;
            }
            self.settle(src as usize);
        }
    }

    pub fn is_pending(&self, src: u32) -> bool {
        self.pending.get(src as usize).copied().unwrap_or(false)
    }

    pub fn rectified(&self, src: u32) -> bool {
        let s = src as usize;
        s < self.raw.len() && self.mode(src).rectify(self.raw[s])
    }

    /// Re-establishes the invariants after a mode or delegation change:
    /// inactive sources have no pending or enable bit and no target; level
    /// sources in direct mode track the rectified input; in MSI mode a low
    /// level clears the bit.
    fn settle(&mut self, s: usize) {
        let mode = self.mode(s as u32);
        if mode == SourceMode::Inactive {
            self.pending[s] = false;
            self.enabled[s] = false;
            self.target[s] = 0;
            return;
        }
        if mode.level() {
            let r = mode.rectify(self.raw[s]);
            if !self.dm {
                self.pending[s] = r;
            } else if !r {
                self.pending[s] = false;
            }
        }
    }

    /// The wire of source `src` changed to `high` (fabric use: the wire
    /// reaches every domain of the tree; only the one where the source is
    /// active acts on it).
    pub fn set_wire(&mut self, src: u32, high: bool) {
        let s = src as usize;
        if src == 0 || src > self.sources {
            return;
        }
        let mode = self.mode(src);
        let before = mode.rectify(self.raw[s]);
        self.raw[s] = high;
        let after = mode.rectify(high);
        match mode {
            SourceMode::Inactive | SourceMode::Detached => {}
            SourceMode::EdgeRising | SourceMode::EdgeFalling => {
                if !before && after {
                    self.pending[s] = true;
                }
            }
            SourceMode::LevelHigh | SourceMode::LevelLow => {
                if !self.dm {
                    self.pending[s] = after;
                } else if !after {
                    self.pending[s] = false;
                } else if !before {
                    self.pending[s] = true;
                }
            }
        }
    }

    /// A `setip`/`setipnum` write for `src`.
    fn set_pending(&mut self, src: u32) {
        let s = src as usize;
        match self.mode(src) {
            SourceMode::Inactive => {}
            SourceMode::Detached | SourceMode::EdgeRising | SourceMode::EdgeFalling => {
                self.pending[s] = true;
            }
            SourceMode::LevelHigh | SourceMode::LevelLow => {
                if self.dm && self.rectified(src) {
                    self.pending[s] = true;
                }
            }
        }
    }

    /// An `in_clrip`/`clripnum` write for `src`.
    fn clear_pending(&mut self, src: u32) {
        let s = src as usize;
        match self.mode(src) {
            SourceMode::Inactive => {}
            m if m.level() && !self.dm => {}
            _ => self.pending[s] = false,
        }
    }

    fn set_enable(&mut self, src: u32, on: bool) {
        if self.mode(src) != SourceMode::Inactive {
            self.enabled[src as usize] = on;
        }
    }

    fn word_bits(&self, w: u64, f: impl Fn(u32) -> bool) -> u32 {
        (0..32u32)
            .filter(|b| {
                let s = w as u32 * 32 + b;
                s != 0 && s <= self.sources && f(s)
            })
            .fold(0, |acc, b| acc | (1 << b))
    }

    fn words(&self) -> u64 {
        u64::from(self.sources) / 32 + 1
    }

    /// The root's MSI address configuration (also the copy a child uses).
    pub fn msi_cfg(&self) -> MsiAddrCfg {
        self.msicfg
    }

    /// Fabric use: a child computes its MSI addresses with the root's
    /// configuration.
    pub fn set_msi_cfg(&mut self, cfg: MsiAddrCfg) {
        self.msicfg = cfg;
    }

    /// The direct-mode IDC `topi` value: identity in 25:16, priority 7:0.
    pub fn topi(&self, idc: usize) -> u32 {
        let Some(c) = self.idcs.get(idc) else {
            return 0;
        };
        if self.dm {
            return 0;
        }
        let mut best: Option<(u32, u32)> = None;
        for s in 1..=self.sources {
            let i = s as usize;
            if !self.pending[i] || !self.enabled[i] {
                continue;
            }
            let t = self.target[i];
            if (t >> TARGET_HART_SHIFT) as usize != idc {
                continue;
            }
            let prio = t & TARGET_IPRIO;
            if c.threshold != 0 && prio >= c.threshold {
                continue;
            }
            if best.is_none_or(|(bp, _)| prio < bp) {
                best = Some((prio, s));
            }
        }
        best.map_or(0, |(p, s)| (s << TOP_ID_SHIFT) | p)
    }

    /// `claimi` read.
    pub fn claimi(&mut self, idc: usize) -> u32 {
        let v = self.topi(idc);
        if v == 0 {
            if let Some(c) = self.idcs.get_mut(idc) {
                c.force = false;
            }
            return 0;
        }
        let s = (v >> TOP_ID_SHIFT) as usize;
        let mode = self.mode(s as u32);
        if mode.level() {
            // Direct mode: the bit is the rectified input.
            self.pending[s] = mode.rectify(self.raw[s]);
        } else {
            self.pending[s] = false;
        }
        v
    }

    /// The IDC's interrupt output.
    pub fn idc_asserted(&self, idc: usize) -> bool {
        self.idcs
            .get(idc)
            .is_some_and(|c| self.ie && !self.dm && c.delivery && (c.force || self.topi(idc) != 0))
    }

    /// Forwards every pending, enabled source as an MSI (MSI mode with IE),
    /// clearing its pending bit, and returns the MSIs produced since the
    /// last call (including `genmsi`).
    pub fn take_msis(&mut self) -> Vec<Msi> {
        if self.dm && self.ie {
            for s in 1..=self.sources as usize {
                if self.pending[s] && self.enabled[s] {
                    let t = self.target[s];
                    let hart = t >> TARGET_HART_SHIFT;
                    let guest = (t >> TARGET_GUEST_SHIFT) & TARGET_GUEST;
                    let addr = self.msicfg.address(self.supervisor, hart, guest);
                    self.outbox.push(Msi {
                        addr,
                        data: t & TARGET_EIID,
                    });
                    self.pending[s] = false;
                }
            }
        }
        std::mem::take(&mut self.outbox)
    }

    fn decode(&self, addr: u64) -> Result<u64, AiaError> {
        if !addr.is_multiple_of(4) {
            return Err(AiaError::Misaligned { addr });
        }
        if !self.contains(addr) {
            return Err(AiaError::Unmapped { addr });
        }
        Ok(addr - self.base)
    }

    fn num(&self, v: u32) -> Option<u32> {
        (v != 0 && v <= self.sources).then_some(v)
    }

    /// A 32-bit MMIO read.
    pub fn read32(&mut self, addr: u64) -> Result<u32, AiaError> {
        let off = self.decode(addr)?;
        let w = |base: u64| (off - base) / 4;
        Ok(match off {
            DOMAINCFG => {
                DOMAINCFG_READ_ONE
                    | if self.ie { DOMAINCFG_IE } else { 0 }
                    | if self.dm { DOMAINCFG_DM } else { 0 }
            }
            o if (SOURCECFG..SOURCECFG + 4 * u64::from(self.sources)).contains(&o) => {
                let s = (w(SOURCECFG) + 1) as usize;
                if self.active_here[s] {
                    self.sourcecfg[s]
                } else {
                    0
                }
            }
            MMSIADDRCFG if self.root && self.msi_capable => self.msicfg.m_ppn as u32,
            MMSIADDRCFGH if self.root && self.msi_capable => self.msicfg.high_m(),
            SMSIADDRCFG if self.root && self.msi_capable => self.msicfg.s_ppn as u32,
            SMSIADDRCFGH if self.root && self.msi_capable => self.msicfg.high_s(),
            MMSIADDRCFG..=SMSIADDRCFGH => 0,
            o if (SETIP..SETIP + 4 * self.words()).contains(&o) => {
                self.word_bits(w(SETIP), |s| self.pending[s as usize])
            }
            o if (IN_CLRIP..IN_CLRIP + 4 * self.words()).contains(&o) => {
                self.word_bits(w(IN_CLRIP), |s| self.rectified(s))
            }
            o if (SETIE..SETIE + 4 * self.words()).contains(&o) => {
                self.word_bits(w(SETIE), |s| self.enabled[s as usize])
            }
            SETIPNUM | CLRIPNUM | SETIENUM | CLRIENUM | SETIPNUM_LE | SETIPNUM_BE => 0,
            o if (CLRIE..CLRIE + 4 * self.words()).contains(&o) => 0,
            GENMSI => 0,
            o if (TARGET..TARGET + 4 * u64::from(self.sources)).contains(&o) => {
                self.target[(w(TARGET) + 1) as usize]
            }
            o if o >= APLIC_IDC && self.direct_capable => {
                let i = ((o - APLIC_IDC) / IDC_BYTES) as usize;
                match (o - APLIC_IDC) % IDC_BYTES {
                    IDC_IDELIVERY => u32::from(self.idcs[i].delivery),
                    IDC_IFORCE => u32::from(self.idcs[i].force),
                    IDC_ITHRESHOLD => self.idcs[i].threshold,
                    IDC_TOPI => self.topi(i),
                    IDC_CLAIMI => self.claimi(i),
                    _ => return Err(AiaError::Unmapped { addr }),
                }
            }
            _ => return Err(AiaError::Unmapped { addr }),
        })
    }

    /// A 32-bit MMIO write.
    pub fn write32(&mut self, addr: u64, v: u32) -> Result<(), AiaError> {
        let off = self.decode(addr)?;
        let w = |base: u64| (off - base) / 4;
        match off {
            DOMAINCFG => {
                self.ie = v & DOMAINCFG_IE != 0;
                if self.msi_capable && self.direct_capable {
                    self.dm = v & DOMAINCFG_DM != 0;
                    for s in 1..=self.sources as usize {
                        self.settle(s);
                    }
                }
            }
            o if (SOURCECFG..SOURCECFG + 4 * u64::from(self.sources)).contains(&o) => {
                let s = (w(SOURCECFG) + 1) as usize;
                if !self.active_here[s] {
                    return Ok(());
                }
                self.sourcecfg[s] = if v & SOURCECFG_D != 0 && self.children > 0 {
                    SOURCECFG_D | (v & SOURCECFG_CHILD)
                } else {
                    SourceMode::of(v).bits()
                };
                self.settle(s);
            }
            MMSIADDRCFG..=SMSIADDRCFGH => {
                if !self.root || !self.msi_capable || self.msicfg.locked {
                    return Ok(());
                }
                let c = &mut self.msicfg;
                match off {
                    MMSIADDRCFG => c.m_ppn = (c.m_ppn & !u64::from(u32::MAX)) | u64::from(v),
                    MMSIADDRCFGH => {
                        c.m_ppn = (c.m_ppn & u64::from(u32::MAX))
                            | (u64::from(v & MSIADDRCFGH_PPN) << 32);
                        c.lhxw = (v >> MSIADDRCFGH_LHXW_SHIFT) & MSIADDRCFGH_LHXW;
                        c.hhxw = (v >> MSIADDRCFGH_HHXW_SHIFT) & MSIADDRCFGH_HHXW;
                        c.lhxs_m = (v >> MSIADDRCFGH_LHXS_SHIFT) & MSIADDRCFGH_LHXS;
                        c.hhxs = (v >> MSIADDRCFGH_HHXS_SHIFT) & MSIADDRCFGH_HHXS;
                        c.locked = v & MSIADDRCFGH_L != 0;
                    }
                    SMSIADDRCFG => c.s_ppn = (c.s_ppn & !u64::from(u32::MAX)) | u64::from(v),
                    _ => {
                        c.s_ppn = (c.s_ppn & u64::from(u32::MAX))
                            | (u64::from(v & MSIADDRCFGH_PPN) << 32);
                        c.lhxs_s = (v >> MSIADDRCFGH_LHXS_SHIFT) & MSIADDRCFGH_LHXS;
                    }
                }
            }
            o if (SETIP..SETIP + 4 * self.words()).contains(&o) => {
                for b in 0..32 {
                    if v >> b & 1 != 0
                        && let Some(s) = self.num(w(SETIP) as u32 * 32 + b)
                    {
                        self.set_pending(s);
                    }
                }
            }
            SETIPNUM | SETIPNUM_LE => {
                if let Some(s) = self.num(v) {
                    self.set_pending(s);
                }
            }
            SETIPNUM_BE => {
                if let Some(s) = self.num(v.swap_bytes()) {
                    self.set_pending(s);
                }
            }
            o if (IN_CLRIP..IN_CLRIP + 4 * self.words()).contains(&o) => {
                for b in 0..32 {
                    if v >> b & 1 != 0
                        && let Some(s) = self.num(w(IN_CLRIP) as u32 * 32 + b)
                    {
                        self.clear_pending(s);
                    }
                }
            }
            CLRIPNUM => {
                if let Some(s) = self.num(v) {
                    self.clear_pending(s);
                }
            }
            o if (SETIE..SETIE + 4 * self.words()).contains(&o) => {
                for b in 0..32 {
                    if v >> b & 1 != 0
                        && let Some(s) = self.num(w(SETIE) as u32 * 32 + b)
                    {
                        self.set_enable(s, true);
                    }
                }
            }
            SETIENUM => {
                if let Some(s) = self.num(v) {
                    self.set_enable(s, true);
                }
            }
            o if (CLRIE..CLRIE + 4 * self.words()).contains(&o) => {
                for b in 0..32 {
                    if v >> b & 1 != 0
                        && let Some(s) = self.num(w(CLRIE) as u32 * 32 + b)
                    {
                        self.set_enable(s, false);
                    }
                }
            }
            CLRIENUM => {
                if let Some(s) = self.num(v) {
                    self.set_enable(s, false);
                }
            }
            GENMSI => {
                if self.dm {
                    let hart = v >> TARGET_HART_SHIFT;
                    let addr = self.msicfg.address(self.supervisor, hart, 0);
                    self.outbox.push(Msi {
                        addr,
                        data: v & TARGET_EIID,
                    });
                }
            }
            o if (TARGET..TARGET + 4 * u64::from(self.sources)).contains(&o) => {
                let s = (w(TARGET) + 1) as usize;
                if self.mode(s as u32) == SourceMode::Inactive {
                    return Ok(());
                }
                self.target[s] = if self.dm {
                    let guest = if self.supervisor {
                        v & TARGET_GUEST_FIELD
                    } else {
                        0
                    };
                    (v & TARGET_HART) | guest | (v & TARGET_EIID)
                } else {
                    let prio = match v & TARGET_IPRIO {
                        0 => 1,
                        p => p,
                    };
                    (v & TARGET_HART) | prio
                };
            }
            o if o >= APLIC_IDC && self.direct_capable => {
                let i = ((o - APLIC_IDC) / IDC_BYTES) as usize;
                match (o - APLIC_IDC) % IDC_BYTES {
                    IDC_IDELIVERY => self.idcs[i].delivery = v & 1 != 0,
                    IDC_IFORCE => self.idcs[i].force = v & 1 != 0,
                    IDC_ITHRESHOLD => self.idcs[i].threshold = v & ((1 << APLIC_IPRIOLEN) - 1),
                    IDC_TOPI | IDC_CLAIMI => {}
                    _ => return Err(AiaError::Unmapped { addr }),
                }
            }
            _ => return Err(AiaError::Unmapped { addr }),
        }
        Ok(())
    }
}
