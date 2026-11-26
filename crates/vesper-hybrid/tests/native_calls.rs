//! Execute direct calls emitted by the opt-in x86-64 machine backend.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/native_backend.rs"]
mod binding;

use cranelift_codegen::{
    binemit::Reloc,
    ir::{AbiParam, Function, InstBuilder, MemFlags, types},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, ModuleReloc, ModuleRelocTarget};
use object::{Object, ObjectSection, RelocationFlags, elf};
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
fn narrow(x: f64) -> f64 { x }
fn main() -> u64 { if narrow(1.0) > 0.0 { 1 } else { 0 } }
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
}

fn jit_module() -> JITModule {
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(cranelift_codegen::settings::Flags::new(
            cranelift_codegen::settings::builder(),
        ))
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
    for (name, keep_low, keep_high, pressure) in [
        ("low", true, false, false),
        ("high", false, true, false),
        ("pair", true, true, false),
        ("spills", true, true, true),
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
fn direct_calls_preserve_the_abi_across_object_writers() {
    const REGISTER_ARGUMENTS: u64 = 6;
    const HEADER_WORDS: usize = 2;
    const RECORD_WORDS: usize = 4;
    const LOAD: u64 = 23;
    const STORE_QWORD: u64 = 27;
    static CALLABLE_BLOCKS: AtomicUsize = AtomicUsize::new(0);
    static STACK_LEAF_BLOCKS: AtomicUsize = AtomicUsize::new(0);
    static MEMORY_LEAF_BLOCKS: AtomicUsize = AtomicUsize::new(0);
    let program = checked(PROGRAM);
    let stack_arguments = checked(STACK_ARGUMENT);
    let memory = checked(INOUT_MEMORY);
    let unsupported = checked(UNSUPPORTED_SIGNATURE);
    let compile = |checked: &check::Checked| {
        native::object_with_options(&checked.world, "main", None, false)
            .unwrap()
            .0
    };
    let baseline = compile(&program);
    let baseline_stack = compile(&stack_arguments);
    let baseline_memory = compile(&memory);
    let baseline_unsupported = compile(&unsupported);
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
