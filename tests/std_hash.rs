//! `@vesper-std/hash` and `@vesper-std/hash-sys` (self-hosting SH0 lane 5,
//! #1127): BLAKE3 in portable Vesper and through the `blake3` crate behind
//! `.vsys`, and the table hashes FNV-1a, XXH64, `mix64` and `hash_pair`.
//!
//! A driver package folds the outputs of the official BLAKE3 test-vector
//! inputs (bytes `i % 251`, the 22 official lengths, 131 bytes of extended
//! output for `hash`, `keyed_hash` and `derive_key`), incremental updates
//! split at every block and chunk boundary, and the fast hashes of many
//! lengths into one digest each. Each digest must equal the same fold of the
//! `blake3` crate's output (itself checked against official vectors) and of
//! Rust references; the portable driver runs in the JIT with and without
//! `optimize`, as an executable and (on short inputs) in the evaluator. The
//! `.vsys` driver runs in the JIT bound to `hash/sys/shim.rs` and in the
//! evaluator with the same crate standing in.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::ffi_lowering::{self, Handler, Imports};
use vesper::{check, native, packages, runtime::Runtime, runtime::Value};

#[path = "../packages/vesper-std/hash/sys/shim.rs"]
mod blake3_shim;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-std-hash-{}-{}",
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

/// A temporary package with `main.ves` = `source` and `deps` (paths under
/// packages/vesper-std) as dependencies.
fn driver(source: &str, deps: &[(&str, &str)], ceiling: &str) -> Temp {
    let temp = Temp::new();
    let std = fs::canonicalize(STD).unwrap();
    let mut manifest = format!(
        "[package]\nname = \"@local/hash-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [{ceiling}]\n\n[dependencies]\n"
    );
    for (name, dir) in deps {
        manifest.push_str(&format!(
            "\"{name}\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
            std.join(dir).display().to_string().replace('\\', "/")
        ));
    }
    fs::write(temp.0.join("Loom.toml"), manifest).unwrap();
    fs::create_dir_all(temp.0.join("src")).unwrap();
    fs::write(temp.0.join("src/main.ves"), source).unwrap();
    temp
}

fn assemble(dir: &Path) -> packages::Assembled {
    let assembled = packages::load(dir).unwrap();
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    assembled
}

fn load(dir: &Path) -> check::Checked {
    let assembled = assemble(dir);
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

fn word(v: &Value) -> u64 {
    match v {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        other => panic!("not a word: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The Rust model.

/// The lengths of the official BLAKE3 test vectors (test_vectors.json).
const OFFICIAL: [u64; 22] = [
    0, 1, 1023, 1024, 1025, 2048, 2049, 3072, 3073, 4096, 4097, 5120, 5121, 6144, 6145, 7168, 7169,
    8192, 8193, 16384, 31744, 102400,
];
const SMALL: [u64; 7] = [0, 1, 63, 64, 65, 1024, 1025];
/// Split sizes for incremental updates of a 9,000-byte input.
const SPLITS: [u64; 10] = [1, 7, 63, 64, 65, 1000, 1023, 1024, 1025, 4096];
const KEY: &str = "whats the Elvish word for friend";
const CONTEXT: &str = "BLAKE3 2019-12-27 16:29:52 test vectors context";
const FNV0: u64 = 14695981039346656037;

fn input(n: u64) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn fold(acc: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(acc, |h, &b| (h ^ u64::from(b)).wrapping_mul(1099511628211))
}

fn xof(h: &blake3::Hasher) -> [u8; 131] {
    let mut out = [0u8; 131];
    h.finalize_xof().fill(&mut out);
    out
}

fn model_vectors(lens: &[u64]) -> u64 {
    let mut acc = FNV0;
    for &n in lens {
        let data = input(n);
        let mut h = blake3::Hasher::new();
        h.update(&data);
        acc = fold(acc, &xof(&h));
        let mut k = blake3::Hasher::new_keyed(KEY.as_bytes().try_into().unwrap());
        k.update(&data);
        acc = fold(acc, &xof(&k));
        let mut d = blake3::Hasher::new_derive_key(CONTEXT);
        d.update(&data);
        acc = fold(acc, &xof(&d));
    }
    acc
}

fn model_incremental(total: u64) -> u64 {
    let data = input(total);
    let mut acc = FNV0;
    for &s in &SPLITS {
        let mut h = blake3::Hasher::new();
        for piece in data.chunks(s as usize) {
            h.update(piece);
        }
        acc = fold(acc, h.finalize().as_bytes());
    }
    acc
}

/// XXH64 (xxHash specification 0.8), checked against the C implementation's
/// vectors below.
fn xxh64(data: &[u8], seed: u64) -> u64 {
    const P1: u64 = 0x9E3779B185EBCA87;
    const P2: u64 = 0xC2B2AE3D27D4EB4F;
    const P3: u64 = 0x165667B19E3779F9;
    const P4: u64 = 0x85EBCA77C2B2AE63;
    const P5: u64 = 0x27D4EB2F165667C5;
    let round = |acc: u64, x: u64| {
        acc.wrapping_add(x.wrapping_mul(P2))
            .rotate_left(31)
            .wrapping_mul(P1)
    };
    let merge = |acc: u64, v: u64| (acc ^ round(0, v)).wrapping_mul(P1).wrapping_add(P4);
    let u64_at = |i: usize| u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
    let n = data.len();
    let mut i = 0;
    let mut h = if n >= 32 {
        let mut v = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while i + 32 <= n {
            for (k, lane) in v.iter_mut().enumerate() {
                *lane = round(*lane, u64_at(i + 8 * k));
            }
            i += 32;
        }
        let mut h = v[0]
            .rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18));
        for lane in v {
            h = merge(h, lane);
        }
        h
    } else {
        seed.wrapping_add(P5)
    };
    h = h.wrapping_add(n as u64);
    while i + 8 <= n {
        h ^= round(0, u64_at(i));
        h = h.rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
        i += 8;
    }
    if i + 4 <= n {
        let w = u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
        h ^= u64::from(w).wrapping_mul(P1);
        h = h.rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
        i += 4;
    }
    for &b in &data[i..] {
        h ^= u64::from(b).wrapping_mul(P5);
        h = h.rotate_left(11).wrapping_mul(P1);
    }
    h ^= h >> 33;
    h = h.wrapping_mul(P2);
    h ^= h >> 29;
    h = h.wrapping_mul(P3);
    h ^ (h >> 32)
}

fn fnv1a32(data: &[u8]) -> u32 {
    data.iter().fold(2166136261u32, |h, &b| {
        (h ^ u32::from(b)).wrapping_mul(16777619)
    })
}

fn mix64(x: u64) -> u64 {
    let a = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    let b = (a ^ (a >> 27)).wrapping_mul(0x94d049bb133111eb);
    b ^ (b >> 31)
}

fn hash_pair(a: u64, b: u64) -> u64 {
    mix64(a.wrapping_mul(0x9E3779B97F4A7C15) ^ b.rotate_left(32))
}

fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1099511628211).wrapping_add(v)
}

fn model_fast(max: u64) -> u64 {
    let mut acc = FNV0;
    let lens = (0..=max).chain([1000, 4099]);
    for n in lens {
        let data = input(n);
        acc = mix(acc, fold(FNV0, &data));
        acc = mix(acc, u64::from(fnv1a32(&data)));
        acc = mix(acc, xxh64(&data, 0));
        acc = mix(acc, xxh64(&data, 0xae0543311b702d91));
        acc = mix(acc, mix64(n));
        acc = mix(acc, hash_pair(n, n.wrapping_mul(3)));
    }
    acc
}

#[test]
fn rust_references_match_official_vectors() {
    // BLAKE3 (the official test vectors and the reference paper).
    assert_eq!(
        blake3::hash(b"").to_hex().as_str(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
    assert_eq!(
        blake3::hash(b"abc").to_hex().as_str(),
        "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
    // XXH64 vectors of the C implementation.
    let hundred: Vec<u8> = (0..100u8).collect();
    assert_eq!(xxh64(b"", 0), 0xef46_db37_51d8_e999);
    assert_eq!(xxh64(&[42], 0), 0x0a9e_dece_beb0_3ae4);
    assert_eq!(xxh64(b"Hello, world!\0", 0), 0x7b06_c531_ea43_e89f);
    assert_eq!(xxh64(&hundred, 0), 0x6ac1_e580_3216_6597);
    assert_eq!(xxh64(b"", 0xae05_4331_1b70_2d91), 0x4b6a_04fc_df7a_4672);
    assert_eq!(
        xxh64(&hundred, 0xae05_4331_1b70_2d91),
        0x567e_355e_0682_e1f1
    );
    // FNV-1a test vectors (Fowler, Noll, Vo).
    assert_eq!(fold(FNV0, b"a"), 0xaf63dc4c8601ec8c);
    assert_eq!(fnv1a32(b"a"), 0xe40c292c);
}

// ---------------------------------------------------------------------------
// The portable driver.

fn lens_literal(lens: &[u64]) -> String {
    let mut s = String::from("{\n        let mut z: ChunkSeq[u64] = ChunkSeq::new[u64]();\n");
    for n in lens {
        s.push_str(&format!("        z.push({n});\n"));
    }
    s.push_str("        z\n    }");
    s
}

const PORTABLE: &str = "use @vesper-std/hash::blake3::{blake3, blake3_new, blake3_new_keyed, blake3_new_derive_key, digest_bytes, Digest};
use @vesper-std/hash::fast::{fnv1a64, fnv1a32, xxh64, mix64, hash_pair};

fn input(n: u64) -> ChunkSeq[u8] {
    let mut s: ChunkSeq[u8] = ChunkSeq::with_capacity[u8](n);
    let mut i: u64 = 0;
    loop {
        if i >= n {
            break;
        }
        s.push(wrapping_u8(i % 251));
        i = wrapping_add(i, 1);
    }
    s
}

fn fold(acc: u64, in s: Slice[u8]) -> u64 {
    let mut h = acc;
    let mut i: u64 = 0;
    loop {
        if i >= s.len() {
            break h;
        }
        let b = match s.try_get(i) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        i = wrapping_add(i, 1);
    }
}

fn mix(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 1099511628211), v)
}

fn fold_digest(acc: u64, d: Digest) -> u64 {
    let mut b: ChunkSeq[u8] = ChunkSeq::new[u8]();
    digest_bytes(d, inout b);
    fold(acc, in b.as_slice())
}

pub fn vectors() -> u64 {
    let lens: ChunkSeq[u64] = LENS;
    let mut acc: u64 = 14695981039346656037;
    let mut key: String = String::new();
    key.push_str(in \"whats the Elvish word for friend\");
    let mut k: u64 = 0;
    loop {
        if k >= lens.len() {
            break acc;
        }
        let n = match lens.try_get(k) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        let data = input(n);
        let mut h = blake3_new();
        h.update(in data.as_slice());
        let mut out: ChunkSeq[u8] = ChunkSeq::new[u8]();
        h.finalize_xof(131, inout out);
        acc = fold(acc, in out.as_slice());
        match blake3_new_keyed(in key.as_bytes()) {
            Option::Some(kh0) => {
                let mut kh = kh0;
                kh.update(in data.as_slice());
                let mut ko: ChunkSeq[u8] = ChunkSeq::new[u8]();
                kh.finalize_xof(131, inout ko);
                acc = fold(acc, in ko.as_slice());
            },
            Option::None => {
                acc = mix(acc, 1);
            },
        }
        let mut d = blake3_new_derive_key(in \"BLAKE3 2019-12-27 16:29:52 test vectors context\");
        d.update(in data.as_slice());
        let mut dout: ChunkSeq[u8] = ChunkSeq::new[u8]();
        d.finalize_xof(131, inout dout);
        acc = fold(acc, in dout.as_slice());
        k = wrapping_add(k, 1);
    }
}

pub fn incremental() -> u64 {
    let total: u64 = TOTAL;
    let data = input(total);
    let splits: ChunkSeq[u64] = SPLITS;
    let one_shot = blake3(in data.as_slice());
    let mut acc: u64 = 14695981039346656037;
    let mut k: u64 = 0;
    loop {
        if k >= splits.len() {
            break acc;
        }
        let s = match splits.try_get(k) {
            Option::Some(x) => x,
            Option::None => 1,
        };
        let mut h = blake3_new();
        let mut at: u64 = 0;
        loop {
            if at >= total {
                break;
            }
            let end0 = wrapping_add(at, s);
            let end = if end0 > total { total } else { end0 };
            h.update(in data[at..end]);
            at = end;
        }
        let d = h.finalize();
        if h.count() != total {
            acc = mix(acc, 7);
        }
        // Every split agrees with the one-shot digest.
        if d != one_shot {
            acc = mix(acc, 1);
        }
        acc = fold_digest(acc, d);
        k = wrapping_add(k, 1);
    }
}

pub fn fast() -> u64 {
    let max: u64 = MAX;
    let mut lens: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let mut n: u64 = 0;
    loop {
        if n > max {
            break;
        }
        lens.push(n);
        n = wrapping_add(n, 1);
    }
    lens.push(1000);
    lens.push(4099);
    let mut acc: u64 = 14695981039346656037;
    let mut k: u64 = 0;
    loop {
        if k >= lens.len() {
            break acc;
        }
        let n = match lens.try_get(k) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        let data = input(n);
        acc = mix(acc, fnv1a64(in data.as_slice()));
        acc = mix(acc, widen_u64(fnv1a32(in data.as_slice())));
        acc = mix(acc, xxh64(in data.as_slice(), 0));
        acc = mix(acc, xxh64(in data.as_slice(), 12539502615675809169));
        acc = mix(acc, mix64(n));
        acc = mix(acc, hash_pair(n, wrapping_mul(n, 3)));
        k = wrapping_add(k, 1);
    }
}

fn main() -> u64 {
    vectors() ^ incremental() ^ fast()
}
";

fn portable(lens: &[u64], total: u64, max: u64) -> String {
    PORTABLE
        .replace("LENS", &lens_literal(lens))
        .replace("SPLITS", &lens_literal(&SPLITS))
        .replace("TOTAL", &total.to_string())
        .replace("MAX", &max.to_string())
}

fn expected(lens: &[u64], total: u64, max: u64) -> [(&'static str, u64); 3] {
    [
        ("vectors", model_vectors(lens)),
        ("incremental", model_incremental(total)),
        ("fast", model_fast(max)),
    ]
}

const STD_HASH: [(&str, &str); 1] = [("@vesper-std/hash", "hash")];

/// BLAKE3, XXH64 and FNV agree with Rust natively, on the official inputs.
#[test]
fn portable_hashes_match_rust_natively() {
    let dir = driver(
        &portable(&OFFICIAL, 9000, 100),
        &STD_HASH,
        "\"Fail\", \"Alloc\"",
    );
    let c = load(&dir.0);
    let want = expected(&OFFICIAL, 9000, 100);
    for optimize in [true, false] {
        for (entry, value) in want {
            let got = native::jit_with_options(&c.world, entry, optimize)
                .unwrap_or_else(|e| panic!("JIT {entry} (optimize {optimize}) failed: {e}"))
                .to_string();
            assert_eq!(got, value.to_string(), "JIT {entry} (optimize {optimize})");
        }
    }
    if links_executables() {
        let all = want.iter().fold(0, |a, (_, v)| a ^ v);
        assert_eq!(
            executable_exit(&dir.0),
            Some((all & 0xff) as i32),
            "executable"
        );
    }
}

/// The evaluator, the reference semantics, agrees on short inputs.
#[test]
fn portable_hashes_match_rust_in_the_evaluator() {
    let dir = driver(
        &portable(&SMALL, 2100, 40),
        &STD_HASH,
        "\"Fail\", \"Alloc\"",
    );
    let c = load(&dir.0);
    for (entry, value) in expected(&SMALL, 2100, 40) {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = false;
        runtime.fuel = 4_000_000_000;
        let got = runtime.run(entry, vec![]).unwrap().to_string();
        assert_eq!(got, value.to_string(), "evaluator {entry}");
    }
}

/// The hash package checks on its own within its ceiling.
#[test]
fn hash_package_checks_on_its_own() {
    load(Path::new("packages/vesper-std/hash"));
}

/// The compression function is the output of its generator.
#[test]
fn compress_is_generated() {
    let script = Path::new("packages/vesper-std/hash/gen/blake3_gen.py");
    let Ok(out) = Command::new("python3")
        .arg(script)
        .arg(std::env::temp_dir().join(format!("blake3-compress-{}.ves", std::process::id())))
        .output()
    else {
        eprintln!("python3 unavailable; generator check skipped");
        return;
    };
    if !out.status.success() {
        eprintln!("python3 failed; generator check skipped");
        return;
    }
    let path = std::env::temp_dir().join(format!("blake3-compress-{}.ves", std::process::id()));
    let generated = fs::read_to_string(&path).unwrap();
    let _ = fs::remove_file(&path);
    let committed = fs::read_to_string("packages/vesper-std/hash/src/compress.ves").unwrap();
    assert_eq!(
        generated.replace("\r\n", "\n"),
        committed.replace("\r\n", "\n"),
        "regenerate src/compress.ves with gen/blake3_gen.py"
    );
}

// ---------------------------------------------------------------------------
// The `.vsys` driver: the same folds through @vesper-std/hash-sys.

const SYS: &str = "use @vesper-std/hash::blake3::{digest_bytes, Digest};
use @vesper-std/hash-sys::{hash, hasher_new, hasher_new_keyed, hasher_new_derive_key, update, finalize, finalize_xof, hasher_free};

fn input(n: u64) -> ChunkSeq[u8] {
    let mut s: ChunkSeq[u8] = ChunkSeq::with_capacity[u8](n);
    let mut i: u64 = 0;
    loop {
        if i >= n {
            break;
        }
        s.push(wrapping_u8(i % 251));
        i = wrapping_add(i, 1);
    }
    s
}

fn fold(acc: u64, in s: Slice[u8]) -> u64 {
    let mut h = acc;
    let mut i: u64 = 0;
    loop {
        if i >= s.len() {
            break h;
        }
        let b = match s.try_get(i) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        i = wrapping_add(i, 1);
    }
}

fn mix(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 1099511628211), v)
}

fn fold_digest(acc: u64, d: Digest) -> u64 {
    let mut b: ChunkSeq[u8] = ChunkSeq::new[u8]();
    digest_bytes(d, inout b);
    fold(acc, in b.as_slice())
}

fn xof_fold(acc: u64, sink h0: Option[Hasher], in data: Slice[u8]) -> u64 eff [Ffi[Blake3]] {
    match h0 {
        Option::Some(h) => {
            let mut x = h;
            update(inout x, in data);
            let mut out: ChunkSeq[u8] = ChunkSeq::new[u8]();
            finalize_xof(in x, 131, inout out);
            hasher_free(x);
            fold(acc, in out.as_slice())
        },
        Option::None => mix(acc, 1),
    }
}

pub fn vectors() -> u64 eff [Ffi[Blake3]] {
    let lens: ChunkSeq[u64] = LENS;
    let mut acc: u64 = 14695981039346656037;
    let mut key: String = String::new();
    key.push_str(in \"whats the Elvish word for friend\");
    let mut ctx: String = String::new();
    ctx.push_str(in \"BLAKE3 2019-12-27 16:29:52 test vectors context\");
    let mut k: u64 = 0;
    loop {
        if k >= lens.len() {
            break acc;
        }
        let n = match lens.try_get(k) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        let data = input(n);
        let plain = hasher_new();
        acc = xof_fold(acc, plain, in data.as_slice());
        let keyed = hasher_new_keyed(in key.as_bytes());
        acc = xof_fold(acc, keyed, in data.as_slice());
        let derived = hasher_new_derive_key(in ctx.as_bytes());
        acc = xof_fold(acc, derived, in data.as_slice());
        k = wrapping_add(k, 1);
    }
}

pub fn incremental() -> u64 eff [Ffi[Blake3]] {
    let total: u64 = TOTAL;
    let data = input(total);
    let splits: ChunkSeq[u64] = SPLITS;
    let one_shot = hash(in data.as_slice());
    let mut acc: u64 = 14695981039346656037;
    let mut k: u64 = 0;
    loop {
        if k >= splits.len() {
            break acc;
        }
        let s = match splits.try_get(k) {
            Option::Some(x) => x,
            Option::None => 1,
        };
        match hasher_new() {
            Option::Some(h0) => {
                let mut h = h0;
                let mut at: u64 = 0;
                loop {
                    if at >= total {
                        break;
                    }
                    let end0 = wrapping_add(at, s);
                    let end = if end0 > total { total } else { end0 };
                    update(inout h, in data[at..end]);
                    at = end;
                }
                let d = finalize(in h);
                hasher_free(h);
                if d != one_shot {
                    acc = mix(acc, 1);
                }
                acc = fold_digest(acc, d);
            },
            Option::None => {
                acc = mix(acc, 3);
            },
        }
        k = wrapping_add(k, 1);
    }
}

fn main() -> u64 eff [Ffi[Blake3]] {
    vectors() ^ incremental()
}
";

fn host_target() -> Option<(&'static str, &'static str)> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("x86_64-unknown-linux-gnu", "sysv64"))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(("aarch64-unknown-linux-gnu", "aapcs64"))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(("aarch64-apple-darwin", "darwin64"))
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(("x86_64-pc-windows-msvc", "win64"))
    } else {
        None
    }
}

/// The `.vsys` library on the `// | ` lines of hash-sys's lib.ves.
fn sys_vsys(target: (&str, &str)) -> String {
    let src = fs::read_to_string("packages/vesper-std/hash/sys/src/lib.ves").unwrap();
    src.lines()
        .filter_map(|l| l.strip_prefix("// | "))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("@TARGET@", target.0)
        .replace("@ABI@", target.1)
}

fn sys_checked(
    lens: &[u64],
    total: u64,
    target: (&str, &str),
) -> (Temp, ffi_lowering::ForeignChecked) {
    let source = SYS
        .replace("LENS", &lens_literal(lens))
        .replace("SPLITS", &lens_literal(&SPLITS))
        .replace("TOTAL", &total.to_string());
    let dir = driver(
        &source,
        &[
            ("@vesper-std/hash", "hash"),
            ("@vesper-std/hash-sys", "hash/sys"),
        ],
        "\"Fail\", \"Alloc\", \"Ffi\"",
    );
    let assembled = assemble(&dir.0);
    let mut imports = Imports::new();
    imports.add(&sys_vsys(target), Handler::Direct).unwrap();
    let checked =
        ffi_lowering::check_module(&assembled.module, &imports).unwrap_or_else(|e| panic!("{e}"));
    (dir, checked)
}

/// `.vsys` BLAKE3 in the JIT, bound to the `blake3` crate.
#[test]
fn sys_hashes_match_rust_in_the_jit() {
    let Some(target) = host_target() else {
        eprintln!("no .vsys target for this host; JIT evidence unavailable");
        return;
    };
    let (_dir, checked) = sys_checked(&OFFICIAL, 9000, target);
    let symbols = blake3_shim::symbols();
    for (entry, want) in [
        ("vectors", model_vectors(&OFFICIAL)),
        ("incremental", model_incremental(9000)),
    ] {
        let got = checked
            .jit(entry, &symbols)
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        assert_eq!(word(&got), want, "{entry}");
    }
}

/// The evaluator's stand-in host for `Blake3`: handles index `hashers`.
#[derive(Default)]
struct EvalHost {
    hashers: Vec<Option<blake3::Hasher>>,
}
impl EvalHost {
    fn call(&mut self, call: &mut ffi_lowering::EvalCall<'_, '_>) -> Result<Value, String> {
        use std::collections::BTreeMap;
        let args = call.arguments().to_vec();
        let option = |v: Option<Value>| match v {
            Some(v) => Value::Data {
                ty: "Option".into(),
                constructor: "Some".into(),
                fields: BTreeMap::from([("0".to_string(), v)]),
                resource_identity: None,
            },
            None => Value::Data {
                ty: "Option".into(),
                constructor: "None".into(),
                fields: BTreeMap::new(),
                resource_identity: None,
            },
        };
        let handle = |v: &Value| {
            ffi_lowering::handle_word(v)
                .map(|w| w as usize - 1)
                .ok_or("not a handle")
        };
        let mut make = |h: Option<blake3::Hasher>| {
            option(h.map(|h| {
                self.hashers.push(Some(h));
                ffi_lowering::handle_value("Hasher", self.hashers.len() as u64)
            }))
        };
        let unit = Value::Tuple(vec![]);
        Ok(match call.symbol.as_str() {
            "vesper_blake3_new" => make(Some(blake3::Hasher::new())),
            "vesper_blake3_new_keyed" => {
                let key = ffi_lowering::slice_bytes(&args[0]).ok_or("not a byte slice")?;
                make(
                    <[u8; 32]>::try_from(key.as_slice())
                        .ok()
                        .map(|k| blake3::Hasher::new_keyed(&k)),
                )
            }
            "vesper_blake3_new_derive_key" => {
                let ctx = ffi_lowering::slice_bytes(&args[0]).ok_or("not a byte slice")?;
                make(
                    String::from_utf8(ctx)
                        .ok()
                        .map(|c| blake3::Hasher::new_derive_key(&c)),
                )
            }
            "vesper_blake3_update" => {
                let data = ffi_lowering::slice_bytes(&args[1]).ok_or("not a byte slice")?;
                let i = handle(&args[0])?;
                self.hashers[i]
                    .as_mut()
                    .ok_or("use after free")?
                    .update(&data);
                unit
            }
            "vesper_blake3_finalize" => {
                let mut out = ffi_lowering::slice_bytes(&args[1]).ok_or("not a byte slice")?;
                let i = handle(&args[0])?;
                self.hashers[i]
                    .as_ref()
                    .ok_or("use after free")?
                    .finalize_xof()
                    .fill(&mut out);
                ffi_lowering::write_slice_bytes(&args[1], &out)?;
                unit
            }
            "vesper_blake3_free" => {
                let i = handle(&args[0])?;
                self.hashers[i].take().ok_or("double free")?;
                unit
            }
            "vesper_blake3_hash" => {
                let data = ffi_lowering::slice_bytes(&args[0]).ok_or("not a byte slice")?;
                let mut out = ffi_lowering::slice_bytes(&args[1]).ok_or("not a byte slice")?;
                let mut h = blake3::Hasher::new();
                h.update(&data);
                h.finalize_xof().fill(&mut out);
                ffi_lowering::write_slice_bytes(&args[1], &out)?;
                unit
            }
            other => return Err(format!("unknown symbol {other}")),
        })
    }
}

/// `.vsys` BLAKE3 in the evaluator: every hasher is freed exactly once.
#[test]
fn sys_hashes_match_rust_in_the_evaluator() {
    let (_dir, checked) = sys_checked(&SMALL, 2100, ("x86_64-unknown-linux-gnu", "sysv64"));
    for (entry, want) in [
        ("vectors", model_vectors(&SMALL)),
        ("incremental", model_incremental(2100)),
    ] {
        let mut host = EvalHost::default();
        let got = checked
            .evaluate(entry, |call| host.call(call))
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        assert_eq!(word(&got), want, "{entry}");
        assert!(!host.hashers.is_empty(), "{entry}");
        assert!(host.hashers.iter().all(Option::is_none), "{entry}: leaked");
    }
}
