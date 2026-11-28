use super::{binding, integer_block_words};
use cranelift_codegen::{
    cursor::{Cursor, FuncCursor},
    ir::{AbiParam, InstBuilder, MemFlags, Opcode, Type, Value, condcodes::IntCC, types},
    settings,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;
use vesper::native::machine::x86_64_block_words;

fn store_bits(b: &mut FunctionBuilder<'_>, ty: Type, output: Value, index: usize, value: Value) {
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
            store_bits(&mut b, ty, output, index, value);
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

#[test]
fn scalar_and_wide_rotates_complements_and_min_max_match_cranelift() {
    const LEFT_IMMEDIATE: i64 = 65;
    const RIGHT_IMMEDIATE: i64 = -9;
    const RESULTS: usize = 11;
    type Entry = unsafe extern "C" fn(u64, u64, u64, u64, u64, *mut [u64; 2]);
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
        context.func.signature.params = vec![AbiParam::new(types::I64); 6];
        let mut state = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut context.func, &mut state);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let [la, ha, lb, hb, count, output] = *b.block_params(entry) else {
            unreachable!()
        };
        let (a, b_operand) = match ty {
            types::I128 => (b.ins().iconcat(la, ha), b.ins().iconcat(lb, hb)),
            types::I64 => (la, lb),
            _ => (b.ins().ireduce(ty, la), b.ins().ireduce(ty, lb)),
        };
        let values = [
            b.ins().rotl(a, count),
            b.ins().rotr(a, count),
            b.ins().rotl_imm(a, LEFT_IMMEDIATE),
            b.ins().rotr_imm(a, RIGHT_IMMEDIATE),
            b.ins().band_not(a, b_operand),
            b.ins().bor_not(a, b_operand),
            b.ins().bxor_not(a, b_operand),
            b.ins().smin(a, b_operand),
            b.ins().smax(a, b_operand),
            b.ins().umin(a, b_operand),
            b.ins().umax(a, b_operand),
        ];
        for (index, value) in values.into_iter().enumerate() {
            store_bits(&mut b, ty, output, index, value);
        }
        b.ins().return_(&[]);
        b.seal_all_blocks();
        b.finalize();
        let reference = module
            .declare_anonymous_function(&context.func.signature)
            .unwrap();
        let candidate = module
            .declare_anonymous_function(&context.func.signature)
            .unwrap();
        let words = integer_block_words(&context.func).expect("integer bit operations");
        let code = binding::x86_64_integer_block(&words);
        assert!(!code.is_empty(), "{ty}: {words:?}");
        let alignment = u64::from(module.isa().function_alignment().minimum);
        module
            .define_function_bytes(candidate, alignment, &code, &[])
            .unwrap();
        if ty == types::I128 {
            // Use explicit comparisons for the reference's wide min and max.
            let comparisons: Vec<_> = context
                .func
                .layout
                .block_insts(entry)
                .filter_map(|inst| {
                    let cc = match context.func.dfg.insts[inst].opcode() {
                        Opcode::Smin => IntCC::SignedLessThan,
                        Opcode::Smax => IntCC::SignedGreaterThan,
                        Opcode::Umin => IntCC::UnsignedLessThan,
                        Opcode::Umax => IntCC::UnsignedGreaterThan,
                        _ => return None,
                    };
                    Some((inst, cc))
                })
                .collect();
            for (inst, cc) in comparisons {
                let mut cursor = FuncCursor::new(&mut context.func).at_inst(inst);
                let condition = cursor.ins().icmp(cc, a, b_operand);
                cursor
                    .func
                    .dfg
                    .replace(inst)
                    .select(condition, a, b_operand);
            }
        }
        module.define_function(reference, &mut context).unwrap();
        module.finalize_definitions().unwrap();

        let width = ty.bits();
        let mask = u128::MAX >> (u128::BITS - width);
        let sign = 1u128 << (width - 1);
        for (a, b) in [
            (0, mask),
            (sign, sign - 1),
            (sign - 1, sign),
            (
                0xf27f_0000_0000_0081_a500_8080_887f_0000 & mask,
                0xa7f7_1234_0000_0400_00f9_8170_00f8_0a31 & mask,
            ),
        ] {
            let [minimum, maximum] = if (a ^ sign) < (b ^ sign) {
                [a, b]
            } else {
                [b, a]
            };
            let rot_right = |count: u32| {
                let bits = count % width;
                if bits == 0 {
                    a
                } else {
                    ((a >> bits) | (a << (width - bits))) & mask
                }
            };
            for count in [0, 1, width - 1, width, width + 1, u32::MAX] {
                let expected = [
                    rot_right(count.wrapping_neg()),
                    rot_right(count),
                    rot_right((LEFT_IMMEDIATE as u32).wrapping_neg()),
                    rot_right(RIGHT_IMMEDIATE as u32),
                    a & !b,
                    (a | !b) & mask,
                    (a ^ !b) & mask,
                    minimum,
                    maximum,
                    a.min(b),
                    a.max(b),
                ]
                .map(|value| [value as u64, (value >> u64::BITS) as u64]);
                for id in [reference, candidate] {
                    let entry = module.get_finalized_function(id);
                    let run = unsafe { std::mem::transmute::<*const u8, Entry>(entry) };
                    let mut result = [[u64::MAX; 2]; RESULTS];
                    unsafe {
                        run(
                            a as u64,
                            (a >> u64::BITS) as u64,
                            b as u64,
                            (b >> u64::BITS) as u64,
                            u64::from(count),
                            result.as_mut_ptr(),
                        )
                    };
                    assert_eq!(result, expected, "{ty} {a:#x} {b:#x} {count}");
                }
            }
        }
    }
    unsafe { module.free_memory() };
}
