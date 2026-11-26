//! Source formatting preserves the Stage 1 spelling before import resolution.
use vesper_fmt::{Limits, format_source};
use vesper_syntax::lexer::{LexLimits, TokenKind, sugar_lex_with_limits};

fn spellings(source: &str) -> Vec<(TokenKind, &str)> {
    let lexed = sugar_lex_with_limits(source, LexLimits::default()).unwrap();
    assert!(lexed.diagnostics.is_empty(), "{:?}", lexed.diagnostics);
    lexed
        .tokens
        .into_iter()
        .filter(|token| !matches!(token.kind, TokenKind::Whitespace | TokenKind::Eof))
        .map(|token| (token.kind, &source[token.span]))
        .collect()
}

fn formatted(source: &str) -> String {
    let result = format_source(source, Limits::default())
        .unwrap_or_else(|err| panic!("formatting failed: {err:?}\n{source}"));
    assert_eq!(spellings(&result.output), spellings(source));
    let second = format_source(&result.output, Limits::default()).unwrap();
    assert_eq!(second.output, result.output);
    assert!(!second.changed);
    result.output
}

#[test]
fn declarations_preserve_holes_repetitions_and_operator_symbols() {
    let source = r#"pub sugar defer:stmt{rule(defer $cleanup:block $..rest:scope)=>{{$..rest;$cleanup;}}}
@lean_notation
pub sugar vadd:infix(`.+`,additive,left){rule($a:expr .+ $b:expr)=>{add($a,$b)}}
pub sugar collect:expr emits eff [Alloc]{rule(collect $($x:expr),+)=>{make()}}"#;
    let out = formatted(source);
    for spelling in [
        "$cleanup:block",
        "$..rest:scope",
        "`.+`",
        "$a:expr",
        "$b:expr",
    ] {
        assert!(out.contains(spelling), "{out}");
    }
}

#[test]
fn tag_adjacency_and_clause_line_boundaries_survive_formatting() {
    let out = formatted(
        "fn f(){let bytes=hex\"dead beef\";let header=bits{ver:u4=4};bits /* spaced */ {run()}\n\
         with_txn tx on inout db {run()} on_error(e){recover(e)}\n\
         retry {again()}\nnext {done()}}",
    );
    for spelling in [
        "hex\"dead beef\"",
        "bits{",
        "bits /* spaced */",
        "} on_error(e)",
        "\n    next {",
    ] {
        assert!(out.contains(spelling), "{out}");
    }
}

#[test]
fn unexpanded_slots_survive_without_declaring_or_importing_the_sugar() {
    for source in [
        "test \"empty frame\" { expect(ok) }",
        "fn f(){send (AppendReq { entry }) on sink ch;recv ack from sink ch;}",
        "fn f(){let output=frame |> parse(_, in cfg) |?> validate();output}",
        "fn f(){let next=pos .+ vel .* dt_splat; next}",
        "fn f(){ghost calc { value }; modify_reg inout uart.cr { .txe = 1, .rxe = 1 };}",
        "fn f(){while lo < hi\ninvariant lo <= hi\ndecreases hi - lo\n{step()}}",
    ] {
        formatted(source);
    }
}

#[test]
fn malformed_sugar_and_invalid_core_are_rejected() {
    for source in [
        "sugar guard:stmt {}",
        "sugar guard:stmt { rule () => { x } }",
        "sugar guard:stmt { rule (guard $x:bogus) => { x } }",
        "sugar guard:stmt { rule (guard $..x:expr) => { x } }",
        "sugar guard:expr emits [Alloc] { rule (guard) => { x } }",
        "sugar add:infix(`.+`,unknown,left) {rule (go) => { x }}",
        "fn f() { send req on sink ; }",
        "fn f() { guard let p = ; }",
        "fn f() { let x = frame |> ; }",
        "fn f() { let x = (a .+ ); }",
        "fn f() { modify_reg inout uart.cr { .txe = (1] }; }",
        "fn f() { let broken = ; send req on sink ch; }",
    ] {
        assert!(
            format_source(source, Limits::default()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn nested_token_groups_preserve_tags_and_statement_boundaries() {
    let output = formatted(
        r#"fn f(){holding x = open() {let s=sum{v|v in xs};let t=log"x";par { a() } /* keep */ and { b() }
retry { again() }}}"#,
    );
    for spelling in [
        "sum{",
        "log\"x\"",
        "} /* keep */ and {",
        "\n        retry {",
    ] {
        assert!(output.contains(spelling), "{output}");
    }
}

#[test]
fn test_only_items_receive_the_same_sugar_layout() {
    let output = formatted("test { fn f(){let x=tag{v:1};x |> process(_)} }");
    assert!(output.contains("tag{"), "{output}");
    assert!(output.contains("x |> process(_)"), "{output}");
}

#[test]
fn mixed_expression_macros_keep_their_profile_and_unexpanded_sugar_arguments() {
    let declarations =
        "macro keep($x:expr)->expr{quote{$x}}\nsugar twice:tag{rule(twice{$x:expr})=>{keep!($x)}}";
    let source =
        format!("{declarations}\nfn main(){{let first=keep!(bytes\"a\");keep!(first .+ next)}}");
    let out = formatted(&source);
    for expected in ["keep!(bytes\"a\")", "keep!(first .+ next)", "keep!($x)"] {
        assert!(out.contains(expected), "{out}");
    }
    for expr in [
        "unknown!(bytes\"a\")",
        "keep!(x,y)",
        "keep!(keep!(x))",
        "keep!(x,)",
        "keep!{x}",
        "keep!(1+2)",
    ] {
        let source = format!("{declarations}\nfn main(){{{expr}}}");
        assert!(
            format_source(&source, Limits::default()).is_err(),
            "{source}"
        );
    }
    let inactive = "macro keep($x:expr)->expr{quote{$x}} test {
        sugar thing:tag{rule(thing{$x:expr})=>{$x}} fn bad(){keep!()} }";
    assert!(format_source(inactive, Limits::default()).is_err());
    for invalid in [
        "macro keep($x:pat)->expr{quote{$x}}",
        "macro keep($x:expr)->pat{quote{$x}}",
        "macro keep($x:expr)->expr{quote{$wrong}}",
        "macro keep($x:expr)->expr{quote{keep!($x)}}",
        "macro keep($x:expr)->expr{quote{$x .+ x}}",
    ] {
        let source = format!("{invalid}\nsugar thing:tag{{rule(thing{{$x:expr}})=>{{$x}}}}");
        assert!(
            format_source(&source, Limits::default()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn macros_inside_sugar_templates_validate_names_arity_and_spliced_arguments() {
    let prefix = "macro keep($x:expr)->expr{quote{$x}}\nsugar wrap:expr{rule(wrap $..xs:args)=>{";
    let dynamic = format!("{prefix}keep!($..xs)}}}}");
    assert!(formatted(&dynamic).contains("keep!($..xs)"));
    for invalid in [
        "unknown!($..xs)",
        "keep!()",
        "keep!(1,2)",
        "keep!(keep!(1))",
        "keep!{opaque}",
    ] {
        let source = format!("{prefix}{invalid}}}}}");
        assert!(
            format_source(&source, Limits::default()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn expression_macro_token_budget_applies_only_to_real_macros() {
    let raw = format!(
        "sugar words:tag{{rule(words{{$x:expr}})=>{{macro;quote;{}}}}}",
        "x; ".repeat(3000)
    );
    assert!(format_source(&raw, Limits::default()).is_ok());
    let mixed = format!("macro keep($x:expr)->expr{{quote{{$x}}}}\n{raw}");
    assert_eq!(
        format_source(&mixed, Limits::default()).unwrap_err(),
        vesper_fmt::Error::TokenLimit
    );
}

#[test]
fn match_pattern_sugar_roundtrips_without_expansion() {
    let source = "sugar present:pat{rule(present $p:pat)=>{Option::Some($p)}} fn f(){match x{present value=>value,_=>0}}";
    let out = formatted(source);
    assert!(out.contains("present value => value"), "{out}");
    assert!(out.contains("$p:pat"), "{out}");
}

#[test]
fn tagged_calls_stay_on_one_line_and_hug_their_block() {
    // Short calls stay on one line, spaced like struct literals.
    assert_eq!(
        formatted("fn f(){sum{\n{\nn\n}\n| n in data\n}}"),
        "fn f() {\n    sum{ { n } | n in data }\n}\n"
    );
    assert_eq!(
        formatted("fn f(){let p=zip{\na,\nb\n};p}"),
        "fn f() {\n    let p = zip{ a, b };\n    p\n}\n"
    );
    // A block body breaks first; the clause follows its `}` and the call
    // adds no indentation of its own.
    assert_eq!(
        formatted(
            "fn f(){sum{{visits=wrapping_add(visits,1);x*narrow_i64(weight)}\n\
             |(x,weight) in zip{inputs.coefficients,inputs.weights}}}"
        ),
        "fn f() {\n    sum{ {\n        visits = wrapping_add(visits, 1);\n        \
         x * narrow_i64(weight)\n    } | (x, weight) in zip{ inputs.coefficients, \
         inputs.weights } }\n}\n"
    );
    assert_eq!(
        formatted(
            "fn f()->bool{let found=any{{mark(inout visited, x)==2}|x in \
             mark(inout visited,1)..mark(inout visited,4)};found}"
        ),
        "fn f() -> bool {\n    let found = any{ {\n        mark(inout visited, x) == 2\n    \
         } | x in mark(inout visited, 1)..mark(inout visited, 4) };\n    found\n}\n"
    );
    // A `:` after the block stays glued to its `}`.
    assert_eq!(
        formatted("fn f(){max{{x}:i64|x in xs}}"),
        "fn f() {\n    max{ { x }: i64 | x in xs }\n}\n"
    );
    // A comment or a top-level `;` keeps the block layout.
    let commented = formatted("fn f(){sum{ { n } // why\n| n in data }}");
    assert!(commented.contains("// why\n"), "{commented}");
}

#[test]
fn a_sugar_template_else_on_its_own_line_keeps_the_if_vertical() {
    // The template keeps `else` on its own line, so the `if` is not one
    // group (this used to panic).
    assert_eq!(
        formatted("sugar choose:expr{rule(choose $b:block)=>{{let x=if true{1}\nelse{2};x}}}"),
        "sugar choose: expr {
    rule (choose $b:block) => {
        {
            let x = if true {
                1
            }
            else {
                2
            };
            x
        }
    }
}
"
    );
}

#[test]
fn a_comprehension_pipe_is_neither_a_closure_nor_a_bitwise_or() {
    assert_eq!(
        formatted("fn f(in d:Slice[i64])->i64{sum{ { n } | n in d }}"),
        "fn f(in d: Slice[i64]) -> i64 {\n    sum{ { n } | n in d }\n}\n"
    );
}
