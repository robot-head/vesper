//! Cranelift JIT/AOT lowering for `String` and `Str` (§13 strings).
//!
//! A string or view is a 24-byte value `{len, cap, data}` whose address is
//! a `ChunkSeq[u8]` handle: growth goes through `vesper_seq_grow`, which
//! copies borrowed bytes (capacity 0) before the first write. A literal
//! points at static data; a view is `{len, 0, data + start}`.
use super::*;
use crate::strings::StrOp;
use cranelift_codegen::ir::{self, InstBuilder, MemFlags, condcodes::IntCC, types};
use cranelift_module::{DataDescription, Module};

const LEN: i32 = 0;
const CAP: i32 = 8;
const DATA: i32 = 16;

/// `vesper_seq_fault` code of a string interval off a char boundary.
pub(crate) const BOUNDARY_FAULT: i64 = 2;

impl<M: Module> Codegen<'_, '_, M> {
    fn str_word(&mut self, ptr: ir::Value, offset: i32) -> ir::Value {
        self.b.ins().load(types::I64, MemFlags::new(), ptr, offset)
    }

    fn str_value(&mut self, len: ir::Value, cap: ir::Value, data: ir::Value) -> ir::Value {
        let ptr = self.alloc(24);
        self.b.ins().store(MemFlags::new(), len, ptr, LEN);
        self.b.ins().store(MemFlags::new(), cap, ptr, CAP);
        self.b.ins().store(MemFlags::new(), data, ptr, DATA);
        ptr
    }

    /// A string literal: `{n, 0, &static_bytes}` (`{0, 0, null}` when empty).
    pub(super) fn string_literal(&mut self, text: &str) -> Result<ir::Value, String> {
        let len = self.b.ins().iconst(types::I64, text.len() as i64);
        let zero = self.zero();
        if text.is_empty() {
            return Ok(self.str_value(len, zero, zero));
        }
        let id = self
            .m
            .declare_anonymous_data(false, false)
            .map_err(|e| e.to_string())?;
        let mut data = DataDescription::new();
        data.define(text.as_bytes().to_vec().into_boxed_slice());
        data.set_align(8);
        self.m.define_data(id, &data).map_err(|e| e.to_string())?;
        let global = self.m.declare_data_in_func(id, self.b.func);
        let pointer = self.m.target_config().pointer_type();
        let address = self.b.ins().symbol_value(pointer, global);
        Ok(self.str_value(len, zero, address))
    }

    /// The byte at `data + i` (zero-extended).
    fn str_byte(&mut self, value: ir::Value, i: ir::Value) -> ir::Value {
        let data = self.str_word(value, DATA);
        let at = self.b.ins().iadd(data, i);
        self.b.ins().uload8(types::I64, MemFlags::new(), at, 0)
    }

    /// Whether byte offset `i` of `value` (with `i <= len`) starts a char.
    /// At `i == len` the load reads a zeroed stack byte instead of the
    /// string, whose data may be null (empty) or end exactly at `i`.
    fn str_boundary(&mut self, value: ir::Value, i: ir::Value, len: ir::Value) -> ir::Value {
        let at_end = self.b.ins().icmp(IntCC::Equal, i, len);
        let scratch = self.alloc(8);
        let data = self.str_word(value, DATA);
        let inside = self.b.ins().iadd(data, i);
        let address = self.b.ins().select(at_end, scratch, inside);
        let byte = self.b.ins().uload8(types::I64, MemFlags::new(), address, 0);
        let high = self.b.ins().band_imm(byte, 0xC0);
        let lead = self.b.ins().icmp_imm(IntCC::NotEqual, high, 0x80);
        self.b.ins().bor(at_end, lead)
    }

    /// Appends `n` bytes from `src` to the string at `h`.
    fn str_append(&mut self, h: ir::Value, src: ir::Value, n: ir::Value) -> Result<(), String> {
        let len = self.str_word(h, LEN);
        let needed = self.b.ins().iadd(len, n);
        self.seq_reserve(h, needed, 1, None)?;
        let data = self.str_word(h, DATA);
        let dst = self.b.ins().iadd(data, len);
        self.seq_copy_run(dst, src, n);
        self.b.ins().store(MemFlags::new(), needed, h, LEN);
        Ok(())
    }

    /// Lowers one string primitive on its operand values.
    pub(super) fn str_op(&mut self, op: StrOp, args: &[ir::Value]) -> Result<ir::Value, String> {
        let arg = |i: usize| {
            args.get(i)
                .copied()
                .ok_or_else(|| format!("native String::{} lacks an operand", op.name()))
        };
        let s = arg(0)?;
        Ok(match op {
            StrOp::Len => self.str_word(s, LEN),
            StrOp::ByteAt => {
                let len = self.str_word(s, LEN);
                let i = arg(1)?;
                let valid = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                self.seq_guard(valid)?;
                self.str_byte(s, i)
            }
            StrOp::TryByteAt => {
                let len = self.str_word(s, LEN);
                let i = arg(1)?;
                let valid = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                let u8_ty = Type::named("u8");
                let option = Type::Named("Option".into(), vec![u8_ty.clone()]);
                self.seq_option(&option, &u8_ty, valid, |this| Ok(this.str_byte(s, i)))?
            }
            StrOp::AsStr => s,
            StrOp::AsBytes => {
                let len = self.str_word(s, LEN);
                let zero = self.zero();
                self.str_value(s, zero, len)
            }
            StrOp::SubStr => {
                let len = self.str_word(s, LEN);
                let lo = arg(1)?;
                let hi = arg(2)?;
                let ordered = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, lo, hi);
                let inside = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, hi, len);
                let valid = self.b.ins().band(ordered, inside);
                let good = self.b.create_block();
                let bad = self.b.create_block();
                let done = self.b.create_block();
                self.b.append_block_param(done, types::I8);
                self.branch(valid, good, &[], bad, &[]);
                self.b.switch_to_block(bad);
                let code = self.b.ins().iconst(types::I64, 1);
                self.seq_runtime_call("vesper_seq_fault", &[code])?;
                let no = self.b.ins().iconst(types::I8, 0);
                self.jump(done, &[no.into()]);
                self.b.switch_to_block(good);
                let a = self.str_boundary(s, lo, len);
                let b = self.str_boundary(s, hi, len);
                let on = self.b.ins().band(a, b);
                let fault = self.b.create_block();
                let fine = self.b.create_block();
                self.branch(on, fine, &[], fault, &[]);
                self.b.switch_to_block(fault);
                let code = self.b.ins().iconst(types::I64, BOUNDARY_FAULT);
                self.seq_runtime_call("vesper_seq_fault", &[code])?;
                let no = self.b.ins().iconst(types::I8, 0);
                self.jump(done, &[no.into()]);
                self.b.switch_to_block(fine);
                let yes = self.b.ins().iconst(types::I8, 1);
                self.jump(done, &[yes.into()]);
                self.b.switch_to_block(done);
                // A fault reports and continues with an empty view in the
                // JIT, and exits in an executable.
                let ok = self.b.block_params(done)[0];
                let zero = self.zero();
                let n = self.b.ins().isub(hi, lo);
                let n = self.b.ins().select(ok, n, zero);
                let data = self.str_word(s, DATA);
                let at = self.b.ins().iadd(data, lo);
                let at = self.b.ins().select(ok, at, data);
                self.str_value(n, zero, at)
            }
            StrOp::PushStr => {
                let src = arg(1)?;
                let n = self.str_word(src, LEN);
                let data = self.str_word(src, DATA);
                self.str_append(s, data, n)?;
                self.zero()
            }
            StrOp::PushChar => self.str_push_char(s, arg(1)?)?,
            StrOp::Clear => {
                let zero = self.zero();
                self.b.ins().store(MemFlags::new(), zero, s, LEN);
                zero
            }
            StrOp::Reserve => {
                let len = self.str_word(s, LEN);
                let n = arg(1)?;
                // A request past the address space saturates; growth then
                // faults as an out-of-memory sequence does.
                let (needed, over) = self.b.ins().uadd_overflow(len, n);
                let max = self.b.ins().iconst(types::I64, -1);
                let needed = self.b.ins().select(over, max, needed);
                self.seq_reserve(s, needed, 1, None)?;
                self.zero()
            }
        })
    }

    /// `s.push_char(c)`: the UTF-8 encoding of a scalar value, or `false`.
    fn str_push_char(&mut self, s: ir::Value, c: ir::Value) -> Result<ir::Value, String> {
        // Encode into a scratch word, little-endian, with its width.
        let scratch = self.alloc(8);
        let lt = |this: &mut Self, v: i64| this.b.ins().icmp_imm(IntCC::UnsignedLessThan, c, v);
        let one = lt(self, 0x80);
        let two = lt(self, 0x800);
        let three = lt(self, 0x1_0000);
        let four = lt(self, 0x11_0000);
        let low_sur = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, c, 0xD800);
        let high_sur = lt(self, 0xE000);
        let surrogate = self.b.ins().band(low_sur, high_sur);
        let not_sur = self.b.ins().bxor_imm(surrogate, 1);
        let valid = self.b.ins().band(four, not_sur);
        let cont = |this: &mut Self, shift: i64| {
            let v = this.b.ins().ushr_imm(c, shift);
            let v = this.b.ins().band_imm(v, 0x3F);
            this.b.ins().bor_imm(v, 0x80)
        };
        let lead = |this: &mut Self, shift: i64, tag: i64| {
            let v = this.b.ins().ushr_imm(c, shift);
            this.b.ins().bor_imm(v, tag)
        };
        // Two bytes.
        let l2 = lead(self, 6, 0xC0);
        let c20 = cont(self, 0);
        let c20 = self.b.ins().ishl_imm(c20, 8);
        let w2 = self.b.ins().bor(l2, c20);
        // Three bytes.
        let l3 = lead(self, 12, 0xE0);
        let c31 = cont(self, 6);
        let c31 = self.b.ins().ishl_imm(c31, 8);
        let c30 = cont(self, 0);
        let c30 = self.b.ins().ishl_imm(c30, 16);
        let w3 = self.b.ins().bor(l3, c31);
        let w3 = self.b.ins().bor(w3, c30);
        // Four bytes.
        let l4 = lead(self, 18, 0xF0);
        let c42 = cont(self, 12);
        let c42 = self.b.ins().ishl_imm(c42, 8);
        let c41 = cont(self, 6);
        let c41 = self.b.ins().ishl_imm(c41, 16);
        let c40 = cont(self, 0);
        let c40 = self.b.ins().ishl_imm(c40, 24);
        let w4 = self.b.ins().bor(l4, c42);
        let w4 = self.b.ins().bor(w4, c41);
        let w4 = self.b.ins().bor(w4, c40);
        let word = self.b.ins().select(three, w3, w4);
        let word = self.b.ins().select(two, w2, word);
        let word = self.b.ins().select(one, c, word);
        let n4 = self.b.ins().iconst(types::I64, 4);
        let n3 = self.b.ins().iconst(types::I64, 3);
        let n2 = self.b.ins().iconst(types::I64, 2);
        let n1 = self.b.ins().iconst(types::I64, 1);
        let width = self.b.ins().select(three, n3, n4);
        let width = self.b.ins().select(two, n2, width);
        let width = self.b.ins().select(one, n1, width);
        self.b.ins().store(MemFlags::new(), word, scratch, 0);
        let push = self.b.create_block();
        let done = self.b.create_block();
        self.branch(valid, push, &[], done, &[]);
        self.b.switch_to_block(push);
        self.str_append(s, scratch, width)?;
        self.jump(done, &[]);
        self.b.switch_to_block(done);
        Ok(self.b.ins().uextend(types::I64, valid))
    }

    /// `a op b` for strings and views, in byte order.
    pub(super) fn str_compare(
        &mut self,
        op: &str,
        a: ir::Value,
        b: ir::Value,
    ) -> Result<ir::Value, String> {
        let order = emit_str_order(&mut self.b, a, b);
        let cc = match op {
            "==" => IntCC::Equal,
            "!=" => IntCC::NotEqual,
            "<" => IntCC::SignedLessThan,
            "<=" => IntCC::SignedLessThanOrEqual,
            ">" => IntCC::SignedGreaterThan,
            ">=" => IntCC::SignedGreaterThanOrEqual,
            _ => return Err(format!("native strings have no operator '{op}'")),
        };
        let r = self.b.ins().icmp_imm(cc, order, 0);
        Ok(self.b.ins().uextend(types::I64, r))
    }
}

/// `-1`, `0` or `1`: the byte-order comparison of two strings or views.
pub(super) fn emit_str_order(b: &mut FunctionBuilder<'_>, x: ir::Value, y: ir::Value) -> ir::Value {
    let flags = MemFlags::new();
    let xl = b.ins().load(types::I64, flags, x, LEN);
    let yl = b.ins().load(types::I64, flags, y, LEN);
    let xd = b.ins().load(types::I64, flags, x, DATA);
    let yd = b.ins().load(types::I64, flags, y, DATA);
    let shorter = b.ins().icmp(IntCC::UnsignedLessThan, xl, yl);
    let n = b.ins().select(shorter, xl, yl);
    let head = b.create_block();
    let body = b.create_block();
    let differ = b.create_block();
    let tail = b.create_block();
    let done = b.create_block();
    b.append_block_param(head, types::I64);
    b.append_block_param(done, types::I64);
    let zero = b.ins().iconst(types::I64, 0);
    b.ins().jump(head, &[zero.into()]);
    b.switch_to_block(head);
    let k = b.block_params(head)[0];
    let more = b.ins().icmp(IntCC::UnsignedLessThan, k, n);
    b.ins().brif(more, body, &[], tail, &[]);
    b.switch_to_block(body);
    let xa = b.ins().iadd(xd, k);
    let ya = b.ins().iadd(yd, k);
    let xb = b.ins().uload8(types::I64, flags, xa, 0);
    let yb = b.ins().uload8(types::I64, flags, ya, 0);
    let same = b.ins().icmp(IntCC::Equal, xb, yb);
    let next = b.ins().iadd_imm(k, 1);
    b.ins().brif(same, head, &[next.into()], differ, &[]);
    b.switch_to_block(differ);
    let less = b.ins().icmp(IntCC::UnsignedLessThan, xb, yb);
    let minus = b.ins().iconst(types::I64, -1);
    let plus = b.ins().iconst(types::I64, 1);
    let r = b.ins().select(less, minus, plus);
    b.ins().jump(done, &[r.into()]);
    b.switch_to_block(tail);
    let lt = b.ins().icmp(IntCC::UnsignedLessThan, xl, yl);
    let gt = b.ins().icmp(IntCC::UnsignedGreaterThan, xl, yl);
    let lt = b.ins().uextend(types::I64, lt);
    let gt = b.ins().uextend(types::I64, gt);
    let r = b.ins().isub(gt, lt);
    b.ins().jump(done, &[r.into()]);
    b.switch_to_block(done);
    b.block_params(done)[0]
}
