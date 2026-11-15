use vesper_syntax::{
    ast::{ExprKind, Item, Statement},
    parser,
};

#[test]
fn expression_paths_preserve_components_and_struct_fields() {
    let source = "fn f() { let x = A::B::value; let y = A::B::C { field: x }; }";
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.syntax().text().to_string(), source);
    let Item::Function(function) = &parsed.module.items[0] else {
        panic!()
    };
    let ExprKind::Block { statements, .. } = &function.body.as_ref().unwrap().kind else {
        panic!()
    };
    let Statement::Let { value, .. } = &statements[0] else {
        panic!()
    };
    assert!(matches!(&value.kind, ExprKind::Name(name) if name == "A::B::value"));
    let Statement::Let { value, .. } = &statements[1] else {
        panic!()
    };
    assert!(
        matches!(&value.kind, ExprKind::Struct { name, fields } if name == "A::B::C" && fields.len() == 1)
    );
}

#[test]
fn incomplete_expression_path_is_diagnosed_and_lossless() {
    let source = "fn f() { A::B:: }";
    let parsed = parser::parse(source);
    assert!(!parsed.diagnostics.is_empty());
    assert_eq!(parsed.syntax().text().to_string(), source);
}
