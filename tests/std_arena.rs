//! `@vesper-std/arena` (self-hosting SH0 lane 5, #1127): typed index arenas,
//! and the same expression tree as a `Box` tree and as an index graph.
//!
//! A driver builds a pseudo-random expression tree of `Lit`, `Add`, `Mul`
//! and `Neg` nodes twice: as a recursive `enum Expr` of `Box`es, and as
//! `Node` values in an `Arena[Node]` whose children are `Idx[Node]`. Both
//! evaluate (wrapping arithmetic) to the Rust model's value; the arena is
//! also evaluated in one forward pass, because children are allocated
//! before their parents. A second entry exercises checked access: `get`
//! of an index the arena never handed out, `set` past the end, `idx_none`,
//! `truncate` and the `IdxMap` side table. The driver runs in the JIT with
//! and without `optimize`, as an executable and (on a small tree) in the
//! evaluator. `tree_box` and `tree_arena` are the two shapes the report
//! measures on a 100,000-node tree.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, packages, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-std-arena-{}-{}",
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

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn driver(source: &str) -> Temp {
    let temp = Temp::new();
    let std = fs::canonicalize("packages/vesper-std/arena").unwrap();
    let manifest = format!(
        "[package]\nname = \"@local/arena-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Fail\", \"Alloc\"]\n\n[dependencies]\n\"@vesper-std/arena\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
        std.display().to_string().replace('\\', "/")
    );
    fs::write(temp.0.join("Loom.toml"), manifest).unwrap();
    fs::create_dir_all(temp.0.join("src")).unwrap();
    fs::write(temp.0.join("src/main.ves"), source).unwrap();
    temp
}

fn load(dir: &Path) -> check::Checked {
    let assembled = packages::load(dir).unwrap();
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    let checked = check::check(&assembled.module);
    let shown: Vec<_> = checked
        .diagnostics
        .iter()
        .map(|d| assembled.sources.localize(d))
        .collect();
    assert!(checked.diagnostics.is_empty(), "{shown:?}");
    checked
}

fn executable_exit(dir: &Path) -> Option<i32> {
    let exe = dir.join("driver");
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(dir)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "vesper build failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    Command::new(&exe).output().unwrap().status.code()
}

// ---------------------------------------------------------------------------
// The Rust model: the same tree, evaluated recursively.

fn lcg(x: u64) -> u64 {
    x.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

/// The value of the tree of `size` nodes grown from `seed` (the driver's
/// `grow`): one node is a literal; otherwise `seed % 5` picks `Neg` (1),
/// `Mul` (2) or `Add`, and the remaining nodes split by the seed.
fn model_eval(size: u64, seed: u64) -> u64 {
    if size <= 1 {
        return seed >> 40;
    }
    let x = lcg(seed);
    let rest = size - 1;
    if seed % 5 == 1 {
        return model_eval(rest, x).wrapping_neg();
    }
    if rest < 2 {
        return model_eval(rest, x);
    }
    let left = 1 + (x >> 33) % (rest - 1);
    let a = model_eval(left, lcg(x));
    let b = model_eval(rest - left, lcg(lcg(x)));
    if seed % 5 == 2 {
        a.wrapping_mul(b)
    } else {
        a.wrapping_add(b)
    }
}

fn model_checked() -> u64 {
    // get(out of range) = None -> 1, set(past end) = false -> 1, idx_none
    // -> is_none 1 and get None 1, truncate(2) leaves len 2 and drops idx 2
    // (1), IdxMap: default 9 for unset, 77 at 5, len 6.
    let mut acc = 0u64;
    for v in [1, 1, 1, 1, 2, 1, 9, 77, 6, 3] {
        acc = acc.wrapping_mul(31).wrapping_add(v);
    }
    acc
}

// ---------------------------------------------------------------------------
// The Vesper driver.

const DRIVER: &str = "use @vesper-std/arena::{Arena, Idx, arena_new, arena_with_capacity, idx_none, idx_from_raw, idx_map_new};

enum Expr {
    Lit(u64),
    Neg(Box[Expr]),
    Add(Box[Expr], Box[Expr]),
    Mul(Box[Expr], Box[Expr])
}

copy struct Node {
    op: u32,
    a: Idx[Node],
    b: Idx[Node],
    v: u64
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

fn eval_box(sink e: Expr) -> u64 {
    match e {
        Expr::Lit(v) => v,
        Expr::Neg(x) => match x {
            Box(a) => wrapping_sub(0, eval_box(a)),
        },
        Expr::Add(l, r) => match l {
            Box(a) => match r {
                Box(b) => wrapping_add(eval_box(a), eval_box(b)),
            },
        },
        Expr::Mul(l, r) => match l {
            Box(a) => match r {
                Box(b) => wrapping_mul(eval_box(a), eval_box(b)),
            },
        },
    }
}

fn grow_arena(inout ar: Arena[Node], size: u64, seed: u64) -> Idx[Node] {
    let z = idx_none[Node]();
    if size <= 1 {
        ar.alloc(Node {
            op: 0,
            a: z,
            b: z,
            v: seed >> 40
        })
    } else {
        let x = lcg(seed);
        let rest = wrapping_sub(size, 1);
        let k = rem(seed, 5);
        if k == 1 {
            let c = grow_arena(inout ar, rest, x);
            ar.alloc(Node {
                op: 1,
                a: c,
                b: z,
                v: 0
            })
        } else if rest < 2 {
            grow_arena(inout ar, rest, x)
        } else {
            let left = split(rest, x);
            let a = grow_arena(inout ar, left, lcg(x));
            let b = grow_arena(inout ar, wrapping_sub(rest, left), lcg(lcg(x)));
            let op: u32 = if k == 2 { 3 } else { 2 };
            ar.alloc(Node {
                op: op,
                a: a,
                b: b,
                v: 0
            })
        }
    }
}

fn eval_arena(in ar: Arena[Node], i: Idx[Node]) -> u64 {
    match ar.get(i) {
        Option::Some(n) => if n.op == 0 {
            n.v
        } else if n.op == 1 {
            wrapping_sub(0, eval_arena(in ar, n.a))
        } else if n.op == 2 {
            wrapping_add(eval_arena(in ar, n.a), eval_arena(in ar, n.b))
        } else {
            wrapping_mul(eval_arena(in ar, n.a), eval_arena(in ar, n.b))
        },
        Option::None => 0,
    }
}

fn val(in vals: Slice[u64], i: Idx[Node]) -> u64 {
    match vals.try_get(i.index()) {
        Option::Some(v) => v,
        Option::None => 0,
    }
}

// One forward pass: children precede their parents.
fn eval_forward(in ar: Arena[Node]) -> u64 {
    let n = ar.len();
    let mut vals: ChunkSeq[u64] = ChunkSeq::with_capacity[u64](n);
    let mut k: u64 = 0;
    loop {
        if k >= n {
            break;
        }
        let v = match ar.at(k) {
            Option::Some(node) => if node.op == 0 {
                node.v
            } else if node.op == 1 {
                wrapping_sub(0, val(in vals.as_slice(), node.a))
            } else if node.op == 2 {
                wrapping_add(val(in vals.as_slice(), node.a), val(in vals.as_slice(), node.b))
            } else {
                wrapping_mul(val(in vals.as_slice(), node.a), val(in vals.as_slice(), node.b))
            },
            Option::None => 0,
        };
        vals.push(v);
        k = wrapping_add(k, 1);
    }
    match vals.pop() {
        Option::Some(v) => v,
        Option::None => 0,
    }
}

pub fn tree_box() -> u64 {
    let e = grow_box(SIZE, 12345);
    eval_box(e)
}

pub fn tree_arena() -> u64 {
    let mut ar: Arena[Node] = arena_with_capacity[Node](SIZE);
    let root = grow_arena(inout ar, SIZE, 12345);
    eval_arena(in ar, root)
}

pub fn tree_forward() -> u64 {
    let mut ar: Arena[Node] = arena_new[Node]();
    let root = grow_arena(inout ar, SIZE, 12345);
    let v = eval_forward(in ar);
    if root.index() == wrapping_sub(ar.len(), 1) { v } else { 0 }
}

fn bit(b: bool) -> u64 {
    if b { 1 } else { 0 }
}

fn put(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 31), v)
}

pub fn checked() -> u64 {
    let mut ar: Arena[u64] = arena_new[u64]();
    let a = ar.alloc(10);
    let b = ar.alloc(20);
    let c = ar.alloc(30);
    let mut acc: u64 = 0;
    let far = idx_from_raw[u64](7);
    acc = put(acc, match ar.get(far) { Option::Some(_v) => 0, Option::None => 1 });
    acc = put(acc, bit(!ar.set(far, 5)));
    let z = idx_none[u64]();
    acc = put(acc, bit(z.is_none()));
    acc = put(acc, match ar.get(z) { Option::Some(_v) => 0, Option::None => 1 });
    ar.truncate(2);
    acc = put(acc, ar.len());
    acc = put(acc, bit(!ar.contains(c) && ar.contains(b) && ar.get_or(a, 0) == 10));
    let mut m = idx_map_new[u64, u64](9);
    let five = idx_from_raw[u64](5);
    m.set(five, 77);
    acc = put(acc, m.get(b));
    acc = put(acc, m.get(five));
    acc = put(acc, m.len());
    acc = put(acc, wrapping_add(bit(!m.set(z, 1)), 2));
    acc
}

fn main() -> u64 {
    tree_box() ^ tree_arena() ^ tree_forward() ^ checked()
}
";

fn source(size: u64) -> String {
    DRIVER.replace("SIZE", &size.to_string())
}

#[test]
fn both_tree_shapes_match_rust_natively() {
    let size = 100_000;
    let dir = driver(&source(size));
    let c = load(&dir.0);
    let value = model_eval(size, 12345);
    let want = [
        ("tree_box", value),
        ("tree_arena", value),
        ("tree_forward", value),
        ("checked", model_checked()),
    ];
    for optimize in [true, false] {
        for (entry, v) in want {
            let got = native::jit_with_options(&c.world, entry, optimize)
                .unwrap_or_else(|e| panic!("JIT {entry} (optimize {optimize}) failed: {e}"))
                .to_string();
            assert_eq!(got, v.to_string(), "JIT {entry} (optimize {optimize})");
        }
    }
    if links_executables() {
        let all = value ^ value ^ value ^ model_checked();
        assert_eq!(
            executable_exit(&dir.0),
            Some((all & 0xff) as i32),
            "executable"
        );
    }
}

#[test]
fn both_tree_shapes_match_rust_in_the_evaluator() {
    let size = 300;
    let dir = driver(&source(size));
    let c = load(&dir.0);
    let value = model_eval(size, 12345);
    for (entry, v) in [
        ("tree_box", value),
        ("tree_arena", value),
        ("tree_forward", value),
        ("checked", model_checked()),
    ] {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = false;
        runtime.fuel = 4_000_000_000;
        let got = runtime.run(entry, vec![]).unwrap().to_string();
        assert_eq!(got, v.to_string(), "evaluator {entry}");
    }
}

#[test]
fn arena_package_checks_on_its_own() {
    load(Path::new("packages/vesper-std/arena"));
}
