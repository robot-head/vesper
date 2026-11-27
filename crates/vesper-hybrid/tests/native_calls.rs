//! Execute direct calls emitted by the opt-in x86-64 machine backend.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/native_backend.rs"]
mod binding;

use cranelift_codegen::{
    binemit::Reloc,
    ir::{
        AbiParam, BlockArg, ExtFuncData, ExternalName, Function, InstBuilder, LibCall, MemFlags,
        StackSlotData, StackSlotKind, types,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, ModuleReloc, ModuleRelocTarget};
use cranelift_object::{ObjectBuilder, ObjectModule};
use object::{Object, ObjectSection, ObjectSymbol, RelocationFlags, RelocationTarget, elf};
use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};
use vesper::{check, native, parser};

const PROGRAM: &str = "
fn mix(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> u64 {
    let low = wrapping_add(a, wrapping_mul(b, 2));
    let mid = wrapping_add(wrapping_mul(c, 3), wrapping_mul(d, 4));
    let high = wrapping_add(wrapping_mul(e, 5), wrapping_mul(f, 6));
    wrapping_add(wrapping_add(low, mid), high)
}
fn call_twice(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> u64 {
    let first = mix(f, e, d, c, b, a);
    let second = mix(b, a, c, d, f, e);
    wrapping_add(wrapping_sub(second, first), a)
}
fn main() -> u64 { wrapping_add(call_twice(3, 5, 7, 11, 13, 17), 1) }
";
const STACK_ARGUMENT: &str = "
fn wide(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64, i: u64) -> u64 {
    wrapping_add(wrapping_add(d, wrapping_mul(g, 3)), wrapping_add(h, wrapping_mul(i, 5)))
}
fn count_wide(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64) -> u64 {
    wrapping_add(wide(a, b, c, d, e, f, g, h, a), widen_u64(leading_zeros_u64(g)))
}
fn forward(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64, i: u64) -> u64 {
    let first = wide(g, h, i, b, d, e, i, g, a);
    let second = count_wide(a, b, c, d, e, f, first, h);
    wrapping_add(second, i)
}
fn main() -> u64 { forward(1, 2, 3, 4, 5, 6, 7, 8, 9) }
";
const UNSUPPORTED_SIGNATURE: &str = "
fn ninth(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64, i: f64) -> f64 { i }
fn main() -> u64 { if ninth(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0) > 0.0 { 1 } else { 0 } }
";
const DETFLOAT: &str = "
fn eval32(x: f32, y: f32) -> f32 eff [DetFloat] { sqrt(x + y) }
fn eval64(x: f64, y: f64) -> f64 eff [DetFloat] { (x - y) / (x * y) }
fn main() -> u64 {
    let signed_signaling = from_bits_f32(0xff800123);
    let signed_quiet = from_bits_f64(0xfff8000000000123);
    let result32 = to_bits_f32(eval32(signed_signaling, 1.0));
    let invalid32 = to_bits_f32(eval32(-2.0, 1.0));
    let result64 = to_bits_f64(eval64(signed_quiet, 1.0));
    let valid64 = eval64(8.0, 2.0);
    if result32 == 0x7fc00000 && invalid32 == 0x7fc00000
        && result64 == 0x7ff8000000000000 && valid64 == 0.375 {
        0
    } else { 1 }
}
";
const INOUT_MEMORY: &str = "
fn bump(inout n: u64) -> u64 {
    let old = n;
    n = wrapping_add(n, 4);
    old
}
fn main() -> u64 {
    let mut n: u64 = 19;
    let old = bump(inout n);
    wrapping_add(old, n)
}
";

fn checked(source: &str) -> check::Checked {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    checked
}

#[test]
fn call_records_require_relocations_and_keep_discarded_calls() {
    let call = [0, 0, 38, 1, 0, 0];
    assert!(binding::x86_64_integer_block(&call).is_empty());
    assert!(!binding::x86_64_relocatable_block(&call).is_empty());
    // One argument was promised with no preceding argument record.
    assert!(binding::x86_64_relocatable_block(&[0, 0, 38, 1, 1, 0]).is_empty());
    // The call argument refers to its own metadata slot.
    assert!(binding::x86_64_relocatable_block(&[1, 2, 39, 1, 0, 0, 38, 1, 1, 0]).is_empty());
    // A retained input supplies the return value. The call still gets a relocation.
    assert!(!binding::x86_64_relocatable_block(&[1, 0, 38, 1, 0, 0]).is_empty());
    // A two-result call owns the next slot, which cannot appear on its own.
    assert!(binding::x86_64_relocatable_block(&[0, 0, 41, 1, 0, 0]).is_empty());
    assert!(binding::x86_64_relocatable_block(&[0, 1, 38, 1, 0, 0, 42, 0, 0, 0]).is_empty());
    assert!(!binding::x86_64_relocatable_block(&[0, 1, 41, 1, 0, 0, 42, 0, 0, 0]).is_empty());
    // Both data-address forms require a relocation.
    for address in [43, 44] {
        let request = [0, 0, address, 1, 0, 0];
        assert!(binding::x86_64_integer_block(&request).is_empty());
        assert!(!binding::x86_64_relocatable_block(&request).is_empty());
    }

    const FLOAT_ARGUMENT: u64 = 79;
    const ARGUMENT: u64 = 39;
    const CALL_FLOAT: u64 = 97;
    const FLOAT_RETURN: u64 = 96;
    const FLOAT_BITS: u64 = (size_of::<f64>() * u8::BITS as usize) as u64;
    const CALL_XMM_COUNT: usize = 13;
    let mut mixed = [
        0,
        2,
        FLOAT_ARGUMENT,
        0,
        0,
        FLOAT_BITS,
        ARGUMENT,
        0,
        0,
        0,
        CALL_FLOAT,
        1,
        1,
        1,
        FLOAT_RETURN,
        0,
        0,
        0,
    ];
    assert!(!binding::x86_64_relocatable_block(&mixed).is_empty());
    mixed[CALL_XMM_COUNT] = 0;
    assert!(binding::x86_64_relocatable_block(&mixed).is_empty());
}

fn jit_module() -> JITModule {
    let mut flags = settings::builder();
    flags.set("enable_llvm_abi_extensions", "true").unwrap();
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(flags))
        .unwrap();
    JITModule::new(JITBuilder::with_isa(
        isa,
        cranelift_module::default_libcall_names(),
    ))
}

fn define_callable(
    module: &mut JITModule,
    entry: FuncId,
    callee: FuncId,
    function: &Function,
    expected_relocations: usize,
) {
    const WORD_BYTES: usize = size_of::<u64>();
    const REL32_BYTES: i64 = size_of::<i32>() as i64;
    const PLT_CALL: u64 = 0;
    let words = native::machine::integer_block_words(function).unwrap();
    let packet = binding::x86_64_relocatable_block(&words);
    let (length, packet) = packet.split_first_chunk::<WORD_BYTES>().unwrap();
    let (code, targets) = packet.split_at(u64::from_le_bytes(*length) as usize);
    let (targets, rest) = targets.as_chunks::<{ 3 * WORD_BYTES }>();
    assert_eq!(targets.len(), expected_relocations);
    assert!(rest.is_empty());
    let relocations: Vec<_> = targets
        .iter()
        .map(|target| {
            let ([offset, kind, id], _) = target.as_chunks::<WORD_BYTES>() else {
                unreachable!();
            };
            assert_eq!(u64::from_le_bytes(*kind), PLT_CALL);
            let id = u64::from_le_bytes(*id) as u32;
            assert_eq!(id, callee.as_u32());
            ModuleReloc {
                offset: u64::from_le_bytes(*offset) as u32,
                kind: Reloc::X86CallPCRel4,
                name: ModuleRelocTarget::user(0, id),
                addend: -REL32_BYTES,
            }
        })
        .collect();
    let alignment = u64::from(module.isa().function_alignment().minimum);
    module
        .define_function_bytes(entry, alignment, code, &relocations)
        .unwrap();
}

#[test]
fn returning_a_callee_saved_temporary_preserves_its_value() {
    const VALUES: usize = 12;
    const REGISTER_PRESSURE_INDEX: usize = 4;
    type Entry = unsafe extern "C" fn(*mut u64, u64) -> u64;
    let mut module = jit_module();
    let mut context = module.make_context();
    context.func.signature.params = vec![AbiParam::new(types::I64); 2];
    context
        .func
        .signature
        .returns
        .push(AbiParam::new(types::I64));
    let mut state = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut context.func, &mut state);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let [output, input] = *b.block_params(entry) else {
        unreachable!()
    };
    let terms: Vec<_> = (1..=VALUES)
        .map(|offset| b.ins().iadd_imm(input, offset as i64))
        .collect();
    for (index, &value) in terms.iter().enumerate() {
        b.ins().store(
            MemFlags::trusted(),
            value,
            output,
            (index * size_of::<u64>()) as i32,
        );
    }
    b.ins().return_(&[terms[REGISTER_PRESSURE_INDEX]]);
    b.seal_all_blocks();
    b.finalize();

    let words = native::machine::integer_block_words(&context.func).unwrap();
    let bytes = binding::x86_64_integer_block(&words);
    const MOV_RBX_TO_RAX: [u8; 3] = [0x48, 0x89, 0xd8];
    assert!(
        bytes
            .windows(MOV_RBX_TO_RAX.len())
            .any(|window| window == MOV_RBX_TO_RAX)
    );
    let id = module
        .declare_anonymous_function(&context.func.signature)
        .unwrap();
    let alignment = u64::from(module.isa().function_alignment().minimum);
    module
        .define_function_bytes(id, alignment, &bytes, &[])
        .unwrap();
    module.finalize_definitions().unwrap();
    let call =
        unsafe { std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(id)) };
    let mut values = [0; VALUES];
    for input in [0u64, u64::MAX - 1] {
        let expected: [u64; VALUES] =
            std::array::from_fn(|index| input.wrapping_add(index as u64 + 1));
        assert_eq!(
            unsafe { call(values.as_mut_ptr(), input) },
            expected[REGISTER_PRESSURE_INDEX]
        );
        assert_eq!(values, expected);
    }
    unsafe { module.free_memory() };
}

#[test]
fn narrow_system_v_cells_cross_both_backend_boundaries() {
    type Entry = unsafe extern "C" fn(u64, u64) -> u64;
    let mut module = jit_module();
    let widths = [
        types::I8,
        types::I16,
        types::I32,
        types::I64,
        types::I64,
        types::I64,
        types::I8,
        types::I16,
    ];
    let mut narrow = module.make_context();
    narrow
        .func
        .signature
        .params
        .extend(widths.map(AbiParam::new));
    narrow
        .func
        .signature
        .returns
        .extend([types::I8, types::I32].map(AbiParam::new));
    let signature = narrow.func.signature.clone();
    let mut state = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut narrow.func, &mut state);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let [byte, word, dword, _, _, _, stack_byte, stack_word] = *b.block_params(entry) else {
        unreachable!()
    };
    let low = b.ins().bxor(byte, stack_byte);
    let first = b.ins().uextend(types::I32, word);
    let second = b.ins().sextend(types::I32, stack_word);
    let high = b.ins().iadd(dword, first);
    let high = b.ins().iadd(high, second);
    b.ins().return_(&[low, high]);
    b.seal_all_blocks();
    b.finalize();
    let mut extended = narrow.func.clone();
    extended.signature.params[0] = extended.signature.params[0].sext();
    assert!(native::machine::integer_block_words(&extended).is_none());

    let reference = module.declare_anonymous_function(&signature).unwrap();
    let candidate = module.declare_anonymous_function(&signature).unwrap();
    let words = native::machine::integer_block_words(&narrow.func).unwrap();
    let bytes = binding::x86_64_integer_block(&words);
    assert!(!bytes.is_empty());
    let alignment = u64::from(module.isa().function_alignment().minimum);
    module
        .define_function_bytes(candidate, alignment, &bytes, &[])
        .unwrap();
    module.define_function(reference, &mut narrow).unwrap();

    let mut entry_signature = module.make_signature();
    entry_signature.params = vec![AbiParam::new(types::I64); 2];
    entry_signature.returns.push(AbiParam::new(types::I64));
    let mut entries = Vec::new();
    for (port, target) in [
        (false, reference),
        (true, reference),
        (false, candidate),
        (true, candidate),
    ] {
        let mut context = module.make_context();
        context.func.signature = entry_signature.clone();
        let callee = module.declare_func_in_func(target, &mut context.func);
        let mut b = FunctionBuilder::new(&mut context.func, &mut state);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let [a, z] = *b.block_params(entry) else {
            unreachable!()
        };
        let inputs = [a, z, z, a, z, a, z, a];
        let args: Vec<_> = widths
            .into_iter()
            .zip(inputs)
            .map(|(width, value)| {
                if width == types::I64 {
                    value
                } else {
                    b.ins().ireduce(width, value)
                }
            })
            .collect();
        let call = b.ins().call(callee, &args);
        let [low, high] = *b.inst_results(call) else {
            unreachable!()
        };
        let low = b.ins().uextend(types::I64, low);
        let high = b.ins().uextend(types::I64, high);
        let high = b.ins().ishl_imm(high, i64::from(u8::BITS));
        let result = b.ins().bor(high, low);
        b.ins().return_(&[result]);
        b.seal_all_blocks();
        b.finalize();
        let id = module.declare_anonymous_function(&entry_signature).unwrap();
        if port {
            define_callable(&mut module, id, target, &context.func, 1);
        } else {
            module.define_function(id, &mut context).unwrap();
        }
        entries.push(id);
    }
    module.finalize_definitions().unwrap();
    for (a, z) in [
        (0, 0),
        (u64::MAX, 1),
        (0xdead_beef_1000_8001, 0xcafe_babe_ffff_fffe),
    ] {
        let low = u64::from(a as u8 ^ z as u8);
        let high = (z as u32)
            .wrapping_add(u32::from(z as u16))
            .wrapping_add(i32::from(a as i16) as u32);
        let expected = (u64::from(high) << u8::BITS) | low;
        for &id in &entries {
            let run = unsafe {
                std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(id))
            };
            assert_eq!(unsafe { run(a, z) }, expected, "{id:?}({a:#x}, {z:#x})");
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn void_calls_and_conditional_returns_preserve_side_effects() {
    type Procedure = unsafe extern "C" fn(*mut u64, u64);
    const NO_VALUE: u64 = u64::MAX;
    let code = binding::x86_64_integer_block(&[0, NO_VALUE]);
    assert!(!code.is_empty(), "an empty procedure still needs a return");
    // The value-less call's record must never be used as a result.
    assert!(binding::x86_64_relocatable_block(&[0, 0, 40, 1, 0, 0]).is_empty());

    let mut module = jit_module();
    let mut signature = module.make_signature();
    signature.params = vec![AbiParam::new(types::I64); 2];
    let mut declare = |name| {
        module
            .declare_function(name, Linkage::Local, &signature)
            .unwrap()
    };
    let reference_add = declare("reference_add");
    let candidate_add = declare("candidate_add");
    let reference = declare("reference");
    let candidate = declare("candidate");
    let mut context = module.make_context();
    context.func.signature = signature.clone();
    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let [pointer, delta] = *b.block_params(entry) else {
            unreachable!();
        };
        let flags = MemFlags::trusted();
        let previous = b.ins().load(types::I64, flags, pointer, 0);
        let next = b.ins().iadd(previous, delta);
        b.ins().store(flags, next, pointer, 0);
        b.ins().return_(&[]);
        b.seal_all_blocks();
        b.finalize();
    }
    let alignment = u64::from(module.isa().function_alignment().minimum);
    let words = native::machine::integer_block_words(&context.func).unwrap();
    let code = binding::x86_64_integer_block(&words);
    assert!(!code.is_empty());
    module
        .define_function_bytes(candidate_add, alignment, &code, &[])
        .unwrap();
    module.define_function(reference_add, &mut context).unwrap();

    for (entry, add, use_vesper) in [
        (reference, reference_add, false),
        (candidate, candidate_add, true),
    ] {
        module.clear_context(&mut context);
        context.func.signature = signature.clone();
        let add_ref = module.declare_func_in_func(add, &mut context.func);
        let mut b = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry_block = b.create_block();
        let yes = b.create_block();
        let no = b.create_block();
        b.append_block_params_for_function_params(entry_block);
        b.switch_to_block(entry_block);
        let [pointer, input] = *b.block_params(entry_block) else {
            unreachable!();
        };
        b.ins().brif(input, yes, &[], no, &[]);
        b.switch_to_block(yes);
        let other = b.ins().iconst(types::I64, 11);
        b.ins().call(add_ref, &[pointer, input]);
        b.ins().call(add_ref, &[pointer, other]);
        b.ins().return_(&[]);
        b.switch_to_block(no);
        b.ins().return_(&[]);
        b.seal_all_blocks();
        b.finalize();
        if !use_vesper {
            module.define_function(entry, &mut context).unwrap();
            continue;
        }
        define_callable(&mut module, entry, add, &context.func, 2);
    }
    module.finalize_definitions().unwrap();
    for function in [reference, candidate] {
        let procedure = unsafe {
            std::mem::transmute::<*const u8, Procedure>(module.get_finalized_function(function))
        };
        for input in [0u64, 3, u64::MAX] {
            let mut result = 7u64;
            unsafe { procedure(&mut result, input) };
            let expected = if input == 0 {
                7
            } else {
                7u64.wrapping_add(input).wrapping_add(11)
            };
            assert_eq!(result, expected);
        }
    }
}

#[test]
fn paired_call_results_survive_live_registers_and_spills() {
    type Entry = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64;
    const LARGE_LOCAL_BYTES: u32 = 256;
    const STACK_ALIGNMENT_LOG2: u8 = 4;
    const LOCAL_VALUE_OFFSET: i32 = (LARGE_LOCAL_BYTES - size_of::<u64>() as u32) as i32;
    let mut module = jit_module();
    let scalar = AbiParam::new(types::I64);
    let mut pair_signature = module.make_signature();
    pair_signature.params = vec![scalar; 2];
    pair_signature.returns = vec![scalar; 2];
    let callee = module
        .declare_function("pair", Linkage::Local, &pair_signature)
        .unwrap();
    let mut context = module.make_context();
    context.func.signature = pair_signature;
    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let a = b.block_params(entry)[0];
        let b_arg = b.block_params(entry)[1];
        let low = b.ins().isub(a, b_arg);
        let product = b.ins().imul_imm(b_arg, 11);
        let high = b.ins().iadd(product, a);
        b.ins().return_(&[low, high]);
        b.seal_all_blocks();
        b.finalize();
    }
    module.define_function(callee, &mut context).unwrap();
    let mut signature = module.make_signature();
    signature.params = vec![scalar; 6];
    signature.returns.push(scalar);
    for (name, keep_low, keep_high, pressure, large_local) in [
        ("low", true, false, false, false),
        ("high", false, true, false, false),
        ("pair", true, true, false, false),
        ("spills", true, true, true, false),
        ("spills_large_local", true, true, true, true),
    ] {
        let baseline = module
            .declare_function(&format!("{name}_base"), Linkage::Local, &signature)
            .unwrap();
        let candidate = module
            .declare_function(&format!("{name}_vesper"), Linkage::Local, &signature)
            .unwrap();
        module.clear_context(&mut context);
        context.func.signature = signature.clone();
        let target = module.declare_func_in_func(callee, &mut context.func);
        let mut b = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let [a, b_arg, c, d, e, f] = *b.block_params(entry) else {
            unreachable!();
        };
        let local = large_local.then(|| {
            let slot = b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                LARGE_LOCAL_BYTES,
                STACK_ALIGNMENT_LOG2,
            ));
            b.ins().stack_store(f, slot, LOCAL_VALUE_OFFSET);
            slot
        });
        let sum = b.ins().iadd(c, d);
        let diff = b.ins().isub(e, f);
        let call = b.ins().call(target, &[b_arg, a]);
        let low = b.inst_results(call)[0];
        let high = b.inst_results(call)[1];
        let mut result = match (keep_low, keep_high) {
            (true, true) => b.ins().isub(high, low),
            (true, false) => low,
            (false, true) => high,
            _ => unreachable!(),
        };
        if pressure {
            for value in [a, b_arg, c, d, e, f, sum, diff] {
                result = b.ins().iadd(result, value);
            }
        }
        if let Some(slot) = local {
            let value = b.ins().stack_load(types::I64, slot, LOCAL_VALUE_OFFSET);
            let address = b.ins().stack_addr(types::I64, slot, 0);
            let misalignment = b.ins().band_imm(address, (1 << STACK_ALIGNMENT_LOG2) - 1);
            result = b.ins().iadd(result, value);
            result = b.ins().iadd(result, misalignment);
        }
        b.ins().return_(&[result]);
        b.seal_all_blocks();
        b.finalize();
        define_callable(&mut module, candidate, callee, &context.func, 1);
        module.define_function(baseline, &mut context).unwrap();
        module.finalize_definitions().unwrap();
        let entry = |id| unsafe {
            std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(id))
        };
        let a = entry(baseline);
        let b = entry(candidate);
        for [i, j, k, l, m, n] in [[3, 5, 7, 11, 13, 17], [u64::MAX, 42, 1, 3, 0, 1]] {
            assert_eq!(
                unsafe { a(i, j, k, l, m, n) },
                unsafe { b(i, j, k, l, m, n) },
                "{name}"
            );
        }
    }
}

#[test]
fn wide_direct_calls_use_consecutive_integer_registers() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[repr(C)]
    struct Pair(u64, u64);
    type Entry = unsafe extern "C" fn(u64, u64, u64, u64) -> Pair;

    let mut module = jit_module();
    let scalar = AbiParam::new(types::I64);
    let wide = AbiParam::new(types::I128);
    let mut signature = module.make_signature();
    signature.params.extend([scalar, wide, scalar]);
    signature.returns.push(wide);
    let callee = module
        .declare_function("wide", Linkage::Local, &signature)
        .unwrap();
    let mut context = module.make_context();
    let mut builder_context = FunctionBuilderContext::new();
    context.func.signature = signature.clone();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let [lo, input, hi] = *builder.block_params(entry) else {
            unreachable!()
        };
        let addend = builder.ins().iconcat(lo, hi);
        let sum = builder.ins().iadd(input, addend);
        builder.ins().return_(&[sum]);
        builder.seal_all_blocks();
        builder.finalize();
    }
    module.define_function(callee, &mut context).unwrap();
    module.clear_context(&mut context);
    context.func.signature = signature.clone();
    let callee_ref = module.declare_func_in_func(callee, &mut context.func);
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let [lo, input, hi] = *builder.block_params(entry) else {
            unreachable!()
        };
        let call = builder.ins().call(callee_ref, &[hi, input, lo]);
        let called = builder.inst_results(call)[0];
        let output = builder.ins().iadd(input, called);
        builder.ins().return_(&[output]);
        builder.seal_all_blocks();
        builder.finalize();
    }
    let reference = module
        .declare_function("reference_wide", Linkage::Local, &signature)
        .unwrap();
    let candidate = module
        .declare_function("candidate_wide", Linkage::Local, &signature)
        .unwrap();
    define_callable(&mut module, candidate, callee, &context.func, 1);
    module.define_function(reference, &mut context).unwrap();
    module.finalize_definitions().unwrap();
    let entry =
        |id| unsafe { std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(id)) };
    let reference = entry(reference);
    let candidate = entry(candidate);
    for [a, lo, hi, b] in [
        [1, 2, 3, 4],
        [9, u64::MAX, u64::MAX, 2],
        [11, 0, u64::MAX, 31],
    ] {
        assert_eq!(unsafe { candidate(a, lo, hi, b) }, unsafe {
            reference(a, lo, hi, b)
        });
    }
    unsafe { module.free_memory() };
}

#[test]
fn mixed_float_calls_preserve_live_xmm_values_and_stack_arguments() {
    type Single = unsafe extern "C" fn(u64, f32, f32, u64, f32, u64, u64, u64, u64, u64) -> f32;
    type Double = unsafe extern "C" fn(u64, f64, f64, u64, f64, u64, u64, u64, u64, u64) -> f64;
    let mut module = jit_module();
    let integer = types::I64;
    for fp in [types::F32, types::F64] {
        let mut signature = module.make_signature();
        signature.params.extend(
            [
                integer, fp, fp, integer, fp, integer, integer, integer, integer, integer,
            ]
            .map(AbiParam::new),
        );
        signature.returns.push(AbiParam::new(fp));
        let callee = module.declare_anonymous_function(&signature).unwrap();
        let mut context = module.make_context();
        let mut builder_context = FunctionBuilderContext::new();
        context.func.signature = signature.clone();
        {
            let mut b = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let [first, left, middle, _, right, _, _, _, _, stack] = *b.block_params(entry) else {
                unreachable!()
            };
            let difference = b.ins().fsub(left, middle);
            let product = b.ins().fmul(difference, right);
            let gp_difference = b.ins().isub(first, stack);
            let float_difference = b.ins().fcvt_from_sint(fp, gp_difference);
            let result = b.ins().fadd(product, float_difference);
            b.ins().return_(&[result]);
            b.seal_all_blocks();
            b.finalize();
        }
        module.define_function(callee, &mut context).unwrap();
        module.clear_context(&mut context);
        context.func.signature = signature.clone();
        let target = module.declare_func_in_func(callee, &mut context.func);
        {
            let mut b = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let [first, left, middle, gp, right, a, c, d, e, stack] = *b.block_params(entry) else {
                unreachable!()
            };
            // This rotates the first three XMM registers and passes the first
            // integer argument on the stack. The original XMM0 stays live.
            let call = b
                .ins()
                .call(target, &[stack, middle, right, gp, left, a, c, d, e, first]);
            let returned = b.inst_results(call)[0];
            let result = b.ins().fsub(returned, left);
            b.ins().return_(&[result]);
            b.seal_all_blocks();
            b.finalize();
        }
        let reference = module.declare_anonymous_function(&signature).unwrap();
        let candidate = module.declare_anonymous_function(&signature).unwrap();
        define_callable(&mut module, candidate, callee, &context.func, 1);
        module.define_function(reference, &mut context).unwrap();
        module.finalize_definitions().unwrap();
        let reference = module.get_finalized_function(reference);
        let candidate = module.get_finalized_function(candidate);
        for (first, left, middle, right, stack) in [
            (23, 2.0, 7.0, 3.0, 89),
            (55, -5.0, -0.0, 8.0, 13),
            (13, 2.5, 1.0, -11.0, 1),
        ] {
            let (reference, candidate) = unsafe {
                if fp == types::F32 {
                    let reference = std::mem::transmute::<*const u8, Single>(reference);
                    let candidate = std::mem::transmute::<*const u8, Single>(candidate);
                    let run = |entry: Single| {
                        entry(
                            first,
                            left as f32,
                            middle as f32,
                            5,
                            right as f32,
                            7,
                            11,
                            17,
                            19,
                            stack,
                        )
                        .to_bits()
                    };
                    (u64::from(run(reference)), u64::from(run(candidate)))
                } else {
                    let reference = std::mem::transmute::<*const u8, Double>(reference);
                    let candidate = std::mem::transmute::<*const u8, Double>(candidate);
                    let run = |entry: Double| {
                        entry(first, left, middle, 5, right, 7, 11, 17, 19, stack).to_bits()
                    };
                    (run(reference), run(candidate))
                }
            };
            assert_eq!(candidate, reference, "{fp}");
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn float_cfg_homes_survive_a_callee_clobbering_the_xmm_home_bank() {
    type Entry = unsafe extern "C" fn(u64, f64) -> f64;
    const REX_HIGH_REGISTERS: u8 = 0x45;
    const TWO_BYTE_OPCODE: u8 = 0x0f;
    const XORPS: u8 = 0x57;
    const REGISTER_MODRM: u8 = 0xc0;
    const MODRM_REG_SHIFT: u8 = 3;
    const HOME_REGISTERS: u8 = 5;
    const RETURN: u8 = 0xc3;
    let mut module = jit_module();
    let signature = module.make_signature();
    let callee = module.declare_anonymous_function(&signature).unwrap();
    let mut callee_bytes = Vec::new();
    for low in 0..HOME_REGISTERS {
        callee_bytes.extend([
            REX_HIGH_REGISTERS,
            TWO_BYTE_OPCODE,
            XORPS,
            REGISTER_MODRM | (low << MODRM_REG_SHIFT) | low,
        ]);
    }
    callee_bytes.push(RETURN);
    let alignment = u64::from(module.isa().function_alignment().minimum);
    module
        .define_function_bytes(callee, alignment, &callee_bytes, &[])
        .unwrap();

    let mut context = module.make_context();
    let mut state = FunctionBuilderContext::new();
    let signature = &mut context.func.signature;
    signature
        .params
        .extend([types::I64, types::F64].map(AbiParam::new));
    signature.returns.push(AbiParam::new(types::F64));
    let target = module.declare_func_in_func(callee, &mut context.func);
    let mut b = FunctionBuilder::new(&mut context.func, &mut state);
    let entry = b.create_block();
    let header = b.create_block();
    let body = b.create_block();
    let exit = b.create_block();
    b.append_block_params_for_function_params(entry);
    let count = b.append_block_param(header, types::I64);
    let result = b.append_block_param(header, types::F64);
    b.switch_to_block(entry);
    let [iterations, value] = *b.block_params(entry) else {
        unreachable!()
    };
    b.ins().jump(
        header,
        &[BlockArg::Value(iterations), BlockArg::Value(value)],
    );
    b.switch_to_block(header);
    b.ins().brif(count, body, &[], exit, &[]);
    b.switch_to_block(body);
    b.ins().call(target, &[]);
    let remaining = b.ins().iadd_imm(count, -1);
    b.ins().jump(
        header,
        &[BlockArg::Value(remaining), BlockArg::Value(result)],
    );
    b.switch_to_block(exit);
    b.ins().return_(&[result]);
    b.seal_all_blocks();
    b.finalize();

    let candidate = module
        .declare_anonymous_function(&context.func.signature)
        .unwrap();
    define_callable(&mut module, candidate, callee, &context.func, 1);
    module.finalize_definitions().unwrap();
    let entry = unsafe {
        std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(candidate))
    };
    for value in [3.5, -0.0, f64::from_bits(0x7ff8_0000_0000_1234)] {
        assert_eq!(unsafe { entry(3, value) }.to_bits(), value.to_bits());
    }
    unsafe { module.free_memory() };
}

#[test]
fn compiler_memory_calls_relocate_to_the_runtime_symbols() {
    const WORD_BYTES: usize = size_of::<u64>();
    const PLT_LIBCALL: u64 = 3;
    const PC_RELATIVE_ADDEND: i64 = -(size_of::<i32>() as i64);
    let mut flags = settings::builder();
    flags.set("is_pic", "true").unwrap();
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(flags))
        .unwrap();
    let names = cranelift_module::default_libcall_names();
    let mut module = ObjectModule::new(ObjectBuilder::new(isa, "memory_calls", names).unwrap());
    for (call, id, name) in [
        (LibCall::Memmove, 0, "move_bytes"),
        (LibCall::Memcpy, 1, "copy_bytes"),
    ] {
        let signature = call.signature(module.isa().default_call_conv(), types::I64);
        let mut context = module.make_context();
        context.func.signature = signature.clone();
        let callee_signature = context.func.import_signature(signature.clone());
        let callee = context.func.import_function(ExtFuncData {
            name: ExternalName::LibCall(call),
            signature: callee_signature,
            colocated: false,
        });
        let mut state = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut context.func, &mut state);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let args = b.block_params(entry).to_vec();
        let invoke = b.ins().call(callee, &args);
        let result = b.inst_results(invoke)[0];
        b.ins().return_(&[result]);
        b.seal_all_blocks();
        b.finalize();

        let request = native::machine::integer_block_words(&context.func).unwrap();
        let packet = binding::x86_64_relocatable_block(&request);
        let (length, body) = packet.split_first_chunk::<WORD_BYTES>().unwrap();
        let (code, relocation) = body.split_at(u64::from_le_bytes(*length) as usize);
        let ([offset, kind, target], rest) = relocation.as_chunks::<WORD_BYTES>() else {
            unreachable!()
        };
        assert!(rest.is_empty());
        assert_eq!(u64::from_le_bytes(*kind), PLT_LIBCALL);
        assert_eq!(u64::from_le_bytes(*target), id);
        let candidate = module
            .declare_function(name, Linkage::Export, &signature)
            .unwrap();
        let alignment = u64::from(module.isa().function_alignment().minimum);
        module
            .define_function_bytes(
                candidate,
                alignment,
                code,
                &[ModuleReloc {
                    offset: u32::try_from(u64::from_le_bytes(*offset)).unwrap(),
                    kind: Reloc::X86CallPLTRel4,
                    name: ModuleRelocTarget::LibCall(call),
                    addend: PC_RELATIVE_ADDEND,
                }],
            )
            .unwrap();
    }
    let bytes = module.finish().emit().unwrap();
    let object = object::File::parse(bytes.as_slice()).unwrap();
    let symbols: Vec<_> = object
        .section_by_name(".text")
        .unwrap()
        .relocations()
        .map(|(_, relocation)| {
            assert_eq!(
                relocation.flags(),
                RelocationFlags::Elf {
                    r_type: elf::R_X86_64_PLT32,
                },
            );
            assert_eq!(relocation.addend(), PC_RELATIVE_ADDEND);
            let RelocationTarget::Symbol(index) = relocation.target() else {
                panic!("expected a libcall symbol");
            };
            object.symbol_by_index(index).unwrap().name().unwrap()
        })
        .collect();
    assert_eq!(symbols, ["memmove", "memcpy"]);

    if Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("vesper-sh7-memcalls-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let obj = dir.join("memory_calls.o");
    let harness = dir.join("memory_calls.c");
    let exe = dir.join("memory_calls");
    fs::write(&obj, bytes).unwrap();
    fs::write(
        &harness,
        r#"
#include <stddef.h>
#include <string.h>
void *move_bytes(void *, const void *, size_t);
void *copy_bytes(void *, const void *, size_t);
int main(void) {
    char expected[16] = "0123456789abcde";
    char actual[16] = "0123456789abcde";
    memmove(expected + 2, expected, 5);
    if (move_bytes(actual + 2, actual, 5) != actual + 2) return 1;
    if (memcmp(expected, actual, sizeof(actual)) != 0) return 2;
    memcpy(expected + 8, expected, 5);
    if (copy_bytes(actual + 8, actual, 5) != actual + 8) return 3;
    return memcmp(expected, actual, sizeof(actual)) != 0;
}
"#,
    )
    .unwrap();
    let link = Command::new("cc")
        .arg(&harness)
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        link.status.success(),
        "{}",
        String::from_utf8_lossy(&link.stderr)
    );
    assert!(Command::new(&exe).status().unwrap().success());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn direct_calls_preserve_the_abi_across_object_writers() {
    const REGISTER_ARGUMENTS: u64 = 6;
    const HEADER_WORDS: usize = 2;
    const RECORD_WORDS: usize = 4;
    const LOAD: u64 = 23;
    const STORE_QWORD: u64 = 27;
    const FLOAT_DIV: u64 = 84;
    const FLOAT_SQRT: u64 = 85;
    static CALLABLE_BLOCKS: AtomicUsize = AtomicUsize::new(0);
    static STACK_LEAF_BLOCKS: AtomicUsize = AtomicUsize::new(0);
    static MEMORY_LEAF_BLOCKS: AtomicUsize = AtomicUsize::new(0);
    static DETFLOAT_LEAF_BLOCKS: AtomicUsize = AtomicUsize::new(0);
    let program = checked(PROGRAM);
    let stack_arguments = checked(STACK_ARGUMENT);
    let memory = checked(INOUT_MEMORY);
    let unsupported = checked(UNSUPPORTED_SIGNATURE);
    let detfloat = checked(DETFLOAT);
    assert!(native::needs_nan_canonicalization(&detfloat.world));
    let compile = |checked: &check::Checked| {
        native::object_with_options(&checked.world, "main", None, false)
            .unwrap()
            .0
    };
    let baseline = compile(&program);
    let baseline_stack = compile(&stack_arguments);
    let baseline_memory = compile(&memory);
    let baseline_unsupported = compile(&unsupported);
    let baseline_detfloat = compile(&detfloat);
    assert!(
        native::machine::install_integer_encoder(|words| {
            let code = binding::x86_64_integer_block(words);
            if code.is_empty() {
                return None;
            }
            if words
                .first()
                .is_some_and(|count| *count > REGISTER_ARGUMENTS)
            {
                STACK_LEAF_BLOCKS.fetch_add(1, Relaxed);
            }
            let records = words[HEADER_WORDS..].as_chunks::<RECORD_WORDS>().0;
            if records.iter().any(|record| record[0] == LOAD)
                && records.iter().any(|record| record[0] == STORE_QWORD)
            {
                MEMORY_LEAF_BLOCKS.fetch_add(1, Relaxed);
            }
            if records
                .iter()
                .any(|record| matches!(record[0], FLOAT_DIV | FLOAT_SQRT))
            {
                DETFLOAT_LEAF_BLOCKS.fetch_add(1, Relaxed);
            }
            Some(code)
        })
        .is_ok()
    );
    assert!(
        native::machine::install_relocation_encoder(|words| {
            let packet = binding::x86_64_relocatable_block(words);
            if packet.is_empty() {
                return None;
            }
            CALLABLE_BLOCKS.fetch_add(1, Relaxed);
            Some(packet)
        })
        .is_ok()
    );
    let candidate = compile(&program);
    assert_eq!(compile(&unsupported), baseline_unsupported);
    assert_eq!(CALLABLE_BLOCKS.load(Relaxed), 2);
    let file = object::File::parse(candidate.as_slice()).unwrap();
    let relocations: Vec<_> = file
        .section_by_name(".text")
        .unwrap()
        .relocations()
        .collect();
    assert_eq!(relocations.len(), 3);
    for (_, relocation) in relocations {
        assert_eq!(
            relocation.flags(),
            RelocationFlags::Elf {
                r_type: elf::R_X86_64_PLT32
            }
        );
        assert_eq!(relocation.addend(), -4);
    }
    let candidate_stack = compile(&stack_arguments);
    assert_eq!(CALLABLE_BLOCKS.load(Relaxed), 4);
    assert_eq!(STACK_LEAF_BLOCKS.load(Relaxed), 1);
    // The leading-zero count falls back to Cranelift and calls the Vesper-generated
    // wide function. The stack ABI crosses the boundary in both directions.
    assert_ne!(candidate_stack, baseline_stack);
    let candidate_memory = compile(&memory);
    assert_eq!(CALLABLE_BLOCKS.load(Relaxed), 5);
    assert_eq!(MEMORY_LEAF_BLOCKS.load(Relaxed), 1);
    assert!(
        native::elf::install_object_encoder(|words, bytes| {
            let object = binding::x86_64_elf_object(words, bytes);
            (!object.is_empty()).then_some(object)
        })
        .is_ok()
    );
    let combined = compile(&program);
    let combined_stack = compile(&stack_arguments);
    let combined_memory = compile(&memory);
    assert_eq!(CALLABLE_BLOCKS.load(Relaxed), 10);
    assert_eq!(STACK_LEAF_BLOCKS.load(Relaxed), 2);
    assert_eq!(MEMORY_LEAF_BLOCKS.load(Relaxed), 2);
    let combined_detfloat = compile(&detfloat);
    assert_ne!(combined_detfloat, baseline_detfloat);
    assert_eq!(DETFLOAT_LEAF_BLOCKS.load(Relaxed), 2);
    assert_ne!(
        combined, candidate,
        "the ELF writer should also be selected"
    );
    if Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("vesper-sh7-call-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    for (name, object, expected) in [
        ("baseline", baseline, 96),
        ("candidate", candidate, 96),
        ("combined", combined, 96),
        ("baseline_stack", baseline_stack, 207),
        ("candidate_stack", candidate_stack, 207),
        ("combined_stack", combined_stack, 207),
        ("baseline_memory", baseline_memory, 42),
        ("candidate_memory", candidate_memory, 42),
        ("combined_memory", combined_memory, 42),
        ("baseline_detfloat", baseline_detfloat, 0),
        ("combined_detfloat", combined_detfloat, 0),
    ] {
        let obj = dir.join(format!("{name}.o"));
        let exe = dir.join(name);
        fs::write(&obj, object).unwrap();
        let link = Command::new("cc")
            .arg(&obj)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        assert_eq!(
            Command::new(&exe).status().unwrap().code(),
            Some(expected),
            "{name}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
