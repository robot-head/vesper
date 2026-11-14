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

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// A static `ET_EXEC` image whose entry point is the first code byte.
pub fn elf_executable(machine: u16, code: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ELF_CODE_OFFSET as usize + code.len());
    // e_ident: ELFCLASS64, ELFDATA2LSB, EV_CURRENT, ELFOSABI_SYSV.
    out.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
    out.extend_from_slice(&[0; 8]);
    put16(&mut out, 2); // e_type = ET_EXEC
    put16(&mut out, machine);
    put32(&mut out, 1); // e_version
    put64(&mut out, ELF_BASE + ELF_CODE_OFFSET); // e_entry
    put64(&mut out, 64); // e_phoff
    put64(&mut out, 0); // e_shoff
    put32(&mut out, 0); // e_flags
    put16(&mut out, 64); // e_ehsize
    put16(&mut out, 56); // e_phentsize
    put16(&mut out, 1); // e_phnum
    put16(&mut out, 64); // e_shentsize
    put16(&mut out, 0); // e_shnum
    put16(&mut out, 0); // e_shstrndx
    // PT_LOAD, R+X, the whole file at ELF_BASE.
    let size = ELF_CODE_OFFSET + code.len() as u64;
    put32(&mut out, 1); // p_type = PT_LOAD
    put32(&mut out, 5); // p_flags = PF_R | PF_X
    put64(&mut out, 0); // p_offset
    put64(&mut out, ELF_BASE); // p_vaddr
    put64(&mut out, ELF_BASE); // p_paddr
    put64(&mut out, size); // p_filesz
    put64(&mut out, size); // p_memsz
    put64(&mut out, 0x1000); // p_align
    out.resize(ELF_CODE_OFFSET as usize, 0);
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
