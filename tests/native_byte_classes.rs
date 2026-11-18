//! Byte-class and ASCII case builtins (port gaps wave, lane 4, #987):
//! `find_not_in`, `find_either`, `eq_ignore_ascii_case` and
//! `extend_ascii_lowercase` on `ChunkSeq[u8]` and `Slice[u8]`. Optimized
//! native code runs them on 128-bit vectors with a scalar tail; unoptimized
//! code runs the scalar loop. Every program here runs in the evaluator, the
//! JIT with and without `optimize`, and an AOT executable, and is checked
//! against a Rust model over every length 0..=256 at every start of a
//! 16-byte step, with random bytes and every byte value at the class and
//! case boundaries.
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
            "vesper-native-byte-classes-{}-{}",
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

/// `noise(n, seed)`: `n` pseudo-random bytes from a 64-bit LCG, the same
/// sequence as [`model_noise`].
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

/// Starts `0..OFFSETS` cover every alignment of a 16-byte vector step.
const OFFSETS: usize = 17;
const MAX_LEN: usize = 256;
const BUF: usize = OFFSETS + MAX_LEN;

/// The model of `find_not_in`'s class.
fn member(lo: u64, hi: u64, high: bool, x: u8) -> bool {
    match x {
        0..=63 => lo >> x & 1 == 1,
        64..=127 => hi >> (x - 64) & 1 == 1,
        _ => high,
    }
}

/// `at(s)`: 1 + the index of the first byte outside the class, or 0.
fn model_at(s: &[u8], c: (u64, u64, bool)) -> u64 {
    s.iter()
        .position(|&x| !member(c.0, c.1, c.2, x))
        .map_or(0, |i| i as u64 + 1)
}

/// The classes the scans are tested with: RFC 9110 `tchar`, a field value
/// (no control byte but tab, high bytes allowed), ASCII only, every byte,
/// no byte, and a class with only the edge bits 0, 63, 64 and 127 set.
const CLASSES: [(u64, u64, bool); 6] = [
    (288068722172624896, 6341068274398134270, false),
    (0xffff_ffff_0000_0200, 0x7fff_ffff_ffff_ffff, true),
    (u64::MAX, u64::MAX, false),
    (u64::MAX, u64::MAX, true),
    (0, 0, false),
    (1 | 1 << 63, 1 | 1 << 63, false),
];

fn class_source(lo: u64, hi: u64, high: bool) -> String {
    format!(
        "{PRELUDE}fn member(b: u8) -> bool {{
    let lo: u64 = {lo};
    let hi: u64 = {hi};
    let i = widen_u64(b);
    if i < 64 {{
        (rotate_right(lo, i) & 1) == 1
    }} else if i < 128 {{
        (rotate_right(hi, wrapping_sub(i, 64)) & 1) == 1
    }} else {{
        {high}
    }}
}}
fn at(in s: Slice[u8]) -> u64 {{
    match s.find_not_in({lo}, {hi}, {high}) {{
        Option::Some(i) => wrapping_add(i, 1),
        Option::None => 0,
    }}
}}
fn run() -> u64 eff [Fail[RuntimeError]] {{
    let offsets: u64 = {OFFSETS};
    let max_len: u64 = {MAX_LEN};
    let all: u64 = {BUF};
    let mut fill: u8 = 0;
    let mut found = false;
    let bytes: u64 = 256;
    for x in 0..bytes {{
        if !found && member(wrapping_u8(x)) {{
            fill = wrapping_u8(x);
            found = true;
        }}
    }}
    let mut s = noise({BUF}, 13);
    for i in 0..all {{
        if !member(s[i]) {{
            s.set(i, fill);
        }}
    }}
    let raw = noise({BUF}, 17);
    let mut acc: u64 = 1;
    for off in 0..offsets {{
        for n in 0..=max_len {{
            let hi = wrapping_add(off, n);
            acc = mix(acc, at(in s[off..hi]));
            acc = mix(acc, at(in raw[off..hi]));
            if n > 0 {{
                let three: u64 = 3;
                for r in 0..three {{
                    let j = pos(off, n, r);
                    let old = s[j];
                    s.set(j, wrapping_u8(wrapping_add(wrapping_mul(off, 31), wrapping_add(wrapping_mul(n, 7), r))));
                    acc = mix(acc, at(in s[off..hi]));
                    s.set(j, old);
                }}
            }}
        }}
    }}
    // Every byte value at the first, a middle and the last position of a
    // 40-byte interval at an unaligned start.
    let off: u64 = 3;
    let n: u64 = 40;
    let hi = wrapping_add(off, n);
    for x in 0..bytes {{
        let three: u64 = 3;
        for r in 0..three {{
            let j = pos(off, n, r);
            let old = s[j];
            s.set(j, wrapping_u8(x));
            acc = mix(acc, at(in s[off..hi]));
            s.set(j, old);
        }}
    }}
    mix(acc, at(in s.as_slice()))
}}
"
    )
}

fn class_model(c: (u64, u64, bool)) -> u64 {
    let fill = (0..=255u8).find(|&x| member(c.0, c.1, c.2, x)).unwrap_or(0);
    let mut s = model_noise(BUF, 13);
    for b in &mut s {
        if !member(c.0, c.1, c.2, *b) {
            *b = fill;
        }
    }
    let raw = model_noise(BUF, 17);
    let mut acc = 1u64;
    for off in 0..OFFSETS {
        for n in 0..=MAX_LEN {
            let hi = off + n;
            acc = mix(acc, model_at(&s[off..hi], c));
            acc = mix(acc, model_at(&raw[off..hi], c));
            if n > 0 {
                for (r, j) in [off, off + ((n * 7) >> 3), hi - 1].into_iter().enumerate() {
                    let old = s[j];
                    s[j] = (off * 31 + n * 7 + r) as u8;
                    acc = mix(acc, model_at(&s[off..hi], c));
                    s[j] = old;
                }
            }
        }
    }
    let (off, n) = (3, 40);
    for x in 0..=255u8 {
        for j in [off, off + ((n * 7) >> 3), off + n - 1] {
            let old = s[j];
            s[j] = x;
            acc = mix(acc, model_at(&s[off..off + n], c));
            s[j] = old;
        }
    }
    mix(acc, model_at(&s, c))
}

#[test]
fn find_not_in_every_class_length_offset_and_byte() {
    for (k, c) in CLASSES.into_iter().enumerate() {
        differential(
            &format!("find_not_in_{k}"),
            &class_source(c.0, c.1, c.2),
            class_model(c),
        );
    }
}

/// The class is a run-time value too, not only a constant.
#[test]
fn find_not_in_with_runtime_class() {
    let source = format!(
        "{PRELUDE}fn at(in s: Slice[u8], lo: u64, hi: u64, high: bool) -> u64 {{
    match s.find_not_in(lo, hi, high) {{
        Option::Some(i) => wrapping_add(i, 1),
        Option::None => 0,
    }}
}}
fn run() -> u64 eff [Fail[RuntimeError]] {{
    let s = noise(300, 21);
    let masks = noise(64, 23);
    let mut acc: u64 = 1;
    let rounds: u64 = 8;
    for r in 0..rounds {{
        let lo = masks.read_u64_le(wrapping_mul(r, 8)) | masks.read_u64_be(0);
        let hi = masks.read_u64_be(wrapping_mul(r, 8)) | masks.read_u64_le(0);
        let high = (r & 1) == 1;
        acc = mix(acc, at(in s.as_slice(), lo, hi, high));
        acc = mix(acc, at(in s.as_slice(), 18446744073709551615, 18446744073709551615, high));
    }}
    acc
}}
"
    );
    let s = model_noise(300, 21);
    let m = model_noise(64, 23);
    let le = |i: usize| u64::from_le_bytes(m[i..i + 8].try_into().unwrap());
    let be = |i: usize| u64::from_be_bytes(m[i..i + 8].try_into().unwrap());
    let mut acc = 1u64;
    for r in 0..8 {
        let lo = le(r * 8) | be(0);
        let hi = be(r * 8) | le(0);
        let high = r & 1 == 1;
        acc = mix(acc, model_at(&s, (lo, hi, high)));
        acc = mix(acc, model_at(&s, (u64::MAX, u64::MAX, high)));
    }
    differential("find_not_in_runtime", &source, acc);
}

#[test]
fn find_either_every_length_offset_and_position() {
    // Needles planted at every position `j` of every interval, alone and
    // both at once, plus a pair that never occurs and a repeated needle.
    let source = format!(
        "{PRELUDE}fn at(in s: Slice[u8], a: u8, b: u8) -> u64 {{
    match s.find_either(a, b) {{
        Option::Some(i) => wrapping_add(i, 1),
        Option::None => 0,
    }}
}}
fn run() -> u64 eff [Fail[RuntimeError]] {{
    let offsets: u64 = {OFFSETS};
    let max_len: u64 = {MAX_LEN};
    let mut s = noise({BUF}, 9);
    let all: u64 = {BUF};
    for i in 0..all {{
        if s[i] == 13 || s[i] == 10 {{
            s.set(i, 32);
        }}
    }}
    let mut acc: u64 = 1;
    for off in 0..offsets {{
        for n in 0..=max_len {{
            let hi = wrapping_add(off, n);
            acc = mix(acc, at(in s[off..hi], 13, 10));
            acc = mix(acc, at(in s[off..hi], s[wrapping_add(off, n & 3)], 10));
            if n > 0 {{
                let three: u64 = 3;
                for r in 0..three {{
                    let j = pos(off, n, r);
                    let old = s[j];
                    s.set(j, 10);
                    acc = mix(acc, at(in s[off..hi], 13, 10));
                    acc = mix(acc, at(in s[off..hi], 10, 10));
                    let k = pos(off, n, wrapping_sub(2, r));
                    let old_k = s[k];
                    s.set(k, 13);
                    acc = mix(acc, at(in s[off..hi], 13, 10));
                    acc = mix(acc, at(in s[off..hi], 10, 13));
                    s.set(k, old_k);
                    s.set(j, old);
                }}
            }}
        }}
    }}
    acc
}}
"
    );
    let mut s = model_noise(BUF, 9);
    for b in &mut s {
        if *b == 13 || *b == 10 {
            *b = 32;
        }
    }
    let at = |s: &[u8], a: u8, b: u8| {
        s.iter()
            .position(|&x| x == a || x == b)
            .map_or(0, |i| i as u64 + 1)
    };
    let mut acc = 1u64;
    for off in 0..OFFSETS {
        for n in 0..=MAX_LEN {
            let hi = off + n;
            acc = mix(acc, at(&s[off..hi], 13, 10));
            acc = mix(acc, at(&s[off..hi], s[off + (n & 3)], 10));
            if n > 0 {
                let ps = [off, off + ((n * 7) >> 3), hi - 1];
                for r in 0..3 {
                    let j = ps[r];
                    let old = s[j];
                    s[j] = 10;
                    acc = mix(acc, at(&s[off..hi], 13, 10));
                    acc = mix(acc, at(&s[off..hi], 10, 10));
                    let k = ps[2 - r];
                    let old_k = s[k];
                    s[k] = 13;
                    acc = mix(acc, at(&s[off..hi], 13, 10));
                    acc = mix(acc, at(&s[off..hi], 10, 13));
                    s[k] = old_k;
                    s[j] = old;
                }
            }
        }
    }
    differential("find_either", &source, acc);
}

#[test]
fn eq_ignore_ascii_case_every_length_offset_and_byte() {
    // `t` is `s` with the case bit (0x20) flipped on every ASCII letter, so
    // the two match ignoring case. Flipping the case bit of byte `j` of
    // `t` keeps them equal only when that byte is a letter: `@`/`` ` ``,
    // `[`/`{` and the high bytes sit next to the letter ranges.
    let source = format!(
        "{PRELUDE}fn same(in a: Slice[u8], in b: Slice[u8]) -> u64 {{
    bit(a.eq_ignore_ascii_case(in b))
}}
fn letter(b: u8) -> bool {{
    (b >= 65 && b <= 90) || (b >= 97 && b <= 122)
}}
fn run() -> u64 eff [Fail[RuntimeError]] {{
    let offsets: u64 = {OFFSETS};
    let max_len: u64 = {MAX_LEN};
    let s = noise({BUF}, 19);
    let mut t = noise({BUF}, 19);
    let all: u64 = {BUF};
    for i in 0..all {{
        let x = t[i];
        if letter(x) {{
            t.set(i, x ^ 32);
        }}
    }}
    let mut acc: u64 = 1;
    for off in 0..offsets {{
        for n in 0..=max_len {{
            let hi = wrapping_add(off, n);
            acc = mix(acc, same(in s[off..hi], in t[off..hi]));
            acc = mix(acc, same(in s[off..hi], in s[off..hi]));
            if n > 0 {{
                let short = wrapping_sub(hi, 1);
                acc = mix(acc, same(in s[off..hi], in t[off..short]));
                let three: u64 = 3;
                for r in 0..three {{
                    let j = pos(off, n, r);
                    let old = t[j];
                    t.set(j, old ^ 32);
                    acc = mix(acc, same(in s[off..hi], in t[off..hi]));
                    t.set(j, old ^ 1);
                    acc = mix(acc, same(in s[off..hi], in t[off..hi]));
                    t.set(j, old);
                }}
            }}
        }}
    }}
    // Every byte against every byte that differs from it by the case bit.
    let mut a: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let mut b: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let bytes: u64 = 256;
    for x in 0..bytes {{
        a.push(wrapping_u8(x));
        b.push(wrapping_u8(x) ^ 32);
    }}
    for x in 0..bytes {{
        let lo = x & 240;
        let hi = wrapping_add(lo, 16);
        acc = mix(acc, same(in a[x..wrapping_add(x, 1)], in b[x..wrapping_add(x, 1)]));
        acc = mix(acc, same(in a[lo..hi], in b[lo..hi]));
    }}
    mix(acc, same(in a.as_slice(), in b.as_slice()))
}}
"
    );
    let s = model_noise(BUF, 19);
    let mut t: Vec<u8> = s
        .iter()
        .map(|&b| if b.is_ascii_alphabetic() { b ^ 32 } else { b })
        .collect();
    let same = |a: &[u8], b: &[u8]| u64::from(a.eq_ignore_ascii_case(b));
    let mut acc = 1u64;
    for off in 0..OFFSETS {
        for n in 0..=MAX_LEN {
            let hi = off + n;
            acc = mix(acc, same(&s[off..hi], &t[off..hi]));
            acc = mix(acc, same(&s[off..hi], &s[off..hi]));
            if n > 0 {
                acc = mix(acc, same(&s[off..hi], &t[off..hi - 1]));
                for j in [off, off + ((n * 7) >> 3), hi - 1] {
                    let old = t[j];
                    t[j] = old ^ 32;
                    acc = mix(acc, same(&s[off..hi], &t[off..hi]));
                    t[j] = old ^ 1;
                    acc = mix(acc, same(&s[off..hi], &t[off..hi]));
                    t[j] = old;
                }
            }
        }
    }
    let a: Vec<u8> = (0..=255u8).collect();
    let b: Vec<u8> = a.iter().map(|x| x ^ 32).collect();
    for x in 0..256 {
        let lo = x & 240;
        acc = mix(acc, same(&a[x..x + 1], &b[x..x + 1]));
        acc = mix(acc, same(&a[lo..lo + 16], &b[lo..lo + 16]));
    }
    differential("eq_ignore_ascii_case", &source, mix(acc, same(&a, &b)));
}

#[test]
fn extend_ascii_lowercase_every_length_and_offset() {
    // Each interval is appended to an empty sequence and to one that
    // already holds a byte (so it grows), and both are compared with a
    // byte-by-byte lowercase copy of the whole buffer. The concatenation
    // of the longest results and every byte value are hashed.
    let source = format!(
        "{PRELUDE}fn hash(in s: Slice[u8]) -> u64 {{
    let mut acc: u64 = s.len();
    for i in 0..s.len() {{
        let b: u64 = match s.try_get(i) {{
            Option::Some(b) => widen_u64(b),
            Option::None => 999,
        }};
        acc = mix(acc, b);
    }}
    acc
}}
fn same(in a: Slice[u8], in b: Slice[u8]) -> u64 {{
    bit(a.eq(in b))
}}
fn run() -> u64 eff [Fail[RuntimeError]] {{
    let offsets: u64 = {OFFSETS};
    let max_len: u64 = {MAX_LEN};
    let all: u64 = {BUF};
    let s = noise({BUF}, 29);
    let mut lower: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for i in 0..all {{
        let x = s[i];
        if x >= 65 && x <= 90 {{
            lower.push(x | 32);
        }} else {{
            lower.push(x);
        }}
    }}
    let mut acc: u64 = 1;
    let mut long: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for off in 0..offsets {{
        for n in 0..=max_len {{
            let hi = wrapping_add(off, n);
            let mut out: ChunkSeq[u8] = ChunkSeq::new[u8]();
            out.extend_ascii_lowercase(in s[off..hi]);
            acc = mix(acc, bit(out.eq(in lower[off..hi])));
            let mut grown: ChunkSeq[u8] = ChunkSeq::with_capacity[u8](1);
            grown.push(65);
            grown.extend_ascii_lowercase(in s[off..hi]);
            let end = wrapping_add(n, 1);
            acc = mix(acc, bit(grown.len() == end && grown[0] == 65));
            acc = mix(acc, same(in lower[off..hi], in grown[1..end]));
            if n == max_len {{
                long.extend_ascii_lowercase(in grown.as_slice());
            }}
        }}
    }}
    let mut every: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let bytes: u64 = 256;
    for x in 0..bytes {{
        every.push(wrapping_u8(x));
    }}
    let mut lowered: ChunkSeq[u8] = ChunkSeq::new[u8]();
    lowered.extend_ascii_lowercase(in every.as_slice());
    acc = mix(acc, hash(in lowered.as_slice()));
    mix(acc, hash(in long.as_slice()))
}}
"
    );
    let s = model_noise(BUF, 29);
    let hash = |s: &[u8]| {
        s.iter()
            .fold(s.len() as u64, |acc, &b| mix(acc, u64::from(b)))
    };
    let lower = |s: &[u8]| s.to_ascii_lowercase();
    let mut acc = 1u64;
    let mut long = Vec::new();
    for off in 0..OFFSETS {
        for n in 0..=MAX_LEN {
            for _ in 0..3 {
                acc = mix(acc, 1);
            }
            if n == MAX_LEN {
                let mut grown = vec![65u8];
                grown.extend(lower(&s[off..off + n]));
                long.extend(lower(&grown));
            }
        }
    }
    let every: Vec<u8> = (0..=255u8).collect();
    acc = mix(acc, hash(&lower(&every)));
    differential("extend_ascii_lowercase", &source, mix(acc, hash(&long)));
}

/// The optimized build of each builtin holds 128-bit vector code and the
/// unoptimized one does not. `find_not_in` needs a byte shuffle: AArch64
/// has one in its baseline, the x86-64 baseline (no SSSE3) runs it scalar.
#[test]
fn optimized_objects_use_vectors() {
    let source = "fn main() -> u64 {
    let n: u64 = 100;
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for i in 0..n {
        s.push(wrapping_u8(i));
    }
    let found: u64 = match s.find_either(13, 10) {
        Option::Some(i) => i,
        Option::None => 0,
    };
    let same: u64 = if s.eq_ignore_ascii_case(in s[0..n]) { 1 } else { 0 };
    let mut out: ChunkSeq[u8] = ChunkSeq::new[u8]();
    out.extend_ascii_lowercase(in s.as_slice());
    wrapping_add(wrapping_add(out.len(), found), same)
}
";
    let class = "fn main() -> u64 {
    let n: u64 = 100;
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for i in 0..n {
        s.push(wrapping_u8(i));
    }
    match s.find_not_in(288068722172624896, 6341068274398134270, false) {
        Option::Some(i) => i,
        Option::None => 0,
    }
}
";
    let c = accepted(source);
    let k = accepted(class);
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (_, fast) = native::object_with_options(&c.world, "main", Some(target), true)
            .unwrap_or_else(|e| panic!("{target}: optimized object failed: {e}"));
        for needle in ["i8x16", "vhigh_bits", "store little"] {
            assert!(fast.contains(needle), "{target}: no {needle} in\n{fast}");
        }
        let (_, slow) = native::object_with_options(&c.world, "main", Some(target), false)
            .unwrap_or_else(|e| panic!("{target}: object failed: {e}"));
        assert!(
            !slow.contains("i8x16"),
            "{target}: unoptimized object uses vectors"
        );
        let (_, fast) = native::object_with_options(&k.world, "main", Some(target), true)
            .unwrap_or_else(|e| panic!("{target}: optimized class object failed: {e}"));
        let aarch64 = target.starts_with("aarch64");
        assert_eq!(
            fast.contains("swizzle"),
            aarch64,
            "{target}: find_not_in vectorized: {}\n{fast}",
            fast.contains("swizzle")
        );
    }
}

#[test]
fn builtins_need_byte_elements_and_typed_operands() {
    rejected(
        "fn main() -> bool {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    s.push(1);
    s.eq_ignore_ascii_case(in s.as_slice())
}
",
        "E1525",
        "'eq_ignore_ascii_case' reads and writes bytes",
    );
    rejected(
        "fn main() -> u64 {
    let s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    match s.find_not_in(1, 2) {
        Option::Some(i) => i,
        Option::None => 0,
    }
}
",
        "E0207",
        "'find_not_in' expects 3 arguments",
    );
    rejected(
        "fn main() -> u64 {
    let s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    match s.find_not_in(1, 2, 3) {
        Option::Some(i) => i,
        Option::None => 0,
    }
}
",
        "E0200",
        "expected bool",
    );
    rejected(
        "fn main() -> u64 {
    let s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    match s.find_either(13, true) {
        Option::Some(i) => i,
        Option::None => 0,
    }
}
",
        "E0200",
        "expected u8, found bool",
    );
    rejected(
        "fn main() -> u64 {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    s.push(65);
    s.extend_ascii_lowercase(in s.as_slice());
    s.len()
}
",
        "E0301",
        "cannot read from the sequence it writes",
    );
}
