//! The freestanding runtime, generated as Cranelift IR: no allocator, no
//! scheduler, no libc.
//!
//! * `vesper_freestanding_main` calls the entry, prints
//!   `vesper: <entry>() = <value>` and a newline on the UART, and calls
//!   `vesper_board_exit(0)`. When the entry may fail, it passes a failure
//!   record and executes an abort trap if the record's tag is set.
//! * `vesper_freestanding_trap(cause, epc, tval)` is what the stub's trap
//!   vector calls. It looks `epc` up in the image's trap table (the trap
//!   sites Cranelift emitted for backend aborts), prints
//!   `vesper: abort: <reason> pc=0x… cause=0x… tval=0x…` and exits with
//!   [`super::EXIT_ABORT`], or with [`super::EXIT_EXCEPTION`] when the PC is
//!   not an abort site.
//!
//! The trap table is a read-only object, `count: u64` followed by
//! `{ pc: u64, message: *const u8, len: u64 }` entries. The message pointers
//! are relocations; the PCs are filled after linking, when the final
//! addresses of the backend's local functions are known.

use std::sync::Arc;

use cranelift_codegen::ir::{
    self, AbiParam, InstBuilder, MemFlags, UserExternalName, condcodes::IntCC, types::I64,
};
use cranelift_codegen::isa::TargetIsa;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

use super::stub::EXIT_SYMBOL;
use super::{
    Board, EXIT_ABORT, EXIT_EXCEPTION, FreestandingError, FreestandingTarget, ResultKind, Uart,
};

pub const MAIN_SYMBOL: &str = "vesper_freestanding_main";
pub const TRAP_SYMBOL: &str = "vesper_freestanding_trap";
pub const TRAP_TABLE: &str = "vesper_trap_table";
pub const TRAP_HEADER_BYTES: u64 = 8;
pub const TRAP_ENTRY_BYTES: u64 = 24;

/// Namespaces of the function names handed to Silicon: the backend's
/// functions keep `u0:<FuncId>`, runtime functions are `u1:<FuncId>` and the
/// stub's `vesper_board_exit` is the external `u2:0`.
pub const BACKEND_NAMESPACE: u32 = 0;
pub const RUNTIME_NAMESPACE: u32 = 1;
pub const STUB_NAMESPACE: u32 = 2;

/// Offsets of a trap-table entry's `message` and `len` fields (`pc` is at 0).
const TRAP_ENTRY_MESSAGE: u64 = 8;
const TRAP_ENTRY_LEN: u64 = 16;

/// NS16550 line status register offset and its transmit-holding-empty bit.
const NS16550_LSR: u64 = 5;
const NS16550_LSR_THRE: i64 = 0x20;
/// PL011 flag register offset and its transmit-FIFO-full bit.
const PL011_FR: u64 = 0x18;
const PL011_FR_TXFF: i64 = 0x20;

/// `put_udec`'s digit buffer: the 20 digits of `u64::MAX`, in whole words.
const UDEC_BUFFER_BYTES: u32 = 24;
/// log2 of the alignment of the runtime's stack slots (8 bytes).
const STACK_SLOT_ALIGN_SHIFT: u8 = 3;
/// `put_hex` prints the 16 nibbles of a `u64` from the top one down.
const NIBBLE_BITS: i64 = 4;
const NIBBLE_MASK: i64 = 0xf;
const HEX_FIRST_SHIFT: i64 = 64 - NIBBLE_BITS;

/// Text the runtime prints.
const TRUE_TEXT: &[u8] = b"true";
const FALSE_TEXT: &[u8] = b"false";
const UNIT_TEXT: &[u8] = b"()";
const NEWLINE_TEXT: &[u8] = b"\n";
const ABORT_TEXT: &[u8] = b"vesper: abort: ";
const UNEXPECTED_TEXT: &[u8] = b"unexpected exception";
const PC_TEXT: &[u8] = b" pc=0x";
const CAUSE_TEXT: &[u8] = b" cause=0x";
const TVAL_TEXT: &[u8] = b" tval=0x";

pub struct RuntimeSpec {
    pub target: FreestandingTarget,
    pub isa: Arc<dyn TargetIsa>,
    pub board: Board,
    pub entry: String,
    pub entry_func_id: u32,
    pub result: ResultKind,
    /// The entry may fail: the runtime passes a failure record and turns a
    /// set tag into an abort trap (`user(ABORT_TRAP_BASE + i)`).
    pub fail: Option<super::FailEntry>,
    pub trap_messages: Vec<String>,
    /// Message index of each trap-table entry, in table order.
    pub trap_entries: Vec<usize>,
}

pub struct Runtime {
    pub object: Vec<u8>,
    /// The runtime's functions as Silicon sees them (renamed, see
    /// [`RUNTIME_NAMESPACE`]), with their object symbols.
    pub functions: Vec<(String, ir::Function)>,
    pub main: String,
    pub trap: String,
    pub exit: String,
    /// Trap sites of the runtime's own code: `(symbol, offset, code)`.
    pub traps: Vec<(String, u64, ir::TrapCode)>,
}

struct Ids {
    putc: FuncId,
    puts: FuncId,
    put_udec: FuncId,
    put_hex: FuncId,
    exit: FuncId,
}

fn sig(m: &ObjectModule, params: usize, returns: usize) -> ir::Signature {
    let mut s = m.make_signature();
    for _ in 0..params {
        s.params.push(AbiParam::new(I64));
    }
    for _ in 0..returns {
        s.returns.push(AbiParam::new(I64));
    }
    s
}

fn codegen<E: std::fmt::Display>(e: E) -> FreestandingError {
    FreestandingError::Codegen(e.to_string())
}

struct Gen {
    m: ObjectModule,
    strings: Vec<(Vec<u8>, DataId)>,
    defined: Vec<(String, FuncId, ir::Function)>,
}

impl Gen {
    fn string(&mut self, s: &[u8]) -> Result<DataId, FreestandingError> {
        if let Some((_, id)) = self.strings.iter().find(|(b, _)| b == s) {
            return Ok(*id);
        }
        let name = format!("vesper_rt_str_{}", self.strings.len());
        let id = self
            .m
            .declare_data(&name, Linkage::Local, false, false)
            .map_err(codegen)?;
        let mut d = DataDescription::new();
        d.define(s.to_vec().into_boxed_slice());
        self.m.define_data(id, &d).map_err(codegen)?;
        self.strings.push((s.to_vec(), id));
        Ok(id)
    }

    fn define(
        &mut self,
        name: &str,
        id: FuncId,
        signature: ir::Signature,
        body: impl FnOnce(
            &mut FunctionBuilder,
            &mut ObjectModule,
            &[ir::Value],
        ) -> Result<(), FreestandingError>,
    ) -> Result<(), FreestandingError> {
        let mut ctx = self.m.make_context();
        ctx.func.signature = signature;
        ctx.func.name = ir::UserFuncName::user(BACKEND_NAMESPACE, id.as_u32());
        let mut fctx = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let params = b.block_params(entry).to_vec();
            body(&mut b, &mut self.m, &params)?;
            b.seal_all_blocks();
            b.finalize();
        }
        let func = ctx.func.clone();
        self.m
            .define_function(id, &mut ctx)
            .map_err(|e| FreestandingError::Codegen(format!("runtime {name}: {e:?}")))?;
        self.defined.push((name.to_string(), id, func));
        Ok(())
    }

    /// `puts(ptr, len)` of a string literal.
    fn emit_puts(
        b: &mut FunctionBuilder,
        m: &mut ObjectModule,
        puts: FuncId,
        data: DataId,
        len: usize,
    ) {
        let ptr = data_address(b, m, data);
        let n = b.ins().iconst(I64, len as i64);
        let f = m.declare_func_in_func(puts, b.func);
        b.ins().call(f, &[ptr, n]);
    }
}

/// `symbol_value` of a runtime data object as an absolute 64-bit literal.
/// Cranelift's RISC-V `LoadExtNameNear` (colocated data) uses
/// `R_RISCV_PCREL_HI20`, which `cranelift-object` 0.125 cannot emit, so the
/// reference is marked non-colocated (`auipc; ld; .8byte sym`, `R_RISCV_64`).
fn data_address(b: &mut FunctionBuilder, m: &mut ObjectModule, data: DataId) -> ir::Value {
    let gv = m.declare_data_in_func(data, b.func);
    if let ir::GlobalValueData::Symbol { colocated, .. } = &mut b.func.global_values[gv] {
        *colocated = false;
    }
    b.ins().symbol_value(I64, gv)
}

fn call(b: &mut FunctionBuilder, m: &mut ObjectModule, id: FuncId, args: &[ir::Value]) -> ir::Inst {
    let f = m.declare_func_in_func(id, b.func);
    b.ins().call(f, args)
}

pub fn build(spec: &RuntimeSpec) -> Result<Runtime, FreestandingError> {
    let builder = ObjectBuilder::new(
        spec.isa.clone(),
        "vesper-freestanding-runtime",
        cranelift_module::default_libcall_names(),
    )
    .map_err(codegen)?;
    let mut g = Gen {
        m: ObjectModule::new(builder),
        strings: Vec::new(),
        defined: Vec::new(),
    };
    let s1 = sig(&g.m, 1, 0);
    let s2 = sig(&g.m, 2, 0);
    let s3 = sig(&g.m, 3, 0);
    let s0 = sig(&g.m, 0, 0);
    let entry_sig = sig(&g.m, usize::from(spec.fail.is_some()), 1);
    let declare = |m: &mut ObjectModule, name: &str, linkage: Linkage, s: &ir::Signature| {
        m.declare_function(name, linkage, s).map_err(codegen)
    };
    let ids = Ids {
        putc: declare(&mut g.m, "vesper_rt_putc", Linkage::Local, &s1)?,
        puts: declare(&mut g.m, "vesper_rt_puts", Linkage::Local, &s2)?,
        put_udec: declare(&mut g.m, "vesper_rt_put_udec", Linkage::Local, &s1)?,
        put_hex: declare(&mut g.m, "vesper_rt_put_hex", Linkage::Local, &s1)?,
        exit: declare(&mut g.m, EXIT_SYMBOL, Linkage::Import, &s1)?,
    };
    let entry_id = declare(&mut g.m, &spec.entry, Linkage::Import, &entry_sig)?;
    let main_id = declare(&mut g.m, MAIN_SYMBOL, Linkage::Export, &s0)?;
    let trap_id = declare(&mut g.m, TRAP_SYMBOL, Linkage::Export, &s3)?;

    // putc(byte)
    let uart = spec.board.uart;
    g.define("vesper_rt_putc", ids.putc, s1.clone(), |b, _, p| {
        let byte = p[0];
        let wait = b.create_block();
        let send = b.create_block();
        b.ins().jump(wait, &[]);
        b.switch_to_block(wait);
        let flags = MemFlags::new();
        match uart {
            Uart::Ns16550 { base } => {
                let lsr = b.ins().iconst(I64, (base + NS16550_LSR) as i64);
                let s = b.ins().uload8(I64, flags, lsr, 0);
                let empty = b.ins().band_imm(s, NS16550_LSR_THRE);
                b.ins().brif(empty, send, &[], wait, &[]);
                b.switch_to_block(send);
                let thr = b.ins().iconst(I64, base as i64);
                b.ins().istore8(flags, byte, thr, 0);
            }
            Uart::Pl011 { base } => {
                let fr = b.ins().iconst(I64, (base + PL011_FR) as i64);
                let s = b.ins().uload32(flags, fr, 0);
                let full = b.ins().band_imm(s, PL011_FR_TXFF);
                b.ins().brif(full, wait, &[], send, &[]);
                b.switch_to_block(send);
                let dr = b.ins().iconst(I64, base as i64);
                b.ins().istore32(flags, byte, dr, 0);
            }
        }
        b.ins().return_(&[]);
        Ok(())
    })?;

    // puts(ptr, len)
    let putc = ids.putc;
    g.define("vesper_rt_puts", ids.puts, s2.clone(), |b, m, p| {
        let head = b.create_block();
        let body = b.create_block();
        let done = b.create_block();
        b.append_block_param(head, I64);
        b.append_block_param(head, I64);
        b.ins().jump(head, &[p[0].into(), p[1].into()]);
        b.switch_to_block(head);
        let (ptr, len) = (b.block_params(head)[0], b.block_params(head)[1]);
        b.ins().brif(len, body, &[], done, &[]);
        b.switch_to_block(body);
        let c = b.ins().uload8(I64, MemFlags::new(), ptr, 0);
        call(b, m, putc, &[c]);
        let ptr1 = b.ins().iadd_imm(ptr, 1);
        let len1 = b.ins().iadd_imm(len, -1);
        b.ins().jump(head, &[ptr1.into(), len1.into()]);
        b.switch_to_block(done);
        b.ins().return_(&[]);
        Ok(())
    })?;

    // put_udec(value)
    let puts = ids.puts;
    g.define("vesper_rt_put_udec", ids.put_udec, s1.clone(), |b, m, p| {
        let slot = b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            UDEC_BUFFER_BYTES,
            STACK_SLOT_ALIGN_SHIFT,
        ));
        let end = b.ins().stack_addr(I64, slot, UDEC_BUFFER_BYTES as i32);
        let lp = b.create_block();
        let out = b.create_block();
        b.append_block_param(lp, I64);
        b.append_block_param(lp, I64);
        b.append_block_param(out, I64);
        b.ins().jump(lp, &[p[0].into(), end.into()]);
        b.switch_to_block(lp);
        let (v, at) = (b.block_params(lp)[0], b.block_params(lp)[1]);
        let ten = b.ins().iconst(I64, 10);
        let q = b.ins().udiv(v, ten);
        let qt = b.ins().imul(q, ten);
        let r = b.ins().isub(v, qt);
        let digit = b.ins().iadd_imm(r, i64::from(b'0'));
        let at1 = b.ins().iadd_imm(at, -1);
        b.ins().istore8(MemFlags::new(), digit, at1, 0);
        b.ins()
            .brif(q, lp, &[q.into(), at1.into()], out, &[at1.into()]);
        b.switch_to_block(out);
        let start = b.block_params(out)[0];
        let end = b.ins().stack_addr(I64, slot, UDEC_BUFFER_BYTES as i32);
        let len = b.ins().isub(end, start);
        call(b, m, puts, &[start, len]);
        b.ins().return_(&[]);
        Ok(())
    })?;

    // put_hex(value): 16 digits.
    g.define("vesper_rt_put_hex", ids.put_hex, s1.clone(), |b, m, p| {
        let lp = b.create_block();
        let done = b.create_block();
        b.append_block_param(lp, I64);
        let sixty = b.ins().iconst(I64, HEX_FIRST_SHIFT);
        b.ins().jump(lp, &[sixty.into()]);
        b.switch_to_block(lp);
        let shift = b.block_params(lp)[0];
        let v = b.ins().ushr(p[0], shift);
        let nib = b.ins().band_imm(v, NIBBLE_MASK);
        let is_digit = b.ins().icmp_imm(IntCC::UnsignedLessThan, nib, 10);
        let as_digit = b.ins().iadd_imm(nib, i64::from(b'0'));
        let as_letter = b.ins().iadd_imm(nib, i64::from(b'a') - 10);
        let c = b.ins().select(is_digit, as_digit, as_letter);
        call(b, m, putc, &[c]);
        let next = b.ins().iadd_imm(shift, -NIBBLE_BITS);
        let more = b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, next, 0);
        b.ins().brif(more, lp, &[next.into()], done, &[]);
        b.switch_to_block(done);
        b.ins().return_(&[]);
        Ok(())
    })?;

    // main
    let prefix = format!("vesper: {}() = ", spec.entry).into_bytes();
    let prefix_id = g.string(&prefix)?;
    let true_id = g.string(TRUE_TEXT)?;
    let false_id = g.string(FALSE_TEXT)?;
    let unit_id = g.string(UNIT_TEXT)?;
    let newline_id = g.string(NEWLINE_TEXT)?;
    let (put_udec, put_hex, exit) = (ids.put_udec, ids.put_hex, ids.exit);
    let kind = spec.result;
    let fail = spec.fail.clone();
    g.define(MAIN_SYMBOL, main_id, s0.clone(), |b, m, _| {
        let r = if let Some(fail) = &fail {
            let slot = b.create_sized_stack_slot(ir::StackSlotData::new(
                ir::StackSlotKind::ExplicitSlot,
                fail.record_bytes,
                STACK_SLOT_ALIGN_SHIFT,
            ));
            let record = b.ins().stack_addr(I64, slot, 0);
            let zero = b.ins().iconst(I64, 0);
            b.ins().store(MemFlags::trusted(), zero, record, 0);
            let inst = call(b, m, entry_id, &[record]);
            let r = b.inst_results(inst)[0];
            let tag = b.ins().load(I64, MemFlags::trusted(), record, 0);
            let ok = b.create_block();
            let failed = b.create_block();
            b.ins().brif(tag, failed, &[], ok, &[]);
            b.switch_to_block(failed);
            // An escaping failure is an abort: one trap per instance, then
            // one for an unknown tag. The trap handler names it.
            for i in 0..fail.instances.len() {
                let hit = b.create_block();
                let next = b.create_block();
                let is = b.ins().icmp_imm(IntCC::Equal, tag, i as i64 + 1);
                b.ins().brif(is, hit, &[], next, &[]);
                b.switch_to_block(hit);
                b.ins()
                    .trap(ir::TrapCode::unwrap_user(super::ABORT_TRAP_BASE + i as u8));
                b.switch_to_block(next);
            }
            b.ins().trap(ir::TrapCode::unwrap_user(
                super::ABORT_TRAP_BASE + fail.instances.len() as u8,
            ));
            b.switch_to_block(ok);
            r
        } else {
            let inst = call(b, m, entry_id, &[]);
            b.inst_results(inst)[0]
        };
        Gen::emit_puts(b, m, puts, prefix_id, prefix.len());
        let done = b.create_block();
        match kind {
            ResultKind::Signed => {
                let neg = b.create_block();
                let pos = b.create_block();
                b.append_block_param(pos, I64);
                let is_neg = b.ins().icmp_imm(IntCC::SignedLessThan, r, 0);
                b.ins().brif(is_neg, neg, &[], pos, &[r.into()]);
                b.switch_to_block(neg);
                let minus = b.ins().iconst(I64, i64::from(b'-'));
                call(b, m, putc, &[minus]);
                let abs = b.ins().ineg(r);
                b.ins().jump(pos, &[abs.into()]);
                b.switch_to_block(pos);
                let v = b.block_params(pos)[0];
                call(b, m, put_udec, &[v]);
                b.ins().jump(done, &[]);
            }
            ResultKind::Unsigned(bits) => {
                let v = if bits < 64 {
                    b.ins().band_imm(r, ((1u64 << bits) - 1) as i64)
                } else {
                    r
                };
                call(b, m, put_udec, &[v]);
                b.ins().jump(done, &[]);
            }
            ResultKind::Bool => {
                let t = b.create_block();
                let f = b.create_block();
                b.ins().brif(r, t, &[], f, &[]);
                b.switch_to_block(t);
                Gen::emit_puts(b, m, puts, true_id, TRUE_TEXT.len());
                b.ins().jump(done, &[]);
                b.switch_to_block(f);
                Gen::emit_puts(b, m, puts, false_id, FALSE_TEXT.len());
                b.ins().jump(done, &[]);
            }
            ResultKind::Unit => {
                Gen::emit_puts(b, m, puts, unit_id, UNIT_TEXT.len());
                b.ins().jump(done, &[]);
            }
        }
        b.switch_to_block(done);
        Gen::emit_puts(b, m, puts, newline_id, NEWLINE_TEXT.len());
        let ok = b.ins().iconst(I64, 0);
        call(b, m, exit, &[ok]);
        b.ins().return_(&[]);
        Ok(())
    })?;

    // Trap table and handler.
    let mut message_ids = Vec::new();
    for msg in &spec.trap_messages {
        message_ids.push(g.string(msg.as_bytes())?);
    }
    let table_id =
        g.m.declare_data(TRAP_TABLE, Linkage::Local, false, false)
            .map_err(codegen)?;
    let mut table = DataDescription::new();
    let n = spec.trap_entries.len() as u64;
    let mut bytes = vec![0u8; (TRAP_HEADER_BYTES + n * TRAP_ENTRY_BYTES) as usize];
    bytes[..8].copy_from_slice(&n.to_le_bytes());
    for (i, &msg) in spec.trap_entries.iter().enumerate() {
        let at = (TRAP_HEADER_BYTES + i as u64 * TRAP_ENTRY_BYTES) as usize;
        let len = spec.trap_messages[msg].len() as u64;
        bytes[at + TRAP_ENTRY_LEN as usize..at + TRAP_ENTRY_BYTES as usize]
            .copy_from_slice(&len.to_le_bytes());
    }
    table.define(bytes.into_boxed_slice());
    table.set_align(8);
    for (i, &msg) in spec.trap_entries.iter().enumerate() {
        let gv = g.m.declare_data_in_data(message_ids[msg], &mut table);
        let at = (TRAP_HEADER_BYTES + i as u64 * TRAP_ENTRY_BYTES + TRAP_ENTRY_MESSAGE) as u32;
        table.write_data_addr(at, gv, 0);
    }
    g.m.define_data(table_id, &table).map_err(codegen)?;
    let abort_id = g.string(ABORT_TEXT)?;
    let unexpected_id = g.string(UNEXPECTED_TEXT)?;
    let pc_id = g.string(PC_TEXT)?;
    let cause_id = g.string(CAUSE_TEXT)?;
    let tval_id = g.string(TVAL_TEXT)?;
    g.define(TRAP_SYMBOL, trap_id, s3, |b, m, p| {
        let (cause, epc, tval) = (p[0], p[1], p[2]);
        Gen::emit_puts(b, m, puts, abort_id, ABORT_TEXT.len());
        let table = data_address(b, m, table_id);
        let count = b.ins().load(I64, MemFlags::trusted(), table, 0);
        let head = b.create_block();
        let check = b.create_block();
        let next = b.create_block();
        let found = b.create_block();
        let missing = b.create_block();
        let tail = b.create_block();
        b.append_block_param(head, I64);
        b.append_block_param(tail, I64);
        let zero = b.ins().iconst(I64, 0);
        b.ins().jump(head, &[zero.into()]);
        b.switch_to_block(head);
        let i = b.block_params(head)[0];
        let more = b.ins().icmp(IntCC::UnsignedLessThan, i, count);
        b.ins().brif(more, check, &[], missing, &[]);
        b.switch_to_block(check);
        let off = b.ins().imul_imm(i, TRAP_ENTRY_BYTES as i64);
        let entry = b.ins().iadd(table, off);
        let pc = b
            .ins()
            .load(I64, MemFlags::trusted(), entry, TRAP_HEADER_BYTES as i32);
        let hit = b.ins().icmp(IntCC::Equal, pc, epc);
        b.ins().brif(hit, found, &[], next, &[]);
        b.switch_to_block(next);
        let i1 = b.ins().iadd_imm(i, 1);
        b.ins().jump(head, &[i1.into()]);
        b.switch_to_block(found);
        let msg = b.ins().load(
            I64,
            MemFlags::trusted(),
            entry,
            (TRAP_HEADER_BYTES + TRAP_ENTRY_MESSAGE) as i32,
        );
        let len = b.ins().load(
            I64,
            MemFlags::trusted(),
            entry,
            (TRAP_HEADER_BYTES + TRAP_ENTRY_LEN) as i32,
        );
        call(b, m, puts, &[msg, len]);
        let code = b.ins().iconst(I64, i64::from(EXIT_ABORT));
        b.ins().jump(tail, &[code.into()]);
        b.switch_to_block(missing);
        Gen::emit_puts(b, m, puts, unexpected_id, UNEXPECTED_TEXT.len());
        let code = b.ins().iconst(I64, i64::from(EXIT_EXCEPTION));
        b.ins().jump(tail, &[code.into()]);
        b.switch_to_block(tail);
        let status = b.block_params(tail)[0];
        Gen::emit_puts(b, m, puts, pc_id, PC_TEXT.len());
        call(b, m, put_hex, &[epc]);
        Gen::emit_puts(b, m, puts, cause_id, CAUSE_TEXT.len());
        call(b, m, put_hex, &[cause]);
        Gen::emit_puts(b, m, puts, tval_id, TVAL_TEXT.len());
        call(b, m, put_hex, &[tval]);
        Gen::emit_puts(b, m, puts, newline_id, NEWLINE_TEXT.len());
        call(b, m, exit, &[status]);
        b.ins().return_(&[]);
        Ok(())
    })?;

    // Silicon's view: rename every function reference.
    let rename = |id: FuncId| -> UserExternalName {
        if id == entry_id {
            UserExternalName::new(BACKEND_NAMESPACE, spec.entry_func_id)
        } else if id == ids.exit {
            UserExternalName::new(STUB_NAMESPACE, 0)
        } else {
            UserExternalName::new(RUNTIME_NAMESPACE, id.as_u32())
        }
    };
    let mut functions = Vec::new();
    let defined = std::mem::take(&mut g.defined);
    for (symbol, id, mut func) in defined {
        let refs: Vec<_> = func
            .params
            .user_named_funcs()
            .iter()
            .map(|(r, n)| (r, n.clone()))
            .collect();
        for (r, n) in refs {
            if n.namespace == BACKEND_NAMESPACE {
                func.params
                    .reset_user_func_name(r, rename(FuncId::from_u32(n.index)));
            }
        }
        let n = rename(id);
        func.name = ir::UserFuncName::user(n.namespace, n.index);
        functions.push((symbol, func));
    }
    let name = |id: FuncId| {
        let n = rename(id);
        format!("u{}:{}", n.namespace, n.index)
    };
    let mut traps = Vec::new();
    for (symbol, func) in &functions {
        let mut ctx = cranelift_codegen::Context::for_function(func.clone());
        let compiled = ctx
            .compile(
                &*spec.isa,
                &mut cranelift_codegen::control::ControlPlane::default(),
            )
            .map_err(|e| FreestandingError::Codegen(format!("runtime {symbol}: {:?}", e.inner)))?;
        for t in compiled.buffer.traps() {
            traps.push((symbol.clone(), u64::from(t.offset), t.code));
        }
    }
    let object = g.m.finish().emit().map_err(codegen)?;
    let _ = spec.target;
    Ok(Runtime {
        traps,
        object,
        functions,
        main: name(main_id),
        trap: name(trap_id),
        exit: name(ids.exit),
    })
}
