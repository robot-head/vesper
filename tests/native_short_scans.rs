//! Short byte scans (port speed wave, lane 6, #1026): optimized native code
//! runs `find_byte`, `find_either`, `find_not_in`, `eq` and
//! `eq_ignore_ascii_case` on fewer than 16 bytes as one 16-byte vector step
//! that may read past the scanned bytes, but never across a 4 KiB boundary,
//! and masks the result to the scanned lanes. Each scan is checked against
//! a Rust model at every start in a buffer longer than a page (so some views
//! start in the last 15 bytes of a page, which take the scalar path) and every
//! length 0..=17, with the bytes just past each view set to values that would
//! change the result if they were not masked off. The evaluator runs a
//! shorter sweep; the JIT with and without `optimize`, and an AOT executable,
//! run the whole one.
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
            "vesper-native-short-scans-{}-{}",
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

/// Runs `main` in the evaluator with `small` (a short sweep), and in both
/// JIT configurations and (where a C driver links executables) an AOT
/// executable with `large`, checking each result against its model.
fn differential(name: &str, small: (&str, u64), large: (&str, u64)) {
    let c = accepted(small.0);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    assert_eq!(evaluated, small.1.to_string(), "{name}: evaluator vs model");
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: small JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(jit, evaluated, "{name}: small JIT (optimize {optimize})");
    }
    let c = accepted(large.0);
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            jit,
            large.1.to_string(),
            "{name}: JIT (optimize {optimize})"
        );
    }
    if links_executables() {
        assert_eq!(
            executable_status(name, large.0),
            Some((large.1 & 0xff) as i32),
            "{name}: executable"
        );
    }
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

/// Views start at `0..offsets`; the large sweep covers more than a page.
const SMALL: usize = 40;
const LARGE: usize = 4128;
const MAX_LEN: usize = 17;
/// The second view of `eq` starts this many bytes later.
const SHIFT: usize = 5;
const BUF: usize = LARGE + MAX_LEN + SHIFT;

/// Whether `i` is a multiple of `m`. The Vesper side keeps a counter per
/// modulus instead of `%`.
fn every(i: usize, m: usize) -> bool {
    i.is_multiple_of(m)
}

/// The buffers of each scan, built the same way by [`buffers_source`].
/// `find_byte` looks for 7, `find_either` for 7 or 200, `find_not_in` for a
/// byte outside RFC 9110 `tchar`; `eq` compares `s` with `t`, which differs
/// in one bit every 37 bytes, and `eq_ignore_ascii_case` with `t`, which also
/// flips the case of every letter.
fn model_buffers(op: &str) -> (Vec<u8>, Vec<u8>) {
    let noise = model_noise(BUF, 41);
    let mut s = noise.clone();
    for (i, x) in s.iter_mut().enumerate() {
        match op {
            "find_byte" | "find_either" => {
                if *x == 7 || *x == 200 {
                    *x = 8;
                }
                if every(i, 23) {
                    *x = 7;
                } else if op == "find_either" && every(i, 29) {
                    *x = 200;
                }
            }
            "find_not_in" => {
                *x = 97 + (*x & 15);
                if every(i, 23) {
                    *x = 32;
                } else if every(i, 31) {
                    *x = 200;
                }
            }
            _ => {}
        }
    }
    let mut t = s.clone();
    for (i, x) in t.iter_mut().enumerate() {
        if op == "eq_ignore_ascii_case" && x.is_ascii_alphabetic() {
            *x ^= 32;
        }
        if every(i, 37) {
            *x ^= 1;
        }
    }
    (s, t)
}

const TCHAR: (u64, u64) = (288068722172624896, 6341068274398134270);

fn model_tchar(x: u8) -> bool {
    match x {
        0..=63 => TCHAR.0 >> x & 1 == 1,
        64..=127 => TCHAR.1 >> (x - 64) & 1 == 1,
        _ => false,
    }
}

fn model_scan(op: &str, offsets: usize) -> u64 {
    let (s, t) = model_buffers(op);
    let found = |p: Option<usize>| p.map_or(0, |i| i as u64 + 1);
    let mut acc = 1u64;
    for off in 0..offsets {
        for n in 0..=MAX_LEN {
            let v = &s[off..off + n];
            match op {
                "find_byte" => acc = mix(acc, found(v.iter().position(|&x| x == 7))),
                "find_either" => acc = mix(acc, found(v.iter().position(|&x| x == 7 || x == 200))),
                "find_not_in" => acc = mix(acc, found(v.iter().position(|&x| !model_tchar(x)))),
                _ => {
                    let w = &t[off..off + n];
                    let u = &t[off + SHIFT..off + SHIFT + n];
                    let same = |a: &[u8], b: &[u8]| {
                        u64::from(if op == "eq" {
                            a == b
                        } else {
                            a.eq_ignore_ascii_case(b)
                        })
                    };
                    acc = mix(acc, same(v, w));
                    acc = mix(acc, same(v, u));
                }
            }
        }
    }
    acc
}

fn scan_source(op: &str, offsets: usize) -> String {
    let (lo, hi) = TCHAR;
    let scan = match op {
        "find_byte" => "s.find_byte(7)".to_string(),
        "find_either" => "s.find_either(7, 200)".to_string(),
        "find_not_in" => format!("s.find_not_in({lo}, {hi}, false)"),
        "eq" => "s.eq(in t)".to_string(),
        _ => "s.eq_ignore_ascii_case(in t)".to_string(),
    };
    let (shape, fill_s, fill_t, call) = if op.starts_with("find") {
        (
            format!(
                "fn at(in s: Slice[u8]) -> u64 {{
    match {scan} {{
        Option::Some(i) => wrapping_add(i, 1),
        Option::None => 0,
    }}
}}"
            ),
            match op {
                "find_not_in" => {
                    "        x = wrapping_add(97, x & 15);
        if c23 == 0 {
            x = 32;
        } else if c31 == 0 {
            x = 200;
        }"
                }
                "find_byte" => {
                    "        if x == 7 || x == 200 {
            x = 8;
        }
        if c23 == 0 {
            x = 7;
        }"
                }
                _ => {
                    "        if x == 7 || x == 200 {
            x = 8;
        }
        if c23 == 0 {
            x = 7;
        } else if c29 == 0 {
            x = 200;
        }"
                }
            },
            "",
            "acc = mix(acc, at(in s[off..hi]));",
        )
    } else {
        (
            format!(
                "fn same(in s: Slice[u8], in t: Slice[u8]) -> u64 {{
    bit({scan})
}}"
            ),
            "",
            if op == "eq" {
                ""
            } else {
                "        if (y >= 65 && y <= 90) || (y >= 97 && y <= 122) {
            y = y ^ 32;
        }"
            },
            "acc = mix(acc, same(in s[off..hi], in t[off..hi]));
            let off2 = wrapping_add(off, shift);
            acc = mix(acc, same(in s[off..hi], in t[off2..wrapping_add(off2, n)]));",
        )
    };
    format!(
        "{PRELUDE}{shape}
fn run() -> u64 eff [Fail[RuntimeError]] {{
    let all: u64 = {BUF};
    let offsets: u64 = {offsets};
    let max_len: u64 = {MAX_LEN};
    let shift: u64 = {SHIFT};
    let raw = noise({BUF}, 41);
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let mut t: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let mut c23: u64 = 0;
    let mut c29: u64 = 0;
    let mut c31: u64 = 0;
    let mut c37: u64 = 0;
    for i in 0..all {{
        let mut x = raw[i];
{fill_s}
        s.push(x);
        c23 = wrapping_add(c23, 1);
        c29 = wrapping_add(c29, 1);
        c31 = wrapping_add(c31, 1);
        if c23 == 23 {{
            c23 = 0;
        }}
        if c29 == 29 {{
            c29 = 0;
        }}
        if c31 == 31 {{
            c31 = 0;
        }}
    }}
    for i in 0..all {{
        let mut y = s[i];
{fill_t}
        if c37 == 0 {{
            y = y ^ 1;
        }}
        t.push(y);
        c37 = wrapping_add(c37, 1);
        if c37 == 37 {{
            c37 = 0;
        }}
    }}
    let mut acc: u64 = 1;
    for off in 0..offsets {{
        for n in 0..=max_len {{
            let hi = wrapping_add(off, n);
            {call}
        }}
    }}
    acc
}}
"
    )
}

fn check_scan(op: &str) {
    differential(
        op,
        (&scan_source(op, SMALL), model_scan(op, SMALL)),
        (&scan_source(op, LARGE), model_scan(op, LARGE)),
    );
}

#[test]
fn short_find_byte_every_start_across_a_page() {
    check_scan("find_byte");
}

#[test]
fn short_find_either_every_start_across_a_page() {
    check_scan("find_either");
}

#[test]
fn short_find_not_in_every_start_across_a_page() {
    check_scan("find_not_in");
}

#[test]
fn short_eq_every_start_across_a_page() {
    check_scan("eq");
}

#[test]
fn short_eq_ignore_ascii_case_every_start_across_a_page() {
    check_scan("eq_ignore_ascii_case");
}

/// Optimized hosted code takes the short vector step (its 4 KiB boundary
/// test is in the function) on both targets; unoptimized code and
/// freestanding images do not over-read.
#[test]
fn optimized_hosted_objects_have_the_short_step() {
    let source = "fn main() -> u64 {
    let n: u64 = 10;
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for i in 0..n {
        s.push(wrapping_u8(i));
    }
    match s.find_byte(7) {
        Option::Some(i) => i,
        Option::None => 0,
    }
}
";
    let c = accepted(source);
    let page = "4095";
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (_, fast) = native::object_with_options(&c.world, "main", Some(target), true)
            .unwrap_or_else(|e| panic!("{target}: optimized object failed: {e}"));
        assert!(fast.contains(page), "{target}: no short step in\n{fast}");
        let (_, slow) = native::object_with_options(&c.world, "main", Some(target), false)
            .unwrap_or_else(|e| panic!("{target}: object failed: {e}"));
        assert!(!slow.contains(page), "{target}: unoptimized short step");
    }
    for target in ["x86_64-unknown-none", "aarch64-unknown-none"] {
        let Ok((_, fast)) = native::object_with_options(&c.world, "main", Some(target), true)
        else {
            continue;
        };
        assert!(
            !fast.contains(page),
            "{target}: freestanding short step in\n{fast}"
        );
    }
}
