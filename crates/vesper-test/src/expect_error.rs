//! Diagnostic verification for in-memory negative substructural tests
//! (`@test(expect_error = EXXXX, primary = "...", related = "...")`).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::attr::ExpectErrorSpec;

/// A diagnostic observed from an isolated checker run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedDiagnostic {
    /// Diagnostic code if assigned (e.g. `Some("E0302")`); `None` typically
    /// indicates an uncoded parse/syntax error.
    pub code: Option<String>,
    /// Human-readable primary diagnostic message.
    pub message: String,
    /// Source slice or rendered label text at the primary span.
    pub primary_text: String,
    /// Rendered label texts from related secondary spans.
    pub related_texts: Vec<String>,
}

/// Outcome of verifying [`ObservedDiagnostic`]s against an [`ExpectErrorSpec`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpectErrorOutcome {
    /// A diagnostic matched the expected code and all specified span substrings.
    Matched {
        /// Matched diagnostic code.
        code: String,
        /// Primary message of the matching diagnostic.
        message: String,
    },
    /// The isolated check succeeded with zero diagnostics.
    NoDiagnostics,
    /// An unrelated parse/syntax error occurred (rejected rather than treated
    /// as a valid substructural failure).
    UnexpectedParseError {
        /// The offending parse diagnostics.
        diagnostics: Vec<ObservedDiagnostic>,
    },
    /// Diagnostics were emitted, but none carried the expected error code.
    CodeMismatch {
        /// Expected diagnostic code.
        expected: String,
        /// All diagnostic codes that were observed.
        observed_codes: Vec<String>,
    },
    /// The expected code was emitted, but its primary span text did not match.
    PrimarySpanMismatch {
        /// Matched diagnostic code.
        code: String,
        /// Expected substring in the primary span text.
        expected_primary: String,
        /// Actual primary span text observed.
        observed_primary: String,
    },
    /// The expected code and primary span matched, but no related label matched.
    RelatedSpanMismatch {
        /// Matched diagnostic code.
        code: String,
        /// Expected substring in a related label.
        expected_related: String,
        /// Actual related label texts observed.
        observed_related: Vec<String>,
    },
}

impl ExpectErrorOutcome {
    /// Returns `true` if the negative test expectation was satisfied.
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Matched { .. })
    }
}

impl fmt::Display for ExpectErrorOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Matched { code, message } => write!(f, "matched {code}: {message}"),
            Self::NoDiagnostics => f.write_str("expected diagnostic, but checking succeeded"),
            Self::UnexpectedParseError { diagnostics } => {
                let first = diagnostics
                    .first()
                    .map(|d| d.message.as_str())
                    .unwrap_or("syntax error");
                write!(f, "rejected unrelated parse error: {first}")
            }
            Self::CodeMismatch {
                expected,
                observed_codes,
            } => write!(
                f,
                "expected error code {expected}, observed [{}]",
                observed_codes.join(", ")
            ),
            Self::PrimarySpanMismatch {
                code,
                expected_primary,
                observed_primary,
            } => write!(
                f,
                "{code} primary span mismatch: expected substring `{expected_primary}`, got `{observed_primary}`"
            ),
            Self::RelatedSpanMismatch {
                code,
                expected_related,
                observed_related,
            } => write!(
                f,
                "{code} related span mismatch: expected substring `{expected_related}`, observed [{}]",
                observed_related.join(", ")
            ),
        }
    }
}

/// Returns `true` when `code` belongs to the parser/lexer error range (`E00xx`).
fn is_parse_error_code(code: &str) -> bool {
    code.starts_with("E00") || code.starts_with("PARSE")
}

/// Verify observed diagnostics from a forked checker run against `spec`.
///
/// Rules:
/// 1. Empty diagnostic lists fail with [`ExpectErrorOutcome::NoDiagnostics`].
/// 2. Uncoded diagnostics (`code: None`) or parse-range codes (`E00xx`) fail
///    immediately with [`ExpectErrorOutcome::UnexpectedParseError`] unless
///    `spec.code` itself expects a parse error code.
/// 3. At least one diagnostic must match `spec.code` exactly, contain
///    `spec.primary` (if specified) in `primary_text`, and contain `spec.related`
///    (if specified) in at least one entry of `related_texts`.
pub fn verify_expect_error(
    spec: &ExpectErrorSpec,
    diagnostics: &[ObservedDiagnostic],
) -> ExpectErrorOutcome {
    if diagnostics.is_empty() {
        return ExpectErrorOutcome::NoDiagnostics;
    }

    let expects_parse_error = is_parse_error_code(&spec.code);
    let parse_errors: Vec<ObservedDiagnostic> = diagnostics
        .iter()
        .filter(|d| match d.code.as_deref() {
            None => true,
            Some(code) => !expects_parse_error && is_parse_error_code(code),
        })
        .cloned()
        .collect();

    if !parse_errors.is_empty() {
        return ExpectErrorOutcome::UnexpectedParseError {
            diagnostics: parse_errors,
        };
    }

    let code_matches: Vec<&ObservedDiagnostic> = diagnostics
        .iter()
        .filter(|d| d.code.as_deref() == Some(spec.code.as_str()))
        .collect();

    if code_matches.is_empty() {
        let observed_codes = diagnostics
            .iter()
            .filter_map(|d| d.code.clone())
            .collect();
        return ExpectErrorOutcome::CodeMismatch {
            expected: spec.code.clone(),
            observed_codes,
        };
    }

    let mut primary_failure: Option<&ObservedDiagnostic> = None;
    let mut related_failure: Option<&ObservedDiagnostic> = None;

    for diag in code_matches {
        let primary_ok = spec
            .primary
            .as_deref()
            .is_none_or(|want| diag.primary_text.contains(want));
        if !primary_ok {
            if primary_failure.is_none() {
                primary_failure = Some(diag);
            }
            continue;
        }

        let related_ok = spec
            .related
            .as_deref()
            .is_none_or(|want| diag.related_texts.iter().any(|r| r.contains(want)));
        if !related_ok {
            if related_failure.is_none() {
                related_failure = Some(diag);
            }
            continue;
        }

        return ExpectErrorOutcome::Matched {
            code: spec.code.clone(),
            message: diag.message.clone(),
        };
    }

    if let Some(diag) = related_failure {
        return ExpectErrorOutcome::RelatedSpanMismatch {
            code: spec.code.clone(),
            expected_related: spec.related.clone().unwrap_or_default(),
            observed_related: diag.related_texts.clone(),
        };
    }

    let diag = primary_failure.expect("code_matches was non-empty");
    ExpectErrorOutcome::PrimarySpanMismatch {
        code: spec.code.clone(),
        expected_primary: spec.primary.clone().unwrap_or_default(),
        observed_primary: diag.primary_text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear_leak_diag() -> ObservedDiagnostic {
        ObservedDiagnostic {
            code: Some("E0302".into()),
            message: "unconsumed linear obligation `txn`".into(),
            primary_text: "early_return?".into(),
            related_texts: vec!["linear value `txn` created here".into()],
        }
    }

    #[test]
    fn matches_exact_code_primary_and_related() {
        let spec = ExpectErrorSpec::new("E0302")
            .with_primary("early_return?")
            .with_related("created here");

        let outcome = verify_expect_error(&spec, &[linear_leak_diag()]);
        assert!(outcome.is_pass());
        assert_eq!(
            outcome,
            ExpectErrorOutcome::Matched {
                code: "E0302".into(),
                message: "unconsumed linear obligation `txn`".into(),
            }
        );
    }

    #[test]
    fn rejects_empty_diagnostics() {
        let spec = ExpectErrorSpec::new("E0302");
        assert_eq!(
            verify_expect_error(&spec, &[]),
            ExpectErrorOutcome::NoDiagnostics
        );
    }

    #[test]
    fn rejects_unrelated_parse_errors() {
        let spec = ExpectErrorSpec::new("E0302");
        let parse_err = ObservedDiagnostic {
            code: None,
            message: "unexpected token `}`".into(),
            primary_text: "}".into(),
            related_texts: vec![],
        };
        let outcome = verify_expect_error(&spec, &[parse_err.clone(), linear_leak_diag()]);
        assert!(!outcome.is_pass());
        assert!(matches!(
            outcome,
            ExpectErrorOutcome::UnexpectedParseError { .. }
        ));

        let coded_parse_err = ObservedDiagnostic {
            code: Some("E0001".into()),
            message: "expected `;`".into(),
            primary_text: "let x = 1".into(),
            related_texts: vec![],
        };
        assert!(matches!(
            verify_expect_error(&spec, &[coded_parse_err]),
            ExpectErrorOutcome::UnexpectedParseError { .. }
        ));
    }

    #[test]
    fn reports_code_primary_and_related_mismatches() {
        let wrong_code = ExpectErrorSpec::new("E0301");
        assert!(matches!(
            verify_expect_error(&wrong_code, &[linear_leak_diag()]),
            ExpectErrorOutcome::CodeMismatch { .. }
        ));

        let wrong_primary = ExpectErrorSpec::new("E0302").with_primary("other_expr");
        assert!(matches!(
            verify_expect_error(&wrong_primary, &[linear_leak_diag()]),
            ExpectErrorOutcome::PrimarySpanMismatch { .. }
        ));

        let wrong_related = ExpectErrorSpec::new("E0302")
            .with_primary("early_return?")
            .with_related("borrowed here");
        assert!(matches!(
            verify_expect_error(&wrong_related, &[linear_leak_diag()]),
            ExpectErrorOutcome::RelatedSpanMismatch { .. }
        ));
    }
}
