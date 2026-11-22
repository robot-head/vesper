//! Lean 4 extraction of `ChunkSeq[T]` and `Slice[T]` (§13) as `Array T`.
//!
//! Element reads and writes raise `Fail[RuntimeError]` `IndexOutOfBounds`
//! exactly as the backends do; `try_get` is `a[i]?`. A slice is the copied
//! subarray `a.extract lo hi`, which is exact for `in` views. `inout` slice
//! arguments write through to their sequence, which the copied model cannot
//! express, so extraction refuses them. Interval faults (`s[a..b]` outside
//! the sequence) are run-time faults outside the model, like `TiledSeq`
//! bounds faults.
use super::{
    DS, Generator, Statements, bind_local, let_local, pure, reference, runtime_failure, sequence,
};
use crate::ast::*;
use crate::lean_nodes::global;
use crate::seq::{self, SeqOp};
use vesper_lean_syntax::{BinaryOp, Term, UnaryOp};

fn nat(t: Term) -> Term {
    crate::lean_nodes::project(t, "toNat")
}

fn size(s: Term) -> Term {
    Term::app(global(&["Array", "size"]), [s])
}

fn in_bounds(s: Term, i: Term) -> Term {
    Term::app(
        global(&["Decidable", "decide"]),
        [Term::binary(BinaryOp::Lt, nat(i), size(s))],
    )
}

fn unless(condition: Term, failure: Term) -> DS {
    DS::If {
        condition: Term::Unary {
            op: UnaryOp::BoolNot,
            value: Box::new(condition),
        },
        yes: vec![DS::Expr(failure)],
        no: None,
    }
}

/// Refusal for the `inout` slice arguments the copied model cannot express.
pub(super) const INOUT_SLICE: &str =
    "Lean extraction does not model inout Slice arguments: they write through to their sequence";

impl Generator<'_> {
    /// A checked action `do { unless ok fail; pure value }` bound to a fresh
    /// local, as the other partial operations are.
    fn seq_checked(&mut self, ok: Term, value: Term, out: &mut Statements) -> Result<Term, String> {
        self.sync(out);
        self.guard_abort(true)?;
        let t = self.temp();
        let action = sequence(
            vec![unless(ok, runtime_failure("IndexOutOfBounds"))],
            pure(value),
        );
        bind_local(out, t.clone(), action, None);
        Ok(reference(&t))
    }

    fn seq_update(&mut self, recv: &Expr, value: Term, out: &mut Statements) -> Result<(), String> {
        let place = crate::check::Place::from_expr(recv)
            .ok_or("a mutable Lean ChunkSeq receiver requires a place")?;
        self.assign(&place, value, out)
    }

    /// Extracts a sequence call, element index or slice view, or `None`.
    pub(super) fn seq_expr(
        &mut self,
        e: &Expr,
        out: &mut Statements,
    ) -> Option<Result<Term, String>> {
        let (op, operands): (SeqOp, Vec<&Expr>) = match &e.kind {
            ExprKind::Index(base, index) => {
                if let Some((base, lo, hi)) = seq::interval(e) {
                    (SeqOp::SubSlice, vec![base, lo, hi])
                } else {
                    let t = self.type_of(base)?;
                    let op = if seq::is_seq_type(&t) {
                        SeqOp::Get
                    } else if seq::is_slice_type(&t) {
                        SeqOp::SliceGet
                    } else {
                        return None;
                    };
                    (op, vec![base, index])
                }
            }
            ExprKind::Call { callee, args, .. } => match &callee.kind {
                ExprKind::Name(n) if !self.scope.contains_key(n) => {
                    if self.world.functions.contains_key(n) {
                        return None;
                    }
                    (
                        seq::resolve(n, None)?,
                        args.iter().map(|a| &a.expr).collect(),
                    )
                }
                ExprKind::Field(base, method) => {
                    let t = self.type_of(base)?;
                    let op = seq::resolve(method, Some(&t))?.with_arity(args.len());
                    let mut operands = vec![base.as_ref()];
                    operands.extend(args.iter().map(|a| &a.expr));
                    (op, operands)
                }
                _ => return None,
            },
            _ => return None,
        };
        Some(self.seq_op(op, &operands, out))
    }

    fn seq_op(&mut self, op: SeqOp, es: &[&Expr], out: &mut Statements) -> Result<Term, String> {
        if let SeqOp::Str(op) = op {
            return Err(crate::strings::lean_refusal(op));
        }
        if matches!(
            op,
            SeqOp::CopyFrom { .. }
                | SeqOp::CopyWithin { .. }
                | SeqOp::Fill
                | SeqOp::Resize
                | SeqOp::FillRange { .. }
                | SeqOp::ExtendFromWithin { .. }
                | SeqOp::ExtendReversed
                | SeqOp::Wide(_)
                | SeqOp::Swap
                | SeqOp::Replace
                | SeqOp::Vector(_)
        ) || op.byte_scan()
        {
            return Err(format!(
                "the Lean model of sequences has no ChunkSeq::{} yet",
                op.name()
            ));
        }
        if matches!(op, SeqOp::Hoisted(_) | SeqOp::BoundsGuard { .. }) {
            return Err(format!(
                "the Lean model has no native-only ChunkSeq::{}",
                op.name()
            ));
        }
        if op == SeqOp::SubSlice
            && es
                .first()
                .and_then(|r| self.type_of(r))
                .is_some_and(|t| crate::strings::is_text(&t))
        {
            return Err(crate::strings::lean_refusal(crate::strings::StrOp::SubStr));
        }
        if matches!(op, SeqOp::SliceSet)
            || (op == SeqOp::TrySet
                && es
                    .first()
                    .and_then(|r| self.type_of(r))
                    .is_some_and(|t| seq::is_slice_type(&t)))
        {
            return Err(INOUT_SLICE.into());
        }
        let terms = self.operands(es, out)?;
        let at = |i: usize| terms[i].clone();
        Ok(match op {
            SeqOp::New => Term::app(
                global(&["Array", "emptyWithCapacity"]),
                [Term::Nat(0u64.into())],
            ),
            SeqOp::WithCapacity => Term::app(global(&["Array", "emptyWithCapacity"]), [nat(at(0))]),
            SeqOp::Len | SeqOp::SliceLen => Term::app(global(&["Nat", "toUInt64"]), [size(at(0))]),
            SeqOp::Push => {
                let value = Term::app(global(&["Array", "push"]), [at(0), at(1)]);
                self.seq_update(es[0], value, out)?;
                crate::lean_nodes::unit()
            }
            SeqOp::Pop => {
                let t = self.temp();
                let_local(
                    out,
                    t.clone(),
                    Term::app(global(&["Array", "back?"]), [at(0)]),
                    None,
                    false,
                );
                let value = Term::app(global(&["Array", "pop"]), [at(0)]);
                self.seq_update(es[0], value, out)?;
                reference(&t)
            }
            SeqOp::Get | SeqOp::SliceGet => {
                let value = Term::app(global(&["GetElem?", "getElem!"]), [at(0), nat(at(1))]);
                self.seq_checked(in_bounds(at(0), at(1)), value, out)?
            }
            SeqOp::Set => {
                let value = Term::app(
                    global(&["Array", "setIfInBounds"]),
                    [at(0), nat(at(1)), at(2)],
                );
                let updated = self.seq_checked(in_bounds(at(0), at(1)), value, out)?;
                self.seq_update(es[0], updated, out)?;
                crate::lean_nodes::unit()
            }
            SeqOp::TryGet => Term::app(global(&["GetElem?", "getElem?"]), [at(0), nat(at(1))]),
            SeqOp::Last => Term::app(global(&["Array", "back?"]), [at(0)]),
            SeqOp::TrySet => {
                let t = self.temp();
                let_local(out, t.clone(), in_bounds(at(0), at(1)), None, false);
                let value = Term::app(
                    global(&["Array", "setIfInBounds"]),
                    [at(0), nat(at(1)), at(2)],
                );
                self.seq_update(es[0], value, out)?;
                reference(&t)
            }
            SeqOp::Clear => {
                let value = Term::app(
                    global(&["Array", "emptyWithCapacity"]),
                    [Term::Nat(0u64.into())],
                );
                self.seq_update(es[0], value, out)?;
                crate::lean_nodes::unit()
            }
            SeqOp::Truncate => {
                let value = Term::app(
                    global(&["Array", "extract"]),
                    [at(0), Term::Nat(0u64.into()), nat(at(1))],
                );
                self.seq_update(es[0], value, out)?;
                crate::lean_nodes::unit()
            }
            // A capacity hint leaves the modelled array unchanged.
            SeqOp::Reserve => crate::lean_nodes::unit(),
            SeqOp::Extend => {
                let value = Term::app(global(&["Array", "append"]), [at(0), at(1)]);
                self.seq_update(es[0], value, out)?;
                crate::lean_nodes::unit()
            }
            SeqOp::Finish => {
                let empty = Term::binary(BinaryOp::BoolEq, size(at(0)), Term::Nat(0u64.into()));
                self.seq_checked(empty, crate::lean_nodes::unit(), out)?
            }
            SeqOp::AsSlice => at(0),
            SeqOp::SubSlice => Term::app(
                global(&["Array", "extract"]),
                [at(0), nat(at(1)), nat(at(2))],
            ),
            SeqOp::SliceSet
            | SeqOp::Str(_)
            | SeqOp::Hoisted(_)
            | SeqOp::BoundsGuard { .. }
            | SeqOp::CopyFrom { .. }
            | SeqOp::CopyWithin { .. }
            | SeqOp::Swap
            | SeqOp::Replace
            | SeqOp::Fill
            | SeqOp::Resize
            | SeqOp::FillRange { .. }
            | SeqOp::ExtendFromWithin { .. }
            | SeqOp::ExtendReversed
            | SeqOp::Wide(_)
            | SeqOp::SumBe16
            | SeqOp::BytesEq
            | SeqOp::BytesCmp
            | SeqOp::FindByte
            | SeqOp::FindEither
            | SeqOp::FindNotIn
            | SeqOp::EqIgnoreAsciiCase
            | SeqOp::ExtendAsciiLowercase
            | SeqOp::MakeAsciiLowercase { .. }
            | SeqOp::Vector(_) => {
                unreachable!("refused above")
            }
        })
    }
}
