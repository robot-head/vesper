use super::binding;
use super::machine::integer_block_words;
use cranelift_codegen::ir::{AbiParam, BlockArg, InstBuilder, Type, condcodes::IntCC, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

const ABI_ARGS: usize = 6;

#[derive(Clone, Copy, Debug)]
enum Case {
    Constant(i64),
    Argument(usize),
    Binary(Binary),
    Chain,
    RepeatedOperands,
    DeadValues,
    HighRegisters,
    Compare64(IntCC),
    CompareNarrow(Type, IntCC),
    NarrowArithmetic(Type),
    NarrowShifts(Type),
    ChainedCasts,
    JumpChain,
    ReturningBranch,
    ShiftCountInRcx,
    ShiftDestinationRcx,
    ShiftSameRegister,
    ShiftDword,
    CompareImmediate,
    Select,
    SelectKeepingArms,
    SelectKeepingTrue,
    ByteExtensions,
}

#[derive(Clone, Copy, Debug)]
enum Binary {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
}

#[test]
fn integer_backend_matches_cranelift_results() {
    // Reject a truncated header and a return value outside the value space.
    assert!(binding::x86_64_integer_block(&[1]).is_empty());
    assert!(binding::x86_64_integer_block(&[1, 1]).is_empty());
    // The first instruction cannot reference its own result.
    assert!(binding::x86_64_integer_block(&[1, 1, 1, 1, 0, 0]).is_empty());

    let isa = cranelift_native::builder()
        .unwrap()
        .finish(cranelift_codegen::settings::Flags::new(
            cranelift_codegen::settings::builder(),
        ))
        .unwrap();
    let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    let mut module = JITModule::new(builder);
    let mut signature = module.make_signature();
    signature.params = vec![AbiParam::new(types::I64); ABI_ARGS];
    signature.returns.push(AbiParam::new(types::I64));

    let cases = [
        Case::Constant(0),
        Case::Constant(i64::from(u32::MAX)),
        Case::Constant(-1),
        Case::Constant(i64::MIN),
        Case::Argument(0),
        Case::Argument(1),
        Case::Argument(2),
        Case::Argument(3),
        Case::Argument(4),
        Case::Argument(5),
        Case::Binary(Binary::Add),
        Case::Binary(Binary::Sub),
        Case::Binary(Binary::Mul),
        Case::Binary(Binary::And),
        Case::Binary(Binary::Or),
        Case::Binary(Binary::Xor),
        Case::Chain,
        Case::RepeatedOperands,
        Case::DeadValues,
        Case::HighRegisters,
        Case::CompareImmediate,
        Case::Select,
        Case::SelectKeepingArms,
        Case::SelectKeepingTrue,
        Case::ByteExtensions,
        Case::ChainedCasts,
        Case::JumpChain,
        Case::ReturningBranch,
        Case::ShiftCountInRcx,
        Case::ShiftDestinationRcx,
        Case::ShiftSameRegister,
        Case::ShiftDword,
    ];
    let cases: Vec<_> = cases
        .into_iter()
        .chain(IntCC::all().iter().copied().map(Case::Compare64))
        .chain(
            [types::I8, types::I16, types::I32]
                .into_iter()
                .flat_map(|width| {
                    IntCC::all()
                        .iter()
                        .copied()
                        .map(move |cc| Case::CompareNarrow(width, cc))
                }),
        )
        .chain([types::I8, types::I16, types::I32].map(Case::NarrowArithmetic))
        .chain([types::I8, types::I16, types::I32].map(Case::NarrowShifts))
        .collect();
    let case_count = cases.len();
    let mut byte_differences = 0;
    for (index, case) in cases.into_iter().enumerate() {
        let baseline = module
            .declare_function(&format!("baseline_{index}"), Linkage::Local, &signature)
            .unwrap();
        let candidate = module
            .declare_function(&format!("candidate_{index}"), Linkage::Local, &signature)
            .unwrap();
        let mut context = module.make_context();
        context.func.signature = signature.clone();

        let mut builder_context = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let [a, b, c, d, e, f] = *builder.block_params(entry) else {
            unreachable!();
        };
        let result = match case {
            Case::Constant(bits) => builder.ins().iconst(types::I64, bits),
            Case::Argument(index) => builder.block_params(entry)[index],
            Case::Binary(op) => match op {
                Binary::Add => builder.ins().iadd(e, f),
                Binary::Sub => builder.ins().isub(e, f),
                Binary::Mul => builder.ins().imul(e, f),
                Binary::And => builder.ins().band(e, f),
                Binary::Or => builder.ins().bor(e, f),
                Binary::Xor => builder.ins().bxor(e, f),
            },
            Case::Chain => {
                let left = builder.ins().iadd_imm(a, i64::MIN);
                let neg = builder.ins().ineg(b);
                let high = builder.ins().sshr_imm(left, 33);
                let low = builder.ins().ushr_imm(neg, 5);
                let folded = builder.ins().bxor(high, low);
                let shifted = builder.ins().ishl_imm(folded, 65);
                builder.ins().bnot(shifted)
            }
            Case::RepeatedOperands => {
                let product = builder.ins().imul(a, a);
                let difference = builder.ins().isub(product, a);
                let sum = builder.ins().iadd(difference, product);
                builder.ins().iadd(sum, product)
            }
            Case::DeadValues => {
                let dead = builder.ins().iadd(e, f);
                builder.ins().imul(dead, dead);
                builder.ins().isub(a, a)
            }
            Case::HighRegisters => {
                // Keep all incoming registers live across three temporaries.
                let first = builder.ins().iadd(a, b);
                let second = builder.ins().imul(c, d);
                let third = builder.ins().bxor(e, f);
                let left = builder.ins().isub(a, b);
                let middle = builder.ins().band(c, d);
                let right = builder.ins().bor(e, f);
                let high = builder.ins().imul(third, second);
                let low = builder.ins().isub(first, left);
                let parts = builder.ins().iadd(high, low);
                let tail = builder.ins().bxor(middle, right);
                builder.ins().isub(parts, tail)
            }
            Case::Compare64(cc) => {
                let predicate = builder.ins().icmp(cc, e, f);
                builder.ins().uextend(types::I64, predicate)
            }
            Case::CompareNarrow(width, cc) => {
                let left = builder.ins().ireduce(width, a);
                let right = builder.ins().ireduce(width, b);
                let predicate = builder.ins().icmp(cc, left, right);
                builder.ins().uextend(types::I64, predicate)
            }
            Case::NarrowArithmetic(width) => {
                let left = builder.ins().ireduce(width, a);
                let right = builder.ins().ireduce(width, b);
                let salt = builder.ins().iconst(width, -13);
                let sum = builder.ins().iadd(left, right);
                let odd = builder.ins().bor_imm(sum, 1);
                let product = builder.ins().imul(odd, salt);
                let diff = builder.ins().isub(product, left);
                let complement = builder.ins().bnot(diff);
                let neg = builder.ins().ineg(complement);
                let bits = builder.ins().bxor(neg, right);
                let low = builder.ins().band_imm(bits, -2);
                builder.ins().uextend(types::I64, low)
            }
            Case::NarrowShifts(width) => {
                let input = builder.ins().ireduce(width, a);
                let left = builder.ins().ishl_imm(input, i64::from(width.bits()) + 1);
                let unsigned = builder.ins().ushr_imm(input, 1);
                let signed = builder.ins().sshr_imm(input, i64::from(width.bits()) + 1);
                let low = builder.ins().uextend(types::I64, left);
                let high = builder.ins().uextend(types::I64, unsigned);
                let separate = builder.ins().ishl_imm(high, 32);
                let bits = builder.ins().bor(low, separate);
                let signed = builder.ins().sextend(types::I64, signed);
                builder.ins().bxor(bits, signed)
            }
            Case::ChainedCasts => {
                let byte = builder.ins().ireduce(types::I8, a);
                let signed_word = builder.ins().sextend(types::I16, byte);
                let signed_dword = builder.ins().sextend(types::I32, signed_word);
                let signed = builder.ins().sextend(types::I64, signed_dword);
                let unsigned = builder.ins().uextend(types::I64, signed_word);
                builder.ins().isub(signed, unsigned)
            }
            Case::JumpChain => {
                let first = builder.create_block();
                let second = builder.create_block();
                let low = builder.append_block_param(first, types::I64);
                let high = builder.append_block_param(first, types::I64);
                let result = builder.append_block_param(second, types::I64);
                builder
                    .ins()
                    .jump(first, &[BlockArg::Value(b), BlockArg::Value(a)]);
                builder.switch_to_block(first);
                let difference = builder.ins().isub(high, low);
                builder.ins().jump(second, &[BlockArg::Value(difference)]);
                builder.switch_to_block(second);
                builder.ins().imul(result, c)
            }
            Case::ReturningBranch => {
                let yes = builder.create_block();
                let no = builder.create_block();
                let yes_operand = builder.append_block_param(yes, types::I64);
                let no_operand = builder.append_block_param(no, types::I64);
                let predicate = builder.ins().icmp(IntCC::SignedLessThan, a, b);
                builder.ins().brif(
                    predicate,
                    yes,
                    &[BlockArg::Value(e)],
                    no,
                    &[BlockArg::Value(f)],
                );
                builder.switch_to_block(yes);
                let result = builder.ins().iadd(yes_operand, c);
                builder.ins().return_(&[result]);
                builder.switch_to_block(no);
                builder.ins().isub(no_operand, d)
            }
            Case::ShiftCountInRcx => {
                let shifted = builder.ins().ushr(a, d);
                builder.ins().bxor(shifted, d)
            }
            Case::ShiftDestinationRcx => {
                let shifted = builder.ins().ishl(d, e);
                builder.ins().iadd(shifted, e)
            }
            Case::ShiftSameRegister => {
                let shifted = builder.ins().sshr(e, e);
                builder.ins().bxor(shifted, d)
            }
            Case::ShiftDword => {
                let word = builder.ins().ireduce(types::I32, a);
                let count = builder.ins().ireduce(types::I32, e);
                let shifted = builder.ins().sshr(word, count);
                let full = builder.ins().uextend(types::I64, shifted);
                builder.ins().iadd(full, d)
            }
            Case::CompareImmediate => {
                let byte = builder.ins().ireduce(types::I8, a);
                let zero = builder.ins().icmp_imm(IntCC::Equal, byte, 0);
                let low = builder.ins().icmp_imm(IntCC::SignedLessThan, b, -1);
                let predicate = builder.ins().bor(zero, low);
                builder.ins().uextend(types::I64, predicate)
            }
            Case::Select => builder.ins().select(a, e, f),
            Case::SelectKeepingArms => {
                let predicate = builder.ins().icmp(IntCC::UnsignedGreaterThan, a, b);
                let selected = builder.ins().select(predicate, e, f);
                let sum = builder.ins().iadd(e, f);
                builder.ins().isub(sum, selected)
            }
            Case::SelectKeepingTrue => {
                let selected = builder.ins().select(a, e, f);
                builder.ins().iadd(selected, e)
            }
            Case::ByteExtensions => {
                let left = builder.ins().ireduce(types::I8, e);
                let right = builder.ins().ireduce(types::I8, f);
                let selected = builder.ins().select(a, left, right);
                let sign_extended = builder.ins().sextend(types::I64, selected);
                let zero_extended = builder.ins().uextend(types::I64, selected);
                builder.ins().isub(sign_extended, zero_extended)
            }
        };
        builder.ins().return_(&[result]);
        builder.seal_all_blocks();
        builder.finalize();

        let request =
            integer_block_words(&context.func).unwrap_or_else(|| panic!("declined {case:?}"));
        let bytes = binding::x86_64_integer_block(&request);
        assert!(!bytes.is_empty(), "{case:?}");
        module.define_function(baseline, &mut context).unwrap();
        let oracle = context.compiled_code().unwrap().code_buffer();
        byte_differences += usize::from(oracle != bytes);
        let alignment = u64::from(module.isa().function_alignment().minimum);
        module
            .define_function_bytes(candidate, alignment, &bytes, &[])
            .unwrap();
        module.finalize_definitions().unwrap();

        type Fn6 = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64;
        // Both functions have six normal i64 arguments and one i64 result
        // under System V, and the module keeps their finalized code alive.
        let baseline: Fn6 = unsafe { std::mem::transmute(module.get_finalized_function(baseline)) };
        let candidate: Fn6 =
            unsafe { std::mem::transmute(module.get_finalized_function(candidate)) };
        for [a, b, c, d, e, f] in [
            [2, 3, 5, 7, 13, 17],
            [0, 0, 0, 0, 0, 0],
            [u64::MAX, 1, 0, 0, u64::MAX, 2],
            [1, 2, 3, 4, 1 << 63, u64::MAX],
            [0x80, 0x7f, 0, 0, 0x80, 0xff],
            [0x100, u64::MAX - 1, 0, 0, 0x100, 0x80],
            [0x8000, 0x7fff, 0, 0, 0, 0],
            [0x8000_0000, 0x7fff_ffff, 0, 65, 33, 0],
            [u64::MAX, 0, 0, 32, 65, 0],
        ] {
            assert_eq!(
                unsafe { candidate(a, b, c, d, e, f) },
                unsafe { baseline(a, b, c, d, e, f) },
                "{case:?}"
            );
        }
    }
    eprintln!(
        "{} of {} selected integer blocks differ in bytes from Cranelift",
        byte_differences, case_count
    );
    // All code pointers have gone out of scope.
    unsafe { module.free_memory() };
}

#[test]
fn integer_backend_declines_register_pressure_and_unsupported_operations() {
    let mut context = cranelift_codegen::Context::new();
    context.func.signature.call_conv = cranelift_codegen::isa::CallConv::SystemV;
    context
        .func
        .signature
        .returns
        .push(AbiParam::new(types::I64));
    let mut builder_context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
    let entry = builder.create_block();
    builder.switch_to_block(entry);
    let constants: Vec<_> = (1..=10)
        .map(|n| builder.ins().iconst(types::I64, n))
        .collect();
    let sum = constants
        .into_iter()
        .reduce(|a, b| builder.ins().iadd(a, b))
        .unwrap();
    builder.ins().return_(&[sum]);
    builder.seal_all_blocks();
    builder.finalize();
    let request = integer_block_words(&context.func).unwrap();
    assert!(binding::x86_64_integer_block(&request).is_empty());

    context.clear();
    context.func.signature.call_conv = cranelift_codegen::isa::CallConv::SystemV;
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
    let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let argument = builder.block_params(entry)[0];
    let result = builder.ins().sdiv(argument, argument);
    builder.ins().return_(&[result]);
    builder.seal_all_blocks();
    builder.finalize();
    assert!(integer_block_words(&context.func).is_none());
}

#[test]
fn integer_backend_declines_cycles_and_effectful_branch_arms() {
    let mut context = cranelift_codegen::Context::new();
    let mut builder_context = FunctionBuilderContext::new();
    for cyclic in [false, true] {
        context.clear();
        context.func.signature.call_conv = cranelift_codegen::isa::CallConv::SystemV;
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
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        let yes = builder.create_block();
        let no = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let argument = builder.block_params(entry)[0];
        builder.ins().brif(argument, yes, &[], no, &[]);

        builder.switch_to_block(yes);
        if cyclic {
            builder.ins().jump(yes, &[]);
        } else {
            let division = builder.ins().sdiv(argument, argument);
            builder.ins().return_(&[division]);
        }

        builder.switch_to_block(no);
        builder.ins().return_(&[argument]);
        builder.seal_all_blocks();
        builder.finalize();
        assert!(integer_block_words(&context.func).is_none());
    }
}
