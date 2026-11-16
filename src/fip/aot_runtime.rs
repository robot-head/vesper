//! Generated Cranelift AOT runtime for `Box[T]` (`vesper_box_alloc`) (§12.4).
//!
//! Only emitted into an `ObjectModule` if a non-`@fip` function actually calls
//! `Box::new` or calls a `Box::new_in_caller` `@fip` function from non-`@fip`
//! code; pure `@fip` libraries never even reference `vesper_box_alloc`.

use cranelift_codegen::ir::{AbiParam, InstBuilder, condcodes::IntCC, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};

pub fn declare_or_define<M: Module>(m: &mut M) -> Result<FuncId, String> {
    let e = |err: cranelift_module::ModuleError| err.to_string();
    if let Some(id) = m.get_name("vesper_box_alloc") {
        let cranelift_module::FuncOrDataId::Func(func_id) = id else {
            return Err("vesper_box_alloc collides with data symbol".into());
        };
        return Ok(func_id);
    }
    if m.target_config().pointer_type() != types::I64 {
        return Err("Box[T] AOT runtime requires a 64-bit target".into());
    }
    let mut calloc_sig = m.make_signature();
    calloc_sig.params.push(AbiParam::new(types::I64));
    calloc_sig.params.push(AbiParam::new(types::I64));
    calloc_sig.returns.push(AbiParam::new(types::I64));
    let calloc = m
        .declare_function("calloc", Linkage::Import, &calloc_sig)
        .map_err(e)?;

    let mut abort_sig = m.make_signature();
    abort_sig.params.push(AbiParam::new(types::I32));
    let exit_fn = m
        .declare_function("exit", Linkage::Import, &abort_sig)
        .map_err(e)?;

    let mut alloc_sig = m.make_signature();
    alloc_sig.params.push(AbiParam::new(types::I64));
    alloc_sig.returns.push(AbiParam::new(types::I64));
    let alloc_id = m
        .declare_function("vesper_box_alloc", Linkage::Local, &alloc_sig)
        .map_err(e)?;

    let mut ctx = m.make_context();
    ctx.func.signature = alloc_sig;
    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let entry = b.create_block();
        let oom = b.create_block();
        let ok = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let req = b.block_params(entry)[0];
        let eight = b.ins().iconst(types::I64, 8);
        let too_small = b.ins().icmp(IntCC::UnsignedLessThan, req, eight);
        let size = b.ins().select(too_small, eight, req);
        let one = b.ins().iconst(types::I64, 1);
        let calloc_ref = m.declare_func_in_func(calloc, b.func);
        let call = b.ins().call(calloc_ref, &[one, size]);
        let ptr = b.inst_results(call)[0];
        let is_null = b.ins().icmp_imm(IntCC::Equal, ptr, 0);
        b.ins().brif(is_null, oom, &[], ok, &[]);

        b.switch_to_block(oom);
        let exit_ref = m.declare_func_in_func(exit_fn, b.func);
        let status = b.ins().iconst(types::I32, 72);
        b.ins().call(exit_ref, &[status]);
        b.ins()
            .trap(cranelift_codegen::ir::TrapCode::unwrap_user(72));

        b.switch_to_block(ok);
        b.ins().return_(&[ptr]);
        b.seal_all_blocks();
        b.finalize();
    }
    m.define_function(alloc_id, &mut ctx).map_err(e)?;
    m.clear_context(&mut ctx);
    Ok(alloc_id)
}
