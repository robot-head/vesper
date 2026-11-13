//! §22.4 host-library boundary for telemetry values.
//!
//! Only types implementing [`Telemetrable`] can become exported attribute
//! values. It is implemented for fixed-width scalars, `&'static str`
//! vocabulary and the content-independent [`Redacted`] marker. It is
//! deliberately **not** implemented for [`Secret`], [`Pii`], `String`, `&str`
//! with a non-static lifetime, or byte slices, so a raw sensitive or
//! runtime-built value cannot be handed to an exporter by mistake. Sensitive
//! values reach telemetry only through an explicit redaction policy applied
//! by the exporter (see `vesper-beacon-arrow`), via [`Sensitive`].
//!
//! This is Rust type discipline at the library boundary. It is not the
//! Vesper source checker's `Telemetrable` coherence rule (a compiler step),
//! it does not zeroize memory, and `Box::leak` can still manufacture a
//! `&'static str` from runtime data.
//!
//! ```compile_fail
//! use vesper_beacon::{Pii, Telemetrable};
//! fn export(_: impl Telemetrable) {}
//! export(Pii::new("alice@example.com"));
//! ```
//!
//! ```compile_fail
//! use vesper_beacon::{Secret, Telemetrable};
//! fn export(_: impl Telemetrable) {}
//! export(Secret::new("bearer-token"));
//! ```
//!
//! Runtime strings are not telemetry vocabulary either:
//!
//! ```compile_fail
//! use vesper_beacon::Telemetrable;
//! fn export(_: impl Telemetrable) {}
//! let user_input = String::from("alice@example.com");
//! export(user_input.as_str());
//! ```

use crate::{Redacted, Secret};

/// A value tagged as personally identifiable information (the `@pii`
/// annotation of §22.4). Like [`Secret`], it has no `Debug`, `Display`,
/// `Clone` or [`Telemetrable`] implementation.
///
/// ```compile_fail
/// use vesper_beacon::Pii;
/// let email = Pii::new("alice@example.com");
/// let _ = format!("{email:?}");
/// ```
pub struct Pii<T>(T);
impl<T> Pii<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }
    /// Content-independent redaction: does not inspect the payload.
    pub fn redact(&self) -> Redacted {
        Redacted::marker()
    }
    /// Explicit declassification for non-telemetry uses.
    pub fn declassify(self) -> T {
        self.0
    }
}

mod sealed {
    pub trait Sealed {}
}
impl<T: AsRef<[u8]>> sealed::Sealed for Secret<T> {}
impl<T: AsRef<[u8]>> sealed::Sealed for Pii<T> {}

/// Sealed: implemented only for [`Secret`] and [`Pii`] over byte-like
/// payloads. `fingerprint_input` is an explicit declassification point that
/// exists so an exporter can apply a keyed fingerprint policy; nothing in
/// this crate formats or exports what it returns.
pub trait Sensitive: sealed::Sealed {
    fn fingerprint_input(&self) -> &[u8];
}
impl<T: AsRef<[u8]>> Sensitive for Secret<T> {
    fn fingerprint_input(&self) -> &[u8] {
        self.expose().as_ref()
    }
}
impl<T: AsRef<[u8]>> Sensitive for Pii<T> {
    fn fingerprint_input(&self) -> &[u8] {
        self.0.as_ref()
    }
}

/// The closed set of values an exporter may encode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TelemetryValue {
    Bool(bool),
    Int(i64),
    Double(f64),
    /// Static vocabulary chosen by the program, never runtime input.
    Static(&'static str),
    Redacted(Redacted),
}

/// Values that may be exported as telemetry attributes without a policy.
pub trait Telemetrable {
    fn telemetry_value(&self) -> TelemetryValue;
}
macro_rules! int_telemetrable {
    ($($t:ty),*) => {$(
        impl Telemetrable for $t {
            fn telemetry_value(&self) -> TelemetryValue {
                TelemetryValue::Int(i64::from(*self))
            }
        }
    )*};
}
int_telemetrable!(i8, i16, i32, i64, u8, u16, u32);
impl Telemetrable for bool {
    fn telemetry_value(&self) -> TelemetryValue {
        TelemetryValue::Bool(*self)
    }
}
impl Telemetrable for f64 {
    fn telemetry_value(&self) -> TelemetryValue {
        TelemetryValue::Double(*self)
    }
}
impl Telemetrable for &'static str {
    fn telemetry_value(&self) -> TelemetryValue {
        TelemetryValue::Static(self)
    }
}
impl Telemetrable for Redacted {
    fn telemetry_value(&self) -> TelemetryValue {
        TelemetryValue::Redacted(*self)
    }
}
