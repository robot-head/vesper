//! The Vesper build (`vesper-port`): the table, index, batch update, lane
//! kernels and Toeplitz hashes run in `vesper/src/*.ves`, through the
//! generated binding. These wrappers keep the caller's `BuildHasher`, the
//! ISA choice and the conversions between the crate's types and words.

// Generated and committed, kept outside src/ so formatters never reflow it
// (see docs/maintenance/porting-a-crate.md).
#[rustfmt::skip]
#[allow(unsafe_code, missing_docs, dead_code, clippy::all)]
#[path = "../../port/binding.rs"]
pub(crate) mod binding;

pub(crate) mod batch;
pub(crate) mod rss;
pub(crate) mod simd;
pub(crate) mod table;
pub(crate) mod types;
