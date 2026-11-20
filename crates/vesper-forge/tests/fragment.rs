//! The lexical fragment's reserved words and delimiter rules, on host
//! source tokens (src/syntax.rs, `check_delimiters` in src/host.rs).

mod common;

use common::*;
use vesper_forge::*;

fn resolve(src: &str) -> Result<ResolvedFragment, ForgeError> {
    let mut h = host();
    let toks = h.lex_source(APP, src)?;
    h.resolve(toks)
}

#[test]
fn reserved_words_are_not_identifiers() {
    for (src, token) in [
        ("let with = 1; with", 1),
        ("let handle = 1; handle", 1),
        ("let let = 1; print", 1),
        ("print(with)", 2),
        ("handle with with print { 1 }", 1),
    ] {
        assert_eq!(
            resolve(src).unwrap_err(),
            ForgeError::Parse {
                token,
                expected: "identifier"
            },
            "{src}"
        );
    }
}

#[test]
fn delimiters_must_match_and_nest() {
    for (src, token) in [
        ("print(1]", 3),
        ("{ print )", 2),
        ("print(1", 3),
        ("print)", 1),
    ] {
        assert_eq!(
            resolve(src).unwrap_err(),
            ForgeError::UnbalancedDelimiters { token },
            "{src}"
        );
    }
    let deep = format!("{}1{}", "(".repeat(200), ")".repeat(200));
    assert_eq!(
        resolve(&deep).unwrap_err(),
        ForgeError::QuoteLimit {
            what: "nesting depth",
            limit: 128
        }
    );
}

#[test]
fn well_formed_fragments_resolve() {
    let f = resolve("let x = print; { x(1, \"s\"); (hash_into)(x) }").unwrap();
    assert_eq!(f.binders.len(), 1);
    assert_eq!(f.resolution_at(3), Some(Resolution::Global(APP_PRINT)));
    // `print`, the aliased call `x(..)` and the parenthesized callee.
    let kinds: Vec<_> = f.operations.iter().map(|o| o.kind).collect();
    assert_eq!(
        kinds,
        [
            OperationKind::Use { def: APP_PRINT },
            OperationKind::Use { def: APP_PRINT },
            OperationKind::Use { def: HASH_INTO },
        ]
    );
}
