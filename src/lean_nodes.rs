//! Shared structured syntax lowering for both Lean backends.
//! No helper accepts or parses serialized Lean fragments.

use crate::{
    ast::{Literal, Pass, Type},
    check::Place,
    numeric::IntegerKind,
};
use vesper_lean_syntax::{BinaryOp, Ident, Name, Projection, Term, UnaryOp};

pub(crate) fn ident(value: &str) -> Result<Ident, String> {
    Ident::new(value).map_err(|e| format!("unsupported Lean identifier {value:?}: {e}"))
}

pub(crate) fn source_name(value: &str) -> Result<Name, String> {
    Name::single(value).map_err(|e| format!("unsupported Lean name {value:?}: {e}"))
}

pub(crate) fn source_ref(value: &str) -> Result<Term, String> {
    Ok(Term::Ref(source_name(value)?))
}

/// A trusted, fixed standard-library path, kept separate from source identifiers.
pub(crate) fn global(path: &[&str]) -> Term {
    Term::Ref(
        Name::qualified(
            path.iter()
                .map(|s| Ident::new(*s).expect("fixed Lean path component")),
        )
        .expect("nonempty fixed Lean path")
        .rooted(),
    )
}

pub(crate) fn unit() -> Term {
    global(&["Unit", "unit"])
}
pub(crate) fn product(mut terms: Vec<Term>) -> Term {
    match terms.len() {
        0 => global(&["Unit"]),
        1 => terms.pop().expect("one element"),
        _ => Term::ProductType(terms),
    }
}
pub(crate) fn tuple(mut terms: Vec<Term>) -> Term {
    match terms.len() {
        0 => unit(),
        1 => terms.pop().expect("one element"),
        _ => Term::Tuple(terms),
    }
}

pub(crate) fn ty(value: &Type) -> Result<Term, String> {
    typed(value, None)
}
pub(crate) fn ty_monad(value: &Type, monad: &Term) -> Result<Term, String> {
    typed(value, Some(monad))
}

fn typed(value: &Type, monad: Option<&Term>) -> Result<Term, String> {
    Ok(match value {
        Type::IntLiteral(n) => typed(&IntegerKind::default_for(*n).ty(), monad)?,
        Type::Named(name, arguments) => {
            let constructor = match name.as_str() {
                "u8" => global(&["UInt8"]),
                "u32" => global(&["UInt32"]),
                "u64" => global(&["UInt64"]),
                "i64" => global(&["Int64"]),
                "f64" => global(&["Float"]),
                "bool" => global(&["Bool"]),
                "Unit" => global(&["Unit"]),
                "Never" => global(&["Empty"]),
                "String" => global(&["String"]),
                "Option" => global(&["Option"]),
                "Result" => global(&["Except"]),
                _ => source_ref(name)?,
            };
            let ordered = if name == "Result" && arguments.len() == 2 {
                vec![&arguments[1], &arguments[0]]
            } else {
                arguments.iter().collect()
            };
            if ordered.is_empty() {
                constructor
            } else {
                Term::app(
                    constructor,
                    ordered
                        .into_iter()
                        .map(|t| typed(t, monad))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
        }
        Type::Tuple(terms) => product(
            terms
                .iter()
                .map(|t| typed(t, monad))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            let mut results = vec![typed(result, monad)?];
            results.extend(
                params
                    .iter()
                    .filter(|(pass, _)| *pass == Pass::Inout)
                    .map(|(_, t)| typed(t, monad))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            let result = product(results);
            let mut result = if !effects.instances.is_empty() || effects.tail.is_some() {
                Term::app(
                    monad
                        .ok_or("effectful function type needs an ambient Lean monad")?
                        .clone(),
                    [result],
                )
            } else {
                result
            };
            if params.is_empty() {
                result = Term::Arrow(Box::new(global(&["Unit"])), Box::new(result));
            }
            for (_, parameter) in params.iter().rev() {
                result = Term::Arrow(Box::new(typed(parameter, monad)?), Box::new(result));
            }
            result
        }
        Type::Infer => Term::Infer,
        Type::Row(_) => return Err("effect rows are not first-class Lean values".into()),
    })
}

pub(crate) fn effectful_arrow(value: &Type) -> bool {
    match value {
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            !effects.instances.is_empty()
                || effects.tail.is_some()
                || params.iter().any(|(_, t)| effectful_arrow(t))
                || effectful_arrow(result)
        }
        Type::Named(_, ts) | Type::Tuple(ts) => ts.iter().any(effectful_arrow),
        _ => false,
    }
}

pub(crate) fn literal(value: &Literal) -> Term {
    match value {
        Literal::Int(n) => {
            let magnitude = Term::Nat(n.unsigned_abs().into());
            if *n < 0 {
                unary(UnaryOp::Neg, magnitude)
            } else {
                magnitude
            }
        }
        Literal::Float(n) => Term::app(
            global(&["Float", "ofBits"]),
            [Term::Nat(n.to_bits().into())],
        ),
        Literal::Bool(b) => Term::Bool(*b),
        Literal::String(s) => Term::String(s.clone()),
    }
}

fn unary(op: UnaryOp, value: Term) -> Term {
    Term::Unary {
        op,
        value: Box::new(value),
    }
}
fn nat(value: u128) -> Term {
    Term::Nat(value.into())
}
fn negative(value: u128) -> Term {
    unary(UnaryOp::Neg, nat(value))
}
fn project(value: Term, field: &str) -> Term {
    Term::Project {
        base: Box::new(value),
        projection: Projection::Field(Ident::new(field).expect("fixed numeric method")),
    }
}
fn decide(proposition: Term) -> Term {
    Term::app(global(&["Decidable", "decide"]), [proposition])
}

pub(crate) fn in_range(op: &str, kind: IntegerKind, a: Term, b: Term) -> Option<Term> {
    Some(if kind.unsigned() {
        match op {
            "+" | "*" => decide(Term::binary(
                BinaryOp::Le,
                Term::binary(
                    if op == "+" {
                        BinaryOp::Add
                    } else {
                        BinaryOp::Mul
                    },
                    project(a, "toNat"),
                    project(b, "toNat"),
                ),
                nat(kind.max() as u128),
            )),
            "-" => decide(Term::binary(BinaryOp::Le, b, a)),
            "neg" => Term::binary(BinaryOp::BoolEq, a, nat(0)),
            _ => return None,
        }
    } else {
        match op {
            "+" | "-" | "*" => Term::app(
                source_ref("inI64").expect("fixed prelude name"),
                [Term::binary(
                    match op {
                        "+" => BinaryOp::Add,
                        "-" => BinaryOp::Sub,
                        _ => BinaryOp::Mul,
                    },
                    project(a, "toInt"),
                    project(b, "toInt"),
                )],
            ),
            "/" => unary(
                UnaryOp::BoolNot,
                Term::binary(
                    BinaryOp::BoolAnd,
                    Term::binary(BinaryOp::BoolEq, a, negative(1u128 << 63)),
                    Term::binary(BinaryOp::BoolEq, b, negative(1)),
                ),
            ),
            "neg" => Term::binary(BinaryOp::BoolNe, a, negative(1u128 << 63)),
            _ => return None,
        }
    })
}

pub(crate) fn arith_term(
    op: &str,
    kind: Option<IntegerKind>,
    a: Term,
    b: Term,
) -> Result<Term, String> {
    let operator = match op {
        "+" => BinaryOp::Add,
        "-" => BinaryOp::Sub,
        "*" => BinaryOp::Mul,
        "/" => BinaryOp::Div,
        "%" => BinaryOp::Mod,
        "==" => BinaryOp::BoolEq,
        "!=" => BinaryOp::BoolNe,
        "<" => BinaryOp::Lt,
        "<=" => BinaryOp::Le,
        ">" => BinaryOp::Gt,
        ">=" => BinaryOp::Ge,
        "&&" => BinaryOp::BoolAnd,
        "||" => BinaryOp::BoolOr,
        _ => return Err(format!("unsupported Lean binary operator {op:?}")),
    };
    if op == "%" && kind == Some(IntegerKind::I64) {
        Ok(Term::If {
            condition: Box::new(Term::binary(BinaryOp::BoolEq, b.clone(), negative(1))),
            yes: Box::new(nat(0)),
            no: Box::new(Term::binary(operator, a, b)),
        })
    } else {
        Ok(Term::binary(operator, a, b))
    }
}

pub(crate) fn update_place(place: &Place, value: Term) -> Result<Term, String> {
    fn update(base: Term, path: &[String], value: Term) -> Result<Term, String> {
        let Some((field, rest)) = path.split_first() else {
            return Ok(value);
        };
        let field = ident(field)?;
        let next = Term::Project {
            base: Box::new(base.clone()),
            projection: Projection::Field(field.clone()),
        };
        Ok(Term::Update {
            base: Box::new(base),
            fields: vec![(field, update(next, rest, value)?)],
        })
    }
    update(source_ref(&place.root)?, &place.path, value)
}
