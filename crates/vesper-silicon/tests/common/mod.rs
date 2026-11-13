//! Builders for real Cranelift functions used by the acceptance tests.
#![allow(dead_code)]

use cranelift_codegen::ir::{
    AbiParam, ExtFuncData, ExternalName, Function, InstBuilder, MemFlags, Signature, StackSlotData,
    StackSlotKind, UserFuncName, types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

pub fn sig() -> Signature {
    let mut s = Signature::new(CallConv::SystemV);
    s.params.push(AbiParam::new(types::I64));
    s.returns.push(AbiParam::new(types::I64));
    s
}

/// A function with one explicit stack slot of `slot_bytes` that calls each
/// of `callees` (all `fn(i64) -> i64`), optionally an indirect call through
/// its argument, and returns.
pub fn func(name: &str, slot_bytes: u32, callees: &[&str], indirect: bool) -> Function {
    let mut f = Function::with_name_signature(UserFuncName::testcase(name), sig());
    let mut ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut f, &mut ctx);
    let block = b.create_block();
    b.append_block_params_for_function_params(block);
    b.switch_to_block(block);
    b.seal_block(block);
    let mut v = b.block_params(block)[0];
    if slot_bytes > 0 {
        let ss = b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            slot_bytes,
            3,
        ));
        b.ins().stack_store(v, ss, 0);
        let off = i32::try_from(slot_bytes - 8).unwrap();
        b.ins().stack_store(v, ss, off);
        let w = b.ins().stack_load(types::I64, ss, off);
        v = b.ins().iadd(v, w);
    }
    for callee in callees {
        let sigref = b.import_signature(sig());
        let fref = b.import_function(ExtFuncData {
            name: ExternalName::testcase(*callee),
            signature: sigref,
            colocated: true,
        });
        let call = b.ins().call(fref, &[v]);
        v = b.inst_results(call)[0];
    }
    if indirect {
        let sigref = b.import_signature(sig());
        let target = b.block_params(block)[0];
        let call = b.ins().call_indirect(sigref, target, &[v]);
        v = b.inst_results(call)[0];
    }
    let _ = MemFlags::trusted();
    b.ins().return_(&[v]);
    b.finalize();
    f
}

/// `fn name(x) { if x { then_calls } else { else_calls }; after_calls }`.
pub fn branchy(
    name: &str,
    then_calls: &[&str],
    else_calls: &[&str],
    after_calls: &[&str],
) -> Function {
    let mut f = Function::with_name_signature(UserFuncName::testcase(name), sig());
    let mut ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut f, &mut ctx);
    let entry = b.create_block();
    let then_b = b.create_block();
    let else_b = b.create_block();
    let join = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.append_block_param(join, types::I64);
    b.switch_to_block(entry);
    let x = b.block_params(entry)[0];
    b.ins().brif(x, then_b, &[], else_b, &[]);
    for (blk, calls) in [(then_b, then_calls), (else_b, else_calls)] {
        b.switch_to_block(blk);
        let mut v = x;
        for c in calls {
            v = call(&mut b, c, v);
        }
        b.ins().jump(join, &[v.into()]);
    }
    b.switch_to_block(join);
    let mut v = b.block_params(join)[0];
    for c in after_calls {
        v = call(&mut b, c, v);
    }
    b.ins().return_(&[v]);
    b.seal_all_blocks();
    b.finalize();
    f
}

fn call(
    b: &mut FunctionBuilder<'_>,
    callee: &str,
    v: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let sigref = b.import_signature(sig());
    let fref = b.import_function(ExtFuncData {
        name: ExternalName::testcase(callee),
        signature: sigref,
        colocated: true,
    });
    let c = b.ins().call(fref, &[v]);
    b.inst_results(c)[0]
}

/// A function computing `ceil(f64)`; on baseline x86_64 (no SSE4.1)
/// Cranelift lowers it to a libcall.
pub fn ceil_user(name: &str) -> Function {
    let mut s = Signature::new(CallConv::SystemV);
    s.params.push(AbiParam::new(types::F64));
    s.returns.push(AbiParam::new(types::F64));
    let mut f = Function::with_name_signature(UserFuncName::testcase(name), s);
    let mut ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut f, &mut ctx);
    let blk = b.create_block();
    b.append_block_params_for_function_params(blk);
    b.switch_to_block(blk);
    b.seal_block(blk);
    let x = b.block_params(blk)[0];
    let y = b.ins().ceil(x);
    b.ins().return_(&[y]);
    b.finalize();
    f
}

/// Loads/stores at constant MMIO addresses: `ops` is a list of
/// ("load", addr) / ("store_const", addr, value) / ("rmw", addr, set_bits).
pub enum Mmio {
    Load(u64),
    StoreConst(u64, u32),
    Rmw(u64, u32),
    Uload8(u64),
    Istore8(u64, u8),
    /// Load in one block, pass the value as a block argument, OR and store
    /// in the successor.
    RmwAcrossBlocks(u64, u32),
}

pub fn mmio_fn(name: &str, ops: &[Mmio]) -> Function {
    let mut f = Function::with_name_signature(UserFuncName::testcase(name), sig());
    let mut ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut f, &mut ctx);
    let blk = b.create_block();
    b.append_block_params_for_function_params(blk);
    b.switch_to_block(blk);
    b.seal_block(blk);
    let x = b.block_params(blk)[0];
    let fl = MemFlags::new();
    for op in ops {
        match *op {
            Mmio::Load(a) => {
                let base = b.ins().iconst(types::I64, (a & !0xf) as i64);
                b.ins().load(types::I32, fl, base, (a & 0xf) as i32);
            }
            Mmio::StoreConst(a, v) => {
                let addr = b.ins().iconst(types::I64, a as i64);
                let v = b.ins().iconst(types::I32, i64::from(v));
                b.ins().store(fl, v, addr, 0);
            }
            Mmio::Rmw(a, set) => {
                let addr = b.ins().iconst(types::I64, a as i64);
                let old = b.ins().load(types::I32, fl, addr, 0);
                let new = b.ins().bor_imm(old, i64::from(set));
                b.ins().store(fl, new, addr, 0);
            }
            Mmio::Uload8(a) => {
                let addr = b.ins().iconst(types::I64, a as i64);
                b.ins().uload8(types::I32, fl, addr, 0);
            }
            Mmio::Istore8(a, v) => {
                let addr = b.ins().iconst(types::I64, a as i64);
                let v = b.ins().iconst(types::I32, i64::from(v));
                b.ins().istore8(fl, v, addr, 0);
            }
            Mmio::RmwAcrossBlocks(a, set) => {
                let addr = b.ins().iconst(types::I64, a as i64);
                let old = b.ins().load(types::I32, fl, addr, 0);
                let next = b.create_block();
                b.append_block_param(next, types::I32);
                b.ins().jump(next, &[old.into()]);
                b.switch_to_block(next);
                b.seal_block(next);
                let p = b.block_params(next)[0];
                let new = b.ins().bor_imm(p, i64::from(set));
                let addr2 = b.ins().iconst(types::I64, a as i64);
                b.ins().store(fl, new, addr2, 0);
            }
        }
    }
    b.ins().return_(&[x]);
    b.finalize();
    f
}

/// Keeps `live` values alive across a call so the register allocator must
/// use callee-saved registers (exercising clobber saves in the prologue).
pub fn clobbering(name: &str, callee: &str, live: usize) -> Function {
    let mut f = Function::with_name_signature(UserFuncName::testcase(name), sig());
    let mut ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut f, &mut ctx);
    let blk = b.create_block();
    b.append_block_params_for_function_params(blk);
    b.switch_to_block(blk);
    b.seal_block(blk);
    let x = b.block_params(blk)[0];
    let vals: Vec<_> = (0..live)
        .map(|i| b.ins().iadd_imm(x, i as i64 + 1))
        .collect();
    let mut acc = call(&mut b, callee, x);
    for v in vals {
        let m = b.ins().imul(v, v);
        acc = b.ins().iadd(acc, m);
    }
    b.ins().return_(&[acc]);
    b.finalize();
    f
}
