use vesper_syntax::{
    ast::{ExprKind, Item},
    macro_source::{self, ErrorKind, Limits, Resource},
    parser,
};

const KEEP: &str = "macro keep($x:expr)->expr{quote{$x}} ";

fn parse(source: &str) -> macro_source::MacroSource {
    macro_source::parse(source, Limits::default()).unwrap_or_else(|e| panic!("{e:?}: {source}"))
}

#[test]
fn unit_group_singleton_multiple_and_nested_values_keep_actual_source_ranges() {
    for value in [
        "()",
        "(42)",
        "(42,)",
        "(40,2)",
        "(40,2,)",
        "(((),),(40,(2,)),)",
    ] {
        let text = format!("{KEEP}fn main(){{keep!({value})}}");
        let source = parse(&text);
        let invocation = &source.invocations()[0];
        assert_eq!(invocation.arguments.len(), 1);
        assert_eq!(&text[invocation.arguments[0].clone()], value);
        assert_eq!(&text[invocation.span.clone()], format!("keep!({value})"));
        assert_eq!(source.source(), text);
        assert_eq!(
            source.function(invocation.function).unwrap().invocations,
            [invocation.id]
        );
    }
}

#[test]
fn quoted_slots_tuples_and_independent_body_invocations_retain_order() {
    let text = "macro pair($x:expr)->expr{quote{($x, ((), $x,),)}} \
                fn main(){(pair!((40,2)), (pair!(()),),)}";
    let source = parse(text);
    assert_eq!(source.definitions().len(), 1);
    assert_eq!(
        &text[source.definitions()[0].quote_span.clone()],
        "($x, ((), $x,),)"
    );
    assert_eq!(source.invocations().len(), 2);
    let first = &source.invocations()[0];
    let second = &source.invocations()[1];
    assert_eq!(&text[first.arguments[0].clone()], "(40,2)");
    assert_eq!(&text[second.arguments[0].clone()], "()");
    assert!(first.span.end <= second.span.start);
    assert_eq!(source.functions()[0].invocations, [first.id, second.id]);
}

#[test]
fn tuple_commas_in_comments_and_strings_are_not_argument_boundaries() {
    let text = "// Unicode λ 🦀 shifts offsets\r\n\
                macro keep($x:expr)->expr{quote{($x, /* , ( ) */ ())}}\r\n\
                fn main(){keep!((\"comma, close) fake!(0)\", /* , ( { */ 42,))}";
    let source = parse(text);
    assert_eq!(source.invocations().len(), 1);
    let argument = &source.invocations()[0].arguments[0];
    assert_eq!(
        &text[argument.clone()],
        "(\"comma, close) fake!(0)\", /* , ( { */ 42,)"
    );
    assert!(text.is_char_boundary(argument.start) && text.is_char_boundary(argument.end));
    assert_eq!(
        &text[source.definitions()[0].quote_span.clone()],
        "($x, /* , ( ) */ ())"
    );
}

#[test]
fn tuple_arity_is_independent_of_macro_parameter_limits() {
    let wide = format!(
        "({},)",
        (0..40).map(|n| n.to_string()).collect::<Vec<_>>().join(",")
    );
    let text = format!("macro wide()->expr{{quote{{{wide}}}}} fn main(){{wide!()}}");
    let source = macro_source::parse(
        &text,
        Limits {
            max_parameters: 0,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(source.definitions()[0].parameters.is_empty());
    assert_eq!(&text[source.definitions()[0].quote_span.clone()], wide);

    let input = format!("{KEEP}fn main(){{keep!({wide})}}");
    let source = macro_source::parse(
        &input,
        Limits {
            max_parameters: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(source.invocations()[0].arguments.len(), 1);
    assert_eq!(&input[source.invocations()[0].arguments[0].clone()], wide);
}

#[test]
fn tuple_support_does_not_relax_call_commas_or_nested_macro_restrictions() {
    for tail in [
        "fn main(){keep!(42,)}",
        "fn main(){keep!((42,),)}",
        "fn main(){keep!(helper(42,))}",
        "fn main(){helper(keep!(()),)}",
        "fn main(){keep!((keep!(42),))}",
        "fn main(){keep!(((),keep!(42)))}",
    ] {
        let text = format!("{KEEP}{tail}");
        assert!(
            macro_source::parse(&text, Limits::default()).is_err(),
            "{text}"
        );
    }
    let quoted_nested = "macro keep($x:expr)->expr{quote{($x,keep!(()))}} fn main(){keep!(42)}";
    assert_eq!(
        macro_source::parse(quoted_nested, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    let accepted = parse(&format!("{KEEP}fn main(){{helper(keep!((42,)))}}"));
    assert_eq!(accepted.invocations().len(), 1);
}

#[test]
fn malformed_commas_delimiters_and_missing_values_refuse_in_quote_and_input() {
    for value in [
        "(,)", "(,42)", "(42,,)", "(42,,2)", "(42 2)", "(42,;2)", "(42,]", "(42,", "((42,)",
    ] {
        for text in [
            format!("macro bad()->expr{{quote{{{value}}}}} fn main(){{bad!()}}"),
            format!("{KEEP}fn main(){{keep!({value})}}"),
        ] {
            assert!(
                macro_source::parse(&text, Limits::default()).is_err(),
                "{text}"
            );
        }
    }
}

#[test]
fn parameter_inventory_and_real_support_tuple_ast_are_unchanged() {
    let ordinary = "fn unit(){()} fn group()->i64{(42)} fn singleton()->(i64,){(42,)} \
                    fn nested()->((i64,),()){((42,),())}";
    let actual = parser::parse(ordinary);
    assert!(actual.diagnostics.is_empty(), "{:?}", actual.diagnostics);
    let source = parse(ordinary);
    assert_eq!(source.support_items(), &actual.module);
    for (i, arity) in [(0, 0), (2, 1), (3, 2)] {
        let Item::Function(function) = &source.support_items().items[i] else {
            panic!("missing real function")
        };
        let ExprKind::Block {
            tail: Some(tail), ..
        } = &function.body.as_ref().unwrap().kind
        else {
            panic!("missing actual body tail")
        };
        let ExprKind::Tuple(values) = &tail.kind else {
            panic!("tuple form changed")
        };
        assert_eq!(values.len(), arity);
    }
    let Item::Function(group) = &source.support_items().items[1] else {
        panic!("missing group")
    };
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &group.body.as_ref().unwrap().kind
    else {
        panic!("missing group tail")
    };
    assert!(matches!(tail.kind, ExprKind::Literal(_)));

    let text = format!("{KEEP}fn f(in self:(i64,),inout pair:(i64,i64)){{keep!((self,pair))}}");
    let source = parse(&text);
    let names = source
        .parameter_name_spans(source.functions()[0].id)
        .unwrap();
    assert_eq!(
        names
            .iter()
            .map(|span| &text[span.clone()])
            .collect::<Vec<_>>(),
        ["self", "pair"]
    );
    assert_eq!(
        &text[source.invocations()[0].arguments[0].clone()],
        "(self,pair)"
    );
}

#[test]
fn nested_tuple_work_depth_and_metadata_refusals_are_bounded() {
    let value = format!("{}42{}", "(".repeat(12), ")".repeat(12));
    let text = format!("{KEEP}fn main(){{keep!({value})}}");
    assert!(macro_source::parse(&text, Limits::default()).is_ok());
    for (limits, expected) in [
        (
            Limits {
                max_depth: 4,
                ..Limits::default()
            },
            Resource::Depth,
        ),
        (
            Limits {
                max_work: text.len() * 4,
                ..Limits::default()
            },
            Resource::Work,
        ),
        (
            Limits {
                max_metadata_bytes: 1,
                ..Limits::default()
            },
            Resource::Metadata,
        ),
    ] {
        assert_eq!(
            macro_source::parse(&text, limits).unwrap_err().kind,
            ErrorKind::Budget(expected)
        );
    }
}

#[test]
fn every_utf8_prefix_refuses_or_retains_exact_tuple_argument_spans() {
    let text = format!("// λ 🦀\r\n{KEEP}fn main(){{keep!((\"comma,)\", (42,), /* , */ (),))}}");
    for end in (0..=text.len()).filter(|end| text.is_char_boundary(*end)) {
        let prefix = &text[..end];
        let Ok(source) = macro_source::parse(prefix, Limits::default()) else {
            continue;
        };
        assert_eq!(source.source(), prefix);
        for invocation in source.invocations() {
            assert!(prefix.get(invocation.span.clone()).is_some());
            for argument in &invocation.arguments {
                assert!(prefix.get(argument.clone()).is_some());
                assert!(
                    argument.start >= invocation.span.start && argument.end <= invocation.span.end
                );
            }
        }
    }
}
