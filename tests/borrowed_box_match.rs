//! Borrowed matching of `Box` trees (self-hosting SH1 lane 6, #1149,
//! manuscript §4.3): `match e { .. }` on a place of an `in` or `inout`
//! binding whose type is not copy borrows the place, and its pattern binds
//! `in` borrows of the payloads, `Box` contents included. A tree passed
//! `in` can therefore be walked any number of times without consuming it.
//!
//! Before this lane, `fn eval(in e: Expr) -> u64 { match e { .. } }` was
//! `E0305` (cannot move `e` through a borrowed parameter), so the SH0 std
//! data lane (#1127, gap 3) could only consume `Box` trees (`sink e`).
//!
//! Every program runs in the evaluator, the Cranelift JIT (optimizer on and
//! off) and, where a C compiler links, an executable. In the JIT every box
//! allocated is freed exactly once (a borrowed match frees nothing), and
//! the executable exits with the evaluator's value.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, fip, native, parser, runtime::Runtime, seq::native_runtime as seq_runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-borrowed-box-match-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn links() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

fn codes(source: &str) -> Vec<String> {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    check::check(&p.module)
        .diagnostics
        .iter()
        .map(|d| d.code.to_string())
        .collect()
}

/// Runs `main` in the JIT (both optimizer settings) and, where it links, as
/// an executable; checks each against `expected` and the JIT's box
/// accounting. Runs the evaluator too when `evaluate` is set.
fn native_runs(name: &str, source: &str, expected: u64, evaluate: bool) {
    let c = accepted(source);
    if evaluate {
        let mut runtime = Runtime::new(&c.world);
        runtime.fuel = 4_000_000_000;
        let got = runtime
            .run("main", vec![])
            .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
            .to_string();
        assert_eq!(got, expected.to_string(), "{name}: evaluator");
    }
    for optimize in [true, false] {
        fip::native_runtime::reset_counts();
        let stray = seq_runtime::stray_frees();
        let got = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            got,
            expected.to_string(),
            "{name}: JIT (optimize {optimize})"
        );
        assert!(
            fip::native_runtime::alloc_count() > 0,
            "{name}: the program allocates boxes"
        );
        assert_eq!(
            fip::native_runtime::free_count(),
            fip::native_runtime::alloc_count(),
            "{name}: JIT (optimize {optimize}) must free every box exactly once"
        );
        assert_eq!(
            seq_runtime::stray_frees(),
            stray,
            "{name}: JIT (optimize {optimize}) freed a string or sequence that was not live"
        );
    }
    if links() {
        let temp = Temp::new();
        let file = temp.0.join(format!("{name}.ves"));
        fs::write(&file, source).unwrap();
        let exe = temp.0.join(name);
        let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .arg("build")
            .arg(&file)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{name}: vesper build failed\n{}{}",
            String::from_utf8_lossy(&built.stdout),
            String::from_utf8_lossy(&built.stderr)
        );
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(
            run.status.code(),
            Some((expected & 0xff) as i32),
            "{name}: executable stderr={}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// The SH0 std data tree (tests/std_arena.rs), walked through borrows.

fn lcg(x: u64) -> u64 {
    x.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

/// The value and the node count of the tree of `size` nodes grown from
/// `seed`, as `grow_box` in the driver builds it.
fn model(size: u64, seed: u64) -> (u64, u64) {
    if size <= 1 {
        return (seed >> 40, 1);
    }
    let x = lcg(seed);
    let rest = size - 1;
    if seed % 5 == 1 {
        let (v, n) = model(rest, x);
        return (v.wrapping_neg(), n + 1);
    }
    if rest < 2 {
        return model(rest, x);
    }
    let left = 1 + (x >> 33) % (rest - 1);
    let (a, na) = model(left, lcg(x));
    let (b, nb) = model(rest - left, lcg(lcg(x)));
    let v = if seed % 5 == 2 {
        a.wrapping_mul(b)
    } else {
        a.wrapping_add(b)
    };
    (v, na + nb + 1)
}

const TREE: &str = "enum Expr {
    Lit(u64),
    Neg(Box[Expr]),
    Add(Box[Expr], Box[Expr]),
    Mul(Box[Expr], Box[Expr])
}

fn lcg(x: u64) -> u64 {
    wrapping_add(wrapping_mul(x, 6364136223846793005), 1442695040888963407)
}

fn rem(x: u64, n: u64) -> u64 {
    if n == 0 { 0 } else { x % n }
}

fn split(rest: u64, x: u64) -> u64 {
    wrapping_add(1, rem(x >> 33, wrapping_sub(rest, 1)))
}

fn grow_box(size: u64, seed: u64) -> Expr {
    if size <= 1 {
        Expr::Lit(seed >> 40)
    } else {
        let x = lcg(seed);
        let rest = wrapping_sub(size, 1);
        let k = rem(seed, 5);
        if k == 1 {
            Expr::Neg(Box::new(grow_box(rest, x)))
        } else if rest < 2 {
            grow_box(rest, x)
        } else {
            let left = split(rest, x);
            let a = grow_box(left, lcg(x));
            let b = grow_box(wrapping_sub(rest, left), lcg(lcg(x)));
            if k == 2 {
                Expr::Mul(Box::new(a), Box::new(b))
            } else {
                Expr::Add(Box::new(a), Box::new(b))
            }
        }
    }
}

// Walks the tree through borrows: nothing is moved or freed.
fn eval(in e: Expr) -> u64 {
    match e {
        Expr::Lit(v) => v,
        Expr::Neg(x) => match x {
            Box(a) => wrapping_sub(0, eval(in a)),
        },
        Expr::Add(l, r) => match l {
            Box(a) => match r {
                Box(b) => wrapping_add(eval(in a), eval(in b)),
            },
        },
        Expr::Mul(l, r) => match l {
            Box(a) => match r {
                Box(b) => wrapping_mul(eval(in a), eval(in b)),
            },
        },
    }
}

fn count(in e: Expr) -> u64 {
    match e {
        Expr::Lit(_) => 1,
        Expr::Neg(x) => match x { Box(a) => wrapping_add(1, count(in a)) },
        Expr::Add(l, r) => match l { Box(a) => match r { Box(b) => wrapping_add(1, wrapping_add(count(in a), count(in b))) } },
        Expr::Mul(l, r) => match l { Box(a) => match r { Box(b) => wrapping_add(1, wrapping_add(count(in a), count(in b))) } },
    }
}

// The consuming walk of tests/std_arena.rs, for comparison.
fn eval_box(sink e: Expr) -> u64 {
    match e {
        Expr::Lit(v) => v,
        Expr::Neg(x) => match x { Box(a) => wrapping_sub(0, eval_box(a)) },
        Expr::Add(l, r) => match l { Box(a) => match r { Box(b) => wrapping_add(eval_box(a), eval_box(b)) } },
        Expr::Mul(l, r) => match l { Box(a) => match r { Box(b) => wrapping_mul(eval_box(a), eval_box(b)) } },
    }
}

fn main() -> u64 {
    let t = grow_box(SIZE, 12345);
    let first = eval(in t);
    let second = eval(in t);
    let n = count(in t);
    let last = eval_box(t);
    if first == second && second == last { wrapping_add(first, n) } else { 0 }
}
";

fn tree_expected(size: u64) -> u64 {
    let (v, n) = model(size, 12345);
    v.wrapping_add(n)
}

#[test]
fn borrowed_tree_walks_match_rust_natively() {
    let size = 100_000;
    native_runs(
        "tree",
        &TREE.replace("SIZE", &size.to_string()),
        tree_expected(size),
        false,
    );
}

#[test]
fn borrowed_tree_walks_match_rust_in_the_evaluator() {
    let size = 300;
    native_runs(
        "tree_small",
        &TREE.replace("SIZE", &size.to_string()),
        tree_expected(size),
        true,
    );
}

// ---------------------------------------------------------------------------
// Payload shapes: strings, `Option[Box[_]]` struct fields, nested `Box`
// patterns, wildcards, an `inout` scrutinee, and a helper the inliner takes.

const SHAPES: &str = "enum Expr {
    Lit(u64),
    Neg(Box[Expr]),
    Add(Box[Expr], Box[Expr]),
    Name(String)
}

struct Cell {
    v: u64,
    next: Option[Box[Cell]]
}

fn eval(in e: Expr) -> u64 {
    match e {
        Expr::Lit(v) => v,
        Expr::Neg(Box(Expr::Lit(v))) => wrapping_sub(0, v),
        Expr::Neg(x) => match x {
            Box(a) => wrapping_sub(0, eval(in a)),
        },
        Expr::Add(_, r) => match r {
            Box(b) => wrapping_add(left(in e), eval(in b)),
        },
        Expr::Name(s) => s.len(),
    }
}

fn left(in e: Expr) -> u64 {
    match e {
        Expr::Add(l, _) => match l { Box(a) => eval(in a) },
        _ => 0,
    }
}

fn head(in e: Expr) -> u64 {
    match e {
        Expr::Neg(x) => match x { Box(a) => match a { Expr::Lit(v) => v, _ => 1 } },
        Expr::Add(_, b) => match b { Box(c) => match c { Expr::Lit(v) => v, _ => 2 } },
        _ => 0,
    }
}

fn walk(in c: Cell) -> u64 {
    match c.next {
        Option::Some(n) => match n { Box(d) => wrapping_add(c.v, walk(in d)) },
        Option::None => c.v,
    }
}

fn twice(inout e: Expr) -> u64 {
    let a = eval(in e);
    let b = match e {
        Expr::Add(l, _) => match l { Box(x) => eval(in x) },
        _ => 7,
    };
    e = Expr::Lit(5);
    wrapping_add(wrapping_add(a, b), eval(in e))
}

fn main() -> u64 {
    let mut t = Expr::Add(Box::new(Expr::Name(\"abc\")), Box::new(Expr::Neg(Box::new(Expr::Add(Box::new(Expr::Lit(9)), Box::new(Expr::Neg(Box::new(Expr::Lit(4)))))))));
    let a = eval(in t);
    let b = eval(in t);
    let l = Cell { v: 1, next: Option::Some(Box::new(Cell { v: 2, next: Option::Some(Box::new(Cell { v: 3, next: Option::None })) })) };
    let w = walk(in l);
    let x = walk(in l);
    let n = Expr::Neg(Box::new(Expr::Lit(4)));
    let p = Expr::Add(Box::new(Expr::Lit(1)), Box::new(Expr::Lit(6)));
    let h = wrapping_add(wrapping_add(head(in n), head(in n)), wrapping_add(head(in p), head(in p)));
    let y = twice(inout t);
    wrapping_add(wrapping_add(wrapping_add(a, b), wrapping_add(w, x)), wrapping_add(y, h))
}
";

#[test]
fn borrowed_payload_shapes_agree_everywhere() {
    // eval(t) = 3 + -(9 + -4) = -2 (twice), walk = 6 (twice),
    // head: 4 + 4 + 6 + 6 = 20, twice(t) = -2 + 3 + 5 = 6.
    native_runs("shapes", SHAPES, 34, true);
}

// ---------------------------------------------------------------------------
// What a borrowed match still refuses.

#[test]
fn a_borrowed_binding_cannot_be_moved() {
    let src = "enum Expr { Lit(u64), Neg(Box[Expr]) }
fn take(in e: Expr) -> Box[Expr] {
    match e { Expr::Neg(x) => x, Expr::Lit(v) => Box::new(Expr::Lit(v)) }
}
fn main() -> u64 { 0 }
";
    assert_eq!(codes(src), ["E0305"]);
}

#[test]
fn the_matched_place_is_frozen_inside_the_arms() {
    let src = "enum Expr { Lit(u64), Neg(Box[Expr]) }
fn f(inout e: Expr) -> u64 {
    match e { Expr::Neg(x) => { e = Expr::Lit(1); match x { Box(_a) => 2 } }, Expr::Lit(v) => v }
}
fn main() -> u64 { 0 }
";
    assert_eq!(codes(src), ["E0301"]);
}

#[test]
fn bindings_of_an_inout_scrutinee_are_shared() {
    let src = "enum Expr { Lit(u64), Neg(Box[Expr]) }
fn g(inout x: Box[Expr]) -> u64 { 0 }
fn f(inout e: Expr) -> u64 {
    match e { Expr::Neg(x) => g(inout x), Expr::Lit(v) => v }
}
fn main() -> u64 { 0 }
";
    assert_eq!(codes(src), ["E0307"]);
}

#[test]
fn matching_an_owned_tree_still_moves_it() {
    let src = "enum Expr { Lit(u64), Neg(Box[Expr]) }
fn f(sink e: Expr) -> u64 {
    let n = match e { Expr::Neg(_x) => 1, Expr::Lit(v) => v };
    match e { Expr::Neg(_x) => 1, Expr::Lit(v) => v }
}
fn main() -> u64 { 0 }
";
    assert_eq!(codes(src), ["E0303"]);
}

// ---------------------------------------------------------------------------
// Strings bound by a borrowed match belong to the matched value: an unused
// binding frees nothing (the executable runs under the allocator's checks,
// and the string is read again after each call).

const STRINGS: &str = "enum Term {
    Nat(u64),
    Lam {
        name: String,
        body: Box[Term]
    }
}

fn lam_only(in t: Term) -> u64 {
    match t {
        Term::Lam { name, body } => 1,
        _ => 0,
    }
}

fn name_len(in t: Term) -> u64 {
    match t {
        Term::Lam { name, body } => name.len(),
        _ => 0,
    }
}

fn main() -> u64 {
    let mut s = String::new();
    s.push_str(\"a string long enough to own a buffer\");
    let t = Term::Lam { name: s, body: Box::new(Term::Nat(1)) };
    let a = lam_only(in t);
    let b = name_len(in t);
    let c = lam_only(in t);
    let d = name_len(in t);
    wrapping_add(wrapping_add(a, b), wrapping_add(c, d))
}
";

#[test]
fn borrowed_string_bindings_are_not_freed() {
    native_runs("strings", STRINGS, 2 + 2 * 36, true);
}
