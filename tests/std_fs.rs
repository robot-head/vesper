//! `@vesper-std/fs` (self-hosting SH0 lane 3, #1125): whole-file reads and
//! writes, atomic replace, directories, metadata and the sorted walk, run
//! against the real file system in the JIT (optimize on and off) and the
//! evaluator, each answer checked against `std::fs` and every host handle
//! finalized (tests/std_io_support).
mod std_io_support;

use std::fs;
use std::path::Path;
use std_io_support::{MODES, Temp, agree, check_driver, lit};

const DRIVER: &str = r#"use @vesper-std/fs::{
    IoError, Metadata, read, read_path, read_path_limit, read_string, write, append, create_new, replace, rename,
    remove_file, remove_dir, remove_dir_all, create_dir, create_dir_all, metadata,
    symlink_metadata, exists, kind_file, kind_dir, is_executable, canonicalize, write_stdout,
    write_stderr, create_dir_mode, set_permissions, copy
};
use @vesper-std/fs::walk::{
    Entries, walk, list_dir, entry_count, entry_start, entry_end, entry_kind, entry_join
};

fn mix(h: u64, b: u64) -> u64 {
    wrapping_mul(h ^ b, 1099511628211)
}

fn hash(in s: Slice[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for i in 0..s.len() {
        let b = match s.try_get(i) {
            Option::Some(b) => b,
            Option::None => 0,
        };
        h = mix(h, widen_u64(b));
    }
    mix(h, s.len())
}

// The error a failing body raised, as 1000 + kind; the body's value
// otherwise.
fn code(e: IoError) -> u64 {
    wrapping_add(1000, widen_u64(e.kind))
}

fn pattern(n: u64) -> ChunkSeq[u8] {
    let mut b: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for i in 0..n {
        b.push(wrapping_u8(wrapping_mul(i, 31)));
    }
    b
}

// Write a line to standard output and one to standard error: 0, or the
// failed write's OS error code.
pub fn stdio() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        write_stdout(in "@@vesper-stdout@@\n".as_bytes());
        write_stderr(in "@@vesper-stderr@@\n".as_bytes());
        0
    } with {
        return v => v,
        raise(e, _) => widen_u64(e.os),
    }
}

// The hash of each path's canonical form, or 1000 + kind when it fails.
fn canon(in p: Str) -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        let c = canonicalize(in p);
        hash(in c.as_slice())
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

pub fn canonical_paths() -> u64 eff [Ffi[StdFs]] {
    let a = canon(in "@DIR@/x/run");
    let b = canon(in "@DIR@/x/../x/plain");
    let c = canon(in "@DIR@/x");
    let d = canon(in "@DIR@/x/missing");
    let e = canon(in "@DIR@/x/link");
    let f = canon(in "@DIR@/x/dangling");
    mix(mix(mix(mix(mix(a, b), c), d), e), f)
}

// Bit i is set when the i-th path is executable.
pub fn executables() -> u64 eff [Ffi[StdFs]] {
    let mut bits: u64 = 0;
    if is_executable(in "@DIR@/x/run") {
        bits = bits | 1;
    }
    if is_executable(in "@DIR@/x/plain") {
        bits = bits | 2;
    }
    if is_executable(in "@DIR@/x") {
        bits = bits | 4;
    }
    if is_executable(in "@DIR@/x/missing") {
        bits = bits | 8;
    }
    if is_executable(in "@DIR@/x/link") {
        bits = bits | 16;
    }
    if is_executable(in "@DIR@/x/dangling") {
        bits = bits | 32;
    }
    bits
}

pub fn read_back() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        let data = pattern(3000);
        write(in "@DIR@/a.bin", in data.as_slice());
        let got = read(in "@DIR@/a.bin");
        let empty: ChunkSeq[u8] = ChunkSeq::new[u8]();
        write(in "@DIR@/empty.bin", in empty.as_slice());
        let none = read(in "@DIR@/empty.bin");
        mix(hash(in got.as_slice()), none.len())
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

// The first `limit` bytes for limits around the 4096-byte read steps and
// the file's own length (10000).
pub fn read_limited() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        let data = pattern(10000);
        write(in "@DIR@/l.bin", in data.as_slice());
        let mut h: u64 = 0;
        let limits: [u64; 9] = [0, 1, 4095, 4096, 4097, 9999, 10000, 10001, 20000];
        for i in 0..9 {
            let got = read_path_limit(in "@DIR@/l.bin".as_bytes(), limits[i]);
            h = mix(h, hash(in got.as_slice()));
        }
        h
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

pub fn text() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        let s = "héllo, wörld\n";
        write(in "@DIR@/t.txt", in s.as_bytes());
        append(in "@DIR@/t.txt", in s.as_bytes());
        let t = read_string(in "@DIR@/t.txt");
        let bad: ChunkSeq[u8] = pattern(300);
        write(in "@DIR@/bad.txt", in bad.as_slice());
        let rejected = handle Fail[IoError] in {
            let u = read_string(in "@DIR@/bad.txt");
            u.len()
        } with {
            return v => v,
            raise(e, _) => code(e),
        };
        mix(hash(in t.as_bytes()), rejected)
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

pub fn replace_atomic() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        create_dir(in "@DIR@/r");
        write(in "@DIR@/r/x.txt", in "old".as_bytes());
        let big = pattern(5000);
        replace(in "@DIR@/r/x.txt", in big.as_slice());
        replace(in "@DIR@/r/new.txt", in "fresh".as_bytes());
        // A name near the 255-byte limit: the temporary's name is its own,
        // not the target's plus a suffix.
        replace(in "@DIR@/r/@LONG@", in "long".as_bytes());
        // On Unix a backslash is a name byte, not a separator.
        replace(in "@DIR@/r/@ODD@", in "odd".as_bytes());
        let got = read(in "@DIR@/r/x.txt");
        let fresh = read(in "@DIR@/r/new.txt");
        // No temporary file is left behind.
        let left = list_dir(in "@DIR@/r");
        mix(mix(hash(in got.as_slice()), hash(in fresh.as_slice())), entry_count(in left))
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

pub fn errors() -> u64 eff [Ffi[StdFs]] {
    let missing = handle Fail[IoError] in {
        let b = read(in "@DIR@/missing.txt");
        b.len()
    } with {
        return v => v,
        raise(e, _) => code(e),
    };
    let twice = handle Fail[IoError] in {
        create_new(in "@DIR@/once.txt", in "1".as_bytes());
        create_new(in "@DIR@/once.txt", in "2".as_bytes());
        0
    } with {
        return v => v,
        raise(e, _) => code(e),
    };
    let full = handle Fail[IoError] in {
        create_dir_all(in "@DIR@/full/inner");
        remove_dir(in "@DIR@/full");
        0
    } with {
        return v => v,
        raise(e, _) => code(e),
    };
    let nul = handle Fail[IoError] in {
        let mut p: ChunkSeq[u8] = ChunkSeq::new[u8]();
        p.extend(in "@DIR@/a".as_bytes());
        p.push(0);
        p.push(98);
        let b = read_path(in p.as_slice());
        b.len()
    } with {
        return v => v,
        raise(e, _) => code(e),
    };
    // Reading a directory opens a file handle on Unix and fails at the
    // read: the handle is still closed exactly once.
    let dir = handle Fail[IoError] in {
        let b = read(in "@DIR@/full");
        b.len()
    } with {
        return v => v,
        raise(e, _) => if e.kind == 0 { 999 } else { 1000 },
    };
    let gone = handle Fail[IoError] in {
        remove_dir_all(in "@DIR@/full");
        if exists(in "@DIR@/full") { 1 } else { 2 }
    } with {
        return v => v,
        raise(e, _) => code(e),
    };
    mix(mix(mix(mix(mix(missing, twice), full), nul), dir), gone)
}

pub fn paths() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        create_dir_all(in "@DIR@/p/q/r");
        create_dir_all(in "@DIR@/p/q/r");
        write(in "@DIR@/p/q/r/f", in "abc".as_bytes());
        rename(in "@DIR@/p/q/r/f", in "@DIR@/p/g");
        let moved = exists(in "@DIR@/p/q/r/f");
        let there = exists(in "@DIR@/p/g");
        let m = metadata(in "@DIR@/p/g");
        let d = metadata(in "@DIR@/p/q");
        remove_file(in "@DIR@/p/g");
        let after = exists(in "@DIR@/p/g");
        let a: u64 = if moved { 1 } else { 0 };
        let b: u64 = if there { 1 } else { 0 };
        let c: u64 = if after { 1 } else { 0 };
        mix(mix(mix(mix(mix(a, b), c), m.size), widen_u64(m.kind)), widen_u64(d.kind))
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

// `remove_dir_all` on a tree with links in it (the test makes them): the
// tree goes, what the links point at stays; on a link it removes the link;
// on a file it is a `not_a_directory()` error.
pub fn remove_trees() -> u64 eff [Ffi[StdFs]] {
    let tree = handle Fail[IoError] in {
        remove_dir_all(in "@DIR@/t");
        let a: u64 = if exists(in "@DIR@/t") { 1 } else { 2 };
        let b: u64 = if exists(in "@DIR@/outside/keep.txt") { 3 } else { 4 };
        mix(a, b)
    } with {
        return v => v,
        raise(e, _) => code(e),
    };
    let link = handle Fail[IoError] in {
        remove_dir_all(in "@DIR@/dlink");
        let a: u64 = if exists(in "@DIR@/dlink") { 1 } else { 2 };
        let b: u64 = if exists(in "@DIR@/outside/keep.txt") { 3 } else { 4 };
        mix(a, b)
    } with {
        return v => v,
        raise(e, _) => code(e),
    };
    let file = handle Fail[IoError] in {
        remove_dir_all(in "@DIR@/outside/keep.txt");
        0
    } with {
        return v => v,
        raise(e, _) => code(e),
    };
    mix(mix(tree, link), file)
}

// The size, kind and modification second of a file the test made.
pub fn meta() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        let m = metadata(in "@DIR@/m.bin");
        let l = symlink_metadata(in "@DIR@/m.bin");
        mix(mix(mix(m.size, widen_u64(m.kind)), wrapping_u64(m.mtime_s)), widen_u64(m.mtime_ns ^ l.mtime_ns))
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

// A private directory (0o700), permission changes and a copy that keeps
// the source's bits; the source is made writable again at the end so the
// next run can rewrite it.
pub fn permissions() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        if exists(in "@DIR@/priv") {
            remove_dir(in "@DIR@/priv");
        }
        create_dir_mode(in "@DIR@/priv", 448);
        let d = metadata(in "@DIR@/priv");
        write(in "@DIR@/p.txt", in "permission bits".as_bytes());
        set_permissions(in "@DIR@/p.txt", 416);
        let a = metadata(in "@DIR@/p.txt");
        let n = copy(in "@DIR@/p.txt", in "@DIR@/q.txt");
        let b = metadata(in "@DIR@/q.txt");
        let q = read(in "@DIR@/q.txt");
        set_permissions(in "@DIR@/p.txt", 292);
        let c = metadata(in "@DIR@/p.txt");
        set_permissions(in "@DIR@/p.txt", 420);
        let mut h = mix(widen_u64(d.kind), widen_u64(d.mode));
        h = mix(mix(h, widen_u64(a.mode)), mix(n, widen_u64(b.mode)));
        mix(mix(h, hash(in q.as_slice())), widen_u64(c.mode))
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

// A private file larger than the copy buffer: the copy has its bits
// and all its bytes.
pub fn copy_private() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        let data = pattern(70000);
        write(in "@DIR@/s.bin", in data.as_slice());
        set_permissions(in "@DIR@/s.bin", 384);
        let n = copy(in "@DIR@/s.bin", in "@DIR@/t.bin");
        let m = metadata(in "@DIR@/t.bin");
        let t = read(in "@DIR@/t.bin");
        mix(mix(n, widen_u64(m.mode)), hash(in t.as_slice()))
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

// Copying a directory or a missing file.
pub fn copy_errors() -> u64 eff [Ffi[StdFs]] {
    let a = handle Fail[IoError] in {
        copy(in "@DIR@", in "@DIR@/x.txt")
    } with {
        return v => v,
        raise(e, _) => code(e),
    };
    let b = handle Fail[IoError] in {
        copy(in "@DIR@/no-such-file", in "@DIR@/x.txt")
    } with {
        return v => v,
        raise(e, _) => code(e),
    };
    mix(a, b)
}

fn entries_hash(in e: Entries) -> u64 {
    let mut h: u64 = 7;
    for i in 0..entry_count(in e) {
        let a = entry_start(in e, i);
        let b = entry_end(in e, i);
        h = mix(mix(h, hash(in e.bytes[a..b])), widen_u64(entry_kind(in e, i)));
    }
    mix(h, entry_count(in e))
}

pub fn walk_tree() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        let e = walk(in "@DIR@/tree");
        entries_hash(in e)
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

pub fn list_tree() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        let e = list_dir(in "@DIR@/tree");
        entries_hash(in e)
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

// Many handles: 200 files written and read back, each opened and closed.
pub fn churn() -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        create_dir(in "@DIR@/c");
        let mut total: u64 = 0;
        for i in 0..200 {
            let p = pattern(i);
            replace(in "@DIR@/c/one", in p.as_slice());
            let got = read(in "@DIR@/c/one");
            total = wrapping_add(total, hash(in got.as_slice()));
        }
        total
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

// The throughput benchmark: walk `roots` of the repository and read every
// `.ves` file under them; the bytes read plus the file count << 40.
fn is_ves(in b: Slice[u8], x: u64, y: u64) -> bool {
    if wrapping_sub(y, x) < 4 {
        return false;
    }
    let s = wrapping_sub(y, 4);
    let a = match b.try_get(s) {
        Option::Some(v) => v == 46,
        Option::None => false,
    };
    let c = match b.try_get(wrapping_add(s, 1)) {
        Option::Some(v) => v == 118,
        Option::None => false,
    };
    let d = match b.try_get(wrapping_add(s, 2)) {
        Option::Some(v) => v == 101,
        Option::None => false,
    };
    let e = match b.try_get(wrapping_add(s, 3)) {
        Option::Some(v) => v == 115,
        Option::None => false,
    };
    a && c && d && e
}

fn read_sources(in root: Str) -> u64 eff [Ffi[StdFs], Fail[IoError]] {
    let e = walk(in root);
    let mut total: u64 = 0;
    for i in 0..entry_count(in e) {
        let a = entry_start(in e, i);
        let b = entry_end(in e, i);
        if entry_kind(in e, i) == kind_file() && is_ves(in e.bytes.as_slice(), a, b) {
            let path = entry_join(in root.as_bytes(), in e, i);
            let text = read_path(in path.as_slice());
            total = wrapping_add(wrapping_add(total, text.len()), 1099511627776);
        }
    }
    total
}

fn bench(rounds: u64) -> u64 eff [Ffi[StdFs]] {
    handle Fail[IoError] in {
        let mut t: u64 = 0;
        for r in 0..rounds {
            t = wrapping_add(t, read_sources(in "@ROOT@/packages"));
            t = wrapping_add(t, read_sources(in "@ROOT@/crates"));
            t = wrapping_add(t, read_sources(in "@ROOT@/src"));
            t = wrapping_add(t, read_sources(in "@ROOT@/examples"));
            t = wrapping_add(t, read_sources(in "@ROOT@/tests"));
        }
        t
    } with {
        return v => v,
        raise(e, _) => code(e),
    }
}

pub fn bench_read() -> u64 eff [Ffi[StdFs]] {
    bench(5)
}

pub fn bench_none() -> u64 eff [Ffi[StdFs]] {
    bench(0)
}
"#;

fn mix(h: u64, b: u64) -> u64 {
    (h ^ b).wrapping_mul(1099511628211)
}

fn hash(s: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &b in s {
        h = mix(h, u64::from(b));
    }
    mix(h, s.len() as u64)
}

fn pattern(n: u64) -> Vec<u8> {
    (0..n).map(|i| i.wrapping_mul(31) as u8).collect()
}

fn code(e: &std::io::Error) -> u64 {
    1000 + std_io_support::oracle::IoKind::of(e) as u64
}

fn source(dir: &Temp) -> String {
    let root = lit(&fs::canonicalize(".").unwrap());
    DRIVER
        .replace("@DIR@", &dir.lit())
        .replace("@ROOT@", &root)
        .replace("@LONG@", &long_name())
        .replace("@ODD@", if cfg!(unix) { "a\\\\b.txt" } else { "odd.txt" })
}

/// A 240-byte file name for `replace_atomic`.
fn long_name() -> String {
    "n".repeat(240)
}

/// The file `replace_atomic` names `@ODD@`, as the OS lists it.
fn odd_name() -> &'static str {
    if cfg!(unix) { "a\\b.txt" } else { "odd.txt" }
}

/// Every `.ves` file under `dir` (not following links), read with `std::fs`:
/// bytes plus files << 40, as `read_sources` counts them.
fn rust_read_sources(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let t = e.file_type().unwrap();
            if t.is_dir() {
                stack.push(e.path());
            } else if t.is_file() && e.file_name().to_string_lossy().ends_with(".ves") {
                total = total
                    .wrapping_add(fs::read(e.path()).unwrap().len() as u64)
                    .wrapping_add(1 << 40);
            }
        }
    }
    total
}

/// Throughput of reading the repository's `.ves` sources, Vesper (JIT,
/// optimized) against `std::fs`, in ABBA order. Run by hand:
/// `cargo test --release --test std_fs -- --ignored --nocapture bench`.
#[test]
#[ignore = "benchmark"]
fn bench_reading_repository_sources() {
    let dir = Temp::new("std-fs");
    let checked = check_driver(&source(&dir));
    let roots = ["packages", "crates", "src", "examples", "tests"];
    let rust = || {
        let mut t = 0u64;
        for _ in 0..5 {
            for r in roots {
                t = t.wrapping_add(rust_read_sources(Path::new(r)));
            }
        }
        t
    };
    let symbols = std_io_support::symbols();
    let jit = |entry: &str| {
        let start = std::time::Instant::now();
        let v = std_io_support::word(&checked.jit(entry, &symbols).unwrap());
        (v, start.elapsed())
    };
    let want = rust();
    let files = (want >> 40) / 5;
    let bytes = (want & ((1 << 40) - 1)) / 5;
    eprintln!("{files} .ves files, {bytes} bytes per round, 5 rounds");
    let mut vesper = vec![];
    let mut native = vec![];
    for round in 0..4 {
        let order: &[bool] = if round % 2 == 0 {
            &[true, false]
        } else {
            &[false, true]
        };
        for &v in order {
            if v {
                let (got, t) = jit("bench_read");
                let (_, base) = jit("bench_none");
                assert_eq!(got, want);
                vesper.push(t.saturating_sub(base));
            } else {
                let start = std::time::Instant::now();
                assert_eq!(rust(), want);
                native.push(start.elapsed());
            }
        }
    }
    vesper.sort();
    native.sort();
    let mb = |d: std::time::Duration| (bytes * 5) as f64 / d.as_secs_f64() / 1e6;
    eprintln!(
        "median: Vesper {:?} ({:.0} MB/s), std::fs {:?} ({:.0} MB/s), ratio {:.2}",
        vesper[2],
        mb(vesper[2]),
        native[2],
        mb(native[2]),
        vesper[2].as_secs_f64() / native[2].as_secs_f64()
    );
}

/// Every path under `root`, relative, `/`-separated, sorted by bytes, with
/// its kind (not following links): what `walk` must return.
fn model_walk(root: &Path, rel: &str, out: &mut Vec<(Vec<u8>, u32)>) {
    for e in fs::read_dir(root.join(rel)).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().into_string().unwrap();
        let path = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let t = e.file_type().unwrap();
        let kind = if t.is_symlink() {
            3
        } else if t.is_dir() {
            2
        } else if t.is_file() {
            1
        } else {
            4
        };
        out.push((path.clone().into_bytes(), kind));
        if kind == 2 {
            model_walk(root, &path, out);
        }
    }
}

fn entries_hash(mut e: Vec<(Vec<u8>, u32)>) -> u64 {
    e.sort();
    let mut h = 7u64;
    for (p, k) in &e {
        h = mix(mix(h, hash(p)), u64::from(*k));
    }
    mix(h, e.len() as u64)
}

#[test]
fn whole_files_round_trip_in_every_mode() {
    let dir = Temp::new("std-fs");
    let checked = check_driver(&source(&dir));
    let want = mix(hash(&pattern(3000)), 0);
    assert_eq!(agree(&checked, "read_back", &MODES), want);
    assert_eq!(fs::read(dir.path().join("a.bin")).unwrap(), pattern(3000));
    let file = pattern(10000);
    let limited = [0, 1, 4095, 4096, 4097, 9999, 10000, 10001, 20000]
        .iter()
        .fold(0, |h, &n| mix(h, hash(&file[..n.min(file.len())])));
    assert_eq!(agree(&checked, "read_limited", &MODES), limited);

    let s = "héllo, wörld\n";
    let both = format!("{s}{s}");
    let invalid = 1000 + 7;
    assert_eq!(
        agree(&checked, "text", &MODES),
        mix(hash(both.as_bytes()), invalid)
    );
}

#[test]
fn replace_is_atomic_and_leaves_no_temporary() {
    let dir = Temp::new("std-fs");
    let checked = check_driver(&source(&dir));
    for mode in MODES {
        let _ = fs::remove_dir_all(dir.path().join("r"));
        let got = agree(&checked, "replace_atomic", &[mode]);
        assert_eq!(
            got,
            mix(mix(hash(&pattern(5000)), hash(b"fresh")), 4),
            "{mode:?}"
        );
        let mut names: Vec<_> = fs::read_dir(dir.path().join("r"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        let mut want = vec![
            long_name(),
            "new.txt".into(),
            odd_name().into(),
            "x.txt".into(),
        ];
        want.sort();
        assert_eq!(names, want, "{mode:?}");
    }
}

#[test]
fn failures_raise_typed_io_errors() {
    let dir = Temp::new("std-fs");
    let checked = check_driver(&source(&dir));
    let missing = code(&fs::read(dir.path().join("missing.txt")).unwrap_err());
    assert_eq!(missing, 1001);
    let want = mix(mix(mix(mix(mix(missing, 1003), 1006), 1007), 1000), 2);
    for mode in MODES {
        let _ = fs::remove_file(dir.path().join("once.txt"));
        assert_eq!(agree(&checked, "errors", &[mode]), want, "{mode:?}");
    }
}

#[cfg(unix)]
#[test]
fn remove_dir_all_does_not_follow_links() {
    use std::os::unix::fs::symlink;
    let dir = Temp::new("std-fs");
    let checked = check_driver(&source(&dir));
    let d = dir.path();
    let want = mix(mix(mix(2, 3), mix(2, 3)), 1004);
    for mode in MODES {
        fs::create_dir_all(d.join("outside")).unwrap();
        fs::write(d.join("outside/keep.txt"), "keep").unwrap();
        fs::create_dir_all(d.join("t/a/b")).unwrap();
        fs::write(d.join("t/a/f"), "f").unwrap();
        symlink(d.join("outside"), d.join("t/link")).unwrap();
        symlink(d.join("outside/keep.txt"), d.join("t/a/b/flink")).unwrap();
        symlink(d.join("outside"), d.join("dlink")).unwrap();
        assert_eq!(agree(&checked, "remove_trees", &[mode]), want, "{mode:?}");
        assert_eq!(
            fs::read_to_string(d.join("outside/keep.txt")).unwrap(),
            "keep"
        );
    }
}

#[test]
fn directories_rename_and_metadata() {
    let dir = Temp::new("std-fs");
    let checked = check_driver(&source(&dir));
    assert_eq!(
        agree(&checked, "paths", &MODES),
        mix(mix(mix(mix(mix(0, 1), 0), 3), 1), 2)
    );
    fs::write(dir.path().join("m.bin"), pattern(1234)).unwrap();
    let m = fs::metadata(dir.path().join("m.bin")).unwrap();
    let t = m
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    assert_eq!(
        agree(&checked, "meta", &MODES),
        mix(mix(mix(1234, 1), t.as_secs()), 0)
    );
}

#[test]
fn permissions_and_copies_match_std() {
    let dir = Temp::new("std-fs");
    let checked = check_driver(&source(&dir));
    let text = b"permission bits";
    let n = text.len() as u64;
    // On Windows the bits are the read-only attribute: 0o444 or 0o666.
    let (dir_mode, rw, ro) = if cfg!(unix) {
        (0o700, 0o640, 0o444)
    } else {
        (0o666, 0o666, 0o444)
    };
    let kind_dir = 2;
    let mut h = mix(kind_dir, dir_mode);
    h = mix(mix(h, rw), mix(n, rw));
    let want = mix(mix(h, hash(text)), ro);
    assert_eq!(agree(&checked, "permissions", &MODES), want);
    let private = if cfg!(unix) { 0o600 } else { 0o666 };
    assert_eq!(
        agree(&checked, "copy_private", &MODES),
        mix(mix(70000, private), hash(&pattern(70000)))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        // What std does for the same requests.
        let rust = dir.path().join("rust");
        fs::DirBuilder::new().mode(0o700).create(&rust).unwrap();
        let mode = |p: &Path| u64::from(fs::metadata(p).unwrap().permissions().mode() & 0o7777);
        assert_eq!(mode(&rust), dir_mode);
        let (p, q) = (dir.path().join("rp.txt"), dir.path().join("rq.txt"));
        fs::write(&p, text).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(fs::copy(&p, &q).unwrap(), n);
        assert_eq!(mode(&q), rw);
        let want = mix(
            code(&fs::copy(dir.path(), dir.path().join("x.txt")).unwrap_err()),
            code(&fs::copy(dir.path().join("no-such-file"), dir.path().join("x.txt")).unwrap_err()),
        );
        assert_eq!(agree(&checked, "copy_errors", &MODES), want);
    }
}

#[test]
fn walks_are_sorted_by_path_bytes() {
    let dir = Temp::new("std-fs");
    let tree = dir.path().join("tree");
    // Names chosen so that OS order, creation order and sorted order all
    // differ, with `.` (46) sorting before `/` (47) and a long name.
    let long = "n".repeat(200);
    for p in [
        "b/z.ves",
        "b/a.ves",
        "a/b/c/d.ves",
        "a.b",
        "a/b.x",
        "c",
        "B.txt",
        "a/b/c/e/f.ves",
        &format!("a/{long}"),
    ] {
        let path = tree.join(p);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, p).unwrap();
    }
    fs::create_dir_all(tree.join("empty")).unwrap();
    let checked = check_driver(&source(&dir));
    let mut model = vec![];
    model_walk(&tree, "", &mut model);
    assert_eq!(agree(&checked, "walk_tree", &MODES), entries_hash(model));
    let mut top = vec![];
    for e in fs::read_dir(&tree).unwrap() {
        let e = e.unwrap();
        let kind = if e.file_type().unwrap().is_dir() {
            2
        } else {
            1
        };
        top.push((e.file_name().into_string().unwrap().into_bytes(), kind));
    }
    assert_eq!(agree(&checked, "list_tree", &MODES), entries_hash(top));
}

#[test]
fn handles_do_not_leak_across_many_files() {
    let dir = Temp::new("std-fs");
    let checked = check_driver(&source(&dir));
    let want = (0..200u64).fold(0u64, |t, i| t.wrapping_add(hash(&pattern(i))));
    // The evaluator marshals every byte through source values; the JIT
    // covers the 200 opens and closes, both pipelines.
    for mode in [std_io_support::Mode::Jit, std_io_support::Mode::JitNoOpt] {
        let _ = fs::remove_dir_all(dir.path().join("c"));
        assert_eq!(agree(&checked, "churn", &[mode]), want, "{mode:?}");
    }
    let _ = lit;
}

/// `is_executable` is the check `vesper <tool>` makes before running a
/// tool: a regular file (following links) with an execute bit on Unix.
#[test]
fn is_executable_matches_std_permissions() {
    let dir = Temp::new("std-fs");
    let x = dir.path().join("x");
    fs::create_dir_all(&x).unwrap();
    fs::write(x.join("run"), b"#!/bin/sh\n").unwrap();
    fs::write(x.join("plain"), b"text").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(x.join("run"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(x.join("plain"), fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(x.join("run"), x.join("link")).unwrap();
        std::os::unix::fs::symlink(x.join("gone"), x.join("dangling")).unwrap();
    }
    let rust = |p: &Path| -> bool {
        let Ok(meta) = p.metadata() else {
            return false;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        meta.is_file()
    };
    let mut want = 0;
    for (i, name) in ["run", "plain", "", "missing", "link", "dangling"]
        .iter()
        .enumerate()
    {
        let p = if name.is_empty() {
            x.clone()
        } else {
            x.join(name)
        };
        if rust(&p) {
            want |= 1 << i;
        }
    }
    let checked = check_driver(&source(&dir));
    assert_eq!(agree(&checked, "executables", &MODES), want);
}

/// The child of `stdout_and_stderr_are_written_unbuffered`: run `stdio`
/// and exit with its answer.
#[test]
#[ignore = "run by stdout_and_stderr_are_written_unbuffered"]
fn stdio_child() {
    let Ok(mode) = std::env::var("VESPER_STD_STDIO_CHILD") else {
        return;
    };
    let dir = Temp::new("std-fs");
    let checked = check_driver(&source(&dir));
    let mode = match mode.as_str() {
        "jit" => std_io_support::Mode::Jit,
        "nojit" => std_io_support::Mode::JitNoOpt,
        _ => std_io_support::Mode::Evaluator,
    };
    // libtest has printed its own lines by now; point descriptor 1 at the
    // full device only for the Vesper write.
    #[cfg(target_os = "linux")]
    if std::env::var_os("VESPER_STD_STDIO_FULL").is_some() {
        use std::os::fd::AsRawFd;
        let full = fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        // SAFETY: both descriptors are open; fd 1 is replaced in place.
        assert!(unsafe { libc::dup2(full.as_raw_fd(), 1) } == 1);
    }
    let got = agree(&checked, "stdio", &[mode]);
    std::process::exit(got as i32);
}

/// `write_stdout` and `write_stderr` reach the process's own streams
/// (descriptors 1 and 2, or the standard handles), and a failed write
/// raises the OS error (on Linux, a full device: ENOSPC).
#[test]
fn stdout_and_stderr_are_written_unbuffered() {
    for mode in ["jit", "nojit", "eval"] {
        if mode != "eval" && !std_io_support::jit_host() {
            continue;
        }
        let child = || {
            let mut c = std::process::Command::new(std::env::current_exe().unwrap());
            c.args(["--ignored", "--exact", "stdio_child", "--test-threads=1"])
                .env("VESPER_STD_STDIO_CHILD", mode);
            c
        };
        let out = child().output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{mode}: {out:?}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stdout.contains("@@vesper-stdout@@\n"), "{mode}: {stdout}");
        assert!(!stdout.contains("@@vesper-stderr@@"), "{mode}: {stdout}");
        assert!(stderr.contains("@@vesper-stderr@@\n"), "{mode}: {stderr}");
        #[cfg(target_os = "linux")]
        {
            let status = child()
                .env("VESPER_STD_STDIO_FULL", "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(28), "{mode}");
        }
    }
}

/// `canonicalize` is `std::fs::canonicalize`: links, `.` and `..`
/// resolved, and a missing path (or a dangling link) an error.
#[test]
fn canonicalize_matches_std() {
    let dir = Temp::new("std-fs");
    let x = dir.path().join("x");
    fs::create_dir_all(&x).unwrap();
    fs::write(x.join("run"), b"run").unwrap();
    fs::write(x.join("plain"), b"text").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(x.join("run"), x.join("link")).unwrap();
        std::os::unix::fs::symlink(x.join("gone"), x.join("dangling")).unwrap();
    }
    let rust = |p: &Path| -> u64 {
        match fs::canonicalize(p) {
            Ok(c) => hash(&std_io_support::oracle::os_bytes(c.as_os_str())),
            Err(e) => code(&e),
        }
    };
    let names = [
        "x/run",
        "x/../x/plain",
        "x",
        "x/missing",
        "x/link",
        "x/dangling",
    ];
    let mut want = 0;
    for (i, n) in names.iter().enumerate() {
        let h = rust(&dir.path().join(n));
        want = if i == 0 { h } else { mix(want, h) };
    }
    let checked = check_driver(&source(&dir));
    assert_eq!(agree(&checked, "canonical_paths", &MODES), want);
}
