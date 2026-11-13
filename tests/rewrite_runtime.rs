//! Actual evaluator/JIT agreement around the AST optimizer's witness gate.
use vesper::{
    ast::{Expr, ExprKind, Item, Literal, Module, Pass, Statement, TypeMap},
    check, native, numeric, optimize, parser,
    runtime::Runtime,
};

fn transform(source: &str) -> (Module, check::Checked, Module, check::Checked) {
    let parsed = parser::parse(source);
    assert!(
        parsed.diagnostics.is_empty(),
        "{source}\n{:?}",
        parsed.diagnostics
    );
    let before = parsed.module;
    let checked = check::check(&before);
    assert!(
        checked.diagnostics.is_empty(),
        "{source}\n{:?}",
        checked.diagnostics
    );
    let mut after = before.clone();
    optimize::optimize(&mut after, &checked.expression_types);
    let rewritten = check::check(&after);
    assert!(
        rewritten.diagnostics.is_empty(),
        "optimized {source}\n{:?}",
        rewritten.diagnostics
    );
    (before, checked, after, rewritten)
}

fn worlds_agree(before: &check::World, after: &check::World, expected: &str, source: &str) {
    for world in [before, after] {
        for ir_optimization in [false, true] {
            let mut evaluator = Runtime::new(world);
            evaluator.optimize = ir_optimization;
            assert_eq!(
                evaluator.run("main", vec![]).unwrap().to_string(),
                expected,
                "evaluator: {source}"
            );
            assert_eq!(
                native::jit_with_options(world, "main", ir_optimization)
                    .unwrap()
                    .to_string(),
                expected,
                "JIT: {source}"
            );
        }
    }
}

fn agree(source: &str, expected: &str) -> Module {
    let (_, before, after, checked_after) = transform(source);
    worlds_agree(&before.world, &checked_after.world, expected, source);
    after
}

fn function<'a>(module: &'a Module, name: &str) -> &'a Expr {
    module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(f) if f.name == name => f.body.as_ref(),
            _ => None,
        })
        .unwrap()
}

fn tail(mut expression: &Expr) -> &Expr {
    while let ExprKind::Block {
        tail: Some(value), ..
    } = &expression.kind
    {
        expression = value;
    }
    expression
}

fn visit<'a>(expression: &'a Expr, callback: &mut impl FnMut(&'a Expr)) {
    callback(expression);
    match &expression.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
        ExprKind::Tuple(values) => {
            for value in values {
                visit(value, callback);
            }
        }
        ExprKind::Struct { fields, .. } => {
            for (_, value) in fields {
                visit(value, callback);
            }
        }
        ExprKind::Block { statements, tail } => {
            for statement in statements {
                match statement {
                    Statement::Let { value, .. } | Statement::Expr(value) => visit(value, callback),
                }
            }
            if let Some(value) = tail {
                visit(value, callback);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            visit(callee, callback);
            for argument in args {
                visit(&argument.expr, callback);
            }
        }
        ExprKind::Field(value, _) | ExprKind::Try(value) | ExprKind::Unary(_, value) => {
            visit(value, callback)
        }
        ExprKind::Binary(_, left, right)
        | ExprKind::Index(left, right)
        | ExprKind::Assign(_, left, right) => {
            visit(left, callback);
            visit(right, callback);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            visit(condition, callback);
            visit(then, callback);
            if let Some(value) = otherwise {
                visit(value, callback);
            }
        }
        ExprKind::Match { value, arms } => {
            visit(value, callback);
            for (_, arm) in arms {
                visit(arm, callback);
            }
        }
        ExprKind::Closure { body, .. } => visit(body, callback),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, value)) = state {
                visit(value, callback);
            }
            visit(body, callback);
            visit(&ret.body, callback);
            for operation in ops {
                visit(&operation.body, callback);
            }
        }
    }
}

fn origins(expression: &Expr, types: &TypeMap) -> Vec<(String, std::ops::Range<usize>)> {
    let mut result = Vec::new();
    visit(expression, &mut |value| {
        if let ExprKind::Call { callee, .. } = &value.kind
            && let ExprKind::Name(name) = &callee.kind
            && numeric::wrapping_intrinsic(name).is_some()
            && !types.contains_key(&(callee.span.start, callee.span.end))
        {
            result.push((name.clone(), callee.span.clone()));
        }
    });
    result
}

fn guarded(kind: &str, expression: &str) -> String {
    format!(
        "fn main() -> {kind} {{ handle Fail[ArithOverflow] in {{ {expression} }} with {{ return v => v, raise(e, k) => 42, }} }}"
    )
}

#[test]
fn actual_six_fold_families_preserve_every_supported_integer_kind() {
    for kind in ["u8", "u32", "u64", "i64"] {
        for (expression, expected) in [
            ("2 + 3", 5),
            ("9 - 4", 5),
            ("2 * 3", 6),
            ("wrapping_add(2, 3)", 5),
            ("wrapping_sub(9, 4)", 5),
            ("wrapping_mul(2, 3)", 6),
        ] {
            let source = format!("fn main() -> {kind} {{ {expression} }}");
            let after = agree(&source, &expected.to_string());
            assert_eq!(
                tail(function(&after, "main")).kind,
                ExprKind::Literal(Literal::Int(expected))
            );
        }
    }
}

#[test]
fn actual_identities_and_prior_totality_support_install_simplifications() {
    for kind in ["u8", "u32", "u64", "i64"] {
        let source =
            format!("fn f(x: {kind}) -> {kind} {{ x + 0 }} fn main() -> {kind} {{ f(42) }}");
        let after = agree(&source, "42");
        assert!(matches!(&tail(function(&after, "f")).kind, ExprKind::Name(name) if name == "x"));
        let source =
            format!("fn f(x: {kind}) -> {kind} {{ (x + 0) * 0 }} fn main() -> {kind} {{ f(42) }}");
        let after = agree(&source, "0");
        assert_eq!(
            tail(function(&after, "f")).kind,
            ExprKind::Literal(Literal::Int(0))
        );
    }
}

#[test]
fn overflow_cannot_be_erased_by_checked_or_wrapping_zero_and_self_rules() {
    for (kind, maximum) in [
        ("u8", "255"),
        ("u32", "4294967295"),
        ("u64", "18446744073709551615"),
        ("i64", "9223372036854775807"),
    ] {
        for expression in [
            "(x + 1) * 0",
            "0 * (x + 1)",
            "(x + 1) - (x + 1)",
            "wrapping_mul(x + 1, 0)",
            "wrapping_sub(x + 1, x + 1)",
        ] {
            let source = format!(
                "fn f(x: {kind}) -> {kind} eff [Fail[ArithOverflow]] {{ {expression} }} {}",
                guarded(kind, &format!("f({maximum})"))
            );
            let after = agree(&source, "42");
            assert_ne!(
                tail(function(&after, "f")).kind,
                ExprKind::Literal(Literal::Int(0)),
                "{source}"
            );
        }
    }
}

#[test]
fn high_u64_products_and_signed_wrapping_endpoints_keep_exact_values() {
    for (kind, expression, expected) in [
        (
            "u64",
            "wrapping_mul(18446744073709551615, 18446744073709551615)",
            1i128,
        ),
        ("u64", "9223372036854775808 * 1", 9223372036854775808),
        (
            "u64",
            "wrapping_add(18446744073709551615, 0)",
            18446744073709551615,
        ),
        ("u64", "wrapping_mul(9223372036854775808, 2)", 0),
        (
            "i64",
            "wrapping_mul(-9223372036854775808, -1)",
            -9223372036854775808,
        ),
    ] {
        let source = format!("fn main() -> {kind} {{ {expression} }}");
        let after = agree(&source, &expected.to_string());
        assert_eq!(
            tail(function(&after, "main")).kind,
            ExprKind::Literal(Literal::Int(expected))
        );
    }
    let source = guarded("u64", "18446744073709551615 * 18446744073709551615");
    agree(&source, "42");
}

#[test]
fn intrinsic_candidates_copy_real_original_callee_templates() {
    let source = "// current source é🦀\nfn f(x: i64, y: i64) -> i64 { wrapping_add(wrapping_add(x, 0), y) } fn main() -> i64 { f(40, 2) }";
    let (before, checked, after, rewritten) = transform(source);
    let original = origins(function(&before, "f"), &checked.expression_types);
    assert_eq!(original.len(), 2);
    let mut calls = 0;
    visit(function(&after, "f"), &mut |value| {
        if let ExprKind::Call {
            callee,
            args,
            types,
        } = &value.kind
            && let ExprKind::Name(name) = &callee.kind
            && numeric::wrapping_intrinsic(name).is_some()
        {
            calls += 1;
            assert!(original.contains(&(name.clone(), callee.span.clone())));
            assert_eq!(&source[callee.span.clone()], name);
            assert!(!callee.span.is_empty());
            assert!(types.is_empty());
            assert_eq!(args.len(), 2);
            assert!(
                args.iter()
                    .all(|argument| argument.pass == Pass::In && !argument.explicit)
            );
        }
    });
    assert_eq!(calls, 1, "the genuine nested identity must simplify");
    worlds_agree(&checked.world, &rewritten.world, "42", source);
}

#[test]
fn typed_shadowed_intrinsics_stay_calls_while_their_pure_arguments_rewrite() {
    for (name, body, a, b) in [
        ("wrapping_add", "wrapping_mul(a, b)", 6, 7),
        ("wrapping_sub", "wrapping_add(a, b)", 40, 2),
        ("wrapping_mul", "wrapping_add(a, b)", 40, 2),
    ] {
        let source = format!(
            "fn main() -> i64 {{ let {name} = |a: i64, b: i64| {body}; {name}({a} + 0, {b} + 0) }}"
        );
        let (before, checked, after, rewritten) = transform(&source);
        let ExprKind::Call {
            callee: original, ..
        } = &tail(function(&before, "main")).kind
        else {
            panic!("source call")
        };
        assert!(
            checked
                .expression_types
                .contains_key(&(original.span.start, original.span.end))
        );
        let ExprKind::Call { callee, args, .. } = &tail(function(&after, "main")).kind else {
            panic!("shadowed call was erased")
        };
        assert_eq!(callee.as_ref(), original.as_ref());
        for (argument, expected) in args.iter().zip([a, b]) {
            assert_eq!(
                argument.expr.kind,
                ExprKind::Literal(Literal::Int(expected))
            );
        }
        worlds_agree(&checked.world, &rewritten.world, "42", &source);
    }
}

#[test]
fn pure_children_inside_barriers_rewrite_without_moving_effect_or_failure_order() {
    let source = "fn f(x: i64) -> i64 { (x + 0) / 2 } fn main() -> i64 { f(84) }";
    let after = agree(source, "42");
    let ExprKind::Binary(operation, left, _) = &tail(function(&after, "f")).kind else {
        panic!("division barrier")
    };
    assert_eq!(operation, "/");
    assert!(matches!(&left.kind, ExprKind::Name(name) if name == "x"));
    let source = "fn f(x: i64) -> i64 eff [Fail[RuntimeError]] { x / 1 } fn main() -> i64 { handle Fail[RuntimeError] in { f(20 + 22) } with { return v => v + 0, raise(e, k) => 42 + 0, } }";
    let after = agree(source, "42");
    let ExprKind::Handle { body, ret, ops, .. } = &tail(function(&after, "main")).kind else {
        panic!("handler barrier")
    };
    assert!(matches!(&tail(&ret.body).kind, ExprKind::Name(name) if name == "v"));
    assert_eq!(tail(&ops[0].body).kind, ExprKind::Literal(Literal::Int(42)));
    let ExprKind::Call { args, .. } = &tail(body).kind else {
        panic!("handler body call")
    };
    assert_eq!(args[0].expr.kind, ExprKind::Literal(Literal::Int(42)));
    agree(
        "fn next(inout n: i64) -> i64 { n = wrapping_add(n, 1); n } fn main() -> i64 { let n = 0; let a = wrapping_mul(next(inout n), 0); let b = next(inout n); wrapping_add(wrapping_mul(n, 100), wrapping_add(wrapping_mul(a, 10), b)) }",
        "202",
    );
    for (operands, expected) in [
        ("overflow(), division()", "10"),
        ("division(), overflow()", "20"),
    ] {
        let source = format!(
            "fn overflow() -> i64 eff [Fail[ArithOverflow]] {{ 9223372036854775807 + 1 }} fn division() -> i64 eff [Fail[RuntimeError]] {{ 1 / 0 }} fn main() -> i64 {{ handle Fail[RuntimeError] in {{ handle Fail[ArithOverflow] in {{ wrapping_mul({operands}) }} with {{ return v => v, raise(e, k) => 10, }} }} with {{ return v => v, raise(e, k) => 20, }} }}"
        );
        agree(&source, expected);
    }
}

#[test]
fn depth_refusal_preserves_the_entire_maximal_island_without_child_retry() {
    let expression = "x".to_owned() + &" + 0".repeat(70);
    let source = format!("fn f(x: i64) -> i64 {{ {expression} }} fn main() -> i64 {{ f(42) }}");
    let (before, checked, after, rewritten) = transform(&source);
    assert_eq!(function(&before, "f"), function(&after, "f"));
    worlds_agree(&checked.world, &rewritten.world, "42", &source);
    let mut repeated = before.clone();
    optimize::optimize(&mut repeated, &checked.expression_types);
    assert_eq!(
        after, repeated,
        "identical checked inputs produce identical AST output"
    );
}
