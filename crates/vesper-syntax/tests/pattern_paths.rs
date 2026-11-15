use vesper_syntax::{
    ast::{ExprKind, Item, PatternKind, Statement},
    parser,
};

#[test]
fn constructor_patterns_keep_all_path_components_and_fields() {
    let source = "fn f() { let A::B::C(x) = v; let A::B::D { field } = v; }";
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.syntax().text().to_string(), source);
    let Item::Function(function) = &parsed.module.items[0] else {
        panic!()
    };
    let ExprKind::Block { statements, .. } = &function.body.as_ref().unwrap().kind else {
        panic!()
    };
    for (statement, expected, named) in [
        (&statements[0], "A::B::C", false),
        (&statements[1], "A::B::D", true),
    ] {
        let Statement::Let { pat, .. } = statement else {
            panic!()
        };
        let PatternKind::Constructor {
            name,
            fields,
            named: actual,
        } = &pat.kind
        else {
            panic!()
        };
        assert_eq!(name, expected);
        assert_eq!(*actual, named);
        assert_eq!(fields.len(), 1);
    }
}

#[test]
fn incomplete_path_is_diagnosed_and_retained() {
    let source = "fn f() { let A::B:: = v; }";
    let parsed = parser::parse(source);
    assert!(!parsed.diagnostics.is_empty());
    assert_eq!(parsed.syntax().text().to_string(), source);
}
