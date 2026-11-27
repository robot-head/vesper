// Lean extraction (SH5 lane 18, #1341), from `modules/lean_extract`:
// `vesper::lean::extract`, the text of `vesper emit --emit lean` and the
// driver's Lean emit step, built by `@vesper/lean-extract`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/lean_extract.rs"]
mod lean_extract_binding;

// The extraction input words (#1341), shared with the root package's
// tests/self_host_lean_extract.rs.
#[path = "../../../../tests/common/lean_words.rs"]
mod lean_words;

use super::{Registry, Swap, check_words, ir_words, must, on, open};

/// Worlds whose Lean module came from Vesper.
static LEAN_EXTRACT_CALLS: AtomicU64 = AtomicU64::new(0);

/// `vesper::lean::LeanExtractTable` from `modules/lean_extract`. A world
/// the word format cannot hold (a literal wider than 64 bits) keeps Rust's
/// extraction.
const VESPER_LEAN_EXTRACT: vesper::lean::LeanExtractTable = vesper::lean::LeanExtractTable {
    extract: |world| {
        let Some(input) = lean_words::cfg_input_full(world, true, true) else {
            return vesper::lean::rust_extract(world);
        };
        LEAN_EXTRACT_CALLS.fetch_add(1, Relaxed);
        decode(&lean_extract_binding::extract_text_words(&input))
    },
};

/// The packed text of `extract_text_words` (its byte length, then the
/// bytes, eight to a word): the module, or `error: <message>\n`.
fn decode(words: &[u64]) -> Result<String, String> {
    let len = words.first().copied().unwrap_or(0) as usize;
    let mut bytes = Vec::with_capacity(len + 8);
    for w in words.iter().skip(1) {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes.truncate(len);
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    match text.strip_prefix("error: ") {
        Some(message) => Err(message.strip_suffix('\n').unwrap_or(message).to_owned()),
        None => Ok(text),
    }
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open(
    "lean_extract",
    "flip gate FAIL 1.68x with encoding, 0 diffs (scripts/ci/flip-gate/lean-extract.json, #2289)",
)];

pub(super) fn register(_r: &mut Registry) {
    if on("lean_extract") {
        // The input layout the Rust encoder writes must be the module's.
        if lean_extract_binding::format() != 1 {
            std::process::abort();
        }
        must(vesper::lean::install_lean_extract(VESPER_LEAN_EXTRACT));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: lean_extract built {} Lean modules from Vesper",
        LEAN_EXTRACT_CALLS.load(Relaxed),
    );
}
