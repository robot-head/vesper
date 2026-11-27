//! Mach-O writer differentials for the native backend.

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/native_backend.rs"]
mod binding;

use object::{
    Architecture, FileFlags, Object, ObjectSection, ObjectSymbol, SectionFlags, SymbolScope, macho,
};
use std::collections::HashMap;

const ARM64: u64 = 0;
const X86_64: u64 = 1;
const REGULAR: u64 = 0;
const CODE: u64 = 1;
const BSS: u64 = 2;
const CSTRING: u64 = 3;
const LOCAL: u64 = 0;
const EXPORT: u64 = 1;
const HIDDEN: u64 = 2;
const UNDEFINED: u64 = 3;
const NO_SECTION: u64 = u64::MAX;
const RELOC_COUNT: usize = 3;
const MIN_OS: usize = 4;
const MACOS_11: u64 = 0x000b_0000;
const RELOCATION_FLAGS_SHIFT: u32 = 24;

// Turn the reference object's metadata into a request without carrying its
// serialized headers, symbol table or relocation table into the candidate.
pub(crate) fn request(reference: &[u8]) -> Option<(Vec<u64>, Vec<u8>)> {
    let object::File::MachO64(file) = object::File::parse(reference).ok()? else {
        return None;
    };
    let FileFlags::MachO { flags } = file.flags() else {
        return None;
    };
    if !file.is_little_endian() || flags & !macho::MH_SUBSECTIONS_VIA_SYMBOLS != 0 {
        return None;
    }
    let arch = match file.architecture() {
        Architecture::Aarch64 => ARM64,
        Architecture::X86_64 => X86_64,
        _ => return None,
    };
    let endian = file.endian();
    let cpu = file.macho_header().cputype.get(endian);
    let min_os = match file.build_version().ok()? {
        Some(version)
            if version.platform.get(endian) == macho::PLATFORM_MACOS
                && version.sdk.get(endian) == 0
                && version.ntools.get(endian) == 0 =>
        {
            version.minos.get(endian)
        }
        Some(_) => return None,
        None => 0,
    };
    let mut bytes = Vec::new();
    let mut words = vec![
        arch,
        file.sections().count() as u64,
        file.symbols().count() as u64,
        0,
        u64::from(min_os),
        u64::from(flags),
    ];
    let append = |out: &mut Vec<u8>, slice: &[u8]| {
        let offset = out.len() as u64;
        out.extend_from_slice(slice);
        offset
    };
    for section in file.sections() {
        let SectionFlags::MachO { flags } = section.flags() else {
            return None;
        };
        let kind = match flags {
            macho::S_REGULAR => REGULAR,
            flags if flags == macho::S_ATTR_PURE_INSTRUCTIONS | macho::S_ATTR_SOME_INSTRUCTIONS => {
                CODE
            }
            macho::S_ZEROFILL => BSS,
            macho::S_CSTRING_LITERALS => CSTRING,
            _ => return None,
        };
        let segment = section.segment_name_bytes().ok()??;
        let name = section.name_bytes().ok()?;
        words.extend([
            append(&mut bytes, segment),
            segment.len() as u64,
            append(&mut bytes, name),
            name.len() as u64,
            append(&mut bytes, section.data().ok()?),
            section.size(),
            section.align(),
            kind,
        ]);
    }
    let mut indices = HashMap::new();
    for (index, symbol) in file.symbols().enumerate() {
        if symbol.is_weak() {
            return None;
        }
        let (section, value, kind) = if symbol.is_undefined() {
            (NO_SECTION, 0, UNDEFINED)
        } else {
            let section = symbol.section_index()?;
            let value = symbol
                .address()
                .checked_sub(file.section_by_index(section).ok()?.address())?;
            let kind = match symbol.scope() {
                SymbolScope::Compilation => LOCAL,
                SymbolScope::Linkage => HIDDEN,
                SymbolScope::Dynamic => EXPORT,
                _ => return None,
            };
            (section.0.checked_sub(1)? as u64, value, kind)
        };
        let name = symbol.name_bytes().ok()?;
        words.extend([
            append(&mut bytes, name),
            name.len() as u64,
            section,
            value,
            kind,
        ]);
        indices.insert(symbol.index().0, index as u64);
    }
    for (section_index, section) in file.sections().enumerate() {
        if section.macho_section().nreloc.get(endian) == 0 {
            continue;
        }
        for raw in section.macho_relocations().ok()? {
            if raw.r_scattered(endian, cpu) {
                return None;
            }
            let reloc = raw.info(endian);
            let target = if reloc.r_extern {
                *indices.get(&(reloc.r_symbolnum as usize))?
            } else {
                u64::from(reloc.r_symbolnum.checked_sub(1)?)
            };
            words[RELOC_COUNT] += 1;
            words.extend([
                section_index as u64,
                u64::from(reloc.r_address),
                target,
                u64::from(raw.r_word1.get(endian) >> RELOCATION_FLAGS_SHIFT),
            ]);
        }
    }
    Some((words, bytes))
}

pub(crate) fn candidate(words: &[u64], bytes: &[u8]) -> Vec<u8> {
    binding::macho_object(words, bytes)
}

pub(crate) fn fixture(triple: &str, companion: bool) -> Vec<u8> {
    use cranelift_codegen::{
        ir::{AbiParam, InstBuilder, types},
        settings,
    };
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use cranelift_module::{DataDescription, Linkage, Module};
    use cranelift_object::{ObjectBuilder, ObjectModule};

    const POINTER_BYTES: usize = size_of::<u64>();
    const EXTERNAL_NAME: &str = "sh7_helper";
    let isa = cranelift_codegen::isa::lookup(triple.parse().unwrap())
        .unwrap()
        .finish(settings::Flags::new(settings::builder()))
        .unwrap();
    let mut module = ObjectModule::new(
        ObjectBuilder::new(isa, "sh7", cranelift_module::default_libcall_names()).unwrap(),
    );
    let mut ctx = module.make_context();
    ctx.func.signature.returns.push(AbiParam::new(types::I64));
    let entry = module
        .declare_function(
            if companion { EXTERNAL_NAME } else { "main" },
            Linkage::Export,
            &ctx.func.signature,
        )
        .unwrap();
    let helper = if companion {
        None
    } else {
        Some(
            module
                .declare_function(EXTERNAL_NAME, Linkage::Import, &ctx.func.signature)
                .unwrap(),
        )
    };
    let imported = helper.map(|id| module.declare_func_in_func(id, &mut ctx.func));
    let mut cx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut ctx.func, &mut cx);
    let block = builder.create_block();
    builder.switch_to_block(block);
    let result = if let Some(imported) = imported {
        let call = builder.ins().call(imported, &[]);
        builder.inst_results(call)[0]
    } else {
        builder.ins().iconst(types::I64, 73)
    };
    builder.ins().return_(&[result]);
    builder.seal_all_blocks();
    builder.finalize();
    module.define_function(entry, &mut ctx).unwrap();

    if let Some(helper) = helper {
        let table = module
            .declare_data("sh7_table", Linkage::Hidden, false, false)
            .unwrap();
        let mut data = DataDescription::new();
        data.define(vec![0; 2 * POINTER_BYTES].into_boxed_slice());
        data.set_align(POINTER_BYTES as u64);
        let local = module.declare_func_in_data(entry, &mut data);
        let external = module.declare_func_in_data(helper, &mut data);
        data.write_function_addr(0, local);
        data.write_function_addr(POINTER_BYTES as u32, external);
        module.define_data(table, &data).unwrap();

        let zero = module
            .declare_data("sh7_zero", Linkage::Local, true, false)
            .unwrap();
        data.clear();
        data.define_zeroinit(2 * POINTER_BYTES);
        data.set_align(POINTER_BYTES as u64);
        module.define_data(zero, &data).unwrap();
    }
    module.finish().emit().unwrap()
}

#[test]
fn macho_sections_symbols_and_relocations_match_cranelift() {
    const TRIPLES: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
    for triple in TRIPLES {
        let baseline = fixture(triple, false);
        let (mut words, bytes) = request(&baseline).unwrap();
        assert!(words[RELOC_COUNT] > 0);
        let generated = candidate(&words, &bytes);
        assert_eq!(request(&generated), Some((words.clone(), bytes.clone())));
        let expected_file = object::File::parse(baseline.as_slice()).unwrap();
        let actual_file = object::File::parse(generated.as_slice()).unwrap();
        assert!(
            expected_file
                .sections()
                .map(|section| section.address())
                .eq(actual_file.sections().map(|section| section.address()))
        );
        words[MIN_OS] = MACOS_11;
        let versioned = candidate(&words, &bytes);
        assert_eq!(request(&versioned), Some((words.clone(), bytes.clone())));
        words[RELOC_COUNT] += 1;
        assert!(candidate(&words, &bytes).is_empty());
    }
}

#[test]
fn macho_remaps_relocations_when_partitioning_symbols() {
    const HEADER_WORDS: usize = 6;
    const SECTION_COUNT: usize = 1;
    const SYMBOL_COUNT: usize = 2;
    const SECTION_WORDS: usize = 8;
    const SYMBOL_WORDS: usize = 5;
    const RELOCATION_WORDS: usize = 4;
    const TARGET: usize = 2;
    const FLAGS: usize = 3;
    const EXTERN: u64 = 8;
    const FIRST_EXTERNAL: usize = 1;

    let reference = fixture("aarch64-apple-darwin", false);
    let (words, bytes) = request(&reference).unwrap();
    let mut shuffled = words.clone();
    let first = HEADER_WORDS + words[SECTION_COUNT] as usize * SECTION_WORDS;
    let last = words[SYMBOL_COUNT] as usize - 1;
    for field in 0..SYMBOL_WORDS {
        shuffled.swap(first + field, first + last * SYMBOL_WORDS + field);
    }
    let relocs_at = first + words[SYMBOL_COUNT] as usize * SYMBOL_WORDS;
    let (relocations, remainder) = shuffled[relocs_at..].as_chunks_mut::<RELOCATION_WORDS>();
    assert!(remainder.is_empty());
    for relocation in relocations {
        if relocation[FLAGS] & EXTERN != 0 {
            relocation[TARGET] = match relocation[TARGET] {
                0 => last as u64,
                index if index == last as u64 => 0,
                index => index,
            };
        }
    }
    let generated = candidate(&shuffled, &bytes);
    assert_eq!(request(&generated), Some((words.clone(), bytes.clone())));

    let mut unordered = words;
    let external = first + FIRST_EXTERNAL * SYMBOL_WORDS;
    for field in 0..SYMBOL_WORDS {
        unordered.swap(external + field, external + SYMBOL_WORDS + field);
    }
    assert!(candidate(&unordered, &bytes).is_empty());
}
