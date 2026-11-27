use super::{aarch64_leaf_words, binding, integer_block_words};
use cranelift_codegen::ir::condcodes::FloatCC;
use cranelift_codegen::{
    Context,
    ir::{
        AbiParam, ConstantData, InstBuilder, MemFlags, Opcode, StackSlotData, StackSlotKind, types,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;
use vesper::native::machine::x86_64_block_words;

const VECTOR_BYTES: usize = 16;
const UNALIGNED: i32 = 1;
const NAN_CANONICALIZATION: &str = "enable_nan_canonicalization";

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
    compile_with_reference(module, context, None)
}

fn compile_with_reference(
    module: &mut JITModule,
    context: &mut Context,
    baseline: Option<&mut Context>,
) -> [*const u8; 2] {
    let arm = aarch64_leaf_words(&context.func).expect("AArch64 vector machine request");
    assert!(!binding::aarch64_integer_leaf(&arm).is_empty(), "{arm:?}");
    let words = x86_64_block_words(&context.func, module.isa()).expect("vector machine request");
    let bytes = binding::x86_64_integer_block(&words);
    assert!(!bytes.is_empty(), "{words:?}");
    let reference = module
        .declare_anonymous_function(&context.func.signature)
        .unwrap();
    let candidate = module
        .declare_anonymous_function(&context.func.signature)
        .unwrap();
    module
        .define_function(reference, baseline.unwrap_or(context))
        .unwrap();
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

#[test]
fn packed_float_arithmetic_matches_cranelift() {
    const ARM_ADD: u32 = 0x4e20_d400;
    const ARM_SUB: u32 = 0x4ea0_d400;
    const ARM_MUL: u32 = 0x6e20_dc00;
    const ARM_DIV: u32 = 0x6e20_fc00;
    const ARM_SQRT: u32 = 0x6ea1_f800;
    const ARM_DOUBLE: u32 = 0x0040_0000;
    const ARM_RN_RD: u32 = 0x3ff;
    const ARM_RM: u32 = 0x1f_0000;
    type Entry = unsafe extern "C" fn(*const u8, *const u8, *mut u8);
    let mut module = module();
    let mut flags = settings::builder();
    flags.enable(NAN_CANONICALIZATION).unwrap();
    let deterministic = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(flags))
        .unwrap();
    for ty in [types::F32X4, types::F64X2] {
        let width = ty.lane_type().bytes() as usize;
        let values = if ty == types::F32X4 {
            [
                0,
                0x8000_0000,
                1,
                0x4040_0000,
                0xbf00_0000,
                0x7f80_0000,
                0x7fc0_0003,
                0xffc0_0005,
                0x7f80_0007,
            ]
        } else {
            [
                0,
                0x8000_0000_0000_0000,
                1,
                0x4008_0000_0000_0000,
                0xbfe0_0000_0000_0000,
                0x7ff0_0000_0000_0000,
                0x7ff8_0000_0000_0003,
                0xfff8_0000_0000_0005,
                0x7ff0_0000_0000_0007,
            ]
        };
        let lanes = |first| -> [u8; VECTOR_BYTES] {
            std::array::from_fn(|index| {
                let lane: u64 = values[(first + index / width) % values.len()];
                lane.to_le_bytes()[index % width]
            })
        };
        for (opcode, arm) in [
            (Opcode::Fadd, ARM_ADD),
            (Opcode::Fsub, ARM_SUB),
            (Opcode::Fmul, ARM_MUL),
            (Opcode::Fdiv, ARM_DIV),
            (Opcode::Sqrt, ARM_SQRT),
        ] {
            for keep_left in [false, true] {
                let mut context = module.make_context();
                context.func.signature.params = vec![AbiParam::new(types::I64); 3];
                let mut state = FunctionBuilderContext::new();
                let mut b = FunctionBuilder::new(&mut context.func, &mut state);
                let entry = b.create_block();
                b.append_block_params_for_function_params(entry);
                b.switch_to_block(entry);
                let [left, right, destination] = *b.block_params(entry) else {
                    unreachable!()
                };
                let flags = MemFlags::new().with_notrap();
                let left = b.ins().load(ty, flags, left, 0);
                let right = b.ins().load(ty, flags, right, 0);
                let result = match opcode {
                    Opcode::Fadd => b.ins().fadd(left, right),
                    Opcode::Fsub => b.ins().fsub(left, right),
                    Opcode::Fmul => b.ins().fmul(left, right),
                    Opcode::Fdiv => b.ins().fdiv(left, right),
                    _ => b.ins().sqrt(left),
                };
                b.ins().store(flags, result, destination, 0);
                if keep_left {
                    b.ins().store(flags, left, destination, VECTOR_BYTES as i32);
                }
                b.ins().return_(&[]);
                b.seal_all_blocks();
                b.finalize();

                assert!(x86_64_block_words(&context.func, deterministic.as_ref()).is_none());
                let tape = aarch64_leaf_words(&context.func).unwrap();
                let mask = !(ARM_RN_RD | if opcode == Opcode::Sqrt { 0 } else { ARM_RM });
                let arm = arm | if ty == types::F64X2 { ARM_DOUBLE } else { 0 };
                let arm_bytes = binding::aarch64_integer_leaf(&tape);
                assert!(
                    arm_bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|instruction| { u32::from_le_bytes(*instruction) & mask == arm }),
                    "{opcode} {ty}"
                );

                let functions = compile(&mut module, &mut context);
                let functions = functions
                    .map(|pointer| unsafe { std::mem::transmute::<*const u8, Entry>(pointer) });
                for i in 0..values.len() {
                    for j in 0..values.len() {
                        let (left, right) = (lanes(i), lanes(j));
                        let [reference, candidate] = functions.map(|run| {
                            let mut out = [0xcc; VECTOR_BYTES * 2];
                            unsafe { run(left.as_ptr(), right.as_ptr(), out.as_mut_ptr()) };
                            out
                        });
                        assert_eq!(candidate[VECTOR_BYTES..], reference[VECTOR_BYTES..]);
                        for (got, want) in candidate[..VECTOR_BYTES]
                            .chunks_exact(width)
                            .zip(reference[..VECTOR_BYTES].chunks_exact(width))
                        {
                            // Ordinary floating-point arithmetic leaves the NaN bits unspecified.
                            let is_nan = |lane: &[u8]| {
                                if ty == types::F32X4 {
                                    f32::from_le_bytes(lane.try_into().unwrap()).is_nan()
                                } else {
                                    f64::from_le_bytes(lane.try_into().unwrap()).is_nan()
                                }
                            };
                            assert!(
                                got == want || (is_nan(got) && is_nan(want)),
                                "{opcode} {ty} {keep_left} {i} {j} {got:x?} {want:x?}"
                            );
                        }
                    }
                }
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn packed_float_comparisons_match_every_cranelift_predicate() {
    const LIVE_VECTORS: u8 = 16;
    type Entry = unsafe extern "C" fn(*const u64, *const u64, *mut u64);
    let mut module = module();
    for ty in [types::F32X4, types::F64X2] {
        let inputs: [[u64; 2]; 6] = if ty == types::F32X4 {
            [
                [0x8000_0000_0000_0000, 0x0000_0001_8000_0001],
                [0x4040_0000_bf00_0000, 0xbf00_0000_4040_0000],
                [0x7f80_0000_ff80_0000, 0xff80_0000_7f80_0000],
                [0x7fc0_0003_ffc0_0005, 0xffc0_0005_7fc0_0003],
                [0x7f80_0007_4040_0000, 0x4040_0000_7f80_0007],
                [0x7f7f_ffff_ff7f_ffff, 0x3f80_0000_bf80_0000],
            ]
        } else {
            [
                [0, 0x8000_0000_0000_0000],
                [0x4008_0000_0000_0000, 0xbfe0_0000_0000_0000],
                [0x7ff0_0000_0000_0000, 0xfff0_0000_0000_0000],
                [0x7ff8_0000_0000_0003, 0xfff8_0000_0000_0005],
                [0x7ff0_0000_0000_0007, 0x4008_0000_0000_0000],
                [1, 0x8000_0000_0000_0001],
            ]
        };
        for &predicate in FloatCC::all() {
            for (keep_inputs, spills) in [(false, false), (true, false), (false, true)] {
                let build = |module: &JITModule, compose| {
                    let mut context = module.make_context();
                    context.func.signature.params = vec![AbiParam::new(types::I64); 3];
                    let mut state = FunctionBuilderContext::new();
                    let mut b = FunctionBuilder::new(&mut context.func, &mut state);
                    let entry = b.create_block();
                    b.append_block_params_for_function_params(entry);
                    b.switch_to_block(entry);
                    let [left, right, destination] = *b.block_params(entry) else {
                        unreachable!()
                    };
                    let live = if spills {
                        (1..=LIVE_VECTORS)
                            .map(|byte| {
                                let bits = b
                                    .func
                                    .dfg
                                    .constants
                                    .insert(ConstantData::from(vec![byte; VECTOR_BYTES]));
                                b.ins().vconst(ty.as_int(), bits)
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let flags = MemFlags::new().with_notrap();
                    let left = b.ins().load(ty, flags, left, 0);
                    let right = b.ins().load(ty, flags, right, 0);
                    let mut mask = if compose {
                        let less = b.ins().fcmp(FloatCC::LessThan, left, right);
                        let greater = b.ins().fcmp(FloatCC::GreaterThan, left, right);
                        let unequal = b.ins().bor(less, greater);
                        if predicate == FloatCC::UnorderedOrEqual {
                            b.ins().bnot(unequal)
                        } else {
                            unequal
                        }
                    } else {
                        b.ins().fcmp(predicate, left, right)
                    };
                    for value in live {
                        mask = b.ins().bxor(mask, value);
                    }
                    b.ins().store(flags, mask, destination, 0);
                    if keep_inputs {
                        b.ins().store(flags, left, destination, VECTOR_BYTES as i32);
                        b.ins()
                            .store(flags, right, destination, (VECTOR_BYTES * 2) as i32);
                    }
                    b.ins().return_(&[]);
                    b.seal_all_blocks();
                    b.finalize();
                    context
                };
                let mut context = build(&module, false);
                // Pinned Cranelift x64 lacks direct vector instructions for these predicates.
                let mut baseline = matches!(
                    predicate,
                    FloatCC::OrderedNotEqual | FloatCC::UnorderedOrEqual
                )
                .then(|| build(&module, true));
                let functions =
                    compile_with_reference(&mut module, &mut context, baseline.as_mut());
                let functions = functions
                    .map(|pointer| unsafe { std::mem::transmute::<*const u8, Entry>(pointer) });
                for left in &inputs {
                    for right in &inputs {
                        let [reference, candidate] = functions.map(|run| {
                            let mut out = [u64::MAX; 6];
                            unsafe { run(left.as_ptr(), right.as_ptr(), out.as_mut_ptr()) };
                            out
                        });
                        assert_eq!(
                            candidate, reference,
                            "{predicate:?} {ty} {keep_inputs} {spills} {left:x?} {right:x?}"
                        );
                    }
                }
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn packed_float_sign_operations_preserve_every_non_sign_bit() {
    const LIVE_VECTORS: u8 = 21;
    type Entry = unsafe extern "C" fn(*const u64, *const u64, *mut u64);
    let mut module = module();
    let mut flags = settings::builder();
    flags.enable(NAN_CANONICALIZATION).unwrap();
    let deterministic = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(flags))
        .unwrap();
    for ty in [types::F32X4, types::F64X2] {
        let (lane_signs, inputs): (u64, [[u64; 2]; 5]) = if ty == types::F32X4 {
            (
                0x8000_0000_8000_0000,
                [
                    [0x8000_0000_0000_0000, 0x0000_0001_8000_0001],
                    [0x4040_0000_bf00_0000, 0xbf00_0000_4040_0000],
                    [0x7f80_0000_ff80_0000, 0xff80_0000_7f80_0000],
                    [0x7fc0_0003_ffc0_0005, 0xffc0_0005_7fc0_0003],
                    [0x7f80_0007_4040_0000, 0x4040_0000_ff80_0007],
                ],
            )
        } else {
            (
                0x8000_0000_0000_0000,
                [
                    [0, 0x8000_0000_0000_0000],
                    [0x7ff0_0000_0000_0000, 0xfff0_0000_0000_0000],
                    [0x7ff8_0000_0000_0003, 0xfff8_0000_0000_0005],
                    [0x7ff0_0000_0000_0007, 0xfff0_0000_0000_0007],
                    [1, 0x8000_0000_0000_0001],
                ],
            )
        };
        for opcode in [Opcode::Fneg, Opcode::Fabs, Opcode::Fcopysign] {
            for (keep_left, spills) in [(false, false), (true, false), (false, true)] {
                let build = |module: &JITModule, expanded| {
                    let mut context = module.make_context();
                    context.func.signature.params = vec![AbiParam::new(types::I64); 3];
                    let mut state = FunctionBuilderContext::new();
                    let mut b = FunctionBuilder::new(&mut context.func, &mut state);
                    let entry = b.create_block();
                    b.append_block_params_for_function_params(entry);
                    b.switch_to_block(entry);
                    let [left, sign, destination] = *b.block_params(entry) else {
                        unreachable!()
                    };
                    let live = if spills {
                        (1..=LIVE_VECTORS)
                            .map(|byte| {
                                let constant = b
                                    .func
                                    .dfg
                                    .constants
                                    .insert(ConstantData::from(vec![byte; VECTOR_BYTES]));
                                b.ins().vconst(ty.as_int(), constant)
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let flags = MemFlags::new().with_notrap();
                    let left = b.ins().load(ty, flags, left, 0);
                    let mut result = match opcode {
                        Opcode::Fneg => b.ins().fneg(left),
                        Opcode::Fabs => b.ins().fabs(left),
                        _ => {
                            let sign = b.ins().load(ty, flags, sign, 0);
                            if expanded {
                                let mut bits = lane_signs.to_le_bytes().to_vec();
                                bits.extend(lane_signs.to_le_bytes());
                                let constant =
                                    b.func.dfg.constants.insert(ConstantData::from(bits));
                                let mask = b.ins().vconst(ty, constant);
                                b.ins().bitselect(mask, sign, left)
                            } else {
                                b.ins().fcopysign(left, sign)
                            }
                        }
                    };
                    if spills {
                        result = b.ins().bitcast(ty.as_int(), MemFlags::new(), result);
                        for value in live {
                            result = b.ins().bxor(result, value);
                        }
                    }
                    b.ins().store(flags, result, destination, 0);
                    if keep_left {
                        b.ins().store(flags, left, destination, VECTOR_BYTES as i32);
                    }
                    b.ins().return_(&[]);
                    b.seal_all_blocks();
                    b.finalize();
                    context
                };
                let mut context = build(&module, false);
                assert!(x86_64_block_words(&context.func, deterministic.as_ref()).is_some());
                // Cranelift x64 selects scalar copy-sign. Its vector bitselect gives the reference.
                let mut baseline = (opcode == Opcode::Fcopysign).then(|| build(&module, true));
                let functions =
                    compile_with_reference(&mut module, &mut context, baseline.as_mut());
                let functions = functions
                    .map(|pointer| unsafe { std::mem::transmute::<*const u8, Entry>(pointer) });
                for left in &inputs {
                    for sign in &inputs {
                        let [reference, candidate] = functions.map(|run| {
                            let mut out = [u64::MAX; 4];
                            unsafe { run(left.as_ptr(), sign.as_ptr(), out.as_mut_ptr()) };
                            out
                        });
                        assert_eq!(
                            candidate, reference,
                            "{opcode} {ty} {keep_left} {spills} {left:x?} {sign:x?}"
                        );
                    }
                }
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn vector_lane_access_matches_cranelift_for_each_index() {
    let mut module = module();
    for ty in [
        types::I8X16,
        types::I16X8,
        types::I32X4,
        types::I64X2,
        types::F32X4,
        types::F64X2,
    ] {
        let lane = ty.lane_type();
        let int = lane.as_int();
        let width = lane.bytes() as usize;
        for index in 0..ty.lane_count() {
            let mut context = module.make_context();
            context.func.signature.params = vec![AbiParam::new(types::I64); 3];
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
            let [source, destination, bits] = *builder.block_params(entry) else {
                unreachable!()
            };
            let flags = MemFlags::new().with_notrap();
            let before = builder.ins().load(ty, flags, source, 0);
            let input = if int == types::I64 {
                bits
            } else {
                builder.ins().ireduce(int, bits)
            };
            let input = if lane.is_float() {
                builder.ins().bitcast(lane, MemFlags::new(), input)
            } else {
                input
            };
            let after = builder.ins().insertlane(before, input, index as u8);
            let extracted = builder.ins().extractlane(before, index as u8);
            let extracted = if lane.is_float() {
                builder.ins().bitcast(int, MemFlags::new(), extracted)
            } else {
                extracted
            };
            let extracted = if int == types::I64 {
                extracted
            } else {
                builder.ins().uextend(types::I64, extracted)
            };
            builder.ins().store(flags, after, destination, 0);
            builder.ins().return_(&[extracted]);
            builder.seal_all_blocks();
            builder.finalize();

            let functions = compile(&mut module, &mut context);
            let source: [u8; VECTOR_BYTES] = std::array::from_fn(|i| 0xa5 ^ (i as u8 * 13));
            let start = index as usize * width;
            let range = start..start + width;
            let mut old = [0u8; size_of::<u64>()];
            old[..width].copy_from_slice(&source[range.clone()]);
            for bits in [u64::MAX, 0x7ff8_ccdd_7fc0_0042, 0x4488_3322_fe67_8910] {
                let mut want = source;
                want[range.clone()].copy_from_slice(&bits.to_le_bytes()[..width]);
                for pointer in functions {
                    type Entry = unsafe extern "C" fn(*const u8, *mut u8, u64) -> u64;
                    let run = unsafe { std::mem::transmute::<*const u8, Entry>(pointer) };
                    let mut got = [0; VECTOR_BYTES];
                    let result = unsafe { run(source.as_ptr(), got.as_mut_ptr(), bits) };
                    assert_eq!(got, want, "{ty} lane {index}");
                    assert_eq!(result, u64::from_le_bytes(old), "{ty} lane {index}");
                }
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn vector_reductions_match_cranelift_on_zero_and_sign_bits() {
    let mut module = module();
    for ty in [types::I8X16, types::I16X8, types::I32X4, types::I64X2] {
        let width = ty.lane_type().bytes() as usize;
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
        let vector = builder.ins().load(ty, flags, source, 0);
        let any = builder.ins().vany_true(vector);
        let any = builder.ins().uextend(types::I64, any);
        let all = builder.ins().vall_true(vector);
        let all = builder.ins().uextend(types::I64, all);
        let mask = builder.ins().vhigh_bits(types::I64, vector);
        for (i, value) in [any, all, mask].into_iter().enumerate() {
            builder
                .ins()
                .store(flags, value, destination, (i * size_of::<u64>()) as i32);
        }
        builder.ins().return_(&[]);
        builder.seal_all_blocks();
        builder.finalize();

        let functions = compile(&mut module, &mut context);
        let signed: [u8; VECTOR_BYTES] = std::array::from_fn(|i| {
            if i % width == width - 1 && (i / width).is_multiple_of(2) {
                0x80
            } else {
                0
            }
        });
        let low_bytes: [u8; VECTOR_BYTES] =
            std::array::from_fn(|i| u8::from(i.is_multiple_of(width)));
        for source in [
            [0u8; VECTOR_BYTES],
            [1; VECTOR_BYTES],
            [0xff; VECTOR_BYTES],
            signed,
            low_bytes,
        ] {
            let lanes = source.chunks_exact(width);
            let nonzero = |lane: &[u8]| lane.iter().any(|&b| b != 0);
            let any = u64::from(lanes.clone().any(nonzero));
            let all = u64::from(lanes.clone().all(nonzero));
            let sign = lanes.enumerate().fold(0, |acc, (i, lane)| {
                acc | (u64::from(lane[width - 1] >> (u8::BITS - 1)) << i)
            });
            for pointer in functions {
                type Entry = unsafe extern "C" fn(*const u8, *mut u64);
                let run = unsafe { std::mem::transmute::<*const u8, Entry>(pointer) };
                let mut output = [u64::MAX; 3];
                unsafe { run(source.as_ptr(), output.as_mut_ptr()) };
                assert_eq!(output, [any, all, sign], "{ty} {source:?}");
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn vector_rearrangements_match_cranelift_with_ssse3() {
    if !std::is_x86_feature_detected!("ssse3") {
        return;
    }
    const LANES: u8 = VECTOR_BYTES as u8;
    #[derive(Clone, Copy, Debug)]
    enum Op {
        Pairwise,
        Swizzle,
        Shuffle([u8; VECTOR_BYTES], bool),
    }
    let mut module = module();
    let cases = [
        (types::I8X16, Op::Pairwise),
        (types::I16X8, Op::Pairwise),
        (types::I32X4, Op::Pairwise),
        (types::I8X16, Op::Swizzle),
        (
            types::I8X16,
            Op::Shuffle(std::array::from_fn(|i| LANES - 1 - i as u8), false),
        ),
        (
            types::I8X16,
            Op::Shuffle(std::array::from_fn(|i| LANES + i as u8), false),
        ),
        (
            types::I8X16,
            Op::Shuffle(std::array::from_fn(|i| i as u8), false),
        ),
        (
            types::I8X16,
            Op::Shuffle(
                std::array::from_fn(|i| i as u8 + if i.is_multiple_of(2) { LANES } else { 0 }),
                true,
            ),
        ),
        (
            types::I8X16,
            Op::Shuffle(
                std::array::from_fn(|i| (i as u8 * 3 + 1) % (2 * LANES)),
                false,
            ),
        ),
        (
            types::I8X16,
            Op::Shuffle(
                std::array::from_fn(|i| (i as u8 * 5 + 2) % (2 * LANES)),
                true,
            ),
        ),
    ];
    for (ty, op) in cases {
        let mut context = module.make_context();
        context.func.signature.params = vec![AbiParam::new(types::I64); 3];
        let mut frontend = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let [lhs, rhs, destination] = *builder.block_params(entry) else {
            unreachable!()
        };
        let flags = MemFlags::new().with_notrap();
        let left = builder.ins().load(ty, flags, lhs, 0);
        let right = builder.ins().load(ty, flags, rhs, 0);
        let result = match op {
            Op::Pairwise => builder.ins().iadd_pairwise(left, right),
            Op::Swizzle => builder.ins().swizzle(left, right),
            Op::Shuffle(mask, same_table) => {
                let immediate = builder
                    .func
                    .dfg
                    .immediates
                    .push(ConstantData::from(mask.to_vec()));
                builder
                    .ins()
                    .shuffle(left, if same_table { left } else { right }, immediate)
            }
        };
        builder.ins().store(flags, result, destination, 0);
        builder.ins().return_(&[]);
        builder.seal_all_blocks();
        builder.finalize();

        let baseline = integer_block_words(&context.func);
        let identity = matches!(op, Op::Shuffle(mask, same_table)
            if mask.iter().copied().eq(0..LANES)
                || mask.iter().copied().eq(LANES..LANES * 2)
                || same_table && mask.iter().map(|&index| index % LANES).eq(0..LANES));
        assert_eq!(baseline.is_some(), identity, "{ty} {op:?}");
        if let Some(words) = baseline {
            assert!(!binding::x86_64_integer_block(&words).is_empty());
        }
        let functions = compile(&mut module, &mut context);
        let width = ty.lane_type().bytes() as usize;
        for chunk in 0..=u8::MAX / LANES {
            let left: [u8; VECTOR_BYTES] =
                std::array::from_fn(|i| 0xffu8.wrapping_sub(chunk * 7).wrapping_add(i as u8 * 5));
            let right: [u8; VECTOR_BYTES] = std::array::from_fn(|i| chunk * LANES + i as u8);
            let mut expected = [0; VECTOR_BYTES];
            match op {
                Op::Pairwise => {
                    let input = [left, right].concat();
                    for (pair, lane) in input
                        .chunks_exact(width * 2)
                        .zip(expected.chunks_exact_mut(width))
                    {
                        let bits = |source: &[u8]| {
                            let mut bytes = [0; size_of::<u32>()];
                            bytes[..width].copy_from_slice(source);
                            u32::from_le_bytes(bytes)
                        };
                        let sum = bits(&pair[..width]).wrapping_add(bits(&pair[width..]));
                        lane.copy_from_slice(&sum.to_le_bytes()[..width]);
                    }
                }
                Op::Swizzle => {
                    expected =
                        right.map(|index| left.get(usize::from(index)).copied().unwrap_or(0));
                }
                Op::Shuffle(mask, same_table) => {
                    let input = [left, if same_table { left } else { right }].concat();
                    expected = mask.map(|index| input[usize::from(index)]);
                }
            }
            for pointer in functions {
                type Entry = unsafe extern "C" fn(*const u8, *const u8, *mut u8);
                let run = unsafe { std::mem::transmute::<*const u8, Entry>(pointer) };
                let mut output = [0; VECTOR_BYTES];
                unsafe { run(left.as_ptr(), right.as_ptr(), output.as_mut_ptr()) };
                assert_eq!(output, expected, "{ty} {op:?} {chunk}");
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn packed_integer_comparisons_match_cranelift_on_sign_and_half_boundaries() {
    use cranelift_codegen::ir::condcodes::IntCC;

    type Entry = unsafe extern "C" fn(*const u8, *const u8, *mut u8);
    let mut module = module();
    for ty in [types::I8X16, types::I16X8, types::I32X4, types::I64X2] {
        let width = ty.lane_type().bytes() as usize;
        let bits = ty.lane_type().bits();
        let sign = 1u64 << (bits - 1);
        let mask = u64::MAX >> (u64::BITS - bits);
        let cases = [
            (0, 0),
            (mask, 0),
            (0, mask),
            (sign, sign - 1),
            (sign - 1, sign),
            (sign, sign),
            (0x7fff_ffff, 0x8000_0000),
            (0x0100_0000_0000_0000, 0x00ff_ffff_ffff_ffff),
        ];
        for &cc in IntCC::all() {
            let mut context = module.make_context();
            context.func.signature.params = vec![AbiParam::new(types::I64); 3];
            let mut frontend = FunctionBuilderContext::new();
            let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let [a, b, output] = *builder.block_params(entry) else {
                unreachable!()
            };
            let flags = MemFlags::new().with_notrap();
            let lhs = builder.ins().load(ty, flags, a, 0);
            let rhs = builder.ins().load(ty, flags, b, 0);
            let result = builder.ins().icmp(cc, lhs, rhs);
            builder.ins().store(flags, result, output, 0);
            builder.ins().return_(&[]);
            builder.seal_all_blocks();
            builder.finalize();
            let functions = compile(&mut module, &mut context);
            for start in 0..cases.len() {
                let mut a = [0; VECTOR_BYTES];
                let mut b = [0; VECTOR_BYTES];
                let mut expected = [0; VECTOR_BYTES];
                for lane in 0..VECTOR_BYTES / width {
                    let (x, y) = cases[(start + lane) % cases.len()];
                    let (x, y) = (x & mask, y & mask);
                    let (sx, sy) = (x ^ sign, y ^ sign);
                    let value = match cc {
                        IntCC::Equal => x == y,
                        IntCC::NotEqual => x != y,
                        IntCC::SignedLessThan => sx < sy,
                        IntCC::SignedGreaterThanOrEqual => sx >= sy,
                        IntCC::SignedGreaterThan => sx > sy,
                        IntCC::SignedLessThanOrEqual => sx <= sy,
                        IntCC::UnsignedLessThan => x < y,
                        IntCC::UnsignedGreaterThanOrEqual => x >= y,
                        IntCC::UnsignedGreaterThan => x > y,
                        IntCC::UnsignedLessThanOrEqual => x <= y,
                    };
                    let range = lane * width..(lane + 1) * width;
                    a[range.clone()].copy_from_slice(&x.to_le_bytes()[..width]);
                    b[range.clone()].copy_from_slice(&y.to_le_bytes()[..width]);
                    expected[range].fill(if value { u8::MAX } else { 0 });
                }
                for pointer in functions {
                    let run = unsafe { std::mem::transmute::<*const u8, Entry>(pointer) };
                    let mut result = [0; VECTOR_BYTES];
                    unsafe { run(a.as_ptr(), b.as_ptr(), result.as_mut_ptr()) };
                    assert_eq!(result, expected, "{ty} {cc:?} {start}");
                }
            }
        }
    }
    unsafe { module.free_memory() };
}
