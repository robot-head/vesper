//! Cranelift lowering of `i128` and `u128` (SH2 lane 2, #1174).
//!
//! A 128-bit value is one Cranelift `I128` (two registers, or 16 bytes in
//! memory, low word first). Addition, subtraction, multiplication, the bit
//! operators, shifts, rotations, comparisons and the bit counts are single
//! Cranelift instructions. The overflow checks of `vesper.arith.checked/1`
//! split the operands into 64-bit words where Cranelift has no 128-bit
//! form (`umulhi`), and division is the word algorithm on 64-bit `udiv`
//! ([`Codegen::wide_udivrem`]): Cranelift has no 128-bit divide on x86-64
//! or AArch64.
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
    /// Unsigned quotient and remainder of two `I128` values, `d != 0`
    /// (SH4 lane 3, #1277). Cranelift has no 128-bit divide on x86-64 or
    /// AArch64, so this is the word algorithm Rust's `u128` division uses,
    /// built on 64-bit `udiv`:
    ///
    /// - both operands fit in one word: one `udiv` and one `urem`;
    /// - a one-word divisor: the high word by `udiv`, then the two-word
    ///   remainder by 32-bit digits when the divisor is below 2^32, else
    ///   by [`Self::wide_div_words`];
    /// - a two-word divisor: the quotient fits in one word; estimate it from
    ///   the normalized divisor's high word and the dividend halved
    ///   (Hacker's Delight 9-5), which is exact or one too large.
    fn wide_udivrem(&mut self, n: ir::Value, d: ir::Value) -> (ir::Value, ir::Value) {
        let (nl, nh) = self.b.ins().isplit(n);
        let (dl, dh) = self.b.ins().isplit(d);
        let one_word = self.b.create_block();
        let fast = self.b.create_block();
        let by_word = self.b.create_block();
        let by_wide = self.b.create_block();
        let join = self.b.create_block();
        self.b.append_block_param(join, types::I128);
        self.b.append_block_param(join, types::I128);
        let wide_divisor = self.b.ins().icmp_imm(IntCC::NotEqual, dh, 0);
        self.branch(wide_divisor, by_wide, &[], one_word, &[]);
        self.b.switch_to_block(one_word);
        let wide_dividend = self.b.ins().icmp_imm(IntCC::NotEqual, nh, 0);
        self.branch(wide_dividend, by_word, &[], fast, &[]);
        // Both operands fit in 64 bits: one `udiv`.
        self.b.switch_to_block(fast);
        let (q, r) = self.word_divrem(nl, dl);
        let q = self.b.ins().uextend(types::I128, q);
        let r = self.b.ins().uextend(types::I128, r);
        self.jump(join, &[q.into(), r.into()]);
        // A one-word divisor: `nh / dl`, then `(nh % dl, nl) / dl`, whose
        // high word is below the divisor. A divisor below 2^32 (powers of
        // ten, small radixes) divides the low word one 32-bit digit at a
        // time with no estimate to correct.
        self.b.switch_to_block(by_word);
        let (qh, rh) = self.word_divrem(nh, dl);
        let short = self.b.create_block();
        let long = self.b.create_block();
        let small = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedLessThan, dl, 1i64 << 32);
        self.branch(small, short, &[], long, &[]);
        self.b.switch_to_block(short);
        let digit = self.b.ins().ushr_imm(nl, 32);
        let top = self.b.ins().ishl_imm(rh, 32);
        let top = self.b.ins().bor(top, digit);
        let (q1, r1) = self.word_divrem(top, dl);
        let digit = self.b.ins().band_imm(nl, (1i64 << 32) - 1);
        let next = self.b.ins().ishl_imm(r1, 32);
        let next = self.b.ins().bor(next, digit);
        let (q0, r) = self.word_divrem(next, dl);
        let ql = self.b.ins().ishl_imm(q1, 32);
        let ql = self.b.ins().bor(ql, q0);
        let q = self.b.ins().iconcat(ql, qh);
        let r = self.b.ins().uextend(types::I128, r);
        self.jump(join, &[q.into(), r.into()]);
        self.b.switch_to_block(long);
        let (ql, r) = self.wide_div_words(rh, nl, dl);
        let q = self.b.ins().iconcat(ql, qh);
        let r = self.b.ins().uextend(types::I128, r);
        self.jump(join, &[q.into(), r.into()]);
        // A two-word divisor: `s` normalizes it, `v1` is its top 64 bits.
        // `(n >> 1) / v1` has a high word below `v1`, and its quotient
        // shifted back by `63 - s` is the quotient or one more, after the
        // decrement below; one compare corrects it.
        self.b.switch_to_block(by_wide);
        let s = self.b.ins().clz(dh);
        let up = self.b.ins().ishl(dh, s);
        let down = self.shr_complement(dl, s);
        let v1 = self.b.ins().bor(up, down);
        let half = self.b.ins().ushr_imm(n, 1);
        let (hl, hh) = self.b.ins().isplit(half);
        let (q1, _) = self.wide_div_words(hh, hl, v1);
        let back = self.b.ins().irsub_imm(s, 63);
        let q0 = self.b.ins().ushr(q1, back);
        let nonzero = self.b.ins().icmp_imm(IntCC::NotEqual, q0, 0);
        let nonzero = self.b.ins().uextend(types::I64, nonzero);
        let q0 = self.b.ins().isub(q0, nonzero);
        let wq = self.b.ins().uextend(types::I128, q0);
        let p = self.b.ins().imul(wq, d);
        let r = self.b.ins().isub(n, p);
        let over = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, r, d);
        let less = self.b.ins().isub(r, d);
        let r = self.b.ins().select(over, less, r);
        let over = self.b.ins().uextend(types::I64, over);
        let q0 = self.b.ins().iadd(q0, over);
        let q = self.b.ins().uextend(types::I128, q0);
        self.jump(join, &[q.into(), r.into()]);
        self.b.switch_to_block(join);
        let params = self.b.block_params(join);
        (params[0], params[1])
    }
    /// One-word quotient and remainder, `d != 0`: one `udiv`, the
    /// remainder from the product (a second `udiv` costs tens of cycles).
    fn word_divrem(&mut self, n: ir::Value, d: ir::Value) -> (ir::Value, ir::Value) {
        let q = self.b.ins().udiv(n, d);
        let p = self.b.ins().imul(q, d);
        let r = self.b.ins().isub(n, p);
        (q, r)
    }
    /// `x >> (64 - s)` for `s` in `0..64`, as 0 when `s` is 0 (a 64-bit
    /// `ushr` takes its amount modulo 64).
    fn shr_complement(&mut self, x: ir::Value, s: ir::Value) -> ir::Value {
        let amount = self.b.ins().irsub_imm(s, 64);
        let shifted = self.b.ins().ushr(x, amount);
        let neg = self.b.ins().ineg(s);
        let mask = self.b.ins().sshr_imm(neg, 63);
        self.b.ins().band(shifted, mask)
    }
    /// Quotient and remainder of the two-word `(u1, u0)` by the one-word
    /// `v` when `u1 < v`, so the quotient fits in one word: Knuth's
    /// algorithm D on 32-bit digits with 64-bit `udiv` (Hacker's Delight
    /// `divlu`, 9-3). Each quotient digit estimate is at most two too
    /// large; the two corrections are selects, so the code is straight.
    fn wide_div_words(
        &mut self,
        u1: ir::Value,
        u0: ir::Value,
        v: ir::Value,
    ) -> (ir::Value, ir::Value) {
        const DIGIT: i64 = 1 << 32;
        const DIGIT_MASK: i64 = DIGIT - 1;
        let s = self.b.ins().clz(v);
        let v = self.b.ins().ishl(v, s);
        let vn1 = self.b.ins().ushr_imm(v, 32);
        let vn0 = self.b.ins().band_imm(v, DIGIT_MASK);
        let up = self.b.ins().ishl(u1, s);
        let down = self.shr_complement(u0, s);
        let un32 = self.b.ins().bor(up, down);
        let un10 = self.b.ins().ishl(u0, s);
        let un1 = self.b.ins().ushr_imm(un10, 32);
        let un0 = self.b.ins().band_imm(un10, DIGIT_MASK);
        let q1 = self.quotient_digit(un32, un1, vn1, vn0);
        // un21 = un32 * 2^32 + un1 - q1 * v, modulo 2^64.
        let high = self.b.ins().ishl_imm(un32, 32);
        let high = self.b.ins().bor(high, un1);
        let qv = self.b.ins().imul(q1, v);
        let un21 = self.b.ins().isub(high, qv);
        let q0 = self.quotient_digit(un21, un0, vn1, vn0);
        let low = self.b.ins().ishl_imm(un21, 32);
        let low = self.b.ins().bor(low, un0);
        let qv = self.b.ins().imul(q0, v);
        let r = self.b.ins().isub(low, qv);
        let r = self.b.ins().ushr(r, s);
        let q = self.b.ins().ishl_imm(q1, 32);
        let q = self.b.ins().bor(q, q0);
        (q, r)
    }
    /// One quotient digit of [`Self::wide_div_words`]: `top / vn1`, lowered
    /// by at most two while it is a full digit or `q * vn0` exceeds
    /// `rhat * 2^32 + next` (the second step only while `rhat` is still
    /// below one digit, as `divlu`'s loop).
    fn quotient_digit(
        &mut self,
        top: ir::Value,
        next: ir::Value,
        vn1: ir::Value,
        vn0: ir::Value,
    ) -> ir::Value {
        const DIGIT: i64 = 1 << 32;
        let zero = self.b.ins().iconst(types::I64, 0);
        let (mut q, mut rhat) = self.word_divrem(top, vn1);
        let mut live = None;
        for _ in 0..2 {
            let full = self
                .b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, q, DIGIT);
            let lhs = self.b.ins().imul(q, vn0);
            let rhs = self.b.ins().ishl_imm(rhat, 32);
            let rhs = self.b.ins().bor(rhs, next);
            let big = self.b.ins().icmp(IntCC::UnsignedGreaterThan, lhs, rhs);
            let mut step = self.b.ins().bor(full, big);
            if let Some(live) = live {
                step = self.b.ins().band(step, live);
            }
            let by = self.b.ins().uextend(types::I64, step);
            q = self.b.ins().isub(q, by);
            let add = self.b.ins().select(step, vn1, zero);
            rhat = self.b.ins().iadd(rhat, add);
            let small = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, rhat, DIGIT);
            live = Some(self.b.ins().band(step, small));
        }
        q
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

/// Give every tuple that holds a 128-bit element its 128-bit element type
/// (#1270).
///
/// The checker types an integer literal from the binding it flows into, so
/// in `let t: [i128; 3] = [1, 2, 5]` each element is an `i128` constant,
/// but the tuple expression itself keeps its literal type, which defaults to
/// `(i64, i64, i64)`. The evaluator never reads that type; native code lays
/// the tuple out by it, so it stored three one-word elements that the
/// binding's `[i128; 3]` layout then read as two-word ones. This pass
/// widens the type of each `Op::Tuple` (and of the block parameters that
/// receive one) from its operands, to a fixed point, before lowering. It
/// only changes an element whose operand is 128-bit and whose recorded type
/// is a narrower integer, so every other program is unchanged.
pub(super) fn refine_wide_tuples(program: &mut crate::ir::Program) {
    for f in program.functions.values_mut() {
        refine_function(f);
    }
}

fn refine_function(f: &mut crate::ir::FunctionIr) {
    use crate::ir::{Op, Terminator};
    // Cheap exit: nothing to refine without a 128-bit value.
    if !f.values.values().chain(f.locals.values()).any(holds_wide) {
        return;
    }
    loop {
        let mut changed = false;
        for b in 0..f.blocks.len() {
            for i in 0..f.blocks[b].instructions.len() {
                let inst = &f.blocks[b].instructions[i];
                // A local takes its type from its initializer, so a
                // refined tuple widens the local it binds; a whole read of
                // the local carries the binding's checked type.
                match &inst.op {
                    Op::Bind { pattern, value } => {
                        if let Some(actual) = f.values.get(value).cloned() {
                            changed |= widen_pattern(&mut f.locals, pattern, &actual);
                        }
                        continue;
                    }
                    Op::Read {
                        root,
                        path,
                        projections,
                        ..
                    } if path.is_empty() && projections.is_empty() => {
                        if let Some(t) = f.locals.get(root).and_then(|d| widen(d, &inst.ty)) {
                            f.locals.insert(root.clone(), t);
                            changed = true;
                        }
                        continue;
                    }
                    _ => {}
                }
                let Op::Tuple(ids) = &inst.op else {
                    continue;
                };
                let actual = Type::Tuple(
                    ids.iter()
                        .map(|id| f.values.get(id).cloned().unwrap_or(Type::Infer))
                        .collect(),
                );
                if let Some(t) = widen(&inst.ty, &actual) {
                    let dest = inst.dest;
                    f.blocks[b].instructions[i].ty = t.clone();
                    f.values.insert(dest, t);
                    changed = true;
                }
            }
            if let Terminator::Jump { block, args } = &f.blocks[b].terminator {
                let (block, args) = (*block, args.clone());
                for (param, arg) in f.blocks[block].params.clone().into_iter().zip(args) {
                    let (Some(declared), Some(actual)) = (f.values.get(&param), f.values.get(&arg))
                    else {
                        continue;
                    };
                    if let Some(t) = widen(declared, actual) {
                        f.values.insert(param, t);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return;
        }
    }
}

/// Widen the locals that `pattern` binds from the bound value's `actual`
/// type; whether any changed.
fn widen_pattern(
    locals: &mut std::collections::BTreeMap<String, Type>,
    pattern: &Pattern,
    actual: &Type,
) -> bool {
    match (&pattern.kind, actual) {
        (PatternKind::Bind(name), actual) => {
            match locals.get(name).and_then(|d| widen(d, actual)) {
                Some(t) => {
                    locals.insert(name.clone(), t);
                    true
                }
                None => false,
            }
        }
        (PatternKind::Tuple(ps), Type::Tuple(ts)) if ps.len() == ts.len() => ps
            .iter()
            .zip(ts)
            .fold(false, |any, (p, t)| widen_pattern(locals, p, t) | any),
        _ => false,
    }
}

/// Whether `t` is, or is a tuple that holds, a 128-bit integer.
fn holds_wide(t: &Type) -> bool {
    match t {
        Type::Tuple(ts) => ts.iter().any(holds_wide),
        t => is_wide(t),
    }
}

/// `declared` with each narrower integer element that `actual` holds as a
/// 128-bit integer replaced by it, or `None` when nothing changes.
fn widen(declared: &Type, actual: &Type) -> Option<Type> {
    match (declared, actual) {
        (Type::Tuple(ds), Type::Tuple(acts)) if ds.len() == acts.len() => {
            let mut any = false;
            let ts = ds
                .iter()
                .zip(acts)
                .map(|(d, a)| match widen(d, a) {
                    Some(t) => {
                        any = true;
                        t
                    }
                    None => d.clone(),
                })
                .collect();
            any.then_some(Type::Tuple(ts))
        }
        (d, a)
            if is_wide(a)
                && !is_wide(d)
                && (matches!(d, Type::IntLiteral(_)) || IntegerKind::from_type(d).is_some()) =>
        {
            Some(a.clone())
        }
        _ => None,
    }
}
