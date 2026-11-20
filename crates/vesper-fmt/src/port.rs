//! The port build's side of the boundary (`cfg(vesper_port)`): the error
//! record of the Vesper formatter (`vesper/src/lib.ves`) and the forwarding
//! of [`crate::format_source`].
//!
//! The port formats every source the Rust code formats with its ordinary
//! parser, lexing with the same hard ceilings (`lex_with_limits`). Sources
//! in the macro profile (`m!`, `quote`) come back as `OpaqueMacro`, and the
//! Rust code formats them from the start, so the result is the Rust
//! result.

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
        11 => Error::Conservation,
        12 => Error::NotIdempotent,
        // OpaqueMacro (10): the Rust build formats the macro profile.
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
