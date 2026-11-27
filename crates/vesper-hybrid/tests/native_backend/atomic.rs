use super::{binding, integer_block_words};
use cranelift_codegen::{
    ir::{AbiParam, InstBuilder, MemFlags, types},
    settings,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;

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
