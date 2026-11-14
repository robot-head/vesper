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
    if b.len() < 64 || &b[..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    if b[4] != 2 || b[5] != 1 {
        return Err("not a little-endian ELF64 file".into());
    }
    Ok(())
}

/// `e_flags` of an ELF64 file.
pub fn e_flags(b: &[u8]) -> Result<u32, String> {
    check_header(b)?;
    u32_at(b, 48)
}

/// `e_machine` of an ELF64 file.
pub fn e_machine(b: &[u8]) -> Result<u16, String> {
    check_header(b)?;
    u16_at(b, 18)
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
        e_type: u16_at(b, 16)?,
        machine: u16_at(b, 18)?,
        entry: u64_at(b, 24)?,
        ..Default::default()
    };
    let phoff = usize_of(u64_at(b, 32)?)?;
    let shoff = usize_of(u64_at(b, 40)?)?;
    let phentsize = usize::from(u16_at(b, 54)?);
    let phnum = usize::from(u16_at(b, 56)?);
    let shentsize = usize::from(u16_at(b, 58)?);
    let shnum = usize::from(u16_at(b, 60)?);
    if phnum > 0 && phentsize < 56 {
        return Err("bad program header size".into());
    }
    for i in 0..phnum {
        let p = phoff
            .checked_add(i.checked_mul(phentsize).ok_or("overflow")?)
            .ok_or("overflow")?;
        if u32_at(b, p)? == PT_LOAD {
            let flags = u32_at(b, p + 4)?;
            let offset = u64_at(b, p + 8)?;
            let vaddr = u64_at(b, p + 16)?;
            let filesz = u64_at(b, p + 32)?;
            let memsz = u64_at(b, p + 40)?;
            info.segments.push((vaddr, offset, filesz));
            info.segment_memory.push((vaddr, memsz, flags));
        }
    }
    if shnum > 0 && shentsize < 64 {
        return Err("bad section header size".into());
    }
    let section = |i: usize| -> Result<(u32, u64, u64, u32, u64), String> {
        let s = shoff
            .checked_add(i.checked_mul(shentsize).ok_or("overflow")?)
            .ok_or("overflow")?;
        Ok((
            u32_at(b, s + 4)?,  // type
            u64_at(b, s + 24)?, // offset
            u64_at(b, s + 32)?, // size
            u32_at(b, s + 40)?, // link
            u64_at(b, s + 56)?, // entsize
        ))
    };
    for i in 0..shnum {
        let (kind, offset, size, link, entsize) = section(i)?;
        if kind != SHT_SYMTAB {
            continue;
        }
        if entsize < 24 {
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
            let value = u64_at(b, at + 8)?;
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
    const PAGE: u64 = 0x1000;
    let ehsize = 64u64;
    let phnum = segments.len() as u64;
    let mut out = vec![0u8; (ehsize + phnum * 56) as usize];
    // Section file offsets: congruent to the address modulo the page size.
    let mut file_offsets = vec![0u64; sections.len()];
    let mut seg_info = Vec::new();
    for seg in segments {
        let first = &sections[seg.sections.start];
        let mut off = out.len() as u64;
        let want = first.address % PAGE;
        if off % PAGE != want {
            off += (want + PAGE - off % PAGE) % PAGE;
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
    let mut symtab = vec![0u8; 24];
    let mut ordered: Vec<&OutSymbol> = symbols.iter().filter(|s| !s.global).collect();
    let first_global = 1 + ordered.len();
    ordered.extend(symbols.iter().filter(|s| s.global));
    for s in ordered {
        let name = name_of(&s.name, &mut strtab);
        symtab.extend_from_slice(&name.to_le_bytes());
        let bind = if s.global { 1u8 } else { 0 };
        let kind = if s.function { 2u8 } else { 0 };
        symtab.push(bind << 4 | kind);
        symtab.push(0);
        let shndx: u16 = match s.section {
            Some(i) => u16::try_from(i + 1).map_err(|_| "too many sections")?,
            None => 0xfff1,
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
    let symtab_off = place(&symtab, 8, &mut out);
    let strtab_off = place(&strtab, 1, &mut out);
    let shstrtab_off = place(&shstrtab, 1, &mut out);
    let shoff = (out.len() as u64).div_ceil(8) * 8;
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
            8,
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
        8,
        24,
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
    let mut h = Vec::with_capacity(64);
    h.extend_from_slice(b"\x7fELF");
    h.extend_from_slice(&[2, 1, 1, 0]);
    h.extend_from_slice(&[0; 8]);
    h.extend_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    h.extend_from_slice(&machine.to_le_bytes());
    h.extend_from_slice(&1u32.to_le_bytes());
    h.extend_from_slice(&entry.to_le_bytes());
    h.extend_from_slice(&ehsize.to_le_bytes()); // phoff
    h.extend_from_slice(&shoff.to_le_bytes());
    h.extend_from_slice(&e_flags.to_le_bytes());
    h.extend_from_slice(&64u16.to_le_bytes());
    h.extend_from_slice(&56u16.to_le_bytes());
    h.extend_from_slice(&(phnum as u16).to_le_bytes());
    h.extend_from_slice(&64u16.to_le_bytes());
    h.extend_from_slice(&(shnum as u16).to_le_bytes());
    h.extend_from_slice(&((shnum - 1) as u16).to_le_bytes());
    out[..64].copy_from_slice(&h);
    for (i, (seg, (vaddr, off, filesz, memsz))) in segments.iter().zip(&seg_info).enumerate() {
        let p = 64 + i * 56;
        let mut ph = Vec::with_capacity(56);
        ph.extend_from_slice(&PT_LOAD.to_le_bytes());
        ph.extend_from_slice(&seg.flags.to_le_bytes());
        ph.extend_from_slice(&off.to_le_bytes());
        ph.extend_from_slice(&vaddr.to_le_bytes());
        ph.extend_from_slice(&vaddr.to_le_bytes());
        ph.extend_from_slice(&filesz.to_le_bytes());
        ph.extend_from_slice(&memsz.to_le_bytes());
        ph.extend_from_slice(&PAGE.to_le_bytes());
        out[p..p + 56].copy_from_slice(&ph);
    }
    Ok((out, seg_info))
}
