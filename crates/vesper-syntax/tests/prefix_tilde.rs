use vesper_syntax::{
    ast::{ExprKind, Item},
    lexer::{self, TokenKind},
    parser,
};

#[test]
fn tilde_is_lossless_and_binds_before_binary_operators() {
    let source = "fn f(x: u64)->u64 { ~x + 1 }";
    let (tokens, diagnostics) = lexer::lex(source);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(tokens.iter().any(|token| token.kind == TokenKind::Tilde));
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.syntax().text().to_string(), source);
    let Item::Function(function) = &parsed.module.items[0] else {
        panic!()
    };
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &function.body.as_ref().unwrap().kind
    else {
        panic!()
    };
    let ExprKind::Binary(op, left, _) = &tail.kind else {
        panic!()
    };
    assert_eq!(op, "+");
    assert!(
        matches!(&left.kind, ExprKind::Unary(op, operand) if op == "~" && matches!(&operand.kind, ExprKind::Name(name) if name == "x"))
    );
}

#[test]
fn incomplete_tilde_remains_lossless_and_reports_an_error() {
    let source = "fn f(){ ~ } fn g(){ 1 }";
    let parsed = parser::parse(source);
    assert!(!parsed.diagnostics.is_empty());
    assert_eq!(parsed.syntax().text().to_string(), source);
}
