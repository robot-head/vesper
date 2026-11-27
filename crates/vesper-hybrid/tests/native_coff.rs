//! AMD64 COFF writer differentials for the native backend.

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/native_backend.rs"]
mod binding;

use object::{
    Architecture, LittleEndian as LE, Object, ObjectSection, pe, read::coff::ImageSymbol,
};
use std::collections::HashMap;

const RELOC_COUNT: usize = 2;
const RAW_SYMBOL_BYTES: usize = pe::IMAGE_SIZEOF_SYMBOL;
pub(crate) const TARGET: &str = "x86_64-pc-windows-msvc";

// Copy only section payloads, names, auxiliary data, and semantic fields
// from the reference. The Vesper writer builds all the surrounding tables.
pub(crate) fn request(reference: &[u8]) -> Option<(Vec<u64>, Vec<u8>)> {
    let object::File::Coff(file) = object::File::parse(reference).ok()? else {
        return None;
    };
    if file.architecture() != Architecture::X86_64 {
        return None;
    }
    let symtab = file.coff_symbol_table();
    let header = file.coff_header();
    let mut words = vec![
        file.sections().count() as u64,
        symtab.iter().count() as u64,
        0,
        u64::from(header.characteristics.get(LE)),
    ];
    let mut bytes = Vec::new();
    let append = |out: &mut Vec<u8>, slice: &[u8]| {
        let at = out.len() as u64;
        out.extend_from_slice(slice);
        at
    };
    for section in file.sections() {
        let raw = section.coff_section();
        if raw.number_of_linenumbers.get(LE) != 0
            || raw.virtual_address.get(LE) != 0
            || raw.virtual_size.get(LE) != 0
        {
            return None;
        }
        let name = section.name_bytes().ok()?;
        words.extend([
            append(&mut bytes, name),
            name.len() as u64,
            append(&mut bytes, section.data().ok()?),
            u64::from(raw.size_of_raw_data.get(LE)),
            u64::from(raw.characteristics.get(LE)),
        ]);
    }
    let mut indices = HashMap::new();
    for (index, (raw_index, symbol)) in symtab.iter().enumerate() {
        let name = symbol.name(symtab.strings()).ok()?;
        let count = symbol.number_of_aux_symbols;
        let aux = if count == 0 {
            &[]
        } else {
            let start = (header.pointer_to_symbol_table.get(LE) as usize)
                .checked_add(raw_index.0.checked_add(1)?.checked_mul(RAW_SYMBOL_BYTES)?)?;
            let end = start.checked_add(usize::from(count) * RAW_SYMBOL_BYTES)?;
            reference.get(start..end)?
        };
        words.extend([
            append(&mut bytes, name),
            name.len() as u64,
            u64::from(symbol.value.get(LE)),
            u64::from(symbol.section_number.get(LE)),
            u64::from(symbol.typ.get(LE)),
            u64::from(symbol.storage_class),
            append(&mut bytes, aux),
            u64::from(count),
        ]);
        indices.insert(raw_index.0, index as u64);
    }
    for (index, section) in file.sections().enumerate() {
        if section.coff_section().number_of_relocations.get(LE) == 0 {
            continue;
        }
        for relocation in section.coff_relocations().ok()? {
            words[RELOC_COUNT] += 1;
            words.extend([
                index as u64,
                u64::from(relocation.virtual_address.get(LE)),
                *indices.get(&(relocation.symbol_table_index.get(LE) as usize))?,
                u64::from(relocation.typ.get(LE)),
            ]);
        }
    }
    Some((words, bytes))
}

pub(crate) fn candidate(words: &[u64], bytes: &[u8]) -> Vec<u8> {
    binding::x86_64_coff_object(words, bytes)
}

pub(crate) fn fixture(source: &str) -> Vec<u8> {
    let parsed = vesper::parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = vesper::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    vesper::native::object_with_options(&checked.world, "main", Some(TARGET), true)
        .unwrap()
        .0
}

#[test]
fn coff_matches_cranelift_sections_relocations_and_windows_unwind() {
    let baseline = fixture(include_str!("../../../examples/fibonacci.ves"));
    let (mut words, bytes) = request(&baseline).unwrap();
    let generated = candidate(&words, &bytes);
    assert_eq!(request(&generated), Some((words.clone(), bytes.clone())));
    let file = object::File::parse(generated.as_slice()).unwrap();
    for name in [".text", ".pdata", ".xdata"] {
        assert!(file.section_by_name(name).is_some(), "{name}");
    }
    let pdata = file.section_by_name(".pdata").unwrap();
    assert!(pdata.relocations().any(|(_, relocation)| {
        relocation.flags()
            == object::RelocationFlags::Coff {
                typ: pe::IMAGE_REL_AMD64_ADDR32NB,
            }
    }));
    words[RELOC_COUNT] += 1;
    assert!(candidate(&words, &bytes).is_empty());
}

#[test]
fn coff_preserves_long_names_zero_fill_and_auxiliary_symbol_indices() {
    use object::{
        BinaryFormat, Endianness, RelocationFlags, SectionKind, SymbolFlags, SymbolKind,
        SymbolScope,
        write::{Object as WriteObject, Relocation, Symbol, SymbolSection},
    };

    const POINTER_BYTES: usize = size_of::<u64>();
    const LONG_NAME: &[u8] = b".long_named_data";
    let mut baseline =
        WriteObject::new(BinaryFormat::Coff, Architecture::X86_64, Endianness::Little);
    baseline.add_file_symbol(b"a long compilation unit name.ves".to_vec());
    let section = baseline.add_section(Vec::new(), LONG_NAME.to_vec(), SectionKind::ReadOnlyData);
    baseline.set_section_data(section, vec![0; POINTER_BYTES], POINTER_BYTES as u64);
    let _ = baseline.section_symbol(section);
    let zero = baseline.add_section(Vec::new(), b".bss".to_vec(), SectionKind::UninitializedData);
    baseline.append_section_bss(zero, POINTER_BYTES as u64, POINTER_BYTES as u64);
    let symbol = baseline.add_symbol(Symbol {
        name: b"sh7_external_reference".to_vec(),
        value: 0,
        size: 0,
        kind: SymbolKind::Data,
        scope: SymbolScope::Unknown,
        weak: false,
        section: SymbolSection::Undefined,
        flags: SymbolFlags::None,
    });
    baseline
        .add_relocation(
            section,
            Relocation {
                offset: 0,
                symbol,
                addend: 12,
                flags: RelocationFlags::Coff {
                    typ: pe::IMAGE_REL_AMD64_ADDR64,
                },
            },
        )
        .unwrap();
    let baseline = baseline.write().unwrap();
    let (words, bytes) = request(&baseline).unwrap();
    assert_eq!(request(&candidate(&words, &bytes)), Some((words, bytes)));
}
