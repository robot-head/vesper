//! Length-agnostic strip-mined predication: design §14.3 `VVec[P, T]` and
//! `VMask[P, T]` for `u8` lanes, as a host library.
//!
//! A profile `P` ([`Scalar`], [`Sve`], [`Rvv`]) fixes the number of `u8` lanes
//! one vector holds. [`IsaProfile::active_mask`] is the governing predicate of
//! a strip (`min(lanes, total_len - offset)` leading lanes); masked loads and
//! stores touch only active lanes, so a loop over any length needs no scalar
//! remainder. Every operation has three implementations with one meaning:
//!
//! * [`Scalar`] is the portable reference, with any lane count from 1 to
//!   [`MAX_LANES`], so strip-mining at every vector length (including ones
//!   no hardware has) is testable on any host.
//! * [`Sve`] is AArch64 SVE: `whilelo` predicates, `ld1b`/`st1b` with a
//!   governing predicate, `cmpeq`/`cmplo` into predicate registers, `eorv`.
//! * [`Rvv`] is RISC-V Vector 1.0: `vsetvli` at `e8, m1`, `vle8.v`/`vse8.v`
//!   under a `v0.t` mask, `vmseq`/`vmsltu`, `vredxor.vs`.
//!
//! Rust's SVE and RVV intrinsics are unstable, so both hardware paths are
//! stable `asm!` blocks confined to `vvec/sve.rs` and `vvec/rvv.rs`; each
//! states its register contract there. Vector values live in memory between
//! operations (a scalable register cannot be an `asm!` operand), so per-op
//! calls pay a load and a store: [`kernels`] runs whole loops in one block.
//!
//! Masks are plain bit sets (bit `i` is lane `i`), the same layout SVE's
//! `str p` and RVV's `vsm.v` write; mask logic is ordinary integer code.
//!
//! ```
//! use vesper_simd::vvec::{IsaProfile, Scalar, VVec, scan_record_delimiters};
//!
//! let p = Scalar::new(48).unwrap();
//! let data = b"a,b,c";
//! let mask = p.active_mask(0, data.len());
//! let v = VVec::load_masked(data, 0, &mask, 0).unwrap();
//! assert_eq!(v.simd_eq(&VVec::splat(p, b',')).and(mask).count_ones(), 2);
//! let xor = data.iter().fold(0, |a, b| a ^ b);
//! assert_eq!(scan_record_delimiters(p, data, b','), (2, xor));
//! ```
//!
//! The source-level `eff [Isa[P]]` effect and `VVec` syntax are compiler work
//! this crate does not do: here the profile is a run-time token whose private
//! fields can only be filled after detection.
use std::fmt;
use std::marker::PhantomData;

/// Run the ISA-specific arm when this build targets that architecture and
/// the profile type is the matching one; otherwise the scalar arm. A hardware
/// profile cannot be constructed on another architecture, so the fallback
/// only keeps the match exhaustive.
macro_rules! dispatch {
    ($isa:expr, sve => $sve:expr, rvv => $rvv:expr, scalar => $scalar:expr $(,)?) => {{
        match $isa {
            #[cfg(target_arch = "aarch64")]
            // SAFETY: a `Sve` profile exists only after SVE detection, and
            // every argument respects the contract in `vvec/sve.rs`.
            $crate::vvec::VectorIsa::Sve => unsafe { $sve },
            #[cfg(target_arch = "riscv64")]
            // SAFETY: an `Rvv` profile exists only after V detection, and
            // every argument respects the contract in `vvec/rvv.rs`.
            $crate::vvec::VectorIsa::Rvv => unsafe { $rvv },
            _ => $scalar,
        }
    }};
}

pub mod kernels;
#[cfg(target_arch = "riscv64")]
mod rvv;
#[cfg(target_arch = "aarch64")]
mod sve;

/// The widest vector, in `u8` lanes, a profile may have: 2048 bits, the
/// architectural SVE maximum. RVV allows wider registers; such hosts get
/// [`VvecError::VectorTooLong`] from [`Rvv::detect`] (the [`kernels`] work at
/// any width).
pub const MAX_LANES: usize = 256;
const MASK_WORDS: usize = MAX_LANES / 64;

/// The instruction set a profile runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VectorIsa {
    /// Portable Rust, any lane count.
    Scalar,
    /// AArch64 Scalable Vector Extension (SVE; SVE2 hardware also qualifies).
    Sve,
    /// RISC-V Vector extension 1.0.
    Rvv,
}

impl VectorIsa {
    pub const ALL: [VectorIsa; 3] = [VectorIsa::Scalar, VectorIsa::Sve, VectorIsa::Rvv];

    /// Whether this process can run the ISA on the current CPU.
    pub fn is_available(self) -> bool {
        match self {
            VectorIsa::Scalar => true,
            VectorIsa::Sve => sve_available(),
            VectorIsa::Rvv => rvv_available(),
        }
    }

    /// The current thread's hardware vector length in bytes, if the ISA is
    /// available. SVE and RVV lengths are fixed by the CPU (or, for SVE, the
    /// thread's `prctl` setting); [`VectorIsa::Scalar`] has none.
    pub fn hardware_vector_bytes(self) -> Option<usize> {
        if !self.is_available() {
            return None;
        }
        match self {
            VectorIsa::Scalar => None,
            #[cfg(target_arch = "aarch64")]
            // SAFETY: SVE was detected just above.
            VectorIsa::Sve => Some(unsafe { sve::vector_bytes() }),
            #[cfg(target_arch = "riscv64")]
            // SAFETY: V was detected just above.
            VectorIsa::Rvv => Some(unsafe { rvv::vector_bytes() }),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            VectorIsa::Scalar => "scalar",
            VectorIsa::Sve => "sve",
            VectorIsa::Rvv => "rvv1.0",
        }
    }
}

fn sve_available() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("sve")
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        false
    }
}

fn rvv_available() -> bool {
    #[cfg(target_arch = "riscv64")]
    {
        rvv::detected()
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        false
    }
}

/// Typed failures. Out-of-bounds lanes are errors, never partial work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VvecError {
    /// The CPU (or this build's target architecture) lacks the ISA.
    Unavailable(VectorIsa),
    /// The hardware vector is wider than [`MAX_LANES`] bytes.
    VectorTooLong { isa: VectorIsa, bytes: usize },
    /// A scalar profile needs between 1 and [`MAX_LANES`] lanes.
    InvalidLanes(usize),
    /// An active lane would read or write past the slice.
    OutOfBounds {
        offset: usize,
        lane: usize,
        len: usize,
    },
}

impl fmt::Display for VvecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(isa) => write!(f, "{} is not available here", isa.name()),
            Self::VectorTooLong { isa, bytes } => write!(
                f,
                "{} vectors are {bytes} bytes, above the {MAX_LANES}-byte limit",
                isa.name()
            ),
            Self::InvalidLanes(n) => write!(f, "{n} lanes is outside 1..={MAX_LANES}"),
            Self::OutOfBounds { offset, lane, len } => write!(
                f,
                "active lane {lane} at offset {offset} is outside a {len}-byte slice"
            ),
        }
    }
}

impl std::error::Error for VvecError {}

mod sealed {
    pub trait Sealed {}
}

/// A vector profile: an ISA and the number of `u8` lanes per vector.
///
/// Implementations are sealed; values are obtained only by detection
/// ([`Sve::detect`], [`Rvv::detect`]) or validation ([`Scalar::new`]).
pub trait IsaProfile: Copy + fmt::Debug + Eq + sealed::Sealed {
    const ISA: VectorIsa;

    /// `u8` lanes per vector (the vector length in bytes).
    fn lanes(&self) -> usize;

    /// The governing predicate for the strip at `offset` of a `total_len`
    /// stream: lanes `0..min(lanes, total_len - offset)` are active, none
    /// when `offset >= total_len` (SVE `whilelo`, RVV `vid.v`+`vmsltu`).
    fn active_mask(&self, offset: usize, total_len: usize) -> VMask<Self> {
        let n = self.lanes().min(total_len.saturating_sub(offset));
        let mut bits = [0u64; MASK_WORDS];
        dispatch!(Self::ISA,
            sve => sve::while_lt(n, &mut bits),
            rvv => rvv::while_lt(n, self.lanes(), &mut bits),
            scalar => prefix_bits(n, &mut bits),
        );
        VMask {
            profile: *self,
            bits,
        }
    }

    /// Active lanes of `mask`: how far a strip-mined loop advances.
    fn count(&self, mask: &VMask<Self>) -> usize {
        mask.count_ones()
    }
}

fn prefix_bits(n: usize, bits: &mut [u64; MASK_WORDS]) {
    for (i, w) in bits.iter_mut().enumerate() {
        let lo = i * 64;
        *w = if n >= lo + 64 {
            u64::MAX
        } else if n > lo {
            (1u64 << (n - lo)) - 1
        } else {
            0
        };
    }
}

/// The portable reference profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scalar {
    lanes: usize,
}

impl Scalar {
    /// A scalar profile with `lanes` lanes, `1..=MAX_LANES` (not only powers
    /// of two: SVE allows any multiple of 16 bytes).
    pub fn new(lanes: usize) -> Result<Self, VvecError> {
        if (1..=MAX_LANES).contains(&lanes) {
            Ok(Self { lanes })
        } else {
            Err(VvecError::InvalidLanes(lanes))
        }
    }
}

impl sealed::Sealed for Scalar {}
impl IsaProfile for Scalar {
    const ISA: VectorIsa = VectorIsa::Scalar;
    fn lanes(&self) -> usize {
        self.lanes
    }
}

/// Evidence that this thread runs SVE with `lanes` bytes per vector.
///
/// The SVE vector length is per-thread state on Linux (`PR_SVE_SET_VL`), so
/// the token is `!Send` and `!Sync`. If the thread changes its vector length
/// afterwards, results are wrong but memory safety holds: every access is
/// bounded by a predicate over at most `lanes` lanes of a `MAX_LANES` buffer.
///
/// ```compile_fail
/// let forged = vesper_simd::vvec::Sve { lanes: 64, _thread: std::marker::PhantomData };
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sve {
    lanes: usize,
    _thread: PhantomData<*const ()>,
}

impl Sve {
    pub fn detect() -> Result<Self, VvecError> {
        let bytes = VectorIsa::Sve
            .hardware_vector_bytes()
            .ok_or(VvecError::Unavailable(VectorIsa::Sve))?;
        if bytes > MAX_LANES {
            return Err(VvecError::VectorTooLong {
                isa: VectorIsa::Sve,
                bytes,
            });
        }
        Ok(Self {
            lanes: bytes,
            _thread: PhantomData,
        })
    }
}

impl sealed::Sealed for Sve {}
impl IsaProfile for Sve {
    const ISA: VectorIsa = VectorIsa::Sve;
    fn lanes(&self) -> usize {
        self.lanes
    }
}

/// Evidence that this hart runs RVV 1.0 with `VLEN / 8 = lanes` bytes per
/// vector register (`e8`, `m1`). `!Send`, like [`Sve`], for symmetry: VLEN
/// is fixed per hart, but the token is local evidence, not a portability
/// claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rvv {
    lanes: usize,
    _thread: PhantomData<*const ()>,
}

impl Rvv {
    pub fn detect() -> Result<Self, VvecError> {
        let bytes = VectorIsa::Rvv
            .hardware_vector_bytes()
            .ok_or(VvecError::Unavailable(VectorIsa::Rvv))?;
        if bytes > MAX_LANES {
            return Err(VvecError::VectorTooLong {
                isa: VectorIsa::Rvv,
                bytes,
            });
        }
        Ok(Self {
            lanes: bytes,
            _thread: PhantomData,
        })
    }
}

impl sealed::Sealed for Rvv {}
impl IsaProfile for Rvv {
    const ISA: VectorIsa = VectorIsa::Rvv;
    fn lanes(&self) -> usize {
        self.lanes
    }
}

/// A predicate over the lanes of profile `P`. Bits at or above
/// `P::lanes()` are always clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VMask<P: IsaProfile> {
    profile: P,
    bits: [u64; MASK_WORDS],
}

impl<P: IsaProfile> VMask<P> {
    pub fn profile(&self) -> P {
        self.profile
    }
    /// No lanes active.
    pub fn none(profile: P) -> Self {
        Self {
            profile,
            bits: [0; MASK_WORDS],
        }
    }
    /// Every lane active.
    pub fn all(profile: P) -> Self {
        profile.active_mask(0, profile.lanes())
    }
    /// The mask's bits, lane `i` at bit `i % 64` of word `i / 64`.
    pub fn words(&self) -> [u64; MASK_WORDS] {
        self.bits
    }
    /// Lanes 0..64 as a bitmask (the §20.3 `to_bitmask64`).
    pub fn to_bitmask64(&self) -> u64 {
        self.bits[0]
    }
    pub fn test(&self, lane: usize) -> bool {
        lane < MAX_LANES && self.bits[lane / 64] >> (lane % 64) & 1 != 0
    }
    pub fn count_ones(&self) -> usize {
        self.bits.iter().map(|w| w.count_ones() as usize).sum()
    }
    pub fn any(&self) -> bool {
        self.bits.iter().any(|w| *w != 0)
    }
    pub fn first_active(&self) -> Option<usize> {
        self.bits
            .iter()
            .enumerate()
            .find(|(_, w)| **w != 0)
            .map(|(i, w)| i * 64 + w.trailing_zeros() as usize)
    }
    pub fn last_active(&self) -> Option<usize> {
        self.bits
            .iter()
            .enumerate()
            .rev()
            .find(|(_, w)| **w != 0)
            .map(|(i, w)| i * 64 + 63 - w.leading_zeros() as usize)
    }
    fn zip(self, other: Self, f: impl Fn(u64, u64) -> u64) -> Self {
        let mut bits = self.bits;
        for (a, b) in bits.iter_mut().zip(other.bits) {
            *a = f(*a, b);
        }
        Self { bits, ..self }
    }
    pub fn and(self, other: Self) -> Self {
        self.zip(other, |a, b| a & b)
    }
    pub fn or(self, other: Self) -> Self {
        self.zip(other, |a, b| a | b)
    }
    pub fn xor(self, other: Self) -> Self {
        self.zip(other, |a, b| a ^ b)
    }
    pub fn and_not(self, other: Self) -> Self {
        self.zip(other, |a, b| a & !b)
    }
}

/// Complement within the profile's lanes.
impl<P: IsaProfile> std::ops::Not for VMask<P> {
    type Output = Self;
    fn not(self) -> Self {
        Self::all(self.profile).and_not(self)
    }
}

/// One vector of `u8` lanes. Lanes at or above `P::lanes()` are zero.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VVec<P: IsaProfile> {
    profile: P,
    lanes: [u8; MAX_LANES],
}

impl<P: IsaProfile> fmt::Debug for VVec<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VVec")
            .field("profile", &self.profile)
            .field("lanes", &self.as_slice())
            .finish()
    }
}

/// Check that every active lane of `mask` at `offset` is inside `len`.
fn check_bounds<P: IsaProfile>(
    mask: &VMask<P>,
    offset: usize,
    len: usize,
) -> Result<(), VvecError> {
    if let Some(lane) = mask.last_active() {
        match offset.checked_add(lane) {
            Some(end) if end < len => {}
            _ => return Err(VvecError::OutOfBounds { offset, lane, len }),
        }
    }
    Ok(())
}

impl<P: IsaProfile> VVec<P> {
    pub fn profile(&self) -> P {
        self.profile
    }

    /// The profile's lanes.
    pub fn as_slice(&self) -> &[u8] {
        &self.lanes[..self.profile.lanes()]
    }

    /// Every lane `value` (SVE `dup`, RVV `vmv.v.x`).
    pub fn splat(profile: P, value: u8) -> Self {
        let mut lanes = [0u8; MAX_LANES];
        let n = profile.lanes();
        dispatch!(P::ISA,
            sve => sve::splat(value, n, &mut lanes),
            rvv => rvv::splat(value, n, &mut lanes),
            scalar => lanes[..n].fill(value),
        );
        Self { profile, lanes }
    }

    /// Lane `i` is `data[offset + i]` where `mask` is active and `passthru`
    /// elsewhere. Inactive lanes are never read (SVE `ld1b` under `p0/z`,
    /// RVV `vle8.v` under `v0.t`), so the strip may extend past the slice;
    /// an *active* lane past it is [`VvecError::OutOfBounds`].
    pub fn load_masked(
        data: &[u8],
        offset: usize,
        mask: &VMask<P>,
        passthru: u8,
    ) -> Result<Self, VvecError> {
        check_bounds(mask, offset, data.len())?;
        let profile = mask.profile;
        let n = profile.lanes();
        let mut lanes = [0u8; MAX_LANES];
        if !mask.any() {
            lanes[..n].fill(passthru);
            return Ok(Self { profile, lanes });
        }
        // In bounds: `offset + last_active < len`, so `offset < len`.
        let src = &data[offset..];
        dispatch!(P::ISA,
            sve => sve::load_masked(src.as_ptr(), &mask.bits, passthru, n, &mut lanes),
            rvv => rvv::load_masked(src.as_ptr(), &mask.bits, passthru, n, &mut lanes),
            scalar => {
                for (i, lane) in lanes[..n].iter_mut().enumerate() {
                    *lane = if mask.test(i) { src[i] } else { passthru };
                }
            },
        );
        Ok(Self { profile, lanes })
    }

    /// Write the active lanes to `out[offset + i]`; inactive bytes are left
    /// untouched (SVE `st1b`, RVV `vse8.v` under `v0.t`).
    pub fn store_masked(
        &self,
        out: &mut [u8],
        offset: usize,
        mask: &VMask<P>,
    ) -> Result<(), VvecError> {
        check_bounds(mask, offset, out.len())?;
        if !mask.any() {
            return Ok(());
        }
        let n = self.profile.lanes();
        let dst = &mut out[offset..];
        dispatch!(P::ISA,
            sve => sve::store_masked(dst.as_mut_ptr(), &mask.bits, n, &self.lanes),
            rvv => rvv::store_masked(dst.as_mut_ptr(), &mask.bits, n, &self.lanes),
            scalar => {
                for (i, lane) in self.lanes[..n].iter().enumerate() {
                    if mask.test(i) {
                        dst[i] = *lane;
                    }
                }
            },
        );
        Ok(())
    }

    fn compare(&self, other: &Self, op: CmpOp) -> VMask<P> {
        let n = self.profile.lanes();
        let mut bits = [0u64; MASK_WORDS];
        dispatch!(P::ISA,
            sve => sve::compare(op, n, &self.lanes, &other.lanes, &mut bits),
            rvv => rvv::compare(op, n, &self.lanes, &other.lanes, &mut bits),
            scalar => {
                for i in 0..n {
                    if op.eval(self.lanes[i], other.lanes[i]) {
                        bits[i / 64] |= 1 << (i % 64);
                    }
                }
            },
        );
        VMask {
            profile: self.profile,
            bits,
        }
    }

    /// Lanes equal to `other`'s.
    pub fn simd_eq(&self, other: &Self) -> VMask<P> {
        self.compare(other, CmpOp::Eq)
    }

    /// Lanes unsigned-less-than `other`'s.
    pub fn simd_lt(&self, other: &Self) -> VMask<P> {
        self.compare(other, CmpOp::LtU)
    }

    fn binary(&self, other: &Self, op: BinOp) -> Self {
        let n = self.profile.lanes();
        let mut lanes = [0u8; MAX_LANES];
        dispatch!(P::ISA,
            sve => sve::binary(op, n, &self.lanes, &other.lanes, &mut lanes),
            rvv => rvv::binary(op, n, &self.lanes, &other.lanes, &mut lanes),
            scalar => {
                for i in 0..n {
                    lanes[i] = op.eval(self.lanes[i], other.lanes[i]);
                }
            },
        );
        Self {
            profile: self.profile,
            lanes,
        }
    }

    pub fn bit_xor(&self, other: &Self) -> Self {
        self.binary(other, BinOp::Xor)
    }
    pub fn bit_and(&self, other: &Self) -> Self {
        self.binary(other, BinOp::And)
    }
    pub fn bit_or(&self, other: &Self) -> Self {
        self.binary(other, BinOp::Or)
    }
    pub fn wrapping_add(&self, other: &Self) -> Self {
        self.binary(other, BinOp::Add)
    }

    /// XOR of every lane (SVE `eorv`, RVV `vredxor.vs`).
    pub fn reduce_xor(&self) -> u8 {
        let n = self.profile.lanes();
        dispatch!(P::ISA,
            sve => sve::reduce_xor(n, &self.lanes),
            rvv => rvv::reduce_xor(n, &self.lanes),
            scalar => self.lanes[..n].iter().fold(0, |a, b| a ^ b),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code, reason = "each op is used by some backend")]
enum CmpOp {
    Eq,
    LtU,
}
impl CmpOp {
    fn eval(self, a: u8, b: u8) -> bool {
        match self {
            CmpOp::Eq => a == b,
            CmpOp::LtU => a < b,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinOp {
    Xor,
    And,
    Or,
    Add,
}
impl BinOp {
    fn eval(self, a: u8, b: u8) -> u8 {
        match self {
            BinOp::Xor => a ^ b,
            BinOp::And => a & b,
            BinOp::Or => a | b,
            BinOp::Add => a.wrapping_add(b),
        }
    }
}

/// Design §14.3's `scan_record_delimiters`, written against the `VVec` API
/// exactly as the design writes it: one governing predicate per strip, no
/// scalar remainder. Returns `(matches, xor)`.
pub fn scan_record_delimiters<P: IsaProfile>(profile: P, data: &[u8], delim: u8) -> (u64, u8) {
    let needle = VVec::splat(profile, delim);
    let mut matches_count: u64 = 0;
    let mut xor_acc = VVec::splat(profile, 0);
    let mut idx: usize = 0;
    let len = data.len();
    while idx != len {
        let mask = profile.active_mask(idx, len);
        // `active_mask(idx, len)` never activates a lane at or past `len`.
        let Ok(chunk) = VVec::load_masked(data, idx, &mask, 0) else {
            break;
        };
        let eq_mask = chunk.simd_eq(&needle).and(mask);
        matches_count = matches_count.wrapping_add(eq_mask.count_ones() as u64);
        xor_acc = xor_acc.bit_xor(&chunk); // inactive lanes zeroed by load_masked
        idx = idx.wrapping_add(profile.count(&mask));
    }
    (matches_count, xor_acc.reduce_xor())
}
