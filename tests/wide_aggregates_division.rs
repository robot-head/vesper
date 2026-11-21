//! 128-bit division on native code (self-hosting wave SH4 lane 3, #1277).
//!
//! Native `u128`/`i128` `/` and `%` are the word algorithm on 64-bit
//! `udiv` (a one-word divisor, then a two-word divisor estimated from its
//! normalized high word). These tests compare the JIT, optimized and not,
//! and the evaluator against Rust's own division over dividends and
//! divisors of every bit length, the digit patterns that need both
//! quotient-digit corrections, and pseudo-random pairs.
use vesper::{check, native, parser, runtime::Runtime};

fn run_all(source: &str, expected: &str) {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}): {e}"))
            .to_string();
        assert_eq!(jit, expected, "JIT (optimize {optimize})");
    }
    let mut runtime = Runtime::new(&c.world);
    let value = runtime.run("main", vec![]).unwrap().to_string();
    assert_eq!(value, expected, "evaluator");
}

/// A splitmix64 stream, so the cases are the same on every run.
struct Mix(u64);
impl Mix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// A value of exactly `bits` significant bits (0 for 0).
    fn of_bits(&mut self, bits: u32) -> u128 {
        if bits == 0 {
            return 0;
        }
        let v = (u128::from(self.next()) << 64) | u128::from(self.next());
        (v >> (128 - bits)) | (1u128 << (bits - 1))
    }
}

/// Divisors whose 32-bit digits make the quotient-digit estimate too
/// large (all ones, a lone top bit, a top bit and a one), at every word
/// position, with dividends just below a multiple of them.
fn hard_pairs() -> Vec<(u128, u128)> {
    let digits: [u128; 6] = [
        0xFFFF_FFFF,
        0x8000_0000,
        0x8000_0001,
        0xFFFF_FFFE,
        0x7FFF_FFFF,
        1,
    ];
    let mut out = vec![];
    for top in digits {
        for low in digits {
            for shift in [0u32, 32, 64, 96] {
                let d = ((top << 32) | low) << shift;
                if d == 0 {
                    continue;
                }
                for k in [1u128, 2, 0xFFFF_FFFF, 0xFFFF_FFFF_FFFF_FFFF] {
                    let n = d.wrapping_mul(k).wrapping_sub(1);
                    out.push((n, d));
                    out.push((u128::MAX, d));
                }
            }
        }
    }
    out
}

fn pairs() -> Vec<(u128, u128)> {
    let mut mix = Mix(0x5EED);
    let mut out = hard_pairs();
    for nb in (0..=128).step_by(8) {
        for db in (1..=128).step_by(9) {
            out.push((mix.of_bits(nb), mix.of_bits(db)));
        }
    }
    for _ in 0..400 {
        let (nb, db) = ((mix.next() % 129) as u32, (mix.next() % 128) as u32 + 1);
        out.push((mix.of_bits(nb), mix.of_bits(db)));
    }
    out
}

const WORDS: &str = "fn w(hi: u64, lo: u64) -> u128 { (widen_u128(hi) << 64) | widen_u128(lo) }
";

fn word_call(v: u128) -> String {
    format!("w({}, {})", (v >> 64) as u64, v as u64)
}

/// Rust's quotient and remainder of a pair, as bit patterns.
type Reference = fn(u128, u128) -> (u128, u128);

/// One program per chunk of pairs: `main` returns the quotient and the
/// remainder of each pair, as `kind`.
fn check_chunks(kind: &str, cases: &[(u128, u128)], show: fn(u128) -> String) {
    let (convert, reference): (&str, Reference) = if kind == "u128" {
        ("", |n, d| (n / d, n % d))
    } else {
        ("wrapping_i128", |n, d| {
            let (n, d) = (n as i128, d as i128);
            ((n / d) as u128, (n % d) as u128)
        })
    };
    for chunk in cases.chunks(40) {
        let mut calls = vec![];
        let mut expected = vec![];
        for (n, d) in chunk {
            calls.push(format!(
                "q({}({}), {}({}))",
                convert,
                word_call(*n),
                convert,
                word_call(*d)
            ));
            let (q, r) = reference(*n, *d);
            expected.push(format!("({}, {})", show(q), show(r)));
        }
        let types = vec![format!("({kind}, {kind})"); chunk.len()].join(", ");
        let source = format!(
            "{WORDS}fn q(n: {kind}, d: {kind}) -> ({kind}, {kind}) {{
    handle Fail[ArithOverflow] in {{
        handle Fail[RuntimeError] in {{ (n / d, n % d) }} with {{ return v => v, raise(e, k) => (0, 1), }}
    }} with {{ return v => v, raise(e, k) => (0, 2), }}
}}
fn main() -> ({types}) {{ ({},) }}",
            calls.join(", ")
        );
        run_all(
            &source,
            &format!("({},)", expected.join(", "))
                .replace(",)", if chunk.len() == 1 { ",)" } else { ")" }),
        );
    }
}

#[test]
fn unsigned_division_agrees_with_rust() {
    check_chunks("u128", &pairs(), |v| v.to_string());
}

#[test]
fn signed_division_agrees_with_rust() {
    let cases: Vec<(u128, u128)> = pairs()
        .into_iter()
        .flat_map(|(n, d)| {
            [
                (n, d),
                (n.wrapping_neg(), d),
                (n, d.wrapping_neg()),
                (n.wrapping_neg(), d.wrapping_neg()),
            ]
        })
        .filter(|(n, d)| *d != 0 && !(*n as i128 == i128::MIN && *d as i128 == -1))
        .collect();
    check_chunks("i128", &cases, |v| (v as i128).to_string());
}
