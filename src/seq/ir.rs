//! Place-IR lowering of `ChunkSeq[T]` and `Slice[T]` operations (`Op::Seq`).
use super::{Argument, Lower, Op, ValueId};
use crate::ast::*;
use crate::seq::{SeqOp, interval, resolve, whole_view};

impl Lower<'_> {
    fn seq_type_of(&self, e: &Expr) -> Type {
        self.types
            .get(&(e.span.start, e.span.end))
            .cloned()
            .unwrap_or(Type::Infer)
    }

    /// Lowers `e` when it is a sequence call, element index or slice view.
    pub(super) fn seq_expr(&mut self, e: &Expr) -> Option<ValueId> {
        let (op, operands) = match &e.kind {
            ExprKind::Index(base, index) => {
                if let Some((base, lo, hi)) = interval(e) {
                    let b = self.argument(base, Pass::In, vec![]);
                    let lo = self.argument(lo, Pass::Sink, vec![]);
                    let hi = self.argument(hi, Pass::Sink, vec![]);
                    (SeqOp::SubSlice, vec![b, lo, hi])
                } else {
                    let t = self.seq_type_of(base);
                    let op = if crate::seq::is_seq_type(&t) {
                        SeqOp::Get
                    } else if crate::seq::is_slice_type(&t) {
                        SeqOp::SliceGet
                    } else {
                        return None;
                    };
                    let b = self.argument(base, Pass::In, vec![]);
                    let i = self.argument(index, Pass::Sink, vec![]);
                    (op, vec![b, i])
                }
            }
            ExprKind::Call {
                callee,
                args,
                types,
            } => {
                if let Some((base, _)) = whole_view(e)
                    && crate::seq::is_seq_type(&self.seq_type_of(base))
                {
                    let b = self.argument(base, Pass::In, vec![]);
                    (SeqOp::AsSlice, vec![b])
                } else {
                    match &callee.kind {
                        ExprKind::Name(n) if !self.names.contains_key(n) => {
                            if self.world.functions.contains_key(n) {
                                return None;
                            }
                            let arg_types = args
                                .iter()
                                .map(|a| self.seq_type_of(&a.expr))
                                .collect::<Vec<_>>();
                            if let Some(io) = crate::vector::SliceIo::resolve(
                                n,
                                types,
                                &arg_types,
                                &self.seq_type_of(e),
                            ) {
                                // The sequence is borrowed; the offset and
                                // the stored vector are values.
                                let operands = args
                                    .iter()
                                    .enumerate()
                                    .map(|(i, a)| {
                                        let pass = match (i, io.store) {
                                            (0, true) => Pass::Inout,
                                            (0, false) => Pass::In,
                                            _ => Pass::Sink,
                                        };
                                        self.argument(&a.expr, pass, vec![])
                                    })
                                    .collect();
                                return Some(self.emit(
                                    Op::Seq {
                                        op: SeqOp::Vector(io),
                                        args: operands,
                                    },
                                    e,
                                    true,
                                ));
                            }
                            let op = resolve(n, None)?;
                            // `swap` borrows both places and `replace` its
                            // first one `inout`; other operands are values.
                            let operands = args
                                .iter()
                                .enumerate()
                                .map(|(i, a)| {
                                    let pass =
                                        if op == SeqOp::Swap || (op == SeqOp::Replace && i == 0) {
                                            Pass::Inout
                                        } else {
                                            Pass::Sink
                                        };
                                    self.argument(&a.expr, pass, vec![])
                                })
                                .collect();
                            (op, operands)
                        }
                        ExprKind::Field(base, method) => {
                            let t = self.seq_type_of(base);
                            let op = resolve(method, Some(&t))?.with_arity(args.len());
                            let pass = op.receiver_pass();
                            let mut operands: Vec<Argument> =
                                vec![self.argument(base, pass, vec![])];
                            for (i, a) in args.iter().enumerate() {
                                let pass = if op == SeqOp::Extend
                                    || op == SeqOp::BytesEq
                                    || op == SeqOp::EqIgnoreAsciiCase
                                    || op == SeqOp::ExtendAsciiLowercase
                                    || op == SeqOp::ExtendReversed
                                    || op == SeqOp::Str(crate::strings::StrOp::PushStr)
                                    || (matches!(op, SeqOp::CopyFrom { .. }) && i == 1)
                                {
                                    Pass::In
                                } else {
                                    Pass::Sink
                                };
                                operands.push(self.argument(&a.expr, pass, vec![]));
                            }
                            (op, operands)
                        }
                        _ => return None,
                    }
                }
            }
            _ => return None,
        };
        Some(self.emit(Op::Seq { op, args: operands }, e, true))
    }
}
