//! Differential tests for the opt-in native machine encoder.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/native_backend.rs"]
mod binding;
#[path = "native_backend/elf.rs"]
mod elf;
#[path = "native_backend/elf_host.rs"]
mod elf_host;

use cranelift_codegen::ir::{
    AbiParam, BlockArg, Endianness, InstBuilder, MemFlags, Opcode, StackSlotData, StackSlotKind,
    Type, condcodes::IntCC, types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use vesper::native::machine::integer_block_words;

const ABI_ARGS: usize = 6;

#[derive(Clone, Copy, Debug)]
enum Case {
    Constant(i64),
    Argument(usize),
    Binary(Binary),
    MultiplyHigh(Type, bool),
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
    NestedBranches,
    ShiftCountInRcx,
    ShiftDestinationRcx,
    ShiftSameRegister,
    ShiftDword,
    LoadWidths,
    WideningLoads,
    RetainedLoad,
    MemoryStores,
    GuardedMemory,
    JoinedMemory,
    StackFrame(u32),
    SpilledValues { fixed_slot: bool },
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
    // A store's slot cannot be returned as a value.
    assert!(binding::x86_64_integer_block(&[1, 1, 27, 0, 0, 0]).is_empty());
    // A conditional jump and early return must both be present.
    assert!(binding::x86_64_integer_block(&[1, 0, 28, 0, 0, 0]).is_empty());
    assert!(binding::x86_64_integer_block(&[1, 0, 29, 0, 0, 0]).is_empty());
    // The false arm cannot return a value defined in the true arm.
    assert!(
        binding::x86_64_integer_block(&[1, 2, 28, 0, 0, 0, 0, 9, 0, 0, 29, 2, 0, 1]).is_empty()
    );
    // Two nested branches must close in reverse order.
    let nested = [1, 0, 28, 0, 0, 0, 28, 0, 0, 0, 29, 0, 0, 2, 29, 0, 0, 1];
    assert!(!binding::x86_64_integer_block(&nested).is_empty());
    let crossing = [1, 0, 28, 0, 0, 0, 28, 0, 0, 0, 29, 0, 0, 1, 29, 0, 0, 2];
    assert!(binding::x86_64_integer_block(&crossing).is_empty());
    // Excessive alignment, a frame that needs probing, and an invalid slot offset.
    assert!(binding::x86_64_integer_block(&[1, 0, 30, 32, 32, 0]).is_empty());
    assert!(binding::x86_64_integer_block(&[1, 0, 30, 4096, 16, 0]).is_empty());
    assert!(binding::x86_64_integer_block(&[1, 2, 30, 16, 16, 0, 31, 0, 17, 0]).is_empty());
    assert!(binding::x86_64_integer_block(&[1, 0, 30, 16, 16, 0, 36, 0, 0, 9]).is_empty());
    assert_eq!(
        binding::x86_64_integer_block(&[1, 0, 30, 16, 16, 0]),
        binding::x86_64_integer_block(&[1, 0]),
    );

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
        Case::NestedBranches,
        Case::ShiftCountInRcx,
        Case::ShiftDestinationRcx,
        Case::ShiftSameRegister,
        Case::ShiftDword,
        Case::LoadWidths,
        Case::WideningLoads,
        Case::RetainedLoad,
        Case::MemoryStores,
        Case::GuardedMemory,
        Case::JoinedMemory,
        Case::StackFrame(16),
        Case::StackFrame(256),
        Case::SpilledValues { fixed_slot: false },
        Case::SpilledValues { fixed_slot: true },
    ];
    let cases: Vec<_> =
        cases
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
            .chain([types::I32, types::I64].into_iter().flat_map(|width| {
                [false, true].map(move |signed| Case::MultiplyHigh(width, signed))
            }))
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
            Case::MultiplyHigh(width, signed) => {
                let preserved = builder.ins().iadd(a, b);
                let (lhs, rhs) = if width == types::I64 {
                    (a, c)
                } else {
                    (
                        builder.ins().ireduce(width, a),
                        builder.ins().ireduce(width, c),
                    )
                };
                let high = if signed {
                    builder.ins().smulhi(lhs, rhs)
                } else {
                    builder.ins().umulhi(lhs, rhs)
                };
                let high = if width == types::I64 {
                    high
                } else {
                    builder.ins().uextend(types::I64, high)
                };
                let sum = builder.ins().iadd(high, preserved);
                builder.ins().bxor(sum, c)
            }
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
            Case::NestedBranches => {
                let yes = builder.create_block();
                let no = builder.create_block();
                let join = builder.create_block();
                let chosen = builder.append_block_param(join, types::I64);
                builder.ins().brif(a, yes, &[], no, &[]);
                for (arm, condition, left, right) in [(yes, c, a, b), (no, d, b, a)] {
                    builder.switch_to_block(arm);
                    let sum = builder.ins().iadd(left, right);
                    let difference = builder.ins().isub(left, right);
                    let inner_yes = builder.create_block();
                    let inner_no = builder.create_block();
                    builder.ins().brif(condition, inner_yes, &[], inner_no, &[]);
                    builder.switch_to_block(inner_yes);
                    builder.ins().jump(join, &[BlockArg::Value(sum)]);
                    builder.switch_to_block(inner_no);
                    builder.ins().jump(join, &[BlockArg::Value(difference)]);
                }
                builder.switch_to_block(join);
                builder.ins().iadd(chosen, f)
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
            Case::LoadWidths => {
                let flags = MemFlags::new().with_notrap();
                let byte = builder.ins().load(types::I8, flags, a, -137);
                let word = builder.ins().load(types::I16, flags, a, 0);
                let dword = builder.ins().load(types::I32, flags, e, 7);
                let qword = builder.ins().load(types::I64, flags, e, 152);
                let byte = builder.ins().uextend(types::I64, byte);
                let word = builder.ins().uextend(types::I64, word);
                let dword = builder.ins().uextend(types::I64, dword);
                let small = builder.ins().iadd(byte, word);
                let large = builder.ins().bxor(qword, dword);
                builder.ins().bxor(small, large)
            }
            Case::WideningLoads => {
                let flags = MemFlags::new().with_notrap();
                let byte = builder.ins().sload8(types::I64, flags, a, -128);
                let word = builder.ins().sload16(types::I32, flags, e, 127);
                let dword = builder.ins().sload32(flags, e, 128);
                let unsigned = builder.ins().uload16(types::I64, flags, a, -129);
                let word = builder.ins().sextend(types::I64, word);
                let small = builder.ins().bxor(byte, word);
                let large = builder.ins().iadd(dword, unsigned);
                builder.ins().bxor(small, large)
            }
            Case::RetainedLoad => {
                let flags = MemFlags::new().with_notrap();
                builder.ins().load(types::I64, flags, a, 8);
                builder.ins().load(types::I64, flags, a, 0)
            }
            Case::MemoryStores => {
                let flags = MemFlags::new().with_notrap();
                let old = builder.ins().load(types::I64, flags, a, -137);
                builder.ins().store(flags, b, a, -137);
                builder.ins().istore8(flags, c, a, -136);
                builder.ins().istore16(flags, d, e, 127);
                builder.ins().istore32(flags, f, e, 128);
                let byte = builder.ins().ireduce(types::I8, d);
                builder.ins().store(flags, byte, a, 0);
                let new = builder.ins().load(types::I64, flags, a, -137);
                let overlapping = builder.ins().load(types::I64, flags, e, 126);
                let diff = builder.ins().bxor(new, old);
                builder.ins().bxor(diff, overlapping)
            }
            Case::GuardedMemory => {
                let flags = MemFlags::new().with_notrap();
                let yes = builder.create_block();
                let no = builder.create_block();
                let shared = builder.ins().iadd(d, f);
                let predicate = builder.ins().ireduce(types::I8, b);
                builder.ins().brif(predicate, yes, &[], no, &[]);
                builder.switch_to_block(yes);
                let old = builder.ins().load(types::I64, flags, a, -13);
                let updated = builder.ins().bxor(old, shared);
                builder.ins().store(flags, updated, a, -13);
                builder.ins().return_(&[updated]);
                builder.switch_to_block(no);
                let original = builder.ins().bxor(d, f);
                builder.ins().isub(shared, original)
            }
            Case::JoinedMemory => {
                let flags = MemFlags::new().with_notrap();
                let yes = builder.create_block();
                let no = builder.create_block();
                let join = builder.create_block();
                let left = builder.append_block_param(join, types::I64);
                let right = builder.append_block_param(join, types::I64);
                let byte = builder.append_block_param(join, types::I8);
                let predicate = builder.ins().ireduce(types::I8, b);
                builder.ins().brif(predicate, yes, &[], no, &[]);
                builder.switch_to_block(yes);
                let loaded = builder.ins().load(types::I64, flags, a, -13);
                builder
                    .ins()
                    .jump(join, &[loaded.into(), c.into(), predicate.into()]);
                builder.switch_to_block(no);
                let small = builder.ins().ireduce(types::I8, d);
                builder
                    .ins()
                    .jump(join, &[f.into(), d.into(), small.into()]);
                builder.switch_to_block(join);
                let old = builder.ins().load(types::I64, flags, e, 0);
                let extension = builder.ins().sextend(types::I64, byte);
                let delta = builder.ins().isub(left, right);
                let delta = builder.ins().iadd(delta, extension);
                let written = builder.ins().iadd(old, delta);
                builder.ins().store(flags, written, e, 0);
                written
            }
            Case::StackFrame(size) => {
                let padding = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    3,
                    0,
                ));
                let payload = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    size,
                    4,
                ));
                let offset = i32::try_from(size - 8).unwrap();
                let byte = builder.ins().ireduce(types::I8, d);
                let word = builder.ins().ireduce(types::I16, a);
                let dword = builder.ins().ireduce(types::I32, e);
                builder.ins().stack_store(byte, padding, 1);
                builder.ins().stack_store(word, payload, 0);
                builder.ins().stack_store(dword, payload, 2);
                builder.ins().stack_store(c, payload, offset);
                let base = builder.ins().stack_addr(types::I64, payload, 0);
                let misalignment = builder.ins().band_imm(base, 15);
                let yes = builder.create_block();
                let no = builder.create_block();
                builder.ins().brif(b, yes, &[], no, &[]);
                builder.switch_to_block(yes);
                let wide = builder.ins().stack_load(types::I64, payload, offset);
                let narrow = builder.ins().stack_load(types::I8, padding, 1);
                let narrow = builder.ins().uextend(types::I64, narrow);
                let folded = builder.ins().bxor(wide, narrow);
                let word = builder.ins().stack_load(types::I16, payload, 0);
                let dword = builder.ins().stack_load(types::I32, payload, 2);
                let word = builder.ins().uextend(types::I64, word);
                let dword = builder.ins().uextend(types::I64, dword);
                let more = builder.ins().iadd(word, dword);
                let result = builder.ins().iadd(folded, more);
                let result = builder.ins().iadd(result, misalignment);
                builder.ins().return_(&[result]);
                builder.switch_to_block(no);
                let wide = builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), base, offset);
                builder.ins().isub(wide, misalignment)
            }
            Case::SpilledValues { fixed_slot } => {
                let predicate = builder.ins().iadd(a, b);
                let yes = builder.ins().imul(c, d);
                let no = builder.ins().bxor(e, f);
                let count = builder.ins().isub(d, f);
                let selected = builder.ins().select(predicate, yes, no);
                let shifted = builder.ins().ishl(selected, count);
                let slot = fixed_slot.then(|| {
                    let slot = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        16,
                        4,
                    ));
                    builder.ins().stack_store(shifted, slot, 8);
                    slot
                });
                let taken = builder.create_block();
                let skipped = builder.create_block();
                builder.ins().brif(predicate, taken, &[], skipped, &[]);
                builder.switch_to_block(taken);
                let taken_result = if let Some(slot) = slot {
                    builder.ins().stack_load(types::I64, slot, 8)
                } else {
                    shifted
                };
                builder.ins().return_(&[taken_result]);
                builder.switch_to_block(skipped);
                let left = builder.ins().isub(a, b);
                let middle = builder.ins().band(c, d);
                let right = builder.ins().bor(e, f);
                let pair = builder.ins().iadd(left, middle);
                let all = builder.ins().iadd(pair, right);
                builder.ins().isub(all, shifted)
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
        if matches!(case, Case::SpilledValues { fixed_slot: false }) {
            // This function has no explicit slots. The prologue reserves spills.
            assert!(bytes.starts_with(&[0x48, 0x83, 0xec]));
        }
        if matches!(case, Case::RetainedLoad) {
            // The first read (mov rax, [rdi + 8]) is emitted even though the
            // function returns only the second read.
            assert!(bytes.starts_with(&[0x48, 0x8b, 0x47, 0x08]));
        }
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
        let memory: [u8; 512] = std::array::from_fn(|i| i.wrapping_mul(97) as u8);
        if matches!(case, Case::GuardedMemory | Case::JoinedMemory) {
            for condition in [0, 0x100, 0x101, u64::MAX] {
                let mut reference_memory = memory;
                let mut generated_memory = memory;
                let executes_memory = condition as u8 != 0;
                let run = |function: Fn6, memory: &mut [u8; 512]| {
                    let base = memory.as_mut_ptr();
                    // The first address is read only when the branch is
                    // taken. The join uses a separate region of this buffer.
                    unsafe {
                        let address = if executes_memory {
                            base.add(256) as u64
                        } else {
                            0
                        };
                        function(address, condition, 3, 5, base.add(320) as u64, 11)
                    }
                };
                assert_eq!(
                    run(candidate, &mut generated_memory),
                    run(baseline, &mut reference_memory),
                );
                assert_eq!(generated_memory, reference_memory);
                assert_eq!(
                    generated_memory != memory,
                    executes_memory || matches!(case, Case::JoinedMemory),
                );
            }
            continue;
        }
        if matches!(case, Case::MemoryStores) {
            for (first, second) in [(256, 257), (257, 256)] {
                let mut reference_memory = memory;
                let mut generated_memory = memory;
                let run = |function: Fn6, memory: &mut [u8; 512]| {
                    let base = memory.as_mut_ptr();
                    // The two pointers share this live buffer. Every offset
                    // in MemoryStores lands within its allocated bytes.
                    unsafe {
                        function(
                            base.add(first) as u64,
                            0x1234_5678_90ab_cdef,
                            0xdcba,
                            0x7890,
                            base.add(second) as u64,
                            0x7654_3210,
                        )
                    }
                };
                assert_eq!(
                    run(candidate, &mut generated_memory),
                    run(baseline, &mut reference_memory),
                );
                assert_eq!(generated_memory, reference_memory);
            }
            continue;
        }
        let scalar_inputs = [
            [2, 3, 5, 7, 13, 17],
            [0, 0, 0, 0, 0, 0],
            [0, 3, 5, 7, 13, 17],
            [u64::MAX, 1, 0, 0, u64::MAX, 2],
            [1, 2, 3, 4, 1 << 63, u64::MAX],
            [0x80, 0x7f, 0, 0, 0x80, 0xff],
            [0x100, u64::MAX - 1, 0, 0, 0x100, 0x80],
            [0x8000, 0x7fff, 0, 0, 0, 0],
            [0x8000_0000, 0x7fff_ffff, 0, 65, 33, 0],
            [u64::MAX, 0, 0, 32, 65, 0],
        ];
        let middle = memory[256..].as_ptr() as u64;
        let odd = memory[257..].as_ptr() as u64;
        let memory_inputs = [[middle, 0, 0, 0, odd, 0], [odd, 0, 0, 0, middle, 0]];
        let inputs: &[[u64; ABI_ARGS]] = if matches!(
            case,
            Case::LoadWidths | Case::WideningLoads | Case::RetainedLoad
        ) {
            &memory_inputs
        } else {
            &scalar_inputs
        };
        for &[a, b, c, d, e, f] in inputs {
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
fn fixed_arithmetic_preserves_live_rax_and_rdx() {
    const SIGNED_DIV: u64 = 46;
    const UNSIGNED_DIV: u64 = 47;
    const SIGNED_REM: u64 = 48;
    const UNSIGNED_REM: u64 = 49;
    const SIGNED_MUL_HIGH: u64 = 50;
    const UNSIGNED_MUL_HIGH: u64 = 51;
    const WIDE_BITS: u64 = u64::BITS as u64;

    type Entry = unsafe extern "C" fn(u64, u64, u64) -> u64;
    let mut module =
        JITModule::new(JITBuilder::new(cranelift_module::default_libcall_names()).unwrap());
    let alignment = u64::from(module.isa().function_alignment().minimum);
    let mut signature = module.make_signature();
    signature.params = vec![AbiParam::new(types::I64); 3];
    signature.returns.push(AbiParam::new(types::I64));

    for width in [u32::BITS as u64, WIDE_BITS] {
        let mask = u64::MAX >> (WIDE_BITS - width);
        let minimum = 1u64 << (width - 1);
        let signed = |value: u64| {
            (value.wrapping_shl((WIDE_BITS - width) as u32) as i64 >> (WIDE_BITS - width)) as i128
        };
        for op in SIGNED_DIV..=UNSIGNED_MUL_HIGH {
            let id = module
                .declare_function(&format!("fixed_{op}_{width}"), Linkage::Local, &signature)
                .unwrap();
            // Hold a constant in RAX and the divisor in RDX across the operation.
            #[rustfmt::skip]
            let words = [
                3, 6,
                0, 17, 0, 0,
                op, 0, 2, width,
                1, 3, 4, WIDE_BITS,
                6, 5, 2, WIDE_BITS,
            ];
            let bytes = binding::x86_64_integer_block(&words);
            assert!(!bytes.is_empty());
            module
                .define_function_bytes(id, alignment, &bytes, &[])
                .unwrap();
            module.finalize_definitions().unwrap();
            let function = unsafe {
                std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(id))
            };
            for (a, b) in [(0, 1), (19, 5), (19, mask - 4), (minimum, mask), (mask, 11)] {
                if op == SIGNED_DIV && a == minimum && b == mask {
                    continue;
                }
                let expected = match op {
                    SIGNED_DIV => (signed(a) / signed(b)) as u64,
                    UNSIGNED_DIV => a / b,
                    SIGNED_REM => (signed(a) % signed(b)) as u64,
                    UNSIGNED_REM => a % b,
                    SIGNED_MUL_HIGH => ((signed(a) * signed(b)) >> width) as u64,
                    UNSIGNED_MUL_HIGH => ((u128::from(a) * u128::from(b)) >> width) as u64,
                    _ => unreachable!(),
                };
                assert_eq!(
                    unsafe { function(a, 0, b) },
                    (expected & mask).wrapping_add(17) ^ b,
                    "{op} {width} {a} {b}"
                );
            }
        }
    }
}

#[test]
fn integer_backend_requires_traps_to_end_their_path() {
    const NO_VALUE: u64 = u64::MAX;
    const TRAP_VALUE: u64 = NO_VALUE - 1;
    const UD2: [u8; 2] = [0x0f, 0x0b];

    assert_eq!(
        binding::x86_64_integer_block(&[0, NO_VALUE, 45, 1, 0, 0]),
        UD2
    );
    let trapping_true_arm = [1, 0, 28, 0, 0, 0, 29, TRAP_VALUE, 0, 1];
    let code = binding::x86_64_integer_block(&trapping_true_arm);
    assert!(code.windows(UD2.len()).any(|bytes| bytes == UD2));
    assert_eq!(code.last(), Some(&0xc3));

    let returning_pair = [2, NO_VALUE, 28, 0, 0, 0, 29, 0, 1, 2, 45, 2, 0, 0];
    assert!(binding::x86_64_integer_block(&returning_pair).ends_with(&UD2));
    assert!(binding::x86_64_integer_block(&[0, NO_VALUE, 45, 3, 0, 0]).is_empty());
    assert!(binding::x86_64_integer_block(&[0, 1, 45, 1, 0, 0, 0, 42, 0, 0]).is_empty());
}

#[test]
fn integer_backend_matches_paired_system_v_returns() {
    // Reject a misplaced pair record, a self-reference, and a false-path
    // result defined on the true path.
    assert!(binding::x86_64_integer_block(&[1, 0, 37, 0, 0, 0, 0, 8, 0, 0]).is_empty());
    assert!(binding::x86_64_integer_block(&[1, 0, 37, 1, 0, 0]).is_empty());
    assert!(
        binding::x86_64_integer_block(&[1, 0, 28, 0, 0, 0, 0, 8, 0, 0, 29, 0, 2, 1, 37, 2, 0, 0])
            .is_empty()
    );

    #[derive(Clone, Copy, Debug)]
    enum Case {
        Exchange,
        MoveOrder,
        Duplicate,
        Branch,
        Spill,
    }

    #[derive(Debug, PartialEq, Eq)]
    #[repr(C)]
    struct Pair(u64, u64);
    type Fn6 = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64) -> Pair;

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
    signature.params = vec![AbiParam::new(types::I64); ABI_ARGS];
    signature.returns = vec![AbiParam::new(types::I64); 2];
    for (index, case) in [
        Case::Exchange,
        Case::MoveOrder,
        Case::Duplicate,
        Case::Branch,
        Case::Spill,
    ]
    .into_iter()
    .enumerate()
    {
        let mut context = module.make_context();
        context.func.signature = signature.clone();
        let mut builder_context = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let [a, b, c, _, e, f] = *builder.block_params(entry) else {
            unreachable!();
        };
        let pair = match case {
            Case::Exchange => [c, builder.ins().iconst(types::I64, -17)],
            Case::MoveOrder => [a, builder.ins().iconst(types::I64, 41)],
            Case::Duplicate => [c, c],
            Case::Branch => {
                let yes = builder.create_block();
                let no = builder.create_block();
                let high = builder.ins().iadd(a, b);
                builder.ins().brif(a, yes, &[], no, &[]);
                builder.switch_to_block(yes);
                builder.ins().return_(&[c, high]);
                builder.switch_to_block(no);
                [f, e]
            }
            Case::Spill => {
                let mut terms: Vec<_> = (1..=12).map(|n| builder.ins().iadd_imm(a, n)).collect();
                let high = terms.pop().unwrap();
                let low = terms
                    .into_iter()
                    .fold(b, |sum, term| builder.ins().iadd(sum, term));
                [low, high]
            }
        };
        builder.ins().return_(&pair);
        builder.seal_all_blocks();
        builder.finalize();

        let request = integer_block_words(&context.func).unwrap();
        let bytes = binding::x86_64_integer_block(&request);
        assert!(!bytes.is_empty(), "{case:?}");
        if matches!(case, Case::Exchange) {
            assert!(bytes.ends_with(&[0x48, 0x87, 0xd0, 0xc3]));
        }
        let baseline = module
            .declare_function(&format!("base_{index}"), Linkage::Local, &signature)
            .unwrap();
        let candidate = module
            .declare_function(&format!("port_{index}"), Linkage::Local, &signature)
            .unwrap();
        module.define_function(baseline, &mut context).unwrap();
        let alignment = u64::from(module.isa().function_alignment().minimum);
        module
            .define_function_bytes(candidate, alignment, &bytes, &[])
            .unwrap();
        module.finalize_definitions().unwrap();
        // On System V the C pair of u64 fields uses the two registers of the
        // CLIF signature. The module owns both pointers for these calls.
        let baseline: Fn6 = unsafe { std::mem::transmute(module.get_finalized_function(baseline)) };
        let candidate: Fn6 =
            unsafe { std::mem::transmute(module.get_finalized_function(candidate)) };
        for [a, b, c, d, e, f] in [
            [0, 2, 3, 5, 7, 11],
            [13, 17, 19, 23, 29, 31],
            [u64::MAX; ABI_ARGS],
        ] {
            assert_eq!(
                unsafe { candidate(a, b, c, d, e, f) },
                unsafe { baseline(a, b, c, d, e, f) },
                "{case:?}"
            );
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn integer_backend_declines_excessive_spill_frames_and_unsupported_operations() {
    let mut context = cranelift_codegen::Context::new();
    context.func.signature.call_conv = cranelift_codegen::isa::CallConv::SystemV;
    context
        .func
        .signature
        .returns
        .push(AbiParam::new(types::I64));
    let mut builder_context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
    let slot =
        builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 4088, 0));
    let entry = builder.create_block();
    builder.switch_to_block(entry);
    let constants: Vec<_> = (1..=10)
        .map(|n| builder.ins().iconst(types::I64, n))
        .collect();
    builder.ins().stack_store(constants[0], slot, 0);
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
fn integer_backend_declines_cycles_and_potential_traps_in_branch_arms() {
    let mut context = cranelift_codegen::Context::new();
    let mut builder_context = FunctionBuilderContext::new();
    for branch_op in [Opcode::Jump, Opcode::Sdiv, Opcode::Load, Opcode::Store] {
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
        match branch_op {
            Opcode::Jump => {
                builder.ins().jump(yes, &[]);
            }
            Opcode::Sdiv => {
                let division = builder.ins().sdiv(argument, argument);
                builder.ins().return_(&[division]);
            }
            Opcode::Load => {
                let load = builder.ins().load(types::I64, MemFlags::new(), argument, 0);
                builder.ins().return_(&[load]);
            }
            Opcode::Store => {
                builder.ins().store(MemFlags::new(), argument, argument, 0);
                builder.ins().return_(&[argument]);
            }
            _ => unreachable!(),
        }

        builder.switch_to_block(no);
        builder.ins().return_(&[argument]);
        builder.seal_all_blocks();
        builder.finalize();
        assert!(integer_block_words(&context.func).is_none());
    }
}

#[test]
fn integer_backend_declines_trapping_big_endian_and_checked_memory_operations() {
    let mut context = cranelift_codegen::Context::new();
    let mut builder_context = FunctionBuilderContext::new();
    for (flags, opcode) in [
        MemFlags::new(),
        MemFlags::trusted().with_endianness(Endianness::Big),
        MemFlags::trusted().with_checked(),
    ]
    .into_iter()
    .flat_map(|flags| [(flags, Opcode::Load), (flags, Opcode::Store)])
    {
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
        let base = builder.block_params(entry)[0];
        let result = if opcode == Opcode::Load {
            builder.ins().load(types::I64, flags, base, 0)
        } else {
            builder.ins().store(flags, base, base, 0);
            base
        };
        builder.ins().return_(&[result]);
        builder.seal_all_blocks();
        builder.finalize();
        assert!(integer_block_words(&context.func).is_none());
    }
}
