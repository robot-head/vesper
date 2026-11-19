//! The pilot's fixed-size array library (`vesper/arrays.ves`, P3b gaps
//! lane 6 part 2, #1091), Vesper build only: copy structs with `[T; N]`
//! fields lent in place through their mirrors, whose array fields are
//! their element cells in order, and whole arrays as parameters and
//! results. `tests/arrays.rs` pins each form end to end.

// Generated and committed, but kept outside src/ (see the porting recipe).
#[rustfmt::skip]
#[path = "../port/arrays.rs"]
pub mod binding;
pub use binding::*;
