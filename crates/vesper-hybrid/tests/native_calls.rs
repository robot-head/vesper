//! Execute direct calls emitted by the opt-in x86-64 machine backend.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/native_backend.rs"]
mod binding;

use cranelift_codegen::{
    binemit::Reloc,
    ir::{AbiParam, InstBuilder, MemFlags, types},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, ModuleReloc, ModuleRelocTarget};
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
fn seventh(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64) -> u64 { g }
fn main() -> u64 { wrapping_add(seventh(1, 2, 3, 4, 5, 6, 7), 1) }
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
    assert!(!binding::x86_64_callable_block(&call).is_empty());
    // One argument was promised with no preceding argument record.
    assert!(binding::x86_64_callable_block(&[0, 0, 38, 1, 1, 0]).is_empty());
    // The call argument refers to its own metadata slot.
    assert!(binding::x86_64_callable_block(&[1, 2, 39, 1, 0, 0, 38, 1, 1, 0]).is_empty());
    // A retained input supplies the return value. The call still gets a relocation.
    assert!(!binding::x86_64_callable_block(&[1, 0, 38, 1, 0, 0]).is_empty());
}

#[test]
fn void_calls_and_conditional_returns_preserve_side_effects() {
    type Procedure = unsafe extern "C" fn(*mut u64, u64);
    const NO_VALUE: u64 = u64::MAX;
    let code = binding::x86_64_integer_block(&[0, NO_VALUE]);
    assert!(!code.is_empty(), "an empty procedure still needs a return");
    // The value-less call's record must never be used as a result.
    assert!(binding::x86_64_callable_block(&[0, 0, 40, 1, 0, 0]).is_empty());

    let isa = cranelift_native::builder()
        .unwrap()
        .finish(cranelift_codegen::settings::Flags::new(
            cranelift_codegen::settings::builder(),
        ))
        .unwrap();
    let mut module = JITModule::new(JITBuilder::with_isa(
        isa,
        cranelift_module::default_libcall_names(),
    ));
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
        let words = native::machine::integer_block_words(&context.func).unwrap();
        let packet = binding::x86_64_callable_block(&words);
        let (length, packet) = packet.split_first_chunk::<8>().unwrap();
        let (code, targets) = packet.split_at(u64::from_le_bytes(*length) as usize);
        let (targets, rest) = targets.as_chunks::<16>();
        assert_eq!(targets.len(), 2);
        assert!(rest.is_empty());
        let relocations: Vec<_> = targets
            .iter()
            .map(|target| {
                let (offset, id) = target.split_first_chunk::<8>().unwrap();
                let id = u64::from_le_bytes(id.try_into().unwrap()) as u32;
                assert_eq!(id, add.as_u32());
                ModuleReloc {
                    offset: u64::from_le_bytes(*offset) as u32,
                    kind: Reloc::X86CallPCRel4,
                    name: ModuleRelocTarget::user(0, id),
                    addend: -4,
                }
            })
            .collect();
        module
            .define_function_bytes(entry, alignment, code, &relocations)
            .unwrap();
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
fn direct_calls_preserve_values_and_shuffle_six_arguments() {
    static CALLABLE_BLOCKS: AtomicUsize = AtomicUsize::new(0);
    let program = checked(PROGRAM);
    let unsupported = checked(STACK_ARGUMENT);
    let compile = |checked: &check::Checked| {
        native::object_with_options(&checked.world, "main", None, false)
            .unwrap()
            .0
    };
    let baseline = compile(&program);
    let baseline_unsupported = compile(&unsupported);
    assert!(
        native::machine::install_integer_encoder(|words| {
            let code = binding::x86_64_integer_block(words);
            (!code.is_empty()).then_some(code)
        })
        .is_ok()
    );
    assert!(
        native::machine::install_call_encoder(|words| {
            let packet = binding::x86_64_callable_block(words);
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
    assert!(
        native::elf::install_object_encoder(|words, bytes| {
            let object = binding::x86_64_elf_object(words, bytes);
            (!object.is_empty()).then_some(object)
        })
        .is_ok()
    );
    let combined = compile(&program);
    assert_eq!(CALLABLE_BLOCKS.load(Relaxed), 4);
    assert_ne!(
        combined, candidate,
        "the ELF writer should also be selected"
    );
    if Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("vesper-sh7-call-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    for (name, object) in [
        ("baseline", baseline),
        ("candidate", candidate),
        ("combined", combined),
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
            Some(96),
            "{name}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
