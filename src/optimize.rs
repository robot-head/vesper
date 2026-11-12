//! Equality saturation is confined to pure integer expressions. Effect
//! tokens, calls, division, projections, and ownership transitions are barriers.
//!
//! Under `vesper.arith.checked/1`, `+`, `-` and `*` may raise
//! `Fail[ArithOverflow]`, so two terms are equal only when they produce the
//! same value *and* the same failure. Every rule below preserves that:
//! identities never remove a subterm unless the subterm's e-class contains a
//! variable or constant (and therefore cannot fail), and reassociation,
//! commutation and cancellation are reserved for the total wrapping
//! intrinsics, where they are modular-arithmetic identities.
use crate::ast::*;
use egg::{AstSize, Extractor, Id, RecExpr, Rewrite, Runner, Subst, define_language, rewrite};
define_language! {enum Math {
    Num(i64),
    "+"=Add([Id;2]), "-"=Sub([Id;2]), "*"=Mul([Id;2]),
    "wrapping_add"=WAdd([Id;2]), "wrapping_sub"=WSub([Id;2]), "wrapping_mul"=WMul([Id;2]),
    Symbol(egg::Symbol),
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
            .any(|n| matches!(n, Math::Num(_) | Math::Symbol(_)))
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
fn encode(e: &Expr, types: &TypeMap) -> Option<String> {
    let encode = |e| encode(e, types);
    match &e.kind {
        ExprKind::Literal(Literal::Int(n)) => {
            if i64::try_from(*n).is_ok() {
                Some(n.to_string())
            } else {
                None
            }
        }
        ExprKind::Name(n) => Some(n.clone()),
        ExprKind::Binary(op, a, b) if matches!(op.as_str(), "+" | "-" | "*") => {
            Some(format!("({op} {} {})", encode(a)?, encode(b)?))
        }
        // An intrinsic callee is never typed as a value; a local closure
        // that shadows the intrinsic name is.
        ExprKind::Call {
            callee,
            args,
            types: generic,
        } => match &callee.kind {
            ExprKind::Name(n)
                if crate::numeric::wrapping_intrinsic(n).is_some()
                    && generic.is_empty()
                    && args.len() == 2
                    && !types.contains_key(&(callee.span.start, callee.span.end)) =>
            {
                Some(format!(
                    "({n} {} {})",
                    encode(&args[0].expr)?,
                    encode(&args[1].expr)?
                ))
            }
            _ => None,
        },
        _ => None,
    }
}
fn decode(e: &RecExpr<Math>, id: egg::Id, span: &Span) -> Expr {
    let kind = match &e[id] {
        Math::Num(n) => ExprKind::Literal(Literal::Int(*n as i128)),
        Math::Symbol(s) => ExprKind::Name(s.to_string()),
        Math::Add([a, b]) | Math::Sub([a, b]) | Math::Mul([a, b]) => {
            let op = match &e[id] {
                Math::Add(_) => "+",
                Math::Sub(_) => "-",
                _ => "*",
            };
            ExprKind::Binary(
                op.into(),
                Box::new(decode(e, *a, span)),
                Box::new(decode(e, *b, span)),
            )
        }
        Math::WAdd([a, b]) | Math::WSub([a, b]) | Math::WMul([a, b]) => {
            let name = match &e[id] {
                Math::WAdd(_) => "wrapping_add",
                Math::WSub(_) => "wrapping_sub",
                _ => "wrapping_mul",
            };
            let arg = |id: &Id| Arg {
                pass: Pass::In,
                explicit: false,
                expr: decode(e, *id, span),
            };
            // The callee gets an empty span, which no typed value occupies, so
            // a later `encode` still recognizes the intrinsic.
            ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Name(name.into()),
                    span: span.start..span.start,
                }),
                types: vec![],
                args: vec![arg(a), arg(b)],
            }
        }
    };
    Expr {
        kind,
        span: span.clone(),
    }
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
    // Checked constant folding: an overflowing constant expression is kept,
    // since it must raise at run time rather than wrap.
    if let ExprKind::Binary(op, a, b) = &e.kind
        && let (ExprKind::Literal(Literal::Int(x)), ExprKind::Literal(Literal::Int(y))) =
            (&a.kind, &b.kind)
        && matches!(op.as_str(), "+" | "-" | "*")
        && let Some(kind) = types
            .get(&(e.span.start, e.span.end))
            .and_then(crate::numeric::IntegerKind::from_type)
        && let Ok(n) = kind.checked(op, *x, *y)
    {
        e.kind = ExprKind::Literal(Literal::Int(n));
        return;
    }
    if let ExprKind::Call { callee, args, .. } = &e.kind
        && let ExprKind::Name(name) = &callee.kind
        && let Some(op) = crate::numeric::wrapping_intrinsic(name)
        && !types.contains_key(&(callee.span.start, callee.span.end))
        && let [a, b] = args.as_slice()
        && let (ExprKind::Literal(Literal::Int(x)), ExprKind::Literal(Literal::Int(y))) =
            (&a.expr.kind, &b.expr.kind)
        && let Some(kind) = types
            .get(&(e.span.start, e.span.end))
            .and_then(crate::numeric::IntegerKind::from_type)
    {
        e.kind = ExprKind::Literal(Literal::Int(kind.wrapping(op, *x, *y)));
        return;
    }
    if types
        .get(&(e.span.start, e.span.end))
        .is_some_and(crate::check::integer)
        && let Some(text) = encode(e, types)
        && let Ok(expr) = text.parse::<RecExpr<Math>>()
    {
        let runner = Runner::default()
            .with_iter_limit(8)
            .with_node_limit(2048)
            .with_expr(&expr)
            .run(&rules());
        let root = runner.roots[0];
        let (_, best) = Extractor::new(&runner.egraph, AstSize).find_best(root);
        *e = decode(&best, egg::Id::from(best.as_ref().len() - 1), &e.span);
    }
}
