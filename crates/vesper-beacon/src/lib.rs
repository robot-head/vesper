// The seed build (the compiler's copy) stays no_std and safe. The
// port-backed build links the Vesper port through the generated binding,
// which allocates its failure record and calls the C ABI.
#![cfg_attr(not(vesper_port), no_std)]
#![cfg_attr(not(vesper_port), forbid(unsafe_code))]
#![cfg_attr(vesper_port, deny(unsafe_code))]
//! Bounded host-library foundations for Beacon, independent of compiler/runtime effects.
//!
//! No TLS, clocks, allocator, transport, Arrow/OTLP encoding, or performance guarantee.
//! Histograms: [`FastLogHistogram`] (log-linear bins) and [`ExponentialHistogram`]
//! (the OTLP base-2 exponential layout), both fixed-size.
//! `Telemetrable`/`Sensitive` mark the §22.4 export boundary (see `redaction`).
//! A forgotten Rust span remains visible as an occupied pool slot; Rust does not enforce
//! Vesper's total linear exhaustion. Context inheritance is always an explicit argument.

mod exponential;
mod histogram;
mod redaction;
mod spans;
mod traceparent;
pub use exponential::*;
pub use histogram::*;
pub use redaction::*;
pub use spans::*;
pub use traceparent::*;

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[cfg(vesper_port)]
#[allow(unsafe_code)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;

/// Which build this is: `"vesper"` for the port-backed package
/// `vesper-beacon`, `"rust"` for the seed copy `vesper-beacon-seed` that the
/// compiler links.
pub const IMPLEMENTATION: &str = if cfg!(vesper_port) { "vesper" } else { "rust" };

/// Payload intentionally has no `Debug`, formatting, or telemetry encoding implementation.
/// This wrapper does not zeroize memory or prove information-flow noninterference.
///
/// ```compile_fail
/// use vesper_beacon::Secret;
/// let credential = Secret::new("credential");
/// let _ = format!("{credential:?}");
/// ```
/// A raw secret cannot be supplied as a typed telemetry attribute:
///
/// ```compile_fail
/// use vesper_beacon::{PrimaryAttribute, Secret};
/// let credential = Secret::new(123_u64);
/// let _ = PrimaryAttribute::ItemCount(credential);
/// ```
pub struct Secret<T>(T);
impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }
    /// Explicit content-independent redaction. Does not hash or inspect the payload.
    pub fn redact(&self) -> Redacted {
        Redacted::marker()
    }
    pub(crate) fn expose(&self) -> &T {
        &self.0
    }
    /// Explicit declassification for non-telemetry uses.
    pub fn declassify(self) -> T {
        self.0
    }
}
/// A content-independent marker; only `Secret::redact` and `Pii::redact` construct it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Redacted(());
impl Redacted {
    pub(crate) const fn marker() -> Self {
        Self(())
    }
    pub const fn as_bytes(self) -> &'static [u8] {
        b"[REDACTED]"
    }
}
