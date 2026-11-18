//! Generated Cranelift AOT runtime for `ChunkSeq[T]` (§13).
//!
//! Defines the sequence runtime locally in AOT objects, with the semantics
//! of the JIT runtime (`src/seq/native_runtime.rs`), so linked executables
//! need only `malloc`, `realloc`, `free`, `memcpy` and `exit` from the C
//! library (plus `write` for the fault message off Windows). An allocation
//! that fails or overflows exits with [`crate::seq::FAULT_STATUS`].
//!
//! A sequence is a three-word header `{len, cap, data}`. Generated code
//! makes one with `vesper_seq_make(cap, stride)`: a single `malloc` block
//! holding the header and, right after it, room for `cap` elements when they
//! fit in [`INLINE_BYTES`] (port gaps lane 1, #984). Growth moves the
//! elements to a separate buffer and leaves the inline room unused until the
//! header is freed. `vesper_seq_new(cap, stride)` (the export ABI's
//! constructor, whose buffer a caller may take over) always makes a bare
//! header and a separate buffer.
//!
//! A separate buffer starts [`BUFFER_PREFIX`] bytes into its `malloc` block.
//! So no other object's header can end where a separate buffer starts, and
//! `data == header + 24` (with a non-zero capacity) tells inline elements
//! apart from a separate buffer without any bookkeeping: inline elements are
//! freed with their header, a separate buffer on its own. Capacity 0 with
//! data marks borrowed bytes (a string literal or view), which nothing
//! frees.
//!
//! `vesper_seq_free` frees a header and its buffer and `vesper_str_release`
//! the buffer of an inline `String` value (export ABI v2 calls them),
//! and [`LIVE_COUNTER`] counts the live header and buffer allocations (a
//! header with inline elements counts once) so that leak tests can see
//! allocations a Rust allocator never sees (#898).
use crate::seq::native_runtime::{BUFFER_PREFIX, INLINE_BYTES};
use cranelift_codegen::ir::{
    self, AbiParam, AtomicRmwOp, InstBuilder, MemFlags, condcodes::IntCC, types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, FuncId, Linkage, Module};

const MESSAGES: [&str; 3] = [
    "vesper: native ChunkSeq fault: out of memory\n",
    "vesper: native ChunkSeq fault: slice interval out of bounds\n",
    "vesper: native String fault: interval is not on char boundaries\n",
];

/// The local data word holding the number of live sequence allocations
/// (headers plus separate element buffers).
pub(crate) const LIVE_COUNTER: &str = "vesper_seq_live";

pub fn defined<M: Module>(m: &M) -> bool {
    m.get_name("vesper_seq_fault").is_some()
}

struct Imports {
    malloc: FuncId,
    realloc: FuncId,
    fault: FuncId,
    live: cranelift_module::DataId,
}

pub fn define<M: Module>(m: &mut M) -> Result<(), String> {
    if defined(m) {
        return Ok(());
    }
    if m.target_config().pointer_type() != types::I64 {
        return Err("ChunkSeq in executables needs a 64-bit target".into());
    }
    let windows = m.isa().triple().operating_system == target_lexicon::OperatingSystem::Windows;
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
    let local = |m: &mut M, name: &str, params: usize| {
        let s = sig(m, params, true);
        m.declare_function(name, Linkage::Local, &s).map_err(e)
    };
    let live = m
        .declare_data(LIVE_COUNTER, Linkage::Local, true, false)
        .map_err(e)?;
    let mut zero_word = DataDescription::new();
    zero_word.define_zeroinit(8);
    zero_word.set_align(8);
    m.define_data(live, &zero_word).map_err(e)?;
    let im = Imports {
        malloc: import(m, "malloc", 1, true)?,
        realloc: import(m, "realloc", 2, true)?,
        fault: local(m, "vesper_seq_fault", 1)?,
        live,
    };
    let exit = import(m, "exit", 1, false)?;
    let messages = if windows {
        None
    } else {
        let write = import(m, "write", 3, true)?;
        let mut ids = vec![];
        for (i, text) in MESSAGES.iter().enumerate() {
            let id = m
                .declare_data(
                    &format!("vesper_seq_fault_message_{i}"),
                    Linkage::Local,
                    false,
                    false,
                )
                .map_err(e)?;
            let mut d = DataDescription::new();
            d.define(text.as_bytes().to_vec().into_boxed_slice());
            m.define_data(id, &d).map_err(e)?;
            ids.push((id, text.len() as i64));
        }
        Some((write, ids))
    };
    compile(m, im.fault, 1, |b, m, args| {
        if let Some((write, ids)) = messages {
            // Message `code` (1: an interval fault, 2: a string interval off
            // a char boundary), else 0 (out of memory).
            let mut ptr = {
                let data = m.declare_data_in_func(ids[0].0, b.func);
                b.ins().symbol_value(types::I64, data)
            };
            let mut len = b.ins().iconst(types::I64, ids[0].1);
            for (code, (id, n)) in ids.iter().enumerate().skip(1) {
                let this = {
                    let data = m.declare_data_in_func(*id, b.func);
                    b.ins().symbol_value(types::I64, data)
                };
                let this_len = b.ins().iconst(types::I64, *n);
                let is = b.ins().icmp_imm(IntCC::Equal, args[0], code as i64);
                ptr = b.ins().select(is, this, ptr);
                len = b.ins().select(is, this_len, len);
            }
            let fd = b.ins().iconst(types::I64, 2);
            call(b, m, write, &[fd, ptr, len]);
        }
        let status = b.ins().iconst(types::I64, crate::seq::FAULT_STATUS);
        call(b, m, exit, &[status]);
        b.ins().trap(ir::TrapCode::unwrap_user(1));
        Ok(())
    })?;
    let memcpy = import(m, "memcpy", 3, true)?;
    let free = {
        let mut s = m.make_signature();
        s.params.push(AbiParam::new(types::I64));
        m.declare_function("free", Linkage::Import, &s).map_err(e)?
    };
    // `vesper_seq_new(cap, stride)`: the export ABI's sequence, a bare
    // header and (for `cap > 0`) a separate buffer.
    let new = local(m, "vesper_seq_new", 2)?;
    compile(m, new, 2, |b, m, args| {
        let (cap, stride) = (args[0], args[1]);
        let header_bytes = b.ins().iconst(types::I64, crate::seq::HEADER_BYTES);
        let header = call(b, m, im.malloc, &[header_bytes])[0];
        non_null(b, m, &im, header);
        count(b, m, &im, 1);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().store(MemFlags::new(), zero, header, 0);
        b.ins().store(MemFlags::new(), cap, header, 8);
        b.ins().store(MemFlags::new(), zero, header, 16);
        let empty = b.ins().icmp_imm(IntCC::Equal, cap, 0);
        let alloc = b.create_block();
        let done = b.create_block();
        b.ins().brif(empty, done, &[], alloc, &[]);
        b.switch_to_block(alloc);
        let bytes = checked_bytes(b, m, &im, cap, stride);
        let data = buffer(b, m, &im, bytes);
        b.ins().store(MemFlags::new(), data, header, 16);
        b.ins().jump(done, &[]);
        b.switch_to_block(done);
        b.ins().return_(&[header]);
        Ok(())
    })?;
    // `vesper_seq_make(cap, stride)`: generated code's sequence, one block
    // holding the header and, up to `INLINE_BYTES`, the elements.
    let make = local(m, "vesper_seq_make", 2)?;
    compile(m, make, 2, |b, m, args| {
        let (cap, stride) = (args[0], args[1]);
        let bytes = checked_bytes(b, m, &im, cap, stride);
        let fits = b
            .ins()
            .icmp_imm(IntCC::UnsignedLessThanOrEqual, bytes, INLINE_BYTES);
        let inline = b.create_block();
        let split = b.create_block();
        b.ins().brif(fits, inline, &[], split, &[]);
        b.switch_to_block(inline);
        let total = b.ins().iadd_imm(bytes, crate::seq::HEADER_BYTES);
        let header = call(b, m, im.malloc, &[total])[0];
        non_null(b, m, &im, header);
        count(b, m, &im, 1);
        let zero = b.ins().iconst(types::I64, 0);
        let after = b.ins().iadd_imm(header, crate::seq::HEADER_BYTES);
        let none = b.ins().icmp_imm(IntCC::Equal, bytes, 0);
        // No room: an empty header with no buffer, as `vesper_seq_new`.
        let data = b.ins().select(none, zero, after);
        b.ins().store(MemFlags::new(), zero, header, 0);
        b.ins().store(MemFlags::new(), cap, header, 8);
        b.ins().store(MemFlags::new(), data, header, 16);
        b.ins().return_(&[header]);
        b.switch_to_block(split);
        let r = m.declare_func_in_func(new, b.func);
        let inst = b.ins().call(r, &[cap, stride]);
        let header = b.inst_results(inst)[0];
        b.ins().return_(&[header]);
        Ok(())
    })?;
    let grow = local(m, "vesper_seq_grow", 3)?;
    compile(m, grow, 3, |b, m, args| {
        let (header, stride, needed) = (args[0], args[1], args[2]);
        let cap = b.ins().load(types::I64, MemFlags::new(), header, 8);
        let data = b.ins().load(types::I64, MemFlags::new(), header, 16);
        // new_cap = max(needed, max(4, cap * 2)), saturating.
        let two = b.ins().iconst(types::I64, 2);
        let (doubled, wrapped) = b.ins().umul_overflow(cap, two);
        let max = b.ins().iconst(types::I64, -1);
        let doubled = b.ins().select(wrapped, max, doubled);
        let four = b.ins().iconst(types::I64, 4);
        let floor = b.ins().umax(doubled, four);
        let new_cap = b.ins().umax(floor, needed);
        let bytes = checked_bytes(b, m, &im, new_cap, stride);
        let zero = b.ins().iconst(types::I64, 0);
        let owned = separate_buffer(b, header, cap, data);
        let resize = b.create_block();
        let fresh_block = b.create_block();
        let joined = b.create_block();
        b.append_block_param(joined, types::I64);
        b.ins().brif(owned, resize, &[], fresh_block, &[]);
        // A separate buffer this runtime allocated: resize it in place.
        b.switch_to_block(resize);
        let base = b.ins().iadd_imm(data, -BUFFER_PREFIX);
        let total = b.ins().iadd_imm(bytes, BUFFER_PREFIX);
        let resized = call(b, m, im.realloc, &[base, total])[0];
        non_null(b, m, &im, resized);
        let resized = b.ins().iadd_imm(resized, BUFFER_PREFIX);
        b.ins().jump(joined, &[resized.into()]);
        // No buffer, the header's inline elements, or borrowed bytes
        // (capacity 0 with data: a string literal or view, §13 strings):
        // a fresh buffer, with the old elements copied over.
        b.switch_to_block(fresh_block);
        let fresh = buffer(b, m, &im, bytes);
        let len = b.ins().load(types::I64, MemFlags::new(), header, 0);
        let one = b.ins().iconst(types::I64, 1);
        let unit = b.ins().umax(stride, one);
        let used = b.ins().imul(len, unit);
        let has_data = b.ins().icmp_imm(IntCC::NotEqual, data, 0);
        let copy = b.create_block();
        b.ins().brif(has_data, copy, &[], joined, &[fresh.into()]);
        b.switch_to_block(copy);
        call(b, m, memcpy, &[fresh, data, used]);
        b.ins().jump(joined, &[fresh.into()]);
        b.switch_to_block(joined);
        let grown = b.block_params(joined)[0];
        b.ins().store(MemFlags::new(), new_cap, header, 8);
        b.ins().store(MemFlags::new(), grown, header, 16);
        b.ins().return_(&[zero]);
        Ok(())
    })?;
    // `vesper_seq_free(header)`: free a header and the buffer it owns.
    let release = local(m, "vesper_seq_free", 1)?;
    compile(m, release, 1, |b, m, args| {
        let header = args[0];
        let cap = b.ins().load(types::I64, MemFlags::new(), header, 8);
        let data = b.ins().load(types::I64, MemFlags::new(), header, 16);
        let owned = separate_buffer(b, header, cap, data);
        let free_data = b.create_block();
        let done = b.create_block();
        b.ins().brif(owned, free_data, &[], done, &[]);
        b.switch_to_block(free_data);
        let base = b.ins().iadd_imm(data, -BUFFER_PREFIX);
        call(b, m, free, &[base]);
        count(b, m, &im, -1);
        b.ins().jump(done, &[]);
        b.switch_to_block(done);
        call(b, m, free, &[header]);
        count(b, m, &im, -1);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().return_(&[zero]);
        Ok(())
    })?;
    // `vesper_str_release(value)`: free the buffer a `String` value (stored
    // inline, not behind a header pointer) owns; borrowed bytes stay. A
    // string's bytes are never inline elements.
    let release_str = local(m, "vesper_str_release", 1)?;
    compile(m, release_str, 1, |b, m, args| {
        let value = args[0];
        let data = b.ins().load(types::I64, MemFlags::new(), value, 16);
        let cap = b.ins().load(types::I64, MemFlags::new(), value, 8);
        let present = b.ins().icmp_imm(IntCC::NotEqual, data, 0);
        let owned = b.ins().icmp_imm(IntCC::NotEqual, cap, 0);
        let has_data = b.ins().band(present, owned);
        let free_data = b.create_block();
        let done = b.create_block();
        b.ins().brif(has_data, free_data, &[], done, &[]);
        b.switch_to_block(free_data);
        let base = b.ins().iadd_imm(data, -BUFFER_PREFIX);
        call(b, m, free, &[base]);
        count(b, m, &im, -1);
        b.ins().jump(done, &[]);
        b.switch_to_block(done);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().return_(&[zero]);
        Ok(())
    })?;
    Ok(())
}

/// A separate element buffer of `bytes` bytes: [`BUFFER_PREFIX`] bytes
/// past the start of a `malloc` block, counted as one live allocation.
fn buffer<M: Module>(
    b: &mut FunctionBuilder,
    m: &mut M,
    im: &Imports,
    bytes: ir::Value,
) -> ir::Value {
    let total = b.ins().iadd_imm(bytes, BUFFER_PREFIX);
    let block = call(b, m, im.malloc, &[total])[0];
    non_null(b, m, im, block);
    count(b, m, im, 1);
    b.ins().iadd_imm(block, BUFFER_PREFIX)
}

/// Whether `{cap, data}` of the header at `header` is a separate buffer
/// this runtime owns: not borrowed (capacity 0), not absent, and not the
/// header's own inline elements (`data == header + 24`).
fn separate_buffer(
    b: &mut FunctionBuilder,
    header: ir::Value,
    cap: ir::Value,
    data: ir::Value,
) -> ir::Value {
    let present = b.ins().icmp_imm(IntCC::NotEqual, data, 0);
    let owned = b.ins().icmp_imm(IntCC::NotEqual, cap, 0);
    let after = b.ins().iadd_imm(header, crate::seq::HEADER_BYTES);
    let apart = b.ins().icmp(IntCC::NotEqual, data, after);
    let both = b.ins().band(present, owned);
    b.ins().band(both, apart)
}

/// Add `delta` to the live-allocation counter.
fn count<M: Module>(b: &mut FunctionBuilder, m: &mut M, im: &Imports, delta: i64) {
    let delta = b.ins().iconst(types::I64, delta);
    count_value(b, m, im, delta);
}

fn count_value<M: Module>(b: &mut FunctionBuilder, m: &mut M, im: &Imports, delta: ir::Value) {
    let data = m.declare_data_in_func(im.live, b.func);
    let address = b.ins().symbol_value(types::I64, data);
    b.ins().atomic_rmw(
        types::I64,
        MemFlags::trusted(),
        AtomicRmwOp::Add,
        address,
        delta,
    );
}

/// `cap * max(stride, 1)`, faulting on overflow past `isize::MAX`.
fn checked_bytes<M: Module>(
    b: &mut FunctionBuilder,
    m: &mut M,
    im: &Imports,
    cap: ir::Value,
    stride: ir::Value,
) -> ir::Value {
    let one = b.ins().iconst(types::I64, 1);
    let stride = b.ins().umax(stride, one);
    let (bytes, overflow) = b.ins().umul_overflow(cap, stride);
    let huge = b
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, bytes, i64::MAX);
    let bad = b.ins().bor(overflow, huge);
    let fail = b.create_block();
    let ok = b.create_block();
    b.ins().brif(bad, fail, &[], ok, &[]);
    b.switch_to_block(fail);
    let code = b.ins().iconst(types::I64, 0);
    call(b, m, im.fault, &[code]);
    b.ins().trap(ir::TrapCode::unwrap_user(1));
    b.switch_to_block(ok);
    bytes
}

fn non_null<M: Module>(b: &mut FunctionBuilder, m: &mut M, im: &Imports, ptr: ir::Value) {
    let null = b.ins().icmp_imm(IntCC::Equal, ptr, 0);
    let fail = b.create_block();
    let ok = b.create_block();
    b.ins().brif(null, fail, &[], ok, &[]);
    b.switch_to_block(fail);
    let code = b.ins().iconst(types::I64, 0);
    call(b, m, im.fault, &[code]);
    b.ins().trap(ir::TrapCode::unwrap_user(1));
    b.switch_to_block(ok);
}

fn call<M: Module>(
    b: &mut FunctionBuilder,
    m: &mut M,
    id: FuncId,
    args: &[ir::Value],
) -> Vec<ir::Value> {
    let r = m.declare_func_in_func(id, b.func);
    let inst = b.ins().call(r, args);
    b.inst_results(inst).to_vec()
}

fn compile<M: Module>(
    m: &mut M,
    id: FuncId,
    params: usize,
    build: impl FnOnce(&mut FunctionBuilder, &mut M, &[ir::Value]) -> Result<(), String>,
) -> Result<(), String> {
    let mut ctx = m.make_context();
    ctx.func
        .signature
        .params
        .extend((0..params).map(|_| AbiParam::new(types::I64)));
    ctx.func.signature.returns.push(AbiParam::new(types::I64));
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
