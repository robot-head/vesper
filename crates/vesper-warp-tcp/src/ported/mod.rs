//! The Vesper build: every ported function forwards to the Vesper library
//! `tcp` (`vesper/*.ves`) through the generated binding
//! (`port/binding.rs`). Only the representation adapters live here:
//! conversions between the crate's public types and the plain-word types
//! of the export ABI, and the application of recorded congestion-controller
//! calls.

#[cfg(feature = "vesper-port")]
pub(crate) mod cc;
#[cfg(feature = "vesper-port")]
pub(crate) mod conn;
#[cfg(feature = "vesper-port")]
pub(crate) mod rtt;
#[cfg(feature = "vesper-port")]
pub(crate) mod seq;
pub(crate) mod types;
pub(crate) mod wire;

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[allow(unsafe_code, missing_docs, dead_code, clippy::all)]
#[rustfmt::skip]
#[path = "../../port/binding.rs"]
pub(crate) mod binding;
