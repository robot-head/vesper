//! AMD64 COFF writer differentials for the native backend.

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/native_backend.rs"]
mod binding;

use object::{
    Architecture, LittleEndian as LE, Object, ObjectSection, ObjectSymbol, RelocationTarget, pe,
    read::coff::ImageSymbol,
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
fn coff_swap_uses_win64_leaves_and_preserves_caller_unwind_rows() {
    use std::cell::Cell;

    const LEAF_SUFFIX: &str = "_weighted";
    const CALLER: &str = "main";
    const UNWIND_TABLE: &str = ".pdata";
    const PROGRAM: &str = "
fn weighted(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> u64 {
    wrapping_add(wrapping_add(wrapping_add(a, b), wrapping_sub(c, d)), wrapping_mul(e, f))
}
fn sum(n: u64) -> u64 {
    if n == 0 { weighted(3, 5, 7, 11, 13, 17) }
    else { wrapping_add(sum(wrapping_sub(n, 1)), n) }
}
fn main() -> u64 { wrapping_add(sum(7), 1) }
";

    thread_local! {
        static ACTIVE_CODE: Cell<Option<Vec<u8>>> = const { Cell::new(None) };
    }
    vesper::native::machine::install_integer_encoder(|words| {
        let previous = ACTIVE_CODE.with(Cell::take)?;
        let code = binding::x86_64_integer_block(words);
        if code.is_empty() {
            ACTIVE_CODE.with(|capture| capture.set(Some(previous)));
            return None;
        }
        ACTIVE_CODE.with(|capture| capture.set(Some(code.clone())));
        Some(code)
    })
    .unwrap();

    let baseline = fixture(PROGRAM);
    ACTIVE_CODE.with(|capture| capture.set(Some(Vec::new())));
    let swapped = fixture(PROGRAM);
    let emitted = ACTIVE_CODE.with(Cell::take).unwrap();
    assert!(!emitted.is_empty());
    for (bytes, body) in [(baseline, None), (swapped, Some(emitted))] {
        let selected = body.is_some();
        let file = object::File::parse(bytes.as_slice()).unwrap();
        let leaf = file
            .symbols()
            .find(|symbol| symbol.name().is_ok_and(|name| name.ends_with(LEAF_SUFFIX)))
            .unwrap();
        if let Some(code) = body {
            let section = file
                .section_by_index(leaf.section_index().unwrap())
                .unwrap();
            let start = (leaf.address() - section.address()) as usize;
            assert_eq!(&section.data().unwrap()[start..start + code.len()], code);
        }
        let pdata = file.section_by_name(UNWIND_TABLE).unwrap();
        let starts: Vec<_> = pdata
            .relocations()
            .filter(|(offset, _)| {
                offset.is_multiple_of(vesper::native::windows::RUNTIME_FUNCTION_SIZE as u64)
            })
            .filter_map(|(_, relocation)| match relocation.target() {
                RelocationTarget::Symbol(symbol) => file.symbol_by_index(symbol).ok(),
                _ => None,
            })
            .filter_map(|symbol| symbol.name().ok())
            .collect();
        assert!(starts.contains(&CALLER), "{starts:?}");
        if selected {
            assert!(!starts.contains(&leaf.name().unwrap()), "{starts:?}");
        }
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn win64_function_pair(
    jit: &mut cranelift_jit::JITModule,
    context: &mut cranelift_codegen::Context,
) -> [*const u8; 2] {
    use cranelift_codegen::{isa, settings};
    use cranelift_module::Module;

    let windows = isa::lookup(TARGET.parse().unwrap())
        .unwrap()
        .finish(settings::Flags::new(settings::builder()))
        .unwrap();
    let tape =
        vesper::native::machine::x86_64_block_words(&context.func, windows.as_ref()).unwrap();
    let code = binding::x86_64_integer_block(&tape);
    assert!(!code.is_empty(), "{tape:?}");
    let candidate = jit
        .declare_anonymous_function(&context.func.signature)
        .unwrap();
    let reference = jit
        .declare_anonymous_function(&context.func.signature)
        .unwrap();
    let alignment = u64::from(jit.isa().function_alignment().minimum);
    jit.define_function_bytes(candidate, alignment, &code, &[])
        .unwrap();
    jit.define_function(reference, context).unwrap();
    jit.finalize_definitions().unwrap();
    [candidate, reference].map(|function| jit.get_finalized_function(function))
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn win64_leaf_integer_registers_and_stack_match_cranelift() {
    use cranelift_codegen::{
        ir::{AbiParam, InstBuilder, types},
        isa::CallConv,
    };
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use cranelift_jit::{JITBuilder, JITModule};
    use cranelift_module::Module;

    type Entry = unsafe extern "win64" fn(u8, u16, u32, u64, u64, u64, u64, u64) -> u64;
    let mut jit =
        JITModule::new(JITBuilder::new(cranelift_module::default_libcall_names()).unwrap());
    let mut context = jit.make_context();
    context.func.signature.call_conv = CallConv::WindowsFastcall;
    let params = [
        types::I8,
        types::I16,
        types::I32,
        types::I64,
        types::I64,
        types::I64,
        types::I64,
        types::I64,
    ];
    context.func.signature.params = params.map(AbiParam::new).to_vec();
    context
        .func
        .signature
        .returns
        .push(AbiParam::new(types::I64));
    let mut frontend = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let values = builder.block_params(entry).to_vec();
    let mut total = builder.ins().uextend(types::I64, values[0]);
    for (i, (&value, ty)) in values.iter().zip(params).enumerate().skip(1) {
        let value = if ty == types::I64 {
            value
        } else {
            builder.ins().uextend(types::I64, value)
        };
        let weighted = builder.ins().imul_imm(value, i as i64 + 1);
        total = builder.ins().iadd(total, weighted);
    }
    builder.ins().return_(&[total]);
    builder.seal_all_blocks();
    builder.finalize();
    let [candidate, reference] = win64_function_pair(&mut jit, &mut context)
        .map(|function| unsafe { std::mem::transmute::<*const u8, Entry>(function) });
    let inputs = [0, 1, 0x8512_34ab_cdef_5678, 1u64 << 63, u64::MAX];
    for a in inputs {
        for b in inputs {
            let call = |run: Entry| unsafe {
                run(
                    a as u8,
                    b as u16,
                    a as u32,
                    b,
                    !a,
                    !b,
                    a.rotate_left(13),
                    b.rotate_right(7),
                )
            };
            assert_eq!(call(candidate), call(reference));
        }
    }
    unsafe { jit.free_memory() };
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn win64_leaf_mixed_registers_and_stack_match_cranelift() {
    use cranelift_codegen::{
        ir::{AbiParam, InstBuilder, types},
        isa::CallConv,
    };
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use cranelift_jit::{JITBuilder, JITModule};
    use cranelift_module::Module;

    type Single = unsafe extern "win64" fn(u8, f32, u32, f64, f64, u64, f32) -> f32;
    type Double = unsafe extern "win64" fn(u8, f32, u32, f64, f64, u64, f32) -> f64;
    type Integer = unsafe extern "win64" fn(u8, f32, u32, f64, f64, u64, f32) -> u64;
    let mut jit =
        JITModule::new(JITBuilder::new(cranelift_module::default_libcall_names()).unwrap());
    let inputs = [
        -0.0,
        0.0,
        -1.75,
        3.25,
        f64::from_bits(1),
        18_000_000_000_000_000_000.0,
        19_000_000_000_000_000_000.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ];
    for result in [types::F32, types::F64, types::I64] {
        let mut context = jit.make_context();
        let signature = &mut context.func.signature;
        signature.call_conv = CallConv::WindowsFastcall;
        signature.params = [
            types::I8,
            types::F32,
            types::I32,
            types::F64,
            types::F64,
            types::I64,
            types::F32,
        ]
        .map(AbiParam::new)
        .to_vec();
        signature.returns.push(AbiParam::new(result));
        let mut frontend = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut context.func, &mut frontend);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let [a, b_single, c, d, e, f, g_single] = *b.block_params(entry) else {
            unreachable!()
        };
        let b_double = b.ins().fpromote(types::F64, b_single);
        let signed = b.ins().fcopysign(b_double, d);
        let clipped = b.ins().fmin(signed, e);
        // Keep the right operand live so the minimum needs a scratch register.
        let negative = b.ins().fneg(e);
        let difference = b.ins().fadd(clipped, negative);
        let g_double = b.ins().fpromote(types::F64, g_single);
        let compared = b.ins().fadd(difference, g_double);
        let a = b.ins().uextend(types::I64, a);
        let c = b.ins().uextend(types::I64, c);
        let partial = b.ins().iadd(a, c);
        let total = b.ins().iadd(partial, f);
        let total = b.ins().fcvt_from_uint(types::F64, total);
        let value = b.ins().fadd(total, compared);
        let value = if result == types::F32 {
            b.ins().fdemote(types::F32, value)
        } else if result == types::I64 {
            b.ins().fcvt_to_uint_sat(types::I64, value)
        } else {
            value
        };
        b.ins().return_(&[value]);
        b.seal_all_blocks();
        b.finalize();
        let functions = win64_function_pair(&mut jit, &mut context);
        for (i, &x) in inputs.iter().enumerate() {
            for (j, &y) in inputs.iter().enumerate() {
                let a = i as u8;
                let b = inputs[(i + j) % inputs.len()] as f32;
                let c = u32::MAX - j as u32;
                let f = (j as u64) << 60;
                let g = inputs[(i + 1) % inputs.len()] as f32;
                if result == types::I64 {
                    let [got, want] = functions.map(|ptr| unsafe {
                        std::mem::transmute::<*const u8, Integer>(ptr)(a, b, c, x, y, f, g)
                    });
                    assert_eq!(got, want, "{i} {j}");
                } else {
                    let [got, want] = functions.map(|ptr| unsafe {
                        if result == types::F32 {
                            f64::from(std::mem::transmute::<*const u8, Single>(ptr)(
                                a, b, c, x, y, f, g,
                            ))
                        } else {
                            std::mem::transmute::<*const u8, Double>(ptr)(a, b, c, x, y, f, g)
                        }
                    });
                    assert!(
                        got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan()),
                        "{result} {i} {j}: {got} vs {want}"
                    );
                }
            }
        }
    }
    unsafe { jit.free_memory() };
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
