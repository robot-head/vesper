//! Flattened Devicetree (DTB) input for memory and interrupt-controller
//! nodes (§26.1 items 3 and 5).
//!
//! [`DeviceTree::parse`] reads a DTB blob (format version 17, the one `dtc`
//! and QEMU emit) into a node tree. Every length, offset, alignment, string
//! terminator, token and nesting level is checked; malformed or over-budget
//! input is a typed [`DtbError`], never a panic. The parser is written here
//! rather than taken from the `fdt` crate because `fdt` 0.1.5 `expect`s and
//! `assert`s on malformed structure blocks, which this crate's untrusted-input
//! rule does not allow.
//!
//! [`Platform::from_tree`] then extracts what the Silicon models consume:
//!
//! * memory banks (`device_type = "memory"`), the header's `/memreserve/`
//!   entries and `/reserved-memory` children, and the 4 KiB frame ranges that
//!   remain ([`Platform::usable_frame_ranges`]), which seed
//!   [`FrameAllocator`]s for the frame-conservation model;
//! * interrupt controllers (RISC-V hart-local `riscv,cpu-intc`, PLIC, GICv2
//!   and GICv3) and every device interrupt resolved through
//!   `interrupt-parent` / `interrupts` / `interrupts-extended` to a
//!   controller-global number, range-checked against the controller
//!   (`riscv,ndev` for a PLIC, the SPI/PPI ranges for a GIC), which install
//!   ISRs on the IrqGuard [`Cpu`] model ([`Platform::install_isr`]);
//! * the per-hart `riscv,isa` / `riscv,cbom-block-size` and each device's
//!   `dma-coherent` / `dma-noncoherent` marking, which select the Zicbom
//!   cache-block maintenance of the DMA model ([`Platform::dma_coherence`]).
//!
//! Not supported (typed errors where they matter): interrupt nexus nodes
//! (`interrupt-map`), address translation of device `reg` through bus
//! `ranges`, `#address-cells`/`#size-cells` above 2, and dynamically placed
//! `/reserved-memory` regions (`size` without `reg`).

use std::collections::BTreeMap;

use crate::Arch;
pub use crate::dma::DmaCoherence;
use crate::frames::{FrameAllocator, FrameError, PAGE_BYTES};
use crate::irq::{Cpu, IsrCtx};
use crate::ledger::Ledger;

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;
const HEADER_BYTES: usize = 40;

/// Parser budgets. Real trees are far below these.
pub const MAX_DEPTH: usize = 64;
pub const MAX_NODES: usize = 65_536;
pub const MAX_RESERVATIONS: usize = 1024;
/// Most 4 KiB frames [`Platform::frame_allocators`] will model (16 GiB).
pub const MAX_MODEL_FRAMES: u64 = 1 << 22;

/// A physical address range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MemRange {
    pub base: u64,
    pub size: u64,
}

impl MemRange {
    /// Exclusive end, or `None` when it does not fit in 64 bits.
    pub fn end(&self) -> Option<u64> {
        self.base.checked_add(self.size)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prop {
    pub name: String,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Name with unit address, e.g. `memory@80000000`; empty for the root.
    pub name: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub props: Vec<Prop>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DtbError {
    Truncated {
        what: &'static str,
    },
    BadMagic(u32),
    UnsupportedVersion {
        version: u32,
        last_compatible: u32,
    },
    BlockOutOfBounds {
        block: &'static str,
    },
    Misaligned {
        what: &'static str,
    },
    BadToken {
        offset: usize,
        token: u32,
    },
    UnterminatedString {
        offset: usize,
    },
    NotUtf8 {
        offset: usize,
    },
    TooDeep,
    TooManyNodes,
    TooManyReservations,
    /// A property outside any node, a second root, a missing `FDT_END`, or
    /// unbalanced begin/end tokens.
    BadStructure(&'static str),
    BadProperty {
        node: String,
        prop: String,
        detail: String,
    },
    DuplicatePhandle(u32),
    UnsupportedCells {
        node: String,
        cells: u32,
    },
    AddressOverflow {
        node: String,
    },
    OverlappingMemory {
        a: MemRange,
        b: MemRange,
    },
    MissingInterruptParent {
        node: String,
    },
    UnknownPhandle {
        node: String,
        phandle: u32,
    },
    NotAnInterruptController {
        node: String,
        controller: String,
    },
    /// The interrupt parent is a nexus (`interrupt-map`), which is not
    /// modelled.
    UnsupportedInterruptNexus {
        node: String,
        nexus: String,
    },
    BadInterruptSpecifier {
        node: String,
        detail: String,
    },
    IrqOutOfRange {
        node: String,
        controller: String,
        irq: u32,
        limit: u32,
    },
    /// A `/reserved-memory` child with `size` but no `reg`: firmware places
    /// it, so the usable frames are not known from the tree alone.
    DynamicReservation {
        node: String,
    },
    UnknownArch {
        compatible: String,
    },
    NoSuchNode(String),
    NoInterrupt {
        node: String,
    },
    /// A non-coherent device on a RISC-V platform whose harts do not all
    /// implement Zicbom with one `riscv,cbom-block-size`.
    NoCacheBlockManagement {
        node: String,
        detail: String,
    },
    Frames(FrameError),
    /// The usable memory exceeds [`MAX_MODEL_FRAMES`].
    TooManyFrames {
        frames: u64,
    },
}

impl std::fmt::Display for DtbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for DtbError {}

fn be32(b: &[u8], at: usize, what: &'static str) -> Result<u32, DtbError> {
    let s = b
        .get(at..at.checked_add(4).ok_or(DtbError::Truncated { what })?)
        .ok_or(DtbError::Truncated { what })?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be64(b: &[u8], at: usize, what: &'static str) -> Result<u64, DtbError> {
    let hi = be32(b, at, what)?;
    let lo = be32(
        b,
        at.checked_add(4).ok_or(DtbError::Truncated { what })?,
        what,
    )?;
    Ok((u64::from(hi) << 32) | u64::from(lo))
}

/// NUL-terminated UTF-8 string starting at `at` in `b` (offsets in errors
/// are relative to `base`).
fn cstr(b: &[u8], at: usize, base: usize) -> Result<&str, DtbError> {
    let rest = b
        .get(at..)
        .ok_or(DtbError::UnterminatedString { offset: base + at })?;
    let n = rest
        .iter()
        .position(|&c| c == 0)
        .ok_or(DtbError::UnterminatedString { offset: base + at })?;
    std::str::from_utf8(&rest[..n]).map_err(|_| DtbError::NotUtf8 { offset: base + at })
}

fn align4(n: usize) -> Option<usize> {
    n.checked_add(3).map(|v| v & !3)
}

/// A parsed devicetree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceTree {
    pub boot_cpuid: u32,
    /// The header's memory reservation block (`/memreserve/`).
    pub reservations: Vec<MemRange>,
    nodes: Vec<Node>,
    phandles: BTreeMap<u32, usize>,
}

impl DeviceTree {
    /// Parses a version-17 DTB. Bytes after `totalsize` are ignored.
    pub fn parse(blob: &[u8]) -> Result<Self, DtbError> {
        if blob.len() < HEADER_BYTES {
            return Err(DtbError::Truncated { what: "header" });
        }
        let h = |i: usize| be32(blob, i * 4, "header");
        let magic = h(0)?;
        if magic != FDT_MAGIC {
            return Err(DtbError::BadMagic(magic));
        }
        let total = h(1)? as usize;
        let off_struct = h(2)? as usize;
        let off_strings = h(3)? as usize;
        let off_rsv = h(4)? as usize;
        let version = h(5)?;
        let last_compatible = h(6)?;
        let boot_cpuid = h(7)?;
        let size_strings = h(8)? as usize;
        let size_struct = h(9)? as usize;
        if version < 17 || last_compatible > 17 {
            return Err(DtbError::UnsupportedVersion {
                version,
                last_compatible,
            });
        }
        if total < HEADER_BYTES || total > blob.len() {
            return Err(DtbError::Truncated { what: "totalsize" });
        }
        let blob = &blob[..total];
        let block = |off: usize, size: usize, name: &'static str| -> Result<&[u8], DtbError> {
            let end = off
                .checked_add(size)
                .ok_or(DtbError::BlockOutOfBounds { block: name })?;
            if off < HEADER_BYTES || end > total {
                return Err(DtbError::BlockOutOfBounds { block: name });
            }
            Ok(&blob[off..end])
        };
        if off_struct % 4 != 0 {
            return Err(DtbError::Misaligned {
                what: "structure block",
            });
        }
        if off_rsv % 8 != 0 {
            return Err(DtbError::Misaligned {
                what: "memory reservation block",
            });
        }
        let structs = block(off_struct, size_struct, "structure")?;
        let strings = block(off_strings, size_strings, "strings")?;

        // Memory reservation block: (u64 address, u64 size) pairs up to a
        // (0, 0) terminator, all inside the blob.
        let mut reservations = Vec::new();
        if off_rsv < HEADER_BYTES {
            return Err(DtbError::BlockOutOfBounds {
                block: "memory reservation",
            });
        }
        let mut at = off_rsv;
        loop {
            let base = be64(blob, at, "memory reservation")?;
            let size = be64(blob, at + 8, "memory reservation")?;
            at += 16;
            if base == 0 && size == 0 {
                break;
            }
            if reservations.len() == MAX_RESERVATIONS {
                return Err(DtbError::TooManyReservations);
            }
            reservations.push(MemRange { base, size });
        }

        let mut nodes: Vec<Node> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        let mut i = 0usize;
        let mut root_done = false;
        loop {
            let token = be32(structs, i, "structure token")?;
            let tok_at = i;
            i += 4;
            match token {
                FDT_NOP => {}
                FDT_BEGIN_NODE => {
                    if root_done {
                        return Err(DtbError::BadStructure("second root node"));
                    }
                    let name = cstr(structs, i, off_struct)?.to_string();
                    i = align4(i + name.len() + 1)
                        .ok_or(DtbError::Truncated { what: "node name" })?;
                    if stack.len() == MAX_DEPTH {
                        return Err(DtbError::TooDeep);
                    }
                    if nodes.len() == MAX_NODES {
                        return Err(DtbError::TooManyNodes);
                    }
                    let parent = stack.last().copied();
                    if parent.is_none() && !name.is_empty() {
                        return Err(DtbError::BadStructure("root node has a name"));
                    }
                    let id = nodes.len();
                    nodes.push(Node {
                        name,
                        parent,
                        children: Vec::new(),
                        props: Vec::new(),
                    });
                    if let Some(p) = parent {
                        nodes[p].children.push(id);
                    }
                    stack.push(id);
                }
                FDT_END_NODE => {
                    if stack.pop().is_none() {
                        return Err(DtbError::BadStructure("unbalanced end node"));
                    }
                    if stack.is_empty() {
                        root_done = true;
                    }
                }
                FDT_PROP => {
                    let len = be32(structs, i, "property header")? as usize;
                    let nameoff = be32(structs, i + 4, "property header")? as usize;
                    i += 8;
                    let end = i.checked_add(len).filter(|&e| e <= structs.len()).ok_or(
                        DtbError::Truncated {
                            what: "property value",
                        },
                    )?;
                    let value = structs[i..end].to_vec();
                    i = align4(end).ok_or(DtbError::Truncated {
                        what: "property value",
                    })?;
                    if nameoff >= strings.len() {
                        return Err(DtbError::BlockOutOfBounds {
                            block: "property name",
                        });
                    }
                    let name = cstr(strings, nameoff, off_strings)?.to_string();
                    let Some(&node) = stack.last() else {
                        return Err(DtbError::BadStructure("property outside a node"));
                    };
                    nodes[node].props.push(Prop { name, value });
                }
                FDT_END => {
                    if !stack.is_empty() || !root_done {
                        return Err(DtbError::BadStructure("FDT_END inside a node"));
                    }
                    break;
                }
                other => {
                    return Err(DtbError::BadToken {
                        offset: off_struct + tok_at,
                        token: other,
                    });
                }
            }
        }
        if nodes.is_empty() {
            return Err(DtbError::BadStructure("no root node"));
        }
        let mut tree = Self {
            boot_cpuid,
            reservations,
            nodes,
            phandles: BTreeMap::new(),
        };
        for id in 0..tree.nodes.len() {
            for key in ["phandle", "linux,phandle"] {
                if let Some(ph) = tree.prop_u32(id, key)? {
                    match tree.phandles.insert(ph, id) {
                        Some(prev) if prev != id => return Err(DtbError::DuplicatePhandle(ph)),
                        _ => {}
                    }
                }
            }
        }
        Ok(tree)
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn node(&self, id: usize) -> &Node {
        &self.nodes[id]
    }

    /// Absolute path of a node (`/` for the root).
    pub fn path(&self, id: usize) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        while let Some(n) = cur {
            if self.nodes[n].parent.is_some() {
                parts.push(self.nodes[n].name.as_str());
            }
            cur = self.nodes[n].parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    /// Looks up an absolute path. A component without `@` also matches a
    /// node whose name is `component@unit` when exactly one such child exists.
    pub fn find(&self, path: &str) -> Option<usize> {
        let mut cur = 0usize;
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            let kids = &self.nodes[cur].children;
            cur = match kids.iter().find(|&&k| self.nodes[k].name == comp) {
                Some(&k) => k,
                None if !comp.contains('@') => {
                    let mut m = kids
                        .iter()
                        .filter(|&&k| self.nodes[k].name.split('@').next() == Some(comp));
                    let first = *m.next()?;
                    if m.next().is_some() {
                        return None;
                    }
                    first
                }
                None => return None,
            };
        }
        Some(cur)
    }

    pub fn by_phandle(&self, phandle: u32) -> Option<usize> {
        self.phandles.get(&phandle).copied()
    }

    pub fn prop(&self, id: usize, name: &str) -> Option<&[u8]> {
        self.nodes[id]
            .props
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value.as_slice())
    }

    pub fn has_prop(&self, id: usize, name: &str) -> bool {
        self.prop(id, name).is_some()
    }

    fn bad(&self, id: usize, prop: &str, detail: impl Into<String>) -> DtbError {
        DtbError::BadProperty {
            node: self.path(id),
            prop: prop.to_string(),
            detail: detail.into(),
        }
    }

    /// The property as big-endian 32-bit cells.
    pub fn prop_cells(&self, id: usize, name: &str) -> Result<Option<Vec<u32>>, DtbError> {
        let Some(v) = self.prop(id, name) else {
            return Ok(None);
        };
        if v.len() % 4 != 0 {
            return Err(self.bad(id, name, format!("{} bytes is not whole cells", v.len())));
        }
        Ok(Some(
            v.chunks_exact(4)
                .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        ))
    }

    pub fn prop_u32(&self, id: usize, name: &str) -> Result<Option<u32>, DtbError> {
        match self.prop_cells(id, name)? {
            None => Ok(None),
            Some(c) if c.len() == 1 => Ok(Some(c[0])),
            Some(c) => Err(self.bad(id, name, format!("{} cells, expected 1", c.len()))),
        }
    }

    /// A string-list property (`compatible`, `status`, ...).
    pub fn prop_strings(&self, id: usize, name: &str) -> Result<Option<Vec<String>>, DtbError> {
        let Some(v) = self.prop(id, name) else {
            return Ok(None);
        };
        if v.last() != Some(&0) {
            return Err(self.bad(id, name, "not NUL-terminated"));
        }
        v[..v.len() - 1]
            .split(|&b| b == 0)
            .map(|s| {
                std::str::from_utf8(s)
                    .map(str::to_string)
                    .map_err(|_| self.bad(id, name, "not UTF-8"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    pub fn prop_str(&self, id: usize, name: &str) -> Result<Option<String>, DtbError> {
        Ok(self
            .prop_strings(id, name)?
            .and_then(|v| v.into_iter().next()))
    }

    pub fn compatible(&self, id: usize) -> Result<Vec<String>, DtbError> {
        Ok(self.prop_strings(id, "compatible")?.unwrap_or_default())
    }

    /// `status` is absent, `okay` or `ok`.
    pub fn enabled(&self, id: usize) -> Result<bool, DtbError> {
        Ok(matches!(
            self.prop_str(id, "status")?.as_deref(),
            None | Some("okay") | Some("ok")
        ))
    }

    /// `#address-cells` / `#size-cells` that apply to `id`'s `reg` (the
    /// parent's, defaulting to 2 and 1).
    pub fn reg_cells(&self, id: usize) -> Result<(u32, u32), DtbError> {
        let Some(p) = self.nodes[id].parent else {
            return Ok((2, 1));
        };
        let a = self.prop_u32(p, "#address-cells")?.unwrap_or(2);
        let s = self.prop_u32(p, "#size-cells")?.unwrap_or(1);
        for c in [a, s] {
            if c > 2 {
                return Err(DtbError::UnsupportedCells {
                    node: self.path(p),
                    cells: c,
                });
            }
        }
        if a == 0 {
            return Err(DtbError::UnsupportedCells {
                node: self.path(p),
                cells: 0,
            });
        }
        Ok((a, s))
    }

    /// The node's `reg` entries in its parent bus's address space (no
    /// `ranges` translation).
    pub fn reg(&self, id: usize) -> Result<Vec<MemRange>, DtbError> {
        let Some(cells) = self.prop_cells(id, "reg")? else {
            return Ok(Vec::new());
        };
        let (a, s) = self.reg_cells(id)?;
        let per = (a + s) as usize;
        if cells.is_empty() || cells.len() % per != 0 {
            return Err(self.bad(
                id,
                "reg",
                format!("{} cells is not a multiple of {a}+{s}", cells.len()),
            ));
        }
        let join = |c: &[u32]| c.iter().fold(0u64, |v, &x| (v << 32) | u64::from(x));
        let mut out = Vec::new();
        for e in cells.chunks_exact(per) {
            let r = MemRange {
                base: join(&e[..a as usize]),
                size: join(&e[a as usize..]),
            };
            if r.end().is_none() {
                return Err(DtbError::AddressOverflow {
                    node: self.path(id),
                });
            }
            out.push(r);
        }
        Ok(out)
    }

    /// The node `interrupt-parent` designates for `id` (inherited from the
    /// nearest ancestor that sets it).
    pub fn interrupt_parent(&self, id: usize) -> Result<usize, DtbError> {
        let mut cur = Some(id);
        while let Some(n) = cur {
            if let Some(ph) = self.prop_u32(n, "interrupt-parent")? {
                return self.by_phandle(ph).ok_or(DtbError::UnknownPhandle {
                    node: self.path(id),
                    phandle: ph,
                });
            }
            cur = self.nodes[n].parent;
        }
        Err(DtbError::MissingInterruptParent {
            node: self.path(id),
        })
    }

    /// `#interrupt-cells` of a controller referenced by `user`.
    fn controller_cells(&self, user: usize, ctl: usize) -> Result<u32, DtbError> {
        if self.has_prop(ctl, "interrupt-map") {
            return Err(DtbError::UnsupportedInterruptNexus {
                node: self.path(user),
                nexus: self.path(ctl),
            });
        }
        if !self.has_prop(ctl, "interrupt-controller") {
            return Err(DtbError::NotAnInterruptController {
                node: self.path(user),
                controller: self.path(ctl),
            });
        }
        let cells = self
            .prop_u32(ctl, "#interrupt-cells")?
            .ok_or_else(|| self.bad(ctl, "#interrupt-cells", "missing"))?;
        if !(1..=4).contains(&cells) {
            return Err(self.bad(ctl, "#interrupt-cells", format!("{cells} cells")));
        }
        Ok(cells)
    }

    /// The node's interrupt specifiers as `(controller, cells)`, from
    /// `interrupts-extended` when present, otherwise from `interrupts` and
    /// the interrupt parent.
    pub fn interrupts(&self, id: usize) -> Result<Vec<(usize, Vec<u32>)>, DtbError> {
        let mut out = Vec::new();
        if let Some(cells) = self.prop_cells(id, "interrupts-extended")? {
            let mut k = 0usize;
            while k < cells.len() {
                let ph = cells[k];
                let ctl = self.by_phandle(ph).ok_or(DtbError::UnknownPhandle {
                    node: self.path(id),
                    phandle: ph,
                })?;
                let n = self.controller_cells(id, ctl)? as usize;
                let spec =
                    cells
                        .get(k + 1..k + 1 + n)
                        .ok_or_else(|| DtbError::BadInterruptSpecifier {
                            node: self.path(id),
                            detail: format!("interrupts-extended ends inside a {n}-cell specifier"),
                        })?;
                out.push((ctl, spec.to_vec()));
                k += 1 + n;
            }
            return Ok(out);
        }
        let Some(cells) = self.prop_cells(id, "interrupts")? else {
            return Ok(out);
        };
        let ctl = self.interrupt_parent(id)?;
        let n = self.controller_cells(id, ctl)? as usize;
        if cells.is_empty() || cells.len() % n != 0 {
            return Err(DtbError::BadInterruptSpecifier {
                node: self.path(id),
                detail: format!("{} cells is not a multiple of {n}", cells.len()),
            });
        }
        for spec in cells.chunks_exact(n) {
            out.push((ctl, spec.to_vec()));
        }
        Ok(out)
    }
}

/// Where a reserved range comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReservationSource {
    /// The header's memory reservation block.
    MemReserve,
    /// A `/reserved-memory` child, by path.
    Node { path: String, no_map: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub range: MemRange,
    pub source: ReservationSource,
}

/// Kind of an interrupt controller node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControllerKind {
    /// RISC-V hart-local interrupt controller (`riscv,cpu-intc`) of the hart
    /// with this `reg` (hart id). Specifiers are local interrupt causes.
    RiscvHartIntc { hart: u32 },
    /// RISC-V PLIC with sources `1..=ndev` (source 0 does not exist).
    Plic { ndev: u32 },
    /// Arm GIC (`arm,gic-v3` or a GICv2 `arm,gic-400`/`arm,cortex-a15-gic`).
    /// Specifiers are `<type number flags>`: SPIs map to INTID `32 + n`
    /// (n < 988), PPIs to `16 + n` (n < 16).
    Gic { v3: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterruptController {
    pub node: usize,
    pub path: String,
    pub phandle: Option<u32>,
    pub kind: ControllerKind,
}

/// One device interrupt, resolved to its controller's global number (PLIC
/// source id, GIC INTID, or hart-local cause).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrqLine {
    pub device: String,
    pub controller: String,
    pub number: u32,
    /// The raw specifier cells.
    pub specifier: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hart {
    pub path: String,
    pub id: u32,
    /// `riscv,isa`, when present.
    pub isa: Option<String>,
    pub cbom_block_bytes: Option<u32>,
}

/// What the Silicon models take from a devicetree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub arch: Arch,
    /// Enabled memory banks, sorted, non-overlapping.
    pub memory: Vec<MemRange>,
    pub reservations: Vec<Reservation>,
    pub harts: Vec<Hart>,
    pub controllers: Vec<InterruptController>,
    pub irq_lines: Vec<IrqLine>,
    tree: DeviceTree,
}

fn overlap(a: MemRange, b: MemRange) -> bool {
    // Ends were checked when the ranges were read.
    a.base < b.end().unwrap_or(u64::MAX) && b.base < a.end().unwrap_or(u64::MAX)
}

impl Platform {
    pub fn from_dtb(blob: &[u8]) -> Result<Self, DtbError> {
        Self::from_tree(DeviceTree::parse(blob)?)
    }

    pub fn from_tree(tree: DeviceTree) -> Result<Self, DtbError> {
        let n = tree.nodes().len();
        // Architecture from the first CPU's compatible.
        let cpus = tree.find("/cpus");
        let cpu_nodes: Vec<usize> = match cpus {
            Some(c) => tree
                .node(c)
                .children
                .iter()
                .copied()
                .filter(|&k| {
                    tree.prop_str(k, "device_type").ok().flatten().as_deref() == Some("cpu")
                })
                .collect(),
            None => Vec::new(),
        };
        let first_compat = match cpu_nodes.first() {
            Some(&c) => tree.compatible(c)?,
            None => tree.compatible(0)?,
        };
        let arch = if first_compat.iter().any(|c| c == "riscv") {
            Arch::Riscv64
        } else if first_compat.iter().any(|c| {
            c.starts_with("arm,cortex-a")
                || c.starts_with("arm,armv8")
                || c.starts_with("arm,neoverse")
        }) {
            Arch::Aarch64
        } else {
            return Err(DtbError::UnknownArch {
                compatible: first_compat.join(","),
            });
        };

        let mut harts = Vec::new();
        for &c in &cpu_nodes {
            if !tree.enabled(c)? {
                continue;
            }
            let id = tree.reg(c)?.first().map(|r| r.base).unwrap_or(0);
            harts.push(Hart {
                path: tree.path(c),
                id: u32::try_from(id)
                    .map_err(|_| DtbError::AddressOverflow { node: tree.path(c) })?,
                isa: tree.prop_str(c, "riscv,isa")?,
                cbom_block_bytes: tree.prop_u32(c, "riscv,cbom-block-size")?,
            });
        }

        let mut memory = Vec::new();
        for id in 0..n {
            if tree.prop_str(id, "device_type")?.as_deref() == Some("memory") && tree.enabled(id)? {
                memory.extend(tree.reg(id)?.into_iter().filter(|r| r.size > 0));
            }
        }
        memory.sort();
        for w in memory.windows(2) {
            if overlap(w[0], w[1]) {
                return Err(DtbError::OverlappingMemory { a: w[0], b: w[1] });
            }
        }

        let mut reservations: Vec<Reservation> = tree
            .reservations
            .iter()
            .map(|&range| Reservation {
                range,
                source: ReservationSource::MemReserve,
            })
            .collect();
        for r in &reservations {
            if r.range.end().is_none() {
                return Err(DtbError::AddressOverflow {
                    node: "/memreserve/".into(),
                });
            }
        }
        if let Some(rm) = tree.find("/reserved-memory") {
            for &c in &tree.node(rm).children {
                if !tree.enabled(c)? {
                    continue;
                }
                let regs = tree.reg(c)?;
                if regs.is_empty() {
                    if tree.has_prop(c, "size") {
                        return Err(DtbError::DynamicReservation { node: tree.path(c) });
                    }
                    continue;
                }
                for range in regs {
                    reservations.push(Reservation {
                        range,
                        source: ReservationSource::Node {
                            path: tree.path(c),
                            no_map: tree.has_prop(c, "no-map"),
                        },
                    });
                }
            }
        }

        let mut controllers = Vec::new();
        for id in 0..n {
            if !tree.has_prop(id, "interrupt-controller") {
                continue;
            }
            let compat = tree.compatible(id)?;
            let has = |c: &str| compat.iter().any(|x| x == c);
            let kind = if has("riscv,cpu-intc") {
                let cpu = tree
                    .node(id)
                    .parent
                    .ok_or_else(|| tree.bad(id, "compatible", "cpu-intc at root"))?;
                let hart = tree.reg(cpu)?.first().map(|r| r.base).unwrap_or(0);
                ControllerKind::RiscvHartIntc {
                    hart: u32::try_from(hart).map_err(|_| DtbError::AddressOverflow {
                        node: tree.path(cpu),
                    })?,
                }
            } else if has("riscv,plic0") || has("sifive,plic-1.0.0") {
                let ndev = tree
                    .prop_u32(id, "riscv,ndev")?
                    .ok_or_else(|| tree.bad(id, "riscv,ndev", "missing on a PLIC"))?;
                if ndev == 0 || ndev > 1023 {
                    return Err(tree.bad(id, "riscv,ndev", format!("{ndev} sources")));
                }
                ControllerKind::Plic { ndev }
            } else if has("arm,gic-v3") {
                ControllerKind::Gic { v3: true }
            } else if has("arm,gic-400") || has("arm,cortex-a15-gic") || has("arm,cortex-a9-gic") {
                ControllerKind::Gic { v3: false }
            } else {
                // Other controllers (GPIO banks, MSI frames, ...) are not
                // modelled; lines routed to them are skipped below.
                continue;
            };
            controllers.push(InterruptController {
                node: id,
                path: tree.path(id),
                phandle: tree.prop_u32(id, "phandle")?,
                kind,
            });
        }

        let mut irq_lines = Vec::new();
        for id in 0..n {
            if !tree.enabled(id)? {
                continue;
            }
            for (ctl, spec) in tree.interrupts(id)? {
                let Some(c) = controllers.iter().find(|c| c.node == ctl) else {
                    continue;
                };
                let number = resolve(&tree, id, c, &spec)?;
                irq_lines.push(IrqLine {
                    device: tree.path(id),
                    controller: c.path.clone(),
                    number,
                    specifier: spec,
                });
            }
        }

        Ok(Self {
            arch,
            memory,
            reservations,
            harts,
            controllers,
            irq_lines,
            tree,
        })
    }

    pub fn tree(&self) -> &DeviceTree {
        &self.tree
    }

    /// 4 KiB frame ranges `(base_pfn, count)` inside memory banks and outside
    /// every reservation (each reservation widened to whole pages), sorted.
    pub fn usable_frame_ranges(&self) -> Result<Vec<(u64, u64)>, DtbError> {
        let page = PAGE_BYTES;
        let mut holes: Vec<(u64, u64)> = Vec::new();
        for r in &self.reservations {
            let end = r.range.end().ok_or(DtbError::AddressOverflow {
                node: "reservation".into(),
            })?;
            let lo = r.range.base / page;
            let hi = end.div_ceil(page);
            holes.push((lo, hi));
        }
        holes.sort();
        let mut out = Vec::new();
        for m in &self.memory {
            let end = m.end().ok_or(DtbError::AddressOverflow {
                node: "memory".into(),
            })?;
            let mut lo = m.base.div_ceil(page);
            let hi = end / page;
            for &(h0, h1) in &holes {
                if h1 <= lo || h0 >= hi {
                    continue;
                }
                if h0 > lo {
                    out.push((lo, h0 - lo));
                }
                lo = lo.max(h1);
            }
            if hi > lo {
                out.push((lo, hi - lo));
            }
        }
        Ok(out)
    }

    /// One [`FrameAllocator`] per usable range, all on `ledger`.
    pub fn frame_allocators(&self, ledger: &Ledger) -> Result<Vec<FrameAllocator>, DtbError> {
        let ranges = self.usable_frame_ranges()?;
        let frames = ranges
            .iter()
            .try_fold(0u64, |acc, r| acc.checked_add(r.1))
            .unwrap_or(u64::MAX);
        if frames > MAX_MODEL_FRAMES {
            return Err(DtbError::TooManyFrames { frames });
        }
        ranges
            .into_iter()
            .map(|(base, count)| FrameAllocator::new(base, count, ledger).map_err(DtbError::Frames))
            .collect()
    }

    /// The interrupt numbers wired to `device` (a path).
    pub fn irqs_of(&self, device: &str) -> Result<Vec<u32>, DtbError> {
        let id = self
            .tree
            .find(device)
            .ok_or_else(|| DtbError::NoSuchNode(device.to_string()))?;
        let path = self.tree.path(id);
        let v: Vec<u32> = self
            .irq_lines
            .iter()
            .filter(|l| l.device == path)
            .map(|l| l.number)
            .collect();
        if v.is_empty() {
            return Err(DtbError::NoInterrupt { node: path });
        }
        Ok(v)
    }

    /// Installs `handler` on the IrqGuard [`Cpu`] model for the first
    /// interrupt of `device` that goes to a platform-level controller (PLIC
    /// or GIC), and returns that number. Devices without one are rejected,
    /// so an ISR cannot be bound to a line the hardware does not have.
    pub fn install_isr(
        &self,
        cpu: &mut Cpu,
        device: &str,
        handler: impl FnMut(&IsrCtx) + 'static,
    ) -> Result<u32, DtbError> {
        let id = self
            .tree
            .find(device)
            .ok_or_else(|| DtbError::NoSuchNode(device.to_string()))?;
        let path = self.tree.path(id);
        let line = self
            .irq_lines
            .iter()
            .find(|l| {
                l.device == path
                    && self.controllers.iter().any(|c| {
                        c.path == l.controller
                            && !matches!(c.kind, ControllerKind::RiscvHartIntc { .. })
                    })
            })
            .ok_or(DtbError::NoInterrupt { node: path })?;
        cpu.register_isr(line.number, handler);
        Ok(line.number)
    }

    /// DMA coherence of `device`: `dma-coherent` / `dma-noncoherent` on the
    /// node or an ancestor, else the architecture default (coherent on
    /// RISC-V, non-coherent on Arm, as Linux assumes). Non-coherent RISC-V
    /// DMA needs Zicbom on every hart with one `riscv,cbom-block-size`.
    pub fn dma_coherence(&self, device: &str) -> Result<DmaCoherence, DtbError> {
        let id = self
            .tree
            .find(device)
            .ok_or_else(|| DtbError::NoSuchNode(device.to_string()))?;
        let mut cur = Some(id);
        let mut marked = None;
        while let Some(n) = cur {
            if self.tree.has_prop(n, "dma-coherent") {
                marked = Some(true);
                break;
            }
            if self.tree.has_prop(n, "dma-noncoherent") {
                marked = Some(false);
                break;
            }
            cur = self.tree.node(n).parent;
        }
        let coherent = marked.unwrap_or(self.arch != Arch::Aarch64);
        if coherent {
            return Ok(DmaCoherence::Coherent);
        }
        match self.arch {
            Arch::Aarch64 => Ok(DmaCoherence::ArmNonCoherent),
            Arch::X86_64 => Ok(DmaCoherence::Coherent),
            Arch::Riscv64 => {
                let node = self.tree.path(id);
                let fail = |detail: String| DtbError::NoCacheBlockManagement {
                    node: node.clone(),
                    detail,
                };
                if self.harts.is_empty() {
                    return Err(fail("no enabled harts".into()));
                }
                let mut block = None;
                for h in &self.harts {
                    let isa = h.isa.as_deref().unwrap_or("");
                    if !isa_has_extension(isa, "zicbom") {
                        return Err(fail(format!(
                            "{} lacks Zicbom (riscv,isa = {isa:?})",
                            h.path
                        )));
                    }
                    let b = h
                        .cbom_block_bytes
                        .ok_or_else(|| fail(format!("{} has no riscv,cbom-block-size", h.path)))?;
                    if b == 0 || !b.is_power_of_two() {
                        return Err(fail(format!("{} cbom block size {b}", h.path)));
                    }
                    if block.is_some_and(|x| x != b) {
                        return Err(fail("harts disagree on riscv,cbom-block-size".into()));
                    }
                    block = Some(b);
                }
                Ok(DmaCoherence::Zicbom {
                    block_bytes: block.unwrap_or(0),
                })
            }
        }
    }
}

/// Whether a `riscv,isa` string (`rv64imafdc_zicbom_...`) names `ext`.
pub fn isa_has_extension(isa: &str, ext: &str) -> bool {
    let isa = isa.to_ascii_lowercase();
    isa.starts_with("rv") && isa.split('_').skip(1).any(|e| e == ext)
}

fn resolve(
    tree: &DeviceTree,
    id: usize,
    c: &InterruptController,
    spec: &[u32],
) -> Result<u32, DtbError> {
    let bad = |detail: String| DtbError::BadInterruptSpecifier {
        node: tree.path(id),
        detail,
    };
    let out_of_range = |irq: u32, limit: u32| DtbError::IrqOutOfRange {
        node: tree.path(id),
        controller: c.path.clone(),
        irq,
        limit,
    };
    match c.kind {
        ControllerKind::RiscvHartIntc { .. } => {
            let cause = spec[0];
            if cause >= 64 {
                return Err(out_of_range(cause, 63));
            }
            Ok(cause)
        }
        ControllerKind::Plic { ndev } => {
            let src = spec[0];
            if src == 0 || src > ndev {
                return Err(out_of_range(src, ndev));
            }
            Ok(src)
        }
        ControllerKind::Gic { .. } => {
            if spec.len() < 3 {
                return Err(bad(format!(
                    "GIC specifier has {} cells, expected 3",
                    spec.len()
                )));
            }
            match spec[0] {
                0 if spec[1] < 988 => Ok(32 + spec[1]),
                0 => Err(out_of_range(spec[1], 987)),
                1 if spec[1] < 16 => Ok(16 + spec[1]),
                1 => Err(out_of_range(spec[1], 15)),
                t => Err(bad(format!("GIC interrupt type {t}"))),
            }
        }
    }
}
