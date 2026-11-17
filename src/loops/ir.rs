//! Place IR lowering of loops and early exits (see [`crate::loops`]).
//!
//! `loop { b }` becomes a header block starting with [`Op::Loop`] whose body
//! jumps back to it; `break v` jumps to the exit block with `v` as the exit's
//! block parameter. `for p in a..b { body }` binds hidden index and bound
//! slots, tests `index < bound` (`<=` for `a..=b`) in the header, and
//! increments the index in a latch block; a closed range leaves before an
//! increment past its bound, so the increment never overflows. `return v`
//! is [`Op::Exit`] followed by an `Unreachable` terminator. Code after a jump
//! is lowered into a fresh block without predecessors.
use super::{Instruction, Lower, Op, Terminator};
use crate::ast::*;

#[derive(Debug, Clone)]
pub(super) struct LoopTarget {
    exit: usize,
    /// The exit block takes the `break` value as its parameter (`loop`).
    value: bool,
}

impl Lower<'_> {
    fn emit_at(&mut self, op: Op, span: Span, ty: Type) -> u32 {
        let dest = self.fresh();
        self.values.insert(dest, ty.clone());
        self.blocks[self.current].instructions.push(Instruction {
            dest,
            op,
            ty,
            span,
            token: None,
            fallible: false,
        });
        dest
    }
    fn type_at(&self, span: &Span) -> Type {
        crate::numeric::default_type(
            self.types
                .get(&(span.start, span.end))
                .unwrap_or(&Type::Infer),
        )
    }
    /// Continues after a jump in a block nothing reaches.
    fn after_jump(&mut self, e: &Expr) -> u32 {
        self.current = self.new_block();
        self.emit(Op::Unit, e, false)
    }

    pub(super) fn control(&mut self, e: &Expr) -> u32 {
        match &e.kind {
            ExprKind::Loop(body) => {
                let ty = self.type_at(&e.span);
                let param_ty = if ty.name() == "Never" {
                    Type::unit()
                } else {
                    ty
                };
                let header = self.new_block();
                let exit = self.new_block();
                let result = self.fresh();
                self.values.insert(result, param_ty);
                self.blocks[exit].params.push(result);
                self.blocks[self.current].terminator = Terminator::Jump {
                    block: header,
                    args: vec![],
                };
                self.current = header;
                self.emit_at(Op::Loop, e.span.clone(), Type::unit());
                self.loops.push(LoopTarget { exit, value: true });
                let outer = self.names.clone();
                self.expr(body);
                self.names = outer;
                self.loops.pop();
                self.blocks[self.current].terminator = Terminator::Jump {
                    block: header,
                    args: vec![],
                };
                self.current = exit;
                result
            }
            ExprKind::For { pat, iter, body } => {
                let ExprKind::Range {
                    start,
                    end,
                    inclusive,
                } = &iter.kind
                else {
                    return self.emit(Op::Unit, e, false);
                };
                let element = self.type_at(&start.span);
                // Synthetic instructions use the zero-width spans the checker
                // typed: the element type at the range start, bool at its end.
                let at = iter.span.start..iter.span.start;
                let test_at = iter.span.end..iter.span.end;
                let lo = self.expr(start);
                let hi = self.expr(end);
                let index = format!("$loop{}_index", self.fresh());
                let bound = format!("$loop{}_bound", self.fresh());
                for (slot, value) in [(&index, lo), (&bound, hi)] {
                    self.locals.insert(slot.clone(), element.clone());
                    self.emit_at(
                        Op::Bind {
                            pattern: Pattern {
                                kind: PatternKind::Bind(slot.clone()),
                                span: at.clone(),
                            },
                            value,
                        },
                        at.clone(),
                        Type::unit(),
                    );
                }
                let read = |l: &mut Self, slot: &String| {
                    l.emit_at(
                        Op::Read {
                            root: slot.clone(),
                            path: vec![],
                            pass: Pass::Sink,
                            projections: vec![],
                        },
                        at.clone(),
                        element.clone(),
                    )
                };
                let header = self.new_block();
                let body_block = self.new_block();
                let exit = self.new_block();
                self.blocks[self.current].terminator = Terminator::Jump {
                    block: header,
                    args: vec![],
                };
                self.current = header;
                self.emit_at(Op::Loop, e.span.clone(), Type::unit());
                let i = read(self, &index);
                let n = read(self, &bound);
                let op = if *inclusive { "<=" } else { "<" };
                let condition = self.emit_at(
                    Op::Binary(op.into(), i, n),
                    test_at.clone(),
                    Type::named("bool"),
                );
                self.blocks[self.current].terminator = Terminator::Branch {
                    condition,
                    yes: body_block,
                    no: exit,
                };
                self.current = body_block;
                let outer = self.names.clone();
                let i = read(self, &index);
                let pattern = self.rename_pattern(pat, &element);
                self.emit_at(
                    Op::Bind { pattern, value: i },
                    pat.span.clone(),
                    Type::unit(),
                );
                self.loops.push(LoopTarget { exit, value: false });
                self.expr(body);
                self.loops.pop();
                self.names = outer;
                let latch = self.new_block();
                if *inclusive {
                    // Leave at the bound instead of incrementing past it.
                    let i = read(self, &index);
                    let n = read(self, &bound);
                    let last = self.emit_at(
                        Op::Binary("==".into(), i, n),
                        test_at.clone(),
                        Type::named("bool"),
                    );
                    self.blocks[self.current].terminator = Terminator::Branch {
                        condition: last,
                        yes: exit,
                        no: latch,
                    };
                } else {
                    self.blocks[self.current].terminator = Terminator::Jump {
                        block: latch,
                        args: vec![],
                    };
                }
                self.current = latch;
                let i = read(self, &index);
                let one = self.emit_at(Op::Constant(Literal::Int(1)), at.clone(), element.clone());
                let next =
                    self.emit_at(Op::Binary("+".into(), i, one), at.clone(), element.clone());
                self.emit_at(
                    Op::Write {
                        place: index,
                        op: "=".into(),
                        value: next,
                    },
                    at,
                    Type::unit(),
                );
                self.blocks[self.current].terminator = Terminator::Jump {
                    block: header,
                    args: vec![],
                };
                self.current = exit;
                self.emit(Op::Unit, e, false)
            }
            ExprKind::Range { .. } => self.emit(Op::Unit, e, false),
            ExprKind::Break(value) => {
                let Some(target) = self.loops.last().cloned() else {
                    return self.emit(Op::Unit, e, false);
                };
                let v = match value {
                    Some(v) => self.expr(v),
                    None => self.emit_at(Op::Unit, e.span.clone(), Type::unit()),
                };
                self.blocks[self.current].terminator = Terminator::Jump {
                    block: target.exit,
                    args: if target.value { vec![v] } else { vec![] },
                };
                self.after_jump(e)
            }
            ExprKind::Return(value) => {
                let v = match value {
                    Some(v) => self.expr(v),
                    None => self.emit_at(Op::Unit, e.span.clone(), Type::unit()),
                };
                let owner = self.owner.clone();
                self.emit(Op::Exit { value: v, owner }, e, true);
                self.blocks[self.current].terminator = Terminator::Unreachable;
                self.after_jump(e)
            }
            _ => self.emit(Op::Unit, e, false),
        }
    }
}
