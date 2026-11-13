//! Actual first-pass document capture, with source association kept private.
use crate::{
    DocumentModel, Limits, ModelCapture, ProofError, ProofLimits, Work, format_with_capture,
};
use vesper_syntax::lexer::{self, TokenKind};

/// A bounded document-algebra witness associated with actual formatted source.
/// This is not checked program semantics or a Rust implementation proof.
///
/// ```compile_fail
/// use vesper_fmt::FormatVerification;
/// let forged = FormatVerification {};
/// ```
#[derive(Debug)]
pub struct FormatVerification {
    output: String,
    changed: bool,
    source_hash: [u8; 32],
    output_hash: [u8; 32],
    model: DocumentModel,
}
impl FormatVerification {
    pub fn output(&self) -> &str {
        &self.output
    }
    pub fn changed(&self) -> bool {
        self.changed
    }
    pub fn source_hash(&self) -> [u8; 32] {
        self.source_hash
    }
    pub fn output_hash(&self) -> [u8; 32] {
        self.output_hash
    }
    pub fn model(&self) -> &DocumentModel {
        &self.model
    }
    pub fn emit_lean(&self, limits: ProofLimits) -> Result<String, ProofError> {
        crate::proof_lean::emit(&self.model, self.source_hash, self.output_hash, limits)
    }
}

fn bounded_format_limits(limits: Limits) -> Result<Limits, ProofError> {
    if [
        limits.max_input_bytes,
        limits.max_output_bytes,
        limits.max_tokens,
        limits.max_doc_nodes,
        limits.max_depth,
        limits.max_work,
    ]
    .contains(&0)
    {
        return Err(ProofError::InvalidLimits);
    }
    // The source-proof profile is stricter than the ordinary formatter.
    // Requests retain their tighter limits; ordinary defaults safely clamp.
    Ok(Limits {
        max_input_bytes: limits.max_input_bytes.min(128 * 1024),
        max_output_bytes: limits.max_output_bytes.min(256 * 1024),
        max_tokens: limits.max_tokens.min(8192),
        max_doc_nodes: limits.max_doc_nodes.min(100_000),
        max_depth: limits.max_depth.min(24),
        max_work: limits.max_work.min(128 * 1024 * 1024),
    })
}

/// Format and retain the exact first rendered Doc projection. All ordinary
/// conservation/reparse/idempotence checks must pass before this returns.
pub fn format_for_verification(
    source: &str,
    format: Limits,
    proof: ProofLimits,
) -> Result<FormatVerification, ProofError> {
    proof.validate()?;
    let format = bounded_format_limits(format)?;
    let max_work = format
        .max_work
        .checked_add(proof.max_work)
        .ok_or(ProofError::InvalidLimits)?;
    let mut work = Work::new(max_work);
    // This allowance is paid before the actual arena projection can clone.
    work.charge(proof.max_work).map_err(ProofError::Formatter)?;
    let mut model = None;
    let mut projection_error = None;
    let result = format_with_capture(
        source,
        format,
        &mut work,
        Some(ModelCapture {
            limits: proof,
            model: &mut model,
            error: &mut projection_error,
        }),
    );
    if let Some(error) = projection_error {
        return Err(error);
    }
    let result = result.map_err(ProofError::Formatter)?;
    let model = model.ok_or(ProofError::Conservation)?;
    // Pay for the bounded lexer staging, payload comparisons and both hashes
    // before allocation/traversal; retain no caller-supplied source identity.
    let charge = source
        .len()
        .checked_mul(8)
        .and_then(|n| {
            format
                .max_tokens
                .checked_mul(std::mem::size_of::<lexer::Token>())
                .and_then(|tokens| n.checked_add(tokens))
        })
        .and_then(|n| {
            result
                .output
                .len()
                .checked_mul(2)
                .and_then(|v| n.checked_add(v))
        })
        .and_then(|n| {
            model
                .text_bytes()
                .checked_mul(2)
                .and_then(|v| n.checked_add(v))
        })
        .ok_or(ProofError::Resource("source association work"))?;
    work.charge(charge).map_err(ProofError::Formatter)?;
    let lexed = lexer::lex_with_limits(
        source,
        lexer::LexLimits {
            max_source_bytes: format.max_input_bytes,
            max_tokens: format.max_tokens,
            max_diagnostics: 1,
        },
    )
    .map_err(|_| ProofError::Conservation)?;
    if lexed.limit.is_some() || !lexed.diagnostics.is_empty() {
        return Err(ProofError::Conservation);
    }
    let comments = lexed
        .tokens
        .iter()
        .filter(|token| matches!(token.kind, TokenKind::Comment | TokenKind::BlockComment))
        .map(|token| &source[token.span.clone()]);
    if !comments.eq(model.comments().iter().map(String::as_str)) {
        return Err(ProofError::Conservation);
    }
    Ok(FormatVerification {
        source_hash: *blake3::hash(source.as_bytes()).as_bytes(),
        output_hash: *blake3::hash(result.output.as_bytes()).as_bytes(),
        output: result.output,
        changed: result.changed,
        model,
    })
}
