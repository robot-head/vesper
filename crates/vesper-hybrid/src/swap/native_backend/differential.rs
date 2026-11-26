use super::binding;
use super::machine::LeafOp;
use cranelift_codegen::ir::{AbiParam, InstBuilder, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

const ABI_ARGS: usize = 6;

#[derive(Clone, Copy, Debug)]
enum Case {
    Constant(i64),
    Argument(usize),
    Binary(Binary),
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
fn leaf_backend_matches_cranelift_results() {
    assert!(binding::x86_64_leaf(u64::MAX, 0, 0).is_empty());
    assert!(binding::x86_64_leaf(LeafOp::ReturnArgument as u64, ABI_ARGS as u64, 0).is_empty());

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
    ];
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
        let (result, request) = match case {
            Case::Constant(bits) => (
                builder.ins().iconst(types::I64, bits),
                (LeafOp::ReturnConstant, bits as u64, 0),
            ),
            Case::Argument(index) => (
                builder.block_params(entry)[index],
                (LeafOp::ReturnArgument, index as u64, 0),
            ),
            Case::Binary(op) => {
                let left = builder.block_params(entry)[4];
                let right = builder.block_params(entry)[5];
                let (result, op) = match op {
                    Binary::Add => (builder.ins().iadd(left, right), LeafOp::AddArguments),
                    Binary::Sub => (builder.ins().isub(left, right), LeafOp::SubArguments),
                    Binary::Mul => (builder.ins().imul(left, right), LeafOp::MulArguments),
                    Binary::And => (builder.ins().band(left, right), LeafOp::AndArguments),
                    Binary::Or => (builder.ins().bor(left, right), LeafOp::OrArguments),
                    Binary::Xor => (builder.ins().bxor(left, right), LeafOp::XorArguments),
                };
                (result, (op, 4, 5))
            }
        };
        builder.ins().return_(&[result]);
        builder.seal_all_blocks();
        builder.finalize();

        let (op, left, right) = request;
        let bytes = binding::x86_64_leaf(op as u64, left, right);
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
        ] {
            assert_eq!(
                unsafe { candidate(a, b, c, d, e, f) },
                unsafe { baseline(a, b, c, d, e, f) },
                "{case:?}"
            );
        }
    }
    eprintln!(
        "{} of {} selected leaves differ in bytes from Cranelift",
        byte_differences,
        cases.len()
    );
    // All code pointers have gone out of scope.
    unsafe { module.free_memory() };
}
