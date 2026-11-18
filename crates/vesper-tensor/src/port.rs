//! The port-backed build's glue (port wave P1 lane 8, #905): the generated
//! binding of the Vesper package in `vesper/`, the `Libm` shim it imports,
//! and the conversions the forwarding functions share.
//!
//! The export ABI has no `f32`, so an `f32` crosses as its bit pattern: a
//! `u32` scalar, and `&[u32]` for `&[f32]`, lent in place
//! ([`bits`], [`bits_mut`]). Every forwarding function maps the port's
//! `Fault` to the [`Error`] the Rust build returns at that call.
use crate::Error;

#[rustfmt::skip]
#[path = "../port/binding.rs"]
#[allow(unsafe_code, clippy::all)]
pub(crate) mod binding;

pub(crate) use binding::Fault;

/// The `.vsys` library `Libm` that `vesper/src/lib.ves` imports: the
/// platform `f32::exp`, so the port's softmax and SwiGLU equal the Rust
/// build's bit for bit (the Rust build documents no cross-ISA claim for
/// `exp`).
pub(crate) mod shim {
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub extern "C" fn vesper_tensor_expf(bits: u32) -> u32 {
        f32::from_bits(bits).exp().to_bits()
    }
}

pub(crate) fn bits(x: &[f32]) -> &[u32] {
    bytemuck::cast_slice(x)
}

pub(crate) fn bits_mut(x: &mut [f32]) -> &mut [u32] {
    bytemuck::cast_slice_mut(x)
}

/// A failure of a port call: checked arithmetic, an index, or a `Fault`.
#[derive(Debug)]
pub(crate) enum Failure {
    Overflow,
    Index,
    Fault(Fault),
}

pub(crate) trait IntoFailure: std::fmt::Debug {
    fn failure(self) -> Failure;
}
impl IntoFailure for vesper_port_abi::ArithOverflow {
    fn failure(self) -> Failure {
        Failure::Overflow
    }
}
impl IntoFailure for vesper_port_abi::RuntimeError {
    fn failure(self) -> Failure {
        Failure::Index
    }
}
impl IntoFailure for Fault {
    fn failure(self) -> Failure {
        Failure::Fault(self)
    }
}
macro_rules! failures {
    ($($t:ident [$($v:ident),*]),* $(,)?) => {$(
        impl IntoFailure for binding::$t {
            fn failure(self) -> Failure {
                match self {
                    $(binding::$t::$v(e) => e.failure(),)*
                }
            }
        }
    )*};
}
failures! {
    AddAssignError [RuntimeError, Fault],
    ArgmaxError [RuntimeError, Fault],
    CheckQ4KBlockError [ArithOverflow, RuntimeError],
    DecodePieceError [ArithOverflow, RuntimeError],
    DequantizeMxfp4BlockError [ArithOverflow, RuntimeError],
    DequantizeQ40BlockError [ArithOverflow, RuntimeError],
    DequantizeQ4KBlockError [ArithOverflow, RuntimeError],
    DequantizeQ80BlockError [ArithOverflow, RuntimeError],
    EncodePromptError [ArithOverflow, RuntimeError, Fault],
    FinitePayloadError [ArithOverflow, RuntimeError],
    Fp8CodesError [ArithOverflow, RuntimeError],
    Fp8ScaleError [RuntimeError, Fault],
    MakeQkx2QuantsError [ArithOverflow, RuntimeError],
    ParseGgufError [ArithOverflow, RuntimeError, Fault],
    ParseJsonError [ArithOverflow, RuntimeError],
    Q4KScaleMinError [ArithOverflow, RuntimeError],
    QuantizeMxfp4Error [ArithOverflow, RuntimeError, Fault],
    QuantizeQ40Error [ArithOverflow, RuntimeError, Fault],
    QuantizeQ4KError [ArithOverflow, RuntimeError, Fault],
    QuantizeQ4KBlockError [ArithOverflow, RuntimeError],
    QuantizeQ80Error [ArithOverflow, RuntimeError, Fault],
    RmsNormError [RuntimeError, Fault],
    SampleError [ArithOverflow, RuntimeError, Fault],
    SampleMultError [ArithOverflow, RuntimeError],
    SampleTopPError [ArithOverflow, RuntimeError],
    SoftmaxError [RuntimeError, Fault],
    SwigluError [RuntimeError, Fault],
    VocabLookupError [ArithOverflow, RuntimeError],
    VocabTableError [ArithOverflow, RuntimeError],
}

/// The value of a port call that cannot fail on its inputs: every index is
/// in bounds and no arithmetic can overflow. A failure here is a port bug,
/// and panics like the Rust build's out-of-range slice index would.
pub(crate) fn must<T, E: IntoFailure>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("vesper-tensor port: unexpected failure {e:?}"),
    }
}

/// A port result as the crate's [`Error`]: `fault` gives the Rust build's
/// error for each `Fault`.
pub(crate) fn check<T, E: IntoFailure>(
    r: Result<T, E>,
    fault: impl FnOnce(Fault) -> Error,
) -> Result<T, Error> {
    r.map_err(|e| match e.failure() {
        Failure::Fault(f) => fault(f),
        Failure::Overflow => Error::Overflow,
        Failure::Index => Error::Malformed("vesper-tensor port index out of range"),
    })
}
