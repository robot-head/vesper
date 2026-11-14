//! Source Tensor types (design §28.2): symbolic shape-typed tensors,
//! quantized wire-struct blocks and linear paged-KV leases.
//!
//! # Surface
//!
//! ```text
//! wire struct Q4Block32Wire { scale_fp16: u16le, packed_nibbles: ByteArray[16] }
//!
//! fn layer[M: dim, K: dim, N: dim](
//!     x: Tensor[F32, [M, K]],
//!     w: Tensor[F32, [K, N]],
//!     q: Tensor[Q4Block32Wire, [N, K]],
//! ) -> Tensor[F32, [M, N]] eff [Tensor] {
//!     Tensor::matmul(x, w)
//! }
//!
//! fn decode(q: Tensor[F32, [32, 128]], k: Tensor[Bf16, [8, 128]])
//!     -> Result[Tensor[F32, [32, 128]], KvOomError] eff [KvCachePool[8, 128]] {
//!     match KvCachePool::begin_sequence(7) {
//!         Ok(seq) => { ...; KvCachePool::release_sequence(seq); Ok(out) },
//!         Err(e) => Err(e),
//!     }
//! }
//! ```
//!
//! `Tensor[DType, [d, ...]]` is a copy tensor value. A dimension is a
//! positive decimal constant, a dimension parameter (`N: dim`, or any generic
//! used in a dimension position) or a product of those (`QHeads * HeadDim`).
//! A dtype is `F32`, `Bf16`, a quantized block wire struct, or a generic used
//! only as a dtype. `KvPageLease[KvHeads, HeadDim]` and
//! `ActiveKvSeq[KvHeads, HeadDim]` are linear. Tensor operations are the
//! operations of the built-in linear effect `Tensor`; pool operations belong
//! to the built-in linear effect `KvCachePool[KvHeads, HeadDim]`.
//!
//! # Static rules (this module)
//!
//! Dimensions are kept in a normal form (constant times a product of
//! variables), so agreement is decided syntactically: `M * K` equals `K * M`,
//! `[M, N * K]` reshapes to `[M * N, K]`, and anything not provably equal is a
//! compile-time error with a span. Inside a function its dimension
//! parameters are rigid; at a call they are inferred by unification with the
//! arguments and the expected result. Rules: matmul/matvec inner axis
//! equality, right-aligned broadcasting (equal or constant 1), reshape element
//! count, RoPE even head width, GQA `QHeads % KvHeads == 0`, quantized inner
//! axis `C % 32 == 0`, element types per operation, and pool instance
//! agreement. A requirement that cannot be decided in a generic function
//! becomes an inferred precondition of that function and is discharged at
//! every call (and, for the entry point, by the evaluator with the concrete
//! dimensions).
//!
//! Wire structs are checked by the Vesper Bridge layout checker
//! (`vesper_ffi::Registry`): explicit-endian integers, `ByteArray[N]`,
//! `ZeroPad[N]` and nested wire structs, no implicit padding. A wire struct
//! used as a tensor dtype must lay out a Q4_0 (18-byte) or Q8_0 (34-byte)
//! block.
//!
//! Leases: the core checker's linear rules make every `KvPageLease` and
//! `ActiveKvSeq` be consumed exactly once on every path (no drop on an early
//! return, no reuse after release). This module makes the pool operations
//! the only exits and entrances: a source program cannot build, destructure
//! or read the fields of a lease, a sequence or a tensor, and cannot handle
//! the `Tensor` or `KvCachePool` effects itself. The consuming exits are
//! `KvCachePool::release_page(sink page)` and
//! `KvCachePool::release_sequence(sink seq)`.
//!
//! # Lowering
//!
//! The checked module is elaborated into the core language before the core
//! checker runs: tensor types are erased to opaque handles, tensor-only
//! generics are removed, each dimension parameter becomes a leading hidden
//! `u64` parameter (`$dim_N`) and each call passes the inferred dimensions.
//! `Tensor::reshape` and `Tensor::fill` receive their target shape as a
//! `Dims` value. The core checker therefore runs unchanged on core syntax.
//! [`eval`] runs the elaborated program on the one-shot effect machine
//! (`runtime::Runtime::begin_external`) and services every tensor and pool
//! operation with the `vesper-tensor` host kernels.
//!
//! These are source typing rules checked by the compiler. They are not a
//! proof about the host kernels, and Rust ownership in the host evaluator is
//! not evidence for them.
use crate::ast::*;
use crate::check::Checked;

pub mod dim;
mod elaborate;
pub mod eval;
mod infer;
pub mod types;
pub mod wire;

pub use infer::{Analysis, FnSig, Obligation};
pub use vesper_syntax::parser::{DIM_PRODUCT, SHAPE_TYPE};

/// The tensor type and the tensor effect.
pub const TENSOR: &str = "Tensor";
/// The paged KV pool effect.
pub const KV_EFFECT: &str = "KvCachePool";
/// The linear page lease.
pub const LEASE: &str = "KvPageLease";
/// The linear active sequence.
pub const ACTIVE_SEQ: &str = "ActiveKvSeq";
/// Pool failure.
pub const KV_ERROR: &str = "KvOomError";
/// Runtime shape descriptor passed to `reshape` and `fill`.
pub const DIMS: &str = "Dims";
pub const BYTE_ARRAY: &str = "ByteArray";
pub const ZERO_PAD: &str = "ZeroPad";
/// Largest supported tensor rank (the `Dims` descriptor has four slots).
pub const MAX_RANK: usize = 4;
/// Prefix of the hidden parameter carrying a dimension.
pub const DIM_PARAM: &str = "$dim_";

/// Names the built-in prelude declares; a module that declares any of them
/// itself keeps its own declarations and gets no tensor typing.
pub const RESERVED: [&str; 8] = [
    TENSOR, KV_EFFECT, LEASE, ACTIVE_SEQ, KV_ERROR, DIMS, "F32", "Bf16",
];

/// Diagnostic codes.
pub mod codes {
    /// Malformed tensor type, unknown dimension or kind conflict.
    pub const KIND: &str = "E0761";
    /// Dimension overflow, zero dimension or rank limit.
    pub const DIM: &str = "E0762";
    /// Shape mismatch.
    pub const SHAPE: &str = "E0763";
    /// A dimension or shape cannot be inferred.
    pub const INFER: &str = "E0764";
    /// RoPE even width, GQA or quantized-block divisibility violated.
    pub const OBLIGATION: &str = "E0765";
    /// Element type mismatch.
    pub const DTYPE: &str = "E0766";
    /// Wire struct layout error or non-block dtype.
    pub const WIRE: &str = "E0767";
    /// A tensor, lease or sequence is built, destructured or projected.
    pub const OPAQUE: &str = "E0768";
    /// A handler for `Tensor` or `KvCachePool`.
    pub const HANDLER: &str = "E0769";
    /// Unsupported tensor use.
    pub const UNSUPPORTED: &str = "E0770";
    /// Pool operation on a lease or sequence of another pool instance.
    pub const POOL: &str = "E0771";
}

/// Explicit-endian integers (and `i8`) valid only as wire struct fields.
pub fn wire_integer(name: &str) -> Option<vesper_ffi::Integer> {
    use vesper_ffi::{Endian, Integer};
    if name == "i8" {
        return Some(Integer::signed(8, Endian::Native));
    }
    let (base, endian) = if let Some(b) = name.strip_suffix("le") {
        (b, Endian::Little)
    } else {
        (name.strip_suffix("be")?, Endian::Big)
    };
    let signed = match base.as_bytes().first()? {
        b'u' => false,
        b'i' => true,
        _ => return None,
    };
    let bits = match &base[1..] {
        "16" => 16,
        "32" => 32,
        "64" => 64,
        _ => return None,
    };
    Some(if signed {
        Integer::signed(bits, endian)
    } else {
        Integer::unsigned(bits, endian)
    })
}

/// Whether the module uses the built-in tensor types: it mentions a reserved
/// name, a shape literal or a wire field type, and declares none of the
/// reserved names itself.
pub fn active(module: &Module) -> bool {
    let declared = module.items.iter().any(|i| match i {
        Item::Adt(a) => RESERVED.contains(&a.name.as_str()),
        Item::Effect(e) => RESERVED.contains(&e.name.as_str()),
        _ => false,
    });
    !declared && infer::mentions_tensors(module)
}

/// Check a module that uses tensor types; `None` when it does not, so the
/// caller runs the ordinary checker unchanged.
pub fn check(module: &Module) -> Option<Checked> {
    if !active(module) {
        return None;
    }
    let analysis = analyze(module);
    Some(analysis.checked)
}

/// Full result of checking a tensor module.
pub fn analyze(module: &Module) -> Analysis {
    infer::analyze(module)
}
