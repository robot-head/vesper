//! Generated Cranelift AOT runtime for `TiledSeq[T]` (§12.5).
//!
//! Defines the same nine `vesper_tiled_*` symbols locally in AOT object files
//! so linked executables only depend on libc (`malloc`, `realloc`, `free`,
//! `memset`, `memmove`, `write`, `exit`).
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, condcodes::IntCC, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};

pub const FAULT_STATUS: i64 = 71;

const W: i32 = 0;
const STRIDE: i32 = 8;
const COLD_W: i32 = 16;
const LEN: i32 = 24;
const CAP: i32 = 32;
const HOT_RAW: i32 = 40;
const HOT_PTR: i32 = 48;
const COLD_PTR: i32 = 56;
const REGION_ID: i32 = 64;
const CHARGED: i32 = 72;
const ROW_CHARGE: i32 = 80;
const HOT_BYTES: i32 = 88;
const RECORD_BYTES: i64 = 96;

#[derive(Clone, Copy)]
enum Fault {
    Invalid,
    Bounds,
    Memory,
}

const FAULTS: [(Fault, &str); 3] = [
    (Fault::Invalid, "invalid TiledSeq handle or width"),
    (Fault::Bounds, "TiledSeq index out of bounds"),
    (Fault::Memory, "out of memory allocating TiledSeq buffer"),
];

pub fn defined<M: Module>(m: &M) -> bool {
    m.get_name("vesper_tiled_fault").is_some()
}

struct Imports {
    malloc: FuncId,
    realloc: FuncId,
    free: FuncId,
    memset: FuncId,
    memmove: FuncId,
    write: FuncId,
    exit: FuncId,
    region_alloc: FuncId,
    fault: FuncId,
    ensure_tiles: FuncId,
    charge_region: FuncId,
    messages: Vec<(DataId, i64)>,
}

pub fn define<M: Module>(m: &mut M) -> Result<(), String> {
    if defined(m) {
        return Ok(());
    }
    if m.target_config().pointer_type() != types::I64 {
        return Err("TiledSeq in executables needs a 64-bit target".into());
    }
    super::super::regions::aot_runtime::define(m)?;
    let e = |e: cranelift_module::ModuleError| e.to_string();
    let sig = |m: &M, params: usize, ret: bool| {
        let mut s = m.make_signature();
        s.params
            .extend((0..params).map(|_| AbiParam::new(types::I64)));
        if ret {
            s.returns.push(AbiParam::new(types::I64));
        }
        s
    };
    let import = |m: &mut M, name: &str, params: usize, ret: bool| {
        let s = sig(m, params, ret);
        m.declare_function(name, Linkage::Import, &s).map_err(e)
    };
    let local = |m: &mut M, name: &str, params: usize, ret: bool| {
        let s = sig(m, params, ret);
        m.declare_function(name, Linkage::Local, &s).map_err(e)
    };
    let mut messages = Vec::with_capacity(FAULTS.len());
    for (i, (_, msg)) in FAULTS.iter().enumerate() {
        let text = format!("vesper: native tiled fault: {msg}\n");
        let id = m
            .declare_data(
                &format!("vesper_tiled_msg_{i}"),
                Linkage::Local,
                false,
                false,
            )
            .map_err(e)?;
        let mut d = DataDescription::new();
        d.define(text.as_bytes().to_vec().into_boxed_slice());
        m.define_data(id, &d).map_err(e)?;
        messages.push((id, text.len() as i64));
    }
    let region_alloc = m
        .get_name("vesper_region_alloc")
        .and_then(|n| match n {
            cranelift_module::FuncOrDataId::Func(id) => Some(id),
            _ => None,
        })
        .ok_or("missing vesper_region_alloc in AOT module")?;
    let im = Imports {
        malloc: import(m, "malloc", 1, true)?,
        realloc: import(m, "realloc", 2, true)?,
        free: import(m, "free", 1, false)?,
        memset: import(m, "memset", 3, true)?,
        memmove: import(m, "memmove", 3, true)?,
        write: import(m, "write", 3, true)?,
        exit: import(m, "exit", 1, false)?,
        region_alloc,
        fault: local(m, "vesper_tiled_fault", 1, false)?,
        ensure_tiles: local(m, "vesper_tiled_ensure_tiles", 2, false)?,
        charge_region: local(m, "vesper_tiled_charge_region", 2, false)?,
        messages,
    };

    define_fault(m, &im)?;
    define_charge_region(m, &im)?;
    define_ensure_tiles(m, &im)?;
    define_new(m, &im)?;
    define_len(m, &im)?;
    define_tile_count(m, &im)?;
    define_push_begin(m, &im)?;
    define_hot_load(m, &im)?;
    define_hot_store(m, &im)?;
    define_cold_load(m, &im)?;
    define_cold_store(m, &im)?;
    define_tile_col_ptr(m, &im)?;
    Ok(())
}

fn compile<M: Module>(
    m: &mut M,
    id: FuncId,
    params: usize,
    ret: bool,
    build: impl FnOnce(&mut FunctionBuilder, &mut M, &[ir::Value]) -> Result<(), String>,
) -> Result<(), String> {
    let mut ctx = m.make_context();
    ctx.func
        .signature
        .params
        .extend((0..params).map(|_| AbiParam::new(types::I64)));
    if ret {
        ctx.func.signature.returns.push(AbiParam::new(types::I64));
    }
    let mut fbc = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let args = b.block_params(entry).to_vec();
        build(&mut b, m, &args)?;
        b.seal_all_blocks();
        b.finalize();
    }
    m.define_function(id, &mut ctx).map_err(|e| e.to_string())?;
    m.clear_context(&mut ctx);
    Ok(())
}

fn call_fn<M: Module>(
    b: &mut FunctionBuilder,
    m: &mut M,
    id: FuncId,
    args: &[ir::Value],
) -> Vec<ir::Value> {
    let r = m.declare_func_in_func(id, b.func);
    let inst = b.ins().call(r, args);
    b.inst_results(inst).to_vec()
}

fn trigger_fault<M: Module>(b: &mut FunctionBuilder, m: &mut M, im: &Imports, f: Fault) {
    let code = b.ins().iconst(types::I64, f as i64);
    call_fn(b, m, im.fault, &[code]);
    b.ins().trap(ir::TrapCode::unwrap_user(1));
}

fn check_non_null<M: Module>(
    b: &mut FunctionBuilder,
    m: &mut M,
    im: &Imports,
    ptr: ir::Value,
    fault: Fault,
) {
    let bad = b.ins().icmp_imm(IntCC::Equal, ptr, 0);
    let fail = b.create_block();
    let ok = b.create_block();
    b.ins().brif(bad, fail, &[], ok, &[]);
    b.switch_to_block(fail);
    trigger_fault(b, m, im, fault);
    b.switch_to_block(ok);
}

fn define_fault<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    compile(m, im.fault, 1, false, |b, m, args| {
        let code = args[0];
        let fd = b.ins().iconst(types::I64, 2);
        let done = b.create_block();
        for (i, (data_id, len)) in im.messages.iter().enumerate() {
            let matches = b.ins().icmp_imm(IntCC::Equal, code, i as i64);
            let hit = b.create_block();
            let miss = b.create_block();
            b.ins().brif(matches, hit, &[], miss, &[]);
            b.switch_to_block(hit);
            let gv = m.declare_data_in_func(*data_id, b.func);
            let ptr = b.ins().global_value(types::I64, gv);
            let len = b.ins().iconst(types::I64, *len);
            call_fn(b, m, im.write, &[fd, ptr, len]);
            b.ins().jump(done, &[]);
            b.switch_to_block(miss);
        }
        b.ins().jump(done, &[]);
        b.switch_to_block(done);
        let status = b.ins().iconst(types::I64, FAULT_STATUS);
        call_fn(b, m, im.exit, &[status]);
        b.ins().trap(ir::TrapCode::unwrap_user(1));
        Ok(())
    })
}

fn define_charge_region<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    compile(m, im.charge_region, 2, false, |b, m, args| {
        let seq = args[0];
        let rows = args[1];
        let rid = b.ins().load(types::I64, MemFlags::new(), seq, REGION_ID);
        let no_reg = b.ins().icmp_imm(IntCC::Equal, rid, 0);
        let done = b.create_block();
        let active = b.create_block();
        b.ins().brif(no_reg, done, &[], active, &[]);
        b.switch_to_block(active);
        let row_charge = b.ins().load(types::I64, MemFlags::new(), seq, ROW_CHARGE);
        let target = b.ins().imul(rows, row_charge);
        let charged = b.ins().load(types::I64, MemFlags::new(), seq, CHARGED);
        let need = b.ins().icmp(IntCC::UnsignedGreaterThan, target, charged);
        let do_alloc = b.create_block();
        b.ins().brif(need, do_alloc, &[], done, &[]);
        b.switch_to_block(do_alloc);
        let delta = b.ins().isub(target, charged);
        call_fn(b, m, im.region_alloc, &[rid, delta]);
        b.ins().store(MemFlags::new(), target, seq, CHARGED);
        b.ins().jump(done, &[]);
        b.switch_to_block(done);
        b.ins().return_(&[]);
        Ok(())
    })
}

fn define_ensure_tiles<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    compile(m, im.ensure_tiles, 2, false, |b, m, args| {
        let seq = args[0];
        let needed = args[1];
        let cap = b.ins().load(types::I64, MemFlags::new(), seq, CAP);
        let ok = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, needed, cap);
        let done = b.create_block();
        let grow = b.create_block();
        b.ins().brif(ok, done, &[], grow, &[]);
        b.switch_to_block(grow);

        let one = b.ins().iconst(types::I64, 1);
        let cap_nonzero = b.ins().icmp_imm(IntCC::NotEqual, cap, 0);
        let base_cap = b.ins().select(cap_nonzero, cap, one);
        let doubled = b.ins().ishl_imm(base_cap, 1);
        let use_doubled = b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, doubled, needed);
        let new_tiles = b.ins().select(use_doubled, doubled, needed);

        let stride = b.ins().load(types::I64, MemFlags::new(), seq, STRIDE);
        let new_hot_bytes = b.ins().imul(new_tiles, stride);
        let alloc_bytes = b.ins().iadd_imm(new_hot_bytes, 63);
        let new_raw = call_fn(b, m, im.malloc, &[alloc_bytes])[0];
        check_non_null(b, m, im, new_raw, Fault::Memory);
        let zero = b.ins().iconst(types::I64, 0);
        call_fn(b, m, im.memset, &[new_raw, zero, alloc_bytes]);

        let raw_plus = b.ins().iadd_imm(new_raw, 63);
        let mask = b.ins().iconst(types::I64, !63_i64);
        let new_aligned = b.ins().band(raw_plus, mask);

        let old_raw = b.ins().load(types::I64, MemFlags::new(), seq, HOT_RAW);
        let old_aligned = b.ins().load(types::I64, MemFlags::new(), seq, HOT_PTR);
        let old_bytes = b.ins().load(types::I64, MemFlags::new(), seq, HOT_BYTES);
        let has_old = b.ins().icmp_imm(IntCC::NotEqual, old_raw, 0);
        let copy_old = b.create_block();
        let after_copy = b.create_block();
        b.ins().brif(has_old, copy_old, &[], after_copy, &[]);
        b.switch_to_block(copy_old);
        call_fn(b, m, im.memmove, &[new_aligned, old_aligned, old_bytes]);
        call_fn(b, m, im.free, &[old_raw]);
        b.ins().jump(after_copy, &[]);
        b.switch_to_block(after_copy);

        b.ins().store(MemFlags::new(), new_raw, seq, HOT_RAW);
        b.ins().store(MemFlags::new(), new_aligned, seq, HOT_PTR);
        b.ins()
            .store(MemFlags::new(), new_hot_bytes, seq, HOT_BYTES);

        let w = b.ins().load(types::I64, MemFlags::new(), seq, W);
        let cold_w = b.ins().load(types::I64, MemFlags::new(), seq, COLD_W);
        let has_cold = b.ins().icmp_imm(IntCC::NotEqual, cold_w, 0);
        let grow_cold = b.create_block();
        let finish = b.create_block();
        b.ins().brif(has_cold, grow_cold, &[], finish, &[]);
        b.switch_to_block(grow_cold);
        let old_rows = b.ins().imul(cap, w);
        let old_cold_bytes = b.ins().imul(old_rows, cold_w);
        let old_cold_bytes = b.ins().ishl_imm(old_cold_bytes, 3);
        let new_rows = b.ins().imul(new_tiles, w);
        let new_cold_bytes = b.ins().imul(new_rows, cold_w);
        let new_cold_bytes = b.ins().ishl_imm(new_cold_bytes, 3);
        let old_cold = b.ins().load(types::I64, MemFlags::new(), seq, COLD_PTR);
        let new_cold = call_fn(b, m, im.realloc, &[old_cold, new_cold_bytes])[0];
        check_non_null(b, m, im, new_cold, Fault::Memory);
        let tail = b.ins().iadd(new_cold, old_cold_bytes);
        let delta_cold = b.ins().isub(new_cold_bytes, old_cold_bytes);
        call_fn(b, m, im.memset, &[tail, zero, delta_cold]);
        b.ins().store(MemFlags::new(), new_cold, seq, COLD_PTR);
        b.ins().jump(finish, &[]);

        b.switch_to_block(finish);
        b.ins().store(MemFlags::new(), new_tiles, seq, CAP);
        b.ins().jump(done, &[]);
        b.switch_to_block(done);
        b.ins().return_(&[]);
        Ok(())
    })
}

fn define_new<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    let id = m
        .declare_function("vesper_tiled_new", Linkage::Local, &{
            let mut s = m.make_signature();
            for _ in 0..5 {
                s.params.push(AbiParam::new(types::I64));
            }
            s.returns.push(AbiParam::new(types::I64));
            s
        })
        .map_err(|e| e.to_string())?;
    compile(m, id, 5, true, |b, m, args| {
        let (w, stride, cold_w, rid, cap) = (args[0], args[1], args[2], args[3], args[4]);
        let bad_w = b.ins().icmp_imm(IntCC::Equal, w, 0);
        let fail = b.create_block();
        let ok = b.create_block();
        b.ins().brif(bad_w, fail, &[], ok, &[]);
        b.switch_to_block(fail);
        trigger_fault(b, m, im, Fault::Invalid);
        b.switch_to_block(ok);

        let sz = b.ins().iconst(types::I64, RECORD_BYTES);
        let seq = call_fn(b, m, im.malloc, &[sz])[0];
        check_non_null(b, m, im, seq, Fault::Memory);
        let zero = b.ins().iconst(types::I64, 0);
        call_fn(b, m, im.memset, &[seq, zero, sz]);

        let w_minus_1 = b.ins().iadd_imm(w, -1);
        let numerator = b.ins().iadd(stride, w_minus_1);
        let hot_per_row = b.ins().udiv(numerator, w);
        let cold_bytes = b.ins().ishl_imm(cold_w, 3);
        let sum = b.ins().iadd(hot_per_row, cold_bytes);
        let eight = b.ins().iconst(types::I64, 8);
        let ge8 = b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, sum, 8);
        let row_charge = b.ins().select(ge8, sum, eight);

        b.ins().store(MemFlags::new(), w, seq, W);
        b.ins().store(MemFlags::new(), stride, seq, STRIDE);
        b.ins().store(MemFlags::new(), cold_w, seq, COLD_W);
        b.ins().store(MemFlags::new(), rid, seq, REGION_ID);
        b.ins().store(MemFlags::new(), row_charge, seq, ROW_CHARGE);

        let has_cap = b.ins().icmp_imm(IntCC::NotEqual, cap, 0);
        let init_cap = b.create_block();
        let done = b.create_block();
        b.ins().brif(has_cap, init_cap, &[], done, &[]);
        b.switch_to_block(init_cap);
        call_fn(b, m, im.charge_region, &[seq, cap]);
        let cap_num = b.ins().iadd(cap, w_minus_1);
        let tiles = b.ins().udiv(cap_num, w);
        call_fn(b, m, im.ensure_tiles, &[seq, tiles]);
        b.ins().jump(done, &[]);
        b.switch_to_block(done);
        b.ins().return_(&[seq]);
        Ok(())
    })
}

fn define_len<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    let mut s = m.make_signature();
    s.params.push(AbiParam::new(types::I64));
    s.returns.push(AbiParam::new(types::I64));
    let id = m
        .declare_function("vesper_tiled_len", Linkage::Local, &s)
        .map_err(|e| e.to_string())?;
    compile(m, id, 1, true, |b, m, args| {
        let seq = args[0];
        check_non_null(b, m, im, seq, Fault::Invalid);
        let len = b.ins().load(types::I64, MemFlags::new(), seq, LEN);
        b.ins().return_(&[len]);
        Ok(())
    })
}

fn define_tile_count<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    let mut s = m.make_signature();
    s.params.push(AbiParam::new(types::I64));
    s.returns.push(AbiParam::new(types::I64));
    let id = m
        .declare_function("vesper_tiled_tile_count", Linkage::Local, &s)
        .map_err(|e| e.to_string())?;
    compile(m, id, 1, true, |b, m, args| {
        let seq = args[0];
        check_non_null(b, m, im, seq, Fault::Invalid);
        let len = b.ins().load(types::I64, MemFlags::new(), seq, LEN);
        let w = b.ins().load(types::I64, MemFlags::new(), seq, W);
        let w_m1 = b.ins().iadd_imm(w, -1);
        let num = b.ins().iadd(len, w_m1);
        let tiles = b.ins().udiv(num, w);
        b.ins().return_(&[tiles]);
        Ok(())
    })
}

fn define_push_begin<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    let mut s = m.make_signature();
    s.params.push(AbiParam::new(types::I64));
    s.returns.push(AbiParam::new(types::I64));
    let id = m
        .declare_function("vesper_tiled_push_begin", Linkage::Local, &s)
        .map_err(|e| e.to_string())?;
    compile(m, id, 1, true, |b, m, args| {
        let seq = args[0];
        check_non_null(b, m, im, seq, Fault::Invalid);
        let row = b.ins().load(types::I64, MemFlags::new(), seq, LEN);
        let next_len = b.ins().iadd_imm(row, 1);
        call_fn(b, m, im.charge_region, &[seq, next_len]);
        let w = b.ins().load(types::I64, MemFlags::new(), seq, W);
        let w_m1 = b.ins().iadd_imm(w, -1);
        let num = b.ins().iadd(next_len, w_m1);
        let tiles = b.ins().udiv(num, w);
        call_fn(b, m, im.ensure_tiles, &[seq, tiles]);
        b.ins().store(MemFlags::new(), next_len, seq, LEN);
        b.ins().return_(&[row]);
        Ok(())
    })
}

fn emit_hot_ptr<M: Module>(
    b: &mut FunctionBuilder,
    m: &mut M,
    im: &Imports,
    seq: ir::Value,
    row: ir::Value,
    col_off: ir::Value,
    eb: ir::Value,
) -> ir::Value {
    check_non_null(b, m, im, seq, Fault::Invalid);
    let len = b.ins().load(types::I64, MemFlags::new(), seq, LEN);
    let in_bounds = b.ins().icmp(IntCC::UnsignedLessThan, row, len);
    let ok = b.create_block();
    let fail = b.create_block();
    b.ins().brif(in_bounds, ok, &[], fail, &[]);
    b.switch_to_block(fail);
    trigger_fault(b, m, im, Fault::Bounds);
    b.switch_to_block(ok);

    let w = b.ins().load(types::I64, MemFlags::new(), seq, W);
    let tile = b.ins().udiv(row, w);
    let lane = b.ins().urem(row, w);
    let stride = b.ins().load(types::I64, MemFlags::new(), seq, STRIDE);
    let base_tile = b.ins().imul(tile, stride);
    let lane_off = b.ins().imul(lane, eb);
    let t1 = b.ins().iadd(base_tile, col_off);
    let off = b.ins().iadd(t1, lane_off);
    let hot_ptr = b.ins().load(types::I64, MemFlags::new(), seq, HOT_PTR);
    b.ins().iadd(hot_ptr, off)
}

fn define_hot_load<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    let mut s = m.make_signature();
    for _ in 0..4 {
        s.params.push(AbiParam::new(types::I64));
    }
    s.returns.push(AbiParam::new(types::I64));
    let id = m
        .declare_function("vesper_tiled_hot_load", Linkage::Local, &s)
        .map_err(|e| e.to_string())?;
    compile(m, id, 4, true, |b, m, args| {
        let (seq, row, col_off, eb) = (args[0], args[1], args[2], args[3]);
        let ptr = emit_hot_ptr(b, m, im, seq, row, col_off, eb);
        let is1 = b.ins().icmp_imm(IntCC::Equal, eb, 1);
        let b1 = b.create_block();
        let not1 = b.create_block();
        b.ins().brif(is1, b1, &[], not1, &[]);
        b.switch_to_block(b1);
        let v1 = b.ins().uload8(types::I64, MemFlags::new(), ptr, 0);
        b.ins().return_(&[v1]);

        b.switch_to_block(not1);
        let is4 = b.ins().icmp_imm(IntCC::Equal, eb, 4);
        let b4 = b.create_block();
        let b8 = b.create_block();
        b.ins().brif(is4, b4, &[], b8, &[]);
        b.switch_to_block(b4);
        let v4 = b.ins().uload32(MemFlags::new(), ptr, 0);
        b.ins().return_(&[v4]);

        b.switch_to_block(b8);
        let v8 = b.ins().load(types::I64, MemFlags::new(), ptr, 0);
        b.ins().return_(&[v8]);
        Ok(())
    })
}

fn define_hot_store<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    let mut s = m.make_signature();
    for _ in 0..5 {
        s.params.push(AbiParam::new(types::I64));
    }
    s.returns.push(AbiParam::new(types::I64));
    let id = m
        .declare_function("vesper_tiled_hot_store", Linkage::Local, &s)
        .map_err(|e| e.to_string())?;
    compile(m, id, 5, true, |b, m, args| {
        let (seq, row, col_off, eb, val) = (args[0], args[1], args[2], args[3], args[4]);
        let ptr = emit_hot_ptr(b, m, im, seq, row, col_off, eb);
        let zero = b.ins().iconst(types::I64, 0);
        let is1 = b.ins().icmp_imm(IntCC::Equal, eb, 1);
        let b1 = b.create_block();
        let not1 = b.create_block();
        b.ins().brif(is1, b1, &[], not1, &[]);
        b.switch_to_block(b1);
        b.ins().istore8(MemFlags::new(), val, ptr, 0);
        b.ins().return_(&[zero]);

        b.switch_to_block(not1);
        let is4 = b.ins().icmp_imm(IntCC::Equal, eb, 4);
        let b4 = b.create_block();
        let b8 = b.create_block();
        b.ins().brif(is4, b4, &[], b8, &[]);
        b.switch_to_block(b4);
        b.ins().istore32(MemFlags::new(), val, ptr, 0);
        b.ins().return_(&[zero]);

        b.switch_to_block(b8);
        b.ins().store(MemFlags::new(), val, ptr, 0);
        b.ins().return_(&[zero]);
        Ok(())
    })
}

fn emit_cold_ptr<M: Module>(
    b: &mut FunctionBuilder,
    m: &mut M,
    im: &Imports,
    seq: ir::Value,
    row: ir::Value,
    word_off: ir::Value,
) -> ir::Value {
    check_non_null(b, m, im, seq, Fault::Invalid);
    let len = b.ins().load(types::I64, MemFlags::new(), seq, LEN);
    let in_bounds = b.ins().icmp(IntCC::UnsignedLessThan, row, len);
    let ok = b.create_block();
    let fail = b.create_block();
    b.ins().brif(in_bounds, ok, &[], fail, &[]);
    b.switch_to_block(fail);
    trigger_fault(b, m, im, Fault::Bounds);
    b.switch_to_block(ok);

    let cold_w = b.ins().load(types::I64, MemFlags::new(), seq, COLD_W);
    let row_off = b.ins().imul(row, cold_w);
    let idx = b.ins().iadd(row_off, word_off);
    let byte_off = b.ins().ishl_imm(idx, 3);
    let cold_ptr = b.ins().load(types::I64, MemFlags::new(), seq, COLD_PTR);
    b.ins().iadd(cold_ptr, byte_off)
}

fn define_cold_load<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    let mut s = m.make_signature();
    for _ in 0..3 {
        s.params.push(AbiParam::new(types::I64));
    }
    s.returns.push(AbiParam::new(types::I64));
    let id = m
        .declare_function("vesper_tiled_cold_load", Linkage::Local, &s)
        .map_err(|e| e.to_string())?;
    compile(m, id, 3, true, |b, m, args| {
        let ptr = emit_cold_ptr(b, m, im, args[0], args[1], args[2]);
        let word = b.ins().load(types::I64, MemFlags::new(), ptr, 0);
        b.ins().return_(&[word]);
        Ok(())
    })
}

fn define_cold_store<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    let mut s = m.make_signature();
    for _ in 0..4 {
        s.params.push(AbiParam::new(types::I64));
    }
    s.returns.push(AbiParam::new(types::I64));
    let id = m
        .declare_function("vesper_tiled_cold_store", Linkage::Local, &s)
        .map_err(|e| e.to_string())?;
    compile(m, id, 4, true, |b, m, args| {
        let ptr = emit_cold_ptr(b, m, im, args[0], args[1], args[2]);
        b.ins().store(MemFlags::new(), args[3], ptr, 0);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().return_(&[zero]);
        Ok(())
    })
}

fn define_tile_col_ptr<M: Module>(m: &mut M, im: &Imports) -> Result<(), String> {
    let mut s = m.make_signature();
    for _ in 0..3 {
        s.params.push(AbiParam::new(types::I64));
    }
    s.returns.push(AbiParam::new(types::I64));
    let id = m
        .declare_function("vesper_tiled_tile_col_ptr", Linkage::Local, &s)
        .map_err(|e| e.to_string())?;
    compile(m, id, 3, true, |b, m, args| {
        let (seq, tile_idx, col_off) = (args[0], args[1], args[2]);
        check_non_null(b, m, im, seq, Fault::Invalid);
        let len = b.ins().load(types::I64, MemFlags::new(), seq, LEN);
        let w = b.ins().load(types::I64, MemFlags::new(), seq, W);
        let w_m1 = b.ins().iadd_imm(w, -1);
        let num = b.ins().iadd(len, w_m1);
        let tile_count = b.ins().udiv(num, w);
        let ok_b = b.create_block();
        let fail_b = b.create_block();
        let valid = b.ins().icmp(IntCC::UnsignedLessThan, tile_idx, tile_count);
        b.ins().brif(valid, ok_b, &[], fail_b, &[]);
        b.switch_to_block(fail_b);
        trigger_fault(b, m, im, Fault::Bounds);
        b.switch_to_block(ok_b);

        let stride = b.ins().load(types::I64, MemFlags::new(), seq, STRIDE);
        let tile_off = b.ins().imul(tile_idx, stride);
        let off = b.ins().iadd(tile_off, col_off);
        let hot_ptr = b.ins().load(types::I64, MemFlags::new(), seq, HOT_PTR);
        let ptr = b.ins().iadd(hot_ptr, off);
        b.ins().return_(&[ptr]);
        Ok(())
    })
}
