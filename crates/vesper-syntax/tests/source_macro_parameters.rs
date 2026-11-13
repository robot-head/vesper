use vesper_syntax::{
    ast::{Item, Pass, Type},
    lexer::TokenKind as K,
    macro_source::{self, ErrorKind, Limits, Resource},
};

const KEEP: &str = "macro keep($x:expr)->expr{quote{$x}} ";

fn parse(source: &str) -> macro_source::MacroSource {
    macro_source::parse(source, Limits::default()).unwrap_or_else(|e| panic!("{e:?}: {source}"))
}

#[test]
fn exact_header_tokens_preserve_passes_types_comments_and_self() {
    let text = format!(
        "// Unicode before offsets: λ 🦀\r\n{KEEP}\
         fn select(in /* value fake */ value: i64,\
         inout /* count */ count: i64, sink self: Token, fallback: i64)\
         -> i64 eff [] {{ keep!(value) }}"
    );
    let source = parse(&text);
    let function = &source.functions()[0];
    let names = source.parameter_name_spans(function.id).unwrap();
    assert_eq!(names.len(), 4);
    assert_eq!(
        function
            .header
            .params
            .iter()
            .map(|p| p.pass)
            .collect::<Vec<_>>(),
        [Pass::In, Pass::Inout, Pass::Sink, Pass::In]
    );
    for (i, (span, expected)) in names
        .iter()
        .zip(["value", "count", "self", "fallback"])
        .enumerate()
    {
        assert_eq!(text.get(span.clone()), Some(expected));
        assert!(span.start >= function.header.params[i].span.start);
        assert!(span.end <= function.header.params[i].span.end);
        assert!(span.start >= function.header.span.start);
        assert!(span.end <= function.header.span.end);
        let token = source.tokens().iter().find(|t| &t.span == span).unwrap();
        assert_eq!(
            token.kind,
            if expected == "self" {
                K::SelfKw
            } else {
                K::Lower
            }
        );
    }
    assert_eq!(function.header.params[0].ty, Type::named("i64"));
    assert_eq!(function.header.params[2].ty, Type::named("Token"));
    assert_eq!(&text[function.body_span.clone()], "{ keep!(value) }");
    assert!(source.support_items().items.is_empty());
}

#[test]
fn public_header_copies_cannot_change_the_sealed_inventory() {
    let text = format!("{KEEP}fn f(in value:i64)->i64{{keep!(value)}}");
    let source = parse(&text);
    let function = &source.functions()[0];
    let original = source.parameter_name_spans(function.id).unwrap().to_vec();
    let mut altered = function.clone();
    altered.header.params[0].name = "forged".into();
    altered.header.params[0].span = 0..1;
    altered.header.params.clear();
    assert_eq!(source.parameter_name_spans(altered.id).unwrap(), original);
    assert_eq!(&text[original[0].clone()], "value");
    assert_eq!(
        source.function(altered.id).unwrap().header.params[0].name,
        "value"
    );
}

#[test]
fn function_and_source_identity_select_only_the_actual_parameter_tokens() {
    let text = format!(
        "{KEEP}fn first(in x:i64)->i64{{keep!(x)}}\n\
         fn second(sink x:i64)->i64{{keep!(x)}}\nfn empty()->i64{{keep!(42)}}"
    );
    let source = parse(&text);
    let first = source
        .parameter_name_spans(source.functions()[0].id)
        .unwrap();
    let second = source
        .parameter_name_spans(source.functions()[1].id)
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(&text[first[0].clone()], "x");
    assert_eq!(&text[second[0].clone()], "x");
    assert!(
        source
            .parameter_name_spans(source.functions()[2].id)
            .unwrap()
            .is_empty()
    );
    let changed = parse(&format!("// same headers, shifted source\n{text}"));
    assert_eq!(
        changed
            .parameter_name_spans(source.functions()[0].id)
            .unwrap_err()
            .kind,
        ErrorKind::ForeignId
    );
}

#[test]
fn duplicate_macro_function_parameters_refuse_before_a_rename_can_repair_them() {
    for header in [
        "in value:i64, sink value:i64",
        "value:i64, /* intervening name */ value:i64",
        "in self:i64, inout self:i64",
    ] {
        let text = format!("{KEEP}fn f({header})->i64{{keep!(42)}}");
        let failure = macro_source::parse(&text, Limits::default()).unwrap_err();
        assert_eq!(failure.kind, ErrorKind::DuplicateName);
        assert!(text[failure.span.unwrap()].contains(':'));
    }
}

#[test]
fn macro_function_parameter_count_observes_default_and_hard_caps() {
    fn text(count: usize) -> String {
        let params = (0..count)
            .map(|i| format!("p{i}:i64"))
            .collect::<Vec<_>>()
            .join(",");
        format!("{KEEP}fn f({params})->i64{{keep!(42)}}")
    }
    let source = parse(&text(8));
    assert_eq!(
        source
            .parameter_name_spans(source.functions()[0].id)
            .unwrap()
            .len(),
        8
    );
    assert_eq!(
        macro_source::parse(&text(9), Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Budget(Resource::Parameters)
    );
    let hard_count = Limits {
        max_parameters: 32,
        ..Limits::default()
    };
    let source = macro_source::parse(&text(32), hard_count).unwrap();
    assert_eq!(
        source
            .parameter_name_spans(source.functions()[0].id)
            .unwrap()
            .len(),
        32
    );
    assert_eq!(
        macro_source::parse(&text(33), hard_count).unwrap_err().kind,
        ErrorKind::Budget(Resource::Parameters)
    );
    assert_eq!(
        macro_source::parse(
            "macro zero()->expr{quote{42}} fn f(in x:i64)->i64{zero!()}",
            Limits {
                max_parameters: 0,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Budget(Resource::Parameters)
    );
}

#[test]
fn support_functions_keep_generic_projected_duplicate_and_large_headers() {
    let many = (0..9)
        .map(|i| format!("p{i}:i64"))
        .collect::<Vec<_>>()
        .join(",");
    let text = format!(
        "macro zero()->expr{{quote{{42}}}} \
         fn projected[T](inout pair.{{left}}:Pair[T])->T{{pair.left}} \
         fn duplicate(in x:i64,sink x:i64)->i64{{x}} \
         fn many({many})->i64{{p0}} fn f()->i64{{zero!()}}"
    );
    let source = macro_source::parse(
        &text,
        Limits {
            max_parameters: 0,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(source.functions()[0].header.generics.len(), 1);
    assert_eq!(
        source.functions()[0].header.params[0].projections,
        [vec![String::from("left")]]
    );
    assert_eq!(
        source
            .parameter_name_spans(source.functions()[0].id)
            .unwrap()
            .len(),
        1
    );
    let duplicates = source
        .parameter_name_spans(source.functions()[1].id)
        .unwrap();
    assert_eq!(duplicates.len(), 2);
    assert_ne!(duplicates[0], duplicates[1]);
    assert_eq!(
        source
            .parameter_name_spans(source.functions()[2].id)
            .unwrap()
            .len(),
        9
    );
    let Item::Function(actual) = &source.support_items().items[0] else {
        panic!("missing actual support AST")
    };
    assert_eq!(
        &text[actual.span.clone()],
        "fn projected[T](inout pair.{left}:Pair[T])->T{pair.left}"
    );
    assert_eq!(
        actual.params[0].projections,
        source.functions()[0].header.params[0].projections
    );
}

#[test]
fn projected_and_generic_macro_callers_remain_explicitly_unsupported() {
    for header in [
        "fn f[T](in value:T)->T",
        "fn f(inout value.{left}:Pair)->i64",
    ] {
        let text = format!("{KEEP}{header}{{keep!(42)}}");
        assert_eq!(
            macro_source::parse(&text, Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::Unsupported
        );
    }
}

#[test]
fn every_utf8_prefix_either_refuses_or_retains_only_actual_parameter_ranges() {
    let text = format!(
        "// λ 🦀\r\n{KEEP}fn f(in /* self */ self:i64,inout count:i64)->i64{{keep!(self)}}"
    );
    for end in (0..=text.len()).filter(|end| text.is_char_boundary(*end)) {
        let prefix = &text[..end];
        let Ok(source) = macro_source::parse(prefix, Limits::default()) else {
            continue;
        };
        for function in source.functions() {
            let names = source.parameter_name_spans(function.id).unwrap();
            assert_eq!(names.len(), function.header.params.len());
            for (span, param) in names.iter().zip(&function.header.params) {
                assert_eq!(prefix.get(span.clone()), Some(param.name.as_str()));
                assert!(span.start >= param.span.start && span.end <= param.span.end);
                assert!(span.end <= function.body_span.start);
            }
        }
    }
}
