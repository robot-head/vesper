use vesper_syntax::lexer::{TokenKind as K, lex};

#[test]
fn range_punctuation_is_lossless_and_uses_longest_match() {
    let source = "a[0..mid] u64 is 1..=4096 1.25 1..2 ... ..== .";
    let (tokens, diagnostics) = lex(source);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let actual: Vec<_> = tokens
        .iter()
        .filter(|t| !t.kind.trivia() && t.kind != K::Eof)
        .map(|t| (t.kind, &source[t.span.clone()]))
        .collect();
    assert_eq!(
        actual,
        vec![
            (K::Lower, "a"),
            (K::LBracket, "["),
            (K::Int, "0"),
            (K::DotDot, ".."),
            (K::Lower, "mid"),
            (K::RBracket, "]"),
            (K::Lower, "u64"),
            (K::Lower, "is"),
            (K::Int, "1"),
            (K::DotDotEq, "..="),
            (K::Int, "4096"),
            (K::Float, "1.25"),
            (K::Int, "1"),
            (K::DotDot, ".."),
            (K::Int, "2"),
            (K::DotDot, ".."),
            (K::Dot, "."),
            (K::DotDotEq, "..="),
            (K::Eq, "="),
            (K::Dot, "."),
        ]
    );
    assert_eq!(
        tokens
            .iter()
            .map(|t| &source[t.span.clone()])
            .collect::<String>(),
        source
    );
    assert_eq!(K::DotDot as u16, K::Tilde as u16 + 1);
    assert_eq!(K::DotDotEq as u16, K::DotDot as u16 + 1);
}
