use super::binding;
use object::write::{Object as WriteObject, Relocation, Symbol, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, Object, ObjectSection, ObjectSymbol, RelocationFlags,
    RelocationTarget, SectionKind, SymbolFlags, SymbolKind, SymbolScope, elf,
};
use std::collections::BTreeMap;

const ENTRY: &[u8] = b"sh7_entry";
const EXTERNAL: &[u8] = b"sh7_plus";
const LOCAL: &[u8] = b"sh7_local";
const ADDRESS: &[u8] = b"sh7_addr";
const GOT_CALL: &[u8] = b"sh7_via_got";
const READ: &[u8] = b"sh7_read";
const CONSTANT: &[u8] = b"sh7_constant";
const TABLE: &[u8] = b"sh7_table";
const WRITABLE: &[u8] = b"sh7_writable";
const ZEROED: &[u8] = b"sh7_zeroed";
const IMPORTED_DATA: &[u8] = b"sh7_foreign";
const NO_SECTION: u64 = u64::MAX;
const POINTER_SIZE: u64 = 8;
const TABLE_BYTES: u64 = 2 * POINTER_SIZE;
const BSS_BYTES: u64 = 4 * POINTER_SIZE;
const TABLE_DATA: [u8; TABLE_BYTES as usize] = [0; TABLE_BYTES as usize];
const WRITABLE_DATA: [u8; TABLE_BYTES as usize] = [0, 0, 0, 0, 0, 0, 0, 0, 71, 0, 0, 0, 0, 0, 0, 0];
const TEXT_ALIGNMENT: u64 = 16;
const RODATA_ALIGNMENT: u64 = 16;
const RODATA: &[u8] = &[13, 41, 37];
const TEXT: &[u8] = &[
    0x48, 0x83, 0xec, 0x08, // sub rsp, 8
    0xe8, 0, 0, 0, 0, // call sh7_local
    0x89, 0xc7, // mov edi, eax
    0xe8, 0, 0, 0, 0, // call sh7_plus
    0x48, 0x83, 0xc4, 0x08, // add rsp, 8
    0xc3, // ret
    0, 0, 0, // pad to the helper
    0xb8, 17, 0, 0, 0,    // mov eax, 17
    0xc3, // ret
    0, 0, // pad to the address helper
    0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0,    // movabs rax, sh7_entry + 3
    0xc3, // ret
    0, 0, 0, 0, 0, // pad to the GOT call helper
    0x48, 0x83, 0xec, 0x08, // sub rsp, 8
    0xbf, 17, 0, 0, 0, // mov edi, 17
    0xff, 0x15, 0, 0, 0, 0, // call [rip + sh7_plus@GOTPCREL]
    0x48, 0x83, 0xc4, 0x08, // add rsp, 8
    0xc3, // ret
    0x0f, 0xb6, 0x05, 0, 0, 0, 0,    // movzx eax, byte ptr [rip + sh7_constant]
    0xc3, // ret
];
const ENTRY_SIZE: u64 = 21;
const LOCAL_START: u64 = 24;
const LOCAL_SIZE: u64 = 6;
const ADDRESS_START: u64 = 32;
const ADDRESS_SIZE: u64 = 11;
const GOT_START: u64 = 48;
const GOT_SIZE: u64 = 20;
const READ_START: u64 = 68;
const READ_SIZE: u64 = 8;

fn request() -> (Vec<u64>, Vec<u8>) {
    let mut words = vec![
        11,
        TEXT.len() as u64,
        TEXT_ALIGNMENT,
        RODATA.len() as u64,
        RODATA_ALIGNMENT,
        TABLE_BYTES,
        POINTER_SIZE,
        TABLE_BYTES,
        POINTER_SIZE,
        BSS_BYTES,
        POINTER_SIZE,
    ];
    let mut bytes = TEXT.to_vec();
    bytes.extend(RODATA);
    bytes.extend(TABLE_DATA);
    bytes.extend(WRITABLE_DATA);
    for (name, value, size, kind, section) in [
        (ENTRY, 0, ENTRY_SIZE, 1, 0),
        (EXTERNAL, 0, 0, 2, NO_SECTION),
        (LOCAL, LOCAL_START, LOCAL_SIZE, 0, 0),
        (ADDRESS, ADDRESS_START, ADDRESS_SIZE, 3, 0),
        (GOT_CALL, GOT_START, GOT_SIZE, 1, 0),
        (READ, READ_START, READ_SIZE, 1, 0),
        (CONSTANT, 1, 1, 0, 1),
        (TABLE, 0, TABLE_BYTES, 1, 2),
        (WRITABLE, 0, TABLE_BYTES, 1, 3),
        (ZEROED, 0, BSS_BYTES, 1, 4),
        (IMPORTED_DATA, 0, 0, 2, NO_SECTION),
    ] {
        words.extend([
            bytes.len() as u64,
            name.len() as u64,
            value,
            size,
            kind,
            section,
        ]);
        bytes.extend(name);
    }
    words.extend([0, 5, 2, 2, (-4i64) as u64]);
    words.extend([0, 12, 1, 3, (-4i64) as u64]);
    words.extend([0, 34, 0, 1, 3]);
    words.extend([0, 59, 1, 4, (-4i64) as u64]);
    words.extend([0, 71, 6, 2, (-4i64) as u64]);
    words.extend([2, 0, 0, 1, 0]);
    words.extend([2, POINTER_SIZE, 9, 1, POINTER_SIZE]);
    words.extend([3, 0, 10, 1, POINTER_SIZE]);
    (words, bytes)
}

fn reference() -> Vec<u8> {
    let mut object = WriteObject::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let text = object.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    object.append_section_data(text, TEXT, TEXT_ALIGNMENT);
    let rodata = object.add_section(Vec::new(), b".rodata".to_vec(), SectionKind::ReadOnlyData);
    object.append_section_data(rodata, RODATA, RODATA_ALIGNMENT);
    let relro = object.add_section(
        Vec::new(),
        b".data.rel.ro".to_vec(),
        SectionKind::ReadOnlyDataWithRel,
    );
    object.append_section_data(relro, &TABLE_DATA, POINTER_SIZE);
    let writable = object.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
    object.append_section_data(writable, &WRITABLE_DATA, POINTER_SIZE);
    let bss = object.add_section(Vec::new(), b".bss".to_vec(), SectionKind::UninitializedData);
    object.append_section_bss(bss, BSS_BYTES, POINTER_SIZE);
    object.add_section(Vec::new(), b".note.GNU-stack".to_vec(), SectionKind::Other);
    let mut add = |name: &[u8], value, size, scope, section| {
        object.add_symbol(Symbol {
            name: name.to_vec(),
            value,
            size,
            kind: if section == SymbolSection::Undefined {
                SymbolKind::Unknown
            } else if section == SymbolSection::Section(text) {
                SymbolKind::Text
            } else {
                SymbolKind::Data
            },
            scope,
            weak: false,
            section,
            flags: SymbolFlags::None,
        })
    };
    let entry = add(
        ENTRY,
        0,
        ENTRY_SIZE,
        SymbolScope::Dynamic,
        SymbolSection::Section(text),
    );
    let imported = add(
        EXTERNAL,
        0,
        0,
        SymbolScope::Unknown,
        SymbolSection::Undefined,
    );
    let local = add(
        LOCAL,
        LOCAL_START,
        LOCAL_SIZE,
        SymbolScope::Compilation,
        SymbolSection::Section(text),
    );
    add(
        ADDRESS,
        ADDRESS_START,
        ADDRESS_SIZE,
        SymbolScope::Linkage,
        SymbolSection::Section(text),
    );
    add(
        GOT_CALL,
        GOT_START,
        GOT_SIZE,
        SymbolScope::Dynamic,
        SymbolSection::Section(text),
    );
    add(
        READ,
        READ_START,
        READ_SIZE,
        SymbolScope::Dynamic,
        SymbolSection::Section(text),
    );
    let constant = add(
        CONSTANT,
        1,
        1,
        SymbolScope::Compilation,
        SymbolSection::Section(rodata),
    );
    add(
        TABLE,
        0,
        TABLE_BYTES,
        SymbolScope::Dynamic,
        SymbolSection::Section(relro),
    );
    add(
        WRITABLE,
        0,
        TABLE_BYTES,
        SymbolScope::Dynamic,
        SymbolSection::Section(writable),
    );
    let zeroed = add(
        ZEROED,
        0,
        BSS_BYTES,
        SymbolScope::Dynamic,
        SymbolSection::Section(bss),
    );
    let foreign = add(
        IMPORTED_DATA,
        0,
        0,
        SymbolScope::Unknown,
        SymbolSection::Undefined,
    );
    for (section, offset, symbol, r_type, addend) in [
        (text, 5, local, elf::R_X86_64_PC32, -4),
        (text, 12, imported, elf::R_X86_64_PLT32, -4),
        (text, 34, entry, elf::R_X86_64_64, 3),
        (text, 59, imported, elf::R_X86_64_GOTPCREL, -4),
        (text, 71, constant, elf::R_X86_64_PC32, -4),
        (relro, 0, entry, elf::R_X86_64_64, 0),
        (
            relro,
            POINTER_SIZE,
            zeroed,
            elf::R_X86_64_64,
            POINTER_SIZE as i64,
        ),
        (writable, 0, foreign, elf::R_X86_64_64, POINTER_SIZE as i64),
    ] {
        object
            .add_relocation(
                section,
                Relocation {
                    offset,
                    symbol,
                    addend,
                    flags: RelocationFlags::Elf { r_type },
                },
            )
            .unwrap();
    }
    object.write().unwrap()
}

#[test]
fn elf_writer_matches_sections_symbols_and_relocations() {
    let (words, bytes) = request();
    let output = binding::x86_64_elf_object(&words, &bytes);
    let expected = reference();
    let candidate = object::File::parse(output.as_slice()).unwrap();
    let baseline = object::File::parse(expected.as_slice()).unwrap();
    assert_eq!(candidate.kind(), object::ObjectKind::Relocatable);
    assert_eq!(candidate.architecture(), Architecture::X86_64);
    assert_eq!(candidate.endianness(), Endianness::Little);
    for name in [".text", ".rodata", ".data.rel.ro", ".data", ".bss"] {
        let section = candidate.section_by_name(name).unwrap();
        let reference_section = baseline.section_by_name(name).unwrap();
        assert_eq!(section.align(), reference_section.align());
        assert_eq!(section.flags(), reference_section.flags());
        assert_eq!(section.size(), reference_section.size());
        assert_eq!(section.data().unwrap(), reference_section.data().unwrap());
    }
    let symbols = |file: &object::File<'_>| {
        file.symbols()
            .map(|symbol| {
                (
                    symbol.name_bytes().unwrap().to_vec(),
                    (
                        symbol.address(),
                        symbol.size(),
                        symbol.scope(),
                        symbol.flags(),
                        symbol.section_index().map(|index| {
                            file.section_by_index(index)
                                .unwrap()
                                .name()
                                .unwrap()
                                .to_owned()
                        }),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(symbols(&candidate), symbols(&baseline));
    let relocations = |file: &object::File<'_>, name: &str| {
        file.section_by_name(name)
            .unwrap()
            .relocations()
            .map(|(offset, relocation)| {
                let RelocationTarget::Symbol(index) = relocation.target() else {
                    panic!("expected a symbol relocation");
                };
                let symbol = file.symbol_by_index(index).unwrap();
                (
                    offset,
                    symbol.name_bytes().unwrap().to_vec(),
                    relocation.flags(),
                    relocation.addend(),
                )
            })
            .collect::<Vec<_>>()
    };
    for name in [".text", ".data.rel.ro", ".data"] {
        assert_eq!(relocations(&candidate, name), relocations(&baseline, name));
    }
    let stack = candidate.section_by_name(".note.GNU-stack").unwrap();
    assert_eq!(stack.flags(), object::SectionFlags::Elf { sh_flags: 0 });
    assert!(stack.data().unwrap().is_empty());
    assert!(binding::x86_64_elf_object(&words[..words.len() - 1], &bytes).is_empty());
    assert!(binding::x86_64_elf_object(&words, &bytes[..bytes.len() - 1]).is_empty());
    let mut invalid = words;
    invalid[4] = 0;
    assert!(binding::x86_64_elf_object(&invalid, &bytes).is_empty());
    invalid[4] = RODATA_ALIGNMENT;
    invalid[2] = 3;
    assert!(binding::x86_64_elf_object(&invalid, &bytes).is_empty());
}

#[test]
fn elf_writer_links_an_import_and_local_call() {
    use std::fs;
    use std::process::Command;

    if Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("vesper-sh7-elf-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let source = dir.join("main.c");
    fs::write(
        &source,
        "#include <stdint.h>\n\
         extern int sh7_entry(void);\n\
         extern uintptr_t sh7_addr(void);\n\
         extern int sh7_via_got(void);\n\
         extern int sh7_read(void);\n\
         extern uintptr_t sh7_table[2], sh7_writable[2], sh7_zeroed[4];\n\
         uintptr_t sh7_foreign[2] = {19, 23};\n\
         int sh7_plus(int n) { return n + 5; }\n\
         int main(void) {\n\
             return sh7_entry() == 22 && sh7_via_got() == 22 && sh7_read() == 41\n\
                 && sh7_addr() == (uintptr_t)sh7_entry + 3\n\
                 && sh7_table[0] == (uintptr_t)sh7_entry\n\
                 && sh7_table[1] == (uintptr_t)&sh7_zeroed[1]\n\
                 && sh7_writable[0] == (uintptr_t)&sh7_foreign[1]\n\
                 && sh7_writable[1] == 71 && sh7_zeroed[3] == 0 ? 0 : 1;\n\
         }\n",
    )
    .unwrap();
    let (words, bytes) = request();
    for (name, object) in [
        ("vesper", binding::x86_64_elf_object(&words, &bytes)),
        ("reference", reference()),
    ] {
        let relocatable = dir.join(format!("{name}.o"));
        let exe = dir.join(name);
        fs::write(&relocatable, object).unwrap();
        let output = Command::new("cc")
            .arg("-no-pie")
            .arg(&source)
            .arg(&relocatable)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(Command::new(&exe).status().unwrap().success(), "{name}");
    }
    fs::remove_dir_all(dir).unwrap();
}
