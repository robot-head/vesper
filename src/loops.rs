//! Loops and early exits (manuscript ch. 2.2 `LoopExpr`, `ForExpr`,
//! `BreakExpr`, `ReturnExpr` and `LetStmt` with `mut`; port wave P0 lane 4).
//!
//! * `loop { body }` repeats its body until a `break`. Its type is the join
//!   of its `break` values, or `Never` when no `break` leaves it.
//! * `for pat in a..b { body }` and `for pat in a..=b { body }` iterate an
//!   integer interval in ascending order. The bounds are evaluated once,
//!   before the first iteration; the increment never overflows. Its type is
//!   `Unit`, and a `break` inside it takes no value.
//! * `break` and `break value` leave the innermost loop of the enclosing
//!   function, closure or handled body.
//! * `continue` starts the next iteration of the innermost loop: a `loop`
//!   jumps to its header, a `for` steps its index (and leaves a closed range
//!   at its bound) first.
//! * `return` and `return value` leave the enclosing function or closure,
//!   including from inside a handled body (which abandons the handler).
//! * `let mut` declares an assignable binding.
//!
//! The checker (`loops/check.rs`) types them and enforces the mode rules: a
//! value moved in a loop body must not be live across the back edge, and a
//! `break` or `return` must not drop a live `linear` value. The Place IR
//! (`loops/ir.rs`) lowers them to a cyclic CFG whose loop headers start with
//! [`crate::ir::Op::Loop`], so backends without loop support (the verified
//! backend, Wasm, eBPF, batch JIT) refuse them through their typed
//! unsupported-operation paths. The evaluator and the native Cranelift
//! backend run the CFG directly in constant stack.

/// The Lean extractor's typed refusal for loops and early exits.
pub const LEAN_REFUSAL: &str = "Lean extraction does not model loop, for, break or return yet; \
    write the iteration as a tail-recursive function";

/// The native backends' typed refusal for `Box[T]` values in a function
/// with loops or early exits whose drops the elaboration could not plan
/// ([`crate::fip::loop_drops_elaborated`]): an `@fip` function, or a reuse
/// slot on a branch edge that needs a drop.
pub const BOX_REFUSAL: &str = "Box values in this function with loop, for, break or return \
    are not reclaimed natively; move the loop into a function without boxes";

/// The Wasm backend's typed refusal ([`crate::wasm_backend::Unsupported::Operation`]).
pub const WASM_REFUSAL: &str = "loop, for, break and return are not lowered to Wasm yet; \
    write the iteration as a tail-recursive function";

/// The marker a loop-header block starts with, and the non-local return
/// from a handled body (see [`crate::ir::Op::Loop`] and
/// [`crate::ir::Op::Exit`]).
pub const LOOP_MARKER: &str = "loop";

/// Whether a parsed module uses this feature's syntax (`loop`, `for`,
/// `break`, `return`, or `let mut`). The self-hosted Stage 1 front end in
/// `packages/vesper-compiler` does not parse it yet, so its differential
/// tests leave such sources out until it does.
pub fn uses_loop_syntax(module: &crate::ast::Module) -> bool {
    use crate::ast::{ExprKind, Function, Item, Statement};
    let mut found = false;
    let mut visit = |f: &Function| {
        if let Some(body) = &f.body {
            crate::regions::visit_expr(body, &mut |e| match &e.kind {
                ExprKind::Loop(_)
                | ExprKind::For { .. }
                | ExprKind::Range { .. }
                | ExprKind::Break(_)
                | ExprKind::Continue
                | ExprKind::Return(_) => found = true,
                ExprKind::Block { statements, .. }
                    if statements
                        .iter()
                        .any(|s| matches!(s, Statement::Let { mutable: true, .. })) =>
                {
                    found = true;
                }
                _ => {}
            });
        }
    };
    for item in &module.items {
        match item {
            Item::Function(f) => visit(f),
            Item::Impl { methods, .. } => methods.iter().for_each(&mut visit),
            Item::Trait(t) => t.methods.iter().for_each(&mut visit),
            Item::Conformance(c) => c.methods.iter().for_each(&mut visit),
            Item::Adt(_) | Item::Effect(_) => {}
        }
    }
    found
}
