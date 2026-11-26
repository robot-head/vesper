//! Little-endian ELF64 executable writing and the few header reads the
//! freestanding linker needs. Every read is bounds-checked: malformed bytes
//! are an `Err`, never a panic.

/// `e_machine` values.
pub const EM_AARCH64: u16 = 183;
pub const EM_RISCV: u16 = 243;

const PT_LOAD: u32 = 1;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

const SHT_PROGBITS: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_STRTAB: u32 = 3;
const SHT_NOBITS: u32 = 8;
const SHF_WRITE: u64 = 1;
const SHF_ALLOC: u64 = 2;
const SHF_EXECINSTR: u64 = 4;

/// `e_ident`: magic, class, data encoding and version.
const ELFMAG: &[u8; 4] = b"\x7fELF";
const EI_CLASS: usize = 4;
const EI_DATA: usize = 5;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EV_CURRENT: u8 = 1;
const ELFOSABI_NONE: u8 = 0;
const ET_EXEC: u16 = 2;

/// `Elf64_Ehdr` field offsets.
const E_TYPE: usize = 16;
const E_MACHINE: usize = 18;
const E_ENTRY: usize = 24;
const E_PHOFF: usize = 32;
const E_SHOFF: usize = 40;
const E_FLAGS: usize = 48;
const E_PHENTSIZE: usize = 54;
const E_PHNUM: usize = 56;
const E_SHENTSIZE: usize = 58;
const E_SHNUM: usize = 60;

/// `Elf64_Phdr` field offsets.
const P_FLAGS: usize = 4;
const P_OFFSET: usize = 8;
const P_VADDR: usize = 16;
const P_FILESZ: usize = 32;
const P_MEMSZ: usize = 40;

/// `Elf64_Shdr` field offsets.
const SH_TYPE: usize = 4;
const SH_OFFSET: usize = 24;
const SH_SIZE: usize = 32;
const SH_LINK: usize = 40;
const SH_ENTSIZE: usize = 56;

/// `Elf64_Sym` field offset of `st_value`.
const ST_VALUE: usize = 8;

/// Sizes of `Elf64_Ehdr`, `Elf64_Phdr`, `Elf64_Shdr` and `Elf64_Sym`.
const EHDR_BYTES: u16 = 64;
const PHDR_BYTES: u16 = 56;
const SHDR_BYTES: u16 = 64;
const SYM_BYTES: u16 = 24;

/// `st_info` binding and type, and `st_other` visibility.
const STB_LOCAL: u8 = 0;
const STB_GLOBAL: u8 = 1;
const STT_NOTYPE: u8 = 0;
const STT_FUNC: u8 = 2;
const ST_BIND_SHIFT: u8 = 4;
const STV_DEFAULT: u8 = 0;
/// `st_shndx` of an absolute symbol.
const SHN_ABS: u16 = 0xfff1;

/// Page size: `p_align` of every `PT_LOAD`, and the granule a segment's file
/// offset is congruent to its address modulo.
pub const PAGE_BYTES: u64 = 0x1000;
/// Alignment of `.symtab` and the section header table (8-byte fields).
const ELF64_TABLE_ALIGN: u64 = 8;
/// `sh_addralign` written for every output section.
const OUT_SECTION_ALIGN: u64 = 8;

fn u16_at(b: &[u8], at: usize) -> Result<u16, String> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| format!("truncated ELF at {at}"))
}
fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        .ok_or_else(|| format!("truncated ELF at {at}"))
}
fn u64_at(b: &[u8], at: usize) -> Result<u64, String> {
    b.get(at..at + 8)
        .map(|s| u64::from_le_bytes(s.try_into().unwrap()))
        .ok_or_else(|| format!("truncated ELF at {at}"))
}
fn usize_of(v: u64) -> Result<usize, String> {
    usize::try_from(v).map_err(|_| "ELF offset overflow".to_string())
}

fn check_header(b: &[u8]) -> Result<(), String> {
    if b.len() < usize::from(EHDR_BYTES) || &b[..ELFMAG.len()] != ELFMAG {
        return Err("not an ELF file".into());
    }
    if b[EI_CLASS] != ELFCLASS64 || b[EI_DATA] != ELFDATA2LSB {
        return Err("not a little-endian ELF64 file".into());
    }
    Ok(())
}

/// `e_flags` of an ELF64 file.
pub fn e_flags(b: &[u8]) -> Result<u32, String> {
    check_header(b)?;
    u32_at(b, E_FLAGS)
}

/// `e_machine` of an ELF64 file.
pub fn e_machine(b: &[u8]) -> Result<u16, String> {
    check_header(b)?;
    u16_at(b, E_MACHINE)
}

/// What [`read_executable`] returns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecutableInfo {
    pub entry: u64,
    pub machine: u16,
    pub e_type: u16,
    /// `PT_LOAD` segments: `(vaddr, file offset, file bytes)`.
    pub segments: Vec<(u64, u64, u64)>,
    /// `(vaddr, memory bytes, flags)` of each `PT_LOAD`.
    pub segment_memory: Vec<(u64, u64, u32)>,
    /// Named symbols of `.symtab`: `(name, value)`.
    pub symbols: Vec<(String, u64)>,
}

/// Program headers and symbols of an ELF64 executable.
pub fn read_executable(b: &[u8]) -> Result<ExecutableInfo, String> {
    check_header(b)?;
    let mut info = ExecutableInfo {
        e_type: u16_at(b, E_TYPE)?,
        machine: u16_at(b, E_MACHINE)?,
        entry: u64_at(b, E_ENTRY)?,
        ..Default::default()
    };
    let phoff = usize_of(u64_at(b, E_PHOFF)?)?;
    let shoff = usize_of(u64_at(b, E_SHOFF)?)?;
    let phentsize = usize::from(u16_at(b, E_PHENTSIZE)?);
    let phnum = usize::from(u16_at(b, E_PHNUM)?);
    let shentsize = usize::from(u16_at(b, E_SHENTSIZE)?);
    let shnum = usize::from(u16_at(b, E_SHNUM)?);
    if phnum > 0 && phentsize < usize::from(PHDR_BYTES) {
        return Err("bad program header size".into());
    }
    for i in 0..phnum {
        let p = phoff
            .checked_add(i.checked_mul(phentsize).ok_or("overflow")?)
            .ok_or("overflow")?;
        if u32_at(b, p)? == PT_LOAD {
            let flags = u32_at(b, p + P_FLAGS)?;
            let offset = u64_at(b, p + P_OFFSET)?;
            let vaddr = u64_at(b, p + P_VADDR)?;
            let filesz = u64_at(b, p + P_FILESZ)?;
            let memsz = u64_at(b, p + P_MEMSZ)?;
            info.segments.push((vaddr, offset, filesz));
            info.segment_memory.push((vaddr, memsz, flags));
        }
    }
    if shnum > 0 && shentsize < usize::from(SHDR_BYTES) {
        return Err("bad section header size".into());
    }
    let section = |i: usize| -> Result<(u32, u64, u64, u32, u64), String> {
        let s = shoff
            .checked_add(i.checked_mul(shentsize).ok_or("overflow")?)
            .ok_or("overflow")?;
        Ok((
            u32_at(b, s + SH_TYPE)?,
            u64_at(b, s + SH_OFFSET)?,
            u64_at(b, s + SH_SIZE)?,
            u32_at(b, s + SH_LINK)?,
            u64_at(b, s + SH_ENTSIZE)?,
        ))
    };
    for i in 0..shnum {
        let (kind, offset, size, link, entsize) = section(i)?;
        if kind != SHT_SYMTAB {
            continue;
        }
        if entsize < u64::from(SYM_BYTES) {
            return Err("bad symbol size".into());
        }
        let (_, str_off, str_size, _, _) = section(link as usize)?;
        let strtab = b
            .get(usize_of(str_off)?..usize_of(str_off.checked_add(str_size).ok_or("overflow")?)?)
            .ok_or("truncated string table")?;
        let count = size / entsize;
        for k in 0..count {
            let at = usize_of(offset + k * entsize)?;
            let name_off = u32_at(b, at)? as usize;
            let value = u64_at(b, at + ST_VALUE)?;
            let name = strtab
                .get(name_off..)
                .and_then(|s| s.split(|c| *c == 0).next())
                .ok_or("bad symbol name")?;
            if !name.is_empty() {
                info.symbols
                    .push((String::from_utf8_lossy(name).into_owned(), value));
            }
        }
    }
    Ok(info)
}

/// One output section of [`write_executable`].
#[derive(Clone, Debug)]
pub struct OutSection {
    pub name: &'static str,
    pub address: u64,
    /// Contents; empty and `nobits` for `.bss`.
    pub data: Vec<u8>,
    pub mem_bytes: u64,
    pub nobits: bool,
    pub writable: bool,
    pub executable: bool,
}

/// One `PT_LOAD`: the sections `first..=last` of the section list.
#[derive(Clone, Debug)]
pub struct OutSegment {
    pub sections: std::ops::Range<usize>,
    pub flags: u32,
}

/// A symbol written to `.symtab`.
#[derive(Clone, Debug)]
pub struct OutSymbol {
    pub name: String,
    pub value: u64,
    pub size: u64,
    pub global: bool,
    pub function: bool,
    /// Index into the section list, or `None` for an absolute symbol.
    pub section: Option<usize>,
}

/// `(vaddr, file offset, file bytes, memory bytes)` of a written segment.
pub type SegmentPlacement = (u64, u64, u64, u64);

/// Returns the bytes and the placement of each segment.
pub fn write_executable(
    machine: u16,
    e_flags: u32,
    entry: u64,
    sections: &[OutSection],
    segments: &[OutSegment],
    symbols: &[OutSymbol],
) -> Result<(Vec<u8>, Vec<SegmentPlacement>), String> {
    let ehsize = u64::from(EHDR_BYTES);
    let phnum = segments.len() as u64;
    let mut out = vec![0u8; (ehsize + phnum * u64::from(PHDR_BYTES)) as usize];
    // Section file offsets: congruent to the address modulo the page size.
    let mut file_offsets = vec![0u64; sections.len()];
    let mut seg_info = Vec::new();
    for seg in segments {
        let first = &sections[seg.sections.start];
        let mut off = out.len() as u64;
        let want = first.address % PAGE_BYTES;
        if off % PAGE_BYTES != want {
            off += (want + PAGE_BYTES - off % PAGE_BYTES) % PAGE_BYTES;
        }
        out.resize(off as usize, 0);
        let seg_base = first.address;
        let mut file_end = off;
        let mut mem_end = seg_base;
        for i in seg.sections.clone() {
            let s = &sections[i];
            let at = off + (s.address - seg_base);
            file_offsets[i] = at;
            if !s.nobits {
                out.resize(at as usize, 0);
                out.extend_from_slice(&s.data);
                file_end = at + s.data.len() as u64;
            }
            mem_end = mem_end.max(s.address + s.mem_bytes);
        }
        seg_info.push((seg_base, off, file_end - off, mem_end - seg_base));
    }
    // String tables and symbols.
    let mut shstrtab = vec![0u8];
    let name_of = |n: &str, t: &mut Vec<u8>| {
        let at = t.len() as u32;
        t.extend_from_slice(n.as_bytes());
        t.push(0);
        at
    };
    let mut sh_names = Vec::new();
    for s in sections {
        sh_names.push(name_of(s.name, &mut shstrtab));
    }
    let symtab_name = name_of(".symtab", &mut shstrtab);
    let strtab_name = name_of(".strtab", &mut shstrtab);
    let shstrtab_name = name_of(".shstrtab", &mut shstrtab);
    let mut strtab = vec![0u8];
    let mut symtab = vec![0u8; usize::from(SYM_BYTES)];
    let mut ordered: Vec<&OutSymbol> = symbols.iter().filter(|s| !s.global).collect();
    let first_global = 1 + ordered.len();
    ordered.extend(symbols.iter().filter(|s| s.global));
    for s in ordered {
        let name = name_of(&s.name, &mut strtab);
        symtab.extend_from_slice(&name.to_le_bytes());
        let bind = if s.global { STB_GLOBAL } else { STB_LOCAL };
        let kind = if s.function { STT_FUNC } else { STT_NOTYPE };
        symtab.push(bind << ST_BIND_SHIFT | kind);
        symtab.push(STV_DEFAULT);
        let shndx: u16 = match s.section {
            Some(i) => u16::try_from(i + 1).map_err(|_| "too many sections")?,
            None => SHN_ABS,
        };
        symtab.extend_from_slice(&shndx.to_le_bytes());
        symtab.extend_from_slice(&s.value.to_le_bytes());
        symtab.extend_from_slice(&s.size.to_le_bytes());
    }
    let place = |data: &[u8], align: u64, out: &mut Vec<u8>| {
        let at = (out.len() as u64).div_ceil(align) * align;
        out.resize(at as usize, 0);
        out.extend_from_slice(data);
        at
    };
    let symtab_off = place(&symtab, ELF64_TABLE_ALIGN, &mut out);
    let strtab_off = place(&strtab, 1, &mut out);
    let shstrtab_off = place(&shstrtab, 1, &mut out);
    let shoff = (out.len() as u64).div_ceil(ELF64_TABLE_ALIGN) * ELF64_TABLE_ALIGN;
    out.resize(shoff as usize, 0);
    let shnum = sections.len() + 4;
    let section_header = |out: &mut Vec<u8>,
                          name: u32,
                          kind: u32,
                          flags: u64,
                          addr: u64,
                          off: u64,
                          size: u64,
                          link: u32,
                          info: u32,
                          align: u64,
                          entsize: u64| {
        out.extend_from_slice(&name.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&addr.to_le_bytes());
        out.extend_from_slice(&off.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&link.to_le_bytes());
        out.extend_from_slice(&info.to_le_bytes());
        out.extend_from_slice(&align.to_le_bytes());
        out.extend_from_slice(&entsize.to_le_bytes());
    };
    section_header(&mut out, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    for (i, s) in sections.iter().enumerate() {
        let mut flags = SHF_ALLOC;
        if s.writable {
            flags |= SHF_WRITE;
        }
        if s.executable {
            flags |= SHF_EXECINSTR;
        }
        let kind = if s.nobits { SHT_NOBITS } else { SHT_PROGBITS };
        section_header(
            &mut out,
            sh_names[i],
            kind,
            flags,
            s.address,
            file_offsets[i],
            s.mem_bytes,
            0,
            0,
            OUT_SECTION_ALIGN,
            0,
        );
    }
    let strtab_index = (sections.len() + 2) as u32;
    section_header(
        &mut out,
        symtab_name,
        SHT_SYMTAB,
        0,
        0,
        symtab_off,
        symtab.len() as u64,
        strtab_index,
        first_global as u32,
        ELF64_TABLE_ALIGN,
        u64::from(SYM_BYTES),
    );
    section_header(
        &mut out,
        strtab_name,
        SHT_STRTAB,
        0,
        0,
        strtab_off,
        strtab.len() as u64,
        0,
        0,
        1,
        0,
    );
    section_header(
        &mut out,
        shstrtab_name,
        SHT_STRTAB,
        0,
        0,
        shstrtab_off,
        shstrtab.len() as u64,
        0,
        0,
        1,
        0,
    );
    // ELF header.
    let ehdr_bytes = usize::from(EHDR_BYTES);
    let phdr_bytes = usize::from(PHDR_BYTES);
    let mut h = Vec::with_capacity(ehdr_bytes);
    h.extend_from_slice(ELFMAG);
    h.extend_from_slice(&[ELFCLASS64, ELFDATA2LSB, EV_CURRENT, ELFOSABI_NONE]);
    h.extend_from_slice(&[0; 8]);
    h.extend_from_slice(&ET_EXEC.to_le_bytes());
    h.extend_from_slice(&machine.to_le_bytes());
    h.extend_from_slice(&u32::from(EV_CURRENT).to_le_bytes());
    h.extend_from_slice(&entry.to_le_bytes());
    h.extend_from_slice(&ehsize.to_le_bytes()); // phoff
    h.extend_from_slice(&shoff.to_le_bytes());
    h.extend_from_slice(&e_flags.to_le_bytes());
    h.extend_from_slice(&EHDR_BYTES.to_le_bytes());
    h.extend_from_slice(&PHDR_BYTES.to_le_bytes());
    h.extend_from_slice(&(phnum as u16).to_le_bytes());
    h.extend_from_slice(&SHDR_BYTES.to_le_bytes());
    h.extend_from_slice(&(shnum as u16).to_le_bytes());
    h.extend_from_slice(&((shnum - 1) as u16).to_le_bytes());
    out[..ehdr_bytes].copy_from_slice(&h);
    for (i, (seg, (vaddr, off, filesz, memsz))) in segments.iter().zip(&seg_info).enumerate() {
        let p = ehdr_bytes + i * phdr_bytes;
        let mut ph = Vec::with_capacity(phdr_bytes);
        ph.extend_from_slice(&PT_LOAD.to_le_bytes());
        ph.extend_from_slice(&seg.flags.to_le_bytes());
        ph.extend_from_slice(&off.to_le_bytes());
        ph.extend_from_slice(&vaddr.to_le_bytes());
        ph.extend_from_slice(&vaddr.to_le_bytes());
        ph.extend_from_slice(&filesz.to_le_bytes());
        ph.extend_from_slice(&memsz.to_le_bytes());
        ph.extend_from_slice(&PAGE_BYTES.to_le_bytes());
        out[p..p + phdr_bytes].copy_from_slice(&ph);
    }
    Ok((out, seg_info))
}
