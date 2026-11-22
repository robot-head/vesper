//! 128-bit values in native aggregates (self-hosting wave SH4 lane 3,
//! #1277; fixes #1270).
//!
//! The checker types an integer literal from the binding it flows into, but
//! a tuple or array literal keeps its literal type, which defaults to
//! one-word `i64` elements. Native code laid such a literal out by that
//! type, so `let t: [i128; 3] = [1, 2, 5]` stored three words that the
//! binding's two-word layout then read back wrong. Every program here runs
//! on the evaluator and on the JIT, optimized and not, and must agree; every
//! program also compiles to x86-64 and AArch64 objects.
use vesper::{check, native, parser, runtime::Runtime};

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{source}\n{:?}", c.diagnostics);
    c
}

/// The evaluator and the JIT, optimized and not, agree on `main` and equal
/// `expected`; both object targets compile.
fn agree(source: &str, expected: &str) {
    let c = accepted(source);
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = optimize;
        let value = runtime.run("main", vec![]).unwrap().to_string();
        assert_eq!(value, expected, "evaluator (optimize {optimize}): {source}");
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}): {e}\n{source}"))
            .to_string();
        assert_eq!(jit, expected, "JIT (optimize {optimize}): {source}");
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        native::object_for_target(&c.world, "main", Some(target))
            .unwrap_or_else(|e| panic!("{target}: {e}\n{source}"));
    }
}

/// The repro of #1270.
#[test]
fn array_literal_elements_keep_both_words() {
    agree(
        "fn main() -> u64 {
    let t: [i128; 3] = [1, 2, 5];
    let mut s: u64 = 0;
    if t[0] == 1 { s = wrapping_add(s, 1); }
    if t[1] == 2 { s = wrapping_add(s, 2); }
    if t[2] == 5 { s = wrapping_add(s, 4); }
    s
}",
        "7",
    );
}

#[test]
fn dynamic_index_reads_and_writes() {
    agree(
        "fn get(t: [u128; 4], i: u64) -> u128 eff [Fail[RuntimeError]] { t[i] }
fn body() -> (u128, u128, u128, i128) eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut t: [u128; 4] = [1, 18446744073709551615, 3, 4];
    t[2] = 7;
    t[3] += 18446744073709551615;
    let i: u64 = 1;
    let mut n: [i128; 2] = [-1, -2];
    n[i] -= 5;
    (get(t, 1), t[2], t[3], n[1])
}
fn main() -> (u128, u128, u128, i128) {
    handle Fail[RuntimeError] in {
        handle Fail[ArithOverflow] in { body() } with { return v => v, raise(e, k) => (0, 0, 0, 1), }
    } with { return v => v, raise(e, k) => (0, 0, 0, 2), }
}",
        "(18446744073709551615, 7, 18446744073709551619, -7)",
    );
}

#[test]
fn mixed_tuples_and_nesting() {
    agree(
        "fn main() -> (u8, i128, u16, i128, u128) {
    let t: (u8, i128, u16) = (3, -4, 5);
    let (a, b, c) = t;
    let n: [(i128, u128); 2] = [(-1, 2), (-3, 4)];
    let (x, y) = n[1];
    (a, b, c, x, wrapping_add(y, 18446744073709551615))
}",
        "(3, -4, 5, -3, 18446744073709551619)",
    );
}

#[test]
fn tuples_through_joins_calls_and_loops() {
    agree(
        "fn pick(c: bool) -> (i128, u64) { if c { (-9, 1) } else { (9, 2) } }
fn sum(t: [i128; 3]) -> i128 {
    let mut s: i128 = 0;
    for v in t { s = wrapping_add(s, v); }
    s
}
fn main() -> (i128, u64, i128, i128) {
    let (a, b) = pick(true);
    let t: (i128, u64) = if b == 1 { (-1, 7) } else { (1, 8) };
    let (c, d) = t;
    (a, d, c, sum([-5, 6, -7]))
}",
        "(-9, 7, -1, -6)",
    );
}

#[test]
fn aggregates_hold_wide_elements() {
    agree(
        "struct Row { tag: u8, cells: [i128; 2] }
enum Cell { Wide((i128, u128)), Narrow(u8) }
fn width(c: Cell) -> i128 {
    match c { Cell::Wide(p) => { let (a, b) = p; wrapping_add(a, wrapping_i128(b)) }, Cell::Narrow(n) => 0 }
}
fn main() -> (u8, i128, i128) {
    let r = Row { tag: 2, cells: [-3, 4] };
    (r.tag, r.cells[0], width(Cell::Wide((-10, 3))))
}",
        "(2, -3, -7)",
    );
}
