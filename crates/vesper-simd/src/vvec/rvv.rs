//! RISC-V Vector 1.0 backend: stable `asm!`, entered only after the kernel
//! reports the `V` extension in `AT_HWCAP`.
//!
//! Register contract shared by every block here:
//!
//! * Each block enables the extension for its own text only
//!   (`.option push`, `.option arch, +v`, `.option pop`), so the crate
//!   builds for the baseline `riscv64gc` target.
//! * Each block sets `vl`/`vtype` itself with `vsetvli` (`e8, m1` unless
//!   noted) and leaves them clobbered; the psABI does not preserve them
//!   across calls and compiler-generated code here never relies on them.
//! * Scratch vector registers `v0`–`v3`, `v8`, `v9`, `v16`, `v17` are
//!   declared `vreg` clobbers; general scratch registers are compiler-chosen.
//!   No stack use.
//!
//! Memory contract: element accesses happen only for `i < vl`, and `vl` is
//! set from a lane count `n <= MAX_LANES`, the profile's lanes, which equal
//! `VLMAX` at `e8, m1` (`vlenb`), so `vl = n` exactly. Masked accesses
//! (`v0.t`) skip inactive elements without touching memory or faulting.
//! `vsm.v`/`vlm.v` move `ceil(vl / 8) <= 32` bytes.
use super::{BinOp, CmpOp, MASK_WORDS, MAX_LANES};
use core::arch::asm;

type Lanes = [u8; MAX_LANES];
type Bits = [u64; MASK_WORDS];

const AT_HWCAP: u64 = 16;
const HWCAP_V: u64 = 1 << (b'V' - b'A');

/// Whether the kernel reports `V` in `AT_HWCAP`, read from
/// `/proc/self/auxv` (which `qemu-riscv64` also provides). Any read or parse
/// failure means "not available".
pub(super) fn detected() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let Ok(auxv) = std::fs::read("/proc/self/auxv") else {
            return false;
        };
        let (pairs, _) = auxv.as_chunks::<16>();
        for pair in pairs {
            let (key, value) = pair.split_at(8);
            let key = u64::from_le_bytes(key.try_into().unwrap_or([0; 8]));
            let value = u64::from_le_bytes(value.try_into().unwrap_or([0; 8]));
            match key {
                0 => break,
                AT_HWCAP => return value & HWCAP_V != 0,
                _ => {}
            }
        }
        false
    })
}

/// `vlenb`: bytes per vector register.
///
/// # Safety
/// V is available.
pub(super) unsafe fn vector_bytes() -> usize {
    let n: usize;
    // SAFETY: reads a read-only CSR that exists when V is present.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "csrr {n}, vlenb",
            ".option pop",
            n = out(reg) n,
            options(nomem, nostack, preserves_flags),
        )
    };
    n
}

/// `bits` = lanes `0..n` out of `lanes` (`vid.v` then `vmsltu.vx`, at
/// `e16, m2` so that the index and `n` up to 256 are exact).
pub(super) unsafe fn while_lt(n: usize, lanes: usize, bits: &mut Bits) {
    *bits = [0; MASK_WORDS];
    // SAFETY: `vl = lanes` (`VLMAX` at `e16, m2` is `VLEN / 8`); `vsm.v`
    // writes `lanes / 8 <= 32` bytes into `bits`.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "vsetvli {t}, {lanes}, e16, m2, ta, ma",
            "vid.v v8",
            "vmsltu.vx v0, v8, {n}",
            "vsm.v v0, ({bits})",
            ".option pop",
            t = out(reg) _,
            lanes = in(reg) lanes,
            n = in(reg) n,
            bits = in(reg) bits.as_mut_ptr(),
            out("v0") _,
            out("v8") _,
            out("v9") _,
            options(nostack),
        )
    };
}

pub(super) unsafe fn splat(value: u8, n: usize, out: &mut Lanes) {
    // SAFETY: writes `vl = n` bytes of `out`.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "vsetvli {t}, {n}, e8, m1, ta, ma",
            "vmv.v.x v8, {v}",
            "vse8.v v8, ({out})",
            ".option pop",
            t = out(reg) _,
            n = in(reg) n,
            v = in(reg) usize::from(value),
            out = in(reg) out.as_mut_ptr(),
            out("v8") _,
            options(nostack),
        )
    };
}

/// # Safety
/// V is available, and `src + i` is readable for every active lane `i < n`.
pub(super) unsafe fn load_masked(
    src: *const u8,
    bits: &Bits,
    passthru: u8,
    n: usize,
    out: &mut Lanes,
) {
    // SAFETY: the `v0.t` load reads only active lanes, which the caller
    // bounds; `mu` keeps `passthru` in the inactive ones.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "vsetvli {t}, {n}, e8, m1, ta, mu",
            "vlm.v v0, ({bits})",
            "vmv.v.x v8, {pt}",
            "vle8.v v8, ({src}), v0.t",
            "vse8.v v8, ({out})",
            ".option pop",
            t = out(reg) _,
            n = in(reg) n,
            bits = in(reg) bits.as_ptr(),
            pt = in(reg) usize::from(passthru),
            src = in(reg) src,
            out = in(reg) out.as_mut_ptr(),
            out("v0") _,
            out("v8") _,
            options(nostack),
        )
    };
}

/// # Safety
/// V is available, and `dst + i` is writable for every active lane `i < n`.
pub(super) unsafe fn store_masked(dst: *mut u8, bits: &Bits, n: usize, lanes: &Lanes) {
    // SAFETY: the `v0.t` store writes only active lanes.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "vsetvli {t}, {n}, e8, m1, ta, ma",
            "vlm.v v0, ({bits})",
            "vle8.v v8, ({lanes})",
            "vse8.v v8, ({dst}), v0.t",
            ".option pop",
            t = out(reg) _,
            n = in(reg) n,
            bits = in(reg) bits.as_ptr(),
            lanes = in(reg) lanes.as_ptr(),
            dst = in(reg) dst,
            out("v0") _,
            out("v8") _,
            options(nostack),
        )
    };
}

pub(super) unsafe fn compare(op: CmpOp, n: usize, a: &Lanes, b: &Lanes, bits: &mut Bits) {
    *bits = [0; MASK_WORDS];
    macro_rules! cmp {
        ($insn:literal) => {
            // SAFETY: reads `n` bytes of `a`, `b`; writes `n / 8` mask bytes.
            unsafe {
                asm!(
                    ".option push",
                    ".option arch, +v",
                    "vsetvli {t}, {n}, e8, m1, ta, ma",
                    "vle8.v v8, ({a})",
                    "vle8.v v9, ({b})",
                    $insn,
                    "vsm.v v0, ({bits})",
                    ".option pop",
                    t = out(reg) _,
                    n = in(reg) n,
                    a = in(reg) a.as_ptr(),
                    b = in(reg) b.as_ptr(),
                    bits = in(reg) bits.as_mut_ptr(),
                    out("v0") _,
                    out("v8") _,
                    out("v9") _,
                    options(nostack),
                )
            }
        };
    }
    match op {
        CmpOp::Eq => cmp!("vmseq.vv v0, v8, v9"),
        CmpOp::LtU => cmp!("vmsltu.vv v0, v8, v9"),
    }
}

pub(super) unsafe fn binary(op: BinOp, n: usize, a: &Lanes, b: &Lanes, out: &mut Lanes) {
    macro_rules! bin {
        ($insn:literal) => {
            // SAFETY: reads `n` bytes of `a`, `b`; writes `n` bytes of `out`.
            unsafe {
                asm!(
                    ".option push",
                    ".option arch, +v",
                    "vsetvli {t}, {n}, e8, m1, ta, ma",
                    "vle8.v v8, ({a})",
                    "vle8.v v9, ({b})",
                    $insn,
                    "vse8.v v8, ({out})",
                    ".option pop",
                    t = out(reg) _,
                    n = in(reg) n,
                    a = in(reg) a.as_ptr(),
                    b = in(reg) b.as_ptr(),
                    out = in(reg) out.as_mut_ptr(),
                    out("v8") _,
                    out("v9") _,
                    options(nostack),
                )
            }
        };
    }
    match op {
        BinOp::Xor => bin!("vxor.vv v8, v8, v9"),
        BinOp::And => bin!("vand.vv v8, v8, v9"),
        BinOp::Or => bin!("vor.vv v8, v8, v9"),
        BinOp::Add => bin!("vadd.vv v8, v8, v9"),
    }
}

pub(super) unsafe fn reduce_xor(n: usize, a: &Lanes) -> u8 {
    let r: usize;
    // SAFETY: reads `n` bytes of `a`.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "vsetvli {t}, {n}, e8, m1, ta, ma",
            "vle8.v v8, ({a})",
            "vmv.s.x v9, zero",
            "vredxor.vs v9, v8, v9",
            "vmv.x.s {r}, v9",
            ".option pop",
            t = out(reg) _,
            n = in(reg) n,
            a = in(reg) a.as_ptr(),
            r = lateout(reg) r,
            out("v8") _,
            out("v9") _,
            options(nostack),
        )
    };
    r as u8
}

/// Whole-buffer delimiter count and XOR in one strip-mined loop. Each strip
/// takes `vl = vsetvli(remaining)`, which may be below `VLMAX` near the end
/// (RVV allows `ceil(avl / 2) <= vl` there); the loop advances by whatever
/// `vl` the hardware chose, and the accumulator uses tail-undisturbed policy
/// so lanes past a short `vl` keep their value.
///
/// # Safety
/// V is available and `ptr..ptr + len` is readable.
pub(super) unsafe fn scan(ptr: *const u8, len: usize, delim: u8) -> (u64, u8) {
    let count: u64;
    let xor: usize;
    // SAFETY: each strip reads `vl <= remaining` bytes from `ptr`.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "vsetvli {t}, zero, e8, m1, ta, ma",
            "vmv.v.i v16, 0",
            "li {cnt}, 0",
            "2:",
            "beqz {rem}, 3f",
            "vsetvli {t}, {rem}, e8, m1, tu, ma",
            "vle8.v v8, ({ptr})",
            "vmseq.vx v0, v8, {d}",
            "vcpop.m {c}, v0",
            "add {cnt}, {cnt}, {c}",
            "vxor.vv v16, v16, v8",
            "add {ptr}, {ptr}, {t}",
            "sub {rem}, {rem}, {t}",
            "j 2b",
            "3:",
            "vsetvli {t}, zero, e8, m1, ta, ma",
            "vmv.s.x v17, zero",
            "vredxor.vs v17, v16, v17",
            "vmv.x.s {x}, v17",
            ".option pop",
            ptr = inout(reg) ptr => _,
            rem = inout(reg) len => _,
            d = in(reg) usize::from(delim),
            t = out(reg) _,
            c = out(reg) _,
            cnt = out(reg) count,
            x = lateout(reg) xor,
            out("v0") _,
            out("v8") _,
            out("v16") _,
            out("v17") _,
            options(nostack, readonly),
        )
    };
    (count, xor as u8)
}
