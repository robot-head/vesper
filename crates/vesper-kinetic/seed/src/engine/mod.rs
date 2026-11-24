//! The Rust implementations the Vesper port replaced in the package
//! vesper-kinetic, frozen for the compiler's closure (bug fixes only until
//! self-hosting Stage 2; SH6 lane 9, #1404).

pub(crate) mod coloring;
pub(crate) mod crmath;
pub(crate) mod fastmath;
// `!(x > y)` is deliberate: it is also true when `x` is NaN.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub(crate) mod gjk;
pub(crate) mod lbvh;
pub(crate) mod morton;
pub(crate) mod sap;
