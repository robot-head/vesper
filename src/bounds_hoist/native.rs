//! Cranelift lowering of the operations [`crate::bounds_hoist`] creates: the
//! loop guard [`SeqOp::BoundsGuard`] and the unchecked element accesses
//! [`SeqOp::Hoisted`] of a counted loop's fast copy.
use super::*;
use crate::seq::Access;

/// The scale operand and outer terms of a scaled guard form (#2022).
pub(super) struct Scale {
    scale: ir::Value,
    outer_plus: Vec<ir::Value>,
    outer_minus: Vec<ir::Value>,
}

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
                if self.seq_hook_active() {
                    let (is_seq, inv, _, c0, c1, c2, _) = self.seq_cached_view(arg(2)?, ty);
                    let (np, nm) = (usize::from(form.plus), usize::from(form.minus));
                    let ok = args.get(3 + form.operands()).copied();
                    let mut req = Vec::with_capacity(19 + form.operands());
                    req.extend_from_slice(&[
                        u64::from(inclusive),
                        u64::from(is_seq),
                        u64::from(inv),
                        u64::from(form.negated),
                        u64::from(ok.is_some()),
                        crate::codegen_swap::word(lo),
                        crate::codegen_swap::word(hi),
                        crate::codegen_swap::word(arg(2)?),
                        ok.map_or(0, crate::codegen_swap::word),
                        c0,
                        c1,
                        c2,
                        np as u64,
                        nm as u64,
                    ]);
                    for k in 0..np {
                        req.push(crate::codegen_swap::word(arg(3 + k)?));
                    }
                    for k in 0..nm {
                        req.push(crate::codegen_swap::word(arg(3 + np + k)?));
                    }
                    // The scaled form's scale and outer terms (#2022).
                    let (op_, om) = (usize::from(form.outer_plus), usize::from(form.outer_minus));
                    req.extend_from_slice(&[
                        u64::from(form.scaled),
                        u64::from(form.width),
                        op_ as u64,
                        om as u64,
                    ]);
                    if form.scaled {
                        for k in 0..1 + op_ + om {
                            req.push(crate::codegen_swap::word(arg(3 + np + nm + k)?));
                        }
                    }
                    if let Some(res) =
                        self.seq_swap_req(crate::codegen_swap::seq::BOUNDS_GUARD, &req)?
                    {
                        return crate::codegen_swap::value_of(&res).map(Some);
                    }
                }
                let (_, _, len) = self.seq_view(arg(2)?, ty);
                let (np, nm) = (usize::from(form.plus), usize::from(form.minus));
                let plus: Vec<ir::Value> = (0..np).map(|k| arg(3 + k)).collect::<Result<_, _>>()?;
                let minus: Vec<ir::Value> =
                    (0..nm).map(|k| arg(3 + np + k)).collect::<Result<_, _>>()?;
                let scale = if form.scaled {
                    let at = 3 + np + nm;
                    let (op_, om) = (usize::from(form.outer_plus), usize::from(form.outer_minus));
                    let outer_plus: Vec<ir::Value> = (0..op_)
                        .map(|k| arg(at + 1 + k))
                        .collect::<Result<_, _>>()?;
                    let outer_minus: Vec<ir::Value> = (0..om)
                        .map(|k| arg(at + 1 + op_ + k))
                        .collect::<Result<_, _>>()?;
                    Some(Scale {
                        scale: arg(at)?,
                        outer_plus,
                        outer_minus,
                    })
                } else {
                    None
                };
                let ok = args.get(3 + form.operands()).copied();
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
                let fits = self.guard_range_fits(
                    lo,
                    last,
                    form.negated,
                    &plus,
                    &minus,
                    scale.as_ref(),
                    form.width,
                    len,
                );
                let inside = self.b.ins().bor(empty, fits);
                let inside = self.b.ins().uextend(types::I64, inside);
                self.jump(done, &[inside.into()]);
                self.b.switch_to_block(done);
                Ok(Some(self.b.block_params(done)[0]))
            }
            // A fixed-width access lowers with the others (`byte_seq_op`).
            SeqOp::Hoisted(Access::Wide(_)) => Ok(None),
            SeqOp::Hoisted(access) => {
                let ty = arg_types
                    .first()
                    .ok_or("native hoisted access lacks its receiver")?;
                let elem = seq::elem(ty)
                    .cloned()
                    .ok_or("native hoisted access lacks its element type")?;
                let elem = crate::numeric::default_type(&elem);
                let stride = self.seq_stride(&elem)?;
                if self.seq_hook_active() {
                    use crate::codegen_swap::{seq as sops, value_of, word};
                    let (is_seq, inv, h_inv, c0, c1, c2, cdata) = self.seq_cached_view(arg(0)?, ty);
                    let desc = self.seq_elem_desc(&elem)?;
                    let (kind, val) = match access {
                        Access::Get => (sops::HOIST_GET, 0),
                        Access::Set => (sops::HOIST_SET, word(arg(2)?)),
                        Access::TryGet => (sops::HOIST_TRY_GET, 0),
                        Access::TrySet => (sops::HOIST_TRY_SET, word(arg(2)?)),
                        Access::Wide(_) => unreachable!("matched above"),
                    };
                    let mut req = vec![
                        kind,
                        u64::from(is_seq),
                        u64::from(inv),
                        u64::from(h_inv),
                        u64::from(self.operands_only),
                        word(arg(0)?),
                        word(arg(1)?),
                        val,
                        c0,
                        c1,
                        c2,
                        cdata,
                        desc[0],
                        desc[1],
                        desc[2],
                    ];
                    let meta = if access == Access::TryGet {
                        let option = Type::Named("Option".into(), vec![elem.clone()]);
                        let m = self.seq_option_meta_for(&option, &elem)?;
                        req.extend_from_slice(&m);
                        Some(m)
                    } else {
                        None
                    };
                    if let Some(res) = self.seq_swap_req(sops::HOISTED_ACCESS, &req)? {
                        return if let Some(m) = meta {
                            self.seq_option_finish_swap(&res, m).map(Some)
                        } else {
                            value_of(&res).map(Some)
                        };
                    }
                }
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
                    Access::Wide(_) => unreachable!("matched above"),
                }))
            }
            _ => Ok(None),
        }
    }

    /// Whether the guard's index is in `0..=len - width` at every loop index
    /// in `lo..=last` (when that range is not empty). The index `c * (±i +
    /// Σplus - Σminus) + Σouter_plus - Σouter_minus` (`c` 1 and no outer
    /// terms unscaled) is monotone in `i`, so its two ends bound it: the end
    /// where it is smallest must not be negative and the end where it is
    /// largest, plus `width - 1`, must be below `len`. The sums and
    /// products at the largest loop index are checked for overflow; those
    /// at `lo` then cannot overflow (`lo <= last`).
    #[allow(clippy::too_many_arguments)]
    fn guard_range_fits(
        &mut self,
        lo: ir::Value,
        last: ir::Value,
        negated: bool,
        plus: &[ir::Value],
        minus: &[ir::Value],
        scale: Option<&Scale>,
        width: u8,
        len: ir::Value,
    ) -> ir::Value {
        if !negated && plus.is_empty() && minus.is_empty() && scale.is_none() && width <= 1 {
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
        let mut at_last = checked(self, Some(last), moving);
        let mut constant = checked(self, None, fixed);
        let mut at_lo = lo;
        for &t in moving {
            at_lo = self.b.ins().iadd(at_lo, t);
        }
        // Scaled: each sum times `c` (checked; `c * at_lo` cannot overflow
        // when `c * at_last` does not), then the outer terms: the outer plus
        // terms join the side that is added, the outer minus terms the side
        // that is subtracted.
        let (mut outer_add, mut outer_sub) = (None, None);
        if let Some(sc) = scale {
            let mut mul = |this: &mut Self, x: ir::Value| {
                let (p, o) = this.b.ins().umul_overflow(x, sc.scale);
                overflow = this.b.ins().bor(overflow, o);
                p
            };
            at_last = mul(self, at_last);
            constant = mul(self, constant);
            at_lo = self.b.ins().imul(at_lo, sc.scale);
            let mut sum = |this: &mut Self, terms: &[ir::Value]| {
                let mut acc: Option<ir::Value> = None;
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
                acc
            };
            outer_add = sum(self, &sc.outer_plus);
            outer_sub = sum(self, &sc.outer_minus);
        }
        // Not negated: `c * (i + Σplus) + Σouter_plus - (c * Σminus +
        // Σouter_minus)` grows with `i`, so it is smallest at `lo` and
        // largest at `last`. Negated: `c * Σplus + Σouter_plus - (c * (i +
        // Σminus) + Σouter_minus)` shrinks, so it is smallest at `last` and
        // largest at `lo`.
        let mut add = |this: &mut Self, x: ir::Value, t: Option<ir::Value>, check: bool| match t {
            None => x,
            Some(t) if check => {
                let (s, o) = this.b.ins().uadd_overflow(x, t);
                overflow = this.b.ins().bor(overflow, o);
                s
            }
            Some(t) => this.b.ins().iadd(x, t),
        };
        let (low_p, low_n, high_p, high_n) = if negated {
            let p = add(self, constant, outer_add, true);
            let n_last = add(self, at_last, outer_sub, true);
            let n_lo = add(self, at_lo, outer_sub, false);
            (p, n_last, p, n_lo)
        } else {
            let p_last = add(self, at_last, outer_add, true);
            let p_lo = add(self, at_lo, outer_add, false);
            let n = add(self, constant, outer_sub, true);
            (p_lo, n, p_last, n)
        };
        let below = self
            .b
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, low_n, low_p);
        let mut d = self.b.ins().isub(high_p, high_n);
        if width > 1 {
            // `d + width - 1 < len`; `d` is at most `high_p`, so the sum
            // only overflows when `high_p` is near 2^64: checked.
            let extra = self.b.ins().iconst(types::I64, i64::from(width) - 1);
            let (s, o) = self.b.ins().uadd_overflow(d, extra);
            overflow = self.b.ins().bor(overflow, o);
            d = s;
        }
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

    /// Reads a sequence or slice field of an invariant aggregate (an `in`
    /// parameter of the function, lent on to a specialized callee, #1330)
    /// where a specialized callee's graph begins: the handle word and the
    /// header words are read-only memory for the whole call, so Cranelift
    /// moves them out of the caller's loops.
    pub(crate) fn seq_invariant_field_view(
        &mut self,
        place: &crate::check::Place,
        ty: &Type,
    ) -> Result<ir::Value, String> {
        let e = ir_place_expr(place, 0..0);
        let address = self.address(&e)?;
        let receiver = if seq::is_slice_type(ty) {
            address
        } else {
            self.invariant_word(address, 0)
        };
        self.invariant_headers.insert(receiver);
        Ok(receiver)
    }

    /// A header word of an invariant view or handle. The memory is valid
    /// and unchanged for the whole call (a borrowed view lives in the
    /// caller, and a sequence's header block outlives every view of it), so
    /// the load cannot trap and may move.
    pub(super) fn invariant_word(&mut self, ptr: ir::Value, offset: i32) -> ir::Value {
        if self.seq_hook_active()
            && let Some(res) = self.seq_swap_req_or_record(
                crate::codegen_swap::seq::INVARIANT_WORD,
                &[crate::codegen_swap::word(ptr), offset as i64 as u64],
            )
            && let Some(v) = crate::codegen_swap::value_or_record(&res)
        {
            return v;
        }
        let flags = MemFlags::trusted().with_readonly().with_can_move();
        self.b.ins().load(types::I64, flags, ptr, offset)
    }
}
