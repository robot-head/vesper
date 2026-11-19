//! Bulk `ChunkSeq` builtins (P3b gaps lane 11, #1096): `resize(n, v)`,
//! `fill(lo, hi, v)` and `try_fill` on sequences and slices,
//! `extend_from_within(lo, hi)` and `try_extend_from_within`, and
//! `extend_reversed(in src)`. Every program runs in the evaluator, the JIT
//! with and without `optimize`, and an AOT executable, and is checked
//! against a Rust model over many lengths, element types and growth states
//! (the shared empty header, a full buffer, spare capacity).
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-seq-bulk-{}-{}",
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

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    let shown: Vec<_> = c
        .diagnostics
        .iter()
        .map(|d| {
            format!(
                "{}: {} at `{}`",
                d.code,
                d.message,
                &source[d.span.start..d.span.end]
            )
        })
        .collect();
    assert!(c.diagnostics.is_empty(), "{shown:#?}");
    c
}

fn rejected(source: &str, code: &str, needle: &str) {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == code && d.message.contains(needle)),
        "expected {code} ({needle}): {:#?}",
        c.diagnostics
    );
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn executable_status(name: &str, source: &str) -> Option<i32> {
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
    Command::new(&exe).output().unwrap().status.code()
}

/// Runs `main` in the evaluator, both JIT configurations and (where a C
/// driver links executables) an AOT executable, and checks every result
/// against `expected`.
fn differential(name: &str, source: &str, expected: u64) {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    assert_eq!(
        evaluated,
        expected.to_string(),
        "{name}: evaluator vs model"
    );
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(jit, evaluated, "{name}: JIT (optimize {optimize})");
    }
    if links_executables() {
        assert_eq!(
            executable_status(name, source),
            Some((expected & 0xff) as i32),
            "{name}: executable"
        );
    }
}

const PRELUDE: &str = "fn mix(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 1099511628211), v)
}
fn hash8(in s: Slice[u8]) -> u64 {
    let mut acc = s.len();
    for i in 0..s.len() {
        acc = match s.try_get(i) {
            Option::Some(b) => mix(acc, widen_u64(b)),
            Option::None => acc,
        };
    }
    acc
}
fn hash32(in s: Slice[u32]) -> u64 {
    let mut acc = s.len();
    for i in 0..s.len() {
        acc = match s.try_get(i) {
            Option::Some(b) => mix(acc, widen_u64(b)),
            Option::None => acc,
        };
    }
    acc
}
fn hash64(in s: Slice[u64]) -> u64 {
    let mut acc = s.len();
    for i in 0..s.len() {
        acc = match s.try_get(i) {
            Option::Some(b) => mix(acc, b),
            Option::None => acc,
        };
    }
    acc
}
fn bit(b: bool) -> u64 {
    let one: u64 = 1;
    let zero: u64 = 0;
    if b { one } else { zero }
}
";

fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1099511628211).wrapping_add(v)
}

fn hash<T: Copy + Into<u64>>(s: &[T]) -> u64 {
    s.iter().fold(s.len() as u64, |acc, &x| mix(acc, x.into()))
}

fn main_of(run: &str) -> String {
    format!(
        "{PRELUDE}{run}\nfn main() -> u64 {{\n    handle Fail[RuntimeError] in {{\n        run()\n    }} with {{\n        return x => x,\n        raise(e, k) => 7777,\n    }}\n}}\n"
    )
}

/// `resize` grows from the shared empty header, from a full buffer and
/// within spare capacity, shrinks, and keeps its length; for bytes (the
/// `memset` path), `u32`, `u64` and a copy struct.
#[test]
fn resize_grows_and_shrinks() {
    let source = main_of(
        "copy struct P { a: u64, b: u32 }
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut acc: u64 = 1;
    let steps: u64 = 40;
    for n in 0..steps {
        let mut b: ChunkSeq[u8] = ChunkSeq::new[u8]();
        b.resize(n, 7);
        b.resize(wrapping_mul(n, 3), wrapping_u8(n));
        acc = mix(acc, hash8(in b.as_slice()));
        b.resize(wrapping_add(n, 1), 9);
        acc = mix(acc, hash8(in b.as_slice()));
        let mut w: ChunkSeq[u32] = ChunkSeq::with_capacity[u32](8);
        w.resize(n, wrapping_u32(wrapping_add(n, 100)));
        w.resize(n >> 1, 0);
        w.resize(wrapping_add(n, 5), 3);
        acc = mix(acc, hash32(in w.as_slice()));
        let mut q: ChunkSeq[u64] = ChunkSeq::new[u64]();
        q.push(n);
        q.resize(n, 11);
        acc = mix(acc, hash64(in q.as_slice()));
        let mut p: ChunkSeq[P] = ChunkSeq::new[P]();
        p.resize(n, P { a: n, b: 5 });
        p.resize(wrapping_mul(n, 2), P { a: 1, b: wrapping_u32(n) });
        let mut ps: u64 = p.len();
        for i in 0..p.len() {
            ps = match p.try_get(i) {
                Option::Some(x) => mix(mix(ps, x.a), widen_u64(x.b)),
                Option::None => ps,
            };
        }
        acc = mix(acc, ps);
    }
    acc
}",
    );
    let mut acc = 1u64;
    for n in 0..40u64 {
        let mut b: Vec<u8> = Vec::new();
        b.resize(n as usize, 7);
        b.resize(n as usize * 3, n as u8);
        acc = mix(acc, hash(&b));
        b.resize(n as usize + 1, 9);
        acc = mix(acc, hash(&b));
        let mut w: Vec<u32> = Vec::new();
        w.resize(n as usize, n as u32 + 100);
        w.resize(n as usize >> 1, 0);
        w.resize(n as usize + 5, 3);
        acc = mix(acc, hash(&w));
        let mut q = vec![n];
        q.resize(n as usize, 11);
        acc = mix(acc, hash(&q));
        let mut p: Vec<(u64, u32)> = Vec::new();
        p.resize(n as usize, (n, 5));
        p.resize(n as usize * 2, (1, n as u32));
        let ps = p
            .iter()
            .fold(p.len() as u64, |s, x| mix(mix(s, x.0), u64::from(x.1)));
        acc = mix(acc, ps);
    }
    differential("resize", &source, acc);
}

/// `fill(lo, hi, v)` and `try_fill` on a sequence and on a slice argument,
/// at every interval of a short buffer, including empty and failing ones.
#[test]
fn fill_intervals() {
    let source = main_of(
        "fn fill_in(inout s: Slice[u64], lo: u64, hi: u64, v: u64) -> u64 {
    bit(s.try_fill(lo, hi, v))
}
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut acc: u64 = 1;
    let n: u64 = 19;
    let top: u64 = 22;
    for lo in 0..top {
        for hi in 0..top {
            let mut b: ChunkSeq[u8] = ChunkSeq::new[u8]();
            b.resize(n, 1);
            acc = mix(acc, bit(b.try_fill(lo, hi, wrapping_u8(lo))));
            acc = mix(acc, hash8(in b.as_slice()));
            let mut w: ChunkSeq[u64] = ChunkSeq::new[u64]();
            w.resize(n, 2);
            acc = mix(acc, fill_in(inout w[1..n], lo, hi, hi));
            acc = mix(acc, hash64(in w.as_slice()));
            if hi <= n && lo <= hi {
                w.fill(lo, hi, 5);
                acc = mix(acc, hash64(in w.as_slice()));
            }
        }
    }
    let mut b: ChunkSeq[u8] = ChunkSeq::new[u8]();
    b.resize(4, 0);
    b.fill(2, 5, 1);
    acc
}",
    );
    let n = 19usize;
    let mut acc = 1u64;
    for lo in 0..22usize {
        for hi in 0..22usize {
            let mut b = vec![1u8; n];
            let ok = lo <= hi && hi <= n;
            if ok {
                b[lo..hi].fill(lo as u8);
            }
            acc = mix(acc, u64::from(ok));
            acc = mix(acc, hash(&b));
            let mut w = vec![2u64; n];
            let ok = lo <= hi && hi < n;
            if ok {
                w[1 + lo..1 + hi].fill(hi as u64);
            }
            acc = mix(acc, u64::from(ok));
            acc = mix(acc, hash(&w));
            if hi <= n && lo <= hi {
                w[lo..hi].fill(5);
                acc = mix(acc, hash(&w));
            }
        }
    }
    // The last `fill` is out of bounds and raises.
    let _ = acc;
    differential("fill", &source, 7777);
    // Without the failing call the program returns the model's digest.
    let ok = source.replace("    b.fill(2, 5, 1);\n", "");
    differential("fill_ok", &ok, acc);
}

/// `extend_from_within` copies an interval of the sequence to its end,
/// through a growth that moves the buffer; the total form refuses bad
/// intervals and appends nothing.
#[test]
fn extend_from_within_reads_through_growth() {
    let source = main_of(
        "copy struct P { a: u64, b: u8 }
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut acc: u64 = 1;
    let top: u64 = 12;
    for lo in 0..top {
        for hi in 0..top {
            let mut b: ChunkSeq[u8] = ChunkSeq::new[u8]();
            for i in 0..wrapping_add(lo, 7) {
                b.push(wrapping_u8(wrapping_mul(i, 37)));
            }
            acc = mix(acc, bit(b.try_extend_from_within(lo, hi)));
            acc = mix(acc, bit(b.try_extend_from_within(0, b.len())));
            acc = mix(acc, hash8(in b.as_slice()));
            let mut w: ChunkSeq[u32] = ChunkSeq::with_capacity[u32](64);
            for i in 0..hi {
                w.push(wrapping_u32(wrapping_add(i, 1000)));
            }
            acc = mix(acc, bit(w.try_extend_from_within(lo, hi)));
            acc = mix(acc, hash32(in w.as_slice()));
        }
    }
    let mut e: ChunkSeq[u64] = ChunkSeq::new[u64]();
    e.extend_from_within(0, 0);
    acc = mix(acc, e.len());
    let mut p: ChunkSeq[P] = ChunkSeq::new[P]();
    let five: u64 = 5;
    for i in 0..five {
        p.push(P { a: i, b: wrapping_u8(i) });
    }
    p.extend_from_within(1, 4);
    p.extend_from_within(0, p.len());
    for i in 0..p.len() {
        acc = match p.try_get(i) {
            Option::Some(x) => mix(mix(acc, x.a), widen_u64(x.b)),
            Option::None => acc,
        };
    }
    e.extend_from_within(0, 1);
    acc
}",
    );
    let mut acc = 1u64;
    for lo in 0..12usize {
        for hi in 0..12usize {
            let mut b: Vec<u8> = (0..lo + 7).map(|i| (i * 37) as u8).collect();
            let ok = lo <= hi && hi <= b.len();
            if ok {
                b.extend_from_within(lo..hi);
            }
            acc = mix(acc, u64::from(ok));
            b.extend_from_within(..);
            acc = mix(acc, 1);
            acc = mix(acc, hash(&b));
            let mut w: Vec<u32> = (0..hi).map(|i| i as u32 + 1000).collect();
            let ok = lo <= hi;
            if ok {
                w.extend_from_within(lo..hi);
            }
            acc = mix(acc, u64::from(ok));
            acc = mix(acc, hash(&w));
        }
    }
    acc = mix(acc, 0);
    let mut p: Vec<(u64, u8)> = (0..5).map(|i| (i, i as u8)).collect();
    p.extend_from_within(1..4);
    p.extend_from_within(..);
    for x in &p {
        acc = mix(mix(acc, x.0), u64::from(x.1));
    }
    differential("extend_from_within", &source, 7777);
    let ok = source.replace("    e.extend_from_within(0, 1);\n", "");
    differential("extend_from_within_ok", &ok, acc);
}

/// `extend_reversed` appends a slice last first: bytes (word steps and a
/// tail) at every length and start up to 40, `u32`, `u64` and a struct.
#[test]
fn extend_reversed_appends_last_first() {
    let source = main_of(
        "copy struct P { a: u64, b: u8 }
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut acc: u64 = 1;
    let mut src: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let m: u64 = 60;
    for i in 0..m {
        src.push(wrapping_u8(wrapping_add(wrapping_mul(i, 29), 3)));
    }
    let offs: u64 = 9;
    let lens: u64 = 41;
    for off in 0..offs {
        for n in 0..lens {
            let mut d: ChunkSeq[u8] = ChunkSeq::new[u8]();
            if n > 20 {
                d.push(1);
            }
            d.extend_reversed(in src[off..wrapping_add(off, n)]);
            acc = mix(acc, hash8(in d.as_slice()));
        }
    }
    let mut w: ChunkSeq[u32] = ChunkSeq::new[u32]();
    let mut q: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let mut p: ChunkSeq[P] = ChunkSeq::new[P]();
    let ten: u64 = 10;
    for i in 0..ten {
        w.push(wrapping_u32(i));
        q.push(wrapping_mul(i, i));
        p.push(P { a: i, b: wrapping_u8(wrapping_add(i, 1)) });
    }
    let mut w2: ChunkSeq[u32] = ChunkSeq::new[u32]();
    w2.extend_reversed(in w[2..9]);
    acc = mix(acc, hash32(in w2.as_slice()));
    let mut q2: ChunkSeq[u64] = ChunkSeq::with_capacity[u64](3);
    q2.extend_reversed(in q.as_slice());
    q2.extend_reversed(in q[0..0]);
    acc = mix(acc, hash64(in q2.as_slice()));
    let mut p2: ChunkSeq[P] = ChunkSeq::new[P]();
    p2.extend_reversed(in p[1..10]);
    for i in 0..p2.len() {
        acc = match p2.try_get(i) {
            Option::Some(x) => mix(mix(acc, x.a), widen_u64(x.b)),
            Option::None => acc,
        };
    }
    acc
}",
    );
    let src: Vec<u8> = (0..60u64).map(|i| (i * 29 + 3) as u8).collect();
    let mut acc = 1u64;
    for off in 0..9 {
        for n in 0..41 {
            let mut d: Vec<u8> = if n > 20 { vec![1] } else { vec![] };
            d.extend(src[off..off + n].iter().rev());
            acc = mix(acc, hash(&d));
        }
    }
    let w: Vec<u32> = (0..10).collect();
    let q: Vec<u64> = (0..10).map(|i| i * i).collect();
    let p: Vec<(u64, u8)> = (0..10).map(|i| (i, i as u8 + 1)).collect();
    let w2: Vec<u32> = w[2..9].iter().rev().copied().collect();
    acc = mix(acc, hash(&w2));
    let q2: Vec<u64> = q.iter().rev().copied().collect();
    acc = mix(acc, hash(&q2));
    for x in p[1..10].iter().rev() {
        acc = mix(mix(acc, x.0), u64::from(x.1));
    }
    differential("extend_reversed", &source, acc);
}

/// Element modes, operand types and self-reads are checked.
#[test]
fn bulk_builtins_are_typed() {
    rejected(
        "struct B { x: u64 }
fn main() -> u64 {
    let mut s: ChunkSeq[B] = ChunkSeq::new[B]();
    s.resize(3, B { x: 1 });
    s.len()
}
",
        "E1522",
        "resize copies its elements",
    );
    rejected(
        "fn main() -> u64 {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    s.push(1);
    s.extend_reversed(in s.as_slice());
    s.len()
}
",
        "E0301",
        "cannot read from the sequence it writes",
    );
    rejected(
        "fn main() -> u64 {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    s.fill(0, 1);
    s.len()
}
",
        "E0207",
        "'fill' expects 1 arguments, found 2",
    );
    rejected(
        "fn main() -> bool {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    s.try_extend_from_within(0, true)
}
",
        "E0200",
        "expected u64",
    );
    rejected(
        "fn f(inout s: Slice[u8]) {
    s.resize(1, 0);
}
fn main() -> u64 { 0 }
",
        "E1525",
        "unknown method 'resize' on Slice",
    );
    // The checked forms raise, so they need the Fail effect.
    rejected(
        "fn main() -> u64 {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    s.extend_from_within(0, 0);
    s.len()
}
",
        "E0401",
        "unhandled effect 'Fail'",
    );
}
