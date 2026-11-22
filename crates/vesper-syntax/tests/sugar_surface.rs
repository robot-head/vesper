//! Stage 1 classifies unexpanded sugar from syntax and trivia alone.
use vesper_syntax::parser::{self, ParseLimits};

fn surface(source: &str) -> parser::SyntaxNode {
    let parsed = parser::parse_surface_with_limits(source, ParseLimits::default()).unwrap();
    assert!(
        parsed.diagnostics.is_empty(),
        "{source}: {:?}",
        parsed.diagnostics
    );
    let root = parsed.syntax();
    assert_eq!(root.text().to_string(), source);
    root
}

#[test]
fn declarations_are_lossless_and_stay_out_of_the_executable_parser() {
    let source = "pub sugar defer : stmt {\n // keep rule\n rule (defer $cleanup:block $..rest:scope) => { $cleanup; $..rest }\n}";
    let root = surface(source);
    for kind in [
        parser::SUGAR_DECL,
        parser::SUGAR_RULE,
        parser::SUGAR_PATTERN,
        parser::SUGAR_TEMPLATE,
        parser::SUGAR_HOLE,
    ] {
        assert!(root.descendants().any(|n| n.kind() == kind), "{kind:?}");
    }
    assert!(!parser::parse(source).diagnostics.is_empty());
}

#[test]
fn skeleton_nodes_respect_call_boundaries_and_continuation_newlines() {
    let root = surface(
        "test \"frames\" { verify() }\nfn f() { ordinary(x); send req on sink ch; \
         let result = with_txn tx on inout db { run() } on_error(e) { recover(e) };\n\
         defer { clean() }\nretry { again() } }",
    );
    for (kind, count) in [
        (parser::SUGAR_ITEM, 1),
        (parser::SUGAR_EXPR, 1),
        (parser::SUGAR_CONTINUATION, 1),
        (parser::SUGAR_STMT, 3),
    ] {
        assert_eq!(
            root.descendants().filter(|n| n.kind() == kind).count(),
            count,
            "{kind:?}"
        );
    }

    let same_line = surface("fn f() { work { a() } then { b() } }");
    let newline = surface("fn f() { work { a() }\nthen { b() } }");
    let continuation_count = |root: &parser::SyntaxNode| {
        root.descendants()
            .filter(|n| n.kind() == parser::SUGAR_CONTINUATION)
            .count()
    };
    assert_eq!(continuation_count(&same_line), 1);
    assert_eq!(continuation_count(&newline), 0);
}

#[test]
fn calls_core_headers_and_ordinary_member_access_keep_core_syntax() {
    for source in [
        "fn f() { let x = f(v).field; if ready{ f(x); } match v { _ => 0, } }",
        "fn f(start: u64) { handle Counter state count = start in { 0 } with { return (v, count) => v, } }",
    ] {
        let core = parser::parse(source);
        assert!(core.diagnostics.is_empty(), "{:?}", core.diagnostics);
        let root = surface(source);
        assert_eq!(root.green(), core.syntax().green(), "{source}");
    }
    let tag_in_header = surface("fn f() { if h([bits{v:2}]) { 1 } }");
    assert_eq!(
        tag_in_header
            .descendants()
            .filter(|node| node.kind() == parser::TAGGED_LITERAL)
            .count(),
        1
    );
    let root = surface("fn f() { f(x); errdefer(e) { recover(e) } }");
    assert_eq!(
        root.descendants()
            .filter(|n| n.kind() == parser::SUGAR_STMT)
            .count(),
        1
    );
}

#[test]
fn adjacent_tags_and_dotted_and_pipe_operators_have_distinct_nodes() {
    let source = "fn f() { let b = hex\"de ad\"; let t = bits{ver: 4}; bits /* gap */ { f() }\n\
                  let pos = a .+ b .* c; frame |> parse(_, in cfg) |?> finish(_) }";
    let root = surface(source);
    let tags: Vec<_> = root
        .descendants()
        .filter(|n| n.kind() == parser::TAGGED_LITERAL)
        .map(|n| n.text().to_string().trim().to_owned())
        .collect();
    assert_eq!(tags, ["hex\"de ad\"", "bits{ver: 4}"]);
    let ops: Vec<_> = root
        .descendants()
        .filter(|n| n.kind() == parser::SUGAR_OPERATOR)
        .map(|n| n.text().to_string())
        .collect();
    assert_eq!(ops, [".+", ".*", "|>", "|?>"]);
    let expressions = |kind| {
        root.descendants()
            .filter(|n| n.kind() == kind)
            .map(|n| n.text().to_string().trim().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(expressions(parser::DOTTED_EXPR), ["a .+ b .* c", "b .* c"]);
    assert_eq!(
        expressions(parser::PIPE_EXPR),
        [
            "frame |> parse(_, in cfg) |?> finish(_)",
            "frame |> parse(_, in cfg)"
        ]
    );
    assert!(!parser::parse(source).diagnostics.is_empty());
}

#[test]
fn malformed_forms_produce_diagnostics_and_retain_all_source_text() {
    for source in [
        "sugar bad : expr {}",
        "sugar bad : infix(`+=`, additive, left) { rule (go) => { x } }",
        "sugar bad : infix(`.+`, additive, sideways) { rule (go) => { x } }",
        "sugar bad : stmt { rule (go $..) => {} }",
        "sugar bad : stmt { rule (go $(x)) => {} }",
        "fn f() { send req on sink ; }",
        "fn f() { let x = a .+ ; }",
        "fn f() { let x = frame |?> ; }",
        "fn f() { x |> parse(_ + 1) }",
        "fn f() { x |> parse(_); _ }",
        "fn f() { let x = bits{(]}; }",
        "fn f() { let x = ; send req on sink ch; }",
    ] {
        let result = parser::parse_surface_with_limits(source, ParseLimits::default()).unwrap();
        assert!(!result.diagnostics.is_empty(), "{source}");
        assert_eq!(result.syntax().text().to_string(), source);
    }
}

#[test]
fn nested_sugar_token_trees_obey_the_parser_depth_limit() {
    let source = format!("fn f() {{ bits{{{}0{}}} }}", "(".repeat(32), ")".repeat(32));
    let limits = ParseLimits {
        max_depth: 8,
        ..ParseLimits::default()
    };
    let result = parser::parse_surface_with_limits(&source, limits).unwrap();
    assert!(!result.diagnostics.is_empty());
    assert_eq!(result.syntax().text().to_string(), source);
}

#[test]
fn test_blocks_parse_their_declarations_and_phrases() {
    let root = surface(
        "test { sugar defer: stmt { rule (defer $b:block) => { $b } } fn f() { defer { cleanup() } } }",
    );
    for kind in [parser::SUGAR_DECL, parser::SUGAR_STMT, parser::SUGAR_GROUP] {
        assert!(root.descendants().any(|node| node.kind() == kind));
    }
    let bad = "macro invalid { tokens }";
    let parsed = parser::parse_surface_with_limits(bad, ParseLimits::default()).unwrap();
    assert!(!parsed.diagnostics.is_empty());
    assert_eq!(parsed.diagnostics[0].span, 0..5);
}

#[test]
fn bracket_heads_keep_token_trees_and_indexed_header_expressions() {
    let root = surface("fn f() { configure regs[.flags: 4] { go() }\nguard table[i] > 0; }");
    assert!(
        root.descendants()
            .any(|node| node.kind() == parser::SUGAR_GROUP && node.text() == "[.flags: 4]")
    );
    assert!(
        !root
            .descendants()
            .any(|node| node.kind() == parser::SUGAR_GROUP && node.text() == "[i]")
    );
}

#[test]
fn expression_macros_and_sugar_share_original_source_ranges() {
    let source = "sugar inc: infix(`.+`, additive, left) { rule ($a:expr .+ $b:expr) => { keep!($a) } }\nfn f() { keep!(value .+ delta) }\nmacro keep($x:expr) -> expr { quote { $x } }";
    let root = surface(source);
    let call = root
        .descendants()
        .find(|n| n.kind() == parser::SURFACE_MACRO_CALL)
        .unwrap();
    assert_eq!(call.text(), "keep!(value .+ delta)");
    let arg = call
        .children()
        .find(|n| n.kind() == parser::SURFACE_MACRO_ARG)
        .unwrap();
    assert_eq!(arg.text(), "value .+ delta");
    assert!(arg.descendants().any(|n| n.kind() == parser::DOTTED_EXPR));
    let decl = root
        .descendants()
        .find(|n| n.kind() == parser::SURFACE_MACRO_DECL)
        .unwrap();
    let span = decl.text_range();
    assert_eq!(
        &source[usize::from(span.start())..usize::from(span.end())],
        "macro keep($x:expr) -> expr { quote { $x } }"
    );
}
