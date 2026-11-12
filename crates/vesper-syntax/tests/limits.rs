use vesper_syntax::{
    lexer::{self, LexLimitKind, LexLimits, TokenKind},
    parser::{self, ParseLimits},
};

fn retained(source: &str, limits: ParseLimits) -> parser::Parse {
    let parsed = parser::parse_with_limits(source, limits).unwrap();
    assert_eq!(parsed.syntax().text().to_string(), source);
    for token in parsed
        .syntax()
        .descendants_with_tokens()
        .filter_map(|node| node.into_token())
    {
        let range = token.text_range();
        assert_eq!(
            token.text(),
            &source[usize::from(range.start())..usize::from(range.end())]
        );
    }
    parsed
}
fn limited(source: &str) {
    let parsed = retained(source, ParseLimits::default());
    assert!(parsed.module.items.is_empty());
    assert!(
        parsed.diagnostics.iter().any(|d| d.code == "E0003"),
        "{:?}",
        parsed.diagnostics
    );
    assert!(parsed.diagnostics.len() <= ParseLimits::default().max_diagnostics);
}
#[test]
fn hostile_non_delimiter_recursive_shapes_fail_losslessly() {
    let count = 4000;
    for source in [
        format!("fn f(){{ {}true }}", "!".repeat(count)),
        format!("fn f(){{ {}1 }}", "a=".repeat(count)),
        format!("fn f(){{ {}{{}} }}", "if true {{}} else ".repeat(count)),
        format!("fn f(x: {}u64){{}}", "fn()->".repeat(count)),
        format!("fn f(x.{}last: u64){{}}", "a.".repeat(count)),
        format!(
            "fn f(){{let {}x{}=1;}}",
            "Some(".repeat(count),
            ")".repeat(count)
        ),
        format!(
            "fn f(){{let {}x{}=1;}}",
            "(".repeat(count),
            ")".repeat(count)
        ),
        format!(
            "fn f(x: {}u64{}){{}}",
            "Box[".repeat(count),
            "]".repeat(count)
        ),
    ] {
        limited(&source);
    }
}
#[test]
fn iterative_and_composed_ast_chains_are_bounded_too() {
    for expression in [
        format!("x{}", ".a".repeat(8000)),
        format!("1{}", "+1".repeat(8000)),
        format!("x{}", "?".repeat(8000)),
    ] {
        limited(&format!("fn f(){{{expression}}}"));
    }
    // Each individual chain is short; composing grouped postfix chains must not
    // hide height from the iterative-parser guard or cause recursive Drop overflow.
    let mut expression = "x".to_owned();
    for _ in 0..16 {
        expression = format!("({expression}){}", ".a".repeat(12));
    }
    limited(&format!("fn f(){{{expression}}}"));
}
#[test]
fn lexer_token_exact_boundary_and_opaque_tail_preserve_comments_and_utf8() {
    let source = "fn f() { /* 🦀 */ 1 } //tail\n";
    let original = lexer::lex_with_limits(source, LexLimits::default()).unwrap();
    assert!(original.limit.is_none());
    let count = original.tokens.len() - 1;
    let exact = lexer::lex_with_limits(
        source,
        LexLimits {
            max_tokens: count,
            ..LexLimits::default()
        },
    )
    .unwrap();
    assert!(exact.limit.is_none());
    let limited = lexer::lex_with_limits(
        source,
        LexLimits {
            max_tokens: count - 1,
            ..LexLimits::default()
        },
    )
    .unwrap();
    assert_eq!(limited.limit.unwrap().kind, LexLimitKind::Tokens);
    assert!(limited.tokens.len() <= count);
    let text: String = limited
        .tokens
        .iter()
        .filter(|t| t.kind != TokenKind::Eof)
        .map(|t| &source[t.span.clone()])
        .collect();
    assert_eq!(text, source);
    for token in limited.tokens {
        assert!(source.is_char_boundary(token.span.start));
        assert!(source.is_char_boundary(token.span.end));
    }
    let parsed = retained(
        source,
        ParseLimits {
            max_tokens: count - 1,
            ..ParseLimits::default()
        },
    );
    assert!(parsed.module.items.is_empty());
    assert!(parsed.diagnostics.iter().any(|d| d.code == "E0003"));
}
#[test]
fn diagnostic_storms_are_capped_and_limit_is_never_hidden() {
    for source in ["€ ".repeat(8000), "fn ".repeat(8000), "? ".repeat(8000)] {
        let lexed = lexer::lex_with_limits(
            &source,
            LexLimits {
                max_diagnostics: 3,
                ..LexLimits::default()
            },
        )
        .unwrap();
        assert!(lexed.diagnostics.len() <= 3);
        let parsed = retained(
            &source,
            ParseLimits {
                max_diagnostics: 3,
                ..ParseLimits::default()
            },
        );
        assert!(parsed.diagnostics.len() <= 3);
        assert!(parsed.diagnostics.iter().any(|d| d.code == "E0003"));
        assert!(parsed.module.items.is_empty());
    }
    let parsed = retained(
        "? ?",
        ParseLimits {
            max_diagnostics: 1,
            ..ParseLimits::default()
        },
    );
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].code, "E0003");
}
#[test]
fn byte_preflight_and_invalid_configurations_are_explicit() {
    let source = "fn f(){}";
    let exact = retained(
        source,
        ParseLimits {
            max_source_bytes: source.len(),
            ..ParseLimits::default()
        },
    );
    assert!(exact.diagnostics.is_empty());
    let error = parser::parse_with_limits(
        source,
        ParseLimits {
            max_source_bytes: source.len() - 1,
            ..ParseLimits::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.kind, LexLimitKind::SourceBytes);
    assert_eq!(error.retained_bytes, 0);
    for limits in [
        ParseLimits {
            max_depth: 0,
            ..ParseLimits::default()
        },
        ParseLimits {
            max_depth: parser::HARD_PARSE_DEPTH + 1,
            ..ParseLimits::default()
        },
        ParseLimits {
            max_tokens: 0,
            ..ParseLimits::default()
        },
        ParseLimits {
            max_diagnostics: 0,
            ..ParseLimits::default()
        },
        ParseLimits {
            max_source_bytes: lexer::HARD_SOURCE_BYTES + 1,
            ..ParseLimits::default()
        },
        ParseLimits {
            max_tokens: lexer::HARD_TOKENS + 1,
            ..ParseLimits::default()
        },
        ParseLimits {
            max_diagnostics: lexer::HARD_DIAGNOSTICS + 1,
            ..ParseLimits::default()
        },
    ] {
        assert_eq!(
            parser::parse_with_limits(source, limits).unwrap_err().kind,
            LexLimitKind::Configuration
        );
    }
    let text = "🦀".repeat(LexLimits::default().max_source_bytes / 4 + 1);
    assert_eq!(
        lexer::lex_with_limits(&text, LexLimits::default())
            .unwrap_err()
            .kind,
        LexLimitKind::SourceBytes
    );
    let (tokens, diagnostics) = lexer::lex(&text);
    assert_eq!(diagnostics[0].code, "E0003");
    assert!(tokens.last().unwrap().span.end <= LexLimits::default().max_source_bytes);
    let parsed = parser::parse(&text);
    assert!(parsed.module.items.is_empty());
    assert_eq!(parsed.diagnostics[0].code, "E0003");
    let actual = parsed.syntax().text().to_string();
    assert_eq!(actual, &text[..actual.len()]);
    assert!(actual.len() <= LexLimits::default().max_source_bytes);
}
#[test]
fn configured_descent_boundaries_and_balanced_recovery_are_deterministic() {
    let source = "fn f(){!true}";
    let mut first = None;
    for depth in 1..=32 {
        let parsed = retained(
            source,
            ParseLimits {
                max_depth: depth,
                ..ParseLimits::default()
            },
        );
        if parsed.diagnostics.is_empty() {
            first = Some(depth);
            break;
        } else {
            assert!(parsed.diagnostics.iter().any(|d| d.code == "E0003"));
        }
    }
    let first = first.unwrap();
    assert!(first > 1);
    assert!(
        retained(
            source,
            ParseLimits {
                max_depth: first,
                ..ParseLimits::default()
            }
        )
        .diagnostics
        .is_empty()
    );
    assert!(
        retained(
            source,
            ParseLimits {
                max_depth: first - 1,
                ..ParseLimits::default()
            }
        )
        .module
        .items
        .is_empty()
    );
    for depth in 1..=16 {
        for source in [
            "fn f(x: fn()->fn()->u64){!true}",
            "fn f(x.a.b.c: u64){let Some((a,b))=x; if true {} else if false {} else {}}",
        ] {
            let _ = retained(
                source,
                ParseLimits {
                    max_depth: depth,
                    max_diagnostics: 2,
                    ..ParseLimits::default()
                },
            );
        }
    }
}
#[test]
fn ordinary_ast_cst_spans_and_comments_are_unchanged_by_explicit_defaults() {
    let corpus = [
        "// heading\ncopy struct Pair[T: mod copy] { a:T, b:T }\nfn add(a:i64,b:i64)->i64 { a+b*2 } // end\n",
        "fn main(){foo[u64](1); arr[idx](2); let Some((x,y)) = value; match x { 0=>1, _=>2 }}",
        "fn f(inout broker.{index,metrics.counters}: Broker){if true {1} else if false {2} else {3}}",
        "effect Fail[E] { fn raise(err:E)->Never; } fn f(x:fn(in u64)->u64)->u64 {x(1)}",
        "trait Show[T: mod copy] { fn show(in self)->u64; } impl Show for Item { fn show(in self)->u64 {1} }",
        "fn f(){ /* nested /* 🦀 */ comment */ let text=\"hello\\nworld\"; text }",
    ];
    for source in corpus {
        let default = parser::parse(source);
        let bounded = retained(source, ParseLimits::default());
        assert_eq!(default, bounded);
        assert!(bounded.diagnostics.is_empty(), "{:?}", bounded.diagnostics);
    }
}
#[test]
fn deterministic_invalid_corpus_remains_lossless_with_bounded_diagnostics() {
    let alphabet = b"fn let {}()[],;:@|?+-*/abcXYZ019\n\t";
    let mut state = 123456789_u64;
    for _ in 0..1000 {
        let mut source = String::new();
        for _ in 0..80 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            source.push(alphabet[state as usize % alphabet.len()] as char);
        }
        let parsed = retained(
            &source,
            ParseLimits {
                max_diagnostics: 8,
                ..ParseLimits::default()
            },
        );
        assert!(parsed.diagnostics.len() <= 8);
    }
}

#[test]
fn maximum_supported_depth_handles_real_hostile_inputs_and_balanced_unwind() {
    for source in [
        format!("fn f(){{ {}true }}", "!".repeat(4000)),
        format!("fn f(x: {}u64){{}}", "fn()->".repeat(4000)),
        format!(
            "fn f(){{let {}x{}=1;}}",
            "Some(".repeat(4000),
            ")".repeat(4000)
        ),
    ] {
        let parsed = retained(
            &source,
            ParseLimits {
                max_depth: parser::HARD_PARSE_DEPTH,
                ..ParseLimits::default()
            },
        );
        assert!(parsed.module.items.is_empty());
        assert!(parsed.diagnostics.iter().any(|d| d.code == "E0003"));
    }
}
