//! The only module in this crate that may use `unsafe`: `std::arch` stage 1
//! kernels for x86_64 (SSE4.2, AVX2, AVX-512BW with VPCLMULQDQ) and AArch64 (NEON). Each path is a
//! `#[target_feature]` function entered only after run-time detection of every
//! feature it enables; the remaining `unsafe` blocks are unaligned loads from
//! `&[u8; 16]`/`&[u8; 64]` references, which are always in bounds.
//!
//! Classification uses nibble-indexed shuffle tables for operators and
//! whitespace; UTF-8 validation is the lookup-table algorithm of Keiser and
//! Lemire ("Validating UTF-8 in less than one instruction per byte", 2021),
//! three 16-entry tables indexed by the high and low nibbles of the previous
//! byte and the high nibble of the current byte. Prefix XOR uses a carry-less
//! multiply by all-ones (PCLMULQDQ / PMULL).
#![allow(unsafe_code)]
// Not every table or helper is used on every target architecture.
#![allow(dead_code)]

use super::{Isa, Recorder, RunOutcome, Sink};

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
const BYTE_1_HIGH: [u8; 16] = [
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
const BYTE_1_LOW: [u8; 16] = [
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
const BYTE_2_HIGH: [u8; 16] = [
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
/// `table[b & 15] == b` exactly for space, tab, line feed and carriage return;
/// the filler entries are chosen so that no other byte matches its slot.
const WS_TABLE: [u8; 16] = [
    b' ', 100, 100, 100, 17, 100, 113, 2, 100, b'\t', b'\n', 112, 100, b'\r', 100, 100,
];
/// `table[b & 15] == b | 0x20` for `: { } , [ ]` and, before control bytes are
/// masked out, for 0x0c and 0x1a.
const OP_TABLE: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, b':', b'{', b',', b'}', 0, 0];
/// A lane is incomplete when one of its last three bytes starts a sequence
/// that needs more bytes than remain in the lane.
const INCOMPLETE_16: [u8; 16] = [
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0xef, 0xdf, 0xbf,
];

pub(super) fn sse42_available() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("sse4.2")
            && std::arch::is_x86_feature_detected!("pclmulqdq")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}
pub(super) fn avx2_available() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("pclmulqdq")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}
pub(super) fn avx512_available() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512bw")
            && std::arch::is_x86_feature_detected!("vpclmulqdq")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}
/// Whether the CPU reports SVE. There is no SVE path: Rust's SVE intrinsics
/// are not stable, so SVE hosts run the NEON path and this is reported.
pub(super) fn sve_reported() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("sve")
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        false
    }
}
pub(super) fn neon_available() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("neon")
            && std::arch::is_aarch64_feature_detected!("aes")
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        false
    }
}

/// Run a SIMD path, or `None` when this CPU cannot run it.
pub(super) fn run<R: Recorder>(
    isa: Isa,
    input: &[u8],
    sink: &mut Sink<'_>,
    rec: &mut R,
) -> Option<RunOutcome> {
    match isa {
        #[cfg(target_arch = "x86_64")]
        Isa::Sse42 if sse42_available() => {
            // SAFETY: every feature `run_sse42` enables was detected above.
            Some(unsafe { x86::run_sse42(input, sink, rec) })
        }
        #[cfg(target_arch = "x86_64")]
        Isa::Avx2 if avx2_available() => {
            // SAFETY: every feature `run_avx2` enables was detected above.
            Some(unsafe { x86::run_avx2(input, sink, rec) })
        }
        #[cfg(target_arch = "x86_64")]
        Isa::Avx512 if avx512_available() => {
            // SAFETY: every feature `run_avx512` enables was detected above.
            Some(unsafe { x86::run_avx512(input, sink, rec) })
        }
        #[cfg(target_arch = "aarch64")]
        Isa::Neon if neon_available() => {
            // SAFETY: every feature `run_neon` enables was detected above.
            Some(unsafe { arm::run_neon(input, sink, rec) })
        }
        _ => {
            let _ = (input, sink, rec);
            None
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::super::{
        BlockMasks, Carry, RawMasks, Recorder, RunOutcome, Sink, block_finish, block_quotes,
        padded_tail,
    };
    use super::*;
    use core::arch::x86_64::*;

    #[target_feature(enable = "sse2,pclmulqdq")]
    fn clmul_prefix_xor(x: u64) -> u64 {
        let a = _mm_set_epi64x(0, x as i64);
        let ones = _mm_set1_epi8(-1);
        _mm_cvtsi128_si64(_mm_clmulepi64_si128::<0>(a, ones)) as u64
    }

    #[target_feature(enable = "sse2")]
    fn load16(t: &[u8; 16]) -> __m128i {
        // SAFETY: `t` is 16 readable bytes; the load is unaligned.
        unsafe { _mm_loadu_si128(t.as_ptr().cast()) }
    }

    // ----- SSE4.2: four 16-byte lanes per block -----

    struct Sse {
        ws: __m128i,
        op: __m128i,
        b1h: __m128i,
        b1l: __m128i,
        b2h: __m128i,
        incomplete: __m128i,
        error: __m128i,
        prev_input: __m128i,
        prev_incomplete: __m128i,
    }

    impl Sse {
        #[target_feature(enable = "sse4.2")]
        fn new() -> Self {
            Self {
                ws: load16(&WS_TABLE),
                op: load16(&OP_TABLE),
                b1h: load16(&BYTE_1_HIGH),
                b1l: load16(&BYTE_1_LOW),
                b2h: load16(&BYTE_2_HIGH),
                incomplete: load16(&INCOMPLETE_16),
                error: _mm_setzero_si128(),
                prev_input: _mm_setzero_si128(),
                prev_incomplete: _mm_setzero_si128(),
            }
        }

        #[target_feature(enable = "sse4.2")]
        #[inline]
        fn classify(&self, v: [__m128i; 4]) -> RawMasks {
            let max = _mm_set1_epi8(0x1f);
            let curly = _mm_set1_epi8(0x20);
            let bs = _mm_set1_epi8(b'\\' as i8);
            let qt = _mm_set1_epi8(b'"' as i8);
            let mut m = RawMasks::default();
            for (i, x) in v.into_iter().enumerate() {
                let shift = 16 * i;
                let bits = |y: __m128i| u64::from(_mm_movemask_epi8(y) as u32 as u16) << shift;
                m.control |= bits(_mm_cmpeq_epi8(_mm_max_epu8(x, max), max));
                m.op |= bits(_mm_cmpeq_epi8(
                    _mm_shuffle_epi8(self.op, x),
                    _mm_or_si128(x, curly),
                ));
                m.whitespace |= bits(_mm_cmpeq_epi8(_mm_shuffle_epi8(self.ws, x), x));
                m.backslash |= bits(_mm_cmpeq_epi8(x, bs));
                m.quote |= bits(_mm_cmpeq_epi8(x, qt));
            }
            m.op &= !m.control;
            m
        }

        #[target_feature(enable = "sse4.2")]
        #[inline]
        fn check(&mut self, input: __m128i, prev_input: __m128i) {
            let prev1 = _mm_alignr_epi8::<15>(input, prev_input);
            let prev2 = _mm_alignr_epi8::<14>(input, prev_input);
            let prev3 = _mm_alignr_epi8::<13>(input, prev_input);
            let lo4 = _mm_set1_epi8(0x0f);
            let b1h = _mm_shuffle_epi8(self.b1h, _mm_and_si128(_mm_srli_epi16::<4>(prev1), lo4));
            let b1l = _mm_shuffle_epi8(self.b1l, _mm_and_si128(prev1, lo4));
            let b2h = _mm_shuffle_epi8(self.b2h, _mm_and_si128(_mm_srli_epi16::<4>(input), lo4));
            let special = _mm_and_si128(_mm_and_si128(b1h, b1l), b2h);
            let third = _mm_subs_epu8(prev2, _mm_set1_epi8(0x60));
            let fourth = _mm_subs_epu8(prev3, _mm_set1_epi8(0x70));
            let must23 = _mm_and_si128(_mm_or_si128(third, fourth), _mm_set1_epi8(0x80u8 as i8));
            self.error = _mm_or_si128(self.error, _mm_xor_si128(must23, special));
        }

        #[target_feature(enable = "sse4.2")]
        #[inline]
        fn utf8(&mut self, v: [__m128i; 4]) {
            let any = _mm_or_si128(_mm_or_si128(v[0], v[1]), _mm_or_si128(v[2], v[3]));
            if _mm_movemask_epi8(any) == 0 {
                self.error = _mm_or_si128(self.error, self.prev_incomplete);
                self.prev_incomplete = _mm_setzero_si128();
            } else {
                self.check(v[0], self.prev_input);
                self.check(v[1], v[0]);
                self.check(v[2], v[1]);
                self.check(v[3], v[2]);
                self.prev_incomplete = _mm_subs_epu8(v[3], self.incomplete);
            }
            self.prev_input = v[3];
        }

        #[target_feature(enable = "sse4.2")]
        fn ok(&self) -> bool {
            let e = _mm_or_si128(self.error, self.prev_incomplete);
            _mm_testz_si128(e, e) == 1
        }

        #[target_feature(enable = "sse4.2,pclmulqdq")]
        #[inline]
        fn block(&mut self, block: &[u8; 64], carry: &mut Carry) -> BlockMasks {
            let lanes: &[[u8; 16]; 4] = bytemuck_lanes(block);
            let v = [
                load16(&lanes[0]),
                load16(&lanes[1]),
                load16(&lanes[2]),
                load16(&lanes[3]),
            ];
            let raw = self.classify(v);
            self.utf8(v);
            let (escaped, quotes) = block_quotes(&raw, carry);
            block_finish(raw, escaped, quotes, clmul_prefix_xor(quotes), carry)
        }
    }

    fn bytemuck_lanes(block: &[u8; 64]) -> &[[u8; 16]; 4] {
        let (lanes, rest) = block.as_chunks::<16>();
        debug_assert!(rest.is_empty());
        lanes.try_into().expect("64 bytes are four lanes")
    }

    #[target_feature(enable = "sse4.2,pclmulqdq")]
    pub(super) fn run_sse42<R: Recorder>(
        input: &[u8],
        sink: &mut Sink<'_>,
        rec: &mut R,
    ) -> RunOutcome {
        let mut carry = Carry::default();
        let mut k = Sse::new();
        let (blocks, rest) = input.as_chunks::<64>();
        let mut base = 0u32;
        for block in blocks {
            let m = k.block(block, &mut carry);
            rec.record(&m);
            sink.push(&m, base);
            base = base.wrapping_add(64);
        }
        if !rest.is_empty() {
            let m = k.block(&padded_tail(rest), &mut carry);
            rec.record(&m);
            sink.push(&m, base);
        }
        RunOutcome {
            utf8_ok: k.ok(),
            carry,
        }
    }

    // ----- AVX2: two 32-byte lanes per block -----

    struct Avx {
        ws: __m256i,
        op: __m256i,
        b1h: __m256i,
        b1l: __m256i,
        b2h: __m256i,
        incomplete: __m256i,
        error: __m256i,
        prev_input: __m256i,
        prev_incomplete: __m256i,
    }

    #[target_feature(enable = "avx2")]
    fn broadcast(t: &[u8; 16]) -> __m256i {
        _mm256_broadcastsi128_si256(load16(t))
    }

    #[target_feature(enable = "avx2")]
    fn load32(t: &[u8; 32]) -> __m256i {
        // SAFETY: `t` is 32 readable bytes; the load is unaligned.
        unsafe { _mm256_loadu_si256(t.as_ptr().cast()) }
    }

    impl Avx {
        #[target_feature(enable = "avx2")]
        fn new() -> Self {
            let mut incomplete = [255u8; 32];
            incomplete[16..].copy_from_slice(&INCOMPLETE_16);
            Self {
                ws: broadcast(&WS_TABLE),
                op: broadcast(&OP_TABLE),
                b1h: broadcast(&BYTE_1_HIGH),
                b1l: broadcast(&BYTE_1_LOW),
                b2h: broadcast(&BYTE_2_HIGH),
                incomplete: load32(&incomplete),
                error: _mm256_setzero_si256(),
                prev_input: _mm256_setzero_si256(),
                prev_incomplete: _mm256_setzero_si256(),
            }
        }

        #[target_feature(enable = "avx2")]
        #[inline]
        fn bits(a: __m256i, b: __m256i) -> u64 {
            let lo = _mm256_movemask_epi8(a) as u32 as u64;
            let hi = _mm256_movemask_epi8(b) as u32 as u64;
            lo | (hi << 32)
        }

        #[target_feature(enable = "avx2")]
        #[inline]
        fn classify(&self, a: __m256i, b: __m256i) -> RawMasks {
            let max = _mm256_set1_epi8(0x1f);
            let ctl = |x| _mm256_cmpeq_epi8(_mm256_max_epu8(x, max), max);
            let curly = _mm256_set1_epi8(0x20);
            let op =
                |x| _mm256_cmpeq_epi8(_mm256_shuffle_epi8(self.op, x), _mm256_or_si256(x, curly));
            let ws = |x| _mm256_cmpeq_epi8(_mm256_shuffle_epi8(self.ws, x), x);
            let bs = _mm256_set1_epi8(b'\\' as i8);
            let qt = _mm256_set1_epi8(b'"' as i8);
            let control = Self::bits(ctl(a), ctl(b));
            RawMasks {
                backslash: Self::bits(_mm256_cmpeq_epi8(a, bs), _mm256_cmpeq_epi8(b, bs)),
                quote: Self::bits(_mm256_cmpeq_epi8(a, qt), _mm256_cmpeq_epi8(b, qt)),
                op: Self::bits(op(a), op(b)) & !control,
                whitespace: Self::bits(ws(a), ws(b)),
                control,
            }
        }

        #[target_feature(enable = "avx2")]
        #[inline]
        fn check(&mut self, input: __m256i, prev_input: __m256i) {
            let shifted = _mm256_permute2x128_si256::<0x21>(prev_input, input);
            let prev1 = _mm256_alignr_epi8::<15>(input, shifted);
            let prev2 = _mm256_alignr_epi8::<14>(input, shifted);
            let prev3 = _mm256_alignr_epi8::<13>(input, shifted);
            let lo4 = _mm256_set1_epi8(0x0f);
            let b1h = _mm256_shuffle_epi8(
                self.b1h,
                _mm256_and_si256(_mm256_srli_epi16::<4>(prev1), lo4),
            );
            let b1l = _mm256_shuffle_epi8(self.b1l, _mm256_and_si256(prev1, lo4));
            let b2h = _mm256_shuffle_epi8(
                self.b2h,
                _mm256_and_si256(_mm256_srli_epi16::<4>(input), lo4),
            );
            let special = _mm256_and_si256(_mm256_and_si256(b1h, b1l), b2h);
            let third = _mm256_subs_epu8(prev2, _mm256_set1_epi8(0x60));
            let fourth = _mm256_subs_epu8(prev3, _mm256_set1_epi8(0x70));
            let must23 = _mm256_and_si256(
                _mm256_or_si256(third, fourth),
                _mm256_set1_epi8(0x80u8 as i8),
            );
            self.error = _mm256_or_si256(self.error, _mm256_xor_si256(must23, special));
        }

        #[target_feature(enable = "avx2")]
        #[inline]
        fn utf8(&mut self, a: __m256i, b: __m256i) {
            if _mm256_movemask_epi8(_mm256_or_si256(a, b)) == 0 {
                self.error = _mm256_or_si256(self.error, self.prev_incomplete);
                self.prev_incomplete = _mm256_setzero_si256();
            } else {
                self.check(a, self.prev_input);
                self.check(b, a);
                self.prev_incomplete = _mm256_subs_epu8(b, self.incomplete);
            }
            self.prev_input = b;
        }

        #[target_feature(enable = "avx2")]
        fn ok(&self) -> bool {
            let e = _mm256_or_si256(self.error, self.prev_incomplete);
            _mm256_testz_si256(e, e) == 1
        }

        #[target_feature(enable = "avx2,pclmulqdq")]
        #[inline]
        fn block(&mut self, block: &[u8; 64], carry: &mut Carry) -> BlockMasks {
            let (halves, _) = block.as_chunks::<32>();
            let a = load32(&halves[0]);
            let b = load32(&halves[1]);
            let raw = self.classify(a, b);
            self.utf8(a, b);
            let (escaped, quotes) = block_quotes(&raw, carry);
            block_finish(raw, escaped, quotes, clmul_prefix_xor(quotes), carry)
        }
    }

    #[target_feature(enable = "avx2,pclmulqdq")]
    pub(super) fn run_avx2<R: Recorder>(
        input: &[u8],
        sink: &mut Sink<'_>,
        rec: &mut R,
    ) -> RunOutcome {
        let mut carry = Carry::default();
        let mut k = Avx::new();
        let (blocks, rest) = input.as_chunks::<64>();
        let mut base = 0u32;
        for block in blocks {
            let m = k.block(block, &mut carry);
            rec.record(&m);
            sink.push(&m, base);
            base = base.wrapping_add(64);
        }
        if !rest.is_empty() {
            let m = k.block(&padded_tail(rest), &mut carry);
            rec.record(&m);
            sink.push(&m, base);
        }
        RunOutcome {
            utf8_ok: k.ok(),
            carry,
        }
    }

    // ----- AVX-512BW: one 64-byte register per block, four blocks per step -----

    struct Avx512 {
        ws: __m512i,
        op: __m512i,
        b1h: __m512i,
        b1l: __m512i,
        b2h: __m512i,
        incomplete: __m512i,
        error: __m512i,
        prev_input: __m512i,
        prev_incomplete: __m512i,
    }

    #[target_feature(enable = "avx512f")]
    fn broadcast512(t: &[u8; 16]) -> __m512i {
        _mm512_broadcast_i32x4(load16(t))
    }

    #[target_feature(enable = "avx512f")]
    fn load64(t: &[u8; 64]) -> __m512i {
        // SAFETY: `t` is 64 readable bytes; the load is unaligned.
        unsafe { _mm512_loadu_si512(t.as_ptr().cast()) }
    }

    /// Prefix XOR of four 64-bit quote masks with one VPCLMULQDQ: each mask
    /// sits in the low quadword of its own 128-bit lane and is multiplied,
    /// carry-less, by all ones.
    #[target_feature(enable = "avx512f,vpclmulqdq")]
    fn clmul_prefix_xor4(q: [u64; 4]) -> [u64; 4] {
        let a = _mm512_set_epi64(
            0,
            q[3] as i64,
            0,
            q[2] as i64,
            0,
            q[1] as i64,
            0,
            q[0] as i64,
        );
        let r = _mm512_clmulepi64_epi128::<0>(a, _mm512_set1_epi8(-1));
        let mut out = [0u64; 8];
        // SAFETY: `out` is 64 writable bytes; the store is unaligned.
        unsafe { _mm512_storeu_si512(out.as_mut_ptr().cast(), r) };
        [out[0], out[2], out[4], out[6]]
    }

    impl Avx512 {
        #[target_feature(enable = "avx512f,avx512bw")]
        fn new() -> Self {
            let mut incomplete = [255u8; 64];
            incomplete[48..].copy_from_slice(&INCOMPLETE_16);
            Self {
                ws: broadcast512(&WS_TABLE),
                op: broadcast512(&OP_TABLE),
                b1h: broadcast512(&BYTE_1_HIGH),
                b1l: broadcast512(&BYTE_1_LOW),
                b2h: broadcast512(&BYTE_2_HIGH),
                incomplete: load64(&incomplete),
                error: _mm512_setzero_si512(),
                prev_input: _mm512_setzero_si512(),
                prev_incomplete: _mm512_setzero_si512(),
            }
        }

        #[target_feature(enable = "avx512f,avx512bw")]
        #[inline]
        fn classify(&self, x: __m512i) -> RawMasks {
            let control = _mm512_cmple_epu8_mask(x, _mm512_set1_epi8(0x1f));
            let op = _mm512_cmpeq_epi8_mask(
                _mm512_shuffle_epi8(self.op, x),
                _mm512_or_si512(x, _mm512_set1_epi8(0x20)),
            );
            RawMasks {
                backslash: _mm512_cmpeq_epi8_mask(x, _mm512_set1_epi8(b'\\' as i8)),
                quote: _mm512_cmpeq_epi8_mask(x, _mm512_set1_epi8(b'"' as i8)),
                op: op & !control,
                whitespace: _mm512_cmpeq_epi8_mask(_mm512_shuffle_epi8(self.ws, x), x),
                control,
            }
        }

        #[target_feature(enable = "avx512f,avx512bw")]
        #[inline]
        fn check(&mut self, input: __m512i, prev_input: __m512i) {
            // 128-bit lanes [prev.3, in.0, in.1, in.2], so that `alignr`
            // within each lane sees the bytes just before it.
            let idx = _mm512_set_epi64(13, 12, 11, 10, 9, 8, 7, 6);
            let shifted = _mm512_permutex2var_epi64(prev_input, idx, input);
            let prev1 = _mm512_alignr_epi8::<15>(input, shifted);
            let prev2 = _mm512_alignr_epi8::<14>(input, shifted);
            let prev3 = _mm512_alignr_epi8::<13>(input, shifted);
            let lo4 = _mm512_set1_epi8(0x0f);
            let b1h = _mm512_shuffle_epi8(
                self.b1h,
                _mm512_and_si512(_mm512_srli_epi16::<4>(prev1), lo4),
            );
            let b1l = _mm512_shuffle_epi8(self.b1l, _mm512_and_si512(prev1, lo4));
            let b2h = _mm512_shuffle_epi8(
                self.b2h,
                _mm512_and_si512(_mm512_srli_epi16::<4>(input), lo4),
            );
            let special = _mm512_and_si512(_mm512_and_si512(b1h, b1l), b2h);
            let third = _mm512_subs_epu8(prev2, _mm512_set1_epi8(0x60));
            let fourth = _mm512_subs_epu8(prev3, _mm512_set1_epi8(0x70));
            let must23 = _mm512_and_si512(
                _mm512_or_si512(third, fourth),
                _mm512_set1_epi8(0x80u8 as i8),
            );
            self.error = _mm512_or_si512(self.error, _mm512_xor_si512(must23, special));
        }

        #[target_feature(enable = "avx512f,avx512bw")]
        #[inline]
        fn utf8(&mut self, x: __m512i) {
            if _mm512_movepi8_mask(x) == 0 {
                self.error = _mm512_or_si512(self.error, self.prev_incomplete);
                self.prev_incomplete = _mm512_setzero_si512();
            } else {
                self.check(x, self.prev_input);
                self.prev_incomplete = _mm512_subs_epu8(x, self.incomplete);
            }
            self.prev_input = x;
        }

        #[target_feature(enable = "avx512f,avx512bw")]
        fn ok(&self) -> bool {
            let e = _mm512_or_si512(self.error, self.prev_incomplete);
            _mm512_test_epi8_mask(e, e) == 0
        }

        /// Up to four blocks: classify and validate each, run the escape
        /// carry chain in order, then one VPCLMULQDQ for all quote parities.
        #[target_feature(enable = "avx512f,avx512bw,vpclmulqdq")]
        #[inline]
        fn blocks<R: Recorder>(
            &mut self,
            blocks: &[&[u8; 64]],
            carry: &mut Carry,
            sink: &mut Sink<'_>,
            rec: &mut R,
            base: &mut u32,
        ) {
            let mut raw = [RawMasks::default(); 4];
            let mut escaped = [0u64; 4];
            let mut quotes = [0u64; 4];
            for (k, block) in blocks.iter().enumerate() {
                let x = load64(block);
                raw[k] = self.classify(x);
                self.utf8(x);
                (escaped[k], quotes[k]) = block_quotes(&raw[k], carry);
            }
            let parity = clmul_prefix_xor4(quotes);
            for k in 0..blocks.len() {
                let m = block_finish(raw[k], escaped[k], quotes[k], parity[k], carry);
                rec.record(&m);
                sink.push(&m, *base);
                *base = base.wrapping_add(64);
            }
        }
    }

    #[target_feature(enable = "avx512f,avx512bw,vpclmulqdq")]
    pub(super) fn run_avx512<R: Recorder>(
        input: &[u8],
        sink: &mut Sink<'_>,
        rec: &mut R,
    ) -> RunOutcome {
        let mut carry = Carry::default();
        let mut k = Avx512::new();
        let (blocks, rest) = input.as_chunks::<64>();
        let mut base = 0u32;
        let (quads, singles) = blocks.as_chunks::<4>();
        for quad in quads {
            let refs = [&quad[0], &quad[1], &quad[2], &quad[3]];
            k.blocks(&refs, &mut carry, sink, rec, &mut base);
        }
        let tail = padded_tail(rest);
        let mut last: [&[u8; 64]; 4] = [&tail; 4];
        let mut n = 0;
        for block in singles {
            last[n] = block;
            n += 1;
        }
        if !rest.is_empty() {
            last[n] = &tail;
            n += 1;
        }
        if n > 0 {
            k.blocks(&last[..n], &mut carry, sink, rec, &mut base);
        }
        RunOutcome {
            utf8_ok: k.ok(),
            carry,
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::super::{
        BlockMasks, Carry, RawMasks, Recorder, RunOutcome, Sink, block_finish, block_quotes,
        padded_tail,
    };
    use super::*;
    use core::arch::aarch64::*;

    const BIT_WEIGHTS: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];

    #[target_feature(enable = "neon")]
    fn load16(t: &[u8; 16]) -> uint8x16_t {
        // SAFETY: `t` is 16 readable bytes; NEON loads need no alignment.
        unsafe { vld1q_u8(t.as_ptr()) }
    }

    #[target_feature(enable = "neon,aes")]
    fn pmull_prefix_xor(x: u64) -> u64 {
        vmull_p64(x, u64::MAX) as u64
    }

    struct Neon {
        ws: uint8x16_t,
        op: uint8x16_t,
        b1h: uint8x16_t,
        b1l: uint8x16_t,
        b2h: uint8x16_t,
        incomplete: uint8x16_t,
        weights: uint8x16_t,
        error: uint8x16_t,
        prev_input: uint8x16_t,
        prev_incomplete: uint8x16_t,
    }

    impl Neon {
        #[target_feature(enable = "neon")]
        fn new() -> Self {
            Self {
                ws: load16(&WS_TABLE),
                op: load16(&OP_TABLE),
                b1h: load16(&BYTE_1_HIGH),
                b1l: load16(&BYTE_1_LOW),
                b2h: load16(&BYTE_2_HIGH),
                incomplete: load16(&INCOMPLETE_16),
                weights: load16(&BIT_WEIGHTS),
                error: vdupq_n_u8(0),
                prev_input: vdupq_n_u8(0),
                prev_incomplete: vdupq_n_u8(0),
            }
        }

        #[target_feature(enable = "neon")]
        #[inline]
        fn bits(&self, m: [uint8x16_t; 4]) -> u64 {
            let t0 = vandq_u8(m[0], self.weights);
            let t1 = vandq_u8(m[1], self.weights);
            let t2 = vandq_u8(m[2], self.weights);
            let t3 = vandq_u8(m[3], self.weights);
            let s0 = vpaddq_u8(t0, t1);
            let s1 = vpaddq_u8(t2, t3);
            let s = vpaddq_u8(s0, s1);
            let s = vpaddq_u8(s, s);
            vgetq_lane_u64::<0>(vreinterpretq_u64_u8(s))
        }

        #[target_feature(enable = "neon")]
        #[inline]
        fn classify(&self, v: [uint8x16_t; 4]) -> RawMasks {
            let lo4 = vdupq_n_u8(0x0f);
            let max = vdupq_n_u8(0x1f);
            let curly = vdupq_n_u8(0x20);
            let bs = vdupq_n_u8(b'\\');
            let qt = vdupq_n_u8(b'"');
            let mut control = [vdupq_n_u8(0); 4];
            let mut op = control;
            let mut ws = control;
            let mut backslash = control;
            let mut quote = control;
            for (i, x) in v.into_iter().enumerate() {
                let nib = vandq_u8(x, lo4);
                control[i] = vcleq_u8(x, max);
                op[i] = vceqq_u8(vqtbl1q_u8(self.op, nib), vorrq_u8(x, curly));
                ws[i] = vceqq_u8(vqtbl1q_u8(self.ws, nib), x);
                backslash[i] = vceqq_u8(x, bs);
                quote[i] = vceqq_u8(x, qt);
            }
            let control = self.bits(control);
            RawMasks {
                backslash: self.bits(backslash),
                quote: self.bits(quote),
                op: self.bits(op) & !control,
                whitespace: self.bits(ws),
                control,
            }
        }

        #[target_feature(enable = "neon")]
        #[inline]
        fn check(&mut self, input: uint8x16_t, prev_input: uint8x16_t) {
            let prev1 = vextq_u8::<15>(prev_input, input);
            let prev2 = vextq_u8::<14>(prev_input, input);
            let prev3 = vextq_u8::<13>(prev_input, input);
            let lo4 = vdupq_n_u8(0x0f);
            let b1h = vqtbl1q_u8(self.b1h, vshrq_n_u8::<4>(prev1));
            let b1l = vqtbl1q_u8(self.b1l, vandq_u8(prev1, lo4));
            let b2h = vqtbl1q_u8(self.b2h, vshrq_n_u8::<4>(input));
            let special = vandq_u8(vandq_u8(b1h, b1l), b2h);
            let third = vqsubq_u8(prev2, vdupq_n_u8(0x60));
            let fourth = vqsubq_u8(prev3, vdupq_n_u8(0x70));
            let must23 = vandq_u8(vorrq_u8(third, fourth), vdupq_n_u8(0x80));
            self.error = vorrq_u8(self.error, veorq_u8(must23, special));
        }

        #[target_feature(enable = "neon")]
        #[inline]
        fn utf8(&mut self, v: [uint8x16_t; 4]) {
            let any = vorrq_u8(vorrq_u8(v[0], v[1]), vorrq_u8(v[2], v[3]));
            if vmaxvq_u8(any) < 0x80 {
                self.error = vorrq_u8(self.error, self.prev_incomplete);
                self.prev_incomplete = vdupq_n_u8(0);
            } else {
                self.check(v[0], self.prev_input);
                self.check(v[1], v[0]);
                self.check(v[2], v[1]);
                self.check(v[3], v[2]);
                self.prev_incomplete = vqsubq_u8(v[3], self.incomplete);
            }
            self.prev_input = v[3];
        }

        #[target_feature(enable = "neon")]
        fn ok(&self) -> bool {
            vmaxvq_u8(vorrq_u8(self.error, self.prev_incomplete)) == 0
        }

        #[target_feature(enable = "neon,aes")]
        #[inline]
        fn block(&mut self, block: &[u8; 64], carry: &mut Carry) -> BlockMasks {
            let (lanes, _) = block.as_chunks::<16>();
            let v = [
                load16(&lanes[0]),
                load16(&lanes[1]),
                load16(&lanes[2]),
                load16(&lanes[3]),
            ];
            let raw = self.classify(v);
            self.utf8(v);
            let (escaped, quotes) = block_quotes(&raw, carry);
            block_finish(raw, escaped, quotes, pmull_prefix_xor(quotes), carry)
        }
    }

    #[target_feature(enable = "neon,aes")]
    pub(super) fn run_neon<R: Recorder>(
        input: &[u8],
        sink: &mut Sink<'_>,
        rec: &mut R,
    ) -> RunOutcome {
        let mut carry = Carry::default();
        let mut k = Neon::new();
        let (blocks, rest) = input.as_chunks::<64>();
        let mut base = 0u32;
        for block in blocks {
            let m = k.block(block, &mut carry);
            rec.record(&m);
            sink.push(&m, base);
            base = base.wrapping_add(64);
        }
        if !rest.is_empty() {
            let m = k.block(&padded_tail(rest), &mut carry);
            rec.record(&m);
            sink.push(&m, base);
        }
        RunOutcome {
            utf8_ok: k.ok(),
            carry,
        }
    }
}
