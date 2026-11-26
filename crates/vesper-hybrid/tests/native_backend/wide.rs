use super::{binding, integer_block_words};
use cranelift_codegen::{
    ir::{AbiParam, BlockArg, Function, InstBuilder, condcodes::IntCC, types},
    isa::CallConv,
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

#[derive(Clone, Copy, Debug)]
enum Case {
    Add,
    Subtract,
    HighHalf,
    ByteSwap,
    SignedExtend,
    UnsignedExtend,
    Compare(IntCC),
    Loop,
}

#[test]
fn a_wide_argument_requiring_stack_alignment_uses_the_fallback() {
    const SYSTEM_V_INTEGER_REGISTERS: usize = 6;
    let mut function = Function::new();
    function.signature.call_conv = CallConv::SystemV;
    function.signature.params = vec![AbiParam::new(types::I64); SYSTEM_V_INTEGER_REGISTERS - 1];
    function.signature.params.push(AbiParam::new(types::I128));
    let mut context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut function, &mut context);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.ins().return_(&[]);
    builder.seal_all_blocks();
    builder.finalize();
    assert!(integer_block_words(&function).is_none());
}

#[test]
fn wide_values_match_cranelift_across_registers_and_block_edges() {
    let mut flags = settings::builder();
    flags.set("enable_llvm_abi_extensions", "true").unwrap();
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(flags))
        .unwrap();
    let mut module = JITModule::new(JITBuilder::with_isa(
        isa,
        cranelift_module::default_libcall_names(),
    ));
    let mut signature = module.make_signature();
    let scalar = AbiParam::new(types::I64);
    let wide = AbiParam::new(types::I128);
    signature.params.extend([scalar, wide, wide, scalar]);
    signature.returns.push(wide);
    let cases = [
        Case::Add,
        Case::Subtract,
        Case::HighHalf,
        Case::ByteSwap,
        Case::SignedExtend,
        Case::UnsignedExtend,
        Case::Loop,
    ];
    for (index, case) in cases
        .into_iter()
        .chain(IntCC::all().iter().copied().map(Case::Compare))
        .enumerate()
    {
        let mut context = module.make_context();
        let mut builder_context = FunctionBuilderContext::new();
        context.func.signature = signature.clone();
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let [repetitions, left, right, tail] = *builder.block_params(entry) else {
            unreachable!();
        };
        let result = match case {
            Case::Add => builder.ins().iadd(left, right),
            Case::Subtract => builder.ins().isub(left, right),
            Case::HighHalf => {
                let sum = builder.ins().iadd(left, right);
                let (_, high) = builder.ins().isplit(sum);
                builder.ins().iconcat(tail, high)
            }
            Case::ByteSwap => builder.ins().bswap(left),
            Case::SignedExtend | Case::UnsignedExtend => {
                let small = builder.ins().ireduce(types::I16, left);
                if matches!(case, Case::SignedExtend) {
                    builder.ins().sextend(types::I128, small)
                } else {
                    builder.ins().uextend(types::I128, small)
                }
            }
            Case::Compare(cc) => {
                let pick_left = builder.ins().icmp(cc, left, right);
                let inverted = builder.ins().bnot(right);
                builder.ins().select(pick_left, left, inverted)
            }
            Case::Loop => {
                let header = builder.create_block();
                let body = builder.create_block();
                let exit = builder.create_block();
                let count = builder.append_block_param(header, types::I64);
                let accumulator = builder.append_block_param(header, types::I128);
                let result = builder.append_block_param(exit, types::I128);
                let limit = builder.ins().band_imm(repetitions, 3);
                builder
                    .ins()
                    .jump(header, &[BlockArg::Value(limit), BlockArg::Value(left)]);
                builder.switch_to_block(header);
                builder
                    .ins()
                    .brif(count, body, &[], exit, &[BlockArg::Value(accumulator)]);
                builder.switch_to_block(body);
                let next = builder.ins().iadd(accumulator, right);
                let left = builder.ins().iadd_imm(count, -1);
                builder
                    .ins()
                    .jump(header, &[BlockArg::Value(left), BlockArg::Value(next)]);
                builder.switch_to_block(exit);
                result
            }
        };
        builder.ins().return_(&[result]);
        builder.seal_all_blocks();
        builder.finalize();

        let request = integer_block_words(&context.func).unwrap_or_else(|| panic!("{case:?}"));
        let code = binding::x86_64_integer_block(&request);
        assert!(!code.is_empty(), "{case:?}");
        let baseline = module
            .declare_function(&format!("baseline_{index}"), Linkage::Local, &signature)
            .unwrap();
        let candidate = module
            .declare_function(&format!("candidate_{index}"), Linkage::Local, &signature)
            .unwrap();
        module.define_function(baseline, &mut context).unwrap();
        let alignment = u64::from(module.isa().function_alignment().minimum);
        module
            .define_function_bytes(candidate, alignment, &code, &[])
            .unwrap();
        module.finalize_definitions().unwrap();

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(C)]
        struct Pair(u64, u64);
        type Entry = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64) -> Pair;
        // The wide arguments use consecutive GP registers, and the wide result
        // uses RAX and RDX. The module owns both pointers through the calls.
        let entry = |id| unsafe {
            std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(id))
        };
        let reference = entry(baseline);
        let vesper = entry(candidate);
        for [count, al, ah, bl, bh, tail] in [
            [0, 0, 0, 0, 0, 1],
            [3, u64::MAX, 0, 1, 0, 13],
            [1, 0, 0, 1, 0, 13],
            [2, u64::MAX, u64::MAX, u64::MAX, u64::MAX, 17],
            [3, 0x8000, 0, 0x8000, u64::MAX, 31],
            [0, 0, i64::MIN as u64, u64::MAX, 0, 13],
        ] {
            assert_eq!(
                unsafe { vesper(count, al, ah, bl, bh, tail) },
                unsafe { reference(count, al, ah, bl, bh, tail) },
                "{case:?}"
            );
        }
    }
    unsafe { module.free_memory() };
}
