//! AArch64 SVE backend: stable `asm!`, entered only after
//! `is_aarch64_feature_detected!("sve")`.
//!
//! Register contract shared by every block here:
//!
//! * Inputs are general registers chosen by the compiler. Vector values
//!   arrive and leave through memory (`[u8; MAX_LANES]` buffers, `[u64; 4]`
//!   masks); nothing scalable crosses the `asm!` boundary.
//! * Scratch: `z0`–`z7` and `z16`–`z20` (declared as `v` clobbers: the
//!   compiler only ever holds the low 128 bits there, and the high bits are
//!   caller-saved under AAPCS64), predicates `p0`–`p3` (declared `preg`
//!   clobbers), and the condition flags (`whilelo` sets them).
//! * No stack use. `z8`–`z15`, whose low halves are callee-saved, are never
//!   touched.
//!
//! Memory contract: a buffer is accessed only under a predicate built by
//! `whilelo` from a lane count `n <= MAX_LANES` (or a mask already
//! restricted to it), so at most `n` bytes are read or written whatever the
//! thread's current vector length. `str p`/`ldr p` move one predicate
//! register, `VL / 8 <= 32` bytes, which always fits a `[u64; 4]`.
//! Masked loads and stores never touch inactive lanes (SVE suppresses
//! accesses and faults for them), so callers only bound *active* lanes.
use super::{BinOp, CmpOp, MASK_WORDS, MAX_LANES};
use core::arch::asm;

type Lanes = [u8; MAX_LANES];
type Bits = [u64; MASK_WORDS];

/// Bytes per vector (`cntb`).
#[target_feature(enable = "sve")]
pub(super) unsafe fn vector_bytes() -> usize {
    let n: usize;
    // SAFETY: reads a system value only.
    unsafe { asm!("cntb {n}", n = out(reg) n, options(nomem, nostack, preserves_flags)) };
    n
}

/// `bits` = lanes `0..n` (`whilelo p0.b, xzr, n`).
#[target_feature(enable = "sve")]
pub(super) unsafe fn while_lt(n: usize, bits: &mut Bits) {
    *bits = [0; MASK_WORDS];
    // SAFETY: stores one predicate register (<= 32 bytes) into `bits`.
    unsafe {
        asm!(
            "whilelo p0.b, xzr, {n}",
            "str p0, [{bits}]",
            n = in(reg) n,
            bits = in(reg) bits.as_mut_ptr(),
            out("p0") _,
            options(nostack),
        )
    };
}

#[target_feature(enable = "sve")]
pub(super) unsafe fn splat(value: u8, n: usize, out: &mut Lanes) {
    // SAFETY: stores lanes `0..n` of `out`, `n <= MAX_LANES`.
    unsafe {
        asm!(
            "whilelo p0.b, xzr, {n}",
            "dup z0.b, {v:w}",
            "st1b z0.b, p0, [{out}]",
            n = in(reg) n,
            v = in(reg) u32::from(value),
            out = in(reg) out.as_mut_ptr(),
            out("v0") _,
            out("p0") _,
            options(nostack),
        )
    };
}

/// `out[i] = mask[i] ? src[i] : passthru` for `i < n`.
///
/// # Safety
/// SVE is available, and `src + i` is readable for every active lane `i`
/// of `bits` below `n`.
#[target_feature(enable = "sve")]
pub(super) unsafe fn load_masked(
    src: *const u8,
    bits: &Bits,
    passthru: u8,
    n: usize,
    out: &mut Lanes,
) {
    // SAFETY: `ld1b` reads only active lanes, which the caller bounds;
    // `st1b` writes lanes `0..n` of `out`.
    unsafe {
        asm!(
            "ldr p0, [{bits}]",
            "whilelo p1.b, xzr, {n}",
            "and p0.b, p1/z, p0.b, p1.b",
            "dup z1.b, {pt:w}",
            "ld1b z0.b, p0/z, [{src}]",
            "sel z0.b, p0, z0.b, z1.b",
            "st1b z0.b, p1, [{out}]",
            bits = in(reg) bits.as_ptr(),
            n = in(reg) n,
            pt = in(reg) u32::from(passthru),
            src = in(reg) src,
            out = in(reg) out.as_mut_ptr(),
            out("v0") _,
            out("v1") _,
            out("p0") _,
            out("p1") _,
            options(nostack),
        )
    };
}

/// `dst[i] = lanes[i]` for active lanes `i < n`.
///
/// # Safety
/// SVE is available, and `dst + i` is writable for every active lane.
#[target_feature(enable = "sve")]
pub(super) unsafe fn store_masked(dst: *mut u8, bits: &Bits, n: usize, lanes: &Lanes) {
    // SAFETY: `st1b` writes only active lanes, which the caller bounds.
    unsafe {
        asm!(
            "ldr p0, [{bits}]",
            "whilelo p1.b, xzr, {n}",
            "and p0.b, p1/z, p0.b, p1.b",
            "ld1b z0.b, p1/z, [{lanes}]",
            "st1b z0.b, p0, [{dst}]",
            bits = in(reg) bits.as_ptr(),
            n = in(reg) n,
            lanes = in(reg) lanes.as_ptr(),
            dst = in(reg) dst,
            out("v0") _,
            out("p0") _,
            out("p1") _,
            options(nostack),
        )
    };
}

#[target_feature(enable = "sve")]
pub(super) unsafe fn compare(op: CmpOp, n: usize, a: &Lanes, b: &Lanes, bits: &mut Bits) {
    *bits = [0; MASK_WORDS];
    macro_rules! cmp {
        ($insn:literal) => {
            // SAFETY: reads lanes `0..n` of `a` and `b`; stores one
            // predicate register into `bits`.
            unsafe {
                asm!(
                    "whilelo p1.b, xzr, {n}",
                    "ld1b z0.b, p1/z, [{a}]",
                    "ld1b z1.b, p1/z, [{b}]",
                    $insn,
                    "str p0, [{bits}]",
                    n = in(reg) n,
                    a = in(reg) a.as_ptr(),
                    b = in(reg) b.as_ptr(),
                    bits = in(reg) bits.as_mut_ptr(),
                    out("v0") _,
                    out("v1") _,
                    out("p0") _,
                    out("p1") _,
                    options(nostack),
                )
            }
        };
    }
    match op {
        CmpOp::Eq => cmp!("cmpeq p0.b, p1/z, z0.b, z1.b"),
        // a < b unsigned is b > a (`cmphi`, "higher").
        CmpOp::LtU => cmp!("cmphi p0.b, p1/z, z1.b, z0.b"),
    }
}

#[target_feature(enable = "sve")]
pub(super) unsafe fn binary(op: BinOp, n: usize, a: &Lanes, b: &Lanes, out: &mut Lanes) {
    macro_rules! bin {
        ($insn:literal) => {
            // SAFETY: reads lanes `0..n` of `a`, `b`; writes them in `out`.
            unsafe {
                asm!(
                    "whilelo p1.b, xzr, {n}",
                    "ld1b z0.b, p1/z, [{a}]",
                    "ld1b z1.b, p1/z, [{b}]",
                    $insn,
                    "st1b z0.b, p1, [{out}]",
                    n = in(reg) n,
                    a = in(reg) a.as_ptr(),
                    b = in(reg) b.as_ptr(),
                    out = in(reg) out.as_mut_ptr(),
                    out("v0") _,
                    out("v1") _,
                    out("p1") _,
                    options(nostack),
                )
            }
        };
    }
    match op {
        BinOp::Xor => bin!("eor z0.d, z0.d, z1.d"),
        BinOp::And => bin!("and z0.d, z0.d, z1.d"),
        BinOp::Or => bin!("orr z0.d, z0.d, z1.d"),
        BinOp::Add => bin!("add z0.b, z0.b, z1.b"),
    }
}

#[target_feature(enable = "sve")]
pub(super) unsafe fn reduce_xor(n: usize, a: &Lanes) -> u8 {
    let r: u32;
    // SAFETY: reads lanes `0..n` of `a`.
    unsafe {
        asm!(
            "whilelo p1.b, xzr, {n}",
            "ld1b z0.b, p1/z, [{a}]",
            "eorv b1, p1, z0.b",
            "umov {r:w}, v1.b[0]",
            n = in(reg) n,
            a = in(reg) a.as_ptr(),
            r = lateout(reg) r,
            out("v0") _,
            out("v1") _,
            out("p1") _,
            options(nostack),
        )
    };
    r as u8
}

/// Whole-buffer delimiter count and XOR in one strip-mined loop.
///
/// # Safety
/// SVE is available and `ptr..ptr + len` is readable.
#[target_feature(enable = "sve")]
pub(super) unsafe fn scan(ptr: *const u8, len: usize, delim: u8) -> (u64, u8) {
    let count: u64;
    let xor: u32;
    // SAFETY: every load is under `whilelo off, len`, so reads stay inside
    // `ptr..ptr + len`.
    unsafe {
        asm!(
            "mov {cnt}, xzr",
            "mov {off}, xzr",
            "dup z1.b, {d:w}",
            "dup z2.b, #0",
            "2:",
            "whilelo p0.b, {off}, {len}",
            "b.none 3f",
            "ld1b z0.b, p0/z, [{ptr}, {off}]",
            "cmpeq p1.b, p0/z, z0.b, z1.b",
            "incp {cnt}, p1.b",
            // Inactive lanes were zeroed by the load: XOR them freely.
            "eor z2.d, z2.d, z0.d",
            "incb {off}",
            "b 2b",
            "3:",
            "ptrue p0.b",
            "eorv b3, p0, z2.b",
            "umov {x:w}, v3.b[0]",
            ptr = in(reg) ptr,
            len = in(reg) len,
            d = in(reg) u32::from(delim),
            off = out(reg) _,
            cnt = out(reg) count,
            x = lateout(reg) xor,
            out("v0") _,
            out("v1") _,
            out("v2") _,
            out("v3") _,
            out("p0") _,
            out("p1") _,
            options(nostack, readonly),
        )
    };
    (count, xor as u8)
}
