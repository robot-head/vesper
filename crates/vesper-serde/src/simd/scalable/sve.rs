//! SVE stage 1 classifier: one `asm!` block per chunk, entered only after
//! `is_aarch64_feature_detected!("sve")`.
//!
//! Register contract: constants in `z1`–`z7`, UTF-8 tables in `z16`–`z18`,
//! the UTF-8 error accumulator in `z19`, data in `z0`, previous-byte vectors
//! in `z20`–`z22`, scratch `z23`/`z24`; predicates `p0` (governing, from
//! `whilelo off, len`), `p1`/`p2` (classes), `p3` (table loads); flags.
//! All are declared clobbers (`z` registers through their `v` halves; the
//! callee-saved `z8`–`z15` are not used). General registers are
//! compiler-chosen. No stack use.
//!
//! Memory contract: data loads are `ld1b` under `p0`, inside
//! `chunk[..len]`; history loads read `chunk[off - 3..off]` only when
//! `off >= VL >= 16`. Each strip stores one predicate register
//! (`VL / 8` bytes) per class at bit offset `off`, which stays inside
//! [`Classes`]: `len / 8 + 32 <= WORDS * 8`.
#![allow(unsafe_code)]

use super::{BYTE_1_HIGH, BYTE_1_LOW, BYTE_2_HIGH, CHUNK, Classes, History, WORDS};
use core::arch::asm;

const _: () = assert!(CHUNK / 8 + 32 <= WORDS * 8);

fn available() -> bool {
    std::arch::is_aarch64_feature_detected!("sve")
}

/// Bytes per vector, if SVE is available.
pub(super) fn vector_bytes() -> Option<usize> {
    // SAFETY: SVE was detected.
    available().then(|| unsafe { cntb() })
}

/// Classify `chunk` into `out`; `None` if SVE is unavailable or the chunk
/// is longer than [`CHUNK`], else whether UTF-8 errors were seen.
pub(super) fn classify(chunk: &[u8], history: History, out: &mut Classes) -> Option<bool> {
    if !available() || chunk.len() > CHUNK {
        return None;
    }
    // SAFETY: both preconditions were checked just above.
    Some(unsafe { classify_unchecked(chunk, history, out) })
}

#[target_feature(enable = "sve")]
unsafe fn cntb() -> usize {
    let n: usize;
    // SAFETY: reads a system value only.
    unsafe { asm!("cntb {n}", n = out(reg) n, options(nomem, nostack, preserves_flags)) };
    n
}

/// # Safety
/// SVE is available and `chunk.len() <= CHUNK`.
#[target_feature(enable = "sve")]
unsafe fn classify_unchecked(chunk: &[u8], history: History, out: &mut Classes) -> bool {
    let mut tables = [0u8; 48];
    tables[..16].copy_from_slice(&BYTE_1_HIGH);
    tables[16..32].copy_from_slice(&BYTE_1_LOW);
    tables[32..].copy_from_slice(&BYTE_2_HIGH);
    let err: u32;
    // SAFETY: see the module's memory contract.
    unsafe {
        asm!(
            "whilelo p3.b, xzr, {sixteen}",
            "ld1b z16.b, p3/z, [{tbl}]",
            "add {t}, {tbl}, #16",
            "ld1b z17.b, p3/z, [{t}]",
            "add {t}, {tbl}, #32",
            "ld1b z18.b, p3/z, [{t}]",
            "dup z1.b, #92",
            "dup z2.b, #34",
            "dup z3.b, #123",
            "dup z4.b, #125",
            "dup z5.b, #58",
            "dup z6.b, #44",
            "dup z7.b, #32",
            "dup z19.b, #0",
            "mov {off}, xzr",
            "2:",
            "whilelo p0.b, {off}, {len}",
            "b.none 4f",
            "cbz {off}, 3f",
            "add {t}, {ptr}, {off}",
            "ldurb {h1:w}, [{t}, #-1]",
            "ldurb {h2:w}, [{t}, #-2]",
            "ldurb {h3:w}, [{t}, #-3]",
            "3:",
            "ld1b z0.b, p0/z, [{ptr}, {off}]",
            // Backslash and quote.
            "cmpeq p1.b, p0/z, z0.b, z1.b",
            "str p1, [{obs}]",
            "cmpeq p1.b, p0/z, z0.b, z2.b",
            "str p1, [{oq}]",
            // `{ } [ ] : ,`: `b | 0x20` is `{` only for `[` and `{`, and
            // `}` only for `]` and `}`.
            "mov z23.d, z0.d",
            "orr z23.b, z23.b, #0x20",
            "cmpeq p1.b, p0/z, z23.b, z3.b",
            "cmpeq p2.b, p0/z, z23.b, z4.b",
            "orr p1.b, p0/z, p1.b, p2.b",
            "cmpeq p2.b, p0/z, z0.b, z5.b",
            "orr p1.b, p0/z, p1.b, p2.b",
            "cmpeq p2.b, p0/z, z0.b, z6.b",
            "orr p1.b, p0/z, p1.b, p2.b",
            "str p1, [{oop}]",
            // Space, tab, line feed, carriage return.
            "cmpeq p1.b, p0/z, z0.b, z7.b",
            "cmpeq p2.b, p0/z, z0.b, #9",
            "orr p1.b, p0/z, p1.b, p2.b",
            "cmpeq p2.b, p0/z, z0.b, #10",
            "orr p1.b, p0/z, p1.b, p2.b",
            "cmpeq p2.b, p0/z, z0.b, #13",
            "orr p1.b, p0/z, p1.b, p2.b",
            "str p1, [{ows}]",
            // Control bytes.
            "cmplo p1.b, p0/z, z0.b, #32",
            "str p1, [{octl}]",
            // UTF-8: prev1..prev3 shift the strip up by one, two, three
            // lanes, filling from the bytes before it.
            "mov z20.d, z0.d",
            "insr z20.b, {h1:w}",
            "mov z21.d, z20.d",
            "insr z21.b, {h2:w}",
            "mov z22.d, z21.d",
            "insr z22.b, {h3:w}",
            "lsr z23.b, z20.b, #4",
            "tbl z23.b, {{z16.b}}, z23.b",
            "mov z24.d, z20.d",
            "and z24.b, z24.b, #0x0f",
            "tbl z24.b, {{z17.b}}, z24.b",
            "and z23.d, z23.d, z24.d",
            "lsr z24.b, z0.b, #4",
            "tbl z24.b, {{z18.b}}, z24.b",
            "and z23.d, z23.d, z24.d",
            "uqsub z21.b, z21.b, #0x60",
            "uqsub z22.b, z22.b, #0x70",
            "orr z21.d, z21.d, z22.d",
            "and z21.b, z21.b, #0x80",
            "eor z21.d, z21.d, z23.d",
            "orr z19.b, p0/m, z19.b, z21.b",
            "addpl {obs}, {obs}, #1",
            "addpl {oq}, {oq}, #1",
            "addpl {oop}, {oop}, #1",
            "addpl {ows}, {ows}, #1",
            "addpl {octl}, {octl}, #1",
            "incb {off}",
            "b 2b",
            "4:",
            "ptrue p0.b",
            "umaxv b23, p0, z19.b",
            "umov {err:w}, v23.b[0]",
            ptr = in(reg) chunk.as_ptr(),
            len = in(reg) chunk.len(),
            tbl = in(reg) tables.as_ptr(),
            sixteen = in(reg) 16usize,
            h1 = inout(reg) u32::from(history.h1) => _,
            h2 = inout(reg) u32::from(history.h2) => _,
            h3 = inout(reg) u32::from(history.h3) => _,
            obs = inout(reg) out.backslash.as_mut_ptr() => _,
            oq = inout(reg) out.quote.as_mut_ptr() => _,
            oop = inout(reg) out.op.as_mut_ptr() => _,
            ows = inout(reg) out.whitespace.as_mut_ptr() => _,
            octl = inout(reg) out.control.as_mut_ptr() => _,
            off = out(reg) _,
            t = out(reg) _,
            err = lateout(reg) err,
            out("v0") _, out("v1") _, out("v2") _, out("v3") _,
            out("v4") _, out("v5") _, out("v6") _, out("v7") _,
            out("v16") _, out("v17") _, out("v18") _, out("v19") _,
            out("v20") _, out("v21") _, out("v22") _, out("v23") _,
            out("v24") _,
            out("p0") _, out("p1") _, out("p2") _, out("p3") _,
            options(nostack),
        )
    };
    err != 0
}
