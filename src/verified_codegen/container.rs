//! Output containers: a static Linux ELF executable (one `PT_LOAD`, no
//! sections, no libc, no linker) and a Mach-O AArch64 object with one
//! `__TEXT,__text` section exporting `_main`, for the system linker.
//!
//! Both are written directly, deterministically (no timestamps, no paths), so
//! the same program always gives the same bytes.

/// Virtual address the ELF image is loaded at.
pub const ELF_BASE: u64 = 0x40_0000;
/// File offset (and image offset) of the code in the ELF executable.
pub const ELF_CODE_OFFSET: u64 = 0x80;

pub const EM_X86_64: u16 = 62;
pub const EM_AARCH64: u16 = 183;
pub const EM_RISCV: u16 = 243;

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// File offset of the code when the image also maps the program heap (two
/// program headers do not fit below [`ELF_CODE_OFFSET`]).
pub const ELF_HEAP_CODE_OFFSET: u64 = 0x100;

/// A static `ET_EXEC` image whose entry point is the first code byte. With
/// `heap`, a second, zero-fill read-write `PT_LOAD` maps
/// [`super::HEAP_WORDS`] words at [`super::HEAP_BASE`] (no file bytes).
pub fn elf_executable(machine: u16, code: &[u8], heap: bool) -> Vec<u8> {
    let code_offset = if heap {
        ELF_HEAP_CODE_OFFSET
    } else {
        ELF_CODE_OFFSET
    };
    let mut out = Vec::with_capacity(code_offset as usize + code.len());
    // e_ident: ELFCLASS64, ELFDATA2LSB, EV_CURRENT, ELFOSABI_SYSV.
    out.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
    out.extend_from_slice(&[0; 8]);
    put16(&mut out, 2); // e_type = ET_EXEC
    put16(&mut out, machine);
    put32(&mut out, 1); // e_version
    put64(&mut out, ELF_BASE + code_offset); // e_entry
    put64(&mut out, 64); // e_phoff
    put64(&mut out, 0); // e_shoff
    put32(&mut out, 0); // e_flags
    put16(&mut out, 64); // e_ehsize
    put16(&mut out, 56); // e_phentsize
    put16(&mut out, if heap { 2 } else { 1 }); // e_phnum
    put16(&mut out, 64); // e_shentsize
    put16(&mut out, 0); // e_shnum
    put16(&mut out, 0); // e_shstrndx
    // PT_LOAD, R+X, the whole file at ELF_BASE.
    let size = code_offset + code.len() as u64;
    put32(&mut out, 1); // p_type = PT_LOAD
    put32(&mut out, 5); // p_flags = PF_R | PF_X
    put64(&mut out, 0); // p_offset
    put64(&mut out, ELF_BASE); // p_vaddr
    put64(&mut out, ELF_BASE); // p_paddr
    put64(&mut out, size); // p_filesz
    put64(&mut out, size); // p_memsz
    put64(&mut out, 0x1000); // p_align
    if heap {
        // PT_LOAD, R+W, zero fill.
        put32(&mut out, 1);
        put32(&mut out, 6); // PF_R | PF_W
        put64(&mut out, 0);
        put64(&mut out, super::HEAP_BASE);
        put64(&mut out, super::HEAP_BASE);
        put64(&mut out, 0); // p_filesz
        put64(&mut out, super::HEAP_WORDS * 8); // p_memsz
        put64(&mut out, 0x1000);
    }
    out.resize(code_offset as usize, 0);
    out.extend_from_slice(code);
    out
}

const MH_MAGIC_64: u32 = 0xfeed_facf;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
const MH_OBJECT: u32 = 1;
const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_DYSYMTAB: u32 = 0xb;
const LC_BUILD_VERSION: u32 = 0x32;
const PLATFORM_MACOS: u32 = 1;

fn name16(out: &mut Vec<u8>, name: &str) {
    let mut b = [0u8; 16];
    b[..name.len()].copy_from_slice(name.as_bytes());
    out.extend_from_slice(&b);
}

/// A relocation-free `MH_OBJECT` with `_main` at the start of `__text`.
pub fn macho_object(code: &[u8]) -> Vec<u8> {
    let segment = 72 + 80;
    let symtab = 24;
    let dysymtab = 80;
    let build = 24;
    let sizeofcmds = segment + symtab + dysymtab + build;
    let code_offset = (32 + sizeofcmds + 15) & !15;
    let symoff = (code_offset + code.len() as u32 + 7) & !7;
    let stroff = symoff + 16;
    let strings = b"\0_main\0\0";
    let mut out = vec![];
    put32(&mut out, MH_MAGIC_64);
    put32(&mut out, CPU_TYPE_ARM64);
    put32(&mut out, 0); // cpusubtype ALL
    put32(&mut out, MH_OBJECT);
    put32(&mut out, 4); // ncmds
    put32(&mut out, sizeofcmds);
    put32(&mut out, 0); // flags
    put32(&mut out, 0); // reserved
    // LC_SEGMENT_64 with one section.
    put32(&mut out, LC_SEGMENT_64);
    put32(&mut out, segment);
    name16(&mut out, "");
    put64(&mut out, 0); // vmaddr
    put64(&mut out, code.len() as u64); // vmsize
    put64(&mut out, u64::from(code_offset)); // fileoff
    put64(&mut out, code.len() as u64); // filesize
    put32(&mut out, 7); // maxprot
    put32(&mut out, 7); // initprot
    put32(&mut out, 1); // nsects
    put32(&mut out, 0); // flags
    name16(&mut out, "__text");
    name16(&mut out, "__TEXT");
    put64(&mut out, 0); // addr
    put64(&mut out, code.len() as u64); // size
    put32(&mut out, code_offset); // offset
    put32(&mut out, 2); // align 2^2
    put32(&mut out, 0); // reloff
    put32(&mut out, 0); // nreloc
    put32(&mut out, 0x8000_0400); // S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS
    put32(&mut out, 0);
    put32(&mut out, 0);
    put32(&mut out, 0);
    // LC_BUILD_VERSION: macOS 11.0, no tools.
    put32(&mut out, LC_BUILD_VERSION);
    put32(&mut out, build);
    put32(&mut out, PLATFORM_MACOS);
    put32(&mut out, 0x000b_0000); // minos
    put32(&mut out, 0); // sdk
    put32(&mut out, 0); // ntools
    // LC_SYMTAB
    put32(&mut out, LC_SYMTAB);
    put32(&mut out, symtab);
    put32(&mut out, symoff);
    put32(&mut out, 1);
    put32(&mut out, stroff);
    put32(&mut out, strings.len() as u32);
    // LC_DYSYMTAB: one external defined symbol.
    put32(&mut out, LC_DYSYMTAB);
    put32(&mut out, dysymtab);
    for v in [0u32, 0, 0, 1, 1, 0] {
        put32(&mut out, v);
    }
    out.resize(out.len() + 12 * 4, 0);
    out.resize(code_offset as usize, 0);
    out.extend_from_slice(code);
    out.resize(symoff as usize, 0);
    // nlist_64 for _main: N_SECT | N_EXT in section 1 at 0.
    put32(&mut out, 1);
    out.extend_from_slice(&[0x0f, 1]);
    put16(&mut out, 0);
    put64(&mut out, 0);
    out.extend_from_slice(strings);
    out
}

/// The Windows x64 `UNWIND_INFO` (version 1, no handler, no frame register)
/// of a prologue `push rbp; mov rbp, rsp; [probes;] sub rsp, frame`: the
/// unwinder restores `rsp` by the fixed allocation and pops `rbp`, which is
/// valid because function bodies never move `rsp`.
pub fn unwind_info(p: &super::asm::Prologue) -> Vec<u8> {
    let mut codes: Vec<u16> = vec![];
    if p.frame > 0 {
        if p.frame <= 128 {
            // UWOP_ALLOC_SMALL
            codes.push(u16::from(p.alloc_end) | (2 | (((p.frame - 8) / 8) as u16) << 4) << 8);
        } else if p.frame <= 512 * 1024 - 8 {
            // UWOP_ALLOC_LARGE, OpInfo 0: the size / 8 in the next slot
            codes.push(u16::from(p.alloc_end) | 1 << 8);
            codes.push((p.frame / 8) as u16);
        } else {
            // UWOP_ALLOC_LARGE, OpInfo 1: the size in the next two slots
            codes.push(u16::from(p.alloc_end) | (1 | 1 << 4) << 8);
            codes.push((p.frame & 0xffff) as u16);
            codes.push((p.frame >> 16) as u16);
        }
    }
    // UWOP_PUSH_NONVOL rbp (register 5)
    codes.push(u16::from(p.push_end) | (5 << 4) << 8);
    let count = codes.len() as u8;
    if codes.len() % 2 == 1 {
        codes.push(0);
    }
    let mut out = vec![1, p.alloc_end, count, 0];
    for c in codes {
        put16(&mut out, c);
    }
    out
}

const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
const IMAGE_REL_AMD64_ADDR32NB: u16 = 3;
const IMAGE_REL_AMD64_REL32: u16 = 4;

/// A COFF symbol name: inline when it fits in eight bytes, else an offset
/// into the string table.
fn coff_name(out: &mut Vec<u8>, name: &str, strings: &mut Vec<u8>) {
    if name.len() <= 8 {
        let mut b = [0u8; 8];
        b[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&b);
    } else {
        put32(out, 0);
        put32(out, 4 + strings.len() as u32);
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
    }
}

/// An x86-64 COFF object for `link.exe`: `.text` with the code and the
/// external `main` at `main`, `.xdata` with one `UNWIND_INFO` per function
/// and `.pdata` with one `RUNTIME_FUNCTION` per function, sorted by address,
/// relocated with `IMAGE_REL_AMD64_ADDR32NB`; calls to `externals` are
/// `IMAGE_REL_AMD64_REL32`. No timestamps: deterministic bytes.
pub fn coff_object(
    code: &[u8],
    main: usize,
    prologues: &[super::asm::Prologue],
    externals: &[(usize, &'static str)],
) -> Vec<u8> {
    // Function ranges in address order.
    let mut functions: Vec<(u32, u32, Vec<u8>)> = vec![];
    let mut sorted = prologues.to_vec();
    sorted.sort_by_key(|p| p.start);
    for (i, p) in sorted.iter().enumerate() {
        let end = p
            .end
            .or_else(|| sorted.get(i + 1).map(|n| n.start))
            .unwrap_or(code.len());
        functions.push((p.start as u32, end as u32, unwind_info(p)));
    }
    let mut xdata = vec![];
    let mut pdata = vec![];
    let mut pdata_relocs = vec![];
    for (begin, end, info) in &functions {
        let offset = xdata.len() as u32;
        xdata.extend_from_slice(info);
        let row = pdata.len() as u32;
        put32(&mut pdata, *begin);
        put32(&mut pdata, *end);
        put32(&mut pdata, offset);
        // Symbol 0 is `.text`, symbol 1 is `.xdata`.
        pdata_relocs.push((row, 0u32));
        pdata_relocs.push((row + 4, 0));
        pdata_relocs.push((row + 8, 1));
    }
    let mut names: Vec<&'static str> = externals.iter().map(|(_, n)| *n).collect();
    names.sort_unstable();
    names.dedup();
    let symbol_of = |name: &str| 3 + names.iter().position(|n| *n == name).unwrap_or(0) as u32;

    let header = 20;
    let sections = 3 * 40;
    let text_at = header + sections;
    let text_relocs_at = text_at + code.len();
    let xdata_at = text_relocs_at + externals.len() * 10;
    let pdata_at = xdata_at + xdata.len();
    let pdata_relocs_at = pdata_at + pdata.len();
    let symbols_at = pdata_relocs_at + pdata_relocs.len() * 10;
    let symbol_count = 3 + names.len();

    let mut out = vec![];
    put16(&mut out, IMAGE_FILE_MACHINE_AMD64);
    put16(&mut out, 3); // sections
    put32(&mut out, 0); // timestamp
    put32(&mut out, symbols_at as u32);
    put32(&mut out, symbol_count as u32);
    put16(&mut out, 0); // optional header
    put16(&mut out, 0); // characteristics
    let section = |out: &mut Vec<u8>,
                   name: &[u8; 8],
                   size: usize,
                   at: usize,
                   relocs_at: usize,
                   relocs: usize,
                   flags: u32| {
        out.extend_from_slice(name);
        put32(out, 0);
        put32(out, 0);
        put32(out, size as u32);
        put32(out, at as u32);
        put32(out, if relocs > 0 { relocs_at as u32 } else { 0 });
        put32(out, 0);
        put16(out, relocs as u16);
        put16(out, 0);
        put32(out, flags);
    };
    // CNT_CODE | ALIGN_16BYTES | MEM_EXECUTE | MEM_READ
    section(
        &mut out,
        b".text\0\0\0",
        code.len(),
        text_at,
        text_relocs_at,
        externals.len(),
        0x6050_0020,
    );
    // CNT_INITIALIZED_DATA | ALIGN_4BYTES | MEM_READ
    section(
        &mut out,
        b".xdata\0\0",
        xdata.len(),
        xdata_at,
        0,
        0,
        0x4030_0040,
    );
    section(
        &mut out,
        b".pdata\0\0",
        pdata.len(),
        pdata_at,
        pdata_relocs_at,
        pdata_relocs.len(),
        0x4030_0040,
    );
    out.extend_from_slice(code);
    for (at, name) in externals {
        put32(&mut out, *at as u32);
        put32(&mut out, symbol_of(name));
        put16(&mut out, IMAGE_REL_AMD64_REL32);
    }
    out.extend_from_slice(&xdata);
    out.extend_from_slice(&pdata);
    for (at, symbol) in &pdata_relocs {
        put32(&mut out, *at);
        put32(&mut out, *symbol);
        put16(&mut out, IMAGE_REL_AMD64_ADDR32NB);
    }
    let mut strings = vec![];
    // (name, value, section, type, storage class)
    let mut symbol =
        |out: &mut Vec<u8>, name: &str, value: u32, section: i16, ty: u16, class: u8| {
            coff_name(out, name, &mut strings);
            put32(out, value);
            put16(out, section as u16);
            put16(out, ty);
            out.push(class);
            out.push(0);
        };
    symbol(&mut out, ".text", 0, 1, 0, 3);
    symbol(&mut out, ".xdata", 0, 2, 0, 3);
    symbol(&mut out, "main", main as u32, 1, 0x20, 2);
    for name in &names {
        symbol(&mut out, name, 0, 0, 0x20, 2);
    }
    put32(&mut out, 4 + strings.len() as u32);
    out.extend_from_slice(&strings);
    out
}
