//! Equality saturation is confined to total, pure integer expressions. Effect
//! tokens, calls, division, projections, and ownership transitions are barriers.
use crate::ast::*;
use egg::{AstSize, Extractor, RecExpr, Rewrite, Runner, define_language, rewrite};
define_language! {enum Math { Num(i64), "+"=Add([egg::Id;2]), "-"=Sub([egg::Id;2]), "*"=Mul([egg::Id;2]), Symbol(egg::Symbol), }}
fn rules() -> Vec<Rewrite<Math, ()>> {
    vec![
        rewrite!("add-zero";"(+ ?x 0)"=>"?x"),
        rewrite!("zero-add";"(+ 0 ?x)"=>"?x"),
        rewrite!("sub-zero";"(- ?x 0)"=>"?x"),
        rewrite!("mul-one";"(* ?x 1)"=>"?x"),
        rewrite!("one-mul";"(* 1 ?x)"=>"?x"),
        rewrite!("mul-zero";"(* ?x 0)"=>"0"),
        rewrite!("zero-mul";"(* 0 ?x)"=>"0"),
        rewrite!("self-sub";"(- ?x ?x)"=>"0"),
    ]
}
fn encode(e: &Expr) -> Option<String> {
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
    if let ExprKind::Binary(op, a, b) = &e.kind
        && let (ExprKind::Literal(Literal::Int(x)), ExprKind::Literal(Literal::Int(y))) =
            (&a.kind, &b.kind)
    {
        let n = match op.as_str() {
            "+" => Some(x.wrapping_add(*y)),
            "-" => Some(x.wrapping_sub(*y)),
            "*" => Some(x.wrapping_mul(*y)),
            _ => None,
        };
        if let Some(n) = n {
            let n = types
                .get(&(e.span.start, e.span.end))
                .and_then(crate::numeric::IntegerKind::from_type)
                .map(|k| k.wrap(n))
                .unwrap_or(n);
            e.kind = ExprKind::Literal(Literal::Int(n));
            return;
        }
    }
    if types
        .get(&(e.span.start, e.span.end))
        .is_some_and(crate::check::integer)
        && let Some(text) = encode(e)
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
