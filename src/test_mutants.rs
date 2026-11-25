//! Mutation sites of `vesper test --mutate` behind a swap point
//! (self-hosting SH5 lane 14, #1337): the operator, comparison, boolean and
//! negation sites of every non-test function, method and conformance
//! method of a source, with their replacements.
//!
//! The Rust implementation (moved here unchanged from `src/test_runner.rs`)
//! is the default [`TestMutantsTable`]. `@vesper/test-runner`
//! (packages/vesper-test-runner/src/mutants.ves) implements the same
//! function over `@vesper/ast`; tests/self_host_test_mutants.rs runs both
//! on the repository's corpus and requires equal sites.

use crate::ast::{Expr, ExprKind, Function, Item, Literal, Statement};
use crate::lexer::{self, Token, TokenKind as K};
use crate::parser;
use crate::test_discovery::{case_marker, is_test_marker, preceding_comments};
use std::collections::BTreeSet;

/// One mutation: replace `source[start..end]` (`original`) by
/// `replacement` in `function`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutantSite {
    pub function: String,
    pub start: usize,
    pub end: usize,
    pub original: String,
    pub replacement: String,
}

/// The swappable function.
#[derive(Clone, Copy)]
pub struct TestMutantsTable {
    /// [`collect`].
    pub collect: fn(&str, &BTreeSet<&str>) -> Vec<MutantSite>,
}

/// The Rust implementation, the default [`TestMutantsTable`].
pub const RUST_TEST_MUTANTS: TestMutantsTable = TestMutantsTable {
    collect: rust_collect,
};

static TABLE: std::sync::OnceLock<TestMutantsTable> = std::sync::OnceLock::new();

/// Replace [`RUST_TEST_MUTANTS`] for the rest of the process. Only the
/// first call takes effect; a later one returns its table back.
pub fn install_test_mutants(table: TestMutantsTable) -> Result<(), TestMutantsTable> {
    TABLE.set(table)
}

/// The mutation sites of `source` (sorted by start, end and replacement)
/// outside the functions named in `test_names`, `test_*` functions and
/// functions marked `// @test` or `// @case`.
pub fn collect(source: &str, test_names: &BTreeSet<&str>) -> Vec<MutantSite> {
    (TABLE.get().unwrap_or(&RUST_TEST_MUTANTS).collect)(source, test_names)
}

fn find_operator_span(
    source: &str,
    tokens: &[Token],
    range_start: usize,
    range_end: usize,
    op: &str,
) -> Option<(usize, usize)> {
    let start_idx = tokens.partition_point(|t| t.span.start < range_start);
    for tok in &tokens[start_idx..] {
        if tok.span.end > range_end {
            break;
        }
        if matches!(tok.kind, K::Whitespace | K::Comment | K::BlockComment) {
            continue;
        }
        if &source[tok.span.clone()] == op {
            return Some((tok.span.start, tok.span.end));
        }
    }
    None
}

fn collect_mutants_in_fn(
    source: &str,
    tokens: &[Token],
    func: &Function,
    out: &mut Vec<MutantSite>,
) {
    let Some(body) = &func.body else {
        return;
    };
    let mut stack: Vec<&Expr> = vec![body];
    let mut visited = 0usize;
    while let Some(expr) = stack.pop() {
        visited += 1;
        if visited > 16_384 {
            break;
        }
        match &expr.kind {
            ExprKind::Literal(Literal::Bool(val)) => {
                if let Some(slice) = source.get(expr.span.start..expr.span.end) {
                    let trimmed = slice.trim();
                    if (trimmed == "true" && *val) || (trimmed == "false" && !*val) {
                        let lead = slice.len() - slice.trim_start().len();
                        let start = expr.span.start + lead;
                        let end = start + trimmed.len();
                        out.push(MutantSite {
                            function: func.name.clone(),
                            start,
                            end,
                            original: trimmed.to_string(),
                            replacement: if *val { "false" } else { "true" }.into(),
                        });
                    }
                }
            }
            ExprKind::Unary(op, inner) => {
                if op == "!"
                    && let Some((start, end)) =
                        find_operator_span(source, tokens, expr.span.start, inner.span.start, "!")
                {
                    out.push(MutantSite {
                        function: func.name.clone(),
                        start,
                        end,
                        original: "!".into(),
                        replacement: " ".into(),
                    });
                }
                stack.push(inner);
            }
            ExprKind::Binary(op, lhs, rhs) => {
                let replacements: &[&str] = match op.as_str() {
                    "+" => &["-"],
                    "-" => &["+"],
                    "*" => &["+"],
                    "==" => &["!="],
                    "!=" => &["=="],
                    "<" => &["<=", ">="],
                    "<=" => &["<", ">"],
                    ">" => &[">=", "<="],
                    ">=" => &[">", "<"],
                    "&&" => &["||"],
                    "||" => &["&&"],
                    _ => &[],
                };
                if !replacements.is_empty()
                    && let Some((start, end)) =
                        find_operator_span(source, tokens, lhs.span.end, rhs.span.start, op)
                {
                    for rep in replacements {
                        out.push(MutantSite {
                            function: func.name.clone(),
                            start,
                            end,
                            original: op.clone(),
                            replacement: (*rep).to_string(),
                        });
                    }
                }
                stack.push(rhs);
                stack.push(lhs);
            }
            ExprKind::Assign(op, lhs, rhs) => {
                let rep = match op.as_str() {
                    "+=" => Some("-="),
                    "-=" => Some("+="),
                    _ => None,
                };
                if let Some(rep) = rep
                    && let Some((start, end)) =
                        find_operator_span(source, tokens, lhs.span.end, rhs.span.start, op)
                {
                    out.push(MutantSite {
                        function: func.name.clone(),
                        start,
                        end,
                        original: op.clone(),
                        replacement: rep.to_string(),
                    });
                }
                stack.push(rhs);
                stack.push(lhs);
            }
            ExprKind::Tuple(es) => {
                for e in es.iter().rev() {
                    stack.push(e);
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, e) in fields.iter().rev() {
                    stack.push(e);
                }
            }
            ExprKind::Block { statements, tail } => {
                if let Some(t) = tail {
                    stack.push(t);
                }
                for st in statements.iter().rev() {
                    match st {
                        Statement::Let { value, .. } => stack.push(value),
                        Statement::Expr(e) => stack.push(e),
                    }
                }
            }
            ExprKind::Call { callee, args, .. } => {
                for a in args.iter().rev() {
                    stack.push(&a.expr);
                }
                stack.push(callee);
            }
            ExprKind::Field(inner, _) | ExprKind::Try(inner) => stack.push(inner),
            ExprKind::Index(a, b) => {
                stack.push(b);
                stack.push(a);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                if let Some(e) = otherwise {
                    stack.push(e);
                }
                stack.push(then);
                stack.push(condition);
            }
            ExprKind::Match { value, arms } => {
                for (_, e) in arms.iter().rev() {
                    stack.push(e);
                }
                stack.push(value);
            }
            ExprKind::Closure { body, .. } => stack.push(body),
            k @ (ExprKind::Loop(_)
            | ExprKind::For { .. }
            | ExprKind::Range { .. }
            | ExprKind::Break(_)
            | ExprKind::Continue
            | ExprKind::Return(_)) => {
                let mut children = k.control_children().unwrap_or_default();
                children.reverse();
                stack.extend(children);
            }
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                for c in ops.iter().rev() {
                    stack.push(&c.body);
                }
                stack.push(&ret.body);
                stack.push(body);
                if let Some((_, e)) = state {
                    stack.push(e);
                }
            }
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
        }
    }
}

/// [`collect`] by the Rust implementation, whatever table is installed.
pub fn rust_collect(source: &str, test_names: &BTreeSet<&str>) -> Vec<MutantSite> {
    let (tokens, _) = lexer::lex(source);
    let mut candidates = Vec::new();
    for item in &parser::parse(source).module.items {
        match item {
            Item::Function(f) => {
                if !test_names.contains(f.name.as_str())
                    && !f.name.starts_with("test_")
                    && !preceding_comments(&tokens, f.span.start).any(|comment| {
                        let text = &source[comment.span.clone()];
                        is_test_marker(text) || case_marker(text).is_some()
                    })
                {
                    collect_mutants_in_fn(source, &tokens, f, &mut candidates);
                }
            }
            Item::Impl { methods, .. } => {
                for m in methods {
                    collect_mutants_in_fn(source, &tokens, m, &mut candidates);
                }
            }
            Item::Conformance(c) => {
                for m in &c.methods {
                    collect_mutants_in_fn(source, &tokens, m, &mut candidates);
                }
            }
            Item::Adt(_) | Item::Effect(_) | Item::Trait(_) => {}
        }
    }
    candidates.sort_by_key(|c| (c.start, c.end, c.replacement.clone()));
    candidates
}
