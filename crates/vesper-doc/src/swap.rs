//! The renderers' swap point for the hybrid Stage 2 build (self-hosting SH5
//! lane 15, #1338).
//!
//! By default every renderer is the Rust code of this crate. The hybrid
//! build (`crates/vesper-hybrid`, module `doc`) installs a [`RenderTable`]
//! whose functions are the Vesper renderers of `packages/vesper-doc`
//! before `main` runs. Each one takes one request (little-endian `u64`
//! words and length-prefixed byte strings) and returns its result: a status
//! byte, then the output bytes or the error text. This module writes the
//! requests and reads the results, for the hybrid glue and the
//! differential tests (`tests/self_host_doc.rs`) alike.
//!
//! What crosses is JSON: the compact serialization of the value
//! (`serde_json::to_vec`), which the Vesper writer re-indents into the exact
//! bytes of `serde_json::to_writer_pretty`, plus, for a linked page, the
//! targets, comments and references the Rust link checks validated.
use std::sync::OnceLock;

/// The request and result layout version; `format()` of `@vesper/doc`.
pub const FORMAT: u64 = 1;

/// Result status bytes.
const OK: u8 = 0;
const ERR: u8 = 1;

/// The swappable renderers. Each takes a request and returns a result.
#[derive(Clone, Copy, Debug)]
pub struct RenderTable {
    /// [`crate::to_json`]: [`pretty_request`].
    pub pretty_json: fn(&[u8]) -> Vec<u8>,
    /// [`crate::render_html`] after `limits.validate()`: [`pretty_request`].
    pub static_page: fn(&[u8]) -> Vec<u8>,
    /// The page [`crate::render_document_html`] writes once its link checks
    /// pass: [`linked_request`].
    pub linked_page: fn(&[u8]) -> Vec<u8>,
}

static TABLE: OnceLock<RenderTable> = OnceLock::new();

/// Replace the Rust renderers for the rest of the process. Only the first
/// call takes effect; a later one returns its table back.
pub fn install_render(table: RenderTable) -> Result<(), RenderTable> {
    TABLE.set(table)
}

/// The installed table, if any.
pub fn table() -> Option<&'static RenderTable> {
    TABLE.get()
}

fn word(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn bytes(out: &mut Vec<u8>, b: &[u8]) {
    word(out, b.len() as u64);
    out.extend_from_slice(b);
}

/// The compact JSON of `value`, with serde_json's serializer errors worded
/// as [`crate::to_json`] words them.
pub fn compact(value: &impl serde::Serialize) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|e| format!("bounded JSON output: {e}"))
}

/// The request of `pretty_json` and `static_page`: the byte budget, then
/// the compact JSON.
pub fn pretty_request(compact: &[u8], max_bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(compact.len() + 8);
    word(&mut out, max_bytes as u64);
    out.extend_from_slice(compact);
    out
}

/// One retained comment of a linked page: its text and the source offsets
/// of its first byte and its end.
pub struct LinkedComment<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

/// One validated reference: its source span and its anchor.
pub struct LinkedReference<'a> {
    pub start: usize,
    pub end: usize,
    pub anchor: &'a str,
}

/// The request of `linked_page`: the budget, the report's compact JSON,
/// the targets (anchor id, path), the comments and the references in
/// source order.
pub fn linked_request(
    max_bytes: usize,
    compact: &[u8],
    targets: &[(String, &str)],
    comments: &[LinkedComment<'_>],
    references: &[LinkedReference<'_>],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(compact.len() + 64);
    word(&mut out, max_bytes as u64);
    bytes(&mut out, compact);
    word(&mut out, targets.len() as u64);
    for (anchor, path) in targets {
        bytes(&mut out, anchor.as_bytes());
        bytes(&mut out, path.as_bytes());
    }
    word(&mut out, comments.len() as u64);
    for c in comments {
        bytes(&mut out, c.text.as_bytes());
        word(&mut out, c.start as u64);
        word(&mut out, c.end as u64);
    }
    word(&mut out, references.len() as u64);
    for r in references {
        word(&mut out, r.start as u64);
        word(&mut out, r.end as u64);
        bytes(&mut out, r.anchor.as_bytes());
    }
    out
}

/// A renderer's result as the Rust renderer returns it.
pub fn decode(mut result: Vec<u8>) -> Result<Vec<u8>, String> {
    match result.first() {
        Some(&OK) => {
            result.remove(0);
            Ok(result)
        }
        Some(&ERR) => Err(String::from_utf8_lossy(&result[1..]).into_owned()),
        _ => Err("invalid Vesper renderer result".into()),
    }
}
