//! The pilot's callback-handle library (`vesper/fnptrs.ves`, #1502),
//! Vesper build only: a copy struct holding `.vsys` callback handles (C
//! function pointers the library looked up through [`shim`]) crosses the
//! export ABI as opaque [`vesper_port_abi::FnPtr`] words, and Rust hands
//! them back to be called. `tests/fnptrs.rs` pins each form end to end.

pub mod shim;

// Generated and committed, but kept outside src/ (see the porting recipe).
#[rustfmt::skip]
#[path = "../port/fnptrs.rs"]
pub mod binding;
pub use binding::*;
