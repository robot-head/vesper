use vesper_fmt::*;
use vesper_syntax::{
    lexer::{TokenKind as K, lex},
    parser::parse,
};
fn evidence(source: &str) -> Vec<(K, String)> {
    let (tokens, errors) = lex(source);
    assert!(errors.is_empty());
    tokens
        .into_iter()
        .filter(|t| !matches!(t.kind, K::Whitespace | K::Eof))
        .map(|t| (t.kind, source[t.span].to_owned()))
        .collect()
}
fn verify(source: &str) -> String {
    let result =
        format_source(source, Limits::default()).unwrap_or_else(|e| panic!("{e}: {source}"));
    assert!(parse(&result.output).diagnostics.is_empty());
    assert_eq!(evidence(source), evidence(&result.output));
    let twice = format_source(&result.output, Limits::default()).unwrap();
    assert_eq!(twice.output, result.output);
    assert!(!twice.changed);
    result.output
}
#[test]
fn canonical_simple_program_and_empty() {
    assert_eq!(
        verify("fn main( )->u64{let x=1;x}"),
        "fn main() -> u64 {\n    let x = 1;\n    x\n}\n"
    );
    assert_eq!(verify(""), "\n");
}
#[test]
fn comments_unicode_strings_projection_effects_and_generics() {
    for source in [
        "// Unicode 🦀 é\nfn main()->u64{/*outer/* nested */tail*/let x=1;// keep next token\n x}",
        "copy struct Store{index:u64,metrics:u64}\nfn f(inout s.{index,metrics}:Store)->u64 eff[Fail[ArithOverflow]]{s.index+s.metrics}",
        "fn identity[T:mod copy](in value:T)->T{value}",
        "fn main()->u64{// trailing without a token on this line\n1}",
        "// eof with CR\r\nfn main(){\"héllo 🦀\\n\"}// terminal\r",
        "fn main()->u64{let x=1;/*single*/x}",
    ] {
        verify(source);
    }
}
#[test]
fn line_comment_never_absorbs_next_token() {
    let output = verify("fn main()->u64{let x=1;//must end\n x}");
    assert!(output.contains("//must end\n"));
    assert_eq!(
        evidence(&output)
            .iter()
            .filter(|(k, _)| *k == K::Comment)
            .count(),
        1
    );
    verify("fn f(in x:u64,// args\n in y:u64)->u64{x}");
}
#[test]
fn long_groups_obey_100_column_ribbon() {
    let names = (0..12)
        .map(|i| format!("in parameter_{i}: u64"))
        .collect::<Vec<_>>()
        .join(",");
    let output = verify(&format!("fn call({names})->u64{{0}}"));
    assert!(output.lines().all(|s| s.chars().count() <= 100));
    assert!(output.contains("\n"));
    let literal = format!("fn main(){{\"{}\"}}", "é".repeat(120));
    let output = verify(&literal);
    assert!(output.contains(&"é".repeat(120)));
}
#[test]
fn incomplete_invalid_and_opaque_source_fails_conservatively() {
    for source in [
        "fn main(){",
        "fn main(){let x=",
        "fn main(){quote {  raw  }}",
        "fn main(){m!{ /* raw */  token }}",
        "/*never closes",
        "fn main(){\"bad\\x\"}",
        "fn main(){🦀}",
    ] {
        assert!(
            format_source(source, Limits::default()).is_err(),
            "{source}"
        );
    }
}
#[test]
fn real_repository_source_corpus() {
    let corpus = [
        include_str!("../../../examples/projections.ves"),
        include_str!("../../../examples/closures.ves"),
        include_str!("../../../examples/counter.ves"),
        include_str!("../../../examples/transaction.ves"),
        include_str!("../../../examples/fibonacci.ves"),
        include_str!("../../../examples/resumptions.ves"),
        include_str!("../../../examples/checked_math.ves"),
        include_str!("../../../examples/traits/shapes.ves"),
        include_str!("../../../examples/native-pulse/typed_failures.ves"),
    ];
    for source in corpus {
        verify(source);
    }
}
#[test]
fn checked_source_and_document_limits() {
    let source = "fn main(){1}";
    let base = Limits::default();
    for (limits, error) in [
        (
            Limits {
                max_input_bytes: 1,
                ..base
            },
            Error::InputLimit,
        ),
        (
            Limits {
                max_output_bytes: 1,
                ..base
            },
            Error::OutputLimit,
        ),
        (
            Limits {
                max_tokens: 1,
                ..base
            },
            Error::TokenLimit,
        ),
        (
            Limits {
                max_doc_nodes: 1,
                ..base
            },
            Error::NodeLimit,
        ),
        (
            Limits {
                max_work: 1,
                ..base
            },
            Error::WorkLimit,
        ),
        (
            Limits {
                max_depth: 0,
                ..base
            },
            Error::DepthLimit,
        ),
    ] {
        assert_eq!(format_source(source, limits).unwrap_err(), error);
    }
    let mut doc = Document::new(base).unwrap();
    let text = doc.push(Doc::Text("value")).unwrap();
    let mut other = Document::new(base).unwrap();
    assert_eq!(other.push(Doc::Group(text)).unwrap_err(), Error::InvalidDoc);
    let mut root = text;
    for _ in 0..255 {
        root = doc.push(Doc::Group(root)).unwrap();
    }
    assert_eq!(doc.push(Doc::Group(root)).unwrap_err(), Error::DepthLimit);
    assert!(format_source(&format!("fn main(){{{}true}}", "!".repeat(200)), base).is_err());
}

#[test]
fn bounded_doc_algebra_flat_broken_unicode_and_shared_subtrees() {
    let mut doc = Document::new(Limits::default()).unwrap();
    let text = doc.push(Doc::Text("é🦀")).unwrap();
    let soft = doc.push(Doc::SoftLineOrSpace).unwrap();
    let nil = doc.push(Doc::Nil).unwrap();
    let line = doc.push(Doc::Concat(vec![text, soft, text, nil])).unwrap();
    let group = doc.push(Doc::Group(line)).unwrap();
    assert_eq!(doc.render(group).unwrap(), "é🦀 é🦀");
    let long = "x".repeat(99);
    let prefix = doc.push(Doc::Text(&long)).unwrap();
    let root = doc.push(Doc::Concat(vec![prefix, group])).unwrap();
    assert_eq!(doc.render(root).unwrap(), format!("{long}é🦀\né🦀"));
    let mut limited = Document::new(Limits {
        max_output_bytes: 3,
        ..Limits::default()
    })
    .unwrap();
    let node = limited.push(Doc::Text("four")).unwrap();
    assert_eq!(limited.render(node).unwrap_err(), Error::OutputLimit);
    let mut work = Document::new(Limits {
        max_work: 16,
        ..Limits::default()
    })
    .unwrap();
    let mut root = work.push(Doc::Text("x")).unwrap();
    for _ in 0..3 {
        root = work.push(Doc::Concat(vec![root, root])).unwrap();
    }
    assert_eq!(work.render(root).unwrap_err(), Error::WorkLimit);
}
#[test]
fn whitespace_and_comment_perturbations_are_canonical() {
    let input = "fn f(in x:u64,in y:u64)->u64{let z=x+y;z}";
    let tokens = lex(input).0;
    let mut spaced = String::new();
    for token in tokens {
        if token.kind == K::Eof {
            continue;
        }
        spaced.push_str(" \t\n");
        spaced.push_str(&input[token.span]);
    }
    assert_eq!(verify(input), verify(&spaced));
    verify(
        "/* lead */ fn f(/*inside*/in x:u64,// adjacent next argument\n in y:u64)->u64{x/*tail*/}",
    );
}

#[test]
fn nested_group_separators_cannot_bypass_parser_chain_budget() {
    let repeated = (0..200).map(|_| "f(1,2)").collect::<Vec<_>>().join("+");
    assert_eq!(
        format_source(&format!("fn main(){{{repeated}}}"), Limits::default()).unwrap_err(),
        Error::DepthLimit
    );
    let repeated = (0..200).map(|_| "{0}").collect::<Vec<_>>().join("+");
    assert_eq!(
        format_source(&format!("fn main(){{{repeated}}}"), Limits::default()).unwrap_err(),
        Error::DepthLimit
    );
}

#[test]
fn ribbon_reserves_following_braces_and_breaks_member_chains() {
    for width in 85..=91 {
        let output = verify(&format!("fn {}()->u64{{0}}", "f".repeat(width)));
        assert!(
            output.lines().all(|line| line.chars().count() <= 100),
            "{output}"
        );
    }
    let chain = ".long_field_name".repeat(25);
    let output = verify(&format!("fn main()->u64{{thing{chain}}}"));
    assert!(
        output.lines().all(|line| line.chars().count() <= 100),
        "{output}"
    );
}

#[test]
fn overlong_lines_break_delimited_lists_not_every_space() {
    // §39.3 signature shape: parameters break one per line, projections and
    // the eff row stay intact, and no token is added for the last parameter.
    let source = "fn filter_ids_unoptimized(inout shard.{metrics.requests}: WorkerShard, \
        in shard.{config.limit}: WorkerShard, in raw_ids: Slice[u64]) -> u32 eff [Alloc] { \
        let total = compute_something_long(first_argument_value, second_argument_value, \
        third_argument_value, fourth_argument_value); total }";
    assert_eq!(
        verify(source),
        "fn filter_ids_unoptimized(
    inout shard.{metrics.requests}: WorkerShard,
    in shard.{config.limit}: WorkerShard,
    in raw_ids: Slice[u64]
) -> u32 eff [Alloc] {
    let total = compute_something_long(
        first_argument_value,
        second_argument_value,
        third_argument_value,
        fourth_argument_value
    );
    total
}
"
    );
}

#[test]
fn source_trailing_comma_keeps_a_list_vertical() {
    let output =
        verify("fn f(inout shard.{metrics}: WorkerShard, in raw: Slice[u64],) -> u32 { 0 }");
    assert_eq!(
        output,
        "fn f(\n    inout shard.{metrics}: WorkerShard,\n    in raw: Slice[u64],\n) -> u32 {\n    0\n}\n"
    );
    // Without the trailing comma the same list fits on one line.
    assert_eq!(
        verify("fn f(inout shard.{metrics}: WorkerShard, in raw: Slice[u64]) -> u32 { 0 }"),
        "fn f(inout shard.{metrics}: WorkerShard, in raw: Slice[u64]) -> u32 {\n    0\n}\n"
    );
}

#[test]
fn overlong_chains_and_infix_operators_break_with_nesting() {
    let output = verify(
        "fn main() -> i64 { let value = some_object.first_method(argument_one).second_method(argument_two).third_method(argument_three); \
         let long = alpha_value_number_one + beta_value_number_two + gamma_value_number_three + delta_value_four; long }",
    );
    assert_eq!(
        output,
        "fn main() -> i64 {
    let value = some_object
        .first_method(argument_one)
        .second_method(argument_two)
        .third_method(argument_three);
    let long = alpha_value_number_one
        + beta_value_number_two
        + gamma_value_number_three
        + delta_value_four;
    long
}
"
    );
    // Paths never split, even when the line must break.
    let long = "fn main() -> i64 { Region::scoped(first_argument_value) + Region::scoped(second_argument_value) + Region::scoped(third_argument_value) }";
    let output = verify(long);
    assert!(
        output.contains("    Region::scoped(first_argument_value)\n        + Region::scoped("),
        "{output}"
    );
}

#[test]
fn generics_and_effect_rows_break_inside_their_brackets() {
    let rows = (0..14)
        .map(|i| format!("Capability{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let output = verify(&format!(
        "fn id[T: mod linear](x: T) -> T eff [{rows}] {{ x }}"
    ));
    assert!(
        output.lines().all(|line| line.chars().count() <= 100),
        "{output}"
    );
    assert!(output.starts_with("fn id[T: mod linear](x: T)"), "{output}");
    assert!(output.contains("\n    Capability0,\n"), "{output}");
    assert!(output.contains("\n] {\n    x\n}\n"), "{output}");
}

#[test]
fn comments_inside_broken_lists_are_conserved() {
    let output = verify("fn main() -> i64 { call(alpha, // first\n beta /* second */, gamma) }");
    assert!(
        output.contains("alpha,\n        // first\n        beta /* second */"),
        "{output}"
    );
}
