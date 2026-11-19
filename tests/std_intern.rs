//! `@vesper-std/intern` (self-hosting SH0 lane 5, #1127): string interning
//! against a Rust `HashMap<String, u32>` interner.
//!
//! A driver interns a stream of generated names (short, long, empty and
//! non-ASCII, with many repeats, so the index grows several times) and
//! folds the symbol ids, the interner's length, lookups of present and
//! absent names, `symbol_eq`, `symbol_len` and `resolve_into` round trips.
//! Ids must be dense and in first-insertion order, exactly as the Rust
//! model assigns them. The driver runs in the JIT with and without
//! `optimize`, as an executable and (on a shorter stream) in the evaluator.
use std::{
    collections::HashMap,
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
            "vesper-std-intern-{}-{}",
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
    let std = fs::canonicalize("packages/vesper-std/intern").unwrap();
    let manifest = format!(
        "[package]\nname = \"@local/intern-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Fail\", \"Alloc\"]\n\n[dependencies]\n\"@vesper-std/intern\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
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
// The Rust model.

fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1099511628211).wrapping_add(v)
}

fn lcg(x: u64) -> u64 {
    x.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

/// Name `k`: a prefix chosen by `k % 5` and the decimal digits of `k / 5`
/// (none for the empty name 0).
fn name(k: u64) -> String {
    let prefix = ["", "x", "identifier_long_", "é", "__"][(k % 5) as usize];
    if k == 0 {
        return String::new();
    }
    format!("{prefix}{}", k / 5)
}

/// Intern `count` names drawn from `distinct` with an LCG.
fn model(count: u64, distinct: u64) -> u64 {
    let mut ids: HashMap<String, u32> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    let mut acc = 14695981039346656037u64;
    let mut x = 7u64;
    for _ in 0..count {
        x = lcg(x);
        let s = name((x >> 33) % distinct);
        let id = *ids.entry(s.clone()).or_insert_with(|| {
            names.push(s.clone());
            (names.len() - 1) as u32
        });
        acc = mix(acc, u64::from(id));
    }
    acc = mix(acc, names.len() as u64);
    // Lookups: every name below 2 * distinct (half are absent).
    for k in 0..2 * distinct {
        let s = name(k);
        acc = mix(acc, ids.get(&s).map_or(u64::MAX, |&i| u64::from(i)));
    }
    // Round trips: resolve every symbol and compare.
    for (i, s) in names.iter().enumerate() {
        acc = mix(acc, s.len() as u64);
        acc = mix(acc, 1); // symbol_eq(i, resolved)
        acc = mix(acc, u64::from(ids[s] == i as u32));
    }
    // An unknown symbol resolves to nothing.
    mix(acc, 0)
}

// ---------------------------------------------------------------------------
// The Vesper driver.

const DRIVER: &str = "use @vesper-std/intern::{interner_new, symbol_from_id, Interner, Symbol};

fn mix(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 1099511628211), v)
}

fn lcg(x: u64) -> u64 {
    wrapping_add(wrapping_mul(x, 6364136223846793005), 1442695040888963407)
}

fn rem(x: u64, n: u64) -> u64 {
    if n == 0 { 0 } else { x % n }
}

fn div(x: u64, n: u64) -> u64 {
    if n == 0 { 0 } else { x / n }
}

fn bit(b: bool) -> u64 {
    if b { 1 } else { 0 }
}

fn push_decimal(inout s: String, x: u64) {
    let mut digits: ChunkSeq[u32] = ChunkSeq::new[u32]();
    let mut v = x;
    loop {
        digits.push(wrapping_add(wrapping_u32(rem(v, 10)), 48));
        v = div(v, 10);
        if v == 0 {
            break;
        }
    }
    loop {
        match digits.pop() {
            Option::Some(d) => {
                s.push_char(d);
            },
            Option::None => break,
        }
    }
}

fn name(k: u64) -> String {
    let mut s = String::new();
    if k != 0 {
        let p = rem(k, 5);
        if p == 1 {
            s.push_str(in \"x\");
        } else if p == 2 {
            s.push_str(in \"identifier_long_\");
        } else if p == 3 {
            s.push_str(in \"é\");
        } else if p == 4 {
            s.push_str(in \"__\");
        }
        push_decimal(inout s, div(k, 5));
    }
    s
}

pub fn run() -> u64 {
    let count: u64 = COUNT;
    let distinct: u64 = DISTINCT;
    let mut t = interner_new();
    let mut acc: u64 = 14695981039346656037;
    let mut x: u64 = 7;
    let mut i: u64 = 0;
    loop {
        if i >= count {
            break;
        }
        x = lcg(x);
        let s = name(rem(x >> 33, distinct));
        let sym = t.intern(in s);
        acc = mix(acc, widen_u64(sym.id()));
        i = wrapping_add(i, 1);
    }
    acc = mix(acc, t.len());
    let mut k: u64 = 0;
    loop {
        if k >= wrapping_mul(distinct, 2) {
            break;
        }
        let s = name(k);
        let v = match t.lookup(in s) {
            Option::Some(sym) => widen_u64(sym.id()),
            Option::None => 18446744073709551615,
        };
        acc = mix(acc, v);
        k = wrapping_add(k, 1);
    }
    let mut id: u64 = 0;
    loop {
        if id >= t.len() {
            break;
        }
        let sym = symbol_from_id(wrapping_u32(id));
        let mut back = String::new();
        let ok = t.resolve_into(sym, inout back);
        acc = mix(acc, t.symbol_len(sym));
        acc = mix(acc, bit(ok && t.symbol_eq(sym, in back) && back.len() == t.symbol_len(sym)));
        let again = match t.lookup(in back) {
            Option::Some(s2) => s2 == sym,
            Option::None => false,
        };
        acc = mix(acc, bit(again));
        id = wrapping_add(id, 1);
    }
    let mut none = String::new();
    let unknown = symbol_from_id(wrapping_u32(t.len()));
    acc = mix(acc, bit(t.resolve_into(unknown, inout none) || t.contains(unknown)));
    acc
}

fn main() -> u64 {
    run()
}
";

fn source(count: u64, distinct: u64) -> String {
    DRIVER
        .replace("COUNT", &count.to_string())
        .replace("DISTINCT", &distinct.to_string())
}

#[test]
fn interning_matches_a_rust_hashmap_natively() {
    let (count, distinct) = (60_000, 9_000);
    let dir = driver(&source(count, distinct));
    let c = load(&dir.0);
    let want = model(count, distinct);
    for optimize in [true, false] {
        let got = native::jit_with_options(&c.world, "run", optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(got, want.to_string(), "JIT (optimize {optimize})");
    }
    if links_executables() {
        assert_eq!(executable_exit(&dir.0), Some((want & 0xff) as i32), "executable");
    }
}

#[test]
fn interning_matches_a_rust_hashmap_in_the_evaluator() {
    let (count, distinct) = (600, 150);
    let dir = driver(&source(count, distinct));
    let c = load(&dir.0);
    let mut runtime = Runtime::new(&c.world);
    runtime.optimize = false;
    runtime.fuel = 4_000_000_000;
    let got = runtime.run("run", vec![]).unwrap().to_string();
    assert_eq!(got, model(count, distinct).to_string());
}

#[test]
fn intern_package_checks_on_its_own() {
    load(Path::new("packages/vesper-std/intern"));
}
