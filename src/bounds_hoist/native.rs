//! Cranelift lowering of the operations [`crate::bounds_hoist`] creates: the
//! loop guard [`SeqOp::BoundsGuard`] and the unchecked element accesses
//! [`SeqOp::Hoisted`] of a counted loop's fast copy.
use super::*;
use crate::seq::Access;

/// The shortest `hi - lo` a loop guard checks; a shorter range runs the
/// checked loop without evaluating the guard.
const MIN_GUARDED_SPAN: i64 = 4;

impl<M: Module> Codegen<'_, '_, M> {
    /// Lowers `op` when it is a hoisting operation, else `None`.
    pub(super) fn hoisted_seq_op(
        &mut self,
        op: SeqOp,
        args: &[ir::Value],
        arg_types: &[Type],
    ) -> Result<Option<ir::Value>, String> {
        let arg = |i: usize| {
            args.get(i)
                .copied()
                .ok_or_else(|| format!("native ChunkSeq::{} lacks an operand", op.name()))
        };
        match op {
            SeqOp::BoundsGuard { inclusive, form } => {
                let (lo, hi) = (arg(0)?, arg(1)?);
                let ty = arg_types.get(2).ok_or("bounds guard lacks its sequence")?;
                let (_, _, len) = self.seq_view(arg(2)?, ty);
                let (np, nm) = (usize::from(form.plus), usize::from(form.minus));
                let plus: Vec<ir::Value> = (0..np).map(|k| arg(3 + k)).collect::<Result<_, _>>()?;
                let minus: Vec<ir::Value> =
                    (0..nm).map(|k| arg(3 + np + k)).collect::<Result<_, _>>()?;
                let ok = args.get(3 + np + nm).copied();
                // A short range saves less than the guard costs, and a guard
                // after a failed one need not run: both take the checked
                // loop (the guard may always say no).
                let skip = match ok {
                    Some(ok) => self.b.ins().icmp_imm(IntCC::Equal, ok, 0),
                    None => {
                        let span = self.b.ins().isub(hi, lo);
                        self.b
                            .ins()
                            .icmp_imm(IntCC::UnsignedLessThan, span, MIN_GUARDED_SPAN)
                    }
                };
                let compute = self.b.create_block();
                let done = self.b.create_block();
                self.b.append_block_param(done, types::I64);
                let no = self.zero();
                self.branch(skip, done, &[no.into()], compute, &[]);
                self.b.switch_to_block(compute);
                let empty = if inclusive {
                    IntCC::UnsignedGreaterThan
                } else {
                    IntCC::UnsignedGreaterThanOrEqual
                };
                let empty = self.b.ins().icmp(empty, lo, hi);
                // The last index of a non-empty range (`hi - 1` cannot wrap
                // then: `lo < hi`).
                let last = if inclusive {
                    hi
                } else {
                    self.b.ins().iadd_imm(hi, -1)
                };
                let fits = self.guard_range_fits(lo, last, form.negated, &plus, &minus, len);
                let inside = self.b.ins().bor(empty, fits);
                let inside = self.b.ins().uextend(types::I64, inside);
                self.jump(done, &[inside.into()]);
                self.b.switch_to_block(done);
                Ok(Some(self.b.block_params(done)[0]))
            }
            SeqOp::Hoisted(access) => {
                let ty = arg_types
                    .first()
                    .ok_or("native hoisted access lacks its receiver")?;
                let elem = seq::elem(ty)
                    .cloned()
                    .ok_or("native hoisted access lacks its element type")?;
                let elem = crate::numeric::default_type(&elem);
                let stride = self.seq_stride(&elem)?;
                let (h, start, _) = self.seq_view(arg(0)?, ty);
                let i = arg(1)?;
                Ok(Some(match access {
                    Access::Get => {
                        let addr = self.seq_elem_addr(h, start, i, stride);
                        self.seq_load_elem(addr, &elem)?
                    }
                    Access::Set | Access::TrySet => {
                        let addr = self.seq_elem_addr(h, start, i, stride);
                        self.seq_store_elem(addr, arg(2)?, &elem)?;
                        if access == Access::Set {
                            self.zero()
                        } else {
                            self.b.ins().iconst(types::I64, 1)
                        }
                    }
                    Access::TryGet => {
                        let option = Type::Named("Option".into(), vec![elem.clone()]);
                        let present = self.b.ins().iconst(types::I8, 1);
                        self.seq_option(&option, &elem, present, |this| {
                            let addr = this.seq_elem_addr(h, start, i, stride);
                            this.seq_load_elem(addr, &elem)
                        })?
                    }
                }))
            }
            _ => Ok(None),
        }
    }

    /// Whether the guard's index is in `0..len` at every loop index in
    /// `lo..=last` (when that range is not empty). The index `±i + Σplus -
    /// Σminus` is monotone in `i`, so its two ends bound it: the end where
    /// it is smallest must not be negative and the end where it is largest
    /// must be below `len`. The sums at the largest loop index are checked
    /// for overflow; the sums at `lo` then cannot overflow (`lo <= last`).
    fn guard_range_fits(
        &mut self,
        lo: ir::Value,
        last: ir::Value,
        negated: bool,
        plus: &[ir::Value],
        minus: &[ir::Value],
        len: ir::Value,
    ) -> ir::Value {
        if !negated && plus.is_empty() && minus.is_empty() {
            // `s[i]`: in bounds when the last index is.
            return self.b.ins().icmp(IntCC::UnsignedLessThan, last, len);
        }
        let mut overflow = self.b.ins().iconst(types::I8, 0);
        let mut checked = |this: &mut Self, first: Option<ir::Value>, terms: &[ir::Value]| {
            let mut acc: Option<ir::Value> = first;
            for &t in terms {
                acc = Some(match acc {
                    None => t,
                    Some(a) => {
                        let (s, o) = this.b.ins().uadd_overflow(a, t);
                        overflow = this.b.ins().bor(overflow, o);
                        s
                    }
                });
            }
            acc.unwrap_or_else(|| this.b.ins().iconst(types::I64, 0))
        };
        // The loop-dependent sum (`i + Σplus`, or `i + Σminus` when
        // negated) at `last`, checked, and the constant sum, checked.
        let (moving, fixed) = if negated {
            (minus, plus)
        } else {
            (plus, minus)
        };
        let at_last = checked(self, Some(last), moving);
        let constant = checked(self, None, fixed);
        let mut at_lo = lo;
        for &t in moving {
            at_lo = self.b.ins().iadd(at_lo, t);
        }
        // Not negated: `i + Σplus - Σminus` grows with `i`, so it is
        // smallest at `lo` and largest at `last`. Negated: `Σplus - (i +
        // Σminus)` shrinks, so it is smallest at `last` and largest at `lo`.
        let (low_p, low_n, high_p, high_n) = if negated {
            (constant, at_last, constant, at_lo)
        } else {
            (at_lo, constant, at_last, constant)
        };
        let below = self
            .b
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, low_n, low_p);
        let d = self.b.ins().isub(high_p, high_n);
        let inside = self.b.ins().icmp(IntCC::UnsignedLessThan, d, len);
        let ok = self.b.ins().band(below, inside);
        let ok_flag = self.b.ins().icmp_imm(IntCC::Equal, overflow, 0);
        self.b.ins().band(ok, ok_flag)
    }

    /// Reads a borrowed parameter that nothing in the function changes
    /// ([`crate::bounds_hoist::stable_views`]) once, at function entry: its
    /// view and handle words are then loaded as read-only memory, which
    /// Cranelift shares between accesses and moves out of loops, so a store
    /// through another sequence no longer reloads them.
    pub(crate) fn seq_entry_view(
        &mut self,
        place: &crate::check::Place,
        ty: &Type,
    ) -> Result<ir::Value, String> {
        let e = ir_place_expr(place, 0..0);
        let receiver = if seq::is_slice_type(ty) {
            self.address(&e)?
        } else {
            self.expr(&e)?
        };
        self.invariant_headers.insert(receiver);
        Ok(receiver)
    }

    /// A header word of an invariant view or handle. The memory is valid
    /// and unchanged for the whole call (a borrowed view lives in the
    /// caller, and a sequence's header block outlives every view of it), so
    /// the load cannot trap and may move.
    pub(super) fn invariant_word(&mut self, ptr: ir::Value, offset: i32) -> ir::Value {
        let flags = MemFlags::trusted().with_readonly().with_can_move();
        self.b.ins().load(types::I64, flags, ptr, offset)
    }
}
