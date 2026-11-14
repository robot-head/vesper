//! Stage 1 of design §20.3: a structural index over 64-byte blocks.
//!
//! Every ISA path computes the same five raw byte-class masks per block
//! (backslash, quote, structural operator, whitespace, control byte) and
//! validates UTF-8. Shared branchless integer code then derives the escaped
//! mask (odd backslash runs with a block carry), string interiors (prefix XOR,
//! carry-less multiply on SIMD paths), and the structural positions. The
//! scalar reference classifies one byte at a time and uses `core::str` for
//! UTF-8, so the SIMD paths are checked against an independent definition.
//!
//! Runtime dispatch picks the best available path; [`Isa::Scalar`] is always
//! available. The source-level `eff [Isa[P]]` effect is a later compiler step:
//! this host library makes the choice at run time, not in the type system.
use crate::{Error, ErrorKind};

mod kernels;
pub mod scalable;

/// Supported instruction-set paths for stage 1.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Isa {
    /// Byte-at-a-time reference. Always available.
    Scalar,
    /// x86_64 SSE4.2 (with SSSE3 `pshufb`) and PCLMULQDQ, four 16-byte lanes.
    Sse42,
    /// x86_64 AVX2 and PCLMULQDQ, two 32-byte lanes.
    Avx2,
    /// x86_64 AVX-512F/BW and VPCLMULQDQ: one 64-byte register per block,
    /// and one carry-less multiply computes four blocks' quote parities.
    Avx512,
    /// AArch64 Advanced SIMD and PMULL (the `aes` feature), four 16-byte lanes.
    Neon,
}

impl Isa {
    pub const ALL: [Isa; 5] = [Isa::Scalar, Isa::Sse42, Isa::Avx2, Isa::Avx512, Isa::Neon];

    /// The preferred path this CPU supports, detected at run time. AVX2 is
    /// preferred over AVX-512: on the only AVX-512 host measured (see
    /// docs/parallel-results/serde-json-rest.md) the AVX-512 path was
    /// slower, so it is opt-in through [`IndexedJson::parse_with_isa`] and
    /// [`SimdParser::with_isa`](crate::SimdParser::with_isa).
    ///
    /// [`IndexedJson::parse_with_isa`]: crate::IndexedJson::parse_with_isa
    pub fn detect() -> Self {
        [Isa::Avx2, Isa::Neon, Isa::Sse42, Isa::Avx512]
            .into_iter()
            .find(|isa| isa.is_available())
            .unwrap_or(Isa::Scalar)
    }

    /// Whether this process can run the path on the current CPU.
    pub fn is_available(self) -> bool {
        match self {
            Isa::Scalar => true,
            Isa::Sse42 => kernels::sse42_available(),
            Isa::Avx2 => kernels::avx2_available(),
            Isa::Avx512 => kernels::avx512_available(),
            Isa::Neon => kernels::neon_available(),
        }
    }

    /// Whether the CPU reports the Arm Scalable Vector Extension. No SVE path
    /// exists (Rust's SVE intrinsics are unstable), so such hosts use NEON;
    /// callers can report that SVE was present but not used.
    pub fn sve_reported() -> bool {
        kernels::sve_reported()
    }

    /// Every path available here, scalar first.
    pub fn available() -> impl Iterator<Item = Isa> {
        Self::ALL.into_iter().filter(|isa| isa.is_available())
    }

    pub const fn name(self) -> &'static str {
        match self {
            Isa::Scalar => "scalar",
            Isa::Sse42 => "sse4.2+pclmulqdq",
            Isa::Avx2 => "avx2+pclmulqdq",
            Isa::Avx512 => "avx512bw+vpclmulqdq",
            Isa::Neon => "neon+pmull",
        }
    }
}

/// Byte classes of one 64-byte block; bit `i` describes byte `i`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RawMasks {
    pub backslash: u64,
    pub quote: u64,
    /// `{ } [ ] : ,`
    pub op: u64,
    /// space, tab, line feed, carriage return
    pub whitespace: u64,
    /// bytes `0x00..=0x1f`
    pub control: u64,
}

/// Everything stage 1 derives for one block, for differential testing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BlockMasks {
    pub raw: RawMasks,
    /// Bytes preceded by an unescaped backslash (including escaped backslashes).
    pub escaped: u64,
    /// Unescaped quotes.
    pub quotes: u64,
    /// String interiors: opening quote included, closing quote excluded.
    pub in_string: u64,
    /// Operators, opening and closing quotes and scalar starts outside strings.
    pub structurals: u64,
    /// Backslashes inside strings that begin an escape sequence.
    pub escape_starts: u64,
    /// Control bytes inside strings, which JSON forbids.
    pub string_controls: u64,
}

/// Carries between blocks. Masks are all-ones or zero in their top/low bit.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Carry {
    prev_escaped: u64,
    prev_in_string: u64,
    prev_scalar: u64,
}

const EVEN_BITS: u64 = 0x5555_5555_5555_5555;

/// Branchless odd-backslash-run detection with a one-bit carry; equal to the
/// scalar `compute_escaped_mask_64` reference (tested exhaustively on runs).
#[inline(always)]
pub fn escaped_mask_branchless(backslash: u64, prev_escaped: &mut u64) -> u64 {
    let backslash = backslash & !*prev_escaped;
    let follows_escape = (backslash << 1) | *prev_escaped;
    let odd_sequence_starts = backslash & !EVEN_BITS & !follows_escape;
    let (sequences_starting_on_even_bits, overflow) =
        odd_sequence_starts.overflowing_add(backslash);
    *prev_escaped = u64::from(overflow);
    let invert_mask = sequences_starting_on_even_bits << 1;
    (EVEN_BITS ^ invert_mask) & follows_escape
}

/// Prefix XOR by shifts: bit `i` is the parity of bits `0..=i`.
#[inline(always)]
pub fn prefix_xor_shifts(mut x: u64) -> u64 {
    x ^= x << 1;
    x ^= x << 2;
    x ^= x << 4;
    x ^= x << 8;
    x ^= x << 16;
    x ^= x << 32;
    x
}

/// First half of the shared block step: escapes and unescaped quotes.
#[inline(always)]
pub(crate) fn block_quotes(raw: &RawMasks, carry: &mut Carry) -> (u64, u64) {
    let escaped = escaped_mask_branchless(raw.backslash, &mut carry.prev_escaped);
    (escaped, raw.quote & !escaped)
}

/// Second half, given `prefix_xor(quotes)` from the ISA path.
#[inline(always)]
pub(crate) fn block_finish(
    raw: RawMasks,
    escaped: u64,
    quotes: u64,
    quote_parity: u64,
    carry: &mut Carry,
) -> BlockMasks {
    let in_string = quote_parity ^ carry.prev_in_string;
    carry.prev_in_string = ((in_string as i64) >> 63) as u64;
    let string_tail = in_string ^ quotes;
    let scalar = !(raw.op | raw.whitespace);
    let non_quote_scalar = scalar & !quotes;
    let follows_scalar = (non_quote_scalar << 1) | carry.prev_scalar;
    carry.prev_scalar = non_quote_scalar >> 63;
    let scalar_start = scalar & !follows_scalar;
    let closing_quotes = quotes & !in_string;
    let structurals = ((raw.op | scalar_start) & !string_tail) | closing_quotes;
    BlockMasks {
        raw,
        escaped,
        quotes,
        in_string,
        structurals,
        escape_starts: raw.backslash & !escaped & string_tail,
        string_controls: raw.control & string_tail,
    }
}

/// Stage 1 output, reusable across documents (capacity is retained).
///
/// Structural and escape positions share one `u32` buffer: structurals grow
/// from the front, escapes from the back, so the index never holds more
/// than [`IndexLimits::max_entries`] positions (4 bytes each) in total. A
/// document with more entries is *truncated*: every entry before
/// [`truncated_at`](Self::truncated_at) is kept, and stage 2 reports
/// `AllocationLimit` when it reaches that position, exactly where the scalar
/// parser's equivalent count runs out.
#[derive(Debug, Default)]
pub struct StructuralIndex {
    pub(crate) buf: Vec<u32>,
    pub(crate) structural_len: usize,
    pub(crate) escape_len: usize,
    pub(crate) first_string_control: Option<u32>,
    pub(crate) unclosed_string: bool,
    pub(crate) truncated_at: Option<u32>,
    isa: Option<Isa>,
}

/// Bounds for one stage 1 run. Capacity requests never exceed them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexLimits {
    pub max_input_bytes: usize,
    /// Structural plus escape positions kept, 4 bytes each.
    pub max_entries: usize,
}
impl Default for IndexLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024 * 1024,
            max_entries: 1024 * 1024,
        }
    }
}

/// Positions are `u32`, so one document is below 4 GiB.
pub const MAX_INDEX_INPUT: usize = (u32::MAX as usize) - 64;

pub(crate) struct Sink<'s> {
    /// Front: structurals; back (`buf.len() - escapes..`): escapes, newest
    /// lowest, reversed into increasing order when the run ends.
    buf: &'s mut Vec<u32>,
    structurals: usize,
    escapes: usize,
    max_entries: usize,
    /// Never grow `buf` past this many entries.
    cap_entries: usize,
    pub first_string_control: Option<u32>,
    pub truncated_at: Option<u32>,
    pub alloc_failed: bool,
}
impl<'s> Sink<'s> {
    pub(crate) fn new(buf: &'s mut Vec<u32>, input_len: usize, max_entries: usize) -> Self {
        // Structurals and escapes are distinct byte positions, so a document
        // never has more entries than bytes.
        let cap_entries = max_entries.min(input_len);
        Sink {
            buf,
            structurals: 0,
            escapes: 0,
            max_entries,
            cap_entries,
            first_string_control: None,
            truncated_at: None,
            alloc_failed: false,
        }
    }

    /// Make room for `n` more entries, growing geometrically up to the cap.
    #[cold]
    fn grow(&mut self, n: usize) -> bool {
        let used = self.structurals + self.escapes;
        let old = self.buf.len();
        let want = (used + n)
            .max(old.saturating_mul(2))
            .max(256)
            .min(self.cap_entries);
        if want < used + n {
            return false;
        }
        if self.buf.try_reserve_exact(want - old).is_err() {
            self.alloc_failed = true;
            return false;
        }
        self.buf.resize(want, 0);
        self.buf
            .copy_within(old - self.escapes..old, want - self.escapes);
        true
    }

    #[inline(always)]
    pub fn push(&mut self, m: &BlockMasks, base: u32) {
        if m.string_controls != 0 && self.first_string_control.is_none() {
            self.first_string_control = Some(base + m.string_controls.trailing_zeros());
        }
        if self.truncated_at.is_some() || self.alloc_failed {
            return;
        }
        let mut bits = m.structurals;
        let mut esc = m.escape_starts;
        let n = (bits.count_ones() + esc.count_ones()) as usize;
        if n == 0 {
            return;
        }
        let used = self.structurals + self.escapes;
        if used + n > self.max_entries {
            // Keep the entries before the first one over the limit.
            let all = bits | esc;
            let mut rest = all;
            for _ in 0..self.max_entries - used {
                rest &= rest - 1;
            }
            let cut = rest.trailing_zeros();
            self.truncated_at = Some(base + cut);
            let keep = (1u64 << cut) - 1;
            bits &= keep;
            esc &= keep;
        }
        let n = (bits.count_ones() + esc.count_ones()) as usize;
        if used + n > self.buf.len() && !self.grow(n) {
            return;
        }
        let mut at = self.structurals;
        while bits != 0 {
            self.buf[at] = base + bits.trailing_zeros();
            at += 1;
            bits &= bits - 1;
        }
        self.structurals = at;
        let end = self.buf.len();
        while esc != 0 {
            self.escapes += 1;
            self.buf[end - self.escapes] = base + esc.trailing_zeros();
            esc &= esc - 1;
        }
    }
}

/// Collects every block's masks, for tests and diagnostics.
pub(crate) trait Recorder {
    fn record(&mut self, masks: &BlockMasks);
}
pub(crate) struct NoRecord;
impl Recorder for NoRecord {
    #[inline(always)]
    fn record(&mut self, _: &BlockMasks) {}
}
impl Recorder for Vec<BlockMasks> {
    fn record(&mut self, masks: &BlockMasks) {
        self.push(*masks);
    }
}

/// Result of a kernel run, before error mapping.
pub(crate) struct RunOutcome {
    pub utf8_ok: bool,
    pub carry: Carry,
}

impl StructuralIndex {
    pub fn new() -> Self {
        Self::default()
    }
    /// Structural byte positions in increasing order.
    pub fn structurals(&self) -> &[u32] {
        &self.buf[..self.structural_len]
    }
    /// Positions of backslashes that begin an escape inside a string.
    pub fn escapes(&self) -> &[u32] {
        &self.buf[self.buf.len() - self.escape_len..]
    }
    /// The first control byte inside a string, if any (a JSON syntax error).
    pub fn first_string_control(&self) -> Option<u32> {
        self.first_string_control
    }
    /// Whether the input ended inside a string.
    pub fn unclosed_string(&self) -> bool {
        self.unclosed_string
    }
    /// The position of the first structural or escape that did not fit in
    /// `max_entries`, if any; every earlier entry is present.
    pub fn truncated_at(&self) -> Option<u32> {
        self.truncated_at
    }
    /// Bytes of index buffer currently held (retained between builds).
    pub fn capacity_bytes(&self) -> usize {
        self.buf.capacity() * size_of::<u32>()
    }
    /// The path that built this index.
    pub fn isa(&self) -> Option<Isa> {
        self.isa
    }

    fn reset(&mut self) {
        self.structural_len = 0;
        self.escape_len = 0;
        self.first_string_control = None;
        self.unclosed_string = false;
        self.truncated_at = None;
        self.isa = None;
    }

    /// Index `input` on `isa`. Invalid UTF-8 is an error here; string control
    /// bytes, unclosed strings and truncation at `max_entries` are recorded
    /// for stage 2, which reports them in document order. Errors leave the
    /// index empty. The buffer never grows past `max_entries` entries (nor
    /// past one entry per input byte).
    pub fn build(&mut self, input: &[u8], isa: Isa, limits: IndexLimits) -> Result<(), Error> {
        self.reset();
        if input.len() > limits.max_input_bytes || input.len() > MAX_INDEX_INPUT {
            return Err(Error::new(ErrorKind::InputLimit, 0));
        }
        if !isa.is_available() {
            return Err(Error::new(ErrorKind::IsaUnavailable, 0));
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
        self.isa = Some(isa);
        Ok(())
    }
}

fn finish(input: &[u8], outcome: &RunOutcome, alloc: bool) -> Result<(), Error> {
    if !outcome.utf8_ok {
        // The kernel decides; std only locates the first bad byte. If they
        // ever disagreed this still reports a typed error, never success.
        let offset = core::str::from_utf8(input)
            .err()
            .map_or(0, |e| e.valid_up_to());
        return Err(Error::new(ErrorKind::Utf8, offset));
    }
    if alloc {
        return Err(Error::new(ErrorKind::AllocationFailed, 0));
    }
    Ok(())
}

pub(crate) fn run<R: Recorder>(
    isa: Isa,
    input: &[u8],
    sink: &mut Sink<'_>,
    rec: &mut R,
) -> Result<RunOutcome, Error> {
    match isa {
        Isa::Scalar => Ok(scalar::run(input, sink, rec)),
        other => {
            kernels::run(other, input, sink, rec).ok_or(Error::new(ErrorKind::IsaUnavailable, 0))
        }
    }
}

/// Per-block masks for `input` on `isa`, the final carries' string state and
/// the UTF-8 verdict. Intended for differential tests; allocates freely.
pub fn block_masks(input: &[u8], isa: Isa) -> Result<(Vec<BlockMasks>, bool), Error> {
    if input.len() > MAX_INDEX_INPUT {
        return Err(Error::new(ErrorKind::InputLimit, 0));
    }
    if !isa.is_available() {
        return Err(Error::new(ErrorKind::IsaUnavailable, 0));
    }
    let mut buf = Vec::new();
    let mut sink = Sink::new(&mut buf, input.len(), usize::MAX);
    let mut rec = Vec::new();
    let outcome = run(isa, input, &mut sink, &mut rec)?;
    Ok((rec, outcome.utf8_ok))
}

/// Copy the final partial block, padded with spaces as `load_masked(.., 0x20)`.
#[inline(always)]
pub(crate) fn padded_tail(rest: &[u8]) -> [u8; 64] {
    let mut block = [b' '; 64];
    block[..rest.len()].copy_from_slice(rest);
    block
}

mod scalar {
    use super::*;

    fn classify(block: &[u8; 64]) -> RawMasks {
        let mut m = RawMasks::default();
        for (i, &b) in block.iter().enumerate() {
            let bit = 1u64 << i;
            match b {
                b'\\' => m.backslash |= bit,
                b'"' => m.quote |= bit,
                b'{' | b'}' | b'[' | b']' | b':' | b',' => m.op |= bit,
                _ => {}
            }
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                m.whitespace |= bit;
            }
            if b < 0x20 {
                m.control |= bit;
            }
        }
        m
    }

    /// Independent per-bit reference for quotes and string interiors.
    fn reference_block(raw: RawMasks, carry: &mut Carry) -> BlockMasks {
        let (escaped, carry_out) =
            crate::compute_escaped_mask_64(raw.backslash, carry.prev_escaped != 0);
        carry.prev_escaped = u64::from(carry_out);
        let quotes = raw.quote & !escaped;
        let mut parity = 0u64;
        let mut inside = false;
        for i in 0..64 {
            if quotes & (1 << i) != 0 {
                inside = !inside;
            }
            if inside {
                parity |= 1 << i;
            }
        }
        block_finish(raw, escaped, quotes, parity, carry)
    }

    pub(super) fn run<R: Recorder>(input: &[u8], sink: &mut Sink<'_>, rec: &mut R) -> RunOutcome {
        let mut carry = Carry::default();
        let (blocks, rest) = input.as_chunks::<64>();
        let mut base = 0u32;
        for block in blocks {
            let m = reference_block(classify(block), &mut carry);
            rec.record(&m);
            sink.push(&m, base);
            base += 64;
        }
        if !rest.is_empty() {
            let m = reference_block(classify(&padded_tail(rest)), &mut carry);
            rec.record(&m);
            sink.push(&m, base);
        }
        RunOutcome {
            utf8_ok: core::str::from_utf8(input).is_ok(),
            carry,
        }
    }
}
