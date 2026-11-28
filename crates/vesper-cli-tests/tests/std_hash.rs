//! `@vesper-std/hash` and `@vesper-std/hash-sys` (self-hosting SH0 lane 5,
//! #1127): BLAKE3 in portable Vesper and through the BLAKE3 C library behind
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
//! `.vsys` driver runs as an executable that links hash-sys's
//! `[package] objects` (the official BLAKE3 C implementation in
//! `hash/sys/c`) and in the evaluator with the `blake3` crate standing in.
mod support;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

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
    let std = support::repo_root().join(STD);
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
const SMALL: [u64; 7] = [0, 1, 63, 64, 65, 1024, 1025];
/// Split sizes for incremental updates of a 9,000-byte input.
const SPLITS: [u64; 10] = [1, 7, 63, 64, 65, 1000, 1023, 1024, 1025, 4096];
const FNV0: u64 = 14695981039346656037;

fn fold(acc: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(acc, |h, &b| (h ^ u64::from(b)).wrapping_mul(1099511628211))
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

/// The C that hash-sys's `[package] objects` name for the host (the
/// wrappers, in manifest order), read from its Loom.toml.
fn host_c_objects() -> Vec<PathBuf> {
    let dir = support::repo_root().join(STD).join("hash/sys");
    let manifest = fs::read_to_string(dir.join("Loom.toml")).unwrap();
    let list = manifest.split("objects = [").nth(1).unwrap();
    let list = &list[..list.find(']').unwrap()];
    let arch = std::env::consts::ARCH;
    list.split('"')
        .skip(1)
        .step_by(2)
        .map(|e| dir.join(e.replace("@ARCH@", arch)))
        .filter(|f| f.is_file())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn c_bytes(bytes: &[u8]) -> String {
    let items: Vec<String> = bytes.iter().map(|b| format!("0x{b:02x}")).collect();
    format!(
        "{{{}}}",
        if items.is_empty() {
            "0".into()
        } else {
            items.join(", ")
        }
    )
}

/// The refusal rules of the `.vsys` library, against the C compiled from
/// hash-sys's `[package] objects`: a key that is not 32 bytes or a context
/// that is not UTF-8 gives NULL (`None`), and the one-shot derivation
/// returns 0 and leaves its output as it was. Good contexts (multi-byte
/// UTF-8, U+10FFFF, words of ASCII) match the `blake3` crate. Carried over
/// from the deleted vesper-blake3-sys crate's tests.
#[test]
fn sys_c_refuses_bad_keys_and_contexts() {
    if !links_executables() || host_target().is_none() {
        eprintln!("no C compiler or .vsys target; C refusal evidence unavailable");
        return;
    }
    const KEY: &[u8; 32] = b"whats the Elvish word for friend";
    let bad: [&[u8]; 9] = [
        &[0xff],
        &[0xc0, 0x80],             // overlong
        &[0xe0, 0x80, 0x80],       // overlong
        &[0xed, 0xa0, 0x80],       // surrogate
        &[0xf4, 0x90, 0x80, 0x80], // above U+10FFFF
        &[0xe2, 0x82],             // truncated
        &[b'a', 0x80],
        b"eight ok\xff",    // after a whole ASCII word
        b"seven o\xc0\x80", // overlong across a word
    ];
    let good = [
        "",
        "é",
        "€",
        "𐍈",
        "a\u{10ffff}",
        "eight ok",
        "sixteen bytes ok",
        "eight ok é",
    ];
    let mut c = String::from(
        "#include <stdint.h>\n#include <stdio.h>\n#include <stddef.h>\n\
         void *vesper_blake3_new_keyed(const uint8_t *, size_t);\n\
         void *vesper_blake3_new_derive_key(const uint8_t *, size_t);\n\
         void vesper_blake3_update(void *, const uint8_t *, size_t);\n\
         void vesper_blake3_finalize(const void *, uint8_t *, size_t);\n\
         void vesper_blake3_free(void *);\n\
         uint32_t vesper_blake3_derive_key_hash(const uint8_t *, size_t, const uint8_t *, size_t, uint8_t *, size_t);\n\
         static void put(const uint8_t *b, size_t n) { for (size_t i = 0; i < n; i++) printf(\"%02x\", b[i]); printf(\"\\n\"); }\n\
         static void keyed(const uint8_t *k, size_t n) {\n\
           void *h = vesper_blake3_new_keyed(k, n);\n\
           if (h == NULL) { printf(\"null\\n\"); return; }\n\
           uint8_t out[32]; vesper_blake3_update(h, (const uint8_t *)\"x\", 1); vesper_blake3_finalize(h, out, 32); vesper_blake3_free(h); put(out, 32);\n\
         }\n\
         static void derive(const uint8_t *c, size_t n) {\n\
           void *h = vesper_blake3_new_derive_key(c, n);\n\
           if (h == NULL) { printf(\"null\\n\"); } else {\n\
             uint8_t out[32]; vesper_blake3_update(h, (const uint8_t *)\"x\", 1); vesper_blake3_finalize(h, out, 32); vesper_blake3_free(h); put(out, 32);\n\
           }\n\
           uint8_t once[32]; for (int i = 0; i < 32; i++) once[i] = 7;\n\
           uint32_t ok = vesper_blake3_derive_key_hash(c, n, (const uint8_t *)\"x\", 1, once, 32);\n\
           printf(\"%u \", ok); put(once, 32);\n\
         }\n\
         int main(void) {\n",
    );
    let key = c_bytes(KEY);
    c.push_str(&format!(
        "  {{ static const uint8_t k[] = {key}; keyed(k, 31); keyed(k, 32); }}\n  {{ static const uint8_t k[33] = {key}; keyed(k, 33); }}\n"
    ));
    let mut want = vec!["null".to_string()];
    let crate_derive = |ctx: &str| {
        let mut h = blake3::Hasher::new_derive_key(ctx);
        h.update(b"x");
        hex(h.finalize().as_bytes())
    };
    let mut h = blake3::Hasher::new_keyed(KEY);
    h.update(b"x");
    want.push(hex(h.finalize().as_bytes()));
    want.push("null".into());
    for b in bad {
        assert!(std::str::from_utf8(b).is_err());
        c.push_str(&format!(
            "  {{ static const uint8_t c[] = {}; derive(c, {}); }}\n",
            c_bytes(b),
            b.len()
        ));
        want.push("null".into());
        // The sentinel: a refused derivation writes nothing.
        want.push(format!("0 {}", hex(&[7u8; 32])));
    }
    for g in good {
        c.push_str(&format!(
            "  {{ static const uint8_t c[] = {}; derive(c, {}); }}\n",
            c_bytes(g.as_bytes()),
            g.len()
        ));
        want.push(crate_derive(g));
        want.push(format!("1 {}", crate_derive(g)));
    }
    c.push_str("  return 0;\n}\n");
    let t = Temp::new();
    let main = t.0.join("refusals.c");
    fs::write(&main, c).unwrap();
    let exe = t.0.join("refusals");
    let built = Command::new("cc")
        .arg("-O2")
        .arg(&main)
        .args(host_c_objects())
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    let got: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(got, want);
}

/// The vendored C is the release the `blake3` dev-dependency pins, so the
/// crate is a valid reference for it.
#[test]
fn vendored_c_is_the_crate_version() {
    let header =
        fs::read_to_string(support::repo_root().join(STD).join("hash/sys/c/blake3.h")).unwrap();
    assert!(
        header.contains("#define BLAKE3_VERSION_STRING \"1.8.7\""),
        "the vendored C and the pinned blake3 dev-dependency must be the same release"
    );
}

/// The JIT does not load hash-sys's C objects; it says to build instead.
#[test]
fn sys_jit_points_to_vesper_build() {
    if host_target().is_none() {
        return;
    }
    let source = SYS
        .replace("LENS", &lens_literal(&SMALL))
        .replace("SPLITS", &lens_literal(&SPLITS))
        .replace("TOTAL", "2100");
    let dir = driver(
        &source,
        &[
            ("@vesper-std/hash", "hash"),
            ("@vesper-std/hash-sys", "hash/sys"),
        ],
        "\"Fail\", \"Alloc\", \"Ffi\"",
    );
    let out = support::vesper().arg("run").arg(&dir.0).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("objects of @vesper-std/hash-sys")
            && err.contains("only `vesper build` links"),
        "{err}"
    );
}
