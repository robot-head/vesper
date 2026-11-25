#![no_std]
#![forbid(unsafe_code)]
//! Frozen Rust seed copy of `vesper-beacon` for the compiler's own dependency
//! closure (`cargo tree -p vesper`) and `vesper-beacon` ABBA tests.

#[path = "../../src/exponential.rs"]
mod exponential;
#[path = "../../src/histogram.rs"]
mod histogram;
#[path = "../../src/redaction.rs"]
mod redaction;
#[path = "engine/spans.rs"]
mod spans;
#[path = "engine/traceparent.rs"]
mod traceparent;

pub use exponential::*;
pub use histogram::*;
pub use redaction::*;
pub use spans::*;
pub use traceparent::*;

/// Which build this is: `"rust"` for the seed copy `vesper-beacon-seed`.
pub const IMPLEMENTATION: &str = "rust";

/// Payload intentionally has no `Debug`, formatting, or telemetry encoding implementation.
/// This wrapper does not zeroize memory or prove information-flow noninterference.
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
