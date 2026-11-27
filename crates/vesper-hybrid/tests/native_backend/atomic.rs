use super::{binding, integer_block_words};
use cranelift_codegen::{
    ir::{AbiParam, AtomicRmwOp, InstBuilder, MemFlags, types},
    settings,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;
use std::sync::{
    Barrier,
    atomic::{AtomicU64, Ordering},
};

#[test]
fn compare_exchange_preserves_live_registers_and_narrow_memory() {
    const MARKER_FACTOR: i64 = 137;
    type Entry = unsafe extern "C" fn(*mut u64, u64, u64) -> u64;
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(settings::builder()))
        .unwrap();
    let mut module = JITModule::new(JITBuilder::with_isa(
        isa,
        cranelift_module::default_libcall_names(),
    ));
    for ty in [types::I8, types::I16, types::I32, types::I64] {
        for keep_old in [false, true] {
            let mut context = module.make_context();
            context.func.signature.params = vec![AbiParam::new(types::I64); 3];
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
            let [address, expected, replacement] = *b.block_params(entry) else {
                unreachable!()
            };
            let marker = b.ins().imul_imm(expected, MARKER_FACTOR);
            let (expected, replacement) = if ty == types::I64 {
                (expected, replacement)
            } else {
                (
                    b.ins().ireduce(ty, expected),
                    b.ins().ireduce(ty, replacement),
                )
            };
            let old = b
                .ins()
                .atomic_cas(MemFlags::trusted(), address, expected, replacement);
            let output = if keep_old {
                let old = if ty == types::I64 {
                    old
                } else {
                    b.ins().uextend(types::I64, old)
                };
                b.ins().bxor(marker, old)
            } else {
                marker
            };
            b.ins().return_(&[output]);
            b.seal_all_blocks();
            b.finalize();

            let reference = module
                .declare_anonymous_function(&context.func.signature)
                .unwrap();
            let candidate = module
                .declare_anonymous_function(&context.func.signature)
                .unwrap();
            let words = integer_block_words(&context.func).expect("compare-exchange tape");
            let code = binding::x86_64_integer_block(&words);
            assert!(!code.is_empty(), "{ty} {keep_old}: {words:?}");
            let alignment = u64::from(module.isa().function_alignment().minimum);
            module
                .define_function_bytes(candidate, alignment, &code, &[])
                .unwrap();
            module.define_function(reference, &mut context).unwrap();
            module.finalize_definitions().unwrap();
            let initial = 0xa55a_3cf0_88aa_eec3u64;
            let replacement = 0x8170_caba_7000_febau64;
            let mask = u64::MAX >> (u64::BITS - ty.bits());
            for expected in [initial, !initial, initial ^ !mask] {
                let old = initial & mask;
                let next = if old == expected & mask {
                    (initial & !mask) | (replacement & mask)
                } else {
                    initial
                };
                let output =
                    expected.wrapping_mul(MARKER_FACTOR as u64) ^ if keep_old { old } else { 0 };
                for id in [reference, candidate] {
                    let entry = module.get_finalized_function(id);
                    let run = unsafe { std::mem::transmute::<*const u8, Entry>(entry) };
                    let mut memory = initial;
                    let result = unsafe { run(&mut memory, expected, replacement) };
                    assert_eq!((result, memory), (output, next), "{ty} {keep_old}");
                }
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn atomic_read_modify_write_matches_cranelift_and_retries_under_contention() {
    const MARKER_FACTOR: i64 = 137;
    type Entry = unsafe extern "C" fn(*mut u64, u64, u64) -> u64;
    use AtomicRmwOp::{And, Nand, Or, Smax, Smin, Umax, Umin, Xor};
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(settings::builder()))
        .unwrap();
    let mut module = JITModule::new(JITBuilder::with_isa(
        isa,
        cranelift_module::default_libcall_names(),
    ));
    for ty in [types::I8, types::I16, types::I32, types::I64] {
        for op in [And, Nand, Or, Xor, Umin, Umax, Smin, Smax] {
            let mut context = module.make_context();
            context.func.signature.params = vec![AbiParam::new(types::I64); 3];
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
            let [address, source, seed] = *b.block_params(entry) else {
                unreachable!()
            };
            let marker = b.ins().imul_imm(source, MARKER_FACTOR);
            let input = if ty == types::I64 {
                source
            } else {
                b.ins().ireduce(ty, source)
            };
            let old = b
                .ins()
                .atomic_rmw(ty, MemFlags::trusted(), op, address, input);
            let old = if ty == types::I64 {
                old
            } else {
                b.ins().uextend(types::I64, old)
            };
            let output = b.ins().bxor(old, marker);
            let output = b.ins().bxor(output, seed);
            b.ins().return_(&[output]);
            b.seal_all_blocks();
            b.finalize();

            let reference = module
                .declare_anonymous_function(&context.func.signature)
                .unwrap();
            let candidate = module
                .declare_anonymous_function(&context.func.signature)
                .unwrap();
            let words = integer_block_words(&context.func).expect("atomic update tape");
            let code = binding::x86_64_integer_block(&words);
            assert!(!code.is_empty(), "{ty} {op:?}: {words:?}");
            let alignment = u64::from(module.isa().function_alignment().minimum);
            module
                .define_function_bytes(candidate, alignment, &code, &[])
                .unwrap();
            module.define_function(reference, &mut context).unwrap();
            module.finalize_definitions().unwrap();
            let functions = [reference, candidate].map(|id| unsafe {
                std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(id))
            });
            let mask = u64::MAX >> (u64::BITS - ty.bits());
            let sign = 1u64 << (ty.bits() - 1);
            let seed = 0x4a08_81bd_e873_0051;
            for (initial, source) in [
                (0xa55a_3cf0_88aa_eec3, 0x8170_caba_7000_feba),
                (sign, sign - 1),
                (sign - 1, sign),
                (0, u64::MAX),
                (u64::MAX, 0),
            ] {
                let old = initial & mask;
                let input = source & mask;
                let next = match op {
                    And => old & input,
                    Nand => !(old & input),
                    Or => old | input,
                    Xor => old ^ input,
                    Umin => old.min(input),
                    Umax => old.max(input),
                    Smin => (old ^ sign).min(input ^ sign) ^ sign,
                    Smax => (old ^ sign).max(input ^ sign) ^ sign,
                    _ => unreachable!(),
                };
                let expected_memory = (initial & !mask) | (next & mask);
                let expected_result = old ^ source.wrapping_mul(MARKER_FACTOR as u64) ^ seed;
                for run in functions {
                    let mut memory = initial;
                    let result = unsafe { run(&mut memory, source, seed) };
                    assert_eq!(
                        (result, memory),
                        (expected_result, expected_memory),
                        "{ty} {op:?}"
                    );
                }
            }
            if ty == types::I64 && op == Xor {
                const THREADS: usize = 4;
                const ITERATIONS: usize = 2049;
                let memory = AtomicU64::new(0);
                let barrier = Barrier::new(THREADS);
                let run = functions[1];
                std::thread::scope(|scope| {
                    for lane in 0..THREADS {
                        let memory = &memory;
                        let barrier = &barrier;
                        scope.spawn(move || {
                            barrier.wait();
                            for _ in 0..ITERATIONS {
                                unsafe { run(memory.as_ptr(), 1 << lane, 0) };
                            }
                        });
                    }
                });
                assert_eq!(memory.load(Ordering::Relaxed), (1 << THREADS) - 1);
            }
        }
    }
    unsafe { module.free_memory() };
}
