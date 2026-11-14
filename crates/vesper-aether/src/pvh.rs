//! PVH direct boot (§37.1.1): load an x86_64 ELF kernel that carries the Xen
//! `XEN_ELFNOTE_PHYS32_ENTRY` note and build the `hvm_start_info` handoff.
//!
//! The loader follows the PVH boot ABI (Xen `docs/misc/pvh.pandoc` and
//! `xen/include/public/arch-x86/hvm/start_info.h`, the protocol Linux, Firecracker
//! and Cloud Hypervisor use): the vCPU starts in 32-bit protected mode with
//! paging off, `CR0.PE` set, `CR4 = 0`, `EFER = 0`, flat 4 GiB code and data
//! segments, a 32-bit TSS, `EFLAGS.IF` clear and `EBX` holding the guest
//! physical address of `hvm_start_info` (magic `0x336ec578`, version 1). The
//! memory map is a table of `hvm_memmap_table_entry` records; there is no ACPI
//! RSDP (`rsdp_paddr = 0`) and no modules.
//!
//! Untrusted ELF bytes are bounds-checked throughout; malformed headers,
//! overlapping or out-of-RAM segments, segments over the boot structures,
//! missing, duplicate or out-of-range entry notes and an entry point outside
//! every executable segment are typed [`PvhError`]s. The result is a
//! [`BootCpuState`] the KVM backend installs; validated here without KVM.
#![forbid(unsafe_code)]

use crate::guest_mem::{GuestMemory, MemError};

pub const XEN_ELFNOTE_PHYS32_ENTRY: u32 = 18;
pub const HVM_START_MAGIC: u32 = 0x336e_c578;
pub const HVM_START_INFO_VERSION: u32 = 1;
pub const HVM_START_INFO_BYTES: usize = 56;
pub const HVM_MEMMAP_ENTRY_BYTES: usize = 24;
pub const HVM_MEMMAP_TYPE_RAM: u32 = 1;
/// Boot GDT (null, code, data, TSS descriptors), as Firecracker places it.
pub const BOOT_GDT_GPA: u64 = 0x500;
pub const BOOT_GDT_ENTRIES: usize = 4;
pub const START_INFO_GPA: u64 = 0x6000;
pub const MEMMAP_GPA: u64 = 0x7000;
pub const MAX_MEMMAP_ENTRIES: usize = 128;
pub const CMDLINE_GPA: u64 = 0x2_0000;
pub const CMDLINE_MAX_BYTES: usize = 4096;
pub const CODE_SELECTOR: u16 = 0x08;
pub const DATA_SELECTOR: u16 = 0x10;
pub const TSS_SELECTOR: u16 = 0x18;
const ELF_HEADER_BYTES: usize = 64;
const PHDR_BYTES: usize = 56;
const MAX_PHDRS: usize = 64;
const PT_LOAD: u32 = 1;
const PT_NOTE: u32 = 4;
const PF_X: u32 = 1;
const EM_X86_64: u16 = 62;
const ET_EXEC: u16 = 2;
/// Guest-physical ranges the loader writes itself.
const BOOT_AREAS: [(u64, u64); 3] = [
    (BOOT_GDT_GPA, (BOOT_GDT_ENTRIES * 8) as u64),
    (START_INFO_GPA, 0x2000),
    (CMDLINE_GPA, CMDLINE_MAX_BYTES as u64),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PvhError {
    Truncated,
    BadMagic,
    NotElf64LittleEndian,
    NotX86_64Executable,
    BadProgramHeaders,
    NoLoadSegment,
    SegmentFileRange { index: usize },
    SegmentOverflow { index: usize },
    SegmentOutsideRam { index: usize },
    SegmentsOverlap { a: usize, b: usize },
    SegmentOverBootData { index: usize },
    MalformedNote,
    MissingPvhNote,
    DuplicatePvhNote,
    EntryAbove4G,
    EntryNotExecutable { entry: u32 },
    CmdlineTooLong,
    CmdlineNul,
    TooManyMemmapEntries,
    BootDataOutsideRam,
    Memory(MemError),
}
impl From<MemError> for PvhError {
    fn from(value: MemError) -> Self {
        Self::Memory(value)
    }
}
impl std::fmt::Display for PvhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PvhError {}

/// One `PT_LOAD` segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadSegment {
    pub paddr: u64,
    pub file_offset: u64,
    pub file_bytes: u64,
    pub mem_bytes: u64,
    pub executable: bool,
}

/// A validated PVH kernel image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PvhImage {
    pub entry: u32,
    pub segments: Vec<LoadSegment>,
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, PvhError> {
    let s = b.get(at..at + 2).ok_or(PvhError::Truncated)?;
    Ok(u16::from_le_bytes(s.try_into().expect("2")))
}
fn u32_at(b: &[u8], at: usize) -> Result<u32, PvhError> {
    let s = b.get(at..at + 4).ok_or(PvhError::Truncated)?;
    Ok(u32::from_le_bytes(s.try_into().expect("4")))
}
fn u64_at(b: &[u8], at: usize) -> Result<u64, PvhError> {
    let s = b.get(at..at + 8).ok_or(PvhError::Truncated)?;
    Ok(u64::from_le_bytes(s.try_into().expect("8")))
}
fn overlaps(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 < b.0.saturating_add(b.1) && b.0 < a.0.saturating_add(a.1)
}
fn align4(n: usize) -> Option<usize> {
    n.checked_add(3).map(|v| v & !3)
}

/// Finds the PVH entry in one note segment's bytes. `Ok(None)` if absent.
fn scan_notes(notes: &[u8], found: &mut Option<u32>) -> Result<(), PvhError> {
    let mut at = 0usize;
    while at < notes.len() {
        let namesz = u32_at(notes, at).map_err(|_| PvhError::MalformedNote)? as usize;
        let descsz = u32_at(notes, at + 4).map_err(|_| PvhError::MalformedNote)? as usize;
        let kind = u32_at(notes, at + 8).map_err(|_| PvhError::MalformedNote)?;
        let name_at = at + 12;
        let desc_at = name_at
            .checked_add(align4(namesz).ok_or(PvhError::MalformedNote)?)
            .ok_or(PvhError::MalformedNote)?;
        let next = desc_at
            .checked_add(align4(descsz).ok_or(PvhError::MalformedNote)?)
            .ok_or(PvhError::MalformedNote)?;
        if desc_at.checked_add(descsz).is_none_or(|e| e > notes.len()) {
            return Err(PvhError::MalformedNote);
        }
        let name = &notes[name_at..name_at + namesz];
        if name == b"Xen\0" && kind == XEN_ELFNOTE_PHYS32_ENTRY {
            if found.is_some() {
                return Err(PvhError::DuplicatePvhNote);
            }
            let desc = &notes[desc_at..desc_at + descsz];
            let entry = match descsz {
                4 => u64::from(u32_at(desc, 0)?),
                8 => u64_at(desc, 0)?,
                _ => return Err(PvhError::MalformedNote),
            };
            *found = Some(u32::try_from(entry).map_err(|_| PvhError::EntryAbove4G)?);
        }
        if next <= at {
            return Err(PvhError::MalformedNote);
        }
        at = next;
    }
    Ok(())
}

/// Parses and validates a PVH ELF against the guest RAM layout.
pub fn parse_pvh_elf(elf: &[u8], memory: &GuestMemory) -> Result<PvhImage, PvhError> {
    if elf.len() < ELF_HEADER_BYTES {
        return Err(PvhError::Truncated);
    }
    if elf[0..4] != [0x7f, b'E', b'L', b'F'] {
        return Err(PvhError::BadMagic);
    }
    if elf[4] != 2 || elf[5] != 1 || elf[6] != 1 {
        return Err(PvhError::NotElf64LittleEndian);
    }
    if u16_at(elf, 16)? != ET_EXEC || u16_at(elf, 18)? != EM_X86_64 {
        return Err(PvhError::NotX86_64Executable);
    }
    let phoff = usize::try_from(u64_at(elf, 32)?).map_err(|_| PvhError::BadProgramHeaders)?;
    let phentsize = usize::from(u16_at(elf, 54)?);
    let phnum = usize::from(u16_at(elf, 56)?);
    if phentsize != PHDR_BYTES || phnum == 0 || phnum > MAX_PHDRS {
        return Err(PvhError::BadProgramHeaders);
    }
    if phoff
        .checked_add(phnum * PHDR_BYTES)
        .is_none_or(|e| e > elf.len())
    {
        return Err(PvhError::BadProgramHeaders);
    }
    let mut segments = Vec::new();
    let mut entry = None;
    for i in 0..phnum {
        let p = phoff + i * PHDR_BYTES;
        let kind = u32_at(elf, p)?;
        let flags = u32_at(elf, p + 4)?;
        let offset = u64_at(elf, p + 8)?;
        let paddr = u64_at(elf, p + 24)?;
        let filesz = u64_at(elf, p + 32)?;
        let memsz = u64_at(elf, p + 40)?;
        let file_end = offset
            .checked_add(filesz)
            .filter(|&e| e <= elf.len() as u64)
            .ok_or(PvhError::SegmentFileRange { index: i })?;
        match kind {
            PT_LOAD => {
                if filesz > memsz || memsz == 0 {
                    return Err(PvhError::SegmentFileRange { index: i });
                }
                paddr
                    .checked_add(memsz)
                    .ok_or(PvhError::SegmentOverflow { index: i })?;
                if !memory.contains(paddr, memsz) {
                    return Err(PvhError::SegmentOutsideRam { index: i });
                }
                if BOOT_AREAS.iter().any(|&a| overlaps((paddr, memsz), a)) {
                    return Err(PvhError::SegmentOverBootData { index: i });
                }
                segments.push(LoadSegment {
                    paddr,
                    file_offset: offset,
                    file_bytes: filesz,
                    mem_bytes: memsz,
                    executable: flags & PF_X != 0,
                });
            }
            PT_NOTE => scan_notes(&elf[offset as usize..file_end as usize], &mut entry)?,
            _ => {}
        }
    }
    if segments.is_empty() {
        return Err(PvhError::NoLoadSegment);
    }
    for a in 0..segments.len() {
        for b in a + 1..segments.len() {
            if overlaps(
                (segments[a].paddr, segments[a].mem_bytes),
                (segments[b].paddr, segments[b].mem_bytes),
            ) {
                return Err(PvhError::SegmentsOverlap { a, b });
            }
        }
    }
    let entry = entry.ok_or(PvhError::MissingPvhNote)?;
    let e = u64::from(entry);
    if !segments
        .iter()
        .any(|s| s.executable && s.paddr <= e && e < s.paddr + s.file_bytes)
    {
        return Err(PvhError::EntryNotExecutable { entry });
    }
    Ok(PvhImage { entry, segments })
}

/// A segment register as KVM's `kvm_segment` describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentState {
    pub base: u64,
    pub limit: u32,
    pub selector: u16,
    pub kind: u8,
    pub present: bool,
    pub dpl: u8,
    pub db: bool,
    pub s: bool,
    pub l: bool,
    pub g: bool,
}

/// Initial vCPU state of the PVH ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootCpuState {
    pub rip: u64,
    pub rbx: u64,
    pub rflags: u64,
    pub cr0: u64,
    pub cr4: u64,
    pub efer: u64,
    pub gdt_base: u64,
    pub gdt_limit: u16,
    pub cs: SegmentState,
    /// DS, ES, SS (and FS, GS).
    pub data: SegmentState,
    pub tr: SegmentState,
}

const CR0_PE: u64 = 1;
const CR0_ET: u64 = 1 << 4;

/// GDT descriptor from Firecracker-style `flags` (access byte plus the G/DB/L
/// nibble in bits 12..16), base and 20-bit limit.
fn gdt_entry(flags: u16, base: u32, limit: u32) -> u64 {
    let base = u64::from(base);
    let limit = u64::from(limit);
    ((base & 0xff00_0000) << 32)
        | ((base & 0x00ff_ffff) << 16)
        | (limit & 0xffff)
        | ((limit & 0xf_0000) << 32)
        | (u64::from(flags) << 40)
}
fn segment_from(selector: u16, entry: u64) -> SegmentState {
    let limit_raw = ((entry & 0xffff) | ((entry >> 32) & 0xf_0000)) as u32;
    let g = (entry >> 55) & 1 == 1;
    SegmentState {
        base: ((entry >> 16) & 0xff_ffff) | ((entry >> 32) & 0xff00_0000),
        limit: if g {
            (limit_raw << 12) | 0xfff
        } else {
            limit_raw
        },
        selector,
        kind: ((entry >> 40) & 0xf) as u8,
        s: (entry >> 44) & 1 == 1,
        dpl: ((entry >> 45) & 3) as u8,
        present: (entry >> 47) & 1 == 1,
        l: (entry >> 53) & 1 == 1,
        db: (entry >> 54) & 1 == 1,
        g,
    }
}

/// Writes the segments, boot GDT, `hvm_start_info`, memory map and command
/// line into guest memory and returns the initial vCPU state. Bytes past
/// `p_filesz` up to `p_memsz` are zeroed.
pub fn load_pvh(
    image: &PvhImage,
    elf: &[u8],
    memory: &mut GuestMemory,
    cmdline: &str,
) -> Result<BootCpuState, PvhError> {
    if cmdline.len() >= CMDLINE_MAX_BYTES {
        return Err(PvhError::CmdlineTooLong);
    }
    if cmdline.as_bytes().contains(&0) {
        return Err(PvhError::CmdlineNul);
    }
    let layout = memory.layout();
    if layout.len() > MAX_MEMMAP_ENTRIES {
        return Err(PvhError::TooManyMemmapEntries);
    }
    if BOOT_AREAS
        .iter()
        .any(|&(gpa, len)| !memory.contains(gpa, len))
    {
        return Err(PvhError::BootDataOutsideRam);
    }
    for (i, s) in image.segments.iter().enumerate() {
        let from =
            usize::try_from(s.file_offset).map_err(|_| PvhError::SegmentFileRange { index: i })?;
        let n =
            usize::try_from(s.file_bytes).map_err(|_| PvhError::SegmentFileRange { index: i })?;
        let bytes = elf
            .get(
                from..from
                    .checked_add(n)
                    .ok_or(PvhError::SegmentFileRange { index: i })?,
            )
            .ok_or(PvhError::SegmentFileRange { index: i })?;
        memory.write(s.paddr, bytes)?;
        memory.fill_zero(s.paddr + s.file_bytes, s.mem_bytes - s.file_bytes)?;
    }
    let gdt = [
        0,
        gdt_entry(0xc09b, 0, 0xfffff),
        gdt_entry(0xc093, 0, 0xfffff),
        gdt_entry(0x008b, 0, 0x67),
    ];
    for (i, e) in gdt.iter().enumerate() {
        memory.write(BOOT_GDT_GPA + 8 * i as u64, &e.to_le_bytes())?;
    }
    let mut cmd = cmdline.as_bytes().to_vec();
    cmd.push(0);
    memory.write(CMDLINE_GPA, &cmd)?;
    for (i, (gpa, len)) in layout.iter().enumerate() {
        let mut e = [0u8; HVM_MEMMAP_ENTRY_BYTES];
        e[0..8].copy_from_slice(&gpa.to_le_bytes());
        e[8..16].copy_from_slice(&len.to_le_bytes());
        e[16..20].copy_from_slice(&HVM_MEMMAP_TYPE_RAM.to_le_bytes());
        memory.write(MEMMAP_GPA + (i * HVM_MEMMAP_ENTRY_BYTES) as u64, &e)?;
    }
    let mut info = [0u8; HVM_START_INFO_BYTES];
    info[0..4].copy_from_slice(&HVM_START_MAGIC.to_le_bytes());
    info[4..8].copy_from_slice(&HVM_START_INFO_VERSION.to_le_bytes());
    // flags 0, nr_modules 0, modlist_paddr 0.
    info[24..32].copy_from_slice(&CMDLINE_GPA.to_le_bytes());
    // rsdp_paddr 0: no ACPI.
    info[40..48].copy_from_slice(&MEMMAP_GPA.to_le_bytes());
    info[48..52].copy_from_slice(&(layout.len() as u32).to_le_bytes());
    memory.write(START_INFO_GPA, &info)?;
    Ok(BootCpuState {
        rip: u64::from(image.entry),
        rbx: START_INFO_GPA,
        rflags: 0x2,
        cr0: CR0_PE | CR0_ET,
        cr4: 0,
        efer: 0,
        gdt_base: BOOT_GDT_GPA,
        gdt_limit: (BOOT_GDT_ENTRIES * 8 - 1) as u16,
        cs: segment_from(CODE_SELECTOR, gdt[1]),
        data: segment_from(DATA_SELECTOR, gdt[2]),
        tr: segment_from(TSS_SELECTOR, gdt[3]),
    })
}

/// Builds a minimal PVH ELF for tests and the gated KVM run: one executable
/// `PT_LOAD` segment holding `code` at `paddr` and a `PT_NOTE` with the
/// 32-bit entry `paddr` (4-byte descriptor).
pub fn build_test_pvh_elf(paddr: u32, code: &[u8]) -> Vec<u8> {
    let mut note = Vec::new();
    note.extend_from_slice(&4u32.to_le_bytes());
    note.extend_from_slice(&4u32.to_le_bytes());
    note.extend_from_slice(&XEN_ELFNOTE_PHYS32_ENTRY.to_le_bytes());
    note.extend_from_slice(b"Xen\0");
    note.extend_from_slice(&paddr.to_le_bytes());
    let phoff = ELF_HEADER_BYTES;
    let note_off = phoff + 2 * PHDR_BYTES;
    let code_off = note_off + note.len();
    let mut elf = vec![0u8; ELF_HEADER_BYTES];
    elf[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    elf[4] = 2;
    elf[5] = 1;
    elf[6] = 1;
    elf[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
    elf[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
    elf[20..24].copy_from_slice(&1u32.to_le_bytes());
    elf[24..32].copy_from_slice(&u64::from(paddr).to_le_bytes());
    elf[32..40].copy_from_slice(&(phoff as u64).to_le_bytes());
    elf[52..54].copy_from_slice(&(ELF_HEADER_BYTES as u16).to_le_bytes());
    elf[54..56].copy_from_slice(&(PHDR_BYTES as u16).to_le_bytes());
    elf[56..58].copy_from_slice(&2u16.to_le_bytes());
    let phdr = |kind: u32, flags: u32, off: usize, paddr: u64, size: u64| {
        let mut p = vec![0u8; PHDR_BYTES];
        p[0..4].copy_from_slice(&kind.to_le_bytes());
        p[4..8].copy_from_slice(&flags.to_le_bytes());
        p[8..16].copy_from_slice(&(off as u64).to_le_bytes());
        p[16..24].copy_from_slice(&paddr.to_le_bytes());
        p[24..32].copy_from_slice(&paddr.to_le_bytes());
        p[32..40].copy_from_slice(&size.to_le_bytes());
        p[40..48].copy_from_slice(&size.to_le_bytes());
        p[48..56].copy_from_slice(&4u64.to_le_bytes());
        p
    };
    elf.extend(phdr(
        PT_LOAD,
        PF_X | 4,
        code_off,
        u64::from(paddr),
        code.len() as u64,
    ));
    elf.extend(phdr(PT_NOTE, 4, note_off, 0, note.len() as u64));
    elf.extend(note);
    elf.extend_from_slice(code);
    elf
}
