//! Bounded synchronous LSP framing and transactional, in-memory documents.
//!
//! This crate performs no filesystem access or compiler execution. URI labels
//! carry no filesystem authority. See the README for the explicit JSON-RPC
//! profile, resource accounting and stream error policy. The [`lens`] module
//! ranks and records caller-supplied facts only.
//!
//! ```
//! use vesper_lsp::{DocumentStore, StoreLimits, ContentChange};
//! let mut store = DocumentStore::new(StoreLimits::default())?;
//! store.open("untitled:demo", 1, "hello")?;
//! store.change("untitled:demo", 2, &[ContentChange {
//!     range: None, range_length: None, text: "updated",
//! }])?;
//! assert_eq!(store.get("untitled:demo").unwrap().text(), "updated");
//! # Ok::<(), vesper_lsp::StoreError>(())
//! ```
#![forbid(unsafe_code)]
mod document;
pub mod lens;
mod transport;
pub use document::{ContentChange, Document, DocumentStore, StoreError, StoreLimits};
pub use transport::{
    Envelope, Id, ProtocolError, ProtocolErrorKind, ProtocolLimits, ResponsePayload, encode_frame,
    parse_envelope, read_frame, read_message, write_message,
};
pub use vesper_span::{ByteSpan, LineIndex, Position, Range};
