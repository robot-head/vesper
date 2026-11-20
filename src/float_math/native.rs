//! Cranelift lowering of `f32` and the float intrinsics and conversions of
//! `vesper.float.ieee754/1` (see [`crate::numeric::float`]).
//!
//! Every operation is a Cranelift instruction, so the NaN canonicalisation
//! of DetFloat programs applies to exactly the operations the contract
//! lists. `fma`, `floor`, `ceil` and `trunc` become correctly rounded
//! instructions where the ISA has them and C library calls (`fma`, `floor`,
//! ...) otherwise; both give the same bits. `round` (ties away from zero) is
//! `trunc` plus an exact adjustment.
use super::*;
use crate::numeric::IntegerKind;
use crate::numeric::float::{FloatKind, MathOp};
use cranelift_codegen::ir::MemFlags;

/// Whether `t` is a Cranelift float type.
pub(super) fn is_float_type(t: ir::Type) -> bool {
    t == types::F32 || t == types::F64
}
/// The Cranelift type of a float kind.
pub(super) fn float_type(k: FloatKind) -> ir::Type {
    match k {
        FloatKind::F32 => types::F32,
        FloatKind::F64 => types::F64,
    }
}

/// JIT runtime symbol for `fmodf` (the `f32` remainder).
pub(super) extern "C" fn native_float_remainder_f32(a: f32, b: f32) -> f32 {
    a % b
}

impl<M: Module> Codegen<'_, '_, M> {
    /// The float constant `x` of Cranelift type `ty` (`F32` or `F64`).
    pub(super) fn float_const(&mut self, ty: ir::Type, x: f64) -> ir::Value {
        if ty == types::F32 {
            self.b.ins().f32const(x as f32)
        } else {
            self.b.ins().f64const(x)
        }
    }
    /// The zero of a scalar Cranelift type.
    pub(super) fn scalar_zero(&mut self, ty: ir::Type) -> ir::Value {
        if is_float_type(ty) {
            self.float_const(ty, 0.0)
        } else if ty == types::I128 {
            self.wide_const(0)
        } else {
            self.b.ins().iconst(ty, 0)
        }
    }
    /// A one-operand float intrinsic or conversion `name` of a `from`
    /// operand (`sqrt`, `nearest_f32`, `saturating_u8`, `to_bits_f64`,
    /// `narrow_i32` of a float, ...), or `None` when `name` is not one.
    pub(super) fn float_unary(
        &mut self,
        name: &str,
        from: &Type,
        v: ir::Value,
        span: &Span,
    ) -> Result<Option<ir::Value>, String> {
        use crate::numeric::float::*;
        let flush = self.flushes(span);
        if let Some(op) = math_intrinsic(name) {
            if op.arity() != 1 {
                return Ok(None);
            }
            let v = self.flush(flush, v);
            let r = self.math(op, &[v])?;
            return Ok(Some(self.flush(flush, r)));
        }
        if let Some((_, to)) = float_conversion(name) {
            let target = float_type(to);
            let r = if let Some(kind) = FloatKind::from_type(from) {
                let v = self.flush(flush, v);
                match (kind, to) {
                    (FloatKind::F32, FloatKind::F64) => self.b.ins().fpromote(target, v),
                    (FloatKind::F64, FloatKind::F32) => self.b.ins().fdemote(target, v),
                    _ => v,
                }
            } else {
                let kind = IntegerKind::from_type(from)
                    .ok_or("native float conversion of a non-numeric value")?;
                if kind.signed() {
                    self.b.ins().fcvt_from_sint(target, v)
                } else {
                    self.b.ins().fcvt_from_uint(target, v)
                }
            };
            return Ok(Some(self.flush(flush, r)));
        }
        if let Some(to) = saturating_conversion(name) {
            let v = self.flush(flush, v);
            return Ok(Some(self.float_to_integer(to, v)));
        }
        if let Some((to_bits, kind)) = bits_intrinsic(name) {
            return Ok(Some(match (to_bits, kind) {
                (true, FloatKind::F64) => self.b.ins().bitcast(types::I64, MemFlags::new(), v),
                (true, FloatKind::F32) => {
                    let bits = self.b.ins().bitcast(types::I32, MemFlags::new(), v);
                    self.b.ins().uextend(types::I64, bits)
                }
                (false, FloatKind::F64) => self.b.ins().bitcast(types::F64, MemFlags::new(), v),
                (false, FloatKind::F32) => {
                    let bits = self.b.ins().ireduce(types::I32, v);
                    self.b.ins().bitcast(types::F32, MemFlags::new(), bits)
                }
            }));
        }
        if let Some((crate::numeric::Conversion::Narrow, to)) = crate::numeric::conversion(name)
            && let Some(kind) = FloatKind::from_type(from)
        {
            let v = self.flush(flush, v);
            if self.world.overflow_ops.contains(&(span.start, span.end)) {
                let ty = float_type(kind);
                let t = self.b.ins().trunc(v);
                let (lo, hi) = truncation_bounds(to);
                let lo = self.float_const(ty, lo);
                let hi = self.float_const(ty, hi);
                // Ordered comparisons: a NaN fails both.
                let above = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, t, lo);
                let below = self.b.ins().fcmp(FloatCC::LessThan, t, hi);
                let ok = self.b.ins().band(above, below);
                let overflow = self.b.ins().icmp_imm(IntCC::Equal, ok, 0);
                self.check_overflow(overflow)?;
            }
            return Ok(Some(self.float_to_integer(to, v)));
        }
        Ok(None)
    }
    /// Truncation toward zero, clamped to `to`'s range, NaN to zero, in
    /// `to`'s canonical 64-bit lane.
    fn float_to_integer(&mut self, to: IntegerKind, v: ir::Value) -> ir::Value {
        if to.signed() {
            let n = self.b.ins().fcvt_to_sint_sat(types::I64, v);
            if to.bits() == 64 {
                return n;
            }
            let lo = self.b.ins().iconst(types::I64, to.min() as i64);
            let hi = self.b.ins().iconst(types::I64, to.max() as i64);
            let n = self.b.ins().smax(n, lo);
            self.b.ins().smin(n, hi)
        } else {
            let n = self.b.ins().fcvt_to_uint_sat(types::I64, v);
            if to.bits() == 64 {
                return n;
            }
            let hi = self.b.ins().iconst(types::I64, to.max() as i64);
            self.b.ins().umin(n, hi)
        }
    }
    /// The math intrinsic `op` on operands of one float type.
    pub(super) fn math(&mut self, op: MathOp, args: &[ir::Value]) -> Result<ir::Value, String> {
        if args.len() != op.arity() {
            return Err(format!(
                "native '{}' expects {} operands",
                op.name(),
                op.arity()
            ));
        }
        let ty = self.b.func.dfg.value_type(args[0]);
        if !is_float_type(ty) {
            return Err(format!("native '{}' of a non-float value", op.name()));
        }
        let ins = self.b.ins();
        Ok(match op {
            MathOp::Sqrt => ins.sqrt(args[0]),
            MathOp::Abs => ins.fabs(args[0]),
            MathOp::Floor => ins.floor(args[0]),
            MathOp::Ceil => ins.ceil(args[0]),
            MathOp::Trunc => ins.trunc(args[0]),
            MathOp::Min => ins.fmin(args[0], args[1]),
            MathOp::Max => ins.fmax(args[0], args[1]),
            MathOp::Fma => ins.fma(args[0], args[1], args[2]),
            MathOp::Round => {
                // C `round`: t = trunc(x); |x - t| >= 0.5 moves t one away
                // from zero. x - t is exact, and so is t ± 1.
                let x = args[0];
                let t = self.b.ins().trunc(x);
                let d = self.b.ins().fsub(x, t);
                let d = self.b.ins().fabs(d);
                let half = self.float_const(ty, 0.5);
                let far = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, d, half);
                let one = self.float_const(ty, 1.0);
                let step = self.b.ins().fcopysign(one, x);
                let away = self.b.ins().fadd(t, step);
                self.b.ins().select(far, away, t)
            }
        })
    }
    /// A float intrinsic called by name with `args` (`sqrt(x)`, `min(a, b)`,
    /// `fma(a, b, c)`, conversions), or `None` when `name` is not one.
    pub(super) fn float_call(
        &mut self,
        name: &str,
        args: &[Arg],
        site: &Expr,
    ) -> Result<Option<ir::Value>, String> {
        if !crate::numeric::float::is_float_intrinsic(name)
            && !(crate::numeric::conversion(name).is_some()
                && args.len() == 1
                && FloatKind::from_type(&self.ty(&args[0].expr)).is_some())
        {
            return Ok(None);
        }
        if let Some(op) = crate::numeric::float::math_intrinsic(name)
            && op.arity() > 1
        {
            let flush = self.flushes(&site.span);
            let mut values = vec![];
            for a in args {
                let v = self.expr(&a.expr)?;
                values.push(self.flush(flush, v));
            }
            let r = self.math(op, &values)?;
            return Ok(Some(self.flush(flush, r)));
        }
        let [a] = args else {
            return Err(format!("native '{name}' expects one operand"));
        };
        let from = self.ty(&a.expr);
        let v = self.expr(&a.expr)?;
        let r = self.float_unary(name, &from, v, &site.span)?;
        Ok(r.map(|r| self.coerce(r, Some(&self.ty(site)))))
    }
}
