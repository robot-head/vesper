//! Cranelift lowering of `i128` and `u128` (SH2 lane 2, #1174).
//!
//! A 128-bit value is one Cranelift `I128` (two registers, or 16 bytes in
//! memory, low word first). Addition, subtraction, multiplication, the bit
//! operators, shifts, rotations, comparisons and the bit counts are single
//! Cranelift instructions. The overflow checks of `vesper.arith.checked/1`
//! split the operands into 64-bit words where Cranelift has no 128-bit
//! form (`umulhi`), and division is a loop over the dividend's bits with a
//! one-instruction fast path when both operands fit in 64 bits: Cranelift
//! has no 128-bit divide on x86-64 or AArch64.
use super::*;
use crate::integer_widths::BitOp;
use crate::numeric::{Conversion, IntegerKind};

/// Whether `t` is `i128` or `u128`.
pub(super) fn is_wide(t: &Type) -> bool {
    !matches!(t, Type::IntLiteral(_)) && IntegerKind::from_type(t).is_some_and(IntegerKind::wide)
}

impl<M: Module> Codegen<'_, '_, M> {
    /// The `I128` constant `n`.
    pub(super) fn wide_const(&mut self, n: i128) -> ir::Value {
        let low = self.b.ins().iconst(types::I64, n as i64);
        let high = self.b.ins().iconst(types::I64, (n >> 64) as i64);
        self.b.ins().iconcat(low, high)
    }
    /// Whether the `I128` value `v` is negative.
    fn wide_negative(&mut self, v: ir::Value) -> ir::Value {
        let (_, high) = self.b.ins().isplit(v);
        self.b.ins().icmp_imm(IntCC::SignedLessThan, high, 0)
    }
    /// `|v|` of a signed `I128` as an unsigned one (`|MIN|` is `2^127`).
    fn wide_magnitude(&mut self, v: ir::Value, negative: ir::Value) -> ir::Value {
        let minus = self.b.ins().ineg(v);
        self.b.ins().select(negative, minus, v)
    }
    /// Whether the unsigned product `a * b` leaves 128 bits, and the
    /// product modulo 2^128 (from the same partial products).
    fn wide_product(&mut self, a: ir::Value, b: ir::Value) -> (ir::Value, ir::Value) {
        match (self.low_word_of(a), self.low_word_of(b)) {
            (Some(_), None) => return self.widened_product(b, a),
            (None, Some(_)) => return self.widened_product(a, b),
            _ => {}
        }
        let (al, ah) = self.b.ins().isplit(a);
        let (bl, bh) = self.b.ins().isplit(b);
        // Both high words nonzero: the product is at least 2^128.
        let ah_set = self.b.ins().icmp_imm(IntCC::NotEqual, ah, 0);
        let bh_set = self.b.ins().icmp_imm(IntCC::NotEqual, bh, 0);
        let both = self.b.ins().band(ah_set, bh_set);
        // Otherwise one cross product, whose high word must be zero and
        // whose low word plus the high word of `al * bl` must not carry.
        let c1 = self.b.ins().umulhi(ah, bl);
        let c2 = self.b.ins().umulhi(al, bh);
        let c = self.b.ins().bor(c1, c2);
        let cross_high = self.b.ins().icmp_imm(IntCC::NotEqual, c, 0);
        let l1 = self.b.ins().imul(ah, bl);
        let l2 = self.b.ins().imul(al, bh);
        let cross = self.b.ins().iadd(l1, l2);
        let low_high = self.b.ins().umulhi(al, bl);
        let high = self.b.ins().iadd(low_high, cross);
        let carry = self.b.ins().icmp(IntCC::UnsignedLessThan, high, cross);
        let x = self.b.ins().bor(both, cross_high);
        let overflow = self.b.ins().bor(x, carry);
        let low = self.b.ins().imul(al, bl);
        (overflow, self.b.ins().iconcat(low, high))
    }
    /// The 64-bit word `v` widens, when its high word is known zero: a
    /// `uextend` to `I128` (`widen_u128` of an unsigned kind, or a
    /// widened `u64` constant) or an `iconcat` with a zero high word.
    fn low_word_of(&self, v: ir::Value) -> Option<ir::Value> {
        use ir::{InstructionData as D, Opcode as O};
        let ir::ValueDef::Result(inst, 0) = self.b.func.dfg.value_def(v) else {
            return None;
        };
        match self.b.func.dfg.insts[inst] {
            D::Unary {
                opcode: O::Uextend,
                arg,
            } if self.b.func.dfg.value_type(arg) == types::I64 => Some(arg),
            D::Binary {
                opcode: O::Iconcat,
                args: [low, high],
            } if self.constant_of(high) == Some(0) => Some(low),
            _ => None,
        }
    }
    /// [`Self::wide_product`] when `b`'s high word is zero: `a * bl` leaves
    /// 128 bits only when `ah * bl` does, or when its low word plus the
    /// high word of `al * bl` carries. One cross product instead of two,
    /// and no test of `b`'s high word (#1245).
    fn widened_product(&mut self, a: ir::Value, b: ir::Value) -> (ir::Value, ir::Value) {
        let (al, ah) = self.b.ins().isplit(a);
        let bl = self.low_word_of(b).expect("a widened operand");
        let c = self.b.ins().umulhi(ah, bl);
        let cross_high = self.b.ins().icmp_imm(IntCC::NotEqual, c, 0);
        let cross = self.b.ins().imul(ah, bl);
        let low_high = self.b.ins().umulhi(al, bl);
        let high = self.b.ins().iadd(low_high, cross);
        let carry = self.b.ins().icmp(IntCC::UnsignedLessThan, high, cross);
        let overflow = self.b.ins().bor(cross_high, carry);
        let low = self.b.ins().imul(al, bl);
        (overflow, self.b.ins().iconcat(low, high))
    }
    /// `vesper.arith.checked/1` on a 128-bit kind.
    pub(super) fn wide_checked(
        &mut self,
        op: &str,
        kind: IntegerKind,
        a: ir::Value,
        b: ir::Value,
    ) -> Result<ir::Value, String> {
        let overflow = match (op, kind.signed()) {
            ("+", false) => {
                let r = self.b.ins().iadd(a, b);
                Some(self.b.ins().icmp(IntCC::UnsignedLessThan, r, a))
            }
            ("-", false) => Some(self.b.ins().icmp(IntCC::UnsignedLessThan, a, b)),
            ("+" | "-", true) => {
                let r = if op == "+" {
                    self.b.ins().iadd(a, b)
                } else {
                    self.b.ins().isub(a, b)
                };
                // As for i64: the result's sign differs from both addends,
                // or from the minuend while the operand signs differ.
                let x = self.b.ins().bxor(a, r);
                let y = if op == "+" {
                    self.b.ins().bxor(b, r)
                } else {
                    self.b.ins().bxor(a, b)
                };
                let both = self.b.ins().band(x, y);
                Some(self.wide_negative(both))
            }
            // The product comes from the partial products of the check.
            ("*", false) => {
                // Two widened words: the product always fits in 128 bits.
                if let (Some(al), Some(bl)) = (self.low_word_of(a), self.low_word_of(b)) {
                    let low = self.b.ins().imul(al, bl);
                    let high = self.b.ins().umulhi(al, bl);
                    return Ok(self.b.ins().iconcat(low, high));
                }
                let (overflow, p) = self.wide_product(a, b);
                self.check_overflow(overflow)?;
                return Ok(p);
            }
            ("*", true) => {
                let na = self.wide_negative(a);
                let nb = self.wide_negative(b);
                let ua = self.wide_magnitude(a, na);
                let ub = self.wide_magnitude(b, nb);
                let (unsigned, p) = self.wide_product(ua, ub);
                // A negative product may reach 2^127; a positive one not.
                let limit = self.wide_const(i128::MIN);
                let negative = self.b.ins().bxor(na, nb);
                let above = self.b.ins().icmp(IntCC::UnsignedGreaterThan, p, limit);
                let reaches = self
                    .b
                    .ins()
                    .icmp(IntCC::UnsignedGreaterThanOrEqual, p, limit);
                let out = self.b.ins().select(negative, above, reaches);
                let overflow = self.b.ins().bor(unsigned, out);
                self.check_overflow(overflow)?;
                return Ok(self.wide_magnitude(p, negative));
            }
            ("/", true) => {
                let min = self.wide_const(i128::MIN);
                let minus_one = self.wide_const(-1);
                let is_min = self.b.ins().icmp(IntCC::Equal, a, min);
                let is_minus_one = self.b.ins().icmp(IntCC::Equal, b, minus_one);
                Some(self.b.ins().band(is_min, is_minus_one))
            }
            _ => None,
        };
        if let Some(overflow) = overflow {
            self.check_overflow(overflow)?;
        }
        self.binary(op, a, b, kind.unsigned())
    }
    /// `/` or `%` of two `I128` values with a nonzero divisor. Division
    /// truncates toward zero and the remainder has the dividend's sign;
    /// signed `MIN / -1` wraps to `MIN` (the checked operator raises first).
    pub(super) fn wide_divide(
        &mut self,
        op: &str,
        a: ir::Value,
        b: ir::Value,
        unsigned: bool,
    ) -> Result<ir::Value, String> {
        if unsigned {
            let (q, r) = self.wide_udivrem(a, b);
            return Ok(if op == "/" { q } else { r });
        }
        let na = self.wide_negative(a);
        let nb = self.wide_negative(b);
        let ua = self.wide_magnitude(a, na);
        let ub = self.wide_magnitude(b, nb);
        let (q, r) = self.wide_udivrem(ua, ub);
        Ok(if op == "/" {
            let negative = self.b.ins().bxor(na, nb);
            self.wide_magnitude(q, negative)
        } else {
            self.wide_magnitude(r, na)
        })
    }
    /// Unsigned quotient and remainder of two `I128` values, `d != 0`.
    fn wide_udivrem(&mut self, n: ir::Value, d: ir::Value) -> (ir::Value, ir::Value) {
        let (nl, nh) = self.b.ins().isplit(n);
        let (dl, dh) = self.b.ins().isplit(d);
        let high = self.b.ins().bor(nh, dh);
        let small = self.b.ins().icmp_imm(IntCC::Equal, high, 0);
        let fast = self.b.create_block();
        let slow = self.b.create_block();
        let head = self.b.create_block();
        let step = self.b.create_block();
        let join = self.b.create_block();
        self.b.append_block_param(join, types::I128);
        self.b.append_block_param(join, types::I128);
        self.branch(small, fast, &[], slow, &[]);
        // Both operands fit in 64 bits: one `udiv` and one `urem`.
        self.b.switch_to_block(fast);
        let q = self.b.ins().udiv(nl, dl);
        let r = self.b.ins().urem(nl, dl);
        let q = self.b.ins().uextend(types::I128, q);
        let r = self.b.ins().uextend(types::I128, r);
        self.jump(join, &[q.into(), r.into()]);
        // Restoring division, one dividend bit per iteration from the top.
        self.b.switch_to_block(slow);
        let zero = self.wide_const(0);
        let start = self.b.ins().iconst(types::I64, 128);
        self.jump(head, &[start.into(), zero.into(), zero.into()]);
        self.b.switch_to_block(head);
        let i = self.b.append_block_param(head, types::I64);
        let q = self.b.append_block_param(head, types::I128);
        let r = self.b.append_block_param(head, types::I128);
        let done = self.b.ins().icmp_imm(IntCC::Equal, i, 0);
        self.branch(done, join, &[q.into(), r.into()], step, &[]);
        self.b.switch_to_block(step);
        let i = self.b.ins().iadd_imm(i, -1);
        let bit = self.b.ins().ushr(n, i);
        let one = self.wide_const(1);
        let bit = self.b.ins().band(bit, one);
        let r = self.b.ins().ishl_imm(r, 1);
        let r = self.b.ins().bor(r, bit);
        let fits = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, r, d);
        let less = self.b.ins().isub(r, d);
        let r = self.b.ins().select(fits, less, r);
        let set = self.b.ins().ishl(one, i);
        let with = self.b.ins().bor(q, set);
        let q = self.b.ins().select(fits, with, q);
        self.jump(head, &[i.into(), q.into(), r.into()]);
        self.b.switch_to_block(join);
        let params = self.b.block_params(join);
        (params[0], params[1])
    }
    /// The nonnegative constant `v` holds, if an `iconst` defines it, as
    /// seen through the reductions `coerce` applies to a literal (`band_imm`
    /// with a low mask, `ireduce` then `uextend`).
    fn constant_of(&self, v: ir::Value) -> Option<i64> {
        use ir::{InstructionData as D, Opcode as O};
        let ir::ValueDef::Result(inst, 0) = self.b.func.dfg.value_def(v) else {
            return None;
        };
        let k = match self.b.func.dfg.insts[inst] {
            D::UnaryImm {
                opcode: O::Iconst,
                imm,
            } => imm.bits(),
            D::BinaryImm64 {
                opcode: O::BandImm,
                arg,
                imm,
            } => self.constant_of(arg)? & imm.bits(),
            D::Unary {
                opcode: O::Uextend | O::Ireduce,
                arg,
            } => {
                let bits = self
                    .b
                    .func
                    .dfg
                    .value_type(arg)
                    .bits()
                    .min(self.b.func.dfg.value_type(v).bits());
                let k = self.constant_of(arg)?;
                if bits >= 64 {
                    k
                } else {
                    k & ((1i64 << bits) - 1)
                }
            }
            _ => return None,
        };
        (k >= 0).then_some(k)
    }
    /// `<<` or `>>` of an `I128` lane by a constant amount in `1..128`, on
    /// its two words: Cranelift lowers a 128-bit shift by its variable
    /// form (both directions and a select), about four times the work.
    pub(super) fn wide_shift_const(
        &mut self,
        op: &str,
        kind: IntegerKind,
        a: ir::Value,
        n: ir::Value,
    ) -> Option<ir::Value> {
        if !kind.wide() || !crate::numeric::is_shift(op) {
            return None;
        }
        let k = self.constant_of(n).filter(|k| (1..128).contains(k))?;
        let (lo, hi) = self.b.ins().isplit(a);
        let (lo, hi) = match (op, k) {
            ("<<", 64..) => {
                let zero = self.b.ins().iconst(types::I64, 0);
                let hi = if k == 64 {
                    lo
                } else {
                    self.b.ins().ishl_imm(lo, k - 64)
                };
                (zero, hi)
            }
            ("<<", _) => {
                let up = self.b.ins().ishl_imm(hi, k);
                let carried = self.b.ins().ushr_imm(lo, 64 - k);
                let hi = self.b.ins().bor(up, carried);
                (self.b.ins().ishl_imm(lo, k), hi)
            }
            (_, 64..) => {
                let fill = if kind.unsigned() {
                    self.b.ins().iconst(types::I64, 0)
                } else {
                    self.b.ins().sshr_imm(hi, 63)
                };
                let lo = match (k, kind.unsigned()) {
                    (64, _) => hi,
                    (_, true) => self.b.ins().ushr_imm(hi, k - 64),
                    (_, false) => self.b.ins().sshr_imm(hi, k - 64),
                };
                (lo, fill)
            }
            _ => {
                let down = self.b.ins().ushr_imm(lo, k);
                let carried = self.b.ins().ishl_imm(hi, 64 - k);
                let lo = self.b.ins().bor(down, carried);
                let hi = if kind.unsigned() {
                    self.b.ins().ushr_imm(hi, k)
                } else {
                    self.b.ins().sshr_imm(hi, k)
                };
                (lo, hi)
            }
        };
        Some(self.b.ins().iconcat(lo, hi))
    }
    /// `widen_k`, `narrow_k` and `wrapping_k` where `from` or `to` is a
    /// 128-bit kind. A one-word lane is canonical (narrow unsigned kinds
    /// zero-extended, narrow signed kinds sign-extended), so its exact value
    /// is its extension to 128 bits.
    pub(super) fn wide_convert(
        &mut self,
        conversion: Conversion,
        from: IntegerKind,
        to: IntegerKind,
        v: ir::Value,
        span: &Span,
    ) -> Result<ir::Value, String> {
        let x = if from.wide() {
            v
        } else if from.signed() {
            self.b.ins().sextend(types::I128, v)
        } else {
            self.b.ins().uextend(types::I128, v)
        };
        if conversion == Conversion::Narrow
            && !from.widens_to(to)
            && self.world.overflow_ops.contains(&(span.start, span.end))
        {
            let overflow = if to.wide() {
                // `i* -> u128` fails below zero, `u128 -> i128` above
                // `i128::MAX`: both read the top bit.
                self.wide_negative(x)
            } else if from.unsigned() {
                let max = self.wide_const(to.max());
                self.b.ins().icmp(IntCC::UnsignedGreaterThan, x, max)
            } else {
                let min = self.wide_const(to.min());
                let max = self.wide_const(to.max());
                let low = self.b.ins().icmp(IntCC::SignedLessThan, x, min);
                let high = self.b.ins().icmp(IntCC::SignedGreaterThan, x, max);
                self.b.ins().bor(low, high)
            };
            self.check_overflow(overflow)?;
        }
        Ok(if to.wide() {
            x
        } else {
            let low = self.b.ins().ireduce(types::I64, x);
            self.coerce(low, Some(&to.ty()))
        })
    }
    /// A bit builtin of `integer_widths` on an `I128` lane: one `clz`,
    /// `ctz`, `popcnt`, `ineg`, `ishl`, `ushr` or `sshr` (counts are `u32`
    /// lanes; shift amounts are taken modulo 128).
    pub(super) fn wide_bit_builtin(
        &mut self,
        op: BitOp,
        kind: IntegerKind,
        a: ir::Value,
        n: Option<ir::Value>,
    ) -> Result<ir::Value, String> {
        let count = |s: &mut Self, r: ir::Value| s.b.ins().ireduce(types::I64, r);
        Ok(match op {
            BitOp::LeadingZeros => {
                let r = self.b.ins().clz(a);
                count(self, r)
            }
            BitOp::TrailingZeros => {
                let r = self.b.ins().ctz(a);
                count(self, r)
            }
            BitOp::CountOnes => {
                let r = self.b.ins().popcnt(a);
                count(self, r)
            }
            BitOp::WrappingNeg => self.b.ins().ineg(a),
            BitOp::WrappingShl | BitOp::WrappingShr => {
                let n = n.ok_or("native bit shift without an amount")?;
                match op {
                    BitOp::WrappingShl => self.b.ins().ishl(a, n),
                    _ if kind.unsigned() => self.b.ins().ushr(a, n),
                    _ => self.b.ins().sshr(a, n),
                }
            }
        })
    }
}
