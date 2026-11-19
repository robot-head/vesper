//! The pilot's export ABI widening library (`vesper/widen.ves`, P3b gaps
//! lane 2, #1087), Vesper build only: `f32` scalars, slices, sequences and
//! mirror fields; `in` handles lent as the caller's own words; and handles
//! that own a `ChunkSeq` of other handles (`Fabric`) or boxed trait
//! objects (`Graph`), which Rust drops whole. `tests/widen.rs` pins each
//! form end to end, with the library's live allocation count.

// Generated and committed, but kept outside src/ (see the porting recipe).
#[rustfmt::skip]
#[path = "../port/widen.rs"]
pub mod binding;
pub use binding::*;
