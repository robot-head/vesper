//! RVV 1.0 stage 1 classifier: one `asm!` block per chunk, entered only
//! after the kernel reports `V` in `AT_HWCAP`.
//!
//! Register contract: the block enables V for its own text
//! (`.option arch, +v`), sets `vl`/`vtype` itself and leaves them (and
//! `vxsat`, written by `vssubu`) clobbered. Vector registers: data `v8`,
//! previous-byte vectors `v9`–`v11`, UTF-8 tables `v12`–`v14`, class masks
//! `v1`/`v2`, scratch `v16`–`v18`, UTF-8 error accumulator `v19` and the
//! reduction `v20`; all declared clobbers. General registers are
//! compiler-chosen. No stack use.
//!
//! Strip-mining: each strip requests `avl = min(remaining, VLMAX)` at
//! `e8, m1`, so `vl = avl` exactly (RVV 1.0 §6.3) and, since `remaining` is
//! a multiple of 64 and `VLMAX = VLEN / 8` a multiple of 16, a multiple of
//! 8: each `vsm.v` writes exactly `vl / 8` whole bytes at bit offset `off`.
//! The accumulator uses tail-undisturbed policy across short strips.
//!
//! Memory contract: data loads read `vl <= remaining` bytes; history loads
//! read the three bytes before a strip only for strips after the first
//! (`off >= vl >= 16`); mask stores stay inside [`Classes`].
#![allow(unsafe_code)]

use super::{BYTE_1_HIGH, BYTE_1_LOW, BYTE_2_HIGH, CHUNK, Classes, History};
use core::arch::asm;

const AT_HWCAP: u64 = 16;
const HWCAP_V: u64 = 1 << (b'V' - b'A');

/// Whether the kernel reports `V` in `AT_HWCAP` (from `/proc/self/auxv`,
/// which `qemu-riscv64` also provides). Read or parse failures mean no.
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

/// `vlenb`, if V is available.
pub(super) fn vector_bytes() -> Option<usize> {
    if !detected() {
        return None;
    }
    let n: usize;
    // SAFETY: `vlenb` is a read-only CSR that exists when V is present.
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
    Some(n)
}

/// Classify `chunk` into `out`; `None` if V is unavailable, `chunk` is
/// longer than [`CHUNK`] or not whole blocks, else whether UTF-8 errors
/// were seen.
pub(super) fn classify(chunk: &[u8], history: History, out: &mut Classes) -> Option<bool> {
    if !detected() || chunk.len() > CHUNK || !chunk.len().is_multiple_of(64) {
        return None;
    }
    let mut tables = [0u8; 48];
    tables[..16].copy_from_slice(&BYTE_1_HIGH);
    tables[16..32].copy_from_slice(&BYTE_1_LOW);
    tables[32..].copy_from_slice(&BYTE_2_HIGH);
    let err: usize;
    // SAFETY: V is available; see the module's memory contract.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "vsetvli {vlmax}, zero, e8, m1, ta, ma",
            "vmv.v.i v19, 0",
            "li {t}, 16",
            "vsetvli zero, {t}, e8, m1, ta, ma",
            "vle8.v v12, ({tbl})",
            "addi {t}, {tbl}, 16",
            "vle8.v v13, ({t})",
            "addi {t}, {tbl}, 32",
            "vle8.v v14, ({t})",
            "2:",
            "beqz {rem}, 4f",
            "mv {t}, {rem}",
            "bleu {t}, {vlmax}, 5f",
            "mv {t}, {vlmax}",
            "5:",
            "vsetvli {t}, {t}, e8, m1, tu, ma",
            "beq {p}, {start}, 3f",
            "lbu {h1}, -1({p})",
            "lbu {h2}, -2({p})",
            "lbu {h3}, -3({p})",
            "3:",
            "vle8.v v8, ({p})",
            // Backslash and quote.
            "li {c}, 92",
            "vmseq.vx v1, v8, {c}",
            "vsm.v v1, ({obs})",
            "li {c}, 34",
            "vmseq.vx v1, v8, {c}",
            "vsm.v v1, ({oq})",
            // `{ } [ ] : ,` (`b | 0x20` folds `[`/`]` onto `{`/`}`).
            "li {c}, 32",
            "vor.vx v16, v8, {c}",
            "li {c}, 123",
            "vmseq.vx v1, v16, {c}",
            "li {c}, 125",
            "vmseq.vx v2, v16, {c}",
            "vmor.mm v1, v1, v2",
            "li {c}, 58",
            "vmseq.vx v2, v8, {c}",
            "vmor.mm v1, v1, v2",
            "li {c}, 44",
            "vmseq.vx v2, v8, {c}",
            "vmor.mm v1, v1, v2",
            "vsm.v v1, ({oop})",
            // Space, tab, line feed, carriage return.
            "li {c}, 32",
            "vmseq.vx v1, v8, {c}",
            "vmseq.vi v2, v8, 9",
            "vmor.mm v1, v1, v2",
            "vmseq.vi v2, v8, 10",
            "vmor.mm v1, v1, v2",
            "vmseq.vi v2, v8, 13",
            "vmor.mm v1, v1, v2",
            "vsm.v v1, ({ows})",
            // Control bytes.
            "li {c}, 32",
            "vmsltu.vx v1, v8, {c}",
            "vsm.v v1, ({octl})",
            // UTF-8: prev1..prev3 via slide-up, filled from the history.
            "vslide1up.vx v9, v8, {h1}",
            "vslide1up.vx v10, v9, {h2}",
            "vslide1up.vx v11, v10, {h3}",
            "vsrl.vi v16, v9, 4",
            "vrgather.vv v17, v12, v16",
            "vand.vi v16, v9, 15",
            "vrgather.vv v18, v13, v16",
            "vand.vv v17, v17, v18",
            "vsrl.vi v16, v8, 4",
            "vrgather.vv v18, v14, v16",
            "vand.vv v17, v17, v18",
            "li {c}, 0x60",
            "vssubu.vx v10, v10, {c}",
            "li {c}, 0x70",
            "vssubu.vx v11, v11, {c}",
            "vor.vv v10, v10, v11",
            "li {c}, 0x80",
            "vand.vx v10, v10, {c}",
            "vxor.vv v10, v10, v17",
            "vor.vv v19, v19, v10",
            "srli {c}, {t}, 3",
            "add {obs}, {obs}, {c}",
            "add {oq}, {oq}, {c}",
            "add {oop}, {oop}, {c}",
            "add {ows}, {ows}, {c}",
            "add {octl}, {octl}, {c}",
            "add {p}, {p}, {t}",
            "sub {rem}, {rem}, {t}",
            "j 2b",
            "4:",
            "vsetvli {t}, zero, e8, m1, ta, ma",
            "vmv.s.x v20, zero",
            "vredmaxu.vs v20, v19, v20",
            "vmv.x.s {err}, v20",
            ".option pop",
            p = inout(reg) chunk.as_ptr() => _,
            start = in(reg) chunk.as_ptr(),
            rem = inout(reg) chunk.len() => _,
            tbl = in(reg) tables.as_ptr(),
            h1 = inout(reg) usize::from(history.h1) => _,
            h2 = inout(reg) usize::from(history.h2) => _,
            h3 = inout(reg) usize::from(history.h3) => _,
            obs = inout(reg) out.backslash.as_mut_ptr() => _,
            oq = inout(reg) out.quote.as_mut_ptr() => _,
            oop = inout(reg) out.op.as_mut_ptr() => _,
            ows = inout(reg) out.whitespace.as_mut_ptr() => _,
            octl = inout(reg) out.control.as_mut_ptr() => _,
            vlmax = out(reg) _,
            t = out(reg) _,
            c = out(reg) _,
            err = lateout(reg) err,
            out("v1") _, out("v2") _, out("v8") _, out("v9") _,
            out("v10") _, out("v11") _, out("v12") _, out("v13") _,
            out("v14") _, out("v16") _, out("v17") _, out("v18") _,
            out("v19") _, out("v20") _,
            options(nostack),
        )
    };
    Some(err & 0xff != 0)
}
