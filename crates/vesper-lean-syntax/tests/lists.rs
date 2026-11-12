use vesper_lean_syntax::Term;

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
