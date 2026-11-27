use super::{binding, integer_block_words};
use cranelift_codegen::{
    cursor::{Cursor, FuncCursor},
    ir::{AbiParam, InstBuilder, MemFlags, Opcode, types},
    settings,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;
use vesper::native::machine::x86_64_block_words;

#[test]
fn scalar_and_wide_bit_operations_match_cranelift_with_and_without_popcnt() {
    const RESULTS: usize = 6;
    type Entry = unsafe extern "C" fn(u64, u64, *mut [u64; 2]);
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(settings::builder()))
        .unwrap();
    let mut module = JITModule::new(JITBuilder::with_isa(
        isa,
        cranelift_module::default_libcall_names(),
    ));

    for ty in [types::I8, types::I16, types::I32, types::I64, types::I128] {
        let mut context = module.make_context();
        context.func.signature.params = vec![AbiParam::new(types::I64); 3];
        let mut state = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut context.func, &mut state);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let [low, high, output] = *b.block_params(entry) else {
            unreachable!()
        };
        let input = match ty {
            types::I128 => b.ins().iconcat(low, high),
            types::I64 => low,
            _ => b.ins().ireduce(ty, low),
        };
        let values = [
            b.ins().clz(input),
            b.ins().ctz(input),
            b.ins().popcnt(input),
            b.ins().bitrev(input),
            b.ins().cls(input),
            b.ins().iabs(input),
        ];
        for (index, value) in values.into_iter().enumerate() {
            let [low, high] = if ty == types::I128 {
                let (low, high) = b.ins().isplit(value);
                [low, high]
            } else {
                let low = if ty == types::I64 {
                    value
                } else {
                    b.ins().uextend(types::I64, value)
                };
                [low, b.ins().iconst(types::I64, 0)]
            };
            let flags = MemFlags::new().with_notrap();
            let offset = (index * size_of::<[u64; 2]>()) as i32;
            b.ins().store(flags, low, output, offset);
            b.ins()
                .store(flags, high, output, offset + size_of::<u64>() as i32);
        }
        b.ins().return_(&[]);
        b.seal_all_blocks();
        b.finalize();

        let ids = (0..3)
            .map(|_| {
                module
                    .declare_anonymous_function(&context.func.signature)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let fallback = integer_block_words(&context.func).expect("portable bit operations");
        let native =
            x86_64_block_words(&context.func, module.isa()).expect("target bit operations");
        for (id, words) in ids[1..].iter().zip([fallback, native]) {
            let code = binding::x86_64_integer_block(&words);
            assert!(!code.is_empty(), "{ty}: {words:?}");
            let alignment = u64::from(module.isa().function_alignment().minimum);
            module
                .define_function_bytes(*id, alignment, &code, &[])
                .unwrap();
        }
        // Cranelift's x86-64 lowering needs an equivalent expression for cls.
        let cls = context
            .func
            .layout
            .block_insts(entry)
            .find(|&inst| context.func.dfg.insts[inst].opcode() == Opcode::Cls)
            .unwrap();
        let mut cursor = FuncCursor::new(&mut context.func).at_inst(cls);
        let sign = cursor.ins().sshr_imm(input, i64::from(ty.bits() - 1));
        let unsigned = cursor.ins().bxor(input, sign);
        let zeros = cursor.ins().clz(unsigned);
        cursor.func.dfg.replace(cls).iadd_imm(zeros, -1);
        module.define_function(ids[0], &mut context).unwrap();
        module.finalize_definitions().unwrap();
        let width = ty.bits();
        let padding = u128::BITS - width;
        let mask = u128::MAX >> padding;
        let sign = 1u128 << (width - 1);
        for value in [
            0,
            1,
            mask,
            sign,
            sign - 1,
            sign | 1,
            u128::from(1u64) << u64::BITS,
            0x81a5_ffff_0000_007e_fedc_1234_5600_8080,
        ] {
            let value = value & mask;
            let negative = (value & sign) != 0;
            let signed = if negative { value ^ mask } else { value };
            let absolute = if negative {
                value.wrapping_neg() & mask
            } else {
                value
            };
            let expected = [
                u128::from(value.leading_zeros() - padding),
                u128::from(value.trailing_zeros().min(width)),
                u128::from(value.count_ones()),
                value.reverse_bits() >> padding,
                u128::from(signed.leading_zeros() - padding - 1),
                absolute,
            ]
            .map(|value| [value as u64, (value >> u64::BITS) as u64]);
            for &id in &ids {
                let entry = module.get_finalized_function(id);
                let run = unsafe { std::mem::transmute::<*const u8, Entry>(entry) };
                let mut output = [[u64::MAX; 2]; RESULTS];
                unsafe {
                    run(
                        value as u64,
                        (value >> u64::BITS) as u64,
                        output.as_mut_ptr(),
                    )
                };
                assert_eq!(output, expected, "{ty} {value:#x}");
            }
        }
    }
    unsafe { module.free_memory() };
}
