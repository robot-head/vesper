//! Stage 1 on scalable vectors: Arm SVE and RISC-V Vector 1.0 (§14.3, §20.3).
//!
//! The kernels strip-mine each run of whole 64-byte blocks at the
//! hardware's own vector length: SVE strips are governed by `whilelo`, RVV
//! strips by `vsetvli`. Per strip they compute the five raw byte classes
//! as predicates and store them straight into per-class bit arrays (SVE
//! `str p`, RVV `vsm.v` both write bit `i` for byte `i`), so a strip of any
//! width lands at its bit offset and the 64-bit block words come out the same
//! at every vector length. In the same pass they run the Keiser–Lemire
//! UTF-8 lookup check (`tbl` / `vrgather.vv` over the three nibble tables,
//! previous bytes via `insr` / `vslide1up.vx`). The shared integer block step
//! ([`block_quotes`], [`block_finish`]) then derives escapes, string
//! interiors (prefix XOR by shifts: carry-less multiply is optional in both
//! ISAs and not assumed) and structurals, exactly as the fixed-width paths.
//!
//! These paths sit beside [`Isa`](super::Isa) rather than in it, so the
//! existing dispatch and its callers are unchanged: build an index with
//! [`StructuralIndex::build_scalable`], or compare masks with
//! [`block_masks_scalable`]. An index built here reports
//! [`StructuralIndex::isa`] as `None`.
use super::{
    BlockMasks, Carry, IndexLimits, MAX_INDEX_INPUT, NoRecord, RawMasks, Recorder, RunOutcome,
    Sink, StructuralIndex, block_finish, block_quotes, finish, padded_tail, prefix_xor_shifts,
};
use crate::{Error, ErrorKind};

#[cfg(target_arch = "riscv64")]
mod rvv;
#[cfg(target_arch = "aarch64")]
mod sve;

/// A scalable-vector stage 1 path.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScalableIsa {
    /// AArch64 SVE (SVE2 hardware qualifies), any vector length.
    Sve,
    /// RISC-V Vector 1.0 at `e8, m1`, any VLEN.
    Rvv,
}

impl ScalableIsa {
    pub const ALL: [ScalableIsa; 2] = [ScalableIsa::Sve, ScalableIsa::Rvv];

    /// Whether this process can run the path on the current CPU.
    pub fn is_available(self) -> bool {
        match self {
            ScalableIsa::Sve => {
                #[cfg(target_arch = "aarch64")]
                {
                    sve::vector_bytes().is_some()
                }
                #[cfg(not(target_arch = "aarch64"))]
                {
                    false
                }
            }
            ScalableIsa::Rvv => {
                #[cfg(target_arch = "riscv64")]
                {
                    rvv::detected()
                }
                #[cfg(not(target_arch = "riscv64"))]
                {
                    false
                }
            }
        }
    }

    /// The vector length in bytes this thread runs the path at.
    pub fn vector_bytes(self) -> Option<usize> {
        match self {
            #[cfg(target_arch = "aarch64")]
            ScalableIsa::Sve => sve::vector_bytes(),
            #[cfg(target_arch = "riscv64")]
            ScalableIsa::Rvv => rvv::vector_bytes(),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// Every scalable path available here.
    pub fn available() -> impl Iterator<Item = ScalableIsa> {
        Self::ALL.into_iter().filter(|isa| isa.is_available())
    }

    pub const fn name(self) -> &'static str {
        match self {
            ScalableIsa::Sve => "sve",
            ScalableIsa::Rvv => "rvv1.0",
        }
    }
}

/// Bytes per kernel call: eight blocks.
const CHUNK: usize = 512;
/// Bit-array words per class: the chunk's bits plus 32 bytes of slack,
/// because SVE's `str p` stores a whole predicate register (up to 32 bytes)
/// even for the final, partial strip. Bits past the chunk are never read.
pub(crate) const WORDS: usize = CHUNK / 64 + 4;

/// One kernel call's output: bit `i` of class `k` describes byte `i`.
#[repr(C)]
#[derive(Default)]
pub(crate) struct Classes {
    pub backslash: [u64; WORDS],
    pub quote: [u64; WORDS],
    pub op: [u64; WORDS],
    pub whitespace: [u64; WORDS],
    pub control: [u64; WORDS],
}

/// The three bytes before a chunk (`h1` is the nearest), zero at the start
/// of the input, as the UTF-8 check's `prev1..prev3` lanes.
#[derive(Clone, Copy, Default)]
#[cfg_attr(
    not(any(target_arch = "aarch64", target_arch = "riscv64")),
    allow(dead_code, reason = "read by the SVE and RVV kernels only")
)]
pub(crate) struct History {
    pub h1: u8,
    pub h2: u8,
    pub h3: u8,
}

impl History {
    fn before(input: &[u8], at: usize) -> Self {
        let get = |k: usize| at.checked_sub(k).map_or(0, |i| input[i]);
        Self {
            h1: get(1),
            h2: get(2),
            h3: get(3),
        }
    }
}

/// Classify `chunk` (a multiple of 64 bytes, at most [`CHUNK`]) into `out`
/// and return whether the UTF-8 lookup check found an error in it; `None`
/// when the kernel cannot run here.
fn classify(isa: ScalableIsa, chunk: &[u8], history: History, out: &mut Classes) -> Option<bool> {
    match isa {
        #[cfg(target_arch = "aarch64")]
        ScalableIsa::Sve => sve::classify(chunk, history, out),
        #[cfg(target_arch = "riscv64")]
        ScalableIsa::Rvv => rvv::classify(chunk, history, out),
        #[allow(unreachable_patterns)]
        _ => {
            let _ = (chunk, history, out);
            None
        }
    }
}

/// UTF-8 sequences cut off by the end of the input: a lead byte among the
/// last three bytes that needs more bytes than remain. Only needed when the
/// input ends on a block boundary; a padded tail block catches the rest
/// (spaces are not continuation bytes).
fn incomplete_at_end(input: &[u8]) -> bool {
    let n = input.len();
    let at = |k: usize| n.checked_sub(k).map_or(0, |i| input[i]);
    at(1) >= 0xc0 || at(2) >= 0xe0 || at(3) >= 0xf0
}

fn run<R: Recorder>(
    isa: ScalableIsa,
    input: &[u8],
    sink: &mut Sink<'_>,
    rec: &mut R,
) -> Result<RunOutcome, Error> {
    let unavailable = Error::new(ErrorKind::IsaUnavailable, 0);
    if !isa.is_available() {
        return Err(unavailable);
    }
    let mut carry = Carry::default();
    let mut utf8_error = false;
    let mut classes = Classes::default();
    let mut base = 0u32;
    let mut emit = |classes: &Classes, blocks: usize, carry: &mut Carry, base: &mut u32| {
        for b in 0..blocks {
            let raw = RawMasks {
                backslash: classes.backslash[b],
                quote: classes.quote[b],
                op: classes.op[b],
                whitespace: classes.whitespace[b],
                control: classes.control[b],
            };
            let (escaped, quotes) = block_quotes(&raw, carry);
            let m = block_finish(raw, escaped, quotes, prefix_xor_shifts(quotes), carry);
            rec.record(&m);
            sink.push(&m, *base);
            *base = base.wrapping_add(64);
        }
    };
    let full = input.len() / 64 * 64;
    let mut at = 0;
    while at < full {
        let end = (at + CHUNK).min(full);
        let history = History::before(input, at);
        utf8_error |= classify(isa, &input[at..end], history, &mut classes).ok_or(unavailable)?;
        emit(&classes, (end - at) / 64, &mut carry, &mut base);
        at = end;
    }
    if full < input.len() {
        let tail = padded_tail(&input[full..]);
        let history = History::before(input, full);
        utf8_error |= classify(isa, &tail, history, &mut classes).ok_or(unavailable)?;
        emit(&classes, 1, &mut carry, &mut base);
    } else if incomplete_at_end(input) {
        utf8_error = true;
    }
    Ok(RunOutcome {
        utf8_ok: !utf8_error,
        carry,
    })
}

/// Per-block masks for `input` on a scalable path and the UTF-8 verdict, as
/// [`block_masks`](super::block_masks) for the fixed-width paths.
pub fn block_masks_scalable(
    input: &[u8],
    isa: ScalableIsa,
) -> Result<(Vec<BlockMasks>, bool), Error> {
    if input.len() > MAX_INDEX_INPUT {
        return Err(Error::new(ErrorKind::InputLimit, 0));
    }
    let mut buf = Vec::new();
    let mut sink = Sink::new(&mut buf, input.len(), usize::MAX);
    let mut rec = Vec::new();
    let outcome = run(isa, input, &mut sink, &mut rec)?;
    Ok((rec, outcome.utf8_ok))
}

impl StructuralIndex {
    /// [`build`](Self::build) on a scalable-vector path: the same positions,
    /// errors and limits as every other path.
    pub fn build_scalable(
        &mut self,
        input: &[u8],
        isa: ScalableIsa,
        limits: IndexLimits,
    ) -> Result<(), Error> {
        self.reset();
        if input.len() > limits.max_input_bytes || input.len() > MAX_INDEX_INPUT {
            return Err(Error::new(ErrorKind::InputLimit, 0));
        }
        let mut sink = Sink::new(&mut self.buf, input.len(), limits.max_entries);
        let outcome = run(isa, input, &mut sink, &mut NoRecord)?;
        let (structurals, escapes) = (sink.structurals, sink.escapes);
        let (alloc_failed, control, truncated) = (
            sink.alloc_failed,
            sink.first_string_control,
            sink.truncated_at,
        );
        finish(input, &outcome, alloc_failed)?;
        let len = self.buf.len();
        self.buf[len - escapes..].reverse();
        self.structural_len = structurals;
        self.escape_len = escapes;
        self.first_string_control = control;
        self.truncated_at = truncated;
        self.unclosed_string = outcome.carry.prev_in_string != 0;
        Ok(())
    }
}

// The Keiser–Lemire tables, identical to the fixed-width kernels' copies
// (kept private there); the differential tests check the outcome.
const TOO_SHORT: u8 = 1 << 0;
const TOO_LONG: u8 = 1 << 1;
const OVERLONG_3: u8 = 1 << 2;
const TOO_LARGE: u8 = 1 << 3;
const SURROGATE: u8 = 1 << 4;
const OVERLONG_2: u8 = 1 << 5;
const TOO_LARGE_1000: u8 = 1 << 6;
const OVERLONG_4: u8 = 1 << 6;
const TWO_CONTS: u8 = 1 << 7;
const CARRY: u8 = TOO_SHORT | TOO_LONG | TWO_CONTS;

/// Indexed by the high nibble of the previous byte.
#[allow(dead_code, reason = "used by the SVE and RVV kernels only")]
pub(crate) const BYTE_1_HIGH: [u8; 16] = [
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TWO_CONTS,
    TWO_CONTS,
    TWO_CONTS,
    TWO_CONTS,
    TOO_SHORT | OVERLONG_2,
    TOO_SHORT,
    TOO_SHORT | OVERLONG_3 | SURROGATE,
    TOO_SHORT | TOO_LARGE | TOO_LARGE_1000 | OVERLONG_4,
];
/// Indexed by the low nibble of the previous byte.
#[allow(dead_code, reason = "used by the SVE and RVV kernels only")]
pub(crate) const BYTE_1_LOW: [u8; 16] = [
    CARRY | OVERLONG_3 | OVERLONG_2 | OVERLONG_4,
    CARRY | OVERLONG_2,
    CARRY,
    CARRY,
    CARRY | TOO_LARGE,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000 | SURROGATE,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
];
/// Indexed by the high nibble of the current byte.
#[allow(dead_code, reason = "used by the SVE and RVV kernels only")]
pub(crate) const BYTE_2_HIGH: [u8; 16] = [
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_LONG | OVERLONG_2 | TWO_CONTS | OVERLONG_3 | TOO_LARGE_1000 | OVERLONG_4,
    TOO_LONG | OVERLONG_2 | TWO_CONTS | OVERLONG_3 | TOO_LARGE,
    TOO_LONG | OVERLONG_2 | TWO_CONTS | SURROGATE | TOO_LARGE,
    TOO_LONG | OVERLONG_2 | TWO_CONTS | SURROGATE | TOO_LARGE,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
];
