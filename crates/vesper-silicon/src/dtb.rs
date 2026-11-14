//! Flattened Devicetree (DTB) input for memory, interrupt-controller and
//! bus nodes (§26.1 items 3 and 5).
//!
//! [`DeviceTree::parse`] reads a DTB blob (format version 16 or 17; 17 is
//! what `dtc` and QEMU emit) into a node tree. Every length, offset,
//! alignment, string terminator, token and nesting level is checked;
//! malformed or over-budget input is a typed [`DtbError`], never a panic. The
//! parser is written here rather than taken from the `fdt` crate because
//! `fdt` 0.1.5 `expect`s and `assert`s on malformed structure blocks, which
//! this crate's untrusted-input rule does not allow.
//!
//! Addresses: `#address-cells` / `#size-cells` up to 4 are read into 128-bit
//! values ([`DeviceTree::reg_raw`]); [`DeviceTree::reg_translated`] maps a
//! node's `reg` to CPU physical addresses through every ancestor's `ranges`
//! (including PCI address-space matching), and [`DeviceTree::pci_windows`]
//! lists a PCI host bridge's windows.
//!
//! Interrupts: specifiers are followed through `interrupt-map` nexus nodes
//! (masked unit-address + specifier matching, chains of nexus nodes, parent
//! unit addresses) to the receiving controller ([`DeviceTree::interrupts`],
//! [`DeviceTree::map_interrupt`], [`Platform::pci_intx`]).
//!
//! [`Platform::from_tree`] then extracts what the Silicon models consume:
//!
//! * memory banks (`device_type = "memory"`), the header's `/memreserve/`
//!   entries and `/reserved-memory` children, both fixed (`reg`) and
//!   dynamically placed (`size`, `alignment`, `alloc-ranges`; see
//!   [`place_dynamic`]), and the 4 KiB frame ranges that remain
//!   ([`Platform::usable_frame_ranges`]), which seed [`FrameAllocator`]s for
//!   the frame-conservation model;
//! * interrupt controllers (RISC-V hart-local `riscv,cpu-intc`, PLIC, GICv2
//!   and GICv3) and every device interrupt resolved to a controller-global
//!   number, range-checked against the controller (`riscv,ndev` for a PLIC,
//!   the SPI/PPI ranges for a GIC), which install ISRs on the IrqGuard
//!   [`Cpu`] model ([`Platform::install_isr`]); the PLIC contexts and
//!   CLINT/ACLINT blocks, which [`crate::intc`] emulates;
//! * the per-hart `riscv,isa`, `riscv,isa-base`, `riscv,isa-extensions` and
//!   `riscv,cbom-block-size`, and each device's `dma-coherent` /
//!   `dma-noncoherent` marking, which select the Zicbom cache-block
//!   maintenance of the DMA model ([`Platform::dma_coherence`]), or the
//!   SiFive cache controller's `FLUSH64` on the StarFive JH7100
//!   ([`CcacheInfo`]);
//! * RISC-V AIA: APLIC domains (`riscv,aplic`, with `riscv,children`,
//!   `riscv,delegation`/`riscv,delegate` and `msi-parent`) and IMSIC files
//!   (`riscv,imsics`, with the hart/guest/group index fields), recorded as
//!   [`AplicInfo`] and [`ImsicInfo`] for [`crate::aia`] and [`crate::intc`];
//! * interrupt trigger types from two-cell specifiers ([`Platform::trigger`]),
//!   `interrupt-map-pass-thru`, and any number of PLICs;
//! * `dma-ranges` on every bus level between a device and the root,
//!   composed into the CPU-to-bus windows of [`Platform::dma_windows`] and
//!   [`Platform::dma_address`] (a missing `dma-ranges` is identity, an empty
//!   one is identity at that level, as in Linux).
//!
//! Not supported (typed errors): versions before 16, translation across a bus
//! without `ranges` (a non-memory-mapped bus such as I2C or SPI), PCI
//! `msi-map`, and resolving a wired interrupt to an IMSIC (IMSICs take MSIs
//! only).

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
/// A version-16 header lacks the trailing `size_dt_struct` field.
const HEADER_V16_BYTES: usize = 36;

/// Parser budgets. Real trees are far below these.
pub const MAX_DEPTH: usize = 64;
pub const MAX_NODES: usize = 65_536;
pub const MAX_RESERVATIONS: usize = 1024;
/// Most 4 KiB frames [`Platform::frame_allocators`] will model (16 GiB).
pub const MAX_MODEL_FRAMES: u64 = 1 << 22;
/// Widest `#address-cells` / `#size-cells` accepted (Linux's
/// `OF_MAX_ADDR_CELLS`); addresses are computed in 128 bits.
pub const MAX_CELLS: u32 = 4;
/// Most interrupt nexus nodes one specifier may pass through.
pub const MAX_NEXUS_HOPS: usize = 16;
/// PCI `phys.hi` address-space bits (`ss`), compared when matching `ranges`.
const PCI_SPACE_MASK: u32 = 0x0300_0000;

/// Big-endian cells as one number (at most [`MAX_CELLS`] cells, so it fits).
fn join_cells(c: &[u32]) -> u128 {
    c.iter().fold(0u128, |v, &x| (v << 32) | u128::from(x))
}

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

/// One raw `reg` entry: the address in the parent bus's cells and the size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegEntry {
    pub address: Vec<u32>,
    pub size: u128,
}

/// PCI address space of a host-bridge window (`phys.hi` bits 24..25).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PciSpace {
    Config,
    Io,
    Mem32,
    Mem64,
}

/// One window of a PCI host bridge's `ranges`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciWindow {
    pub space: PciSpace,
    pub prefetchable: bool,
    pub pci_addr: u64,
    /// CPU physical address after translating through every ancestor bus.
    pub cpu_addr: u64,
    pub size: u64,
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
    /// Kept for compatibility: `interrupt-map-pass-thru` is now applied
    /// (see [`DeviceTree::map_interrupt`]) and this is no longer returned.
    UnsupportedInterruptNexus {
        node: String,
        nexus: String,
    },
    /// No `interrupt-map` entry of `nexus` matches the masked unit address
    /// and specifier.
    NoInterruptMapEntry {
        node: String,
        nexus: String,
        unit: Vec<u32>,
        specifier: Vec<u32>,
    },
    /// The specifier passed through more than [`MAX_NEXUS_HOPS`] nexus nodes.
    NexusLoop {
        node: String,
    },
    /// `bus` (an ancestor of `node`) has no `ranges`, so its child addresses
    /// do not map to CPU physical addresses.
    Untranslatable {
        node: String,
        bus: String,
    },
    /// No entry of `bus`'s `ranges` covers the whole region.
    NoRangesEntry {
        node: String,
        bus: String,
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
    /// A dynamically placed `/reserved-memory` child (`size` without `reg`)
    /// for which no aligned free range exists in memory (or `alloc-ranges`).
    ReservationDoesNotFit {
        node: String,
        size: u64,
        alignment: u64,
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
    /// `[addr, addr + len)` is not inside one of the device's DMA windows
    /// (`dma-ranges`), so the bus master cannot reach it.
    NotDmaAddressable {
        node: String,
        addr: u64,
        len: u64,
    },
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
    /// The header's format version (16 or 17).
    pub version: u32,
    pub boot_cpuid: u32,
    /// The header's memory reservation block (`/memreserve/`).
    pub reservations: Vec<MemRange>,
    nodes: Vec<Node>,
    phandles: BTreeMap<u32, usize>,
}

impl DeviceTree {
    /// Parses a version-16 or version-17 DTB (a later version is accepted
    /// when its `last_comp_version` is at most 17, as the format requires).
    /// Version 16 has no `size_dt_struct` header field; its structure block
    /// runs to `FDT_END`, bounded by `totalsize`. Versions before 16 use a
    /// different node-name encoding and are rejected. Bytes after
    /// `totalsize` are ignored.
    pub fn parse(blob: &[u8]) -> Result<Self, DtbError> {
        if blob.len() < HEADER_V16_BYTES {
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
        if version < 16 || last_compatible > 17 || last_compatible > version {
            return Err(DtbError::UnsupportedVersion {
                version,
                last_compatible,
            });
        }
        let header = if version >= 17 {
            HEADER_BYTES
        } else {
            HEADER_V16_BYTES
        };
        if total < header || total > blob.len() {
            return Err(DtbError::Truncated { what: "totalsize" });
        }
        let size_struct = if version >= 17 {
            h(9)? as usize
        } else {
            total
                .checked_sub(off_struct)
                .ok_or(DtbError::BlockOutOfBounds { block: "structure" })?
        };
        let blob = &blob[..total];
        let block = |off: usize, size: usize, name: &'static str| -> Result<&[u8], DtbError> {
            let end = off
                .checked_add(size)
                .ok_or(DtbError::BlockOutOfBounds { block: name })?;
            if off < header || end > total {
                return Err(DtbError::BlockOutOfBounds { block: name });
            }
            Ok(&blob[off..end])
        };
        if !off_struct.is_multiple_of(4) {
            return Err(DtbError::Misaligned {
                what: "structure block",
            });
        }
        if !off_rsv.is_multiple_of(8) {
            return Err(DtbError::Misaligned {
                what: "memory reservation block",
            });
        }
        let structs = block(off_struct, size_struct, "structure")?;
        let strings = block(off_strings, size_strings, "strings")?;

        // Memory reservation block: (u64 address, u64 size) pairs up to a
        // (0, 0) terminator, all inside the blob.
        let mut reservations = Vec::new();
        if off_rsv < header {
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
            version,
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
        let (cells, rest) = v.as_chunks::<4>();
        if !rest.is_empty() {
            return Err(self.bad(id, name, format!("{} bytes is not whole cells", v.len())));
        }
        Ok(Some(cells.iter().map(|c| u32::from_be_bytes(*c)).collect()))
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

    /// `status` is absent, `okay` or `ok` on the node and every ancestor (a
    /// disabled bus disables its whole subtree).
    pub fn enabled(&self, id: usize) -> Result<bool, DtbError> {
        let mut cur = Some(id);
        while let Some(n) = cur {
            if !matches!(
                self.prop_str(n, "status")?.as_deref(),
                None | Some("okay") | Some("ok")
            ) {
                return Ok(false);
            }
            cur = self.nodes[n].parent;
        }
        Ok(true)
    }

    /// The node's own `#address-cells` / `#size-cells` (2 and 1 when absent),
    /// which apply to its children's `reg` and to its `ranges` child side.
    /// Up to [`MAX_CELLS`] cells are accepted (PCI buses use 3 address
    /// cells); `#address-cells = 0` is allowed here and rejected where an
    /// address is needed.
    pub fn bus_cells(&self, bus: usize) -> Result<(u32, u32), DtbError> {
        let a = self.prop_u32(bus, "#address-cells")?.unwrap_or(2);
        let s = self.prop_u32(bus, "#size-cells")?.unwrap_or(1);
        for c in [a, s] {
            if c > MAX_CELLS {
                return Err(DtbError::UnsupportedCells {
                    node: self.path(bus),
                    cells: c,
                });
            }
        }
        Ok((a, s))
    }

    /// `#address-cells` / `#size-cells` that apply to `id`'s `reg` (the
    /// parent's, defaulting to 2 and 1).
    pub fn reg_cells(&self, id: usize) -> Result<(u32, u32), DtbError> {
        let Some(p) = self.nodes[id].parent else {
            return Ok((2, 1));
        };
        let (a, s) = self.bus_cells(p)?;
        if a == 0 {
            return Err(DtbError::UnsupportedCells {
                node: self.path(p),
                cells: 0,
            });
        }
        Ok((a, s))
    }

    /// The node's `reg` entries as raw bus addresses (`#address-cells` cells
    /// of the parent bus) and sizes.
    pub fn reg_raw(&self, id: usize) -> Result<Vec<RegEntry>, DtbError> {
        let Some(cells) = self.prop_cells(id, "reg")? else {
            return Ok(Vec::new());
        };
        let (a, s) = self.reg_cells(id)?;
        let per = (a + s) as usize;
        if cells.is_empty() || !cells.len().is_multiple_of(per) {
            return Err(self.bad(
                id,
                "reg",
                format!("{} cells is not a multiple of {a}+{s}", cells.len()),
            ));
        }
        Ok(cells
            .chunks_exact(per)
            .map(|e| RegEntry {
                address: e[..a as usize].to_vec(),
                size: join_cells(&e[a as usize..]),
            })
            .collect())
    }

    /// The node's `reg` entries in its parent bus's address space, without
    /// `ranges` translation. Addresses or sizes wider than 64 bits (for
    /// example a 3-cell PCI address) are [`DtbError::AddressOverflow`]; use
    /// [`DeviceTree::reg_raw`] or [`DeviceTree::reg_translated`] for those.
    pub fn reg(&self, id: usize) -> Result<Vec<MemRange>, DtbError> {
        self.reg_raw(id)?
            .into_iter()
            .map(|e| {
                let base = u64::try_from(join_cells(&e.address)).ok();
                let size = u64::try_from(e.size).ok();
                match (base, size) {
                    (Some(base), Some(size)) if base.checked_add(size).is_some() => {
                        Ok(MemRange { base, size })
                    }
                    _ => Err(DtbError::AddressOverflow {
                        node: self.path(id),
                    }),
                }
            })
            .collect()
    }

    /// The node's `reg` entries translated to CPU physical addresses through
    /// every ancestor bus's `ranges` (as Linux's `of_translate_address`
    /// does). An empty `ranges` is a 1:1 mapping; a missing one ends the
    /// translatable region ([`DtbError::Untranslatable`]); a region that no
    /// `ranges` entry covers entirely is [`DtbError::NoRangesEntry`]. PCI
    /// buses (`device_type = "pci"`/`"pciex"` with 3 address cells) match
    /// entries by address space (`phys.hi` bits 24..25) as well.
    pub fn reg_translated(&self, id: usize) -> Result<Vec<MemRange>, DtbError> {
        let Some(bus) = self.nodes[id].parent else {
            return Err(self.bad(id, "reg", "the root node has no bus"));
        };
        self.reg_raw(id)?
            .into_iter()
            .map(|e| {
                let base = self.translate(bus, &e.address, e.size, id)?;
                Ok(MemRange {
                    base,
                    size: e.size as u64,
                })
            })
            .collect()
    }

    fn is_pci_bus(&self, bus: usize) -> Result<bool, DtbError> {
        Ok(matches!(
            self.prop_str(bus, "device_type")?.as_deref(),
            Some("pci") | Some("pciex")
        ) && self.bus_cells(bus)?.0 == 3)
    }

    /// Splits a bus address into (PCI `phys.hi`, numeric address).
    fn decode(&self, bus: usize, cells: &[u32]) -> Result<(Option<u32>, u128), DtbError> {
        if self.is_pci_bus(bus)? {
            Ok((Some(cells[0]), join_cells(&cells[1..])))
        } else {
            Ok((None, join_cells(cells)))
        }
    }

    /// Translates `[addr, addr + size)` in `bus`'s child address space (`addr`
    /// has `bus`'s `#address-cells` cells) to a CPU physical address. `who`
    /// names the node in errors.
    pub fn translate(
        &self,
        bus: usize,
        addr: &[u32],
        size: u128,
        who: usize,
    ) -> Result<u64, DtbError> {
        let overflow = || DtbError::AddressOverflow {
            node: self.path(who),
        };
        let (na, _) = self.bus_cells(bus)?;
        if addr.len() != na as usize || na == 0 {
            return Err(DtbError::UnsupportedCells {
                node: self.path(bus),
                cells: na,
            });
        }
        let (mut hi, mut value) = self.decode(bus, addr)?;
        let mut bus = bus;
        for _ in 0..=MAX_DEPTH {
            let Some(parent) = self.nodes[bus].parent else {
                // The root's child address space is the CPU's.
                let base = u64::try_from(value).map_err(|_| overflow())?;
                let size = u64::try_from(size).map_err(|_| overflow())?;
                base.checked_add(size).ok_or_else(overflow)?;
                return Ok(base);
            };
            let (na, ns) = self.bus_cells(bus)?;
            let (pna, _) = self.bus_cells(parent)?;
            let Some(ranges) = self.prop_cells(bus, "ranges")? else {
                return Err(DtbError::Untranslatable {
                    node: self.path(who),
                    bus: self.path(bus),
                });
            };
            if ranges.is_empty() {
                // Identity: the parent sees the same number.
                if pna == 0 || (pna < 4 && value >> (32 * pna) != 0) {
                    return Err(overflow());
                }
                hi = if self.is_pci_bus(parent)? { hi } else { None };
                bus = parent;
                continue;
            }
            let per = (na + pna + ns) as usize;
            if na == 0 || pna == 0 || ns == 0 || !ranges.len().is_multiple_of(per) {
                return Err(self.bad(
                    bus,
                    "ranges",
                    format!(
                        "{} cells is not a multiple of {na}+{pna}+{ns}",
                        ranges.len()
                    ),
                ));
            }
            let end = value.checked_add(size).ok_or_else(overflow)?;
            let mut hit = None;
            for e in ranges.chunks_exact(per) {
                let (na, pna) = (na as usize, pna as usize);
                let (chi, child) = self.decode(bus, &e[..na])?;
                let len = join_cells(&e[na + pna..]);
                if let (Some(a), Some(b)) = (hi, chi)
                    && (a ^ b) & PCI_SPACE_MASK != 0
                {
                    continue;
                }
                let Some(cend) = child.checked_add(len) else {
                    return Err(self.bad(bus, "ranges", "entry overflows"));
                };
                if value >= child && end <= cend {
                    let (phi, pbase) = self.decode(parent, &e[na..na + pna])?;
                    let v = pbase.checked_add(value - child).ok_or_else(overflow)?;
                    hit = Some((phi, v));
                    break;
                }
            }
            let Some((phi, v)) = hit else {
                return Err(DtbError::NoRangesEntry {
                    node: self.path(who),
                    bus: self.path(bus),
                });
            };
            hi = phi;
            value = v;
            bus = parent;
        }
        Err(DtbError::TooDeep)
    }

    /// The DMA windows of device `id`: its bus-master addresses translated
    /// through every ancestor bus's `dma-ranges` (child bus address, parent
    /// bus address, length) to CPU physical addresses. A bus with an empty
    /// or absent `dma-ranges` passes addresses through unchanged (Linux's
    /// `of_translate_dma_address` treats both as identity); entries of
    /// several levels compose by intersection. PCI child addresses drop
    /// their `phys.hi` cell. Windows are sorted by bus address.
    pub fn dma_windows(&self, id: usize) -> Result<Vec<DmaWindow>, DtbError> {
        let overflow = || DtbError::AddressOverflow {
            node: self.path(id),
        };
        // (dma, addr in the current bus's parent space, size); `None` is
        // the identity over everything.
        let mut cur: Option<Vec<(u128, u128, u128)>> = None;
        let mut bus = self.nodes[id].parent;
        while let Some(b) = bus {
            let Some(parent) = self.nodes[b].parent else {
                break;
            };
            let ranges = self.prop_cells(b, "dma-ranges")?.unwrap_or_default();
            if !ranges.is_empty() {
                let (na, ns) = self.bus_cells(b)?;
                let (pna, _) = self.bus_cells(parent)?;
                let per = (na + pna + ns) as usize;
                if na == 0 || pna == 0 || ns == 0 || !ranges.len().is_multiple_of(per) {
                    return Err(self.bad(
                        b,
                        "dma-ranges",
                        format!(
                            "{} cells is not a multiple of {na}+{pna}+{ns}",
                            ranges.len()
                        ),
                    ));
                }
                let mut entries = Vec::new();
                for e in ranges.chunks_exact(per) {
                    let (na, pna) = (na as usize, pna as usize);
                    let (_, child) = self.decode(b, &e[..na])?;
                    let (_, paddr) = self.decode(parent, &e[na..na + pna])?;
                    let len = join_cells(&e[na + pna..]);
                    child
                        .checked_add(len)
                        .ok_or_else(|| self.bad(b, "dma-ranges", "entry overflows"))?;
                    paddr
                        .checked_add(len)
                        .ok_or_else(|| self.bad(b, "dma-ranges", "entry overflows"))?;
                    entries.push((child, paddr, len));
                }
                cur = Some(match cur {
                    None => entries,
                    Some(ws) => {
                        let mut out = Vec::new();
                        for &(d, a, s) in &ws {
                            for &(c, p, l) in &entries {
                                let lo = a.max(c);
                                let hi = (a + s).min(c + l);
                                if lo < hi {
                                    out.push((d + (lo - a), p + (lo - c), hi - lo));
                                }
                            }
                        }
                        out
                    }
                });
            }
            bus = Some(parent);
        }
        let Some(ws) = cur else {
            return Ok(vec![DmaWindow {
                dma_addr: 0,
                cpu_addr: 0,
                size: u64::MAX,
            }]);
        };
        let mut out = ws
            .into_iter()
            .map(|(d, a, s)| {
                // Both ends of the window must lie in the 64-bit address
                // space, so translation inside it cannot overflow.
                let top = |x: u128| x.checked_add(s).is_none_or(|e| e > 1u128 << 64);
                if top(d) || top(a) {
                    return Err(overflow());
                }
                Ok(DmaWindow {
                    dma_addr: u64::try_from(d).map_err(|_| overflow())?,
                    cpu_addr: u64::try_from(a).map_err(|_| overflow())?,
                    size: u64::try_from(s).map_err(|_| overflow())?,
                })
            })
            .collect::<Result<Vec<_>, DtbError>>()?;
        out.sort();
        Ok(out)
    }

    /// The windows a PCI host bridge's `ranges` opens from PCI space into CPU
    /// physical space.
    pub fn pci_windows(&self, host: usize) -> Result<Vec<PciWindow>, DtbError> {
        if !self.is_pci_bus(host)? {
            return Err(self.bad(host, "device_type", "not a PCI bus with 3 address cells"));
        }
        let parent = self.nodes[host]
            .parent
            .ok_or_else(|| self.bad(host, "ranges", "PCI host at the root"))?;
        let (_, ns) = self.bus_cells(host)?;
        let (pna, _) = self.bus_cells(parent)?;
        let cells = self
            .prop_cells(host, "ranges")?
            .ok_or_else(|| self.bad(host, "ranges", "missing"))?;
        let per = (3 + pna + ns) as usize;
        if pna == 0 || ns == 0 || cells.is_empty() || !cells.len().is_multiple_of(per) {
            return Err(self.bad(
                host,
                "ranges",
                format!("{} cells is not a multiple of 3+{pna}+{ns}", cells.len()),
            ));
        }
        let overflow = || DtbError::AddressOverflow {
            node: self.path(host),
        };
        cells
            .chunks_exact(per)
            .map(|e| {
                let hi = e[0];
                let pna = pna as usize;
                let size = join_cells(&e[3 + pna..]);
                let space = match (hi >> 24) & 3 {
                    0 => PciSpace::Config,
                    1 => PciSpace::Io,
                    2 => PciSpace::Mem32,
                    _ => PciSpace::Mem64,
                };
                Ok(PciWindow {
                    space,
                    prefetchable: hi & 0x4000_0000 != 0,
                    pci_addr: u64::try_from(join_cells(&e[1..3])).map_err(|_| overflow())?,
                    cpu_addr: self.translate(parent, &e[3..3 + pna], size, host)?,
                    size: u64::try_from(size).map_err(|_| overflow())?,
                })
            })
            .collect()
    }

    /// The interrupt parent of `id`: its own `interrupt-parent`, else its
    /// devicetree parent when that is an interrupt controller or nexus,
    /// else the parent's inherited interrupt parent.
    pub fn interrupt_parent(&self, id: usize) -> Result<usize, DtbError> {
        let mut cur = id;
        loop {
            if let Some(ph) = self.prop_u32(cur, "interrupt-parent")? {
                return self.by_phandle(ph).ok_or(DtbError::UnknownPhandle {
                    node: self.path(id),
                    phandle: ph,
                });
            }
            let Some(p) = self.nodes[cur].parent else {
                break;
            };
            if self.has_prop(p, "interrupt-controller") || self.has_prop(p, "interrupt-map") {
                return Ok(p);
            }
            cur = p;
        }
        Err(DtbError::MissingInterruptParent {
            node: self.path(id),
        })
    }

    /// `#interrupt-cells` of an interrupt controller or nexus referenced by
    /// `user`. A node with `interrupt-controller` is a controller even if it
    /// also has an `interrupt-map` (Linux's rule).
    fn interrupt_cells(&self, user: usize, ctl: usize) -> Result<u32, DtbError> {
        if !self.has_prop(ctl, "interrupt-controller") && !self.has_prop(ctl, "interrupt-map") {
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

    /// `#address-cells` used for unit addresses matched by `nexus`'s
    /// `interrupt-map`: the nexus's own, else the nearest ancestor's, else 2
    /// (Linux's `of_irq_parse_raw` lookup).
    fn nexus_address_cells(&self, nexus: usize) -> Result<u32, DtbError> {
        let mut cur = Some(nexus);
        while let Some(n) = cur {
            if let Some(a) = self.prop_u32(n, "#address-cells")? {
                if a > MAX_CELLS {
                    return Err(DtbError::UnsupportedCells {
                        node: self.path(n),
                        cells: a,
                    });
                }
                return Ok(a);
            }
            cur = self.nodes[n].parent;
        }
        Ok(2)
    }

    /// The unit address `user` presents to an interrupt nexus: the first
    /// `cells` cells of its `reg`, zero-padded (as Linux does when `reg` is
    /// absent or short).
    fn unit_address(&self, user: usize, cells: u32) -> Result<Vec<u32>, DtbError> {
        let mut v = self.prop_cells(user, "reg")?.unwrap_or_default();
        v.resize(cells as usize, 0);
        Ok(v)
    }

    /// Follows interrupt nexus nodes from `(ctl, unit, spec)` to an
    /// interrupt controller, returning it and the specifier in its format.
    /// `unit` is the child unit address the first nexus matches (as many
    /// cells as that nexus's unit addresses have; later hops use the parent
    /// unit address from the matched entry, sized by the parent's
    /// `#address-cells`, 0 when absent, as in Linux).
    /// Each hop matches `(unit & mask, spec & mask)` against the nexus's
    /// `interrupt-map` entries (`interrupt-map-mask` defaults to all ones);
    /// no match is [`DtbError::NoInterruptMapEntry`], a chain longer than
    /// [`MAX_NEXUS_HOPS`] is [`DtbError::NexusLoop`].
    pub fn map_interrupt(
        &self,
        user: usize,
        mut ctl: usize,
        mut unit: Vec<u32>,
        mut spec: Vec<u32>,
    ) -> Result<(usize, Vec<u32>), DtbError> {
        if unit.len() > MAX_CELLS as usize {
            return Err(DtbError::UnsupportedCells {
                node: self.path(ctl),
                cells: unit.len() as u32,
            });
        }
        for _ in 0..=MAX_NEXUS_HOPS {
            let n = self.interrupt_cells(user, ctl)? as usize;
            if spec.len() != n {
                return Err(DtbError::BadInterruptSpecifier {
                    node: self.path(user),
                    detail: format!(
                        "{} specifier cells for {} which takes {n}",
                        spec.len(),
                        self.path(ctl)
                    ),
                });
            }
            if self.has_prop(ctl, "interrupt-controller") {
                return Ok((ctl, spec));
            }
            let (next, next_unit, next_spec) = self.map_one(user, ctl, &unit, &spec)?;
            ctl = next;
            unit = next_unit;
            spec = next_spec;
        }
        Err(DtbError::NexusLoop {
            node: self.path(user),
        })
    }

    /// One `interrupt-map` lookup in `nexus`.
    fn map_one(
        &self,
        user: usize,
        nexus: usize,
        unit: &[u32],
        spec: &[u32],
    ) -> Result<(usize, Vec<u32>, Vec<u32>), DtbError> {
        let na = unit.len();
        let ni = spec.len();
        let mut key = unit.to_vec();
        key.extend_from_slice(spec);
        let mask = match self.prop_cells(nexus, "interrupt-map-mask")? {
            Some(m) if m.len() == na + ni => m,
            Some(m) => {
                return Err(self.bad(
                    nexus,
                    "interrupt-map-mask",
                    format!("{} cells, expected {na}+{ni}", m.len()),
                ));
            }
            None => vec![u32::MAX; na + ni],
        };
        let map = self
            .prop_cells(nexus, "interrupt-map")?
            .ok_or_else(|| self.bad(nexus, "interrupt-map", "missing"))?;
        let truncated = || self.bad(nexus, "interrupt-map", "ends inside an entry");
        let mut k = 0usize;
        while k < map.len() {
            let child = map.get(k..k + na + ni).ok_or_else(truncated)?;
            let ph = *map.get(k + na + ni).ok_or_else(truncated)?;
            let parent = self.by_phandle(ph).ok_or(DtbError::UnknownPhandle {
                node: self.path(nexus),
                phandle: ph,
            })?;
            let pna = self.prop_u32(parent, "#address-cells")?.unwrap_or(0);
            if pna > MAX_CELLS {
                return Err(DtbError::UnsupportedCells {
                    node: self.path(parent),
                    cells: pna,
                });
            }
            let pni = self.interrupt_cells(nexus, parent)? as usize;
            let at = k + na + ni + 1;
            let paddr = map.get(at..at + pna as usize).ok_or_else(truncated)?;
            let at = at + pna as usize;
            let pspec = map.get(at..at + pni).ok_or_else(truncated)?;
            let hit = key
                .iter()
                .zip(child)
                .zip(&mask)
                .all(|((x, c), m)| (x ^ c) & m == 0);
            if hit {
                let mut pspec = pspec.to_vec();
                // `interrupt-map-pass-thru` (devicetree specification 0.4,
                // §2.5.1): bits set in it are copied from the child
                // specifier into the parent specifier.
                if let Some(pass) = self.prop_cells(nexus, "interrupt-map-pass-thru")? {
                    if pass.len() != ni {
                        return Err(self.bad(
                            nexus,
                            "interrupt-map-pass-thru",
                            format!("{} cells, expected {ni}", pass.len()),
                        ));
                    }
                    for (i, p) in pspec.iter_mut().enumerate().take(ni) {
                        *p = (*p & !pass[i]) | (spec[i] & pass[i]);
                    }
                }
                return Ok((parent, paddr.to_vec(), pspec));
            }
            k = at + pni;
        }
        Err(DtbError::NoInterruptMapEntry {
            node: self.path(user),
            nexus: self.path(nexus),
            unit: unit.to_vec(),
            specifier: spec.to_vec(),
        })
    }

    /// The node's interrupt specifiers as `(controller, cells)`, from
    /// `interrupts-extended` when present, otherwise from `interrupts` and
    /// the interrupt parent, each followed through interrupt nexus nodes to
    /// the controller that receives it.
    pub fn interrupts(&self, id: usize) -> Result<Vec<(usize, Vec<u32>)>, DtbError> {
        let mut raw = Vec::new();
        if let Some(cells) = self.prop_cells(id, "interrupts-extended")? {
            let mut k = 0usize;
            while k < cells.len() {
                let ph = cells[k];
                let ctl = self.by_phandle(ph).ok_or(DtbError::UnknownPhandle {
                    node: self.path(id),
                    phandle: ph,
                })?;
                let n = self.interrupt_cells(id, ctl)? as usize;
                let spec =
                    cells
                        .get(k + 1..k + 1 + n)
                        .ok_or_else(|| DtbError::BadInterruptSpecifier {
                            node: self.path(id),
                            detail: format!("interrupts-extended ends inside a {n}-cell specifier"),
                        })?;
                raw.push((ctl, spec.to_vec()));
                k += 1 + n;
            }
        } else if let Some(cells) = self.prop_cells(id, "interrupts")? {
            let ctl = self.interrupt_parent(id)?;
            let n = self.interrupt_cells(id, ctl)? as usize;
            if cells.is_empty() || !cells.len().is_multiple_of(n) {
                return Err(DtbError::BadInterruptSpecifier {
                    node: self.path(id),
                    detail: format!("{} cells is not a multiple of {n}", cells.len()),
                });
            }
            raw.extend(cells.chunks_exact(n).map(|spec| (ctl, spec.to_vec())));
        }
        raw.into_iter()
            .map(|(ctl, spec)| {
                if self.has_prop(ctl, "interrupt-controller") {
                    return Ok((ctl, spec));
                }
                let unit = self.unit_address(id, self.nexus_address_cells(ctl)?)?;
                self.map_interrupt(id, ctl, unit, spec)
            })
            .collect()
    }
}

/// Where a reserved range comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReservationSource {
    /// The header's memory reservation block.
    MemReserve,
    /// A `/reserved-memory` child with a fixed `reg`, by path.
    Node { path: String, no_map: bool },
    /// A `/reserved-memory` child with `size` (and optionally `alignment`
    /// and `alloc-ranges`) but no `reg`, placed by [`Platform::from_tree`]
    /// the way Linux's early allocator does (see [`place_dynamic`]).
    Dynamic { path: String, no_map: bool },
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
    /// RISC-V AIA APLIC interrupt domain (`riscv,aplic`) with sources
    /// `1..=sources`; specifiers are `<source flags>`.
    Aplic { sources: u32 },
    /// RISC-V AIA IMSIC (`riscv,imsics`) with identities `1..=ids`. It
    /// receives MSIs, not wired lines (`#interrupt-cells = <0>`).
    Imsic { ids: u32 },
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
    /// `riscv,isa-base` (`rv64i`), when present.
    pub isa_base: Option<String>,
    /// `riscv,isa-extensions` (one lowercase name per entry), when present.
    pub isa_extensions: Option<Vec<String>>,
    pub cbom_block_bytes: Option<u32>,
}

impl Hart {
    /// Whether the hart implements `ext`: from `riscv,isa-extensions` when
    /// the hart has it (the binding that supersedes `riscv,isa`, preferred
    /// as Linux does), else from the `riscv,isa` string.
    pub fn has_extension(&self, ext: &str) -> bool {
        match &self.isa_extensions {
            Some(list) => list.iter().any(|e| e.eq_ignore_ascii_case(ext)),
            None => self
                .isa
                .as_deref()
                .is_some_and(|isa| isa_has_extension(isa, ext)),
        }
    }
}

/// Where one interrupt output of a RISC-V platform controller lands: a
/// hart and the local interrupt cause (11 MEIP, 9 SEIP, 3 MSIP, 1 SSIP,
/// 7 MTIP). `cause` is `None` for an output the tree marks `0xffffffff`
/// (present in hardware, not for this software).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HartTarget {
    pub hart: u32,
    pub cause: Option<u32>,
}

/// A PLIC with its register window and contexts. Context `k` is the `k`-th
/// `interrupts-extended` entry; an entry whose cause is `0xffffffff` (as on
/// the SiFive FU540/FU740 M-mode contexts) exists in hardware but is not
/// for the operating system, and has `cause: None` here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlicInfo {
    pub path: String,
    pub base: u64,
    pub size: u64,
    pub ndev: u32,
    pub contexts: Vec<HartTarget>,
}

/// Kind of a RISC-V software-interrupt / timer block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClintKind {
    /// SiFive CLINT (`sifive,clint0` / `riscv,clint0`): MSIP at `0x0 +
    /// 4*k`, MTIMECMP at `0x4000 + 8*k`, MTIME at `0xbff8`.
    Clint,
    /// ACLINT MSWI (`riscv,aclint-mswi`): MSIP at `4*k`.
    AclintMswi,
    /// ACLINT SSWI (`riscv,aclint-sswi`): SETSSIP at `4*k` (write-1 edge).
    AclintSswi,
    /// ACLINT MTIMER (`riscv,aclint-mtimer`): `reg` is MTIME then the
    /// MTIMECMP array (`8*k`), the order QEMU and the binding use.
    AclintMtimer,
}

/// A CLINT or ACLINT device: its `reg` windows (CPU physical) and the hart
/// line of each per-hart index `k`, from `interrupts-extended` (a CLINT has
/// two entries per hart, software then timer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClintInfo {
    pub path: String,
    pub kind: ClintKind,
    pub regs: Vec<MemRange>,
    pub targets: Vec<HartTarget>,
}

/// Trigger type of a wired interrupt, from the specifier's flags cell
/// (`IRQ_TYPE_*` of `dt-bindings/interrupt-controller/irq.h`: 1 rising, 2
/// falling, 4 high, 8 low). One-cell PLIC and hart-local specifiers are
/// level-high.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    EdgeRising,
    EdgeFalling,
    LevelHigh,
    LevelLow,
    /// Flags 0 (`IRQ_TYPE_NONE`): the binding leaves the type to the driver.
    Unspecified,
}

/// One `riscv,delegation` (QEMU 8.2 emits `riscv,delegate`) 3-tuple of an
/// APLIC domain: sources `first..=last` are delegated to `child`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AplicDelegation {
    pub child: String,
    pub first: u32,
    pub last: u32,
}

/// An APLIC interrupt domain (RISC-V AIA binding `riscv,aplic`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AplicInfo {
    pub path: String,
    pub base: u64,
    pub size: u64,
    /// `riscv,num-sources`.
    pub sources: u32,
    /// Direct delivery: the IDC of index `k` is the `k`-th
    /// `interrupts-extended` entry (empty in MSI-only domains).
    pub idcs: Vec<HartTarget>,
    /// MSI delivery: the IMSIC that `msi-parent` names.
    pub msi_parent: Option<String>,
    /// `riscv,children`, in child-index order.
    pub children: Vec<String>,
    pub delegations: Vec<AplicDelegation>,
    /// The domain whose `riscv,children` lists this one (`None` for a root).
    pub parent: Option<String>,
}

/// An IMSIC (RISC-V AIA binding `riscv,imsics`): one interrupt file per
/// `interrupts-extended` entry, plus `2^guest_index_bits - 1` guest files
/// after each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImsicInfo {
    pub path: String,
    pub regs: Vec<MemRange>,
    /// `riscv,num-ids`.
    pub ids: u32,
    pub guest_index_bits: u32,
    pub hart_index_bits: u32,
    pub group_index_bits: u32,
    pub group_index_shift: u32,
    /// The hart and cause (MEIP for M-level files, SEIP for S-level) of
    /// file `k`.
    pub targets: Vec<HartTarget>,
    /// Physical address of file `k` (guest 0): the `k`-th
    /// `2^guest_index_bits`-page stride through the `reg` windows in order,
    /// as Linux's `imsic_parse_fwnode` lays them out.
    pub files: Vec<u64>,
}

impl ImsicInfo {
    /// Whether these are S-level (SEIP) files.
    pub fn supervisor(&self) -> bool {
        self.targets.iter().any(|t| t.cause == Some(9))
    }

    /// The file address of `hart`'s file, guest `guest` (0 = the hart's own).
    pub fn file_of(&self, hart: u32, guest: u32) -> Option<u64> {
        if u64::from(guest) >= 1u64 << self.guest_index_bits {
            return None;
        }
        let k = self.targets.iter().position(|t| t.hart == hart)?;
        Some(self.files[k] + u64::from(guest) * 0x1000)
    }
}

/// A SiFive composable cache controller (L2/L3: `sifive,ccache0`,
/// `sifive,fu540-c000-ccache`, `sifive,fu740-c000-ccache`,
/// `starfive,jh7100-ccache`, `starfive,jh7110-ccache`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcacheInfo {
    pub path: String,
    pub base: u64,
    pub size: u64,
    /// `cache-block-size`.
    pub line_bytes: u32,
    pub sets: Option<u32>,
    pub cache_bytes: Option<u32>,
    /// The `FLUSH64` register (`base + 0x200`) when the platform relies on
    /// it for non-coherent DMA. Linux does so only for the JH7100
    /// (`starfive,jh7100-ccache`, the `QUIRK_NONSTANDARD_CACHE_OPS` of
    /// `drivers/cache/sifive_ccache.c`); elsewhere it is `None`.
    pub dma_flush64: Option<u64>,
}

/// One DMA window of a device: bus-master addresses `[dma_addr, dma_addr +
/// size)` reach CPU physical `[cpu_addr, cpu_addr + size)`. A device behind
/// buses without `dma-ranges` has one identity window covering
/// `0..u64::MAX`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DmaWindow {
    pub dma_addr: u64,
    pub cpu_addr: u64,
    pub size: u64,
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
    /// RISC-V PLICs, for [`crate::intc`].
    pub plics: Vec<PlicInfo>,
    /// RISC-V CLINT / ACLINT blocks, for [`crate::intc`].
    pub clints: Vec<ClintInfo>,
    /// RISC-V AIA APLIC domains, for [`crate::intc`].
    pub aplics: Vec<AplicInfo>,
    /// RISC-V AIA IMSICs, for [`crate::intc`].
    pub imsics: Vec<ImsicInfo>,
    /// SiFive cache controllers.
    pub ccaches: Vec<CcacheInfo>,
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
            let isa_extensions = match tree.prop_strings(c, "riscv,isa-extensions")? {
                None => None,
                Some(list) => {
                    for e in &list {
                        let ok = !e.is_empty()
                            && e.bytes()
                                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
                        if !ok {
                            return Err(tree.bad(
                                c,
                                "riscv,isa-extensions",
                                format!("{e:?} is not a lowercase extension name"),
                            ));
                        }
                    }
                    Some(list)
                }
            };
            harts.push(Hart {
                path: tree.path(c),
                id: u32::try_from(id)
                    .map_err(|_| DtbError::AddressOverflow { node: tree.path(c) })?,
                isa: tree.prop_str(c, "riscv,isa")?,
                isa_base: tree.prop_str(c, "riscv,isa-base")?,
                isa_extensions,
                cbom_block_bytes: tree.prop_u32(c, "riscv,cbom-block-size")?,
            });
        }

        let mut memory = Vec::new();
        for id in 0..n {
            if tree.prop_str(id, "device_type")?.as_deref() == Some("memory") && tree.enabled(id)? {
                memory.extend(tree.reg_translated(id)?.into_iter().filter(|r| r.size > 0));
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
        let mut dynamic = Vec::new();
        if let Some(rm) = tree.find("/reserved-memory") {
            for &c in &tree.node(rm).children {
                if !tree.enabled(c)? {
                    continue;
                }
                let regs = tree.reg_translated(c)?;
                if regs.is_empty() {
                    if tree.has_prop(c, "size") {
                        dynamic.push(c);
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

        if reservations.len() + dynamic.len() > MAX_RESERVATIONS {
            return Err(DtbError::TooManyReservations);
        }
        // Static regions first, then the dynamic ones in tree order, each
        // avoiding everything placed before it (Linux's order).
        for c in dynamic {
            let range = place_dynamic(&tree, c, &memory, &reservations)?;
            reservations.push(Reservation {
                range,
                source: ReservationSource::Dynamic {
                    path: tree.path(c),
                    no_map: tree.has_prop(c, "no-map"),
                },
            });
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
            } else if has("riscv,plic0")
                || has("sifive,plic-1.0.0")
                || has("thead,c900-plic")
                || has("andestech,nceplic100")
            {
                let ndev = tree
                    .prop_u32(id, "riscv,ndev")?
                    .ok_or_else(|| tree.bad(id, "riscv,ndev", "missing on a PLIC"))?;
                if ndev == 0 || ndev > 1023 {
                    return Err(tree.bad(id, "riscv,ndev", format!("{ndev} sources")));
                }
                ControllerKind::Plic { ndev }
            } else if has("riscv,aplic") || has("qemu,aplic") {
                let sources = tree
                    .prop_u32(id, "riscv,num-sources")?
                    .ok_or_else(|| tree.bad(id, "riscv,num-sources", "missing on an APLIC"))?;
                if sources == 0 || sources > 1023 {
                    return Err(tree.bad(id, "riscv,num-sources", format!("{sources} sources")));
                }
                ControllerKind::Aplic { sources }
            } else if has("riscv,imsics") || has("qemu,imsics") {
                let ids = tree
                    .prop_u32(id, "riscv,num-ids")?
                    .ok_or_else(|| tree.bad(id, "riscv,num-ids", "missing on an IMSIC"))?;
                if !(63..=2047).contains(&ids) || !(ids + 1).is_multiple_of(64) {
                    return Err(tree.bad(id, "riscv,num-ids", format!("{ids} identities")));
                }
                ControllerKind::Imsic { ids }
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
                let Some(number) = resolve(&tree, id, c, &spec)? else {
                    continue;
                };
                irq_lines.push(IrqLine {
                    device: tree.path(id),
                    controller: c.path.clone(),
                    number,
                    specifier: spec,
                });
            }
        }

        let (plics, clints) = riscv_blocks(&tree, &controllers)?;
        let (aplics, imsics) = aia_blocks(&tree, &controllers)?;
        let ccaches = ccache_blocks(&tree)?;

        Ok(Self {
            arch,
            memory,
            reservations,
            harts,
            controllers,
            irq_lines,
            plics,
            clints,
            aplics,
            imsics,
            ccaches,
            tree,
        })
    }

    /// A device's `reg` windows as CPU physical addresses (translated
    /// through bus `ranges`).
    pub fn mmio(&self, device: &str) -> Result<Vec<MemRange>, DtbError> {
        let id = self
            .tree
            .find(device)
            .ok_or_else(|| DtbError::NoSuchNode(device.to_string()))?;
        self.tree.reg_translated(id)
    }

    /// The controller and number that PCI legacy interrupt `pin` (1 = INTA
    /// .. 4 = INTD) of `bus:device.function` reaches through `host`'s
    /// `interrupt-map`, as the PCI binding encodes the child unit address
    /// (`bus << 16 | device << 11 | function << 8`, 0, 0).
    pub fn pci_intx(
        &self,
        host: &str,
        bus: u8,
        device: u8,
        function: u8,
        pin: u8,
    ) -> Result<IrqLine, DtbError> {
        let h = self
            .tree
            .find(host)
            .ok_or_else(|| DtbError::NoSuchNode(host.to_string()))?;
        let path = self.tree.path(h);
        if device >= 32 || function >= 8 || !(1..=4).contains(&pin) {
            return Err(DtbError::BadInterruptSpecifier {
                node: path,
                detail: format!("PCI {bus:02x}:{device:02x}.{function} INT{pin} does not exist"),
            });
        }
        if !self.tree.is_pci_bus(h)? {
            return Err(self.tree.bad(h, "device_type", "not a PCI bus"));
        }
        let mut unit =
            vec![(u32::from(bus) << 16) | (u32::from(device) << 11) | (u32::from(function) << 8)];
        unit.resize(self.tree.nexus_address_cells(h)? as usize, 0);
        let (ctl, spec) = self.tree.map_interrupt(h, h, unit, vec![u32::from(pin)])?;
        let c = self
            .controllers
            .iter()
            .find(|c| c.node == ctl)
            .ok_or_else(|| DtbError::NotAnInterruptController {
                node: path.clone(),
                controller: self.tree.path(ctl),
            })?;
        let number = resolve(&self.tree, h, c, &spec)?
            .ok_or(DtbError::NoInterrupt { node: path.clone() })?;
        Ok(IrqLine {
            device: path,
            controller: c.path.clone(),
            number,
            specifier: spec,
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
            Arch::Riscv64 => self.zicbom_coherence(id).or_else(|e| {
                // Without Zicbom, Linux maintains the JH7100's non-coherent
                // DMA through its cache controller's flush register.
                match self
                    .ccaches
                    .iter()
                    .find_map(|c| c.dma_flush64.map(|r| (r, c)))
                {
                    Some((flush64, c)) => Ok(DmaCoherence::SifiveCcache {
                        flush64,
                        line_bytes: c.line_bytes,
                    }),
                    None => Err(e),
                }
            }),
        }
    }

    fn node_of(&self, device: &str) -> Result<usize, DtbError> {
        self.tree
            .find(device)
            .ok_or_else(|| DtbError::NoSuchNode(device.to_string()))
    }

    /// The trigger type of a wired interrupt, from its specifier: one-cell
    /// PLIC and hart-local specifiers are level-high; two-cell PLIC
    /// specifiers (the T-Head C900 and Andes NCEPLIC100 bindings) allow
    /// rising edge or high level, as Linux's PLIC driver does; APLIC and GIC
    /// specifiers carry the `IRQ_TYPE_*` flags (both edges is refused).
    pub fn trigger(&self, line: &IrqLine) -> Result<Trigger, DtbError> {
        let c = self
            .controllers
            .iter()
            .find(|c| c.path == line.controller)
            .ok_or_else(|| DtbError::NoSuchNode(line.controller.clone()))?;
        let bad = |detail: String| DtbError::BadInterruptSpecifier {
            node: line.device.clone(),
            detail,
        };
        let flags = |f: u32| match f & 0xf {
            0 => Ok(Trigger::Unspecified),
            1 => Ok(Trigger::EdgeRising),
            2 => Ok(Trigger::EdgeFalling),
            4 => Ok(Trigger::LevelHigh),
            8 => Ok(Trigger::LevelLow),
            f => Err(bad(format!("trigger flags {f:#x}"))),
        };
        match c.kind {
            ControllerKind::RiscvHartIntc { .. } => Ok(Trigger::LevelHigh),
            ControllerKind::Plic { .. } => match line.specifier.get(1) {
                None => Ok(Trigger::LevelHigh),
                Some(&f) => match flags(f)? {
                    t @ (Trigger::EdgeRising | Trigger::LevelHigh) => Ok(t),
                    t => Err(bad(format!("a PLIC source cannot be {t:?}"))),
                },
            },
            ControllerKind::Aplic { .. } => flags(
                *line
                    .specifier
                    .get(1)
                    .ok_or_else(|| bad("no flags cell".into()))?,
            ),
            ControllerKind::Gic { .. } => flags(
                *line
                    .specifier
                    .get(2)
                    .ok_or_else(|| bad("no flags cell".into()))?,
            ),
            ControllerKind::Imsic { .. } => Err(bad("IMSICs have no wired lines".into())),
        }
    }

    /// The IMSIC a device's MSIs go to: `msi-parent` on the node or its
    /// nearest ancestor that has one (a PCI host bridge for its endpoints).
    pub fn msi_parent(&self, device: &str) -> Result<Option<String>, DtbError> {
        let mut cur = Some(self.node_of(device)?);
        while let Some(n) = cur {
            if let Some(cells) = self.tree.prop_cells(n, "msi-parent")? {
                let ph = *cells
                    .first()
                    .ok_or_else(|| self.tree.bad(n, "msi-parent", "empty"))?;
                let ctl = self
                    .tree
                    .by_phandle(ph)
                    .ok_or_else(|| DtbError::UnknownPhandle {
                        node: self.tree.path(n),
                        phandle: ph,
                    })?;
                return Ok(Some(self.tree.path(ctl)));
            }
            cur = self.tree.node(n).parent;
        }
        Ok(None)
    }

    /// The device's DMA windows (see [`DeviceTree::dma_windows`]).
    pub fn dma_windows(&self, device: &str) -> Result<Vec<DmaWindow>, DtbError> {
        self.tree.dma_windows(self.node_of(device)?)
    }

    /// The bus address the device must be given for CPU physical
    /// `[phys, phys + len)`: the region must lie inside one DMA window.
    pub fn dma_address(&self, device: &str, phys: u64, len: u64) -> Result<u64, DtbError> {
        let id = self.node_of(device)?;
        let end = phys.checked_add(len);
        self.tree
            .dma_windows(id)?
            .iter()
            .find(|w| {
                phys >= w.cpu_addr && end.is_some_and(|e| e <= w.cpu_addr.saturating_add(w.size))
            })
            .and_then(|w| w.dma_addr.checked_add(phys - w.cpu_addr))
            .ok_or(DtbError::NotDmaAddressable {
                node: self.tree.path(id),
                addr: phys,
                len,
            })
    }

    /// The CPU physical address a device's bus address `[dma, dma + len)`
    /// reaches.
    pub fn cpu_address(&self, device: &str, dma: u64, len: u64) -> Result<u64, DtbError> {
        let id = self.node_of(device)?;
        let end = dma.checked_add(len);
        self.tree
            .dma_windows(id)?
            .iter()
            .find(|w| {
                dma >= w.dma_addr && end.is_some_and(|e| e <= w.dma_addr.saturating_add(w.size))
            })
            .and_then(|w| w.cpu_addr.checked_add(dma - w.dma_addr))
            .ok_or(DtbError::NotDmaAddressable {
                node: self.tree.path(id),
                addr: dma,
                len,
            })
    }

    fn zicbom_coherence(&self, id: usize) -> Result<DmaCoherence, DtbError> {
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
            if !h.has_extension("zicbom") {
                let isa = match &h.isa_extensions {
                    Some(list) => format!("riscv,isa-extensions = {list:?}"),
                    None => format!("riscv,isa = {:?}", h.isa.as_deref().unwrap_or("")),
                };
                return Err(fail(format!("{} lacks Zicbom ({isa})", h.path)));
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

/// Whether a `riscv,isa` string (`rv64imafdc_zicbom_...`) names `ext`.
/// A version suffix (`2`, `1p0`) after the extension name is accepted.
pub fn isa_has_extension(isa: &str, ext: &str) -> bool {
    let isa = isa.to_ascii_lowercase();
    let versioned = |rest: &str| {
        let mut parts = rest.splitn(2, 'p');
        let major = parts.next().unwrap_or("");
        let minor = parts.next();
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        digits(major) && minor.is_none_or(digits)
    };
    isa.starts_with("rv")
        && isa
            .split('_')
            .skip(1)
            .any(|e| e == ext || e.strip_prefix(ext).is_some_and(versioned))
}

/// The controller-global number of one specifier, or `None` for a hart-local
/// specifier of `0xffffffff` (a controller output the tree marks as not
/// for this software, as the SiFive PLIC and CLINT bindings do).
fn resolve(
    tree: &DeviceTree,
    id: usize,
    c: &InterruptController,
    spec: &[u32],
) -> Result<Option<u32>, DtbError> {
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
            if cause == HIDDEN_CAUSE {
                return Ok(None);
            }
            if cause >= 64 {
                return Err(out_of_range(cause, 63));
            }
            Ok(Some(cause))
        }
        ControllerKind::Plic { ndev } => {
            let src = spec[0];
            if src == 0 || src > ndev {
                return Err(out_of_range(src, ndev));
            }
            Ok(Some(src))
        }
        ControllerKind::Aplic { sources } => {
            let src = spec[0];
            if src == 0 || src > sources {
                return Err(out_of_range(src, sources));
            }
            Ok(Some(src))
        }
        ControllerKind::Imsic { .. } => Err(bad(
            "an IMSIC receives MSIs, not wired interrupts".to_string()
        )),
        ControllerKind::Gic { .. } => {
            if spec.len() < 3 {
                return Err(bad(format!(
                    "GIC specifier has {} cells, expected 3",
                    spec.len()
                )));
            }
            match spec[0] {
                0 if spec[1] < 988 => Ok(Some(32 + spec[1])),
                0 => Err(out_of_range(spec[1], 987)),
                1 if spec[1] < 16 => Ok(Some(16 + spec[1])),
                1 => Err(out_of_range(spec[1], 15)),
                t => Err(bad(format!("GIC interrupt type {t}"))),
            }
        }
    }
}

/// A hart-local specifier marking a controller output that is not for this
/// software (SiFive PLIC/CLINT bindings).
pub const HIDDEN_CAUSE: u32 = 0xffff_ffff;

/// Places a dynamic `/reserved-memory` child (`size`, optional `alignment`
/// and `alloc-ranges`, no `reg`) the way Linux's
/// `__reserved_mem_alloc_size` does with memblock's default top-down
/// policy: in each `alloc-ranges` entry in order (or anywhere in memory when
/// there are none), at the highest address aligned to `alignment` whose
/// whole region lies inside one memory bank and outside every reservation
/// placed before it. Model choices: `alignment` defaults to 4 KiB (Linux
/// uses `SMP_CACHE_BYTES`; the frame model is page-granular anyway), and
/// the kernel image and firmware regions that are not in the tree are not
/// known, so a real kernel may place the region elsewhere.
pub fn place_dynamic(
    tree: &DeviceTree,
    node: usize,
    memory: &[MemRange],
    placed: &[Reservation],
) -> Result<MemRange, DtbError> {
    let rm = tree
        .node(node)
        .parent
        .ok_or_else(|| tree.bad(node, "size", "not under /reserved-memory"))?;
    let (a, s) = tree.bus_cells(rm)?;
    let number = |prop: &str| -> Result<Option<u64>, DtbError> {
        let Some(c) = tree.prop_cells(node, prop)? else {
            return Ok(None);
        };
        if c.len() != s as usize {
            return Err(tree.bad(node, prop, format!("{} cells, expected {s}", c.len())));
        }
        u64::try_from(join_cells(&c))
            .map(Some)
            .map_err(|_| DtbError::AddressOverflow {
                node: tree.path(node),
            })
    };
    let size = number("size")?.ok_or_else(|| tree.bad(node, "size", "missing"))?;
    if size == 0 {
        return Err(tree.bad(node, "size", "zero"));
    }
    let align = number("alignment")?.unwrap_or(PAGE_BYTES);
    if align == 0 || !align.is_power_of_two() {
        return Err(tree.bad(node, "alignment", format!("{align:#x}")));
    }
    let mut allowed = Vec::new();
    match tree.prop_cells(node, "alloc-ranges")? {
        None => allowed.push((0u64, u64::MAX)),
        Some(c) => {
            let per = (a + s) as usize;
            if a == 0 || c.is_empty() || !c.len().is_multiple_of(per) {
                return Err(tree.bad(
                    node,
                    "alloc-ranges",
                    format!("{} cells is not a multiple of {a}+{s}", c.len()),
                ));
            }
            for e in c.chunks_exact(per) {
                let len = join_cells(&e[a as usize..]);
                let base = tree.translate(rm, &e[..a as usize], len, node)?;
                // `translate` checked that base + len fits in 64 bits.
                allowed.push((base, base + len as u64));
            }
        }
    }
    // Reserved byte ranges, sorted, as [lo, hi).
    let mut holes: Vec<(u64, u64)> = placed
        .iter()
        .map(|r| (r.range.base, r.range.end().unwrap_or(u64::MAX)))
        .collect();
    holes.sort();
    for (lo, hi) in allowed {
        // Free intervals of (memory ∩ [lo, hi)) minus the holes.
        let mut free = Vec::new();
        for m in memory {
            let mut cur = m.base.max(lo);
            let end = m.end().unwrap_or(u64::MAX).min(hi);
            for &(h0, h1) in &holes {
                if h1 <= cur || h0 >= end {
                    continue;
                }
                if h0 > cur {
                    free.push((cur, h0));
                }
                cur = cur.max(h1);
            }
            if end > cur {
                free.push((cur, end));
            }
        }
        free.sort_by_key(|x| std::cmp::Reverse(x.1));
        for (f0, f1) in free {
            if f1 - f0 < size {
                continue;
            }
            let at = (f1 - size) & !(align - 1);
            if at >= f0 {
                return Ok(MemRange { base: at, size });
            }
        }
    }
    Err(DtbError::ReservationDoesNotFit {
        node: tree.path(node),
        size,
        alignment: align,
    })
}

/// Hart targets of a RISC-V platform controller's `interrupts-extended`.
fn hart_targets(
    tree: &DeviceTree,
    id: usize,
    controllers: &[InterruptController],
) -> Result<Vec<HartTarget>, DtbError> {
    tree.interrupts(id)?
        .into_iter()
        .map(|(ctl, spec)| {
            let hart = controllers.iter().find_map(|c| match c.kind {
                ControllerKind::RiscvHartIntc { hart } if c.node == ctl => Some(hart),
                _ => None,
            });
            let Some(hart) = hart else {
                return Err(DtbError::BadInterruptSpecifier {
                    node: tree.path(id),
                    detail: format!("output routed to {}, not a hart", tree.path(ctl)),
                });
            };
            Ok(match spec[0] {
                HIDDEN_CAUSE => HartTarget { hart, cause: None },
                cause if cause < 64 => HartTarget {
                    hart,
                    cause: Some(cause),
                },
                cause => {
                    return Err(DtbError::IrqOutOfRange {
                        node: tree.path(id),
                        controller: tree.path(ctl),
                        irq: cause,
                        limit: 63,
                    });
                }
            })
        })
        .collect()
}

fn riscv_blocks(
    tree: &DeviceTree,
    controllers: &[InterruptController],
) -> Result<(Vec<PlicInfo>, Vec<ClintInfo>), DtbError> {
    let mut plics = Vec::new();
    for c in controllers {
        let ControllerKind::Plic { ndev } = c.kind else {
            continue;
        };
        if !tree.enabled(c.node)? {
            continue;
        }
        let reg = tree.reg_translated(c.node)?;
        let r = *reg
            .first()
            .ok_or_else(|| tree.bad(c.node, "reg", "missing on a PLIC"))?;
        plics.push(PlicInfo {
            path: c.path.clone(),
            base: r.base,
            size: r.size,
            ndev,
            contexts: hart_targets(tree, c.node, controllers)?,
        });
    }
    let mut clints = Vec::new();
    for id in 0..tree.nodes().len() {
        let compat = tree.compatible(id)?;
        let has = |x: &str| compat.iter().any(|c| c == x);
        let kind = if has("sifive,clint0") || has("riscv,clint0") {
            ClintKind::Clint
        } else if has("riscv,aclint-mswi") {
            ClintKind::AclintMswi
        } else if has("riscv,aclint-sswi") {
            ClintKind::AclintSswi
        } else if has("riscv,aclint-mtimer") {
            ClintKind::AclintMtimer
        } else {
            continue;
        };
        if !tree.enabled(id)? {
            continue;
        }
        let regs = tree.reg_translated(id)?;
        let want = if kind == ClintKind::AclintMtimer {
            2
        } else {
            1
        };
        if regs.len() < want {
            return Err(tree.bad(
                id,
                "reg",
                format!("{} regions, expected {want}", regs.len()),
            ));
        }
        clints.push(ClintInfo {
            path: tree.path(id),
            kind,
            regs,
            targets: hart_targets(tree, id, controllers)?,
        });
    }
    Ok((plics, clints))
}

/// The APLIC domains and IMSICs of a RISC-V AIA platform.
fn aia_blocks(
    tree: &DeviceTree,
    controllers: &[InterruptController],
) -> Result<(Vec<AplicInfo>, Vec<ImsicInfo>), DtbError> {
    let mut imsics = Vec::new();
    for c in controllers {
        let ControllerKind::Imsic { ids } = c.kind else {
            continue;
        };
        let id = c.node;
        if !tree.enabled(id)? {
            continue;
        }
        let regs = tree.reg_translated(id)?;
        if regs.is_empty() {
            return Err(tree.bad(id, "reg", "missing on an IMSIC"));
        }
        let targets = hart_targets(tree, id, controllers)?;
        if targets.is_empty() || targets.iter().any(|t| t.cause.is_none()) {
            return Err(tree.bad(id, "interrupts-extended", "no hart files"));
        }
        let cause = targets[0].cause;
        if targets.iter().any(|t| t.cause != cause) || !matches!(cause, Some(9) | Some(11)) {
            return Err(tree.bad(
                id,
                "interrupts-extended",
                "files must all be M-level (11) or all S-level (9)",
            ));
        }
        let get = |name: &str, default: u32, max: u32| -> Result<u32, DtbError> {
            let v = tree.prop_u32(id, name)?.unwrap_or(default);
            if v > max {
                return Err(tree.bad(id, name, format!("{v} is above {max}")));
            }
            Ok(v)
        };
        let n = targets.len() as u32;
        // Linux: enough hart-index bits for every file by default.
        let default_hart_bits = 32 - (n - 1).leading_zeros();
        let guest_index_bits = get("riscv,guest-index-bits", 0, 7)?;
        let hart_index_bits = get("riscv,hart-index-bits", default_hart_bits, 15)?;
        let group_index_bits = get("riscv,group-index-bits", 0, 7)?;
        let group_index_shift = get("riscv,group-index-shift", 24, 55)?;
        if group_index_shift < 24 {
            return Err(tree.bad(
                id,
                "riscv,group-index-shift",
                format!("{group_index_shift} is below 24"),
            ));
        }
        let stride = 0x1000u64 << guest_index_bits;
        let mut files = Vec::new();
        for k in 0..u64::from(n) {
            let mut rel = k * stride;
            let mut at = None;
            for r in &regs {
                if !r.base.is_multiple_of(0x1000) {
                    return Err(tree.bad(id, "reg", format!("{:#x} is not page aligned", r.base)));
                }
                if rel < r.size {
                    if r.size - rel < stride {
                        return Err(tree.bad(id, "reg", "a file straddles a region end"));
                    }
                    at = Some(r.base + rel);
                    break;
                }
                rel -= r.size;
            }
            files.push(at.ok_or_else(|| {
                tree.bad(
                    id,
                    "reg",
                    format!("too small for {n} files of {stride:#x} bytes"),
                )
            })?);
        }
        imsics.push(ImsicInfo {
            path: c.path.clone(),
            regs,
            ids,
            guest_index_bits,
            hart_index_bits,
            group_index_bits,
            group_index_shift,
            targets,
            files,
        });
    }

    let mut aplics = Vec::new();
    for c in controllers {
        let ControllerKind::Aplic { sources } = c.kind else {
            continue;
        };
        let id = c.node;
        if !tree.enabled(id)? {
            continue;
        }
        let r = *tree
            .reg_translated(id)?
            .first()
            .ok_or_else(|| tree.bad(id, "reg", "missing on an APLIC"))?;
        let idcs = if tree.has_prop(id, "interrupts-extended") {
            hart_targets(tree, id, controllers)?
        } else {
            Vec::new()
        };
        let phandle_path = |prop: &str, ph: u32| -> Result<String, DtbError> {
            let n = tree.by_phandle(ph).ok_or(DtbError::UnknownPhandle {
                node: tree.path(id),
                phandle: ph,
            })?;
            let _ = prop;
            Ok(tree.path(n))
        };
        let msi_parent = match tree.prop_cells(id, "msi-parent")? {
            None => None,
            Some(cells) => {
                let ph = *cells
                    .first()
                    .ok_or_else(|| tree.bad(id, "msi-parent", "empty"))?;
                let path = phandle_path("msi-parent", ph)?;
                if !imsics.iter().any(|m| m.path == path) {
                    return Err(tree.bad(id, "msi-parent", format!("{path} is not an IMSIC")));
                }
                Some(path)
            }
        };
        if idcs.is_empty() && msi_parent.is_none() {
            return Err(tree.bad(
                id,
                "interrupts-extended",
                "an APLIC needs interrupts-extended or msi-parent",
            ));
        }
        let children = tree
            .prop_cells(id, "riscv,children")?
            .unwrap_or_default()
            .into_iter()
            .map(|ph| phandle_path("riscv,children", ph))
            .collect::<Result<Vec<_>, _>>()?;
        let mut delegations = Vec::new();
        for name in ["riscv,delegation", "riscv,delegate"] {
            let Some(cells) = tree.prop_cells(id, name)? else {
                continue;
            };
            if !cells.len().is_multiple_of(3) {
                return Err(tree.bad(id, name, "not 3-tuples"));
            }
            for &[ph, first, last] in cells.as_chunks::<3>().0 {
                let child = phandle_path(name, ph)?;
                if !children.contains(&child) {
                    return Err(tree.bad(id, name, format!("{child} is not a child")));
                }
                if first == 0 || first > last || last > sources {
                    return Err(tree.bad(id, name, format!("sources {first}..={last}")));
                }
                delegations.push(AplicDelegation { child, first, last });
            }
        }
        aplics.push(AplicInfo {
            path: c.path.clone(),
            base: r.base,
            size: r.size,
            sources,
            idcs,
            msi_parent,
            children,
            delegations,
            parent: None,
        });
    }
    let links: Vec<(String, String)> = aplics
        .iter()
        .flat_map(|a| a.children.iter().map(|c| (c.clone(), a.path.clone())))
        .collect();
    for (child, parent) in links {
        let Some(a) = aplics.iter_mut().find(|a| a.path == child) else {
            return Err(DtbError::BadProperty {
                node: parent,
                prop: "riscv,children".into(),
                detail: format!("{child} is not an enabled APLIC"),
            });
        };
        if a.parent.is_some() {
            return Err(DtbError::BadProperty {
                node: child,
                prop: "riscv,children".into(),
                detail: "listed by two parents".into(),
            });
        }
        a.parent = Some(parent);
    }
    // A child chain must end at a root.
    for a in &aplics {
        let mut cur = a.parent.clone();
        let mut hops = 0;
        while let Some(p) = cur {
            hops += 1;
            if hops > aplics.len() {
                return Err(DtbError::BadProperty {
                    node: a.path.clone(),
                    prop: "riscv,children".into(),
                    detail: "domain cycle".into(),
                });
            }
            cur = aplics
                .iter()
                .find(|x| x.path == p)
                .and_then(|x| x.parent.clone());
        }
    }
    Ok((aplics, imsics))
}

/// The SiFive cache controllers of a platform.
fn ccache_blocks(tree: &DeviceTree) -> Result<Vec<CcacheInfo>, DtbError> {
    let mut out = Vec::new();
    for id in 0..tree.nodes().len() {
        let compat = tree.compatible(id)?;
        let has = |x: &str| compat.iter().any(|c| c == x);
        if !(has("sifive,ccache0")
            || has("sifive,fu540-c000-ccache")
            || has("sifive,fu740-c000-ccache")
            || has("starfive,jh7100-ccache")
            || has("starfive,jh7110-ccache"))
            || !tree.enabled(id)?
        {
            continue;
        }
        let r = *tree
            .reg_translated(id)?
            .first()
            .ok_or_else(|| tree.bad(id, "reg", "missing on a cache controller"))?;
        let line_bytes = tree
            .prop_u32(id, "cache-block-size")?
            .ok_or_else(|| tree.bad(id, "cache-block-size", "missing"))?;
        if line_bytes == 0 || !line_bytes.is_power_of_two() {
            return Err(tree.bad(id, "cache-block-size", format!("{line_bytes}")));
        }
        let dma_flush64 = if has("starfive,jh7100-ccache") {
            if r.size < 0x208 {
                return Err(tree.bad(id, "reg", "window lacks FLUSH64"));
            }
            Some(r.base + 0x200)
        } else {
            None
        };
        out.push(CcacheInfo {
            path: tree.path(id),
            base: r.base,
            size: r.size,
            line_bytes,
            sets: tree.prop_u32(id, "cache-sets")?,
            cache_bytes: tree.prop_u32(id, "cache-size")?,
            dma_flush64,
        });
    }
    Ok(out)
}
