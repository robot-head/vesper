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
fn parser_sugar_keeps_its_surface_syntax() {
    assert_eq!(
        verify(
            "const LIMIT:u64=10;fn f(n:u64)->u64{for i in 0 .. LIMIT{if i==n{continue;}}match n{0|1=>LIMIT,_=>n,}}"
        ),
        "const LIMIT: u64 = 10;
fn f(n: u64) -> u64 {
    for i in 0..LIMIT {
        if i == n {
            continue;
        }
    }
    match n {
        0 | 1 => LIMIT,
        _ => n,
    }
}
"
    );
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
    assert!(
        output.contains("\n    -> T eff [\n        Capability0,\n"),
        "{output}"
    );
    assert!(output.contains("\n    ] {\n    x\n}\n"), "{output}");
}

#[test]
fn comments_inside_broken_lists_are_conserved() {
    let output = verify("fn main() -> i64 { call(alpha, // first\n beta /* second */, gamma) }");
    assert!(
        output.contains("alpha,\n        // first\n        beta /* second */"),
        "{output}"
    );
}

#[test]
fn one_blank_line_between_items_and_statements_is_kept() {
    let output = verify(
        "fn a() -> u64 {\n    let x = 1;\n\n\n    x\n}\n\n\n\nfn b() -> u64 { 2 }\n// next\nfn c() -> u64 { 3 }",
    );
    assert_eq!(
        output,
        "fn a() -> u64 {\n    let x = 1;\n\n    x\n}\n\nfn b() -> u64 {\n    2\n}\n// next\nfn c() -> u64 {\n    3\n}\n"
    );
    // A blank line right inside a block is not kept, and none has trailing spaces.
    assert_eq!(
        verify("fn a() -> u64 {\n\n    1\n\n}"),
        "fn a() -> u64 {\n    1\n}\n"
    );
}

#[test]
fn prefix_operators_and_keyword_parentheses_are_spaced() {
    assert_eq!(
        verify("fn f(x: i64) -> i64 { let y = - 1; let z = ! (x == y); g(-x, y - -1) }"),
        "fn f(x: i64) -> i64 {\n    let y = -1;\n    let z = !(x == y);\n    g(-x, y - -1)\n}\n"
    );
    assert_eq!(
        verify("fn main() -> i64 { handle Counter in { 1 } with { return (value) => value, } }"),
        "fn main() -> i64 {\n    handle Counter in {\n        1\n    } with {\n        return (value) => value,\n    }\n}\n"
    );
    assert_eq!(
        verify("fn h() -> fn () -> i64 { g }"),
        "fn h() -> fn() -> i64 {\n    g\n}\n"
    );
}

#[test]
fn small_macro_modules_with_several_functions_fit_the_default_budget() {
    let source = "fn increment(in x: u64) -> u64 eff [Fail[ArithOverflow]] { wrapping_add(x, 1) }\n\
        macro bump() -> expr { quote { increment(41) } }\n\
        fn main() -> u64 eff [Fail[ArithOverflow]] { bump!() }\n";
    let output = format_source(source, Limits::default()).unwrap().output;
    assert!(output.contains("macro bump() -> expr {"), "{output}");
    assert!(!format_source(&output, Limits::default()).unwrap().changed);
}

#[test]
fn comment_heavy_macro_modules_are_not_refused_by_the_allowance_estimate() {
    let source = format!(
        "{}macro keep($x:expr)->expr{{quote{{$x}}}}fn main()->i64{{keep!(42)}}",
        "//c\n".repeat(800)
    );
    let output = format_source(&source, Limits::default()).unwrap().output;
    assert!(output.ends_with("keep!(42)\n}\n"), "{output}");
}

fn token_count(source: &str) -> usize {
    vesper_syntax::lexer::lex_with_limits(
        source,
        vesper_syntax::lexer::LexLimits {
            max_source_bytes: vesper_syntax::lexer::HARD_SOURCE_BYTES,
            max_tokens: vesper_syntax::lexer::HARD_TOKENS,
            max_diagnostics: 1,
        },
    )
    .unwrap()
    .tokens
    .len()
}
/// A canonical module of `items` functions, each with calls, a tuple, a block
/// and a comment, so every layout path the renderer has is exercised.
fn large_module(items: usize) -> String {
    let mut source = String::new();
    for i in 0..items {
        source.push_str(&format!(
            "// item {i}\nfn item_{i}(in x: u64, in y: u64) -> u64 {{\n    let pair = (x, y);\n    if x > y {{\n        add(x, y) * 2\n    }} else {{\n        y\n    }}\n}}\n\n"
        ));
    }
    source.pop();
    source
}

#[test]
fn a_64k_token_file_formats_inside_the_default_budget() {
    let source = large_module(1200);
    assert!(
        token_count(&source) >= 64 * 1024,
        "{}",
        token_count(&source)
    );
    let result = format_source(&source, Limits::default()).unwrap();
    assert!(!result.changed, "the generated module is canonical");
    assert_eq!(result.output, source);
    // A scrambled copy reaches the same canonical text.
    let scrambled = source.replace("\n    ", "\n").replace(", ", ",");
    assert_eq!(
        format_source(&scrambled, Limits::default()).unwrap().output,
        source
    );
}

#[test]
fn formatting_work_grows_linearly_with_the_file() {
    // The work budget that admits a file must admit half of it with half the
    // budget (plus slack for constant terms): no quadratic charge remains.
    let full = large_module(400);
    let half = large_module(200);
    let mut low = 0usize;
    let mut high = 1usize << 31;
    while low + 1 < high {
        let mid = low + (high - low) / 2;
        let limits = Limits {
            max_work: mid,
            ..Limits::default()
        };
        if format_source(&full, limits).is_ok() {
            high = mid;
        } else {
            low = mid;
        }
    }
    let limits = Limits {
        max_work: high / 2 + 64 * 1024,
        ..Limits::default()
    };
    format_source(&half, limits).unwrap();
}

#[test]
fn many_methods_in_one_block_are_not_a_depth_error() {
    let mut source = String::from("impl Wheel {\n");
    for i in 0..64 {
        source.push_str(&format!(
            "    pub fn tick_{i}(inout self: Wheel, in now: u64) -> u64 {{\n        now\n    }}\n"
        ));
    }
    source.push_str("}\n");
    assert_eq!(verify(&source), source);
}

#[test]
fn many_items_in_one_block_render_in_linear_time() {
    // Rendering used to copy the whole pending stack for every group, so a
    // block with tens of thousands of statements was quadratic.
    let mut source = String::from("fn main() {\n");
    for _ in 0..9_000 {
        source.push_str("    let x = (a, b);\n");
    }
    source.push_str("}\n");
    assert!(token_count(&source) >= 120_000);
    let start = std::time::Instant::now();
    let result = format_source(&source, Limits::default()).unwrap();
    assert!(!result.changed);
    // Generous enough for slow CI machines and debug builds, far below the
    // tens of seconds the quadratic renderer needed in a debug build.
    assert!(start.elapsed() < std::time::Duration::from_secs(60));
}

#[test]
fn adversarial_inputs_stay_typed_errors() {
    let deep = format!("fn main() {{{}0{}}}", "(".repeat(5000), ")".repeat(5000));
    assert_eq!(
        format_source(&deep, Limits::default()).unwrap_err(),
        Error::DepthLimit
    );
    let deep_blocks = format!("fn main() {}0{}", "{".repeat(5000), "}".repeat(5000));
    assert_eq!(
        format_source(&deep_blocks, Limits::default()).unwrap_err(),
        Error::DepthLimit
    );
    // A chain closed by blocks still counts: `}` followed by an operator
    // continues the expression.
    let chain = (0..200)
        .map(|_| "if c { 0 } else { 1 }")
        .collect::<Vec<_>>()
        .join(" + ");
    assert_eq!(
        format_source(&format!("fn main() {{{chain}}}"), Limits::default()).unwrap_err(),
        Error::DepthLimit
    );
    // Past the token ceiling the input is refused before parsing.
    let huge = "x ".repeat(DEFAULT_MAX_TOKENS);
    assert_eq!(
        format_source(&huge, Limits::default()).unwrap_err(),
        Error::TokenLimit
    );
    let too_long = "x".repeat(Limits::default().max_input_bytes + 1);
    assert_eq!(
        format_source(&too_long, Limits::default()).unwrap_err(),
        Error::InputLimit
    );
    // Caller limits above the lexer's hard ceilings are clamped, never a panic.
    let unbounded = Limits {
        max_input_bytes: usize::MAX,
        max_output_bytes: usize::MAX,
        max_tokens: usize::MAX,
        max_doc_nodes: usize::MAX,
        max_depth: usize::MAX,
        max_work: usize::MAX,
    };
    assert!(format_source(&huge, unbounded).is_err());
    assert!(format_source(&deep, unbounded).is_err());
}

#[test]
fn many_locals_in_one_function_format_in_linear_time() {
    // The parser's scope sets used to be cloned at every block, which made
    // one function with thousands of locals quadratic, and the preflight
    // refused it with `WorkLimit`. Scope sets are now an undo trail.
    let mut source = String::from("fn main() {\n");
    for i in 0..8_000 {
        source.push_str(&format!("    let x{i} = {{\n        0\n    }};\n"));
    }
    source.push_str("}\n");
    let started = std::time::Instant::now();
    assert_eq!(
        format_source(&source, Limits::default()).unwrap().output,
        source
    );
    // Generous for debug builds; the quadratic parser took seconds in release.
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    // The same number of locals spread over many functions is linear.
    let mut spread = String::new();
    for f in 0..400 {
        spread.push_str(&format!("fn f{f}() {{\n"));
        for i in 0..20 {
            spread.push_str(&format!("    let x{i} = {{\n        0\n    }};\n"));
        }
        spread.push_str("}\n\n");
    }
    spread.pop();
    assert_eq!(
        format_source(&spread, Limits::default()).unwrap().output,
        spread
    );
}

#[test]
fn lookahead_over_shared_empty_nodes_is_charged() {
    let mut doc = Document::new(Limits {
        max_work: 1_000_000,
        ..Limits::default()
    })
    .unwrap();
    let nil = doc.push(Doc::Nil).unwrap();
    let group = doc.push(Doc::Group(nil)).unwrap();
    let empty = doc.push(Doc::Concat(vec![])).unwrap();
    let mut children = vec![group; 500];
    children.extend(std::iter::repeat_n(empty, 100_000));
    let root = doc.push(Doc::Concat(children)).unwrap();
    assert_eq!(doc.render(root).unwrap_err(), Error::WorkLimit);
}

#[test]
fn wrapped_boolean_conditions_keep_short_comparisons_together() {
    let output = verify(
        "fn f() -> bool { rejected_before_next && best.len() == 1 \
         && best.try_get(0) == Option::Some(3) && stats.accepted > 0 && stats.invalid > 0 \
         && stats.not_reproducing > 0 && stats.runs < 80 && !stats.budget_exhausted }",
    );
    assert_eq!(
        output,
        "fn f() -> bool {
    rejected_before_next
        && best.len() == 1
        && best.try_get(0) == Option::Some(3)
        && stats.accepted > 0
        && stats.invalid > 0
        && stats.not_reproducing > 0
        && stats.runs < 80
        && !stats.budget_exhausted
}
"
    );
}

#[test]
fn wrapped_method_arguments_nest_under_the_method() {
    let output = verify(
        "fn f() { long_receiver_name.some_method_with_a_long_name(\
         first_argument_with_a_long_name, second_argument_with_a_long_name) }",
    );
    assert_eq!(
        output,
        "fn f() {
    long_receiver_name
        .some_method_with_a_long_name(
            first_argument_with_a_long_name,
            second_argument_with_a_long_name
        )
}
"
    );
}

#[test]
fn brace_patterns_keep_the_assignment_or_match_arrow() {
    let output = verify("fn f() { let Ticket {} = ticket; match ticket { Ticket {} => 1, } }");
    assert!(output.contains("let Ticket {} = ticket;"), "{output}");
    assert!(output.contains("Ticket {} => 1,"), "{output}");
}

#[test]
fn wrapped_tuple_return_type_nests_under_the_return_arrow() {
    let output = verify("fn f() -> (u64,) { (1,) }");
    assert!(
        output.contains("    -> (\n        u64,\n    ) {"),
        "{output}"
    );
}

#[test]
fn subtraction_after_try_and_comments_keeps_binary_spacing() {
    let output = verify("fn f() { let a = call()? - 1; let b = x /* note */ - 1; -1 }");
    assert!(output.contains("call()? - 1;"), "{output}");
    assert!(output.contains("x /* note */ - 1;"), "{output}");
    assert!(output.contains("\n    -1\n"), "{output}");
}

#[test]
fn trailing_comma_before_block_comments_keeps_list_vertical() {
    let output = verify("fn f() { call(alpha, beta, /* keep */) }");
    assert_eq!(
        output,
        "fn f() {\n    call(\n        alpha,\n        beta,\n        /* keep */\n    )\n}\n"
    );
}
