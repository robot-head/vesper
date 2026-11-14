//! Eisel–Lemire decoding: the power-of-five table is recomputed with an
//! independent big integer, and every conversion path is differential-tested
//! for bit equality against `str::parse::<f64>` (correctly rounded).
mod common;

use common::Rng;
use std::collections::HashMap;
use vesper_serde::number::{DecodePath, decode_f64};
use vesper_serde::{FloatLimits, IndexedJson, Limits};

/// Little-endian base-2^32 natural numbers, just enough for the table.
#[derive(Clone, Debug, PartialEq)]
struct Big(Vec<u32>);
impl Big {
    fn one() -> Self {
        Big(vec![1])
    }
    fn mul_small(&mut self, m: u32) {
        let mut carry = 0u64;
        for limb in &mut self.0 {
            let v = u64::from(*limb) * u64::from(m) + carry;
            *limb = v as u32;
            carry = v >> 32;
        }
        if carry > 0 {
            self.0.push(carry as u32);
        }
    }
    fn bits(&self) -> usize {
        let top = *self.0.last().unwrap();
        (self.0.len() - 1) * 32 + (32 - top.leading_zeros() as usize)
    }
    fn bit(&self, i: usize) -> bool {
        self.0.get(i / 32).is_some_and(|l| (l >> (i % 32)) & 1 == 1)
    }
    /// The `n` bits starting at bit `lo`, as a u128 (n <= 128).
    fn window(&self, lo: usize, n: usize) -> u128 {
        (0..n).fold(0u128, |acc, k| acc | (u128::from(self.bit(lo + k)) << k))
    }
    fn ge(&self, other: &Big) -> bool {
        let n = self.0.len().max(other.0.len());
        for i in (0..n).rev() {
            let (a, b) = (
                self.0.get(i).copied().unwrap_or(0),
                other.0.get(i).copied().unwrap_or(0),
            );
            if a != b {
                return a > b;
            }
        }
        true
    }
    fn sub(&mut self, other: &Big) {
        let mut borrow = 0i64;
        for i in 0..self.0.len() {
            let v = i64::from(self.0[i]) - i64::from(other.0.get(i).copied().unwrap_or(0)) - borrow;
            self.0[i] = v.rem_euclid(1 << 32) as u32;
            borrow = i64::from(v < 0);
        }
    }
    fn shl1_or(&mut self, bit: bool) {
        let mut carry = u32::from(bit);
        for limb in &mut self.0 {
            let next = *limb >> 31;
            *limb = (*limb << 1) | carry;
            carry = next;
        }
        if carry > 0 {
            self.0.push(carry);
        }
    }
}

/// The top 128 bits of floor(2^b / d) by bit-serial long division.
fn top128_of_quotient(b: usize, d: &Big) -> (u128, usize) {
    let mut rem = Big(vec![0]);
    let mut q_bits: Vec<bool> = Vec::with_capacity(b + 1);
    for i in (0..=b).rev() {
        rem.shl1_or(i == b);
        if rem.ge(d) {
            rem.sub(d);
            q_bits.push(true);
        } else {
            q_bits.push(false);
        }
    }
    let first = q_bits.iter().position(|x| *x).unwrap();
    let q = &q_bits[first..];
    (
        q.iter()
            .take(128)
            .fold(0u128, |a, x| (a << 1) | u128::from(*x)),
        q.len(),
    )
}

fn expected_entry(q: i64) -> u128 {
    if q >= 0 {
        let mut p = Big::one();
        for _ in 0..q {
            p.mul_small(5);
        }
        let n = p.bits();
        if n >= 128 {
            p.window(n - 128, 128)
        } else {
            p.window(0, n) << (128 - n)
        }
    } else {
        let mut p = Big::one();
        for _ in 0..-q {
            p.mul_small(5);
        }
        let z = p.bits();
        let b = if q >= -27 { z + 127 } else { 2 * z + 128 };
        // floor(2^b / p) + 1, truncated to its top 128 bits. Adding one can
        // only carry into the top 128 bits when the quotient has exactly
        // 128 bits (the q >= -27 case), where the window is the whole value.
        let (top, len) = top128_of_quotient(b, &p);
        if len <= 128 { top + 1 } else { top }
    }
}

#[test]
fn power_of_five_table_matches_an_independent_big_integer() {
    // Anchors from the published fast_float / core::num::dec2flt tables,
    // then every entry of the generated source file.
    for (q, want) in [
        (-342i64, 0xeef4_53d6_923b_d65a_113f_aa29_06a1_3b3fu128),
        (-1, 0xcccc_cccc_cccc_cccc_cccc_cccc_cccc_cccd),
        (0, 0x8000_0000_0000_0000_0000_0000_0000_0000),
        (1, 0xa000_0000_0000_0000_0000_0000_0000_0000),
        (308, 0x8e67_9c2f_5e44_ff8f_570f_09ea_a7ea_7648),
    ] {
        assert_eq!(expected_entry(q), want, "q = {q}");
    }
    let table = include_str!("../src/pow5.rs");
    let entries: Vec<u128> = table
        .lines()
        .filter_map(|l| l.trim().strip_prefix("(0x"))
        .map(|l| {
            let (hi, lo) = l.trim_end_matches("),").split_once(", 0x").unwrap();
            (u128::from_str_radix(hi, 16).unwrap() << 64) | u128::from_str_radix(lo, 16).unwrap()
        })
        .collect();
    assert_eq!(entries.len(), 651);
    for (i, &got) in entries.iter().enumerate() {
        let q = i as i64 - 342;
        assert_eq!(got, expected_entry(q), "entry for 5^{q}");
    }
}

fn check(text: &str, paths: &mut HashMap<DecodePath, usize>) {
    let want: f64 = text.parse().unwrap();
    let (got, path) = decode_f64(text).unwrap_or_else(|| panic!("{text} rejected"));
    assert_eq!(got.to_bits(), want.to_bits(), "{text} via {path:?}");
    *paths.entry(path).or_default() += 1;
}

fn digits(rng: &mut Rng, n: usize) -> String {
    (0..n)
        .map(|_| char::from(b'0' + rng.below(10) as u8))
        .collect()
}

#[test]
fn every_path_is_bit_identical_to_std_on_a_differential_corpus() {
    let mut paths = HashMap::new();
    let mut rng = Rng(0x001e_15e1);
    // Round trips of random doubles in shortest and long forms.
    for _ in 0..60_000 {
        let bits = rng.next() ^ (rng.next() << 32);
        let x = f64::from_bits(bits);
        if !x.is_finite() {
            continue;
        }
        check(&format!("{x:?}").replace("inf", "1e999"), &mut paths);
        check(&format!("{x:e}"), &mut paths);
        check(&format!("{:.25e}", x), &mut paths);
    }
    // Random decimals: 1-40 digits, exponents across the whole range.
    for _ in 0..60_000 {
        let n = 1 + rng.below(20);
        let int = digits(&mut rng, n).trim_start_matches('0').to_owned();
        let int = if int.is_empty() { "0".to_owned() } else { int };
        let n = rng.below(22);
        let frac = digits(&mut rng, n);
        let exp = rng.below(700) as i64 - 360;
        let sign = if rng.below(2) == 0 { "" } else { "-" };
        let text = if frac.is_empty() {
            format!("{sign}{int}e{exp}")
        } else {
            format!("{sign}{int}.{frac}e{exp}")
        };
        if text.parse::<f64>().unwrap().is_finite() {
            check(&text, &mut paths);
        }
    }
    // Halfway cases: the exact midpoint between adjacent doubles, which
    // needs every digit, and its neighbours one unit in the last digit.
    for _ in 0..4_000 {
        let x = f64::from_bits(rng.next() ^ (rng.next() << 32)).abs();
        if !x.is_normal() || x > 1e300 {
            continue;
        }
        check(&exact_midpoint(x), &mut paths);
    }
    for text in [
        "9007199254740993",
        "9007199254740992.5",
        "2.2250738585072011e-308",
        "2.2250738585072012e-308",
        "4.9406564584124654e-324",
        "2.4703282292062327e-324",
        "2.4703282292062328e-324",
        "1.7976931348623157e308",
        "1.7976931348623158e308",
        "7.2057594037927933e16",
        "0.1",
        "123456789012345678901234567890",
        "1.00000000000000011102230246251565404236316680908203125",
        "1.00000000000000011102230246251565404236316680908203124",
        "1.00000000000000011102230246251565404236316680908203126",
        "0.000000000000000000000000000000000000000000001e300",
        "1e-400",
        "1e23",
        "8.988465674311580536566680e307",
    ] {
        check(text, &mut paths);
    }
    eprintln!("decode paths: {paths:?}");
    for path in [
        DecodePath::Clinger,
        DecodePath::EiselLemire,
        DecodePath::Fallback,
    ] {
        assert!(
            paths.get(&path).copied().unwrap_or(0) > 0,
            "{path:?} never taken"
        );
    }
}

/// The exact decimal expansion of the midpoint between `x` and its successor
/// (x > 0 normal): (2m + 1) * 2^(e-1) written out in full.
fn exact_midpoint(x: f64) -> String {
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64 - 1075;
    let m = (bits & ((1 << 52) - 1)) | (1 << 52);
    let mut num = Big(vec![(2 * m + 1) as u32, ((2 * m + 1) >> 32) as u32]);
    let e2 = exp - 1;
    if e2 >= 0 {
        for _ in 0..e2 {
            num.mul_small(2);
        }
        return to_decimal(&num);
    }
    // n / 2^k = n * 5^k / 10^k.
    let k = (-e2) as usize;
    for _ in 0..k {
        num.mul_small(5);
    }
    format!("{}e-{k}", to_decimal(&num))
}

fn to_decimal(n: &Big) -> String {
    let mut limbs = n.0.clone();
    let mut out = Vec::new();
    while limbs.iter().any(|l| *l != 0) {
        let mut rem = 0u64;
        for limb in limbs.iter_mut().rev() {
            let cur = (rem << 32) | u64::from(*limb);
            *limb = (cur / 10) as u32;
            rem = cur % 10;
        }
        out.push(b'0' + rem as u8);
    }
    if out.is_empty() {
        out.push(b'0');
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

#[test]
fn non_numbers_and_infinities() {
    for text in [
        "", "-", "01", "1.", ".5", "1e", "1e+", "+1", "1x", "--1", "0x10",
    ] {
        assert!(decode_f64(text).is_none(), "{text:?}");
    }
    assert!(decode_f64("1e309").unwrap().0.is_infinite());
    // A long significand must not cancel a saturated exponent.
    let long = format!("0.{}1e10000000", "0".repeat(999_999));
    assert!(decode_f64(&long).unwrap().0.is_infinite(), "10^9000000");
    let long = format!("{}e-10000000", "1".repeat(1_000_001));
    assert_eq!(decode_f64(&long).unwrap().0, 0.0, "10^-9000000");
    let huge = format!("1e{}", "9".repeat(40));
    assert!(decode_f64(&huge).unwrap().0.is_infinite());
    let tiny = format!("-1e-{}", "9".repeat(40));
    assert_eq!(decode_f64(&tiny).unwrap().0.to_bits(), (-0.0f64).to_bits());
    assert_eq!(decode_f64("-0").unwrap().0.to_bits(), (-0.0f64).to_bits());
    assert_eq!(
        decode_f64("-1e-999999999999").unwrap().0.to_bits(),
        (-0.0f64).to_bits()
    );
    // `Node::as_f64` uses this decoder and still rejects overflow.
    let doc = IndexedJson::parse(b"[1e309, 0.1]", Limits::default()).unwrap();
    let mut items = doc.root().elements().unwrap();
    assert!(items.read_f64(FloatLimits::default()).is_err());
}
