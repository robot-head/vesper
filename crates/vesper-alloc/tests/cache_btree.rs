//! Property tests of the Rust reference `CacheBTree` against `BTreeMap`, plus
//! the steady-state "0 Alloc" claim with a counting global allocator.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use vesper_alloc::{CacheBTree, CacheBTreeConfig, CacheBTreeError, Canonical, ReverseOrder};

struct Counting;
thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}
// SAFETY: forwards to System; only bumps a thread-local counter.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: caller contract forwarded.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: caller contract forwarded.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: caller contract forwarded.
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn random_ops_match_btreemap_through_four_levels() {
    let mut rng = Rng(0x5eed);
    let mut t = CacheBTree::<u32, u64>::new();
    let mut m = BTreeMap::new();
    let mut max_height = 0;
    for round in 0..200_000u32 {
        // Grow for the first half, then shrink back to empty.
        let grow = round < 100_000;
        let k = (rng.next() % 40_000) as u32;
        let r = rng.next() % 100;
        if (grow && r < 70) || (!grow && r < 25) {
            let v = rng.next();
            assert_eq!(t.insert(k, v).unwrap(), m.insert(k, v));
        } else {
            assert_eq!(t.remove(&k), m.remove(&k));
        }
        if round % 997 == 0 {
            t.check_invariants().unwrap();
            assert!(t.iter().eq(m.iter()));
        }
        max_height = max_height.max(t.stats().height);
    }
    for k in m.keys().copied().collect::<Vec<_>>() {
        assert_eq!(t.remove(&k), m.remove(&k));
    }
    t.check_invariants().unwrap();
    assert!(t.is_empty());
    assert!(max_height >= 4, "height peaked at {max_height}");
}

#[test]
fn ranges_and_bounds_match_btreemap() {
    let mut t = CacheBTree::<u64, u64>::new();
    let mut m = BTreeMap::new();
    for i in 0..5000u64 {
        let k = i.wrapping_mul(0x9e37_79b9) % 100_000;
        t.insert(k, i).unwrap();
        m.insert(k, i);
    }
    for (a, b) in [(0, 10), (500, 9000), (99_990, 200_000), (42, 42)] {
        assert!(t.range(a..b).eq(m.range(a..b)));
        assert!(t.range(a..=b).eq(m.range(a..=b)));
        assert!(t.range(a..).eq(m.range(a..)));
        assert!(t.range(..b).eq(m.range(..b)));
    }
    assert_eq!(t.first(), m.first_key_value());
    assert_eq!(t.last(), m.last_key_value());
    *t.get_mut(&m.keys().next().copied().unwrap()).unwrap() = 7;
    assert_eq!(t.first().unwrap().1, &7);
}

#[test]
fn reverse_order_brand_reverses_iteration() {
    let mut t = CacheBTree::<i32, (), ReverseOrder<Canonical>>::new();
    for k in [5, -1, 9, 0, 3] {
        t.insert(k, ()).unwrap();
    }
    let keys: Vec<i32> = t.iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, [9, 5, 3, 0, -1]);
    t.check_invariants().unwrap();
}

#[test]
fn limits_are_typed_errors_and_return_the_entry() {
    assert!(matches!(
        CacheBTree::<u8, u8>::with_config(CacheBTreeConfig { max_len: 0 }),
        Err(CacheBTreeError::InvalidConfig(_))
    ));
    let mut t = CacheBTree::<u8, String>::with_config(CacheBTreeConfig { max_len: 2 }).unwrap();
    t.insert(1, "a".into()).unwrap();
    t.insert(2, "b".into()).unwrap();
    // Replacing an existing key is not growth.
    assert_eq!(t.insert(2, "c".into()).unwrap().as_deref(), Some("b"));
    let e = t.insert(3, "d".into()).unwrap_err();
    assert_eq!(
        (e.key, e.value.as_str(), e.error),
        (3, "d", CacheBTreeError::Full { max_len: 2 })
    );
}

#[test]
fn trees_relocate_by_move_without_fixups() {
    let mut t = CacheBTree::<u64, u64>::new();
    for i in 0..1000 {
        t.insert(i, i * 2).unwrap();
    }
    let boxed = Box::new(t);
    let mut moved = *boxed;
    let copy = moved.clone();
    moved.insert(5000, 1).unwrap();
    assert_eq!(copy.len(), 1000);
    assert_eq!(moved.get(&999), Some(&1998));
    copy.check_invariants().unwrap();
}

#[test]
fn steady_state_churn_does_not_allocate() {
    let mut t = CacheBTree::<u64, u64>::new();
    t.reserve(4096).unwrap();
    let mut rng = Rng(7);
    let mut live: Vec<u64> = Vec::with_capacity(4096);
    for _ in 0..2048 {
        let k = rng.next();
        if t.insert(k, k).unwrap().is_none() {
            live.push(k);
        }
    }
    // Warm-up churn so the free lists hold every node shape the loop needs.
    let mut churn = |t: &mut CacheBTree<u64, u64>, live: &mut Vec<u64>, n: usize| {
        for _ in 0..n {
            let i = (rng.next() % live.len() as u64) as usize;
            let old = live[i];
            assert_eq!(t.remove(&old), Some(old));
            let k = rng.next();
            assert!(t.insert(k, k).unwrap().is_none());
            live[i] = k;
        }
    };
    churn(&mut t, &mut live, 20_000);
    let before = ALLOCS.with(Cell::get);
    churn(&mut t, &mut live, 20_000);
    let lookups: u64 = live.iter().map(|k| *t.get(k).unwrap() & 1).sum();
    let n = ALLOCS.with(Cell::get) - before;
    std::hint::black_box(lookups);
    assert_eq!(n, 0, "steady-state churn allocated {n} times");
    t.check_invariants().unwrap();
}
