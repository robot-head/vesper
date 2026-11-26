//! The portable scalar arm of each [`VVec`](super::VVec) and
//! [`VMask`](super::VMask) operation. [`reference`] is the Rust version;
//! with the `vesper-port` feature the arms run the Vesper port
//! (`vesper/src/vvec.ves`, #1969), which takes the lane and mask buffers in
//! place.
#[cfg(feature = "vesper-port")]
use super::{BinOp, CmpOp, MASK_WORDS, MAX_LANES, MaskOp};

#[cfg(not(feature = "vesper-port"))]
pub(super) use reference::*;

#[cfg(feature = "vesper-port")]
pub(super) fn prefix_bits(n: usize, bits: &mut [u64; MASK_WORDS]) {
    crate::port::vvec_prefix_bits(n as u64, bits);
}

#[cfg(feature = "vesper-port")]
pub(super) fn mask_zip(op: MaskOp, lanes: usize, a: &mut [u64; MASK_WORDS], b: &[u64; MASK_WORDS]) {
    crate::port::vvec_mask_zip(op as u64, lanes as u64, a, b);
}

#[cfg(feature = "vesper-port")]
pub(super) fn splat(value: u8, n: usize, lanes: &mut [u8; MAX_LANES]) {
    crate::port::vvec_splat(value, n as u64, lanes);
}

#[cfg(feature = "vesper-port")]
pub(super) fn load_masked(
    src: &[u8],
    bits: &[u64; MASK_WORDS],
    passthru: u8,
    n: usize,
    lanes: &mut [u8; MAX_LANES],
) {
    crate::port::vvec_load_masked(src, bits, passthru, n as u64, lanes);
}

#[cfg(feature = "vesper-port")]
pub(super) fn store_masked(
    dst: &mut [u8],
    bits: &[u64; MASK_WORDS],
    n: usize,
    lanes: &[u8; MAX_LANES],
) {
    crate::port::vvec_store_masked(dst, bits, n as u64, lanes);
}

#[cfg(feature = "vesper-port")]
pub(super) fn compare(
    op: CmpOp,
    n: usize,
    a: &[u8; MAX_LANES],
    b: &[u8; MAX_LANES],
    bits: &mut [u64; MASK_WORDS],
) {
    crate::port::vvec_compare(op as u64, n as u64, a, b, bits);
}

#[cfg(feature = "vesper-port")]
pub(super) fn binary(
    op: BinOp,
    n: usize,
    a: &[u8; MAX_LANES],
    b: &[u8; MAX_LANES],
    out: &mut [u8; MAX_LANES],
) {
    crate::port::vvec_binary(op as u64, n as u64, a, b, out);
}

#[cfg(feature = "vesper-port")]
pub(super) fn reduce_xor(n: usize, a: &[u8; MAX_LANES]) -> u8 {
    crate::port::vvec_reduce_xor(n as u64, a)
}

#[cfg(feature = "vesper-port")]
pub(super) fn scan(data: &[u8], delim: u8) -> (u64, u8) {
    let counts = crate::port::vvec_scan(data, delim);
    (counts.matches, counts.xor)
}

/// The Rust scalar arms: the default build's, and the port tests'
/// reference.
#[cfg_attr(all(feature = "vesper-port", not(test)), allow(dead_code))]
pub(super) mod reference {
    use super::super::{BinOp, CmpOp, MASK_WORDS, MAX_LANES, MaskOp};

    pub(in crate::vvec) fn prefix_bits(n: usize, bits: &mut [u64; MASK_WORDS]) {
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

    pub(in crate::vvec) fn mask_zip(
        op: MaskOp,
        lanes: usize,
        a: &mut [u64; MASK_WORDS],
        b: &[u64; MASK_WORDS],
    ) {
        let mut keep = [0u64; MASK_WORDS];
        prefix_bits(lanes, &mut keep);
        for ((x, y), k) in a.iter_mut().zip(b).zip(keep) {
            *x = op.eval(*x, *y) & k;
        }
    }

    pub(in crate::vvec) fn splat(value: u8, n: usize, lanes: &mut [u8; MAX_LANES]) {
        lanes[..n].fill(value);
    }

    fn test(bits: &[u64; MASK_WORDS], lane: usize) -> bool {
        bits[lane / 64] >> (lane % 64) & 1 != 0
    }

    pub(in crate::vvec) fn load_masked(
        src: &[u8],
        bits: &[u64; MASK_WORDS],
        passthru: u8,
        n: usize,
        lanes: &mut [u8; MAX_LANES],
    ) {
        for (i, lane) in lanes[..n].iter_mut().enumerate() {
            *lane = if test(bits, i) { src[i] } else { passthru };
        }
    }

    pub(in crate::vvec) fn store_masked(
        dst: &mut [u8],
        bits: &[u64; MASK_WORDS],
        n: usize,
        lanes: &[u8; MAX_LANES],
    ) {
        for (i, lane) in lanes[..n].iter().enumerate() {
            if test(bits, i) {
                dst[i] = *lane;
            }
        }
    }

    pub(in crate::vvec) fn compare(
        op: CmpOp,
        n: usize,
        a: &[u8; MAX_LANES],
        b: &[u8; MAX_LANES],
        bits: &mut [u64; MASK_WORDS],
    ) {
        *bits = [0; MASK_WORDS];
        for i in 0..n {
            if op.eval(a[i], b[i]) {
                bits[i / 64] |= 1 << (i % 64);
            }
        }
    }

    pub(in crate::vvec) fn binary(
        op: BinOp,
        n: usize,
        a: &[u8; MAX_LANES],
        b: &[u8; MAX_LANES],
        out: &mut [u8; MAX_LANES],
    ) {
        for i in 0..n {
            out[i] = op.eval(a[i], b[i]);
        }
    }

    pub(in crate::vvec) fn reduce_xor(n: usize, a: &[u8; MAX_LANES]) -> u8 {
        a[..n].iter().fold(0, |x, b| x ^ b)
    }

    pub(in crate::vvec) fn scan(data: &[u8], delim: u8) -> (u64, u8) {
        data.iter().fold((0, 0), |(count, xor), byte| {
            (count + u64::from(*byte == delim), xor ^ byte)
        })
    }
}

#[cfg(all(test, feature = "vesper-port"))]
mod tests {
    use super::super::{BinOp, CmpOp, MASK_WORDS, MAX_LANES, MaskOp};
    use super::reference as rust;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn bytes<const N: usize>(&mut self, alphabet: u8) -> [u8; N] {
            std::array::from_fn(|_| (self.next() % u64::from(alphabet)) as u8)
        }
        fn mask(&mut self, n: usize) -> [u64; MASK_WORDS] {
            let mut keep = [0; MASK_WORDS];
            rust::prefix_bits(n, &mut keep);
            let mut bits: [u64; MASK_WORDS] = std::array::from_fn(|_| self.next());
            // Some all-active and all-inactive words, for the word paths.
            match self.next() % 4 {
                0 => bits = [u64::MAX; MASK_WORDS],
                1 => bits[(self.next() % 4) as usize] = 0,
                _ => {}
            }
            for (b, k) in bits.iter_mut().zip(keep) {
                *b &= k;
            }
            bits
        }
    }

    #[test]
    fn vesper_port_vvec_scalar_arms_match_rust() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let widths = (1..=MAX_LANES)
            .step_by(7)
            .chain([16, 48, 64, 65, 128, 255, 256]);
        for n in widths {
            for _ in 0..8 {
                let (mut want, mut got) = ([0; MASK_WORDS], [0; MASK_WORDS]);
                rust::prefix_bits(n, &mut want);
                super::prefix_bits(n, &mut got);
                assert_eq!(got, want, "prefix {n}");

                let (a, b) = (rng.mask(MAX_LANES), rng.mask(MAX_LANES));
                for op in [MaskOp::And, MaskOp::Or, MaskOp::Xor, MaskOp::AndNot] {
                    let (mut want, mut got) = (a, a);
                    rust::mask_zip(op, n, &mut want, &b);
                    super::mask_zip(op, n, &mut got, &b);
                    assert_eq!(got, want, "{op:?} {n}");
                }

                let x: [u8; MAX_LANES] = rng.bytes(4);
                let y: [u8; MAX_LANES] = rng.bytes(4);
                for op in [CmpOp::Eq, CmpOp::LtU] {
                    let (mut want, mut got) = ([0; MASK_WORDS], [u64::MAX; MASK_WORDS]);
                    rust::compare(op, n, &x, &y, &mut want);
                    super::compare(op, n, &x, &y, &mut got);
                    assert_eq!(got, want, "{op:?} {n}");
                }
                for op in [BinOp::Xor, BinOp::And, BinOp::Or, BinOp::Add] {
                    let (mut want, mut got) = ([0; MAX_LANES], [0; MAX_LANES]);
                    rust::binary(op, n, &x, &y, &mut want);
                    super::binary(op, n, &x, &y, &mut got);
                    assert_eq!(got, want, "{op:?} {n}");
                }
                assert_eq!(super::reduce_xor(n, &x), rust::reduce_xor(n, &x), "{n}");
                let (mut want, mut got) = (x, x);
                rust::splat(0xa5, n, &mut want);
                super::splat(0xa5, n, &mut got);
                assert_eq!(got, want, "splat {n}");

                // The source is exactly as long as the last active lane
                // needs, so a read of an inactive lane past it would fail.
                let bits = rng.mask(n);
                let last = (0..n).rev().find(|i| bits[i / 64] >> (i % 64) & 1 != 0);
                let src = &x[..last.map_or(0, |l| l + 1)];
                let (mut want, mut got) = (y, y);
                rust::load_masked(src, &bits, 7, n, &mut want);
                super::load_masked(src, &bits, 7, n, &mut got);
                assert_eq!(got, want, "load {n}");
                let mut want = y[..src.len()].to_vec();
                let mut got = want.clone();
                rust::store_masked(&mut want, &bits, n, &x);
                super::store_masked(&mut got, &bits, n, &x);
                assert_eq!(got, want, "store {n}");
            }
        }
        for len in (0..300).chain([4096, 65_537]) {
            let data: Vec<u8> = (0..len).map(|_| (rng.next() % 5) as u8).collect();
            for start in 0..3.min(len + 1) {
                for delim in [0, 1, 4, 0xff] {
                    let d = &data[start..];
                    assert_eq!(super::scan(d, delim), rust::scan(d, delim), "{len} {start}");
                }
            }
        }
        assert_eq!(crate::port::live_allocations(), 0);
    }
}
