#![cfg(feature = "sandbox-transport")]

mod common;

use common::*;
use vesper_forge::*;
use vesper_loom_sandbox::transport::{
    self as t, Diagnostic, Outcome, Severity, StepKind, TransportLimits,
};

/// Untrusted guest tokens with forged scopes, caller spans and claims.
fn guest(src: &str, scopes: Vec<u32>, span: t::Span) -> Vec<t::Token> {
    lex_fragment(src)
        .into_iter()
        .map(|g| t::Token {
            kind: match g.kind {
                TokenKind::Ident => t::TokenKind::Ident,
                TokenKind::Punct => t::TokenKind::Punct,
                TokenKind::Int => t::TokenKind::IntLiteral,
                TokenKind::Str => t::TokenKind::StringLiteral,
                TokenKind::Open(Delim::Paren) => t::TokenKind::Open(t::Delimiter::Paren),
                TokenKind::Close(Delim::Paren) => t::TokenKind::Close(t::Delimiter::Paren),
                TokenKind::Open(Delim::Brace) => t::TokenKind::Open(t::Delimiter::Brace),
                TokenKind::Close(Delim::Brace) => t::TokenKind::Close(t::Delimiter::Brace),
                TokenKind::Open(Delim::Bracket) => t::TokenKind::Open(t::Delimiter::Bracket),
                TokenKind::Close(Delim::Bracket) => t::TokenKind::Close(t::Delimiter::Bracket),
            },
            text: g.text,
            span,
            scopes: scopes.clone(),
            pre_resolved: None,
        })
        .collect()
}

const CALLER_SPAN: t::Span = t::Span {
    file: 1,
    start: 10,
    end: 11,
};

fn caller_scope_raw(h: &mut HygieneHost) -> u32 {
    h.module_scope(APP).unwrap().index()
}

#[test]
fn invocation_exports_no_host_identities() {
    let mut h = host();
    let toks = h.lex_source(APP, "connect(1)").unwrap();
    let inv = h.invocation_for(StepKind::ProcMacro, "m", &toks);
    assert_eq!(inv.tokens.len(), 4);
    assert!(
        inv.tokens
            .iter()
            .all(|t| t.scopes.is_empty() && t.pre_resolved.is_none())
    );
}

#[test]
fn forged_caller_scopes_and_spans_cannot_capture_or_exempt() {
    let mut h = host();
    let raw = caller_scope_raw(&mut h);
    let mut program = h.lex_source(APP, "let x = 1;").unwrap();
    let x = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap();
    // The guest pretends its `x` is caller syntax.
    let out = Outcome {
        tokens: guest("x", vec![raw], CALLER_SPAN),
        diagnostics: vec![],
    };
    let toks = h
        .finish_flat_outcome(x.id, &out, &TransportLimits::default())
        .unwrap();
    assert_eq!(toks[0].origin(), Origin::Expansion(x.id));
    assert_eq!(
        toks[0].reported_span(),
        Some(ReportedSpan {
            file: 1,
            start: 10,
            end: 11
        })
    );
    assert!(!toks[0].scopes().contains(h.module_scope(APP).unwrap()));
    program.extend(toks);
    assert_eq!(
        h.resolve(program).unwrap_err(),
        ForgeError::Unbound {
            token: 5,
            name: "x".into()
        }
    );

    // A forged caller-looking privileged call is still gated.
    let mut h = host();
    let raw = caller_scope_raw(&mut h);
    let x = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap();
    let mut tokens = guest("@std/net::connect()", vec![raw], CALLER_SPAN);
    tokens[0].pre_resolved = Some(t::DefId {
        package: CONNECT.package.0,
        item: CONNECT.item,
    });
    let out = Outcome {
        tokens,
        diagnostics: vec![],
    };
    let toks = h
        .finish_flat_outcome(x.id, &out, &TransportLimits::default())
        .unwrap();
    let f = h.resolve(toks).unwrap();
    let err = check_authority(h.session(), f, &graph(), &policies(vec![])).unwrap_err();
    assert!(matches!(
        &err.violations[..],
        [Violation::Exceeds { requirement, .. }] if *requirement == net()
    ));
}

#[test]
fn guest_errors_and_transport_violations_fail_the_expansion() {
    let mut h = host();
    let x = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap();
    let out = Outcome {
        tokens: vec![],
        diagnostics: vec![Diagnostic {
            severity: Severity::Error,
            message: "WireHash requires shared `in self`".into(),
            span: None,
        }],
    };
    assert!(matches!(
        h.finish_flat_outcome(x.id, &out, &TransportLimits::default()),
        Err(ForgeError::MacroReportedError { .. })
    ));
    assert_eq!(
        h.finish_flat_outcome(x.id, &Outcome::default(), &TransportLimits::default())
            .unwrap_err(),
        ForgeError::ExpansionClosed(x.id)
    );

    let x = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap();
    let out = Outcome {
        tokens: guest("f(", vec![], CALLER_SPAN),
        diagnostics: vec![],
    };
    assert!(matches!(
        h.finish_flat_outcome(x.id, &out, &TransportLimits::default()),
        Err(ForgeError::Transport(_))
    ));
}

#[test]
fn pre_resolved_transport_claim_is_validated() {
    let mut h = host();
    let x = h
        .begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), vec![])
        .unwrap();
    let mut tokens = guest("other::f()", vec![], CALLER_SPAN);
    tokens[0].pre_resolved = Some(t::DefId {
        package: OTHER_FN.package.0,
        item: OTHER_FN.item,
    });
    let out = Outcome {
        tokens,
        diagnostics: vec![],
    };
    let toks = h
        .finish_flat_outcome(x.id, &out, &TransportLimits::default())
        .unwrap();
    assert_eq!(
        h.resolve(toks).unwrap_err(),
        ForgeError::NotADependency {
            from: DERIVE,
            def: OTHER_FN
        }
    );
}
