//! Collection view callbacks (§4.2 lent closures, self-hosting SH3 lane 3,
//! #1242): `with_ref`/`with_mut` on `FlatTable`, `CacheBTree` and `IdxMap`
//! lend a value that is not copy to a closure in place, and
//! `IdxMap::put_with` fills a gap with a constructor closure. The callback
//! form of each lookup returns what the hand-written element-view form
//! returns, in the evaluator, the JIT (optimize on and off) and an
//! executable, and the JIT frees every sequence exactly once.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, packages, runtime::Runtime, seq::native_runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-closure-modes-collections-{}-{}",
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
    let path = |p: &str| {
        fs::canonicalize(p)
            .unwrap()
            .display()
            .to_string()
            .replace('\\', "/")
    };
    let manifest = format!(
        "[package]\nname = \"@local/closure-modes-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Fail\", \"Alloc\"]\n\n[dependencies]\n\"@vesper-std/collections\" = {{ version = \"^0.1\", path = \"{}\" }}\n\"@vesper-std/arena\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
        path("packages/vesper-std/collections"),
        path("packages/vesper-std/arena"),
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

const DRIVER: &str = "use @vesper-std/collections::btree::{CacheBTree, btree_new};
use @vesper-std/collections::flat_table::{FlatTable, flat_table_new};
use @vesper-std/arena::{IdxMap, idx_from_raw, idx_map_owning};

struct State {
    slots: ChunkSeq[u32],
    n: u64
}

fn state(n: u64) -> State {
    let mut s = State { slots: ChunkSeq::new[u32](), n: n };
    let mut i: u64 = 0;
    loop {
        if i >= n % 5 {
            break;
        }
        s.slots.push(wrapping_u32(i));
        i = wrapping_add(i, 1);
    }
    s
}

fn weight(in s: State) -> u64 {
    wrapping_add(wrapping_mul(s.n, 31), s.slots.len())
}

fn grow(inout s: State, by: u64) {
    s.slots.push(7);
    s.n = wrapping_add(s.n, by);
}

fn mix(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 1000003), v)
}

fn scramble(i: u64, n: u64) -> u64 {
    if n == 0 { 0 } else { wrapping_mul(i, 7919) % n }
}

fn or5(x: Option[u64]) -> u64 {
    match x {
        Option::Some(v) => v,
        Option::None => 5,
    }
}

fn flat_table(n: u64) -> FlatTable[u64, State] {
    let mut t: FlatTable[u64, State] = flat_table_new[u64, State]();
    for i in 0..n {
        let k = scramble(i, n);
        t.insert(k, state(k));
    }
    t
}

fn btree(n: u64) -> CacheBTree[u64, State] {
    let mut t: CacheBTree[u64, State] = btree_new[u64, State]();
    for i in 0..n {
        let k = scramble(i, n);
        t.insert(k, state(k));
    }
    t
}

// Hand-written element views: the lookups before lent closures.
pub fn flat_views(in t: FlatTable[u64, State], in probe: State, n: u64) -> u64 {
    let mut acc: u64 = 0;
    for k in 0..wrapping_add(n, 3) {
        match t.vals.try_get(t.position(k)) {
            Option::Some(v) => { acc = mix(acc, wrapping_add(weight(in v), probe.n)); },
            Option::None => { acc = mix(acc, 5); },
        }
    }
    acc
}

pub fn flat_callbacks(in t: FlatTable[u64, State], in probe: State, n: u64) -> u64 {
    let mut acc: u64 = 0;
    for k in 0..wrapping_add(n, 3) {
        acc = mix(acc, or5(t.with_ref(k, |in v| wrapping_add(weight(in v), probe.n))));
    }
    acc
}

pub fn btree_views(in t: CacheBTree[u64, State], in probe: State, n: u64) -> u64 {
    let mut acc: u64 = 0;
    for k in 0..wrapping_add(n, 3) {
        match t.vals.try_get(t.position(k)) {
            Option::Some(v) => { acc = mix(acc, wrapping_add(weight(in v), probe.n)); },
            Option::None => { acc = mix(acc, 5); },
        }
    }
    acc
}

pub fn btree_callbacks(in t: CacheBTree[u64, State], in probe: State, n: u64) -> u64 {
    let mut acc: u64 = 0;
    for k in 0..wrapping_add(n, 3) {
        acc = mix(acc, or5(t.with_ref(k, |in v| wrapping_add(weight(in v), probe.n))));
    }
    acc
}

fn flat_updates(n: u64, callbacks: bool) -> u64 {
    let mut t = flat_table(n);
    let probe = state(3);
    let by: u64 = 1000;
    for k in 0..n {
        if k % 4 == 0 {
            if callbacks {
                t.with_mut(k, |inout v| grow(inout v, by));
            } else {
                match t.vals.try_get_mut(t.position(k)) {
                    Option::Some(v) => grow(inout v, by),
                    Option::None => {},
                }
            }
        }
    }
    if callbacks { flat_callbacks(in t, in probe, n) } else { flat_views(in t, in probe, n) }
}

fn btree_updates(n: u64, callbacks: bool) -> u64 {
    let mut t = btree(n);
    let probe = state(3);
    let by: u64 = 1000;
    for k in 0..n {
        if k % 4 == 0 {
            if callbacks {
                t.with_mut(k, |inout v| grow(inout v, by));
            } else {
                match t.vals.try_get_mut(t.position(k)) {
                    Option::Some(v) => grow(inout v, by),
                    Option::None => {},
                }
            }
        }
    }
    if callbacks { btree_callbacks(in t, in probe, n) } else { btree_views(in t, in probe, n) }
}

// `put_with` fills the gap below an index with a constructor closure; the
// written-out form pushes the same values first.
fn side_table(n: u64, callbacks: bool) -> u64 {
    let mut m: IdxMap[State, State] = idx_map_owning[State, State](state(4));
    let base: u64 = 300;
    let mut acc: u64 = 0;
    for i in 0..n {
        let at = idx_from_raw[State](wrapping_u32(wrapping_mul(i, 3)));
        if callbacks {
            match m.put_with(at, state(i), |j: u64| state(wrapping_add(j, base))) {
                Option::Some(_) => { acc = mix(acc, 95); },
                Option::None => {},
            }
        } else {
            loop {
                let len = m.len();
                if len >= wrapping_mul(i, 3) {
                    break;
                }
                m.vals.push(state(wrapping_add(len, base)));
            }
            match m.put(at, state(i)) {
                Option::Some(_) => { acc = mix(acc, 95); },
                Option::None => {},
            }
        }
    }
    let probe = state(2);
    for i in 0..wrapping_add(m.len(), 2) {
        let at = idx_from_raw[State](wrapping_u32(i));
        if callbacks {
            m.with_mut(at, |inout v| grow(inout v, probe.n));
            acc = mix(acc, m.with_ref(at, |in v| wrapping_add(weight(in v), probe.slots.len())));
        } else {
            match m.vals.try_get_mut(i) {
                Option::Some(v) => grow(inout v, probe.n),
                Option::None => {},
            }
            match m.vals.try_get(i) {
                Option::Some(v) => { acc = mix(acc, wrapping_add(weight(in v), probe.slots.len())); },
                Option::None => { acc = mix(acc, wrapping_add(weight(in m.dflt), probe.slots.len())); },
            }
        }
    }
    mix(acc, m.len())
}

fn all(n: u64, callbacks: bool) -> u64 {
    mix(mix(flat_updates(n, callbacks), btree_updates(n, callbacks)), side_table(n, callbacks))
}

pub fn views() -> u64 {
    all(60, false)
}

pub fn callbacks() -> u64 {
    all(60, true)
}

fn main() -> u64 {
    if views() == callbacks() { 42 } else { 1 }
}
";

#[test]
fn callbacks_agree_with_element_views_and_do_not_leak() {
    let dir = driver(DRIVER);
    let c = load(&dir.0);
    let want = Runtime::new(&c.world)
        .run("views", vec![])
        .unwrap()
        .to_string();
    let got = Runtime::new(&c.world)
        .run("callbacks", vec![])
        .unwrap()
        .to_string();
    assert_eq!(got, want, "evaluator");
    for optimize in [true, false] {
        for entry in ["views", "callbacks"] {
            let stray = native_runtime::stray_frees();
            let got = native::jit_with_options(&c.world, entry, optimize)
                .unwrap_or_else(|e| panic!("{entry}: JIT (optimize {optimize}) failed: {e}"));
            assert_eq!(got.to_string(), want, "{entry}: JIT (optimize {optimize})");
            assert_eq!(
                native_runtime::teardown_frees(),
                0,
                "{entry}: JIT (optimize {optimize}) left sequences for teardown"
            );
            assert_eq!(
                native_runtime::stray_frees(),
                stray,
                "{entry}: JIT (optimize {optimize}) freed a sequence that was not live"
            );
        }
    }
    if links_executables() {
        assert_eq!(executable_exit(&dir.0), Some(42));
    }
}
