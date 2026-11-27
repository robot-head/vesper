use super::binding;
use cranelift_codegen::{
    Context,
    ir::{
        AbiParam, BlockArg, InstBuilder, MemFlags, StackSlotData, StackSlotKind, Type, Value,
        condcodes::{FloatCC, IntCC},
        types,
    },
    settings,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;
use vesper::native::machine::x86_64_block_words;

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

fn compile(module: &mut JITModule, context: &mut Context) -> (*const u8, *const u8) {
    compile_with_reference(module, context, None)
}

fn compile_with_reference(
    module: &mut JITModule,
    context: &mut Context,
    reference: Option<&mut Context>,
) -> (*const u8, *const u8) {
    let request =
        x86_64_block_words(&context.func, module.isa()).expect("floating machine request");
    let code = binding::x86_64_integer_block(&request);
    assert!(!code.is_empty(), "{}", context.func.display());
    let baseline = module
        .declare_anonymous_function(&context.func.signature)
        .unwrap();
    let candidate = module
        .declare_anonymous_function(&context.func.signature)
        .unwrap();
    module
        .define_function(baseline, reference.unwrap_or(context))
        .unwrap();
    let alignment = u64::from(module.isa().function_alignment().minimum);
    module
        .define_function_bytes(candidate, alignment, &code, &[])
        .unwrap();
    module.finalize_definitions().unwrap();
    (
        module.get_finalized_function(baseline),
        module.get_finalized_function(candidate),
    )
}

fn scalar_conversion(
    module: &mut JITModule,
    convert: impl FnOnce(&mut FunctionBuilder<'_>, Value) -> Value,
    output: Type,
) -> [unsafe extern "C" fn(u64) -> u64; 2] {
    let mut context = scalar_conversion_context(module, convert, output);
    conversion_entries(compile(module, &mut context))
}

fn scalar_conversion_context(
    module: &JITModule,
    convert: impl FnOnce(&mut FunctionBuilder<'_>, Value) -> Value,
    output: Type,
) -> Context {
    let mut context = module.make_context();
    context
        .func
        .signature
        .params
        .push(AbiParam::new(types::I64));
    context
        .func
        .signature
        .returns
        .push(AbiParam::new(types::I64));
    let mut state = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut state);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let input = builder.block_params(entry)[0];
    let value = convert(&mut builder, input);
    let bits = if output.is_float() {
        builder
            .ins()
            .bitcast(output.as_int(), MemFlags::new(), value)
    } else {
        value
    };
    let result = if output.bits() < u64::BITS {
        builder.ins().uextend(types::I64, bits)
    } else {
        bits
    };
    builder.ins().return_(&[result]);
    builder.seal_all_blocks();
    builder.finalize();
    context
}

fn conversion_entries((a, b): (*const u8, *const u8)) -> [unsafe extern "C" fn(u64) -> u64; 2] {
    [a, b].map(|pointer| unsafe {
        std::mem::transmute::<*const u8, unsafe extern "C" fn(u64) -> u64>(pointer)
    })
}

#[test]
fn sse2_saturating_float_to_integer_matches_cranelift() {
    let mut module = module();
    for from in [types::F32, types::F64] {
        for output in [types::I8, types::I16, types::I32, types::I64] {
            for signed in [false, true] {
                let convert = |b: &mut FunctionBuilder<'_>, value, ty| {
                    let value = if from == types::F32 {
                        b.ins().ireduce(types::I32, value)
                    } else {
                        value
                    };
                    let float = b.ins().bitcast(from, MemFlags::new(), value);
                    if signed {
                        b.ins().fcvt_to_sint_sat(ty, float)
                    } else {
                        b.ins().fcvt_to_uint_sat(ty, float)
                    }
                };
                let mut context = scalar_conversion_context(
                    &module,
                    |b, value| convert(b, value, output),
                    output,
                );
                // Cranelift's x64 emitter needs a 32- or 64-bit conversion.
                let mut reference = (output.bits() < u32::BITS).then(|| {
                    scalar_conversion_context(
                        &module,
                        |b, value| {
                            let wide = convert(b, value, types::I32);
                            let edge = 1i64 << (output.bits() - u32::from(signed));
                            let high = b.ins().iconst(types::I32, edge - 1);
                            let clamped = if signed {
                                let low = b.ins().iconst(types::I32, -edge);
                                let lower = b.ins().smax(wide, low);
                                b.ins().smin(lower, high)
                            } else {
                                b.ins().umin(wide, high)
                            };
                            b.ins().ireduce(output, clamped)
                        },
                        output,
                    )
                });
                let functions = conversion_entries(compile_with_reference(
                    &mut module,
                    &mut context,
                    reference.as_mut(),
                ));
                let edge = 2f64.powi((output.bits() - u32::from(signed)) as i32);
                let signed_qword_edge = 2f64.powi((i64::BITS - 1) as i32);
                let next_down = |input: f64| {
                    if from == types::F32 {
                        f64::from((input as f32).next_down())
                    } else {
                        input.next_down()
                    }
                };
                let next_up = |input: f64| {
                    if from == types::F32 {
                        f64::from((input as f32).next_up())
                    } else {
                        input.next_up()
                    }
                };
                for input in [
                    f64::NAN,
                    f64::NEG_INFINITY,
                    next_down(-edge),
                    -edge,
                    next_up(-edge),
                    -edge + 1.0,
                    -1.75,
                    -0.0,
                    0.0,
                    1.75,
                    edge - 1.0,
                    next_down(edge),
                    edge,
                    next_up(edge),
                    next_down(signed_qword_edge),
                    signed_qword_edge,
                    next_up(signed_qword_edge),
                    f64::INFINITY,
                ] {
                    let (bits, value) = if from == types::F32 {
                        let single = input as f32;
                        (u64::from(single.to_bits()), f64::from(single))
                    } else {
                        (input.to_bits(), input)
                    };
                    let expected = match (output, signed) {
                        (types::I8, true) => u64::from(value as i8 as u8),
                        (types::I8, false) => u64::from(value as u8),
                        (types::I16, true) => u64::from(value as i16 as u16),
                        (types::I16, false) => u64::from(value as u16),
                        (types::I32, true) => u64::from(value as i32 as u32),
                        (types::I32, false) => u64::from(value as u32),
                        (_, true) => value as i64 as u64,
                        (_, false) => value as u64,
                    };
                    for run in functions {
                        assert_eq!(
                            unsafe { run(bits) },
                            expected,
                            "{from} {output} {signed} {bits:#x}"
                        );
                    }
                }
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn sse2_min_max_matches_nan_payloads_and_signed_zero() {
    type Entry = unsafe extern "C" fn(u64, u64) -> u64;
    let mut module = module();
    for width in [types::F32, types::F64] {
        let int = width.as_int();
        for min in [false, true] {
            for keep_right in [false, true] {
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
                let [left, right] = *b.block_params(entry) else {
                    unreachable!()
                };
                let (left, right) = if width == types::F32 {
                    (b.ins().ireduce(int, left), b.ins().ireduce(int, right))
                } else {
                    (left, right)
                };
                let left = b.ins().bitcast(width, MemFlags::new(), left);
                let right = b.ins().bitcast(width, MemFlags::new(), right);
                let result = if min {
                    b.ins().fmin(left, right)
                } else {
                    b.ins().fmax(left, right)
                };
                let mut bits = b.ins().bitcast(int, MemFlags::new(), result);
                if keep_right {
                    let rhs = b.ins().bitcast(int, MemFlags::new(), right);
                    bits = b.ins().bxor(bits, rhs);
                }
                if width == types::F32 {
                    bits = b.ins().uextend(types::I64, bits);
                }
                b.ins().return_(&[bits]);
                b.seal_all_blocks();
                b.finalize();
                let (a, b) = compile(&mut module, &mut context);
                let functions = [a, b]
                    .map(|pointer| unsafe { std::mem::transmute::<*const u8, Entry>(pointer) });
                let values = if width == types::F32 {
                    [
                        0,
                        0x8000_0000,
                        1,
                        0x3f80_0000,
                        0xc000_0000,
                        0x7f80_0000,
                        0xff80_0000,
                        0x7fc0_0003,
                        0xffc0_0005,
                        0x7f80_0007,
                    ]
                } else {
                    [
                        0,
                        0x8000_0000_0000_0000,
                        1,
                        0x3ff0_0000_0000_0000,
                        0xc000_0000_0000_0000,
                        0x7ff0_0000_0000_0000,
                        0xfff0_0000_0000_0000,
                        0x7ff8_0000_0000_0003,
                        0xfff8_0000_0000_0005,
                        0x7ff0_0000_0000_0007,
                    ]
                };
                for x in values {
                    for y in values {
                        let [reference, candidate] = functions.map(|run| unsafe { run(x, y) });
                        assert_eq!(
                            candidate, reference,
                            "{width} min={min} {keep_right} {x:#x} {y:#x}"
                        );
                    }
                }
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn sse41_rounding_matches_cranelift_at_ties_and_signed_zero() {
    if !std::is_x86_feature_detected!("sse4.1") {
        return;
    }
    #[derive(Clone, Copy, Debug)]
    enum Round {
        Floor,
        Ceil,
        Nearest,
        Trunc,
    }
    let mut module = module();
    for width in [types::F32, types::F64] {
        for round in [Round::Floor, Round::Ceil, Round::Nearest, Round::Trunc] {
            let functions = scalar_conversion(
                &mut module,
                |b, value| {
                    let value = if width == types::F32 {
                        b.ins().ireduce(types::I32, value)
                    } else {
                        value
                    };
                    let float = b.ins().bitcast(width, MemFlags::new(), value);
                    match round {
                        Round::Floor => b.ins().floor(float),
                        Round::Ceil => b.ins().ceil(float),
                        Round::Nearest => b.ins().nearest(float),
                        Round::Trunc => b.ins().trunc(float),
                    }
                },
                width,
            );
            for value in [
                -3.5,
                -2.5,
                -0.5,
                -0.0,
                0.0,
                0.5,
                2.5,
                3.5,
                f64::INFINITY,
                f64::NAN,
            ] {
                let bits = if width == types::F32 {
                    u64::from((value as f32).to_bits())
                } else {
                    value.to_bits()
                };
                let [reference, candidate] = functions.map(|run| unsafe { run(bits) });
                assert_eq!(candidate, reference, "{width} {round:?} {bits:#x}");
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn scalar_rounding_declines_x86_targets_without_sse41() {
    let isa = cranelift_native::builder_with_options(false)
        .unwrap()
        .finish(settings::Flags::new(settings::builder()))
        .unwrap();
    assert!(!isa.has_round());
    let mut context = Context::new();
    context.func.signature.call_conv = isa.default_call_conv();
    context
        .func
        .signature
        .params
        .push(AbiParam::new(types::F32));
    context
        .func
        .signature
        .returns
        .push(AbiParam::new(types::F32));
    let mut state = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut context.func, &mut state);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let input = b.block_params(entry)[0];
    let result = b.ins().nearest(input);
    b.ins().return_(&[result]);
    b.seal_all_blocks();
    b.finalize();
    assert!(x86_64_block_words(&context.func, isa.as_ref()).is_none());
}

#[test]
fn sse2_integer_and_precision_conversions_match_cranelift() {
    let mut module = module();
    let convert_inputs = [
        0,
        1,
        u64::MAX,
        i64::MAX as u64,
        i64::MIN as u64,
        0x8000_0000_0000_0400,
        0x8000_0000_0000_0401,
        0xffff_ffff_ffff_fbff,
        0x0080_0000_8080_8081,
        0x7f00_0000_ffff_8001,
    ];
    for from in [types::I8, types::I16, types::I32, types::I64] {
        for output in [types::F32, types::F64] {
            for signed in [false, true] {
                let functions = scalar_conversion(
                    &mut module,
                    |builder, input| {
                        let input = if from == types::I64 {
                            input
                        } else {
                            builder.ins().ireduce(from, input)
                        };
                        if signed {
                            builder.ins().fcvt_from_sint(output, input)
                        } else {
                            builder.ins().fcvt_from_uint(output, input)
                        }
                    },
                    output,
                );
                let padding = u64::BITS - from.bits();
                for input in convert_inputs {
                    let bits = input & (u64::MAX >> padding);
                    let signed_bits = ((bits << padding) as i64) >> padding;
                    let expected = match (output, signed) {
                        (types::F32, true) => u64::from((signed_bits as f32).to_bits()),
                        (types::F32, false) => u64::from((bits as f32).to_bits()),
                        (_, true) => (signed_bits as f64).to_bits(),
                        (_, false) => (bits as f64).to_bits(),
                    };
                    for run in functions {
                        assert_eq!(
                            unsafe { run(input) },
                            expected,
                            "{from} {output} {signed} {input:#x}"
                        );
                    }
                }
            }
        }
    }
    let promote = scalar_conversion(
        &mut module,
        |builder, input| {
            let input = builder.ins().ireduce(types::I32, input);
            let single = builder.ins().bitcast(types::F32, MemFlags::new(), input);
            builder.ins().fpromote(types::F64, single)
        },
        types::F64,
    );
    for input in [
        0.0f32,
        -0.0,
        f32::from_bits(1),
        f32::MAX,
        f32::INFINITY,
        f32::NAN,
    ] {
        let bits = u64::from(input.to_bits());
        let [reference, candidate] = promote.map(|run| unsafe { run(bits) });
        assert_eq!(candidate, reference, "{bits:#x}");
    }
    let demote = scalar_conversion(
        &mut module,
        |builder, input| {
            let double = builder.ins().bitcast(types::F64, MemFlags::new(), input);
            builder.ins().fdemote(types::F32, double)
        },
        types::F32,
    );
    for input in [
        0.0f64,
        -0.0,
        f64::from_bits(1),
        f64::MAX,
        f64::INFINITY,
        f64::NAN,
    ] {
        let bits = input.to_bits();
        let [reference, candidate] = demote.map(|run| unsafe { run(bits) });
        assert_eq!(candidate, reference, "{bits:#x}");
    }
    unsafe { module.free_memory() };
}

#[test]
fn sse2_comparisons_match_all_cranelift_predicates() {
    type Single = unsafe extern "C" fn(f32, f32) -> u64;
    type Double = unsafe extern "C" fn(f64, f64) -> u64;
    let mut module = module();
    for width in [types::F32, types::F64] {
        for &cond in FloatCC::all() {
            let mut context = module.make_context();
            context
                .func
                .signature
                .params
                .extend([AbiParam::new(width); 2]);
            context
                .func
                .signature
                .returns
                .push(AbiParam::new(types::I64));
            let mut state = FunctionBuilderContext::new();
            let mut builder = FunctionBuilder::new(&mut context.func, &mut state);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let [left, right] = *builder.block_params(entry) else {
                unreachable!()
            };
            let flag = builder.ins().fcmp(cond, left, right);
            let result = builder.ins().uextend(types::I64, flag);
            builder.ins().return_(&[result]);
            builder.seal_all_blocks();
            builder.finalize();
            let (a, b) = compile(&mut module, &mut context);

            let values = [
                -3.0,
                -0.0,
                0.0,
                7.5,
                f64::MIN_POSITIVE,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NAN,
            ];
            for left in values {
                for right in values {
                    // The reference and candidate use the same scalar SysV ABI.
                    let (reference, candidate) = unsafe {
                        if width == types::F32 {
                            let a = std::mem::transmute::<*const u8, Single>(a);
                            let b = std::mem::transmute::<*const u8, Single>(b);
                            (a(left as f32, right as f32), b(left as f32, right as f32))
                        } else {
                            let a = std::mem::transmute::<*const u8, Double>(a);
                            let b = std::mem::transmute::<*const u8, Double>(b);
                            (a(left, right), b(left, right))
                        }
                    };
                    assert_eq!(candidate, reference, "{cond:?}({left}, {right}) {width}");
                }
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn sse2_values_survive_spills_bitcasts_and_mixed_register_arguments() {
    type Entry = unsafe extern "C" fn(u64, f64, f64, *mut f64) -> f64;
    const PRESSURE_TERMS: usize = 20;
    const STACK_SLOT_ALIGNMENT_LOG2: u8 = 3;
    let mut module = module();
    let mut context = module.make_context();
    context
        .func
        .signature
        .params
        .extend([types::I64, types::F64, types::F64, types::I64].map(AbiParam::new));
    context
        .func
        .signature
        .returns
        .push(AbiParam::new(types::F64));
    let mut state = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut state);
    let entry = builder.create_block();
    let positive = builder.create_block();
    let negative = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let [choose, left, right, address] = *builder.block_params(entry) else {
        unreachable!()
    };
    let slot = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        size_of::<f64>() as u32,
        STACK_SLOT_ALIGNMENT_LOG2,
    ));
    let terms: Vec<_> = (1..=PRESSURE_TERMS)
        .map(|i| {
            let constant = builder.ins().f64const(i as f64);
            builder.ins().fmul(right, constant)
        })
        .collect();
    let selected = builder.ins().select(choose, left, right);
    let negated = builder.ins().fneg(selected);
    builder
        .ins()
        .store(MemFlags::trusted(), negated, address, 0);
    builder.ins().stack_store(negated, slot, 0);
    let from_stack = builder.ins().stack_load(types::F64, slot, 0);
    let from_memory = builder
        .ins()
        .load(types::F64, MemFlags::trusted(), address, 0);
    let bits = builder
        .ins()
        .bitcast(types::I64, MemFlags::new(), from_memory);
    let roundtrip = builder.ins().bitcast(types::F64, MemFlags::new(), bits);
    let difference = builder.ins().fsub(from_stack, left);
    let absolute = builder.ins().fabs(difference);
    let root = builder.ins().sqrt(absolute);
    let total = terms
        .into_iter()
        .fold(root, |sum, term| builder.ins().fadd(sum, term));
    let quotient = builder.ins().fdiv(total, roundtrip);
    let branch = builder.ins().icmp_imm(IntCC::NotEqual, choose, 0);
    builder.ins().brif(branch, positive, &[], negative, &[]);
    builder.switch_to_block(positive);
    builder.ins().return_(&[quotient]);
    builder.switch_to_block(negative);
    builder.ins().return_(&[selected]);
    builder.seal_all_blocks();
    builder.finalize();

    let (reference, candidate) = compile(&mut module, &mut context);
    let reference = unsafe { std::mem::transmute::<*const u8, Entry>(reference) };
    let candidate = unsafe { std::mem::transmute::<*const u8, Entry>(candidate) };
    for (choose, left, right) in [
        (1, 3.0, 8.0),
        (1, 0.5, -2.0),
        (0, -3.0, -0.0),
        (0, 1.0, f64::from_bits(0x7ff8_0000_0000_0042)),
    ] {
        let mut reference_memory = 0.0;
        let mut candidate_memory = 0.0;
        let a = unsafe { reference(choose, left, right, &mut reference_memory) };
        let b = unsafe { candidate(choose, left, right, &mut candidate_memory) };
        assert_eq!(a.to_bits(), b.to_bits());
        assert_eq!(reference_memory.to_bits(), candidate_memory.to_bits());
    }
    unsafe { module.free_memory() };
}

#[test]
fn labeled_float_edges_preserve_parallel_values_and_copy_sign_bits() {
    type Single = unsafe extern "C" fn(u64, f32, f32) -> f32;
    type Double = unsafe extern "C" fn(u64, f64, f64) -> f64;
    let mut module = module();
    for width in [types::F32, types::F64] {
        let mut context = module.make_context();
        context
            .func
            .signature
            .params
            .extend([types::I64, width, width].map(AbiParam::new));
        context.func.signature.returns.push(AbiParam::new(width));
        let mut state = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut context.func, &mut state);
        let entry = b.create_block();
        let header = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.append_block_params_for_function_params(entry);
        let count = b.append_block_param(header, types::I64);
        let left = b.append_block_param(header, width);
        let right = b.append_block_param(header, width);
        let magnitude = b.append_block_param(exit, width);
        let sign = b.append_block_param(exit, width);
        b.switch_to_block(entry);
        let [limit, first, second] = *b.block_params(entry) else {
            unreachable!()
        };
        b.ins().jump(
            header,
            &[
                BlockArg::Value(limit),
                BlockArg::Value(first),
                BlockArg::Value(second),
            ],
        );
        b.switch_to_block(header);
        b.ins().brif(
            count,
            body,
            &[],
            exit,
            &[BlockArg::Value(left), BlockArg::Value(right)],
        );
        b.switch_to_block(body);
        let next = b.ins().fsub(left, right);
        let remaining = b.ins().iadd_imm(count, -1);
        b.ins().jump(
            header,
            &[
                BlockArg::Value(remaining),
                BlockArg::Value(right),
                BlockArg::Value(next),
            ],
        );
        b.switch_to_block(exit);
        let signed = b.ins().fcopysign(magnitude, sign);
        b.ins().return_(&[signed]);
        b.seal_all_blocks();
        b.finalize();

        let (a, b) = compile(&mut module, &mut context);
        for (count, left, right) in [
            (0, f64::from_bits(0x7ff8_0000_5412_0000), -0.0),
            (0, -0.0, 1.25),
            (0, f64::NEG_INFINITY, 0.0),
            (1, 3.5, 4.0),
            (6, -9.0, 0.125),
        ] {
            let (reference, candidate) = unsafe {
                if width == types::F32 {
                    let a = std::mem::transmute::<*const u8, Single>(a);
                    let b = std::mem::transmute::<*const u8, Single>(b);
                    (
                        u64::from(a(count, left as f32, right as f32).to_bits()),
                        u64::from(b(count, left as f32, right as f32).to_bits()),
                    )
                } else {
                    let a = std::mem::transmute::<*const u8, Double>(a);
                    let b = std::mem::transmute::<*const u8, Double>(b);
                    (
                        a(count, left, right).to_bits(),
                        b(count, left, right).to_bits(),
                    )
                }
            };
            assert_eq!(candidate, reference, "{width}: {count}, {left}, {right}");
        }
    }
    unsafe { module.free_memory() };
}
