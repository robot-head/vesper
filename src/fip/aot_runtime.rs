//! Generated Cranelift AOT runtime for `Box[T]` (`vesper_box_alloc` / `vesper_box_free`) (§12.4).
//!
//! Only emitted into an `ObjectModule` if a non-`@fip` function actually calls
//! `Box::new`, reclaims an unpacked/dropped `Box[T]`, or calls a
//! `Box::new_in_caller` `@fip` function from non-`@fip` code; pure `@fip`
//! libraries never reference `vesper_box_alloc` or `vesper_box_free`.
//!
//! [`LIVE_COUNTER`] counts live boxes, so that leak tests of export ABI v2
//! libraries can see allocations a Rust allocator never sees (#898).

use cranelift_codegen::ir::{
    AbiParam, AtomicRmwOp, InstBuilder, MemFlags, condcodes::IntCC, types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};

/// The local data word holding the number of live boxes.
pub(crate) const LIVE_COUNTER: &str = "vesper_box_live";

fn live_counter<M: Module>(m: &mut M) -> Result<DataId, String> {
    let e = |err: cranelift_module::ModuleError| err.to_string();
    if let Some(cranelift_module::FuncOrDataId::Data(id)) = m.get_name(LIVE_COUNTER) {
        return Ok(id);
    }
    let id = m
        .declare_data(LIVE_COUNTER, Linkage::Local, true, false)
        .map_err(e)?;
    let mut zero = DataDescription::new();
    zero.define_zeroinit(8);
    zero.set_align(8);
    m.define_data(id, &zero).map_err(e)?;
    Ok(id)
}

fn count<M: Module>(b: &mut FunctionBuilder, m: &mut M, live: DataId, delta: i64) {
    let data = m.declare_data_in_func(live, b.func);
    let address = b.ins().symbol_value(types::I64, data);
    let delta = b.ins().iconst(types::I64, delta);
    b.ins().atomic_rmw(
        types::I64,
        MemFlags::trusted(),
        AtomicRmwOp::Add,
        address,
        delta,
    );
}

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
    let live = live_counter(m)?;
    let mut calloc_sig = m.make_signature();
    calloc_sig.params.push(AbiParam::new(types::I64));
    calloc_sig.params.push(AbiParam::new(types::I64));
    calloc_sig.returns.push(AbiParam::new(types::I64));
    let calloc = m
        .declare_function("calloc", Linkage::Import, &calloc_sig)
        .map_err(e)?;

    // `exit` is declared with a word argument, as the sequence, region and
    // tiled runtimes declare it, so a module that links several runtimes
    // (an export ABI v2 library) declares one signature.
    let mut abort_sig = m.make_signature();
    abort_sig.params.push(AbiParam::new(types::I64));
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
        let status = b.ins().iconst(types::I64, 72);
        b.ins().call(exit_ref, &[status]);
        b.ins()
            .trap(cranelift_codegen::ir::TrapCode::unwrap_user(72));

        b.switch_to_block(ok);
        count(&mut b, m, live, 1);
        b.ins().return_(&[ptr]);
        b.seal_all_blocks();
        b.finalize();
    }
    m.define_function(alloc_id, &mut ctx).map_err(e)?;
    m.clear_context(&mut ctx);
    Ok(alloc_id)
}

pub fn declare_or_define_free<M: Module>(m: &mut M) -> Result<FuncId, String> {
    let e = |err: cranelift_module::ModuleError| err.to_string();
    if let Some(id) = m.get_name("vesper_box_free") {
        let cranelift_module::FuncOrDataId::Func(func_id) = id else {
            return Err("vesper_box_free collides with data symbol".into());
        };
        return Ok(func_id);
    }
    if m.target_config().pointer_type() != types::I64 {
        return Err("Box[T] AOT runtime requires a 64-bit target".into());
    }
    let live = live_counter(m)?;
    let mut free_sig = m.make_signature();
    free_sig.params.push(AbiParam::new(types::I64));
    let libc_free = m
        .declare_function("free", Linkage::Import, &free_sig)
        .map_err(e)?;

    let box_free_id = m
        .declare_function("vesper_box_free", Linkage::Local, &free_sig)
        .map_err(e)?;

    let mut ctx = m.make_context();
    ctx.func.signature = free_sig;
    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let entry = b.create_block();
        let do_free = b.create_block();
        let done = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let ptr = b.block_params(entry)[0];
        let is_null = b.ins().icmp_imm(IntCC::Equal, ptr, 0);
        b.ins().brif(is_null, done, &[], do_free, &[]);

        b.switch_to_block(do_free);
        let free_ref = m.declare_func_in_func(libc_free, b.func);
        b.ins().call(free_ref, &[ptr]);
        count(&mut b, m, live, -1);
        b.ins().jump(done, &[]);

        b.switch_to_block(done);
        b.ins().return_(&[]);
        b.seal_all_blocks();
        b.finalize();
    }
    m.define_function(box_free_id, &mut ctx).map_err(e)?;
    m.clear_context(&mut ctx);
    Ok(box_free_id)
}
