//! Versioned token and input/output transport for sandboxed steps.
//!
//! These DTOs mirror `wit/sandbox.wit` and are what the compiler side
//! (Forge macro expansion, `build.vsp` orchestration) will exchange with this
//! crate. They are deliberately independent of Wasmtime types so the
//! expander can be developed against them without linking a runtime.
//!
//! This is an experimental transport contract, not the compiler's canonical
//! `.vhir` or CST representation. Hygiene is carried (scope marks and
//! pre-resolved definitions travel unchanged), but deciding binding or
//! enforcing `[T-MacroConfinement]` is the Forge expander's job.

use serde::{Deserialize, Serialize};

/// Payload protocol version understood by this host.
pub const PROTOCOL_VERSION: u32 = 1;
/// WIT package the component must be built against.
pub const WIT_PACKAGE: &str = "vesper:sandbox@0.1.0";
/// Fully qualified exported interface name.
pub const STEP_INTERFACE: &str = "vesper:sandbox/step@0.1.0";
/// The WIT source of the protocol, for guest authors and integration tests.
pub const WIT_SOURCE: &str = include_str!("../wit/sandbox.wit");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub file: u32,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DefId {
    pub package: u32,
    pub item: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Delimiter {
    Paren,
    Bracket,
    Brace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TokenKind {
    Ident,
    Punct,
    IntLiteral,
    StringLiteral,
    Open(Delimiter),
    Close(Delimiter),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub span: Span,
    /// Opaque Forge scope marks.
    pub scopes: Vec<u32>,
    pub pre_resolved: Option<DefId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StepKind {
    BuildScript,
    ProcMacro,
    DeriveMacro,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Invocation {
    pub protocol_version: u32,
    pub step: StepKind,
    pub entry: String,
    pub args: Vec<String>,
    pub tokens: Vec<Token>,
}

impl Invocation {
    /// An invocation at the current [`PROTOCOL_VERSION`].
    pub fn new(step: StepKind, entry: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            step,
            entry: entry.into(),
            args: Vec::new(),
            tokens: Vec::new(),
        }
    }

    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_tokens(mut self, tokens: Vec<Token>) -> Self {
        self.tokens = tokens;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Option<Span>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Outcome {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Bounds on what may cross the boundary in either direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportLimits {
    pub max_tokens: usize,
    pub max_text_bytes: usize,
    pub max_scopes_per_token: usize,
    pub max_diagnostics: usize,
}

impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            max_tokens: 1 << 20,
            max_text_bytes: 64 << 20,
            max_scopes_per_token: 256,
            max_diagnostics: 4096,
        }
    }
}

/// Direction of a transport-limit violation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Input,
    Output,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportViolation {
    TooManyTokens {
        count: usize,
        limit: usize,
    },
    TooMuchText {
        bytes: usize,
        limit: usize,
    },
    TooManyScopes {
        token: usize,
        count: usize,
        limit: usize,
    },
    TooManyDiagnostics {
        count: usize,
        limit: usize,
    },
    UnbalancedDelimiters {
        token: usize,
    },
}

impl TransportLimits {
    /// Checks a token stream: sizes and balanced, correctly nested delimiters.
    pub fn check_tokens(&self, tokens: &[Token]) -> Result<(), TransportViolation> {
        if tokens.len() > self.max_tokens {
            return Err(TransportViolation::TooManyTokens {
                count: tokens.len(),
                limit: self.max_tokens,
            });
        }
        let mut text = 0usize;
        let mut stack = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            text = text.saturating_add(t.text.len());
            if t.scopes.len() > self.max_scopes_per_token {
                return Err(TransportViolation::TooManyScopes {
                    token: i,
                    count: t.scopes.len(),
                    limit: self.max_scopes_per_token,
                });
            }
            match t.kind {
                TokenKind::Open(d) => stack.push(d),
                TokenKind::Close(d) if stack.pop() != Some(d) => {
                    return Err(TransportViolation::UnbalancedDelimiters { token: i });
                }
                _ => {}
            }
        }
        if !stack.is_empty() {
            return Err(TransportViolation::UnbalancedDelimiters {
                token: tokens.len(),
            });
        }
        if text > self.max_text_bytes {
            return Err(TransportViolation::TooMuchText {
                bytes: text,
                limit: self.max_text_bytes,
            });
        }
        Ok(())
    }

    pub fn check_invocation(&self, inv: &Invocation) -> Result<(), TransportViolation> {
        self.check_tokens(&inv.tokens)?;
        let args: usize = inv.args.iter().map(String::len).sum::<usize>() + inv.entry.len();
        if args > self.max_text_bytes {
            return Err(TransportViolation::TooMuchText {
                bytes: args,
                limit: self.max_text_bytes,
            });
        }
        Ok(())
    }

    pub fn check_diagnostics(&self, diags: &[Diagnostic]) -> Result<(), TransportViolation> {
        if diags.len() > self.max_diagnostics {
            return Err(TransportViolation::TooManyDiagnostics {
                count: diags.len(),
                limit: self.max_diagnostics,
            });
        }
        let bytes: usize = diags.iter().map(|d| d.message.len()).sum();
        if bytes > self.max_text_bytes {
            return Err(TransportViolation::TooMuchText {
                bytes,
                limit: self.max_text_bytes,
            });
        }
        Ok(())
    }

    pub fn check_outcome(&self, out: &Outcome) -> Result<(), TransportViolation> {
        self.check_tokens(&out.tokens)?;
        self.check_diagnostics(&out.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(kind: TokenKind) -> Token {
        Token {
            kind,
            text: String::new(),
            span: Span {
                file: 0,
                start: 0,
                end: 0,
            },
            scopes: vec![],
            pre_resolved: None,
        }
    }

    #[test]
    fn balanced_delimiters_accepted() {
        let l = TransportLimits::default();
        let ts = [
            tok(TokenKind::Open(Delimiter::Brace)),
            tok(TokenKind::Open(Delimiter::Paren)),
            tok(TokenKind::Ident),
            tok(TokenKind::Close(Delimiter::Paren)),
            tok(TokenKind::Close(Delimiter::Brace)),
        ];
        assert_eq!(l.check_tokens(&ts), Ok(()));
    }

    #[test]
    fn crossed_and_unclosed_delimiters_rejected() {
        let l = TransportLimits::default();
        let crossed = [
            tok(TokenKind::Open(Delimiter::Brace)),
            tok(TokenKind::Open(Delimiter::Paren)),
            tok(TokenKind::Close(Delimiter::Brace)),
        ];
        assert_eq!(
            l.check_tokens(&crossed),
            Err(TransportViolation::UnbalancedDelimiters { token: 2 })
        );
        let unclosed = [tok(TokenKind::Open(Delimiter::Bracket))];
        assert_eq!(
            l.check_tokens(&unclosed),
            Err(TransportViolation::UnbalancedDelimiters { token: 1 })
        );
        let stray = [tok(TokenKind::Close(Delimiter::Bracket))];
        assert_eq!(
            l.check_tokens(&stray),
            Err(TransportViolation::UnbalancedDelimiters { token: 0 })
        );
    }

    #[test]
    fn size_limits_enforced() {
        let l = TransportLimits {
            max_tokens: 2,
            max_text_bytes: 4,
            max_scopes_per_token: 1,
            max_diagnostics: 1,
        };
        let three = vec![tok(TokenKind::Ident); 3];
        assert!(matches!(
            l.check_tokens(&three),
            Err(TransportViolation::TooManyTokens { count: 3, limit: 2 })
        ));
        let mut long = tok(TokenKind::Ident);
        long.text = "hello".into();
        assert!(matches!(
            l.check_tokens(&[long]),
            Err(TransportViolation::TooMuchText { bytes: 5, .. })
        ));
        let mut marked = tok(TokenKind::Ident);
        marked.scopes = vec![1, 2];
        assert!(matches!(
            l.check_tokens(&[marked]),
            Err(TransportViolation::TooManyScopes { token: 0, .. })
        ));
        let d = Diagnostic {
            severity: Severity::Note,
            message: String::new(),
            span: None,
        };
        assert!(l.check_diagnostics(&[d.clone(), d]).is_err());
    }

    #[test]
    fn dto_round_trips_through_json() {
        let mut t = tok(TokenKind::Open(Delimiter::Paren));
        t.scopes = vec![7, 9];
        t.pre_resolved = Some(DefId {
            package: 3,
            item: 4,
        });
        let inv = Invocation::new(StepKind::DeriveMacro, "derive_wire_hash")
            .with_args(["x"])
            .with_tokens(vec![t, tok(TokenKind::Close(Delimiter::Paren))]);
        let json = serde_json::to_string(&inv).unwrap();
        let back: Invocation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, inv);
        assert_eq!(back.protocol_version, PROTOCOL_VERSION);
    }

    #[test]
    fn wit_source_declares_versioned_package() {
        assert!(WIT_SOURCE.contains(&format!("package {WIT_PACKAGE};")));
        assert!(STEP_INTERFACE.starts_with("vesper:sandbox/step@"));
    }
}
