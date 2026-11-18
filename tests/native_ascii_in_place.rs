//! In-place ASCII lowercase (port speed wave, lane 6, #1026):
//! `s.make_ascii_lowercase(lo, hi)` and its total form
//! `try_make_ascii_lowercase` on `ChunkSeq[u8]` and `Slice[u8]`. Optimized
//! native code lowercases 16 bytes per vector step with an overlapping last
//! step, and runs of 4 to 15 bytes as two overlapping words; unoptimized
//! code runs the byte loop. Every program runs in the evaluator, the JIT with
//! and without `optimize`, and an AOT executable, against a Rust model over
//! every interval of a buffer up to 40 bytes long at 20 starts.
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
            "vesper-native-ascii-in-place-{}-{}",
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

const PRELUDE: &str = "fn noise(n: u64, seed: u64) -> ChunkSeq[u8] {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let mut x = seed;
    for i in 0..n {
        x = wrapping_add(wrapping_mul(x, 6364136223846793005), 1442695040888963407);
        s.push(wrapping_u8(x >> 56));
    }
    s
}
fn mix(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 1099511628211), v)
}
/// Mismatch position `r` (0, 1 or 2) of the interval `off..off + n`:
/// its first byte, one at seven eighths, and its last byte.
fn pos(off: u64, n: u64, r: u64) -> u64 {
    if r == 0 {
        off
    } else if r == 1 {
        wrapping_add(off, wrapping_mul(n, 7) >> 3)
    } else {
        wrapping_sub(wrapping_add(off, n), 1)
    }
}
fn bit(b: bool) -> u64 {
    let one: u64 = 1;
    let zero: u64 = 0;
    if b { one } else { zero }
}
fn main() -> u64 {
    handle Fail[RuntimeError] in {
        run()
    } with {
        return x => x,
        raise(e, k) => 7777,
    }
}
";

fn model_noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 56) as u8
        })
        .collect()
}

fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1099511628211).wrapping_add(v)
}

const OFFSETS: usize = 20;
const MAX_LEN: usize = 40;
const BUF: usize = OFFSETS + MAX_LEN + 3;

fn hash(s: &[u8]) -> u64 {
    s.iter()
        .fold(s.len() as u64, |acc, &b| mix(acc, u64::from(b)))
}

const HASH: &str = "fn hash(in s: Slice[u8]) -> u64 {
    let mut acc: u64 = s.len();
    for i in 0..s.len() {
        let b: u64 = match s.try_get(i) {
            Option::Some(b) => widen_u64(b),
            Option::None => 999,
        };
        acc = mix(acc, b);
    }
    acc
}
";

/// Every interval `off..off + n` of a fresh copy of a noise buffer (with
/// every letter and the bytes next to `A`-`Z` well represented) is
/// lowercased in place, by the checked form on the sequence, by the total
/// form, and through a `Slice` of it that starts at byte 1; the whole buffer is hashed after each,
/// so a byte written outside the interval changes the result.
#[test]
fn make_ascii_lowercase_every_interval() {
    let source = format!(
        "{PRELUDE}{HASH}fn fresh(in raw: Slice[u8]) -> ChunkSeq[u8] {{
    let mut w: ChunkSeq[u8] = ChunkSeq::new[u8]();
    w.extend(in raw);
    w
}}
fn through_slice(inout v: Slice[u8], lo: u64, hi: u64) eff [Fail[RuntimeError]] {{
    v.make_ascii_lowercase(lo, hi);
}}
fn run() -> u64 eff [Fail[RuntimeError]] {{
    let offsets: u64 = {OFFSETS};
    let max_len: u64 = {MAX_LEN};
    let all: u64 = {BUF};
    let noisy = noise({BUF}, 53);
    let mut raw: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for i in 0..all {{
        let x = noisy[i];
        if x < 128 {{
            raw.push(wrapping_add(56, x & 63));
        }} else {{
            raw.push(x);
        }}
    }}
    let mut acc: u64 = 1;
    for off in 0..offsets {{
        for n in 0..=max_len {{
            let hi = wrapping_add(off, n);
            let mut w = fresh(in raw.as_slice());
            w.make_ascii_lowercase(off, hi);
            acc = mix(acc, hash(in w.as_slice()));
            let mut u = fresh(in raw.as_slice());
            acc = mix(acc, bit(u.try_make_ascii_lowercase(off, hi)));
            acc = mix(acc, hash(in u.as_slice()));
            let mut v = fresh(in raw.as_slice());
            through_slice(inout v[1..all], off, hi);
            acc = mix(acc, hash(in v.as_slice()));
        }}
    }}
    let mut every: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let bytes: u64 = 256;
    for x in 0..bytes {{
        every.push(wrapping_u8(x));
    }}
    every.make_ascii_lowercase(0, bytes);
    acc = mix(acc, hash(in every.as_slice()));
    for x in 0..bytes {{
        every.set(x, wrapping_u8(x));
    }}
    for x in 0..bytes {{
        every.make_ascii_lowercase(x, wrapping_add(x, 1));
    }}
    mix(acc, hash(in every.as_slice()))
}}
"
    );
    let noisy = model_noise(BUF, 53);
    let raw: Vec<u8> = noisy
        .iter()
        .map(|&x| if x < 128 { 56 + (x & 63) } else { x })
        .collect();
    let mut acc = 1u64;
    for off in 0..OFFSETS {
        for n in 0..=MAX_LEN {
            let mut w = raw.clone();
            w[off..off + n].make_ascii_lowercase();
            acc = mix(acc, hash(&w));
            acc = mix(acc, 1);
            acc = mix(acc, hash(&w));
            let mut v = raw.clone();
            v[1 + off..1 + off + n].make_ascii_lowercase();
            acc = mix(acc, hash(&v));
        }
    }
    let every: Vec<u8> = (0..=255u8).map(|x| x.to_ascii_lowercase()).collect();
    acc = mix(acc, hash(&every));
    acc = mix(acc, hash(&every));
    differential("make_ascii_lowercase", &source, acc);
}

/// Out-of-range intervals: the total form yields `false` and writes
/// nothing; the checked form raises `IndexOutOfBounds` before it writes.
#[test]
fn make_ascii_lowercase_bounds() {
    let total = format!(
        "{PRELUDE}{HASH}fn run() -> u64 eff [Fail[RuntimeError]] {{
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let n: u64 = 20;
    for i in 0..n {{
        s.push(wrapping_add(65, wrapping_u8(i)));
    }}
    let mut acc: u64 = 1;
    acc = mix(acc, bit(s.try_make_ascii_lowercase(0, 21)));
    acc = mix(acc, bit(s.try_make_ascii_lowercase(5, 4)));
    acc = mix(acc, bit(s.try_make_ascii_lowercase(21, 21)));
    acc = mix(acc, hash(in s.as_slice()));
    acc = mix(acc, bit(s.try_make_ascii_lowercase(20, 20)));
    acc = mix(acc, bit(s.try_make_ascii_lowercase(3, 20)));
    mix(acc, hash(in s.as_slice()))
}}
"
    );
    let mut s: Vec<u8> = (0..20u8).map(|i| 65 + i).collect();
    let mut acc = 1u64;
    for _ in 0..3 {
        acc = mix(acc, 0);
    }
    acc = mix(acc, hash(&s));
    acc = mix(acc, 1);
    s[3..].make_ascii_lowercase();
    acc = mix(acc, 1);
    differential("try_make_ascii_lowercase", &total, mix(acc, hash(&s)));

    // `main` maps the raise to 7777; nothing is written before it.
    let checked = format!(
        "{PRELUDE}fn run() -> u64 eff [Fail[RuntimeError]] {{
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let n: u64 = 20;
    for i in 0..n {{
        s.push(65);
    }}
    s.make_ascii_lowercase(2, 21);
    widen_u64(s[2])
}}
"
    );
    differential("make_ascii_lowercase_raises", &checked, 7777);
}

#[test]
fn make_ascii_lowercase_needs_bytes_and_indices() {
    rejected(
        "fn main() -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    s.push(65);
    s.make_ascii_lowercase(0, 1);
    s.len()
}
",
        "E1525",
        "'make_ascii_lowercase' reads and writes bytes",
    );
    rejected(
        "fn main() -> u64 {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    s.push(65);
    s.make_ascii_lowercase(0);
    s.len()
}
",
        "E0207",
        "'make_ascii_lowercase' expects 2 arguments",
    );
    rejected(
        "fn main() -> bool {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    s.push(65);
    s.try_make_ascii_lowercase(0, true)
}
",
        "E0200",
        "expected u64, found bool",
    );
}
