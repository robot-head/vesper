//! A static linker for freestanding images.
//!
//! It links relocatable ELF64 objects (the entry stub, the backend's object
//! and the runtime object) into one executable laid out as
//! [`super::script::linker_script`] describes:
//!
//! ```text
//! image_base: .text   (stub `.text.boot` first, then every `.text*`)
//!             .rodata
//! page:       .data
//!             .bss
//!             stack   (16-byte aligned, `__stack_bottom`..`__stack_top`)
//! ```
//!
//! Only the relocations these objects use are applied; any other type is a
//! typed [`FreestandingError::UnsupportedRelocation`]. Undefined symbols are
//! [`FreestandingError::UnresolvedSymbol`]: nothing is left for a loader.

use std::collections::BTreeMap;

use object::{
    self, Object, ObjectSection, ObjectSymbol, RelocationFlags, RelocationTarget, SectionKind,
    SymbolSection,
};

use super::elf::{self, OutSection, OutSegment, OutSymbol};
use super::{FreestandingError, FreestandingTarget};

pub const R_RISCV_64: u32 = 2;
pub const R_RISCV_CALL: u32 = 18;
pub const R_RISCV_CALL_PLT: u32 = 19;
pub const R_RISCV_RELAX: u32 = 51;
pub const R_AARCH64_ABS64: u32 = 257;
pub const R_AARCH64_JUMP26: u32 = 282;
pub const R_AARCH64_CALL26: u32 = 283;

/// Symbols the linker (or the linker script) defines.
pub const LINKER_SYMBOLS: [&str; 6] = [
    "__bss_start",
    "__bss_end",
    "__stack_bottom",
    "__stack_top",
    "__image_end",
    "__image_start",
];

#[derive(Clone, Debug)]
pub struct LinkOptions {
    pub machine: FreestandingTarget,
    pub image_base: u64,
    pub ram_base: u64,
    pub ram_bytes: u64,
    pub stack_bytes: u64,
}

/// One loaded segment of the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub address: u64,
    pub file_offset: u64,
    pub file_bytes: u64,
    pub memory_bytes: u64,
    pub flags: u32,
}

/// Addresses of the output sections.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// `(name, address, bytes)` of `.text`, `.rodata`, `.data`, `.bss`.
    pub sections: Vec<(String, u64, u64)>,
    pub stack_bottom: u64,
    pub stack_top: u64,
    pub image_end: u64,
    pub segments: Vec<Segment>,
}

/// A symbol of the output, with the object that defined it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedSymbol {
    pub object: String,
    pub name: String,
    pub address: u64,
    pub size: u64,
    pub global: bool,
}

#[derive(Clone, Debug)]
pub struct Linked {
    pub elf: Vec<u8>,
    pub entry: u64,
    pub layout: Layout,
    pub symbols: Vec<LinkedSymbol>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Out {
    Text = 0,
    Rodata = 1,
    Data = 2,
    Bss = 3,
}

const OUT_NAMES: [&str; 4] = [".text", ".rodata", ".data", ".bss"];

struct Piece {
    object: usize,
    section: object::SectionIndex,
    out: Out,
    boot: bool,
    align: u64,
    data: Vec<u8>,
    size: u64,
    address: u64,
}

fn align_up(v: u64, a: u64) -> u64 {
    let a = a.max(1);
    v.div_ceil(a) * a
}

fn classify(
    name: &str,
    section: &object::Section<'_, '_>,
    object_name: &str,
) -> Result<Option<Out>, FreestandingError> {
    let alloc = match section.flags() {
        object::SectionFlags::Elf { sh_flags } => sh_flags & u64::from(object::elf::SHF_ALLOC) != 0,
        _ => false,
    };
    if !alloc {
        return Ok(None);
    }
    Ok(Some(match section.kind() {
        SectionKind::Text => Out::Text,
        SectionKind::Data => Out::Data,
        SectionKind::ReadOnlyData
        | SectionKind::ReadOnlyString
        | SectionKind::ReadOnlyDataWithRel => Out::Rodata,
        SectionKind::UninitializedData => Out::Bss,
        _ if section.size() == 0 => return Ok(None),
        _ => {
            return Err(FreestandingError::UnsupportedSection {
                object: object_name.into(),
                section: name.into(),
            });
        }
    }))
}

/// Link `objects` (`(name, bytes)`, in order) into an executable.
pub fn link(
    objects: &[(String, Vec<u8>)],
    options: &LinkOptions,
) -> Result<Linked, FreestandingError> {
    let machine = match options.machine {
        FreestandingTarget::Riscv64Imac => elf::EM_RISCV,
        FreestandingTarget::Aarch64 => elf::EM_AARCH64,
    };
    let bad = |name: &str, e: object::Error| FreestandingError::Object(format!("{name}: {e}"));
    let mut files = Vec::new();
    let mut e_flags = None;
    for (name, bytes) in objects {
        let file = object::File::parse(bytes.as_slice()).map_err(|e| bad(name, e))?;
        if file.format() != object::BinaryFormat::Elf
            || file.kind() != object::ObjectKind::Relocatable
            || elf::e_machine(bytes).map_err(FreestandingError::Object)? != machine
        {
            return Err(FreestandingError::Object(format!(
                "{name}: not a relocatable ELF object for {}",
                options.machine
            )));
        }
        let flags = elf::e_flags(bytes).map_err(FreestandingError::Object)?;
        match e_flags {
            None => e_flags = Some(flags),
            Some(f) if f != flags => {
                return Err(FreestandingError::Object(format!(
                    "{name}: e_flags {flags:#x} differ from {f:#x}"
                )));
            }
            _ => {}
        }
        files.push(file);
    }
    // Collect pieces.
    let mut pieces = Vec::new();
    for (oi, file) in files.iter().enumerate() {
        for section in file.sections() {
            let name = section
                .name()
                .map_err(|e| bad(&objects[oi].0, e))?
                .to_string();
            let Some(out) = classify(&name, &section, &objects[oi].0)? else {
                continue;
            };
            let data = if out == Out::Bss {
                Vec::new()
            } else {
                section.data().map_err(|e| bad(&objects[oi].0, e))?.to_vec()
            };
            pieces.push(Piece {
                object: oi,
                section: section.index(),
                out,
                boot: name == ".text.boot",
                align: section.align().max(1),
                data,
                size: section.size(),
                address: 0,
            });
        }
    }
    if pieces.iter().filter(|p| p.boot).count() != 1 {
        return Err(FreestandingError::Object(
            "exactly one input must provide .text.boot".into(),
        ));
    }
    // Layout: boot first, then input order within each output section.
    let mut order: Vec<usize> = (0..pieces.len()).collect();
    order.sort_by_key(|&i| (pieces[i].out, !pieces[i].boot, i));
    let mut cursor = options.image_base;
    let mut out_ranges = [(0u64, 0u64); 4];
    for (k, range) in out_ranges.iter_mut().enumerate() {
        cursor = align_up(cursor, if k == Out::Data as usize { 0x1000 } else { 16 });
        let start = cursor;
        let members: Vec<usize> = order
            .iter()
            .copied()
            .filter(|&i| pieces[i].out as usize == k)
            .collect();
        for i in members {
            let p = &mut pieces[i];
            cursor = align_up(cursor, p.align);
            p.address = cursor;
            cursor += p.size;
        }
        if k == Out::Bss as usize {
            // The stub clears .bss eight bytes at a time.
            cursor = align_up(cursor, 8);
        }
        *range = (start, cursor);
    }
    let bss = out_ranges[Out::Bss as usize];
    let stack_bottom = align_up(bss.1.max(out_ranges[Out::Data as usize].1), 16);
    let stack_top = stack_bottom + align_up(options.stack_bytes, 16);
    let ram_end = options.ram_base + options.ram_bytes;
    if options.image_base < options.ram_base || stack_top > ram_end {
        return Err(FreestandingError::ImageTooLarge {
            needed: stack_top.saturating_sub(options.ram_base),
            available: options.ram_bytes,
        });
    }
    let mut linker_defined = BTreeMap::new();
    linker_defined.insert("__image_start", options.image_base);
    linker_defined.insert("__bss_start", bss.0);
    linker_defined.insert("__bss_end", bss.1);
    linker_defined.insert("__stack_bottom", stack_bottom);
    linker_defined.insert("__stack_top", stack_top);
    linker_defined.insert("__image_end", stack_top);
    let piece_of: BTreeMap<(usize, usize), usize> = pieces
        .iter()
        .enumerate()
        .map(|(i, p)| ((p.object, p.section.0), i))
        .collect();
    // Symbols.
    let mut globals: BTreeMap<String, u64> = BTreeMap::new();
    let mut symbols = Vec::new();
    for (oi, file) in files.iter().enumerate() {
        for sym in file.symbols() {
            let SymbolSection::Section(si) = sym.section() else {
                continue;
            };
            let Some(&pi) = piece_of.get(&(oi, si.0)) else {
                continue;
            };
            let name = sym.name().map_err(|e| bad(&objects[oi].0, e))?;
            if name.is_empty() || sym.kind() == object::SymbolKind::Section {
                continue;
            }
            let address = pieces[pi].address + sym.address();
            let global = sym.is_global();
            if global
                && (LINKER_SYMBOLS.contains(&name)
                    || globals.insert(name.to_string(), address).is_some())
            {
                return Err(FreestandingError::DuplicateSymbol(name.into()));
            }
            symbols.push(LinkedSymbol {
                object: objects[oi].0.clone(),
                name: name.into(),
                address,
                size: sym.size(),
                global,
            });
        }
    }
    let entry = *globals
        .get("_start")
        .ok_or_else(|| FreestandingError::UnresolvedSymbol {
            symbol: "_start".into(),
            object: "linker".into(),
        })?;
    // Relocations.
    for (oi, file) in files.iter().enumerate() {
        let oname = &objects[oi].0;
        for section in file.sections() {
            let Some(&pi) = piece_of.get(&(oi, section.index().0)) else {
                continue;
            };
            for (offset, reloc) in section.relocations() {
                let RelocationFlags::Elf { r_type } = reloc.flags() else {
                    return Err(FreestandingError::Object(format!(
                        "{oname}: non-ELF relocation"
                    )));
                };
                if r_type == R_RISCV_RELAX {
                    continue;
                }
                let RelocationTarget::Symbol(si) = reloc.target() else {
                    return Err(FreestandingError::UnsupportedRelocation {
                        object: oname.clone(),
                        r_type,
                    });
                };
                let sym = file.symbol_by_index(si).map_err(|e| bad(oname, e))?;
                let sym_name = sym.name().unwrap_or("").to_string();
                let target = match sym.section() {
                    SymbolSection::Section(ts) => {
                        let tp = piece_of.get(&(oi, ts.0)).ok_or_else(|| {
                            FreestandingError::Object(format!(
                                "{oname}: relocation against an unloaded section"
                            ))
                        })?;
                        pieces[*tp].address + sym.address()
                    }
                    SymbolSection::Undefined => {
                        if let Some(a) = globals.get(&sym_name) {
                            *a
                        } else if let Some(a) = linker_defined.get(sym_name.as_str()) {
                            *a
                        } else {
                            return Err(FreestandingError::UnresolvedSymbol {
                                symbol: sym_name,
                                object: oname.clone(),
                            });
                        }
                    }
                    SymbolSection::Absolute => sym.address(),
                    _ => {
                        return Err(FreestandingError::Object(format!(
                            "{oname}: unsupported symbol section for '{sym_name}'"
                        )));
                    }
                };
                let s_plus_a = target.wrapping_add(reloc.addend() as u64);
                let place = pieces[pi].address + offset;
                let at = usize::try_from(offset)
                    .ok()
                    .filter(|&a| pieces[pi].out != Out::Bss && a < pieces[pi].data.len())
                    .ok_or_else(|| {
                        FreestandingError::Object(format!(
                            "{oname}: relocation offset out of section"
                        ))
                    })?;
                let range_err = || FreestandingError::RelocationOutOfRange {
                    object: oname.clone(),
                    symbol: sym_name.clone(),
                    r_type,
                };
                let data = &mut pieces[pi].data;
                apply(
                    options.machine,
                    r_type,
                    data,
                    at,
                    s_plus_a,
                    place,
                    range_err,
                    oname,
                )?;
            }
        }
    }
    // Output sections.
    let mut out_sections = Vec::new();
    for (k, (start, end)) in out_ranges.iter().enumerate() {
        let mut data = Vec::new();
        if k != Out::Bss as usize {
            data.resize((end - start) as usize, 0);
            for p in pieces.iter().filter(|p| p.out as usize == k) {
                let o = (p.address - start) as usize;
                data[o..o + p.data.len()].copy_from_slice(&p.data);
            }
        }
        out_sections.push(OutSection {
            name: OUT_NAMES[k],
            address: *start,
            data,
            mem_bytes: end - start,
            nobits: k == Out::Bss as usize,
            writable: k >= Out::Data as usize,
            executable: k == Out::Text as usize,
        });
    }
    out_sections.push(OutSection {
        name: ".stack",
        address: stack_bottom,
        data: Vec::new(),
        mem_bytes: stack_top - stack_bottom,
        nobits: true,
        writable: true,
        executable: false,
    });
    let segments = vec![
        OutSegment {
            sections: 0..2,
            flags: elf::PF_R | elf::PF_X,
        },
        OutSegment {
            sections: 2..5,
            flags: elf::PF_R | elf::PF_W,
        },
    ];
    let section_of = |addr: u64| {
        out_sections
            .iter()
            .position(|s| addr >= s.address && addr < s.address + s.mem_bytes.max(1))
    };
    let mut out_symbols: Vec<OutSymbol> = symbols
        .iter()
        .map(|s| OutSymbol {
            name: s.name.clone(),
            value: s.address,
            size: s.size,
            global: s.global,
            function: section_of(s.address) == Some(0),
            section: section_of(s.address),
        })
        .collect();
    for (name, value) in &linker_defined {
        out_symbols.push(OutSymbol {
            name: (*name).into(),
            value: *value,
            size: 0,
            global: true,
            function: false,
            section: None,
        });
        symbols.push(LinkedSymbol {
            object: "linker".into(),
            name: (*name).into(),
            address: *value,
            size: 0,
            global: true,
        });
    }
    let (bytes, seg_info) = elf::write_executable(
        machine,
        e_flags.unwrap_or(0),
        entry,
        &out_sections,
        &segments,
        &out_symbols,
    )
    .map_err(FreestandingError::Object)?;
    let layout = Layout {
        sections: out_ranges
            .iter()
            .enumerate()
            .map(|(k, (s, e))| (OUT_NAMES[k].to_string(), *s, e - s))
            .collect(),
        stack_bottom,
        stack_top,
        image_end: stack_top,
        segments: seg_info
            .iter()
            .zip(&segments)
            .map(
                |((address, file_offset, file_bytes, memory_bytes), seg)| Segment {
                    address: *address,
                    file_offset: *file_offset,
                    file_bytes: *file_bytes,
                    memory_bytes: *memory_bytes,
                    flags: seg.flags,
                },
            )
            .collect(),
    };
    Ok(Linked {
        elf: bytes,
        entry,
        layout,
        symbols,
    })
}

fn read32(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}

#[allow(clippy::too_many_arguments)]
fn apply(
    machine: FreestandingTarget,
    r_type: u32,
    data: &mut [u8],
    at: usize,
    s_plus_a: u64,
    place: u64,
    range_err: impl Fn() -> FreestandingError,
    object: &str,
) -> Result<(), FreestandingError> {
    let short = || FreestandingError::Object(format!("{object}: relocation runs past its section"));
    match (machine, r_type) {
        (FreestandingTarget::Riscv64Imac, R_RISCV_64)
        | (FreestandingTarget::Aarch64, R_AARCH64_ABS64) => {
            data.get_mut(at..at + 8)
                .ok_or_else(short)?
                .copy_from_slice(&s_plus_a.to_le_bytes());
        }
        (FreestandingTarget::Riscv64Imac, R_RISCV_CALL | R_RISCV_CALL_PLT) => {
            // auipc rX, hi ; jalr rY, lo(rX)
            let off = s_plus_a.wrapping_sub(place) as i64;
            if !(-(1i64 << 31)..(1i64 << 31) - 0x800).contains(&off) {
                return Err(range_err());
            }
            let hi = (off + 0x800) >> 12;
            let lo = off - (hi << 12);
            let auipc = read32(data, at).ok_or_else(short)?;
            let jalr = read32(data, at + 4).ok_or_else(short)?;
            if auipc & 0x7f != 0x17 || jalr & 0x7f != 0x67 {
                return Err(FreestandingError::Object(format!(
                    "{object}: R_RISCV_CALL does not point at auipc+jalr"
                )));
            }
            let auipc = (auipc & 0xfff) | (((hi as u32) & 0xfffff) << 12);
            let jalr = (jalr & 0x000f_ffff) | (((lo as u32) & 0xfff) << 20);
            data[at..at + 4].copy_from_slice(&auipc.to_le_bytes());
            data[at + 4..at + 8].copy_from_slice(&jalr.to_le_bytes());
        }
        (FreestandingTarget::Aarch64, R_AARCH64_CALL26 | R_AARCH64_JUMP26) => {
            let off = s_plus_a.wrapping_sub(place) as i64;
            if off % 4 != 0 || !(-(1i64 << 27)..(1i64 << 27)).contains(&off) {
                return Err(range_err());
            }
            let word = read32(data, at).ok_or_else(short)?;
            let word = (word & !0x03ff_ffff) | (((off >> 2) as u32) & 0x03ff_ffff);
            data[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        _ => {
            return Err(FreestandingError::UnsupportedRelocation {
                object: object.into(),
                r_type,
            });
        }
    }
    Ok(())
}
