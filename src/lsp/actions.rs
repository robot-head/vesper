//! Pure source formatting and freshly checked, SemVer-compatible lint actions.
//!
//! Client diagnostics, fix payloads and purported verification flags are not
//! inputs. URIs are opaque labels, never paths. Returned edits are proposals;
//! this module performs no I/O, client applyEdit, JIT, macro or proof execution.
use crate::tooling::lint::{self, ConfigProvenance};
use serde_json::{Value as Json, json};
use std::fmt;
use vesper_lint::policy::Policy;
use vesper_span::{ByteSpan, LineIndex, Range};

pub const MAX_SOURCE_BYTES: usize = 128 * 1024;
pub const MAX_URI_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceStamp {
    pub version: i32,
    pub source_hash: [u8; 32],
}
/// A validated association of current server text, UTF-16 index and version.
/// Construct a new value after each accepted document change.
pub struct ActionDocument<'a> {
    uri: &'a str,
    source: &'a str,
    index: &'a LineIndex,
    stamp: SourceStamp,
}
impl<'a> ActionDocument<'a> {
    pub fn new(
        uri: &'a str,
        version: i32,
        source: &'a str,
        index: &'a LineIndex,
    ) -> Result<Self, ActionError> {
        if uri.is_empty() || uri.len() > MAX_URI_BYTES {
            return Err(ActionError::InvalidDocument("URI byte limit"));
        }
        if source.len() > MAX_SOURCE_BYTES {
            return Err(ActionError::InvalidDocument("source exceeds 128 KiB"));
        }
        if index.text() != source {
            return Err(ActionError::InvalidDocument("source/index mismatch"));
        }
        Ok(Self {
            uri,
            source,
            index,
            stamp: SourceStamp {
                version,
                source_hash: *blake3::hash(source.as_bytes()).as_bytes(),
            },
        })
    }
    pub fn stamp(&self) -> SourceStamp {
        self.stamp
    }
    fn validate(&self, expected: SourceStamp) -> Result<(), ActionError> {
        if expected != self.stamp {
            Err(ActionError::StaleDocument)
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionError {
    InvalidDocument(&'static str),
    InvalidPolicyProvenance(&'static str),
    StaleDocument,
    InvalidRange(String),
    FormatterUnavailable(String),
    LintUnavailable(String),
    Invariant(&'static str),
}
impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDocument(reason) => write!(f, "invalid action document: {reason}"),
            Self::InvalidPolicyProvenance(reason) => {
                write!(f, "invalid policy provenance: {reason}")
            }
            Self::StaleDocument => f.write_str("document version/source identity changed"),
            Self::InvalidRange(reason) => write!(f, "invalid selection: {reason}"),
            Self::FormatterUnavailable(reason) => write!(f, "formatting unavailable: {reason}"),
            Self::LintUnavailable(reason) => write!(f, "checked lint action unavailable: {reason}"),
            Self::Invariant(reason) => write!(f, "action invariant failed: {reason}"),
        }
    }
}
impl std::error::Error for ActionError {}
#[derive(Debug)]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}
impl TextEdit {
    pub fn to_json(&self) -> Json {
        json!({"range":range_json(self.range),"newText":self.new_text})
    }
}
fn range_json(range: Range) -> Json {
    json!({"start":{"line":range.start.line,"character":range.start.character},
        "end":{"line":range.end.line,"character":range.end.character}})
}
fn hash_text(hash: [u8; 32]) -> String {
    blake3::Hash::from_bytes(hash).to_hex().to_string()
}
fn identity_json(uri: &str, stamp: SourceStamp) -> Json {
    json!({"uri":uri,"version":stamp.version,"source_blake3":hash_text(stamp.source_hash)})
}
fn document_edit_json(uri: &str, stamp: SourceStamp, edits: &[TextEdit]) -> Json {
    json!({"documentChanges":[{"textDocument":{"uri":uri,"version":stamp.version},
        "edits":edits.iter().map(TextEdit::to_json).collect::<Vec<_>>()}]})
}
fn replacement(document: &ActionDocument<'_>, candidate: String) -> Result<TextEdit, ActionError> {
    if candidate.len() > MAX_SOURCE_BYTES {
        return Err(ActionError::InvalidDocument("candidate exceeds 128 KiB"));
    }
    let range = document
        .index
        .range(ByteSpan {
            start: 0,
            end: document.source.len(),
        })
        .map_err(|error| ActionError::InvalidRange(error.to_string()))?;
    // A whole-document replacement never slices a common byte prefix/suffix
    // through UTF-8. The selected lint engine alone decides semantic changes.
    Ok(TextEdit {
        range,
        new_text: candidate,
    })
}
/// Actual canonical formatter result. No tab-width/line-width knobs exist.
#[derive(Debug)]
pub struct Formatting {
    pub edits: Vec<TextEdit>,
    pub provenance: Json,
    uri: String,
    stamp: SourceStamp,
}
impl Formatting {
    /// Standard textDocument/formatting result; LSP defines an edit array.
    pub fn to_json(&self) -> Json {
        Json::Array(self.edits.iter().map(TextEdit::to_json).collect())
    }
    /// Versioned alternative for an explicit editor command, not auto-applied.
    pub fn workspace_edit_json(&self) -> Json {
        document_edit_json(&self.uri, self.stamp, &self.edits)
    }
}
pub fn format_document(
    document: &ActionDocument<'_>,
    expected: SourceStamp,
) -> Result<Formatting, ActionError> {
    document.validate(expected)?;
    let limits = vesper_fmt::Limits {
        max_input_bytes: MAX_SOURCE_BYTES,
        max_output_bytes: MAX_SOURCE_BYTES,
        ..vesper_fmt::Limits::default()
    };
    let formatted = vesper_fmt::format_source(document.source, limits)
        .map_err(|error| ActionError::FormatterUnavailable(error.to_string()))?;
    let edits = if formatted.changed {
        vec![replacement(document, formatted.output)?]
    } else {
        Vec::new()
    };
    Ok(Formatting {
        edits,
        uri: document.uri.to_owned(),
        stamp: document.stamp,
        provenance: json!({"document":identity_json(document.uri,document.stamp),"formatter":"vesper-fmt","line_width":100,"indent_spaces":4,"token_and_comment_conserved":true}),
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionRequest {
    QuickFix,
    FixAllUnusedEffects,
}
#[derive(Debug)]
pub struct CodeAction {
    pub title: &'static str,
    pub kind: &'static str,
    pub edit: TextEdit,
    pub provenance: Json,
    uri: String,
    stamp: SourceStamp,
}
impl CodeAction {
    pub fn to_json(&self) -> Json {
        json!({"title":self.title,"kind":self.kind,
            "edit":document_edit_json(&self.uri,self.stamp,std::slice::from_ref(&self.edit)),"data":self.provenance})
    }
}
fn bounded_reason(reason: String) -> String {
    reason.chars().take(2048).collect()
}
fn validate_provenance(provenance: &ConfigProvenance) -> Result<(), ActionError> {
    if let ConfigProvenance::Explicit {
        path,
        bytes,
        blake3,
    } = provenance
    {
        if path.is_empty() || path.len() > MAX_URI_BYTES || *bytes > 65_536 {
            return Err(ActionError::InvalidPolicyProvenance(
                "path/config byte limit",
            ));
        }
        if blake3.len() != 64 || !blake3.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ActionError::InvalidPolicyProvenance("BLAKE3 hex shape"));
        }
    }
    Ok(())
}
/// Recompute current checked findings, apply policy before selection, then require
/// complete candidate rechecking and actual supported-interface compatibility.
/// QuickFix selects only intersecting findings (caret uses half-open containment).
/// Explicit FixAllUnusedEffects is a separate source.fixAll.vesper action.
pub fn code_actions(
    document: &ActionDocument<'_>,
    expected: SourceStamp,
    selection: Range,
    request: ActionRequest,
    policy: &Policy,
    provenance: ConfigProvenance,
) -> Result<Vec<CodeAction>, ActionError> {
    document.validate(expected)?;
    validate_provenance(&provenance)?;
    // Even a fix-all request cannot smuggle a reversed/interior/out-of-bounds
    // range. Client diagnostics/fix/verified payloads have no API parameter.
    let selected = document
        .index
        .span(selection)
        .map_err(|error| ActionError::InvalidRange(error.to_string()))?;
    let prepared = match request {
        ActionRequest::QuickFix => lint::prepare_source_selected_with_policy(
            document.source,
            crate::ast::Span {
                start: selected.start,
                end: selected.end,
            },
            true,
            policy,
            provenance,
        ),
        ActionRequest::FixAllUnusedEffects => {
            // The existing CLI fix-all retains its older profile. Apply the
            // bounded editor guard before entering that checker path.
            crate::tooling::frontend::check_source(
                document.source,
                crate::tooling::frontend::AnalysisLimits::default(),
            )
            .map_err(|error| ActionError::LintUnavailable(error.to_string()))?;
            lint::prepare_source_with_policy(document.source, true, true, policy, provenance)
        }
    }
    .map_err(|reason| ActionError::LintUnavailable(bounded_reason(reason)))?;
    if let Some(skipped) = prepared.report.skipped.first() {
        return Err(ActionError::LintUnavailable(bounded_reason(format!(
            "conservative unsupported lint evidence in {}: {}",
            skipped.function, skipped.reason
        ))));
    }
    let Some(candidate) = prepared.candidate else {
        return Ok(Vec::new());
    };
    if !prepared.report.changed
        || prepared.report.applied_edits == 0
        || candidate == document.source
    {
        return Err(ActionError::Invariant("candidate/change metadata mismatch"));
    }
    if candidate.len() > MAX_SOURCE_BYTES {
        return Err(ActionError::InvalidDocument("candidate exceeds 128 KiB"));
    }
    let semver = prepared
        .report
        .semver
        .as_ref()
        .ok_or(ActionError::Invariant("missing actual SemVer comparison"))?;
    if !semver.is_compatible() {
        return Err(ActionError::LintUnavailable(
            "candidate is not supported-interface compatible".into(),
        ));
    }
    let rules = policy.rules();
    let rules_json =
        serde_json::to_vec(&rules).map_err(|_| ActionError::Invariant("policy serialization"))?;
    let (title, kind) = match request {
        ActionRequest::QuickFix => (
            "Remove selected unused effect capabilities (checked, SemVer compatible)",
            "quickfix",
        ),
        ActionRequest::FixAllUnusedEffects => (
            "Remove all unused effect capabilities in document (checked, SemVer compatible)",
            "source.fixAll.vesper",
        ),
    };
    let data = json!({"schema":"vesper.code-action/1","document":identity_json(document.uri,document.stamp),
        "candidate_blake3":blake3::hash(candidate.as_bytes()).to_hex().to_string(),
        "selection":range_json(selection),"policy":prepared.report.policy,
        "policy_rules_blake3":blake3::hash(&rules_json).to_hex().to_string(),
        "semver":semver,"applied_edits":prepared.report.applied_edits,
        "scope":"fresh source parse/type/effect/PCG checks and supported-interface compatibility; no proof or liveness certificate"});
    Ok(vec![CodeAction {
        title,
        kind,
        edit: replacement(document, candidate)?,
        provenance: data,
        uri: document.uri.to_owned(),
        stamp: document.stamp,
    }])
}
