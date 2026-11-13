use vesper_lean_syntax::{Declaration, Definition, DefinitionKind, Module, Name, Term};

#[test]
fn ordinary_lists_include_empty_nested_and_escaped_literal_elements() {
    assert_eq!(Term::List(vec![]).render().unwrap(), "[]");
    let value = Term::List(vec![
        Term::String("end\naxiom injected : False\n\" 🦀".into()),
        Term::String("literal".into()),
    ]);
    assert_eq!(
        value.render().unwrap(),
        "[\"end\\naxiom injected : False\\n\\\" 🦀\", \"literal\"]"
    );
    assert_eq!(
        Term::List(vec![Term::List(vec![Term::Bool(true)]), Term::List(vec![])])
            .render()
            .unwrap(),
        "[[true], []]"
    );
}

#[test]
fn total_noncomputable_definitions_use_an_explicit_ast_kind() {
    let module = Module {
        imports: vec![],
        declarations: vec![Declaration::Definition(Definition {
            name: Name::single("logicalList").unwrap(),
            binders: vec![],
            result: Term::app(
                Term::Ref(Name::single("List").unwrap()),
                [Term::Ref(Name::single("String").unwrap())],
            ),
            body: Term::List(vec![Term::String("/* literal */".into())]),
            kind: DefinitionKind::NoncomputableDef,
        })],
    };
    let printed = module.render().unwrap();
    assert!(printed.starts_with("noncomputable def "));
    assert!(printed.contains("[\"/* literal */\"]"));
    assert!(!printed.contains("partial def"));
}
