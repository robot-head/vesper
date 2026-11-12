#![no_std]
#![forbid(unsafe_code)]
//! Bounded host-library foundations for Beacon, independent of compiler/runtime effects.
//!
//! No TLS, clocks, allocator, transport, Arrow/OTLP encoding, or performance guarantee.
//! A forgotten Rust span remains visible as an occupied pool slot; Rust does not enforce
//! Vesper's total linear exhaustion. Context inheritance is always an explicit argument.

mod histogram;
mod spans;
mod traceparent;
pub use histogram::*;
pub use spans::*;
pub use traceparent::*;

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
        Redacted(())
    }
    /// Explicit declassification for non-telemetry uses.
    pub fn declassify(self) -> T {
        self.0
    }
}
/// A content-independent marker; only `Secret::redact` constructs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Redacted(());
impl Redacted {
    pub const fn as_bytes(self) -> &'static [u8] {
        b"[REDACTED]"
    }
}
