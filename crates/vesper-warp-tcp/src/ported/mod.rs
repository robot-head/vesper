//! The Vesper build: every ported function forwards to the Vesper library
//! `tcp` (`vesper/*.ves`) through the generated binding
//! (`port/binding.rs`). Only the representation adapters live here:
//! conversions between the crate's public types and the plain-word types
//! of the export ABI, and the application of recorded congestion-controller
//! calls.

pub(crate) mod cc;
pub(crate) mod conn;
pub(crate) mod rtt;
pub(crate) mod seq;
pub(crate) mod types;
pub(crate) mod wire;

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[allow(unsafe_code, missing_docs, clippy::all)]
#[rustfmt::skip]
#[path = "../../port/binding.rs"]
pub(crate) mod binding;
