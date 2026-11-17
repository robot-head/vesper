use vesper_syntax::lexer::{TokenKind as K, lex};

#[test]
fn bitwise_spellings_are_lossless_and_use_longest_match() {
    let source = "x|=~y & z &= y || x | y == z // |= &= ~\n";
    let (tokens, diagnostics) = lex(source);
    // A single ampersand is the bitwise-and token; &= is still recognized
    // independently of it.
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let actual: Vec<_> = tokens
        .iter()
        .map(|t| (t.kind, &source[t.span.clone()]))
        .collect();
    assert_eq!(
        actual,
        vec![
            (K::Lower, "x"),
            (K::PipeEq, "|="),
            (K::Tilde, "~"),
            (K::Lower, "y"),
            (K::Whitespace, " "),
            (K::Amp, "&"),
            (K::Whitespace, " "),
            (K::Lower, "z"),
            (K::Whitespace, " "),
            (K::AmpEq, "&="),
            (K::Whitespace, " "),
            (K::Lower, "y"),
            (K::Whitespace, " "),
            (K::Or, "||"),
            (K::Whitespace, " "),
            (K::Lower, "x"),
            (K::Whitespace, " "),
            (K::Pipe, "|"),
            (K::Whitespace, " "),
            (K::Lower, "y"),
            (K::Whitespace, " "),
            (K::EqEq, "=="),
            (K::Whitespace, " "),
            (K::Lower, "z"),
            (K::Whitespace, " "),
            (K::Comment, "// |= &= ~"),
            (K::Whitespace, "\n"),
            (K::Eof, ""),
        ]
    );
    assert_eq!(
        tokens
            .iter()
            .map(|t| &source[t.span.clone()])
            .collect::<String>(),
        source
    );
}
