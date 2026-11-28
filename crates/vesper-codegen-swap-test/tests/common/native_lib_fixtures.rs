//! The libraries of plain functions that `@vesper/native-lib` builds
//! byte-identical objects for (tests/self_host_native_lib.rs), shared with
//! the library IR pipeline differential (tests/self_host_library_ir.rs).

/// Libraries of plain scalar functions: arithmetic, branches, loops,
/// floats, narrow integers, direct calls between exports and locals, and
/// `fail_only` functions with their failure records (#2425).
pub const LIBRARIES: &[(&str, &str)] = &[
    (
        "arith",
        "pub fn add3(a: i64, b: i64, c: i64) -> i64 { wrapping_add(wrapping_add(a, b), c) }
pub fn mix(a: u32, b: u32) -> u32 { (a ^ b) | (a & 7) }",
    ),
    (
        "branches",
        "pub fn sign(x: i16) -> i16 { if x < 0 { -1 } else if x == 0 { 0 } else { 1 } }
pub fn pick(c: bool, x: u8) -> u8 { let mut y: u8 = x; if c { y = x & 15; } y }",
    ),
    (
        "loops",
        "pub fn gcd(a: u64, b: u64) -> u64 {
    let mut x: u64 = a;
    let mut y: u64 = b;
    loop {
        if y == 0 { break; }
        let t: u64 = y;
        y = x % y;
        x = t;
    }
    x
}",
    ),
    (
        "floats",
        "pub fn lerp(a: f64, b: f64, t: f64) -> f64 { a + (b - a) * t }
pub fn half(x: f32) -> f32 { x * 0.5 }",
    ),
    (
        "calls",
        "fn square(x: i64) -> i64 { wrapping_mul(x, x) }
pub fn sum_squares(a: i64, b: i64) -> i64 { wrapping_add(square(a), square(b)) }
pub fn quad(x: i64) -> i64 { square(square(x)) }",
    ),
    (
        "fails",
        "fn step(a: u64, b: u64) -> u64 eff [Fail[ArithOverflow]] { a * b + 1 }
pub fn total(a: u64, b: u64) -> u64 eff [Fail[ArithOverflow], Fail[RuntimeError]] { step(a, b) / b }",
    ),
    // 128-bit parameters and results (#1277): each export symbol is a
    // `define_wide_entry` adapter around a local function.
    (
        "wide",
        "pub fn xor128(a: u128, b: u128) -> u128 { a ^ b }
pub fn negate(a: i128) -> i128 { wrapping_sub(0, a) }
pub fn is_zero(a: u128) -> bool { a == 0 }
pub fn mixed(x: u64, a: i128, y: f64) -> f64 { if a < 0 { y } else { y + 1.0 } }
pub fn scaled(a: u128, b: u64) -> u64 eff [Fail[ArithOverflow]] { b * 2 }",
    ),
    // (Checked 128-bit arithmetic has no body plan yet, so a failing
    // function with a wide result stays out.)
    // Cold paths the outliner moves into helpers: an asymmetric slow
    // tail and a loop's early exit.
    (
        "cold",
        "fn slow(x: u64, y: u64, z: u64) -> u64 {
    let mut a: u64 = x;
    let mut acc: u64 = 0;
    for i in 0..8 {
        acc = wrapping_add(acc, wrapping_add(a, y));
        a = wrapping_add(a, i);
        acc = wrapping_add(acc, z);
    }
    acc
}
pub fn dispatch(x: u64, y: u64, z: u64) -> u64 {
    if x < 100 {
        wrapping_add(wrapping_mul(x, 3), y)
    } else {
        let a = slow(x, y, z);
        let b = slow(y, z, x);
        wrapping_add(a, b)
    }
}
pub fn early(n: u64, x: u64, y: u64) -> u64 {
    let mut acc: u64 = 0;
    for i in 0..n {
        if wrapping_add(i, x) > 50 {
            let mut tmp: u64 = 0;
            for j in 0..4 {
                tmp = wrapping_add(tmp, slow(i, j, y));
            }
            return tmp;
        }
        acc = wrapping_add(acc, wrapping_add(i, y));
    }
    acc
}",
    ),
    // v2 libraries: the runtime entry points next to the functions, and
    // the drop glue of a handle type.
    (
        "v2",
        "// @export abi 3
pub fn add(a: u64, b: u64) -> u64 { wrapping_add(a, b) }",
    ),
    (
        "v2_wide",
        "// @export abi 3
pub fn twice(a: i128) -> i128 { wrapping_add(a, a) }
pub fn keep(x: u32) -> u32 { x }",
    ),
    (
        "v2_handle",
        "// @export abi 3
pub struct Buf { data: ChunkSeq[u8], n: u64 }
pub fn count(inout b: Buf) -> u64 { 7 }",
    ),
];
