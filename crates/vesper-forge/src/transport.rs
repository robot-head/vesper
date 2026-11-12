//! Import of untrusted flat `vesper-loom-sandbox` transport output.
//!
//! The sandbox transport carries guest-supplied scopes, spans and optional
//! numeric `DefId`s. None of it is trusted here:
//!
//! * guest scope arrays are discarded; every token gets the macro's
//!   definition-site scope and the expansion's fresh introduction scope;
//! * spans are kept only as [`crate::ReportedSpan`] display data;
//! * a pre-resolved `DefId` becomes an unvalidated claim, checked by the
//!   resolver against the registry and the macro package's visibility;
//! * flat output has no authenticated splice evidence, so every token is
//!   attributed to the macro (none can claim caller origin).

use vesper_loom_sandbox::transport::{
    self as t, Delimiter, Invocation, Outcome, StepKind, TokenKind as TKind, TransportLimits,
};

use crate::error::ForgeError;
use crate::host::HygieneHost;
use crate::identity::ExpansionId;
use crate::registry::{DefId, PackageId};
use crate::token::{Delim, GeneratedToken, HostToken, ReportedSpan, TokenKind};

fn delim(d: Delimiter) -> Delim {
    match d {
        Delimiter::Paren => Delim::Paren,
        Delimiter::Bracket => Delim::Bracket,
        Delimiter::Brace => Delim::Brace,
    }
}

fn undelim(d: Delim) -> Delimiter {
    match d {
        Delim::Paren => Delimiter::Paren,
        Delim::Bracket => Delimiter::Bracket,
        Delim::Brace => Delimiter::Brace,
    }
}

fn kind(k: TKind) -> TokenKind {
    match k {
        TKind::Ident => TokenKind::Ident,
        TKind::Punct => TokenKind::Punct,
        TKind::IntLiteral => TokenKind::Int,
        TKind::StringLiteral => TokenKind::Str,
        TKind::Open(d) => TokenKind::Open(delim(d)),
        TKind::Close(d) => TokenKind::Close(delim(d)),
    }
}

fn unkind(k: TokenKind) -> TKind {
    match k {
        TokenKind::Ident => TKind::Ident,
        TokenKind::Punct => TKind::Punct,
        TokenKind::Int => TKind::IntLiteral,
        TokenKind::Str => TKind::StringLiteral,
        TokenKind::Open(d) => TKind::Open(undelim(d)),
        TokenKind::Close(d) => TKind::Close(undelim(d)),
    }
}

/// Converts one untrusted transport token. Guest scopes are dropped.
pub fn generated_from_transport(tok: &t::Token) -> GeneratedToken {
    GeneratedToken {
        kind: kind(tok.kind),
        text: tok.text.clone(),
        claimed_def: tok.pre_resolved.map(|d| DefId {
            package: PackageId(d.package),
            item: d.item,
        }),
        reported_span: Some(ReportedSpan {
            file: tok.span.file,
            start: tok.span.start,
            end: tok.span.end,
        }),
    }
}

impl HygieneHost {
    /// Builds the invocation sent to the guest. Host scope identities are not
    /// exported (scope arrays are empty) and no `DefId`s are revealed.
    pub fn invocation_for(&self, step: StepKind, entry: &str, tokens: &[HostToken]) -> Invocation {
        let toks = tokens
            .iter()
            .map(|h| t::Token {
                kind: unkind(h.kind),
                text: self.text(h.text).to_owned(),
                span: h
                    .reported_span
                    .map(|s| t::Span {
                        file: s.file,
                        start: s.start,
                        end: s.end,
                    })
                    .unwrap_or(t::Span {
                        file: 0,
                        start: 0,
                        end: 0,
                    }),
                scopes: Vec::new(),
                pre_resolved: None,
            })
            .collect();
        Invocation::new(step, entry).with_tokens(toks)
    }

    /// Finishes expansion `id` from an untrusted flat sandbox outcome.
    ///
    /// The outcome is first checked against `limits`; any error diagnostic
    /// from the guest fails the expansion. The expansion is closed either way.
    pub fn finish_flat_outcome(
        &mut self,
        id: ExpansionId,
        outcome: &Outcome,
        limits: &TransportLimits,
    ) -> Result<Vec<HostToken>, ForgeError> {
        if let Err(v) = limits.check_outcome(outcome) {
            self.abandon(id)?;
            return Err(ForgeError::Transport(format!("{v:?}")));
        }
        if let Some(d) = outcome
            .diagnostics
            .iter()
            .find(|d| d.severity == t::Severity::Error)
        {
            self.abandon(id)?;
            return Err(ForgeError::MacroReportedError {
                message: d.message.clone(),
            });
        }
        let generated = outcome
            .tokens
            .iter()
            .map(generated_from_transport)
            .collect();
        self.finish_generated(id, generated)
    }
}
