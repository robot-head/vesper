use vesper_fmt::{Error, Limits, recovery::format_recovery_source};
use vesper_syntax::{
    lexer::{self, TokenKind as K},
    parser, recovery,
};

fn tokens(source: &str) -> Vec<(K, String)> {
    let (tokens, diagnostics) = lexer::lex(source);
    assert!(diagnostics.is_empty());
    tokens
        .into_iter()
        .filter(|t| !matches!(t.kind, K::Whitespace | K::Eof))
        .map(|t| (t.kind, source[t.span].to_owned()))
        .collect()
}
fn verify(source: &str) -> String {
    let result = format_recovery_source(source, Limits::default())
        .unwrap_or_else(|e| panic!("{e}: {source}"));
    assert_eq!(tokens(source), tokens(result.output()));
    let snapshot = parser::parse_recovery(source, recovery::Limits::default()).unwrap();
    assert_eq!(result.source_hash(), snapshot.source_hash());
    let mut built = String::new();
    let mut cursor = 0;
    for edit in result.edits() {
        let span = edit.span();
        assert!(span.start >= cursor);
        let owner = snapshot
            .declarations()
            .iter()
            .find(|d| d.span == span)
            .unwrap();
        assert_eq!(owner.kind, recovery::DeclarationKind::Function);
        assert_eq!(owner.status, recovery::DeclarationStatus::Complete);
        built.push_str(&source[cursor..span.start]);
        built.push_str(edit.replacement());
        cursor = span.end;
    }
    built.push_str(&source[cursor..]);
    assert_eq!(built, result.output());
    assert_eq!(result.changed(), source != result.output());
    let again = format_recovery_source(result.output(), Limits::default()).unwrap();
    assert_eq!(again.output(), result.output());
    assert!(!again.changed());
    assert!(again.edits().is_empty());
    assert_eq!(
        again.formatted_declarations(),
        result.formatted_declarations()
    );
    result.into_output()
}
#[test]
fn complete_siblings_format_and_damaged_function_stays_exact() {
    let source =
        "\nfn before( )->i64{41}\n\nfn broken()->i64{let x=;}\n\tfn after( )->i64{42}  \r\n";
    let output = verify(source);
    assert!(output.starts_with("\nfn before() -> i64 {\n    41\n}\n\n"));
    assert!(output.contains("fn broken()->i64{let x=;}\n\t"));
    assert!(output.ends_with("fn after() -> i64 {\n    42\n}  \r\n"));
    assert!(!parser::parse(&output).diagnostics.is_empty());
}
#[test]
fn original_crlf_gaps_and_comment_string_bytes_remain_exact() {
    let source = "// 🦀 comment\r\n\r\nfn first( )->i64{/*é*/let text=\"fn fake(){🦀}\";42}\r\n\t//outside\r\nfn bad(){x.}\r\n";
    let output = verify(source);
    assert!(output.starts_with("// 🦀 comment\r\n\r\n"));
    assert!(output.contains("/*é*/"));
    assert!(output.contains("\"fn fake(){🦀}\""));
    assert!(output.ends_with("\r\n\t//outside\r\nfn bad(){x.}\r\n"));
}
#[test]
fn opaque_macro_bodies_and_fake_declarations_are_untouched() {
    let source = "m!{ fn fake( )->i64{0} /* raw   payload */ }\nfn broken(){unknown!{ fn nested( )->i64{0} }}\nfn real( )->i64{42}";
    let output = verify(source);
    assert!(output.starts_with("m!{ fn fake( )->i64{0} /* raw   payload */ }\nfn broken(){unknown!{ fn nested( )->i64{0} }}\n"));
    assert!(output.ends_with("fn real() -> i64 {\n    42\n}"));
}
#[test]
fn zero_eligible_is_unchanged_and_canonical_sibling_still_counts() {
    for source in ["", "fn bad(){x.}", "opaque!{fn fake( ){ 1 }}"] {
        let result = format_recovery_source(source, Limits::default()).unwrap();
        assert_eq!(result.formatted_declarations(), 0);
        assert_eq!(result.output(), source);
        assert!(!result.changed());
        assert!(result.edits().is_empty());
    }
    let source = "fn good() -> i64 {\n    42\n}\nfn bad(){x.}";
    let result = format_recovery_source(source, Limits::default()).unwrap();
    assert_eq!(result.formatted_declarations(), 1);
    assert_eq!(result.output(), source);
    assert!(!result.changed());
}
#[test]
fn lexical_uncertainty_rejects_without_a_proposal() {
    for source in [
        "fn good(){42}\n/*unterminated",
        "fn good(){42}\n\"unterminated",
        "fn good(){42}\n🦀",
    ] {
        assert!(
            format_recovery_source(source, Limits::default()).is_err(),
            "{source}"
        );
    }
}
#[test]
fn failed_limits_never_return_partial_edits() {
    let source = "fn good( )->i64{42}\nfn bad(){x.}";
    let cases = [
        (
            Limits {
                max_input_bytes: 3,
                ..Limits::default()
            },
            Error::InputLimit,
        ),
        (
            Limits {
                max_output_bytes: 3,
                ..Limits::default()
            },
            Error::OutputLimit,
        ),
        (
            Limits {
                max_work: 1,
                ..Limits::default()
            },
            Error::WorkLimit,
        ),
        (
            Limits {
                max_tokens: 1,
                ..Limits::default()
            },
            Error::TokenLimit,
        ),
        (
            Limits {
                max_doc_nodes: 0,
                ..Limits::default()
            },
            Error::NodeLimit,
        ),
        (
            Limits {
                max_depth: 0,
                ..Limits::default()
            },
            Error::DepthLimit,
        ),
    ];
    for (limits, expected) in cases {
        assert_eq!(
            format_recovery_source(source, limits).unwrap_err(),
            expected
        );
    }
}
#[test]
fn one_budget_covers_many_fragments_and_the_second_pass() {
    let source = (0..20)
        .map(|n| format!("fn f{n}( )->i64{{42}}\n"))
        .collect::<String>()
        + "fn bad(){x.}";
    let result = format_recovery_source(&source, Limits::default()).unwrap();
    assert_eq!(result.formatted_declarations(), 20);
    assert_eq!(result.edits().len(), 20);
    let small = Limits {
        max_work: 24 * 1024 * 1024,
        ..Limits::default()
    };
    assert_eq!(
        format_recovery_source(&source, small).unwrap_err(),
        Error::WorkLimit
    );
    let lsp = Limits {
        max_work: 64 * 1024 * 1024,
        ..Limits::default()
    };
    assert!(format_recovery_source(&source, lsp).is_ok());
}
#[test]
fn semantic_errors_are_not_repaired_by_syntax_formatting() {
    let source = "linear struct Tx{}\nfn leaks()->i64{let tx=Tx{};42}\nfn bad(){x.}";
    let output = verify(source);
    assert!(output.contains("let tx = Tx {}"));
    assert!(output.contains("fn bad(){x.}"));
    // The complete function remains a real syntax declaration with its unused
    // linear binding; this API never manufactures semantic diagnostics/approval.
    assert!(tokens(&output).iter().any(|(_, text)| text == "tx"));
}

#[test]
fn actual_utf8_prefixes_never_rewrite_opaque_or_damaged_gaps() {
    let sample = "//🦀\r\nfn good( )->i64{42}\r\nfn bad(){let s=\"é\"; unknown!{fn fake( ){3}};x.}";
    for end in sample
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(sample.len()))
    {
        let source = &sample[..end];
        if let Ok(result) = format_recovery_source(source, Limits::default()) {
            assert_eq!(tokens(source), tokens(result.output()));
            let mut cursor = 0;
            let mut proposed = String::new();
            for edit in result.edits() {
                let span = edit.span();
                assert!(source.is_char_boundary(span.start));
                assert!(source.is_char_boundary(span.end));
                proposed.push_str(&source[cursor..span.start]);
                proposed.push_str(edit.replacement());
                cursor = span.end;
            }
            proposed.push_str(&source[cursor..]);
            assert_eq!(proposed, result.output());
            let again = format_recovery_source(result.output(), Limits::default()).unwrap();
            assert_eq!(result.output(), again.output());
            assert!(!again.changed());
        }
    }
}
