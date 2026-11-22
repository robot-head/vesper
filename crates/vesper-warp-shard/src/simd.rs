//! Sixteen-lane kernels with runtime ISA dispatch.
//!
//! This is the only module of the crate allowed to use `unsafe`. Every
//! `unsafe` block is one of two kinds:
//!
//! 1. An unaligned vector load or store from a `&[T; 16]` / `&mut [T; 16]`
//!    array at a constant lane offset that keeps the whole vector inside the
//!    array. The offsets are written out per call so each bound can be read
//!    against the array length (16 lanes).
//! 2. A call into a `#[target_feature]` function. Those calls happen only
//!    through [`Kernels`], whose private `path` field is set solely by
//!    [`Kernels::new`] after [`IsaPath::is_supported`] confirmed the feature
//!    on the running CPU, so the feature precondition holds.
//!
//! The scalar implementations in [`scalar`] are the reference semantics; the
//! test suite checks every supported path against them.

#![allow(unsafe_code)]

use core::fmt;

/// Number of connection lanes per tile.
pub const LANES: usize = 16;

/// Instruction-set path used by the tile kernels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IsaPath {
    /// Portable scalar reference loops.
    Scalar,
    /// x86_64 SSE2 (128-bit) intrinsics.
    Sse2,
    /// x86_64 AVX2 (256-bit) intrinsics.
    Avx2,
    /// AArch64 Advanced SIMD (NEON, 128-bit) intrinsics.
    Neon,
}

impl IsaPath {
    /// All paths in preference order (best last).
    pub const ALL: [IsaPath; 4] = [IsaPath::Scalar, IsaPath::Sse2, IsaPath::Neon, IsaPath::Avx2];

    /// Whether this path can run on the current CPU (runtime detection).
    pub fn is_supported(self) -> bool {
        match self {
            IsaPath::Scalar => true,
            IsaPath::Sse2 => x86_feature_sse2(),
            IsaPath::Avx2 => x86_feature_avx2(),
            IsaPath::Neon => aarch64_feature_neon(),
        }
    }

    /// The widest supported path on the current CPU.
    pub fn detect() -> IsaPath {
        IsaPath::ALL
            .iter()
            .rev()
            .copied()
            .find(|p| p.is_supported())
            .unwrap_or(IsaPath::Scalar)
    }

    /// Stable lowercase name used in reports.
    pub fn name(self) -> &'static str {
        match self {
            IsaPath::Scalar => "scalar",
            IsaPath::Sse2 => "sse2",
            IsaPath::Avx2 => "avx2",
            IsaPath::Neon => "neon",
        }
    }
}

impl fmt::Display for IsaPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(target_arch = "x86_64")]
fn x86_feature_sse2() -> bool {
    std::arch::is_x86_feature_detected!("sse2")
}
#[cfg(not(target_arch = "x86_64"))]
fn x86_feature_sse2() -> bool {
    false
}
#[cfg(target_arch = "x86_64")]
fn x86_feature_avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}
#[cfg(not(target_arch = "x86_64"))]
fn x86_feature_avx2() -> bool {
    false
}
#[cfg(target_arch = "aarch64")]
fn aarch64_feature_neon() -> bool {
    std::arch::is_aarch64_feature_detected!("neon")
}
#[cfg(not(target_arch = "aarch64"))]
fn aarch64_feature_neon() -> bool {
    false
}

/// The requested ISA path is not available on this CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedIsa(pub IsaPath);

impl fmt::Display for UnsupportedIsa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ISA path {} is not supported on this CPU", self.0)
    }
}

impl std::error::Error for UnsupportedIsa {}

/// A validated kernel dispatcher.
///
/// Invariant: `path.is_supported()` was true when this value was built. The
/// field is private, so the only constructors are [`Kernels::new`],
/// [`Kernels::detect`] and [`Kernels::scalar`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kernels {
    path: IsaPath,
}

impl Kernels {
    /// Builds a dispatcher for `path`, or reports that the CPU lacks it.
    pub fn new(path: IsaPath) -> Result<Kernels, UnsupportedIsa> {
        if path.is_supported() {
            Ok(Kernels { path })
        } else {
            Err(UnsupportedIsa(path))
        }
    }

    /// Dispatcher for the widest supported path.
    pub fn detect() -> Kernels {
        Kernels {
            path: IsaPath::detect(),
        }
    }

    /// The scalar reference dispatcher.
    pub const fn scalar() -> Kernels {
        Kernels {
            path: IsaPath::Scalar,
        }
    }

    /// The path this dispatcher executes.
    pub fn path(self) -> IsaPath {
        self.path
    }
}

#[cfg(feature = "vesper-port")]
pub use crate::port::simd::scalar;
#[cfg(not(feature = "vesper-port"))]
pub use crate::reference::simd::scalar;
