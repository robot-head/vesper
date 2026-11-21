//! Collections that own sequences (self-hosting SH2 lane 1, #1173, census
//! gap C6): `CacheBTree`, `FlatTable`, `IdxMap` and `Arena` hold values
//! that are not copy. `insert`/`put`/`alloc` move a value in (and a replaced
//! one out), `remove` moves the value out, and lookups are element views of
//! the collection's value sequence (`match t.vals.try_get(t.position(k))`,
//! `try_get_mut` to update). A driver whose values are structs that own a
//! `ChunkSeq` runs in the evaluator, the JIT (optimize on and off) and as an
//! executable, against a Rust model (`BTreeMap`/`HashMap` of
//! `(Vec<u32>, u64)`), and the JIT frees every sequence exactly once.
use std::{
    collections::BTreeMap,
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
            "vesper-owning-collections-{}-{}",
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
        "[package]\nname = \"@local/owning-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Fail\", \"Alloc\"]\n\n[dependencies]\n\"@vesper-std/collections\" = {{ version = \"^0.1\", path = \"{}\" }}\n\"@vesper-std/arena\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
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

const DRIVER: &str = "use @vesper-std/collections::btree::{CacheBTree, BTreeCursor, btree_new};
use @vesper-std/collections::flat_table::{FlatTable, flat_table_new};
use @vesper-std/arena::{Arena, Idx, IdxMap, arena_new, idx_from_raw, idx_map_owning};

// A dataflow state that owns its rows, as the census IR check's states do.
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

fn grow(inout s: State) {
    s.slots.push(7);
    s.n = wrapping_add(s.n, 1000);
}

fn mix(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 1000003), v)
}

fn scramble(i: u64, n: u64) -> u64 {
    if n == 0 { 0 } else { wrapping_mul(i, 7919) % n }
}

pub fn flat(n: u64) -> u64 {
    let mut t: FlatTable[u64, State] = flat_table_new[u64, State]();
    let mut acc: u64 = 0;
    for i in 0..n {
        let k = scramble(i, n);
        match t.insert(k, state(k)) {
            Option::Some(_) => { acc = mix(acc, 1); },
            Option::None => {},
        }
    }
    for k in 0..n {
        if k % 3 == 0 {
            match t.insert(k, state(wrapping_add(k, 1))) {
                Option::Some(old) => { acc = mix(acc, weight(in old)); },
                Option::None => { acc = mix(acc, 99); },
            }
        }
    }
    for k in 0..n {
        if k % 4 == 0 {
            match t.vals.try_get_mut(t.position(k)) {
                Option::Some(v) => grow(inout v),
                Option::None => { acc = mix(acc, 98); },
            }
        }
    }
    for k in 0..n {
        if k % 5 == 0 {
            match t.remove(k) {
                Option::Some(v) => { acc = mix(acc, weight(in v)); },
                Option::None => { acc = mix(acc, 97); },
            }
        }
    }
    match t.remove(n) {
        Option::Some(_) => { acc = mix(acc, 96); },
        Option::None => {},
    }
    for k in 0..n {
        match t.vals.try_get(t.position(k)) {
            Option::Some(v) => { acc = mix(acc, weight(in v)); },
            Option::None => { acc = mix(acc, 5); },
        }
    }
    mix(acc, t.len())
}

pub fn btree(n: u64) -> u64 {
    let mut t: CacheBTree[u64, State] = btree_new[u64, State]();
    let mut acc: u64 = 0;
    for i in 0..n {
        let k = scramble(i, n);
        match t.insert(k, state(k)) {
            Option::Some(_) => { acc = mix(acc, 1); },
            Option::None => {},
        }
    }
    for k in 0..n {
        if k % 3 == 0 {
            match t.insert(k, state(wrapping_add(k, 1))) {
                Option::Some(old) => { acc = mix(acc, weight(in old)); },
                Option::None => { acc = mix(acc, 99); },
            }
        }
    }
    for k in 0..n {
        if k % 4 == 0 {
            match t.vals.try_get_mut(t.position(k)) {
                Option::Some(v) => grow(inout v),
                Option::None => { acc = mix(acc, 98); },
            }
        }
    }
    for k in 0..n {
        if k % 5 == 0 {
            match t.remove(k) {
                Option::Some(v) => { acc = mix(acc, weight(in v)); },
                Option::None => { acc = mix(acc, 97); },
            }
        }
    }
    match t.remove(n) {
        Option::Some(_) => { acc = mix(acc, 96); },
        Option::None => {},
    }
    // Key order, through a cursor: each entry's value is a view.
    let mut c = t.cursor_first();
    loop {
        match t.entry_position(c) {
            Option::Some((k, id)) => {
                match t.vals.try_get(id) {
                    Option::Some(v) => { acc = mix(acc, mix(k, weight(in v))); },
                    Option::None => { acc = mix(acc, 3); },
                }
            },
            Option::None => { break; },
        }
        c = t.advance(c);
    }
    mix(acc, t.len())
}

pub fn side_table(n: u64) -> u64 {
    let mut m: IdxMap[State, State] = idx_map_owning[State, State](state(4));
    let mut acc: u64 = 0;
    for i in 0..n {
        match m.put(idx_from_raw[State](wrapping_u32(i)), state(i)) {
            Option::Some(_) => { acc = mix(acc, 95); },
            Option::None => {},
        }
    }
    // A gap cannot be filled: the value comes back.
    match m.put(idx_from_raw[State](wrapping_u32(wrapping_add(n, 5))), state(1)) {
        Option::Some(v) => { acc = mix(acc, weight(in v)); },
        Option::None => { acc = mix(acc, 94); },
    }
    match m.put(idx_from_raw[State](2), state(50)) {
        Option::Some(old) => { acc = mix(acc, weight(in old)); },
        Option::None => { acc = mix(acc, 93); },
    }
    match m.vals.try_get_mut(3) {
        Option::Some(v) => grow(inout v),
        Option::None => {},
    }
    for i in 0..wrapping_add(n, 2) {
        match m.vals.try_get(i) {
            Option::Some(v) => { acc = mix(acc, weight(in v)); },
            Option::None => { acc = mix(acc, weight(in m.dflt)); },
        }
    }
    let mut a: Arena[State] = arena_new[State]();
    for i in 0..n {
        let at = a.alloc(state(wrapping_mul(i, 2)));
        if i % 7 == 0 {
            match a.items.try_get_mut(at.index()) {
                Option::Some(v) => grow(inout v),
                Option::None => {},
            }
        }
    }
    a.truncate(wrapping_sub(n, 3));
    for i in 0..n {
        match a.items.try_get(i) {
            Option::Some(v) => { acc = mix(acc, weight(in v)); },
            Option::None => { acc = mix(acc, 6); },
        }
    }
    mix(mix(acc, m.len()), a.len())
}

pub fn small() -> u64 {
    wrapping_add(wrapping_add(flat(40), btree(40)), side_table(20))
}

pub fn large() -> u64 {
    wrapping_add(wrapping_add(flat(700), btree(700)), side_table(300))
}

fn main() -> u64 {
    large() % 251
}
";

// ---------------------------------------------------------------------------
// The Rust model.

type State = (Vec<u32>, u64);

fn state(n: u64) -> State {
    ((0..n % 5).map(|i| i as u32).collect(), n)
}

fn weight(s: &State) -> u64 {
    s.1.wrapping_mul(31).wrapping_add(s.0.len() as u64)
}

fn grow(s: &mut State) {
    s.0.push(7);
    s.1 = s.1.wrapping_add(1000);
}

fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1000003).wrapping_add(v)
}

/// `flat` and `btree` do the same operations; only the final read differs
/// (by key, or in key order with the key mixed in).
fn model_map(n: u64, ordered: bool) -> u64 {
    let mut t: BTreeMap<u64, State> = BTreeMap::new();
    let mut acc = 0;
    for i in 0..n {
        let k = i.wrapping_mul(7919) % n;
        if t.insert(k, state(k)).is_some() {
            acc = mix(acc, 1);
        }
    }
    for k in (0..n).filter(|k| k % 3 == 0) {
        acc = match t.insert(k, state(k + 1)) {
            Some(old) => mix(acc, weight(&old)),
            None => mix(acc, 99),
        };
    }
    for k in (0..n).filter(|k| k % 4 == 0) {
        match t.get_mut(&k) {
            Some(v) => grow(v),
            None => acc = mix(acc, 98),
        }
    }
    for k in (0..n).filter(|k| k % 5 == 0) {
        acc = match t.remove(&k) {
            Some(v) => mix(acc, weight(&v)),
            None => mix(acc, 97),
        };
    }
    if t.remove(&n).is_some() {
        acc = mix(acc, 96);
    }
    if ordered {
        for (k, v) in &t {
            acc = mix(acc, mix(*k, weight(v)));
        }
    } else {
        for k in 0..n {
            acc = match t.get(&k) {
                Some(v) => mix(acc, weight(v)),
                None => mix(acc, 5),
            };
        }
    }
    mix(acc, t.len() as u64)
}

fn model_side_table(n: u64) -> u64 {
    let dflt = state(4);
    let mut m: Vec<State> = (0..n).map(state).collect();
    let mut acc = 0;
    acc = mix(acc, weight(&state(1)));
    let old = std::mem::replace(&mut m[2], state(50));
    acc = mix(acc, weight(&old));
    grow(&mut m[3]);
    for i in 0..n + 2 {
        acc = mix(acc, weight(m.get(i as usize).unwrap_or(&dflt)));
    }
    let mut a: Vec<State> = vec![];
    for i in 0..n {
        let mut s = state(i * 2);
        if i % 7 == 0 {
            grow(&mut s);
        }
        a.push(s);
    }
    a.truncate((n - 3) as usize);
    for i in 0..n {
        acc = match a.get(i as usize) {
            Some(v) => mix(acc, weight(v)),
            None => mix(acc, 6),
        };
    }
    mix(mix(acc, m.len() as u64), a.len() as u64)
}

fn model(flat: u64, btree: u64, side: u64) -> u64 {
    model_map(flat, false)
        .wrapping_add(model_map(btree, true))
        .wrapping_add(model_side_table(side))
}

#[test]
fn owning_values_agree_with_the_model_and_do_not_leak() {
    let dir = driver(DRIVER);
    let c = load(&dir.0);
    let small = model(40, 40, 20);
    let large = model(700, 700, 300);
    let evaluated = Runtime::new(&c.world).run("small", vec![]).unwrap();
    assert_eq!(evaluated.to_string(), small.to_string(), "evaluator");
    for optimize in [true, false] {
        for (entry, want) in [("small", small), ("large", large)] {
            let stray = native_runtime::stray_frees();
            let got = native::jit_with_options(&c.world, entry, optimize)
                .unwrap_or_else(|e| panic!("{entry}: JIT (optimize {optimize}) failed: {e}"));
            assert_eq!(
                got.to_string(),
                want.to_string(),
                "{entry}: JIT (optimize {optimize})"
            );
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
        assert_eq!(executable_exit(&dir.0), Some((large % 251) as i32));
    }
}
