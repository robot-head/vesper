use super::*;
use crate::parser::{ITEM, MODULE};
use rowan::{GreenNodeBuilder, SyntaxKind};
fn raw(text: &str) -> GreenNode {
    let mut b = GreenNodeBuilder::new();
    b.start_node(MODULE);
    if !text.is_empty() {
        b.token(SyntaxKind(TokenKind::Error as u16), text);
    }
    b.finish_node();
    b.finish()
}
fn snapshot(text: &str) -> RecoverySyntax {
    assemble(
        Arc::from(text),
        raw(text),
        vec![],
        vec![],
        vec![],
        Limits::default(),
    )
    .unwrap()
}
fn function(complete: bool) -> (Arc<str>, GreenNode, Declaration) {
    function_kind(complete, BLOCK_KIND)
}
fn function_kind(complete: bool, body_kind: SyntaxKind) -> (Arc<str>, GreenNode, Declaration) {
    let source = if complete {
        "fn f(){x}\r\n"
    } else {
        "fn f(){x.}\r\n"
    };
    let mut b = GreenNodeBuilder::new();
    b.start_node(MODULE);
    b.start_node(ITEM);
    for (kind, text) in [
        (TokenKind::Fn, "fn"),
        (TokenKind::Whitespace, " "),
        (TokenKind::Lower, "f"),
        (TokenKind::LParen, "("),
        (TokenKind::RParen, ")"),
    ] {
        b.token(SyntaxKind(kind as u16), text);
    }
    b.start_node(body_kind);
    for (kind, text) in [(TokenKind::LBrace, "{"), (TokenKind::Lower, "x")] {
        b.token(SyntaxKind(kind as u16), text);
    }
    if !complete {
        b.token(SyntaxKind(TokenKind::Dot as u16), ".");
    }
    b.token(SyntaxKind(TokenKind::RBrace as u16), "}");
    b.finish_node();
    b.finish_node();
    b.token(SyntaxKind(TokenKind::Whitespace as u16), "\r\n");
    b.finish_node();
    let end = if complete { 9 } else { 10 };
    let decl = Declaration {
        kind: DeclarationKind::Function,
        name_span: Some(3..4),
        span: 0..end,
        header_span: 0..6,
        body_span: Some(6..end),
        body_green_span: Some(6..end),
        status: if complete {
            DeclarationStatus::Complete
        } else {
            DeclarationStatus::Recovered
        },
        body_context: complete.then(|| BlockContext {
            row_names: BTreeSet::new(),
            bound_values: BTreeSet::new(),
            no_struct: false,
        }),
    };
    (Arc::from(source), b.finish(), decl)
}
fn with_decl(d: Declaration) -> Result<RecoverySyntax, Error> {
    let (source, green, _) = function(true);
    assemble(source, green, vec![], vec![d], vec![], Limits::default())
}
#[test]
fn original_unicode_crlf_and_hash_are_exact_without_ast() {
    let text = "// 🦀 é\r\nfn 名\n";
    let source: Arc<str> = Arc::from(text);
    let before = Arc::clone(&source);
    let s = assemble(source, raw(text), vec![], vec![], vec![], Limits::default()).unwrap();
    assert_eq!(s.source(), text);
    assert!(Arc::ptr_eq(s.source_arc(), &before));
    assert_eq!(s.syntax().to_string(), text);
    assert_eq!(s.green().to_string(), text);
    assert_eq!(s.source_hash(), *blake3::hash(text.as_bytes()).as_bytes());
    assert_eq!(s.limits(), Limits::default());
    assert!(s.declarations().is_empty());
    assert!(s.sites().is_empty());
    assert_eq!(
        blake3::Hash::from_bytes(snapshot("").source_hash())
            .to_hex()
            .as_str(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
}
#[test]
fn actual_function_subtree_and_context_remain_syntax_metadata() {
    let (source, green, decl) = function(true);
    let expected = decl.clone();
    let s = assemble(source, green, vec![], vec![decl], vec![], Limits::default()).unwrap();
    assert_eq!(s.declarations(), &[expected]);
    let d = &s.declarations()[0];
    assert_eq!(s.source().get(d.name_span.clone().unwrap()), Some("f"));
    assert_eq!(
        s.source().get(d.body_green_span.clone().unwrap()),
        Some("{x}")
    );
    assert!(d.body_context.is_some());
    assert_eq!(d.status.as_str(), "complete");
}
#[test]
fn real_missing_site_is_zero_width_and_retains_receiver_tokens() {
    let (source, green, decl) = function(false);
    let site = RecoverySite {
        span: 9..9,
        kind: SiteKind::MissingMember,
        owner: Some(0),
        receiver: Some(Receiver {
            root: 7..8,
            projections: vec![],
        }),
    };
    let d = Diagnostic::new("E0001", "expected member", 9..9)
        .with_related(7..8, "receiver syntax")
        .with_help("Complete the member syntax.");
    let s = assemble(
        source,
        green,
        vec![d.clone()],
        vec![decl],
        vec![site.clone()],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(s.sites(), &[site]);
    assert_eq!(s.diagnostics(), &[d]);
    assert_eq!(s.source().get(s.sites()[0].span.clone()), Some(""));
    assert!(s.declarations()[0].body_context.is_none());
}
#[test]
fn source_and_cst_mismatch_never_publish() {
    for (source, tree) in [("a", raw("b")), ("", raw("x")), ("x", raw(""))] {
        assert_eq!(
            assemble(
                Arc::from(source),
                tree,
                vec![],
                vec![],
                vec![],
                Limits::default()
            )
            .unwrap_err(),
            Error::CstMismatch
        );
    }
}
#[test]
fn every_source_span_rejects_unicode_crlf_reversal_and_overflow() {
    let source = "a🦀\r\nz";
    for span in [2..5, 6..7, 0..usize::MAX, Span { start: 4, end: 3 }] {
        let diag = Diagnostic::new("E", "range", span.clone());
        assert_eq!(
            assemble(
                Arc::from(source),
                raw(source),
                vec![diag],
                vec![],
                vec![],
                Limits::default()
            )
            .unwrap_err(),
            Error::InvalidSpan
        );
        let diag = Diagnostic::new("E", "range", 0..1).with_related(span.clone(), "secondary");
        assert_eq!(
            assemble(
                Arc::from(source),
                raw(source),
                vec![diag],
                vec![],
                vec![],
                Limits::default()
            )
            .unwrap_err(),
            Error::InvalidSpan
        );
        let site = RecoverySite {
            span,
            kind: SiteKind::Unexpected,
            owner: None,
            receiver: None,
        };
        assert_eq!(
            assemble(
                Arc::from(source),
                raw(source),
                vec![],
                vec![],
                vec![site],
                Limits::default()
            )
            .unwrap_err(),
            Error::InvalidSpan
        );
    }
}
#[test]
fn owner_can_precede_next_anchor_but_cannot_be_foreign() {
    let (source, green, decl) = function(false);
    let end = source.len();
    let site = RecoverySite {
        span: end..end,
        kind: SiteKind::MissingDelimiter,
        owner: Some(0),
        receiver: None,
    };
    assert!(
        assemble(
            Arc::clone(&source),
            green.clone(),
            vec![],
            vec![decl.clone()],
            vec![site.clone()],
            Limits::default()
        )
        .is_ok()
    );
    let mut invalid = site.clone();
    invalid.owner = Some(1);
    assert_eq!(
        assemble(
            Arc::clone(&source),
            green.clone(),
            vec![],
            vec![decl.clone()],
            vec![invalid],
            Limits::default()
        )
        .unwrap_err(),
        Error::InvalidInventory
    );
    let mut invalid = site;
    invalid.span = 7..8;
    assert_eq!(
        assemble(
            source,
            green,
            vec![],
            vec![decl],
            vec![invalid],
            Limits::default()
        )
        .unwrap_err(),
        Error::InvalidInventory
    );
}
#[test]
fn descriptor_containment_order_name_and_actual_body_node_are_checked() {
    let (_, _, good) = function(true);
    let mut d = good.clone();
    d.name_span = Some(0..2);
    assert_eq!(with_decl(d).unwrap_err(), Error::InvalidInventory);
    let mut d = good.clone();
    d.header_span = 0..11;
    assert_eq!(with_decl(d).unwrap_err(), Error::InvalidInventory);
    let mut d = good.clone();
    d.body_green_span = Some(7..8);
    assert_eq!(with_decl(d).unwrap_err(), Error::InvalidInventory);
    let mut d = good.clone();
    d.body_span = Some(7..8);
    d.body_green_span = Some(7..8);
    assert_eq!(with_decl(d).unwrap_err(), Error::InvalidInventory);
    let mut d = good.clone();
    d.status = DeclarationStatus::Recovered;
    assert_eq!(with_decl(d).unwrap_err(), Error::InvalidInventory);
    let (source, green, _) = function(true);
    assert_eq!(
        assemble(
            source,
            green,
            vec![],
            vec![good.clone(), good],
            vec![],
            Limits::default()
        )
        .unwrap_err(),
        Error::InvalidInventory
    );
}
#[test]
fn receiver_requires_actual_root_and_projection_tokens_in_order() {
    let (source, green, decl) = function(false);
    for receiver in [
        Receiver {
            root: 8..9,
            projections: vec![],
        },
        Receiver {
            root: 7..8,
            projections: std::iter::once(3..4).collect(),
        },
        Receiver {
            root: 7..8,
            projections: std::iter::once(8..9).collect(),
        },
    ] {
        let site = RecoverySite {
            span: 9..9,
            kind: SiteKind::MissingMember,
            owner: Some(0),
            receiver: Some(receiver),
        };
        assert_eq!(
            assemble(
                Arc::clone(&source),
                green.clone(),
                vec![],
                vec![decl.clone()],
                vec![site],
                Limits::default()
            )
            .unwrap_err(),
            Error::InvalidInventory
        );
    }
}
#[test]
fn direct_configuration_and_source_preflight_are_finite() {
    let defaults = Limits::default();
    assert_eq!(defaults.parse.max_source_bytes, 128 * 1024);
    assert_eq!(defaults.parse.max_depth, 48);
    assert!(Limits::HARD.validate().is_ok());
    for invalid in [
        Limits {
            max_work: usize::MAX,
            ..defaults
        },
        Limits {
            max_sites: 4097,
            ..defaults
        },
        Limits {
            parse: ParseLimits {
                max_depth: 0,
                ..defaults.parse
            },
            ..defaults
        },
    ] {
        assert_eq!(invalid.preflight(""), Err(Error::InvalidLimits));
    }
    assert_eq!(
        Limits {
            parse: ParseLimits {
                max_source_bytes: 1,
                ..defaults.parse
            },
            ..defaults
        }
        .preflight("é"),
        Err(Error::Budget(Resource::Source))
    );
    assert_eq!(
        Limits {
            max_work: 1,
            ..defaults
        }
        .preflight("é"),
        Err(Error::Budget(Resource::Work))
    );
}
#[test]
fn cst_token_depth_and_all_inventory_budgets_are_enforced() {
    let (source, green, decl) = function(true);
    let d = Limits::default();
    for (limits, resource) in [
        (
            Limits {
                parse: ParseLimits {
                    max_tokens: 0,
                    ..d.parse
                },
                ..d
            },
            Resource::Tokens,
        ),
        (
            Limits {
                parse: ParseLimits {
                    max_depth: 1,
                    ..d.parse
                },
                ..d
            },
            Resource::Depth,
        ),
        (
            Limits {
                max_declarations: 0,
                ..d
            },
            Resource::Declarations,
        ),
        (
            Limits {
                max_metadata_bytes: 0,
                ..d
            },
            Resource::Metadata,
        ),
        (
            Limits {
                max_work: source.len(),
                ..d
            },
            Resource::Work,
        ),
    ] {
        assert_eq!(
            assemble(
                Arc::clone(&source),
                green.clone(),
                vec![],
                vec![decl.clone()],
                vec![],
                limits
            )
            .unwrap_err(),
            Error::Budget(resource)
        );
    }
    let site = RecoverySite {
        span: 0..0,
        kind: SiteKind::Unexpected,
        owner: None,
        receiver: None,
    };
    assert_eq!(
        assemble(
            Arc::from(""),
            raw(""),
            vec![],
            vec![],
            vec![site],
            Limits { max_sites: 0, ..d }
        )
        .unwrap_err(),
        Error::Budget(Resource::Sites)
    );
    assert_eq!(
        assemble(
            Arc::from(""),
            raw(""),
            vec![Diagnostic::new("E", "m", 0..0)],
            vec![],
            vec![],
            Limits {
                parse: ParseLimits {
                    max_diagnostics: 0,
                    ..d.parse
                },
                ..d
            }
        )
        .unwrap_err(),
        Error::Budget(Resource::Diagnostics)
    );
    let mut b = GreenNodeBuilder::new();
    b.start_node(MODULE);
    b.token(SyntaxKind(TokenKind::Lower as u16), "long");
    b.finish_node();
    assert_eq!(
        assemble(
            Arc::from("long"),
            b.finish(),
            vec![],
            vec![],
            vec![],
            Limits {
                max_identifier_bytes: 3,
                ..d
            }
        )
        .unwrap_err(),
        Error::Budget(Resource::Identifier)
    );
}
#[test]
fn related_help_context_and_receiver_metadata_are_not_free() {
    let (source, green, mut decl) = function(true);
    decl.body_context
        .as_mut()
        .unwrap()
        .bound_values
        .insert("long".into());
    assert_eq!(
        assemble(
            Arc::clone(&source),
            green.clone(),
            vec![],
            vec![decl],
            vec![],
            Limits {
                max_identifier_bytes: 3,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::Budget(Resource::Identifier)
    );
    let text = "help".repeat(1000);
    let diag = Diagnostic::new("E", "m", 0..1)
        .with_help(text.clone())
        .with_related(1..2, text);
    assert_eq!(
        assemble(
            Arc::from("xy"),
            raw("xy"),
            vec![diag],
            vec![],
            vec![],
            Limits {
                max_metadata_bytes: 1024,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::Budget(Resource::Metadata)
    );
}

#[test]
fn eligible_context_requires_actual_block_kind_and_module_root() {
    let (source, changed, decl) = function_kind(true, crate::parser::TYPE);
    assert_eq!(
        assemble(
            source,
            changed,
            vec![],
            vec![decl],
            vec![],
            Limits::default()
        )
        .unwrap_err(),
        Error::InvalidInventory
    );
    let root = GreenNode::new(
        crate::parser::EXPR,
        std::iter::empty::<rowan::NodeOrToken<GreenNode, rowan::GreenToken>>(),
    );
    assert_eq!(
        assemble(
            Arc::from(""),
            root,
            vec![],
            vec![],
            vec![],
            Limits::default()
        )
        .unwrap_err(),
        Error::CstMismatch
    );
}
