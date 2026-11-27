use super::{binding, integer_block_words};
use cranelift_codegen::{
    Context,
    ir::{AbiParam, ConstantData, InstBuilder, MemFlags, StackSlotData, StackSlotKind, types},
    settings,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;

const VECTOR_BYTES: usize = 16;
const UNALIGNED: i32 = 1;

fn module() -> JITModule {
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(settings::builder()))
        .unwrap();
    JITModule::new(JITBuilder::with_isa(
        isa,
        cranelift_module::default_libcall_names(),
    ))
}

fn compile(module: &mut JITModule, context: &mut Context) -> [*const u8; 2] {
    let words = integer_block_words(&context.func).expect("vector machine request");
    let bytes = binding::x86_64_integer_block(&words);
    assert!(!bytes.is_empty(), "{words:?}");
    let reference = module
        .declare_anonymous_function(&context.func.signature)
        .unwrap();
    let candidate = module
        .declare_anonymous_function(&context.func.signature)
        .unwrap();
    module.define_function(reference, context).unwrap();
    let alignment = u64::from(module.isa().function_alignment().minimum);
    module
        .define_function_bytes(candidate, alignment, &bytes, &[])
        .unwrap();
    module.finalize_definitions().unwrap();
    [reference, candidate].map(|function| module.get_finalized_function(function))
}

#[test]
fn vector_constants_splats_and_bitselect_match_cranelift() {
    const SLOT_ALIGNMENT_LOG2: u8 = 4;
    let mut module = module();
    for ty in [
        types::I8X16,
        types::I16X8,
        types::I32X4,
        types::I64X2,
        types::F32X4,
        types::F64X2,
    ] {
        let mut context = module.make_context();
        context.func.signature.params = vec![AbiParam::new(types::I64); 3];
        let mut frontend = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let [source, destination, bits] = *builder.block_params(entry) else {
            unreachable!()
        };
        let lane_type = ty.lane_type();
        let integer_type = lane_type.as_int();
        let lane = if integer_type == types::I64 {
            bits
        } else {
            builder.ins().ireduce(integer_type, bits)
        };
        let lane = if lane_type.is_float() {
            builder.ins().bitcast(lane_type, MemFlags::new(), lane)
        } else {
            lane
        };
        let mask_bytes: Vec<u8> = (0..VECTOR_BYTES)
            .map(|i| 0x12u8.wrapping_add(i as u8 * 7))
            .collect();
        let constant = builder
            .func
            .dfg
            .constants
            .insert(ConstantData::from(mask_bytes.clone()));
        let mask = builder.ins().vconst(ty, constant);
        let yes = builder.ins().splat(ty, lane);
        let no = builder
            .ins()
            .load(ty, MemFlags::new().with_notrap(), source, UNALIGNED);
        let selected = builder.ins().bitselect(mask, yes, no);
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            VECTOR_BYTES as u32,
            SLOT_ALIGNMENT_LOG2,
        ));
        builder.ins().stack_store(selected, slot, 0);
        let reloaded = builder.ins().stack_load(ty, slot, 0);
        builder.ins().store(
            MemFlags::new().with_notrap(),
            reloaded,
            destination,
            UNALIGNED,
        );
        builder.ins().return_(&[]);
        builder.seal_all_blocks();
        builder.finalize();
        let functions = compile(&mut module, &mut context);
        for bits in [0u64, u64::MAX, 0x7ff8_ccdd_7fc0_0042, 0x4488_3322_fe67_8910] {
            let source: [u8; VECTOR_BYTES + 2] = std::array::from_fn(|i| i as u8 * 13);
            let lane_bytes = bits.to_le_bytes();
            let mut expected = [0xdd; VECTOR_BYTES + 2];
            for (i, &mask) in mask_bytes.iter().enumerate() {
                let offset = i + UNALIGNED as usize;
                expected[offset] =
                    mask & lane_bytes[i % lane_type.bytes() as usize] | !mask & source[offset];
            }
            for pointer in functions {
                type Entry = unsafe extern "C" fn(*const u8, *mut u8, u64);
                let run = unsafe { std::mem::transmute::<*const u8, Entry>(pointer) };
                let mut result = [0xdd; VECTOR_BYTES + 2];
                unsafe { run(source.as_ptr(), result.as_mut_ptr(), bits) };
                assert_eq!(result, expected, "{ty} {bits:#x}");
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn vector_loads_keep_both_halves_across_spills_and_calls() {
    const LOADS: usize = 21;
    let mut module = module();
    let mut context = module.make_context();
    context.func.signature.params = vec![AbiParam::new(types::I64); 3];
    let callee = context.func.import_signature(module.make_signature());
    let mut frontend = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let [source, destination, callee_address] = *builder.block_params(entry) else {
        unreachable!()
    };
    let parts: Vec<_> = (0..LOADS)
        .map(|i| {
            builder.ins().load(
                types::I8X16,
                MemFlags::new().with_notrap(),
                source,
                (i * VECTOR_BYTES) as i32,
            )
        })
        .collect();
    builder.ins().call_indirect(callee, callee_address, &[]);
    let sum = parts
        .into_iter()
        .reduce(|left, right| builder.ins().bxor(left, right))
        .unwrap();
    builder
        .ins()
        .store(MemFlags::new().with_notrap(), sum, destination, 0);
    builder.ins().return_(&[]);
    builder.seal_all_blocks();
    builder.finalize();

    extern "C" fn clobber_vectors() {
        // SysV treats the complete XMM bank as caller-saved.
        unsafe {
            std::arch::asm!(
                "pxor xmm0, xmm0",
                "pxor xmm1, xmm1",
                "pxor xmm2, xmm2",
                "pxor xmm3, xmm3",
                "pxor xmm4, xmm4",
                "pxor xmm5, xmm5",
                "pxor xmm6, xmm6",
                "pxor xmm7, xmm7",
                "pxor xmm8, xmm8",
                "pxor xmm9, xmm9",
                "pxor xmm10, xmm10",
                "pxor xmm11, xmm11",
                "pxor xmm12, xmm12",
                "pxor xmm13, xmm13",
                "pxor xmm14, xmm14",
                "pxor xmm15, xmm15",
                clobber_abi("C"),
                options(nomem, nostack, preserves_flags),
            );
        }
    }

    let functions = compile(&mut module, &mut context);
    let mut seed = 0x9029b3957042d5e1u64;
    let source: [u8; LOADS * VECTOR_BYTES] = std::array::from_fn(|_| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as u8
    });
    let expected: [u8; VECTOR_BYTES] = std::array::from_fn(|i| {
        source
            .iter()
            .skip(i)
            .step_by(VECTOR_BYTES)
            .fold(0, |x, y| x ^ y)
    });
    for pointer in functions {
        type Entry = unsafe extern "C" fn(*const u8, *mut u8, extern "C" fn());
        let run = unsafe { std::mem::transmute::<*const u8, Entry>(pointer) };
        let mut result = [0; VECTOR_BYTES];
        unsafe { run(source.as_ptr(), result.as_mut_ptr(), clobber_vectors) };
        assert_eq!(result, expected);
    }
    unsafe { module.free_memory() };
}

#[test]
fn packed_integer_arithmetic_matches_cranelift_at_lane_boundaries() {
    #[derive(Clone, Copy, Debug)]
    enum Op {
        Add,
        Sub,
        SignedAdd,
        UnsignedAdd,
        SignedSub,
        UnsignedSub,
        Average,
    }
    let saturated = [
        Op::SignedAdd,
        Op::UnsignedAdd,
        Op::SignedSub,
        Op::UnsignedSub,
        Op::Average,
    ];
    let mut module = module();
    for ty in [types::I8X16, types::I16X8, types::I32X4, types::I64X2] {
        let lane = ty.lane_type();
        let lane_bytes = lane.bytes() as usize;
        let sign = 1u64 << (lane.bits() - 1);
        let max = (sign - 1) | sign;
        let edges = [0, 1, sign - 1, sign, sign + 1, max - 1, max];
        let ops = [Op::Add, Op::Sub].into_iter().chain(
            saturated
                .into_iter()
                .filter(|_| lane_bytes <= size_of::<u16>()),
        );
        for op in ops {
            let mut context = module.make_context();
            context.func.signature.params = vec![AbiParam::new(types::I64); 3];
            let mut frontend = FunctionBuilderContext::new();
            let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let [left, right, destination] = *builder.block_params(entry) else {
                unreachable!()
            };
            let flags = MemFlags::new().with_notrap();
            let left = builder.ins().load(ty, flags, left, 0);
            let right = builder.ins().load(ty, flags, right, 0);
            let value = match op {
                Op::Add => builder.ins().iadd(left, right),
                Op::Sub => builder.ins().isub(left, right),
                Op::SignedAdd => builder.ins().sadd_sat(left, right),
                Op::UnsignedAdd => builder.ins().uadd_sat(left, right),
                Op::SignedSub => builder.ins().ssub_sat(left, right),
                Op::UnsignedSub => builder.ins().usub_sat(left, right),
                Op::Average => builder.ins().avg_round(left, right),
            };
            builder.ins().store(flags, value, destination, 0);
            builder.ins().return_(&[]);
            builder.seal_all_blocks();
            builder.finalize();
            let functions = compile(&mut module, &mut context);
            let lanes = VECTOR_BYTES / lane_bytes;
            for i in 0..edges.len() {
                for j in 0..edges.len() {
                    let mut left = [0; VECTOR_BYTES];
                    let mut right = [0; VECTOR_BYTES];
                    for lane in 0..lanes {
                        let range = lane * lane_bytes..(lane + 1) * lane_bytes;
                        let a = edges[(i + lane) % edges.len()].to_le_bytes();
                        let b = edges[(j + lane) % edges.len()].to_le_bytes();
                        left[range.clone()].copy_from_slice(&a[..lane_bytes]);
                        right[range].copy_from_slice(&b[..lane_bytes]);
                    }
                    type Entry = unsafe extern "C" fn(*const u8, *const u8, *mut u8);
                    let [reference, candidate] = functions.map(|pointer| {
                        let run = unsafe { std::mem::transmute::<*const u8, Entry>(pointer) };
                        let mut output = [0; VECTOR_BYTES];
                        unsafe { run(left.as_ptr(), right.as_ptr(), output.as_mut_ptr()) };
                        output
                    });
                    assert_eq!(candidate, reference, "{ty} {op:?} {i} {j}");
                }
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn packed_float_precision_conversions_match_cranelift() {
    let mut module = module();
    for demote in [false, true] {
        let input_type = if demote { types::F64X2 } else { types::F32X4 };
        let mut context = module.make_context();
        context.func.signature.params = vec![AbiParam::new(types::I64); 2];
        let mut frontend = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let [source, destination] = *builder.block_params(entry) else {
            unreachable!()
        };
        let flags = MemFlags::new().with_notrap();
        let source = builder.ins().load(input_type, flags, source, 0);
        let result = if demote {
            builder.ins().fvdemote(source)
        } else {
            builder.ins().fvpromote_low(source)
        };
        builder.ins().store(flags, result, destination, 0);
        builder.ins().return_(&[]);
        builder.seal_all_blocks();
        builder.finalize();
        let functions = compile(&mut module, &mut context);
        for input in [
            [0u64, 0],
            [0x8000_0000_8000_0000, 0x7ff0_0000_7f80_0000],
            [0x7ff0_0000_7f80_0042, 0x7ff8_0000_7fc0_0042],
            [0x3ff0_0000_3f80_0000, 0xffef_ffff_ff7f_ffff],
            [u64::MAX, u64::MAX],
        ] {
            type Entry = unsafe extern "C" fn(*const u64, *mut u64);
            let [reference, candidate] = functions.map(|pointer| {
                let run = unsafe { std::mem::transmute::<*const u8, Entry>(pointer) };
                let mut output = [u64::MAX; 2];
                unsafe { run(input.as_ptr(), output.as_mut_ptr()) };
                output
            });
            assert_eq!(candidate, reference, "{input_type} {input:?}");
        }
    }
    unsafe { module.free_memory() };
}
