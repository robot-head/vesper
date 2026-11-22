//! The implementation: thin adapters that keep the crate's Rust API and
//! forward it to the five Vesper libraries in `vesper/` through the generated
//! bindings in `port/` (port wave P1 lane 14, #911; the only build since SH6
//! lane 5, #1400).
//!
//! The adapters hold only what the export ABI cannot carry: byte buffers that
//! the API lends out as `&[u8]`, the generic route payloads `T`, `String`
//! conversions and the `io::Write` call. Every decision is made in Vesper.

pub(crate) mod connection;
pub(crate) mod decoder;
pub(crate) mod response;
pub(crate) mod router;
pub(crate) mod target;
