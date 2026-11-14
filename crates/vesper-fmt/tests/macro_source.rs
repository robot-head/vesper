use vesper_fmt::{Error, Limits, format_source};
use vesper_syntax::{
    lexer::{TokenKind, lex},
    macro_source,
};

fn evidence(source: &str) -> Vec<(TokenKind, String)> {
    let (tokens, errors) = lex(source);
    assert!(errors.is_empty());
    tokens
        .into_iter()
        .filter(|token| !matches!(token.kind, TokenKind::Whitespace | TokenKind::Eof))
        .map(|token| (token.kind, source[token.span].to_owned()))
        .collect()
}

fn verify(source: &str) -> String {
    let result = format_source(source, Limits::default())
        .unwrap_or_else(|error| panic!("{error}: {source}"));
    macro_source::parse(&result.output, macro_source::Limits::default()).unwrap();
    assert_eq!(evidence(source), evidence(&result.output));
    let second = format_source(&result.output, Limits::default()).unwrap();
    assert_eq!(second.output, result.output);
    assert!(!second.changed);
    result.output
}

#[test]
fn formats_actual_fixed_expr_source_and_preserves_invocation_slot_spelling() {
    let source = "macro keep($value:expr)->expr{quote{$value}}fn main()->i64{let n=42;keep!(n)}";
    let output = verify(source);
    assert!(output.contains("macro keep($value: expr) -> expr {"));
    assert!(output.contains("keep!(n)"));
    assert!(output.contains("quote {\n        $value\n    }"));
}

#[test]
fn comments_strings_unicode_and_crlf_are_token_exact() {
    for source in [
        "// 🦀 é\r\nmacro keep(/*param*/$x:expr)->expr{quote{/*before*/$x/*after*/}}\r\nfn main()->i64{let n=42;// arg\r\nkeep!(n)}",
        "macro keep($x:expr)->expr{quote{$x}}fn main()->String{keep!(\"é 🦀 quote { fake!(x) }\\n\")}",
        "macro keep($x:expr)->expr{quote{ $x // quote end\r\n }}fn main()->i64{keep!(42)}// eof\r",
        "macro keep($x:expr)->expr{quote{/*a/*nested*/b*/$x}}fn main()->i64{keep!(42)}",
    ] {
        verify(source);
    }
}

#[test]
fn quoted_blocks_support_helpers_and_multiple_current_invocations() {
    for source in [
        "macro keep($x:expr)->expr{quote{{$x}}}fn main()->i64{keep!({let n=42;n})}",
        "macro both($x:expr,$y:expr)->expr{quote{wrapping_add($x,$y)}}fn main()->i64{both!(20,22)}",
        "macro keep($x:expr)->expr{quote{$x}}fn helper(in n:i64)->i64{n}fn first()->i64{keep!(42)}fn main()->i64{keep!(helper(42))}",
        "macro keep($x:expr)->expr{quote{{let ignored=1;$x}}}fn main()->i64{let n=42;keep!(n)}",
    ] {
        verify(source);
    }
}

#[test]
fn unknown_malformed_and_unsupported_payloads_remain_errors() {
    for source in [
        "fn main(){unknown!{ /* opaque */ token }}",
        "macro keep($x:pat)->expr{quote{$x}}fn main()->i64{keep!(42)}",
        "macro keep($x:expr)->expr{quote{$x}}fn main()->i64{keep!(keep!(42))}",
        "macro keep($x:expr)->expr{quote{$..x}}fn main()->i64{keep!(42)}",
        "macro keep($x:expr)->expr{quote{$unknown}}fn main()->i64{keep!(42)}",
        "macro keep($x:expr)->expr{quote{$x}}fn main()->i64{other!(42)}",
        "macro keep($x:expr)->expr{quote{$x}}fn main()->i64{keep!(42,1)}",
        "macro keep($x:expr)->expr{quote{$x}}fn main(){quasiquote { opaque }}",
        "macro keep($x:expr)->expr{quote{$x}}fn main()->i64{keep!(42)",
    ] {
        assert!(
            format_source(source, Limits::default()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn bare_quote_names_do_not_opt_ordinary_source_into_macro_profile() {
    for source in [
        "fn quote()->i64{42}",
        "fn main()->i64{let quote=42;quote}",
        "fn main()->i64{let quasiquote=42;quasiquote}",
    ] {
        assert_eq!(
            format_source(source, Limits::default()).unwrap_err(),
            Error::OpaqueMacro
        );
    }
    assert!(format_source("fn main()->i64{42}", Limits::default()).is_ok());
}

#[test]
fn macro_resource_guards_cover_shared_work_depth_tokens_and_output() {
    let source = "macro keep($x:expr)->expr{quote{$x}}fn main()->i64{keep!(42)}";
    for limits in [
        Limits {
            max_work: 1,
            ..Limits::default()
        },
        Limits {
            max_input_bytes: source.len() - 1,
            ..Limits::default()
        },
        Limits {
            max_output_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_tokens: 4,
            ..Limits::default()
        },
        Limits {
            max_depth: 1,
            ..Limits::default()
        },
        Limits {
            max_doc_nodes: 1,
            ..Limits::default()
        },
    ] {
        assert!(format_source(source, limits).is_err());
    }
}

#[test]
fn utf8_prefixes_never_publish_changed_token_evidence() {
    let source = "// é 🦀\nmacro keep($x:expr)->expr{quote{$x}}fn main()->i64{keep!(42)}";
    for end in (0..=source.len()).filter(|&end| source.is_char_boundary(end)) {
        let prefix = &source[..end];
        if let Ok(result) = format_source(prefix, Limits::default()) {
            assert_eq!(evidence(prefix), evidence(&result.output));
            assert_eq!(
                format_source(&result.output, Limits::default())
                    .unwrap()
                    .output,
                result.output
            );
        }
    }
}
