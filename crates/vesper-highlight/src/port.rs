//! The port build's side of the boundary (`cfg(vesper_port)`): Tier 0's
//! lexical classes from the Vesper classifier (`vesper/src/lib.ves`).
//!
//! The port returns one word per token, `end | class << 32 | flags << 48`,
//! in source order. The port lexes with the Rust code's ceilings (the hard
//! token ceiling; the hard diagnostic ceiling in token mode, the parser's
//! default 128 in CST mode). A source at which the lexer stops early comes
//! back as `Refused`, and the Rust classifier takes it (with its flat
//! stream), so the result is always the Rust result.

pub use crate::port_binding::*;
use crate::{HighlightSpan, VesperTokenClass};

/// The port could not classify the source as Rust does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    pub len: u64,
}

fn spans(words: Vec<u64>) -> Vec<HighlightSpan> {
    let mut start = 0usize;
    words
        .into_iter()
        .map(|w| {
            let end = (w & 0xffff_ffff) as usize;
            let class = VesperTokenClass::ALL[((w >> 32) & 0xffff) as usize];
            let mut span = HighlightSpan::new(start, end, class);
            span.flags = (w >> 48) as u16;
            start = end;
            span
        })
        .collect()
}

/// Tier 0 in CST mode through the port, or `None` for the Rust classifier.
pub(crate) fn cst(src: &str) -> Option<Vec<HighlightSpan>> {
    tier0(src).ok().map(spans)
}

/// Tier 0 in token mode through the port, or `None` for the Rust classifier.
pub(crate) fn tokens(src: &str) -> Option<Vec<HighlightSpan>> {
    tier0_tokens(src).ok().map(spans)
}
