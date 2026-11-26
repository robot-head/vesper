//! The port build's side of the boundary (`cfg(vesper_port)`): the error
//! record of the Vesper formatter (`vesper/src/lib.ves`) and the forwarding
//! of [`crate::format_source`].
//!
//! The port formats every source the Rust code formats, lexing with the
//! same hard ceilings (`lex_with_limits`): ordinary modules, sugar, test
//! blocks and the expression macro profile (`m!`, `quote`), so its result
//! is the Rust result.

pub use crate::port_binding::*;
use crate::{Error, FormatResult, Limits};

/// `FmtFail` of the port: `code` is the index of the [`Error`] variant,
/// `offset` the byte
/// offset of [`Error::Syntax`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FmtFail {
    pub code: u64,
    pub offset: u64,
}

/// A BLAKE3 digest from the port: its 32 bytes as little-endian words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hash32 {
    pub w0: u64,
    pub w1: u64,
    pub w2: u64,
    pub w3: u64,
}

/// `ProofError` of the port's `format_for_verification` (#1341): `code` is
/// 0 for none, else the variant (1 `InvalidLimits` to 7 `LeanSyntax`),
/// `resource` names a `Resource` limit and `fmt` is a `Formatter` error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofFail {
    pub code: u64,
    pub resource: u64,
    pub fmt: FmtFail,
}

/// [`crate::ProofLimits`] in the port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofLimits {
    pub max_nodes: u64,
    pub max_text_bytes: u64,
    pub max_depth: u64,
    pub max_metadata_bytes: u64,
    pub max_lean_bytes: u64,
    pub max_work: u64,
}

/// `blake3::hash(data)` through `@vesper-std/hash-sys`.
pub(crate) fn blake3_bytes(data: &str) -> [u8; 32] {
    let h = blake3(data);
    let mut out = [0u8; 32];
    for (i, w) in [h.w0, h.w1, h.w2, h.w3].into_iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
    }
    out
}

fn error(e: FmtFail) -> Option<Error> {
    Some(match e.code {
        0 => Error::InputLimit,
        1 => Error::OutputLimit,
        2 => Error::TokenLimit,
        3 => Error::NodeLimit,
        4 => Error::DepthLimit,
        5 => Error::WorkLimit,
        6 => Error::Overflow,
        7 => Error::Allocation,
        8 => Error::InvalidDoc,
        9 => Error::Syntax {
            offset: e.offset as usize,
        },
        10 => Error::OpaqueMacro,
        11 => Error::Conservation,
        12 => Error::NotIdempotent,
        _ => return None,
    })
}

/// `format_source` through the port, or `None` when the Rust code must
/// format this source.
pub(crate) fn format_source(source: &str, limits: Limits) -> Option<Result<FormatResult, Error>> {
    let w = |n: usize| n as u64;
    match format(
        source,
        w(limits.max_input_bytes),
        w(limits.max_output_bytes),
        w(limits.max_tokens),
        w(limits.max_doc_nodes),
        w(limits.max_depth),
        w(limits.max_work),
    ) {
        Ok(bytes) => Some(match String::from_utf8(bytes) {
            Ok(output) => Ok(FormatResult {
                changed: source != output,
                output,
            }),
            Err(_) => Err(Error::Conservation),
        }),
        Err(e) => error(e).map(Err),
    }
}

/// `format_recovery_source` through the port, or `None` for an error code
/// the port does not define. The edits come back as (start, end,
/// replacement length) over the source; the replacements are the output's
/// bytes at the shifted positions, and the source digest is computed here.
pub(crate) fn format_recovery_source(
    source: &str,
    limits: Limits,
) -> Option<Result<crate::recovery::RecoveryFormatResult, Error>> {
    let w = |n: usize| n as u64;
    let r = format_recovery(
        source,
        w(limits.max_input_bytes),
        w(limits.max_output_bytes),
        w(limits.max_tokens),
        w(limits.max_doc_nodes),
        w(limits.max_depth),
        w(limits.max_work),
    );
    if !recovery_ok(&r) {
        return error(recovery_error(&r)).map(Err);
    }
    let Ok(output) = String::from_utf8(recovery_output(&r)) else {
        return Some(Err(Error::Conservation));
    };
    let words = recovery_edits(&r);
    let mut edits = Vec::with_capacity(words.len() / 3);
    // `shift`: how far the output has moved past the source so far.
    let mut shift: isize = 0;
    for &[start, end, len] in words.as_chunks::<3>().0 {
        let (start, end, len) = (start as usize, end as usize, len as usize);
        let at = start.checked_add_signed(shift)?;
        let replacement = output.get(at..at + len)?.to_owned();
        shift += len as isize - (end - start) as isize;
        edits.push((start..end, replacement));
    }
    Some(Ok(crate::recovery::RecoveryFormatResult::from_port(
        output,
        edits,
        *blake3::hash(source.as_bytes()).as_bytes(),
        recovery_formatted(&r) as usize,
        recovery_changed(&r),
    )))
}
