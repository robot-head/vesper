//! The region runtime of AOT executables, generated as Cranelift IR into the
//! object that uses it.
//!
//! `vesper build` links one object with the system C compiler, so the region
//! runtime cannot come from the `vesper` binary itself (the JIT uses the Rust
//! calls in [`crate::regions::native_runtime`]). Instead the first region
//! operation an object module lowers defines the same ten
//! `vesper_region_*` symbols as local functions of that object. They follow
//! the `vesper.region.accounting/1` rules of [`crate::regions::store`], which
//! the differential tests check against the evaluator and the JIT, and use
//! only `malloc`, `realloc`, `free`, `memset`, `memmove`, `write` and `exit`
//! from the C library.
//!
//! Layout. A region id is the address of a record of ten words:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | chunk base (0 until the first allocation) |
//! | 8 | capacity in bytes |
//! | 16 | bytes in use (the bump cursor) |
//! | 24, 32, 40 | slot array, length, capacity |
//! | 48, 56, 64 | watermark array, length, capacity |
//! | 72 | next serial |
//!
//! A slot is five words (start, length, prior cursor, serial, live) and a
//! watermark three (serial, slot count, cursor). A box handle is the slot
//! index in its low 21 bits and the slot's serial above them; a watermark
//! handle is its serial. Every access checks index, serial and liveness, so
//! a stale handle is a fault, never a read of reclaimed bytes.
//!
//! A fault prints `vesper: native region fault: ...` to standard error and
//! exits with status [`FAULT_STATUS`]. A region an abort unwinds past stays
//! allocated until the process exits, as in the JIT, which closes such
//! regions when the entry returns.
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, condcodes::IntCC, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, FuncId, Linkage, Module};

use crate::regions::store::{MAX_ALLOCATIONS, MAX_CHECKPOINTS};

/// Exit status of an executable after a region fault.
pub const FAULT_STATUS: i64 = 70;

const BASE: i32 = 0;
const CAP: i32 = 8;
const CURSOR: i32 = 16;
const SLOTS: i32 = 24;
const NSLOTS: i32 = 32;
const SLOTCAP: i32 = 40;
const MARKS: i32 = 48;
const NMARKS: i32 = 56;
const MARKCAP: i32 = 64;
const SERIAL: i32 = 72;
const RECORD: i64 = 80;

const S_START: i32 = 0;
const S_LEN: i32 = 8;
const S_PRIOR: i32 = 16;
const S_SERIAL: i32 = 24;
const S_LIVE: i32 = 32;
const SLOT: i64 = 40;

const M_SERIAL: i32 = 0;
const M_SLOTS: i32 = 8;
const M_CURSOR: i32 = 16;
const MARK: i64 = 24;

const INDEX_BITS: i64 = 21;
const MAX_SERIAL: i64 = 1 << (63 - INDEX_BITS);

#[derive(Clone, Copy)]
enum Fault {
    Exhausted,
    Allocations,
    Checkpoints,
    Stale,
    Bounds,
    Memory,
}
const FAULTS: [(Fault, &str); 6] = [
    (Fault::Exhausted, "region bound exhausted"),
    (
        Fault::Allocations,
        "region bound exhausted: too many allocations",
    ),
    (
        Fault::Checkpoints,
        "region bound exhausted: too many checkpoints",
    ),
    (
        Fault::Stale,
        "region handle is stale (rewound, reset or moved out)",
    ),
    (
        Fault::Bounds,
        "region handle does not belong to this region",
    ),
    (Fault::Memory, "out of memory"),
];

/// The runtime functions: name and number of `i64` parameters (each returns
/// one `i64`).
pub const FUNCTIONS: [(&str, usize); 10] = [
    ("vesper_region_open", 1),
    ("vesper_region_close", 1),
    ("vesper_region_alloc", 2),
    ("vesper_region_load", 3),
    ("vesper_region_store", 4),
    ("vesper_region_release", 2),
    ("vesper_region_checkpoint", 1),
    ("vesper_region_rewind", 2),
    ("vesper_region_commit", 2),
    ("vesper_region_used", 1),
];

/// Whether `m` already holds the runtime.
pub fn defined<M: Module>(m: &M) -> bool {
    m.get_name("vesper_region_fault").is_some()
}

struct Imports {
    malloc: FuncId,
    realloc: FuncId,
    free: FuncId,
    memset: FuncId,
    memmove: FuncId,
    write: FuncId,
    exit: FuncId,
    fault: FuncId,
    reclaim: FuncId,
    resolve: FuncId,
    find_mark: FuncId,
    messages: Vec<(cranelift_module::DataId, i64)>,
}

/// Defines every runtime function in `m` (once per module).
pub fn define<M: Module>(m: &mut M) -> Result<(), String> {
    if defined(m) {
        return Ok(());
    }
    if m.target_config().pointer_type() != types::I64 {
        return Err("source regions in executables need a 64-bit target".into());
    }
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
    let mut messages = vec![];
    for (i, (_, text)) in FAULTS.iter().enumerate() {
        let bytes = format!("vesper: native region fault: {text}\n").into_bytes();
        let id = m
            .declare_data(
                &format!("vesper_region_message{i}"),
                Linkage::Local,
                false,
                false,
            )
            .map_err(e)?;
        let len = bytes.len() as i64;
        let mut d = DataDescription::new();
        d.define(bytes.into_boxed_slice());
        m.define_data(id, &d).map_err(e)?;
        messages.push((id, len));
    }
    let imports = Imports {
        malloc: import(m, "malloc", 1, true)?,
        realloc: import(m, "realloc", 2, true)?,
        free: import(m, "free", 1, false)?,
        memset: import(m, "memset", 3, true)?,
        memmove: import(m, "memmove", 3, true)?,
        write: import(m, "write", 3, true)?,
        exit: import(m, "exit", 1, false)?,
        fault: local(m, "vesper_region_fault", 1, false)?,
        reclaim: local(m, "vesper_region_reclaim", 1, false)?,
        resolve: local(m, "vesper_region_resolve", 2, true)?,
        find_mark: local(m, "vesper_region_find_mark", 2, true)?,
        messages,
    };
    let mut context = m.make_context();
    let mut fctx = FunctionBuilderContext::new();
    let mut emit = |m: &mut M,
                    id: FuncId,
                    params: usize,
                    ret: bool,
                    body: &dyn Fn(&mut Gen<'_, M>, &[ir::Value])|
     -> Result<(), String> {
        context.func.signature = sig(m, params, ret);
        {
            let b = FunctionBuilder::new(&mut context.func, &mut fctx);
            let mut g = Gen {
                b,
                m,
                imports: &imports,
            };
            let entry = g.b.create_block();
            g.b.append_block_params_for_function_params(entry);
            g.b.switch_to_block(entry);
            let args = g.b.block_params(entry).to_vec();
            body(&mut g, &args);
            g.b.seal_all_blocks();
            g.b.finalize();
        }
        m.define_function(id, &mut context)
            .map_err(|err| format!("region runtime: {err:?}"))?;
        m.clear_context(&mut context);
        Ok(())
    };
    emit(m, imports.fault, 1, false, &|g, a| g.fault_body(a[0]))?;
    emit(m, imports.reclaim, 1, false, &|g, a| g.reclaim_body(a[0]))?;
    emit(m, imports.resolve, 2, true, &|g, a| {
        g.resolve_body(a[0], a[1])
    })?;
    emit(m, imports.find_mark, 2, true, &|g, a| {
        g.find_mark_body(a[0], a[1])
    })?;
    for (name, params) in FUNCTIONS {
        let id = local(m, name, params, true)?;
        emit(m, id, params, true, &|g, a| match name {
            "vesper_region_open" => g.open(a[0]),
            "vesper_region_close" => g.close(a[0]),
            "vesper_region_alloc" => g.alloc(a[0], a[1]),
            "vesper_region_load" => g.load_word(a[0], a[1], a[2]),
            "vesper_region_store" => g.store_word(a[0], a[1], a[2], a[3]),
            "vesper_region_release" => g.release(a[0], a[1]),
            "vesper_region_checkpoint" => g.checkpoint(a[0]),
            "vesper_region_rewind" => g.rewind(a[0], a[1]),
            "vesper_region_commit" => g.commit(a[0], a[1]),
            _ => g.used(a[0]),
        })?;
    }
    Ok(())
}

struct Gen<'a, M: Module> {
    b: FunctionBuilder<'a>,
    m: &'a mut M,
    imports: &'a Imports,
}

impl<M: Module> Gen<'_, M> {
    fn flags() -> MemFlags {
        MemFlags::trusted()
    }
    fn ld(&mut self, p: ir::Value, off: i32) -> ir::Value {
        self.b.ins().load(types::I64, Self::flags(), p, off)
    }
    fn st(&mut self, v: ir::Value, p: ir::Value, off: i32) {
        self.b.ins().store(Self::flags(), v, p, off);
    }
    fn k(&mut self, v: i64) -> ir::Value {
        self.b.ins().iconst(types::I64, v)
    }
    fn call(&mut self, f: FuncId, args: &[ir::Value]) -> Option<ir::Value> {
        let r = self.m.declare_func_in_func(f, self.b.func);
        let call = self.b.ins().call(r, args);
        self.b.inst_results(call).first().copied()
    }
    fn ret(&mut self, v: ir::Value) {
        self.b.ins().return_(&[v]);
    }
    fn ret0(&mut self) {
        let z = self.k(0);
        self.ret(z);
    }
    /// Continues in a fresh block when `cond` is false; reports `fault`
    /// otherwise.
    fn fault_if(&mut self, cond: ir::Value, fault: Fault) {
        let bad = self.b.create_block();
        let ok = self.b.create_block();
        self.b.ins().brif(cond, bad, &[], ok, &[]);
        self.b.switch_to_block(bad);
        let index = FAULTS
            .iter()
            .position(|(f, _)| std::mem::discriminant(f) == std::mem::discriminant(&fault))
            .expect("fault message");
        let k = self.k(index as i64);
        self.call(self.imports.fault, &[k]);
        self.b.ins().trap(ir::TrapCode::unwrap_user(5));
        self.b.switch_to_block(ok);
    }
    fn fault_if_zero(&mut self, v: ir::Value, fault: Fault) {
        let c = self.b.ins().icmp_imm(IntCC::Equal, v, 0);
        self.fault_if(c, fault);
    }
    /// `vesper_region_fault(message index)`: write, exit.
    fn fault_body(&mut self, index: ir::Value) {
        let mut blocks = vec![];
        for (i, (data, len)) in self.imports.messages.clone().into_iter().enumerate() {
            let next = self.b.create_block();
            let this = self.b.create_block();
            let c = self.b.ins().icmp_imm(IntCC::Equal, index, i as i64);
            self.b.ins().brif(c, this, &[], next, &[]);
            blocks.push((this, data, len));
            self.b.switch_to_block(next);
        }
        self.b.ins().trap(ir::TrapCode::unwrap_user(5));
        for (block, data, len) in blocks {
            self.b.switch_to_block(block);
            let gv = self.m.declare_data_in_func(data, self.b.func);
            let ptr = self.b.ins().symbol_value(types::I64, gv);
            let fd = self.k(2);
            let n = self.k(len);
            self.call(self.imports.write, &[fd, ptr, n]);
            let status = self.k(FAULT_STATUS);
            self.call(self.imports.exit, &[status]);
            self.b.ins().trap(ir::TrapCode::unwrap_user(5));
        }
    }
    fn var(&mut self, init: ir::Value) -> Variable {
        let v = self.b.declare_var(types::I64);
        self.b.def_var(v, init);
        v
    }
    /// Grows the array at `ptr_off`/`len_off`/`cap_off` of `rec` so that one
    /// more `entry`-byte element fits; returns the array base.
    fn reserve(
        &mut self,
        rec: ir::Value,
        ptr_off: i32,
        len_off: i32,
        cap_off: i32,
        entry: i64,
    ) -> ir::Value {
        let len = self.ld(rec, len_off);
        let cap = self.ld(rec, cap_off);
        let full = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, len, cap);
        let grow = self.b.create_block();
        let done = self.b.create_block();
        self.b.ins().brif(full, grow, &[], done, &[]);
        self.b.switch_to_block(grow);
        let doubled = self.b.ins().ishl_imm(cap, 1);
        let sixteen = self.k(16);
        let new_cap = self.b.ins().umax(doubled, sixteen);
        let bytes = self.b.ins().imul_imm(new_cap, entry);
        let old = self.ld(rec, ptr_off);
        let fresh = self
            .call(self.imports.realloc, &[old, bytes])
            .expect("realloc result");
        self.fault_if_zero(fresh, Fault::Memory);
        self.st(fresh, rec, ptr_off);
        self.st(new_cap, rec, cap_off);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        self.ld(rec, ptr_off)
    }

    fn open(&mut self, capacity: ir::Value) {
        let size = self.k(RECORD);
        let rec = self
            .call(self.imports.malloc, &[size])
            .expect("malloc result");
        self.fault_if_zero(rec, Fault::Memory);
        let zero = self.k(0);
        for off in (0..RECORD as i32).step_by(8) {
            self.st(zero, rec, off);
        }
        self.st(capacity, rec, CAP);
        let one = self.k(1);
        self.st(one, rec, SERIAL);
        self.ret(rec);
    }
    fn close(&mut self, rec: ir::Value) {
        for off in [BASE, SLOTS, MARKS] {
            let p = self.ld(rec, off);
            self.call(self.imports.free, &[p]);
        }
        self.call(self.imports.free, &[rec]);
        self.ret0();
    }
    fn alloc(&mut self, rec: ir::Value, size: ir::Value) {
        // slot_bytes: round up to whole words, at least one.
        let plus = self.b.ins().iadd_imm(size, 7);
        let rounded = self.b.ins().band_imm(plus, -8);
        let eight = self.k(8);
        let size = self.b.ins().umax(rounded, eight);
        let n = self.ld(rec, NSLOTS);
        let c = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, n, MAX_ALLOCATIONS as i64);
        self.fault_if(c, Fault::Allocations);
        let serial = self.ld(rec, SERIAL);
        let c = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, serial, MAX_SERIAL);
        self.fault_if(c, Fault::Allocations);
        let cap = self.ld(rec, CAP);
        let cursor = self.ld(rec, CURSOR);
        // Fits when cursor + size <= capacity (both below 2^31, no overflow).
        let end = self.b.ins().iadd(cursor, size);
        let c = self.b.ins().icmp(IntCC::UnsignedGreaterThan, end, cap);
        self.fault_if(c, Fault::Exhausted);
        // The chunk is obtained by the first allocation.
        let base = self.ld(rec, BASE);
        let none = self.b.ins().icmp_imm(IntCC::Equal, base, 0);
        let get = self.b.create_block();
        let have = self.b.create_block();
        self.b.ins().brif(none, get, &[], have, &[]);
        self.b.switch_to_block(get);
        let chunk = self
            .call(self.imports.malloc, &[cap])
            .expect("malloc result");
        self.fault_if_zero(chunk, Fault::Memory);
        self.st(chunk, rec, BASE);
        self.b.ins().jump(have, &[]);
        self.b.switch_to_block(have);
        let slots = self.reserve(rec, SLOTS, NSLOTS, SLOTCAP, SLOT);
        let off = self.b.ins().imul_imm(n, SLOT);
        let entry = self.b.ins().iadd(slots, off);
        self.st(cursor, entry, S_START);
        self.st(size, entry, S_LEN);
        self.st(cursor, entry, S_PRIOR);
        self.st(serial, entry, S_SERIAL);
        let one = self.k(1);
        self.st(one, entry, S_LIVE);
        let base = self.ld(rec, BASE);
        let at = self.b.ins().iadd(base, cursor);
        let zero = self.k(0);
        self.call(self.imports.memset, &[at, zero, size]);
        self.st(end, rec, CURSOR);
        let n1 = self.b.ins().iadd_imm(n, 1);
        self.st(n1, rec, NSLOTS);
        let s1 = self.b.ins().iadd_imm(serial, 1);
        self.st(s1, rec, SERIAL);
        let high = self.b.ins().ishl_imm(serial, INDEX_BITS);
        let handle = self.b.ins().bor(high, n);
        self.ret(handle);
    }
    /// `vesper_region_resolve(rec, handle)`: the live slot entry or a fault.
    fn resolve_body(&mut self, rec: ir::Value, handle: ir::Value) {
        let index = self.b.ins().band_imm(handle, (1 << INDEX_BITS) - 1);
        let serial = self.b.ins().ushr_imm(handle, INDEX_BITS);
        let n = self.ld(rec, NSLOTS);
        let c = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, index, n);
        self.fault_if(c, Fault::Stale);
        let slots = self.ld(rec, SLOTS);
        let off = self.b.ins().imul_imm(index, SLOT);
        let entry = self.b.ins().iadd(slots, off);
        let s = self.ld(entry, S_SERIAL);
        let c = self.b.ins().icmp(IntCC::NotEqual, s, serial);
        self.fault_if(c, Fault::Stale);
        let live = self.ld(entry, S_LIVE);
        self.fault_if_zero(live, Fault::Stale);
        self.ret(entry);
    }
    /// The address of word `index` of the payload `handle`.
    fn word(&mut self, rec: ir::Value, handle: ir::Value, index: ir::Value) -> ir::Value {
        let entry = self
            .call(self.imports.resolve, &[rec, handle])
            .expect("resolve result");
        let len = self.ld(entry, S_LEN);
        let words = self.b.ins().ushr_imm(len, 3);
        let c = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, index, words);
        self.fault_if(c, Fault::Bounds);
        let start = self.ld(entry, S_START);
        let base = self.ld(rec, BASE);
        let at = self.b.ins().iadd(base, start);
        let off = self.b.ins().ishl_imm(index, 3);
        self.b.ins().iadd(at, off)
    }
    fn load_word(&mut self, rec: ir::Value, handle: ir::Value, index: ir::Value) {
        let at = self.word(rec, handle, index);
        let v = self.ld(at, 0);
        self.ret(v);
    }
    fn store_word(
        &mut self,
        rec: ir::Value,
        handle: ir::Value,
        index: ir::Value,
        value: ir::Value,
    ) {
        let at = self.word(rec, handle, index);
        self.st(value, at, 0);
        self.ret0();
    }
    fn release(&mut self, rec: ir::Value, handle: ir::Value) {
        let entry = self
            .call(self.imports.resolve, &[rec, handle])
            .expect("resolve result");
        let zero = self.k(0);
        self.st(zero, entry, S_LIVE);
        self.call(self.imports.reclaim, &[rec]);
        self.ret0();
    }
    /// `vesper_region_reclaim(rec)`: pops moved payloads at the unprotected
    /// tip so that their bytes are reused.
    fn reclaim_body(&mut self, rec: ir::Value) {
        let head = self.b.create_block();
        let done = self.b.create_block();
        self.b.ins().jump(head, &[]);
        self.b.switch_to_block(head);
        let n = self.ld(rec, NSLOTS);
        let empty = self.b.ins().icmp_imm(IntCC::Equal, n, 0);
        let step = |g: &mut Self, cond: ir::Value| {
            let next = g.b.create_block();
            g.b.ins().brif(cond, done, &[], next, &[]);
            g.b.switch_to_block(next);
        };
        step(self, empty);
        let last = self.b.ins().iadd_imm(n, -1);
        let slots = self.ld(rec, SLOTS);
        let off = self.b.ins().imul_imm(last, SLOT);
        let entry = self.b.ins().iadd(slots, off);
        let live = self.ld(entry, S_LIVE);
        let is_live = self.b.ins().icmp_imm(IntCC::NotEqual, live, 0);
        step(self, is_live);
        // The newest watermark protects the slots it recorded.
        let m = self.ld(rec, NMARKS);
        let marks = self.ld(rec, MARKS);
        let has_mark = self.b.ins().icmp_imm(IntCC::NotEqual, m, 0);
        let with_mark = self.b.create_block();
        let floor_known = self.b.create_block();
        self.b.append_block_param(floor_known, types::I64);
        let zero = self.k(0);
        self.b
            .ins()
            .brif(has_mark, with_mark, &[], floor_known, &[zero.into()]);
        self.b.switch_to_block(with_mark);
        let top = self.b.ins().iadd_imm(m, -1);
        let moff = self.b.ins().imul_imm(top, MARK);
        let mark = self.b.ins().iadd(marks, moff);
        let f = self.ld(mark, M_SLOTS);
        self.b.ins().jump(floor_known, &[f.into()]);
        self.b.switch_to_block(floor_known);
        let floor = self.b.block_params(floor_known)[0];
        let below = self.b.ins().icmp(IntCC::UnsignedLessThan, last, floor);
        step(self, below);
        let start = self.ld(entry, S_START);
        let len = self.ld(entry, S_LEN);
        let end = self.b.ins().iadd(start, len);
        let cursor = self.ld(rec, CURSOR);
        let not_tip = self.b.ins().icmp(IntCC::NotEqual, cursor, end);
        step(self, not_tip);
        self.st(last, rec, NSLOTS);
        let prior = self.ld(entry, S_PRIOR);
        self.st(prior, rec, CURSOR);
        self.b.ins().jump(head, &[]);
        self.b.switch_to_block(done);
        self.b.ins().return_(&[]);
    }
    fn checkpoint(&mut self, rec: ir::Value) {
        let m = self.ld(rec, NMARKS);
        let c = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, m, MAX_CHECKPOINTS as i64);
        self.fault_if(c, Fault::Checkpoints);
        let marks = self.reserve(rec, MARKS, NMARKS, MARKCAP, MARK);
        let off = self.b.ins().imul_imm(m, MARK);
        let entry = self.b.ins().iadd(marks, off);
        let serial = self.ld(rec, SERIAL);
        self.st(serial, entry, M_SERIAL);
        let n = self.ld(rec, NSLOTS);
        self.st(n, entry, M_SLOTS);
        let cursor = self.ld(rec, CURSOR);
        self.st(cursor, entry, M_CURSOR);
        let m1 = self.b.ins().iadd_imm(m, 1);
        self.st(m1, rec, NMARKS);
        let s1 = self.b.ins().iadd_imm(serial, 1);
        self.st(s1, rec, SERIAL);
        self.ret(serial);
    }
    /// `vesper_region_find_mark(rec, serial)`: the newest watermark position
    /// with that serial, or a fault.
    fn find_mark_body(&mut self, rec: ir::Value, serial: ir::Value) {
        let m = self.ld(rec, NMARKS);
        let i = self.var(m);
        let head = self.b.create_block();
        self.b.ins().jump(head, &[]);
        self.b.switch_to_block(head);
        let at = self.b.use_var(i);
        self.fault_if_zero(at, Fault::Stale);
        let pos = self.b.ins().iadd_imm(at, -1);
        self.b.def_var(i, pos);
        let marks = self.ld(rec, MARKS);
        let off = self.b.ins().imul_imm(pos, MARK);
        let entry = self.b.ins().iadd(marks, off);
        let s = self.ld(entry, M_SERIAL);
        let hit = self.b.ins().icmp(IntCC::Equal, s, serial);
        let found = self.b.create_block();
        self.b.ins().brif(hit, found, &[], head, &[]);
        self.b.switch_to_block(found);
        self.ret(pos);
    }
    fn mark_at(&mut self, rec: ir::Value, pos: ir::Value) -> ir::Value {
        let marks = self.ld(rec, MARKS);
        let off = self.b.ins().imul_imm(pos, MARK);
        self.b.ins().iadd(marks, off)
    }
    fn rewind(&mut self, rec: ir::Value, serial: ir::Value) {
        let pos = self
            .call(self.imports.find_mark, &[rec, serial])
            .expect("find result");
        let entry = self.mark_at(rec, pos);
        let slots = self.ld(entry, M_SLOTS);
        let cursor = self.ld(entry, M_CURSOR);
        self.st(pos, rec, NMARKS);
        self.st(slots, rec, NSLOTS);
        self.st(cursor, rec, CURSOR);
        self.ret0();
    }
    fn commit(&mut self, rec: ir::Value, serial: ir::Value) {
        let pos = self
            .call(self.imports.find_mark, &[rec, serial])
            .expect("find result");
        let entry = self.mark_at(rec, pos);
        let m = self.ld(rec, NMARKS);
        let after = self.b.ins().isub(m, pos);
        let after = self.b.ins().iadd_imm(after, -1);
        let bytes = self.b.ins().imul_imm(after, MARK);
        let next = self.b.ins().iadd_imm(entry, MARK);
        self.call(self.imports.memmove, &[entry, next, bytes]);
        let m1 = self.b.ins().iadd_imm(m, -1);
        self.st(m1, rec, NMARKS);
        self.call(self.imports.reclaim, &[rec]);
        self.ret0();
    }
    fn used(&mut self, rec: ir::Value) {
        let v = self.ld(rec, CURSOR);
        self.ret(v);
    }
}
