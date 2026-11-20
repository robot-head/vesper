//! The total checked builtins `checked_add_k`, `checked_sub_k` and
//! `checked_mul_k` (self-hosting SH1 lane 5, #1148): one per integer kind,
//! returning `Option[k]`, `None` exactly where `+`, `-` or `*` raises
//! `Fail[ArithOverflow]`. A function without `Fail` in its row may call them.
mod ergonomics_support;
use ergonomics_support::{agree, rejects};

const FNV: u64 = 0x100000001b3;

/// (name, bits, signed)
const KINDS: [(&str, u32, bool); 10] = [
    ("u8", 8, false),
    ("u16", 16, false),
    ("u32", 32, false),
    ("u64", 64, false),
    ("usize", 64, false),
    ("i8", 8, true),
    ("i16", 16, true),
    ("i32", 32, true),
    ("i64", 64, true),
    ("isize", 64, true),
];

fn range(bits: u32, signed: bool) -> (i128, i128) {
    if signed {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    }
}

/// Values around every boundary of the kind: the ends, zero and one, and
/// the square-root magnitudes where products start to overflow.
fn edges(bits: u32, signed: bool) -> Vec<i128> {
    let (min, max) = range(bits, signed);
    let half = 1i128 << (bits / 2);
    let mut v = vec![
        min,
        min + 1,
        0,
        1,
        2,
        3,
        half - 1,
        half,
        half + 1,
        max / 2,
        max / 2 + 1,
        max - 1,
        max,
    ];
    if signed {
        v.extend([-1, -2, -half, -half - 1, -half + 1, min / 2, min / 2 - 1]);
    }
    v.retain(|x| (min..=max).contains(x));
    v.sort();
    v.dedup();
    v
}

fn mix(h: u64, x: u64) -> u64 {
    (h ^ x).wrapping_mul(FNV)
}

#[test]
fn every_kind_matches_exact_arithmetic_on_every_backend() {
    for (k, bits, signed) in KINDS {
        let (min, max) = range(bits, signed);
        let values = edges(bits, signed);
        let mut expected: u64 = 14695981039346656037;
        for op in ["add", "sub", "mul"] {
            for &a in &values {
                for &b in &values {
                    let exact = match op {
                        "add" => a.checked_add(b),
                        "sub" => a.checked_sub(b),
                        _ => a.checked_mul(b),
                    };
                    expected = match exact.filter(|e| (min..=max).contains(e)) {
                        Some(e) => mix(mix(expected, 1), e as i64 as u64),
                        None => mix(expected, 2),
                    };
                }
            }
        }
        let n = values.len();
        let list = values
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let loops = ["add", "sub", "mul"]
            .iter()
            .map(|op| {
                format!(
                    "    for i in 0..{n} {{ for j in 0..{n} {{ h = enc(h, checked_{op}_{k}(xs[i], xs[j])); }} }}\n"
                )
            })
            .collect::<String>();
        let source = format!(
            "fn mix(h: u64, x: u64) -> u64 {{ wrapping_mul(h ^ x, {FNV}) }}\n\
             fn enc(h: u64, o: Option[{k}]) -> u64 {{\n\
             \x20   match o {{ Option::Some(v) => mix(mix(h, 1), wrapping_u64(v)), Option::None => mix(h, 2) }}\n\
             }}\n\
             fn main() -> u64 {{\n    let xs: [{k}; {n}] = [{list}];\n    let mut h: u64 = 14695981039346656037;\n{loops}    h\n}}\n"
        );
        assert_eq!(agree(&format!("checked_{k}"), &source), expected, "{k}");
    }
}

#[test]
fn run_time_operands_take_the_same_paths() {
    // Operands from a loop, so neither the optimizer nor the inliner can
    // fold the checks away; the counts are Rust's.
    let source = "
fn main() -> u64 {
    let mut ok: u64 = 0;
    let mut a: u8 = 0;
    for i in 0..256 {
        a = wrapping_u8(i);
        match checked_add_u8(a, 200) { Option::Some(_) => { ok = wrapping_add(ok, 1); }, Option::None => {} }
        match checked_mul_u8(a, a) { Option::Some(_) => { ok = wrapping_add(ok, 1000); }, Option::None => {} }
        match checked_sub_i8(wrapping_i8(i), 100) { Option::Some(_) => { ok = wrapping_add(ok, 1000000); }, Option::None => {} }
    }
    ok
}";
    let add = (0..=255u8).filter(|a| a.checked_add(200).is_some()).count() as u64;
    let mul = (0..=255u8).filter(|a| a.checked_mul(*a).is_some()).count() as u64;
    let sub = (0..=255u8)
        .filter(|a| (*a as i8).checked_sub(100).is_some())
        .count() as u64;
    assert_eq!(
        agree("checked_loop", source),
        add + mul * 1000 + sub * 1_000_000
    );
}

#[test]
fn the_names_are_reserved() {
    rejects(
        "fn checked_add_u8(a: u8, b: u8) -> Option[u8] { Option::None } fn main() -> u64 { 0 }",
        "E0100",
    );
    // Operands take the kind of the name.
    rejects(
        "fn main() -> u64 { let a: u64 = 1; match checked_add_u8(a, 1) { Option::Some(_) => 1, Option::None => 0 } }",
        "E0200",
    );
}
