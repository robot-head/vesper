//! Equality saturation is confined to pure integer expressions. Effect
//! tokens, calls, division, projections, and ownership transitions are barriers.
//!
//! Under `vesper.arith.checked/1`, `+`, `-` and `*` may raise
//! `Fail[ArithOverflow]`, so two terms are equal only when they produce the
//! same value *and* the same failure. Every rule below preserves that:
//! identities never remove a subterm unless the subterm's e-class contains a
//! variable or constant (and therefore cannot fail), and reassociation,
//! commutation and cancellation are reserved for the total wrapping
//! intrinsics, where they are modular-arithmetic identities. E-class guards
//! guide search only: each installation also passes the independent bounded
//! certificate checker, including genuine totality evidence and exact AST
//! binding. Unsupported syntax remains a traversal barrier; refused admitted
//! islands retain their complete original expression.
use crate::ast::*;
use egg::{Id, Rewrite, Subst, define_language, rewrite};

#[cfg(test)]
mod adversarial_tests;
mod certificate;
pub mod witness;

use certificate::{RewriteOutcome, RewriteProposal};
use witness::{WitnessError, WitnessLimits};
define_language! {enum Math {
    Num(i64),
    "+"=Add([Id;2]), "-"=Sub([Id;2]), "*"=Mul([Id;2]),
    "wrapping_add"=WAdd([Id;2]), "wrapping_sub"=WSub([Id;2]), "wrapping_mul"=WMul([Id;2]),
    Input(u32),
}}
type Graph = egg::EGraph<Math, ()>;
/// The matched subterm is (equal to) a variable or a constant, so dropping it
/// cannot discard a potential overflow.
fn total(var: &str) -> impl Fn(&mut Graph, Id, &Subst) -> bool + use<> {
    let var: egg::Var = var.parse().unwrap();
    move |g, _, subst| {
        g[subst[var]]
            .nodes
            .iter()
            .any(|n| matches!(n, Math::Num(_) | Math::Input(_)))
    }
}
fn rules() -> Vec<Rewrite<Math, ()>> {
    vec![
        // Checked identities: the result equals an operand that is still
        // evaluated, and the identity element can never overflow.
        rewrite!("add-zero";"(+ ?x 0)"=>"?x"),
        rewrite!("zero-add";"(+ 0 ?x)"=>"?x"),
        rewrite!("sub-zero";"(- ?x 0)"=>"?x"),
        rewrite!("mul-one";"(* ?x 1)"=>"?x"),
        rewrite!("one-mul";"(* 1 ?x)"=>"?x"),
        // Annihilation and cancellation drop a subterm: only total ones.
        rewrite!("mul-zero";"(* ?x 0)"=>"0" if total("?x")),
        rewrite!("zero-mul";"(* 0 ?x)"=>"0" if total("?x")),
        rewrite!("self-sub";"(- ?x ?x)"=>"0" if total("?x")),
        // Wrapping-only identities (arithmetic modulo 2^bits).
        rewrite!("wadd-zero";"(wrapping_add ?x 0)"=>"?x"),
        rewrite!("wsub-zero";"(wrapping_sub ?x 0)"=>"?x"),
        rewrite!("wmul-one";"(wrapping_mul ?x 1)"=>"?x"),
        rewrite!("wadd-comm";"(wrapping_add ?x ?y)"=>"(wrapping_add ?y ?x)"),
        rewrite!("wmul-comm";"(wrapping_mul ?x ?y)"=>"(wrapping_mul ?y ?x)"),
        rewrite!("wadd-assoc";"(wrapping_add (wrapping_add ?x ?y) ?z)"=>"(wrapping_add ?x (wrapping_add ?y ?z))"),
        rewrite!("wmul-assoc";"(wrapping_mul (wrapping_mul ?x ?y) ?z)"=>"(wrapping_mul ?x (wrapping_mul ?y ?z))"),
        rewrite!("wadd-cancel";"(wrapping_sub (wrapping_add ?x ?y) ?y)"=>"?x" if total("?y")),
        rewrite!("wmul-zero";"(wrapping_mul ?x 0)"=>"0" if total("?x")),
        rewrite!("wself-sub";"(wrapping_sub ?x ?x)"=>"0" if total("?x")),
    ]
}
// This is the only expression-installation seam. The producer is untrusted:
// verify the exact source/candidate and every proof with its cumulative budget.
fn install_proposal(
    expression: &mut Expr,
    types: &TypeMap,
    mut proposal: RewriteProposal,
) -> Result<(), WitnessError> {
    witness::verify_with_budget(
        expression,
        &proposal.candidate,
        types,
        &proposal.certificate,
        &mut proposal.budget,
    )?;
    *expression = proposal.candidate;
    Ok(())
}

pub fn optimize(module: &mut Module, types: &TypeMap) {
    for item in &mut module.items {
        match item {
            Item::Function(f) => {
                if let Some(e) = &mut f.body {
                    visit(e, types);
                }
            }
            Item::Impl { methods, .. } => {
                for f in methods {
                    if let Some(e) = &mut f.body {
                        visit(e, types);
                    }
                }
            }
            _ => {}
        }
    }
}
fn visit(e: &mut Expr, types: &TypeMap) {
    // Admit maximal islands before child edits so type/callee origins still
    // refer to the checked source. Refusal never retries an admitted island's
    // children; only a true unsupported barrier permits ordinary traversal.
    match certificate::propose_rewrite(e, types, WitnessLimits::default()) {
        Ok(RewriteOutcome::Candidate(proposal)) => {
            let _ = install_proposal(e, types, proposal);
            return;
        }
        Ok(RewriteOutcome::Unchanged) | Err(_) => return,
        Ok(RewriteOutcome::Barrier) => {}
    }
    match &mut e.kind {
        ExprKind::Tuple(es) => {
            for e in es {
                visit(e, types)
            }
        }
        ExprKind::Struct { fields, .. } => {
            for (_, e) in fields {
                visit(e, types)
            }
        }
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { value, ty, .. } => {
                        if *ty == Type::Infer
                            && let Some(t) = types.get(&(value.span.start, value.span.end))
                        {
                            *ty = crate::numeric::default_type(t);
                        }
                        visit(value, types)
                    }
                    Statement::Expr(e) => visit(e, types),
                }
            }
            if let Some(e) = tail {
                visit(e, types)
            }
        }
        ExprKind::Call { callee, args, .. } => {
            visit(callee, types);
            for a in args {
                visit(&mut a.expr, types)
            }
        }
        ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => visit(e, types),
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Assign(_, a, b) => {
            visit(a, types);
            visit(b, types)
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            visit(condition, types);
            visit(then, types);
            if let Some(e) = otherwise {
                visit(e, types)
            }
        }
        ExprKind::Match { value, arms } => {
            visit(value, types);
            for (_, e) in arms {
                visit(e, types)
            }
        }
        ExprKind::Closure { body, .. } => visit(body, types),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, e)) = state {
                visit(e, types)
            }
            visit(body, types);
            visit(&mut ret.body, types);
            for c in ops {
                visit(&mut c.body, types)
            }
        }
        _ => {}
    }
}
