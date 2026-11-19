//! `@vesper-std/sort` and the std helpers of P3b gaps lane 11 (#1096):
//! `sort`, `stable_sort`, `is_sorted`, `sort_by_key`, `sort_pairs`, the
//! float sorts, saturating arithmetic, `all_zero` and `ct_eq`. A driver
//! package sorts generated inputs (random, sorted, reversed, equal, few
//! distinct, sawtooth, organ pipe) of many lengths and element types and
//! folds the results into one digest, which must equal the digest of the
//! same inputs sorted by Rust's `sort_unstable` / stable `sort` /
//! `total_cmp`. The driver runs in the JIT with and without `optimize`, as
//! an executable, and (on small inputs) in the evaluator.
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
            "vesper-std-sort-{}-{}",
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

const STD: &str = "packages/vesper-std";

const DRIVER: &str = "use @vesper-std/sort::unstable::{sort, is_sorted};
use @vesper-std/sort::stable::stable_sort;
use @vesper-std/sort::keyed::{sort_by_key, sort_pairs};
use @vesper-std/sort::floats::{sort_f64, stable_sort_f64, is_sorted_f64, sort_f32_bits};

copy struct P {
    a: u32,
    b: u64
}

impl Eq for P {}

impl Ord for P {}

fn mix(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 1099511628211), v)
}

fn bit(b: bool) -> u64 {
    let one: u64 = 1;
    let zero: u64 = 0;
    if b { one } else { zero }
}

// Input `i` of pattern `p` for length `n`, from the LCG state `x`.
fn value(p: u64, i: u64, n: u64, x: u64) -> u64 {
    if p == 0 {
        x >> 1
    } else if p == 1 {
        i
    } else if p == 2 {
        wrapping_sub(n, i)
    } else if p == 3 {
        7
    } else if p == 4 {
        x >> 61
    } else if p == 5 {
        i & 15
    } else if i < (n >> 1) {
        i
    } else {
        wrapping_sub(n, i)
    }
}

fn input(p: u64, n: u64, seed: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::with_capacity[u64](n);
    let mut x = seed;
    for i in 0..n {
        x = wrapping_add(wrapping_mul(x, 6364136223846793005), 1442695040888963407);
        s.push(value(p, i, n, x));
    }
    s
}

fn h64(acc: u64, in s: Slice[u64]) -> u64 {
    let mut a = mix(acc, s.len());
    for i in 0..s.len() {
        a = match s.try_get(i) {
            Option::Some(v) => mix(a, v),
            Option::None => a,
        };
    }
    a
}

fn h32(acc: u64, in s: Slice[u32]) -> u64 {
    let mut a = mix(acc, s.len());
    for i in 0..s.len() {
        a = match s.try_get(i) {
            Option::Some(v) => mix(a, widen_u64(v)),
            Option::None => a,
        };
    }
    a
}

fn h8(acc: u64, in s: Slice[u8]) -> u64 {
    let mut a = mix(acc, s.len());
    for i in 0..s.len() {
        a = match s.try_get(i) {
            Option::Some(v) => mix(a, widen_u64(v)),
            Option::None => a,
        };
    }
    a
}

fn hi64(acc: u64, in s: Slice[i64]) -> u64 {
    let mut a = mix(acc, s.len());
    for i in 0..s.len() {
        a = match s.try_get(i) {
            Option::Some(v) => mix(a, wrapping_u64(v)),
            Option::None => a,
        };
    }
    a
}

fn hf64(acc: u64, in s: Slice[f64]) -> u64 {
    let mut a = mix(acc, s.len());
    for i in 0..s.len() {
        a = match s.try_get(i) {
            Option::Some(v) => mix(a, to_bits_f64(v)),
            Option::None => a,
        };
    }
    a
}

fn one_case(acc0: u64, p: u64, n: u64) -> u64 {
    let mut acc = acc0;
    let src = input(p, n, wrapping_add(wrapping_mul(p, 1000), n));
    let mut a: ChunkSeq[u64] = ChunkSeq::new[u64]();
    a.extend(in src.as_slice());
    acc = mix(acc, bit(is_sorted(in a.as_slice())));
    sort(inout a.as_slice_mut());
    acc = h64(acc, in a.as_slice());
    acc = mix(acc, bit(is_sorted(in a.as_slice())));
    let mut st: ChunkSeq[u64] = ChunkSeq::new[u64]();
    st.extend(in src.as_slice());
    stable_sort(inout st.as_slice_mut());
    acc = h64(acc, in st.as_slice());
    let mut si: ChunkSeq[i64] = ChunkSeq::new[i64]();
    let mut s32: ChunkSeq[u32] = ChunkSeq::new[u32]();
    let mut s8: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let mut ps: ChunkSeq[P] = ChunkSeq::new[P]();
    let mut keys: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let mut order: ChunkSeq[u32] = ChunkSeq::new[u32]();
    let mut vals: ChunkSeq[u32] = ChunkSeq::new[u32]();
    let mut fs: ChunkSeq[f64] = ChunkSeq::new[f64]();
    let mut fb: ChunkSeq[u32] = ChunkSeq::new[u32]();
    for i in 0..n {
        let v = match src.try_get(i) {
            Option::Some(v) => v,
            Option::None => 0,
        };
        let mixed = mix(v, i);
        si.push(wrapping_i64(wrapping_mul(v, 11400714819323198485)));
        s32.push(wrapping_u32(mixed));
        s8.push(wrapping_u8(mixed));
        ps.push(P { a: wrapping_u32(mixed >> 62), b: wrapping_u64(v) });
        keys.push(mixed >> 61);
        order.push(wrapping_u32(i));
        vals.push(wrapping_u32(i));
        fs.push(from_bits_f64(wrapping_mul(mixed, 0x9e3779b97f4a7c15)));
        fb.push(wrapping_u32(wrapping_mul(mixed, 0x9e3779b97f4a7c15) >> 32));
    }
    sort(inout si.as_slice_mut());
    acc = hi64(acc, in si.as_slice());
    sort(inout s32.as_slice_mut());
    acc = h32(acc, in s32.as_slice());
    stable_sort(inout s8.as_slice_mut());
    acc = h8(acc, in s8.as_slice());
    sort(inout ps.as_slice_mut());
    for i in 0..ps.len() {
        acc = match ps.try_get(i) {
            Option::Some(q) => mix(mix(acc, widen_u64(q.a)), q.b),
            Option::None => acc,
        };
    }
    sort_by_key(in keys.as_slice(), inout order.as_slice_mut());
    acc = h32(acc, in order.as_slice());
    sort_pairs(inout keys.as_slice_mut(), inout vals.as_slice_mut());
    acc = h64(acc, in keys.as_slice());
    acc = h32(acc, in vals.as_slice());
    let mut fs2: ChunkSeq[f64] = ChunkSeq::new[f64]();
    fs2.extend(in fs.as_slice());
    sort_f64(inout fs.as_slice_mut());
    acc = hf64(acc, in fs.as_slice());
    acc = mix(acc, bit(is_sorted_f64(in fs.as_slice())));
    stable_sort_f64(inout fs2.as_slice_mut());
    acc = hf64(acc, in fs2.as_slice());
    sort_f32_bits(inout fb.as_slice_mut());
    acc = h32(acc, in fb.as_slice());
    acc
}

fn main() -> u64 {
    let mut acc: u64 = 1;
    let sizes: ChunkSeq[u64] = SIZES;
    let patterns: u64 = 7;
    for k in 0..sizes.len() {
        let n = match sizes.try_get(k) {
            Option::Some(n) => n,
            Option::None => 0,
        };
        for p in 0..patterns {
            acc = one_case(acc, p, n);
        }
    }
    acc
}
";

const BIG: [u64; 14] = [0, 1, 2, 3, 7, 16, 17, 18, 31, 100, 257, 1000, 1500, 4096];
const SMALL: [u64; 8] = [0, 1, 2, 3, 16, 17, 18, 40];

fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1099511628211).wrapping_add(v)
}

fn value(p: u64, i: u64, n: u64, x: u64) -> u64 {
    match p {
        0 => x >> 1,
        1 => i,
        2 => n.wrapping_sub(i),
        3 => 7,
        4 => x >> 61,
        5 => i & 15,
        _ if i < n >> 1 => i,
        _ => n.wrapping_sub(i),
    }
}

fn input(p: u64, n: u64, seed: u64) -> Vec<u64> {
    let mut x = seed;
    (0..n)
        .map(|i| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            value(p, i, n, x)
        })
        .collect()
}

fn h(acc: u64, s: impl ExactSizeIterator<Item = u64>) -> u64 {
    let mut a = mix(acc, s.len() as u64);
    for v in s {
        a = mix(a, v);
    }
    a
}

fn model_case(acc0: u64, p: u64, n: u64) -> u64 {
    let mut acc = acc0;
    let src = input(p, n, p * 1000 + n);
    let mut a = src.clone();
    acc = mix(acc, u64::from(a.is_sorted()));
    a.sort_unstable();
    acc = h(acc, a.iter().copied());
    acc = mix(acc, 1);
    acc = h(acc, a.iter().copied());
    let mixed: Vec<u64> = src
        .iter()
        .enumerate()
        .map(|(i, &v)| mix(v, i as u64))
        .collect();
    let mut si: Vec<i64> = src
        .iter()
        .map(|&v| v.wrapping_mul(11400714819323198485) as i64)
        .collect();
    si.sort_unstable();
    acc = h(acc, si.iter().map(|&v| v as u64));
    let mut s32: Vec<u32> = mixed.iter().map(|&m| m as u32).collect();
    s32.sort_unstable();
    acc = h(acc, s32.iter().map(|&v| u64::from(v)));
    let mut s8: Vec<u8> = mixed.iter().map(|&m| m as u8).collect();
    s8.sort();
    acc = h(acc, s8.iter().map(|&v| u64::from(v)));
    let mut ps: Vec<(u32, u64)> = mixed
        .iter()
        .zip(&src)
        .map(|(&m, &v)| ((m >> 62) as u32, v))
        .collect();
    ps.sort_unstable();
    for (a, b) in &ps {
        acc = mix(mix(acc, u64::from(*a)), *b);
    }
    let keys: Vec<u64> = mixed.iter().map(|&m| m >> 61).collect();
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_by_key(|&i| keys[i as usize]);
    acc = h(acc, order.iter().map(|&v| u64::from(v)));
    let mut pairs: Vec<(u64, u32)> = keys.iter().copied().zip(0..n as u32).collect();
    pairs.sort_by_key(|p| p.0);
    acc = h(acc, pairs.iter().map(|p| p.0));
    acc = h(acc, pairs.iter().map(|p| u64::from(p.1)));
    let mut fs: Vec<f64> = mixed
        .iter()
        .map(|&m| f64::from_bits(m.wrapping_mul(0x9e3779b97f4a7c15)))
        .collect();
    let mut fs2 = fs.clone();
    fs.sort_unstable_by(f64::total_cmp);
    acc = h(acc, fs.iter().map(|v| v.to_bits()));
    acc = mix(acc, 1);
    fs2.sort_by(f64::total_cmp);
    acc = h(acc, fs2.iter().map(|v| v.to_bits()));
    let mut fb: Vec<f32> = mixed
        .iter()
        .map(|&m| f32::from_bits((m.wrapping_mul(0x9e3779b97f4a7c15) >> 32) as u32))
        .collect();
    fb.sort_unstable_by(f32::total_cmp);
    acc = h(acc, fb.iter().map(|v| u64::from(v.to_bits())));
    acc
}

fn model(sizes: &[u64]) -> u64 {
    let mut acc = 1u64;
    for &n in sizes {
        for p in 0..7 {
            acc = model_case(acc, p, n);
        }
    }
    acc
}

/// A temporary package `@local/sort-driver` with `main.ves` = `source`,
/// depending on the std packages by absolute path.
fn driver(source: &str) -> Temp {
    let temp = Temp::new();
    let std = fs::canonicalize(STD).unwrap();
    let dep = |name: &str| {
        format!(
            "\"@vesper-std/{name}\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
            std.join(name).display().to_string().replace('\\', "/")
        )
    };
    let manifest = format!(
        "[package]\nname = \"@local/sort-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Fail\", \"Alloc\"]\n\n[dependencies]\n{}{}{}",
        dep("sort"),
        dep("core"),
        dep("bytes")
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

fn sizes_literal(sizes: &[u64]) -> String {
    // A sequence literal built at run time: the sizes in pushes.
    let mut s = String::from("{\n        let mut z: ChunkSeq[u64] = ChunkSeq::new[u64]();\n");
    for n in sizes {
        s.push_str(&format!("        z.push({n});\n"));
    }
    s.push_str("        z\n    }");
    s
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

/// The sorts agree with Rust on every pattern and length, natively.
#[test]
fn sorts_match_rust_natively() {
    let source = DRIVER.replace("SIZES", &sizes_literal(&BIG));
    let dir = driver(&source);
    let c = load(&dir.0);
    let expected = model(&BIG);
    for optimize in [true, false] {
        let got = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(got, expected.to_string(), "JIT (optimize {optimize})");
    }
    if links_executables() {
        assert_eq!(
            executable_exit(&dir.0),
            Some((expected & 0xff) as i32),
            "executable"
        );
    }
}

/// The evaluator, the reference semantics, agrees on small inputs.
#[test]
fn sorts_match_rust_in_the_evaluator() {
    let source = DRIVER.replace("SIZES", &sizes_literal(&SMALL));
    let dir = driver(&source);
    let c = load(&dir.0);
    let mut runtime = Runtime::new(&c.world);
    runtime.optimize = false;
    runtime.fuel = 4_000_000_000;
    let got = runtime.run("main", vec![]).unwrap().to_string();
    assert_eq!(got, model(&SMALL).to_string());
}

const HELPERS: &str = "use @vesper-std/core::saturating::{
    saturating_add_u8, saturating_sub_u8, saturating_mul_u8,
    saturating_add_u16, saturating_sub_u16, saturating_mul_u16,
    saturating_add_u32, saturating_sub_u32, saturating_mul_u32,
    saturating_add_u64, saturating_sub_u64, saturating_mul_u64,
    saturating_add_i8, saturating_sub_i8, saturating_mul_i8,
    saturating_add_i16, saturating_sub_i16, saturating_mul_i16,
    saturating_add_i32, saturating_sub_i32, saturating_mul_i32,
    saturating_add_i64, saturating_sub_i64, saturating_mul_i64
};
use @vesper-std/bytes::ct::{all_zero, ct_eq};

fn mix(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 1099511628211), v)
}

fn bit(b: bool) -> u64 {
    let one: u64 = 1;
    let zero: u64 = 0;
    if b { one } else { zero }
}

// Edge operand `k` of 12, as a u64 bit pattern.
fn edge(k: u64) -> u64 {
    if k == 0 { 0 } else if k == 1 { 1 } else if k == 2 { 2 } else if k == 3 { 127 } else if k == 4 { 128 } else if k == 5 { 255 } else if k == 6 { 32767 } else if k == 7 { 65535 } else if k == 8 { 2147483647 } else if k == 9 { 4294967295 } else if k == 10 { 9223372036854775807 } else { 18446744073709551615 }
}

fn main() -> u64 {
    let mut acc: u64 = 1;
    let edges: u64 = 12;
    for i in 0..edges {
        for j in 0..edges {
            for neg in 0..wrapping_add(4, 0) {
                let x = if (neg & 1) != 0 { wrapping_sub(0, edge(i)) } else { edge(i) };
                let y = if (neg & 2) != 0 { wrapping_sub(0, edge(j)) } else { edge(j) };
                let (a8, b8) = (wrapping_u8(x), wrapping_u8(y));
                acc = mix(acc, widen_u64(saturating_add_u8(a8, b8)));
                acc = mix(acc, widen_u64(saturating_sub_u8(a8, b8)));
                acc = mix(acc, widen_u64(saturating_mul_u8(a8, b8)));
                let (a16, b16) = (wrapping_u16(x), wrapping_u16(y));
                acc = mix(acc, widen_u64(saturating_add_u16(a16, b16)));
                acc = mix(acc, widen_u64(saturating_sub_u16(a16, b16)));
                acc = mix(acc, widen_u64(saturating_mul_u16(a16, b16)));
                let (a32, b32) = (wrapping_u32(x), wrapping_u32(y));
                acc = mix(acc, widen_u64(saturating_add_u32(a32, b32)));
                acc = mix(acc, widen_u64(saturating_sub_u32(a32, b32)));
                acc = mix(acc, widen_u64(saturating_mul_u32(a32, b32)));
                acc = mix(acc, saturating_add_u64(x, y));
                acc = mix(acc, saturating_sub_u64(x, y));
                acc = mix(acc, saturating_mul_u64(x, y));
                let (c8, d8) = (wrapping_i8(x), wrapping_i8(y));
                acc = mix(acc, wrapping_u64(widen_i64(saturating_add_i8(c8, d8))));
                acc = mix(acc, wrapping_u64(widen_i64(saturating_sub_i8(c8, d8))));
                acc = mix(acc, wrapping_u64(widen_i64(saturating_mul_i8(c8, d8))));
                let (c16, d16) = (wrapping_i16(x), wrapping_i16(y));
                acc = mix(acc, wrapping_u64(widen_i64(saturating_add_i16(c16, d16))));
                acc = mix(acc, wrapping_u64(widen_i64(saturating_sub_i16(c16, d16))));
                acc = mix(acc, wrapping_u64(widen_i64(saturating_mul_i16(c16, d16))));
                let (c32, d32) = (wrapping_i32(x), wrapping_i32(y));
                acc = mix(acc, wrapping_u64(widen_i64(saturating_add_i32(c32, d32))));
                acc = mix(acc, wrapping_u64(widen_i64(saturating_sub_i32(c32, d32))));
                acc = mix(acc, wrapping_u64(widen_i64(saturating_mul_i32(c32, d32))));
                let (c64, d64) = (wrapping_i64(x), wrapping_i64(y));
                acc = mix(acc, wrapping_u64(saturating_add_i64(c64, d64)));
                acc = mix(acc, wrapping_u64(saturating_sub_i64(c64, d64)));
                acc = mix(acc, wrapping_u64(saturating_mul_i64(c64, d64)));
            }
        }
    }
    let lens: u64 = 70;
    for n in 0..lens {
        let mut z: ChunkSeq[u8] = ChunkSeq::new[u8]();
        z.resize(n, 0);
        let mut w: ChunkSeq[u8] = ChunkSeq::new[u8]();
        w.resize(n, 0);
        acc = mix(acc, bit(all_zero(in z.as_slice())));
        acc = mix(acc, bit(ct_eq(in z.as_slice(), in w.as_slice())));
        for at in 0..n {
            let mut d: ChunkSeq[u8] = ChunkSeq::new[u8]();
            d.extend(in z.as_slice());
            d.try_set(at, 128);
            acc = mix(acc, bit(all_zero(in d.as_slice())));
            acc = mix(acc, bit(ct_eq(in z.as_slice(), in d.as_slice())));
            acc = mix(acc, bit(ct_eq(in d.as_slice(), in d.as_slice())));
        }
        acc = mix(acc, bit(ct_eq(in z.as_slice(), in w[0..n >> 1])));
    }
    acc
}
";

fn model_helpers() -> u64 {
    let edges: [u64; 12] = [
        0,
        1,
        2,
        127,
        128,
        255,
        32767,
        65535,
        2147483647,
        4294967295,
        9223372036854775807,
        u64::MAX,
    ];
    let mut acc = 1u64;
    for &ei in &edges {
        for &ej in &edges {
            for neg in 0..4 {
                let x = if neg & 1 != 0 {
                    0u64.wrapping_sub(ei)
                } else {
                    ei
                };
                let y = if neg & 2 != 0 {
                    0u64.wrapping_sub(ej)
                } else {
                    ej
                };
                let (a8, b8) = (x as u8, y as u8);
                acc = mix(acc, u64::from(a8.saturating_add(b8)));
                acc = mix(acc, u64::from(a8.saturating_sub(b8)));
                acc = mix(acc, u64::from(a8.saturating_mul(b8)));
                let (a16, b16) = (x as u16, y as u16);
                acc = mix(acc, u64::from(a16.saturating_add(b16)));
                acc = mix(acc, u64::from(a16.saturating_sub(b16)));
                acc = mix(acc, u64::from(a16.saturating_mul(b16)));
                let (a32, b32) = (x as u32, y as u32);
                acc = mix(acc, u64::from(a32.saturating_add(b32)));
                acc = mix(acc, u64::from(a32.saturating_sub(b32)));
                acc = mix(acc, u64::from(a32.saturating_mul(b32)));
                acc = mix(acc, x.saturating_add(y));
                acc = mix(acc, x.saturating_sub(y));
                acc = mix(acc, x.saturating_mul(y));
                let (c8, d8) = (x as i8, y as i8);
                acc = mix(acc, i64::from(c8.saturating_add(d8)) as u64);
                acc = mix(acc, i64::from(c8.saturating_sub(d8)) as u64);
                acc = mix(acc, i64::from(c8.saturating_mul(d8)) as u64);
                let (c16, d16) = (x as i16, y as i16);
                acc = mix(acc, i64::from(c16.saturating_add(d16)) as u64);
                acc = mix(acc, i64::from(c16.saturating_sub(d16)) as u64);
                acc = mix(acc, i64::from(c16.saturating_mul(d16)) as u64);
                let (c32, d32) = (x as i32, y as i32);
                acc = mix(acc, i64::from(c32.saturating_add(d32)) as u64);
                acc = mix(acc, i64::from(c32.saturating_sub(d32)) as u64);
                acc = mix(acc, i64::from(c32.saturating_mul(d32)) as u64);
                let (c64, d64) = (x as i64, y as i64);
                acc = mix(acc, c64.saturating_add(d64) as u64);
                acc = mix(acc, c64.saturating_sub(d64) as u64);
                acc = mix(acc, c64.saturating_mul(d64) as u64);
            }
        }
    }
    for n in 0..70usize {
        acc = mix(acc, 1);
        acc = mix(acc, 1);
        for _ in 0..n {
            acc = mix(acc, 0);
            acc = mix(acc, 0);
            acc = mix(acc, 1);
        }
        acc = mix(acc, u64::from(n == n >> 1));
    }
    acc
}

/// Saturating arithmetic matches Rust's on every edge operand pair, and
/// `all_zero` and `ct_eq` match their definitions.
#[test]
fn helpers_match_rust() {
    let dir = driver(HELPERS);
    let c = load(&dir.0);
    let expected = model_helpers();
    for optimize in [true, false] {
        let got = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(got, expected.to_string(), "JIT (optimize {optimize})");
    }
    let mut runtime = Runtime::new(&c.world);
    runtime.optimize = false;
    runtime.fuel = 4_000_000_000;
    let got = runtime.run("main", vec![]).unwrap().to_string();
    assert_eq!(got, expected.to_string(), "evaluator");
    if links_executables() {
        assert_eq!(
            executable_exit(&dir.0),
            Some((expected & 0xff) as i32),
            "executable"
        );
    }
}

/// The sort package checks on its own within its ceiling.
#[test]
fn sort_package_checks_on_its_own() {
    load(Path::new("packages/vesper-std/sort"));
}
