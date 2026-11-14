//! "0 Alloc" claims, checked with a counting global allocator over the
//! warmed-up steady state. Counters are per thread, so other test threads do
//! not interfere.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::mpsc;
use std::thread;
use vesper_alloc::{
    Region, RegionConfig, ShardedSlab, ShardedSlabConfig, Tiled, TiledRecord, VirtualArena,
    VirtualArenaConfig,
};

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

fn count<R>(f: impl FnOnce() -> R) -> (R, u64) {
    let before = ALLOCS.with(Cell::get);
    let r = f();
    (r, ALLOCS.with(Cell::get) - before)
}

#[test]
fn same_shard_alloc_and_free_do_not_allocate_once_warm() {
    let h = ShardedSlab::new(ShardedSlabConfig::default()).unwrap();
    let mut s = h.claim(0).unwrap();
    // Provision the two size classes (40 and 48 bytes) the loop uses.
    for size in [40, 48] {
        let warm = s.allocate(size, 8).unwrap();
        s.free(warm).unwrap();
    }
    let ((), n) = count(|| {
        for i in 0..10_000 {
            let mut b = s.allocate(40 + i % 8, 8).unwrap();
            b[0] = 1;
            s.free(b).unwrap();
        }
    });
    assert_eq!(n, 0);
}

#[test]
fn remote_frees_and_their_drain_do_not_allocate_once_warm() {
    let h = ShardedSlab::new(ShardedSlabConfig {
        shards: 2,
        max_slabs_per_shard: 4,
        remote_batch: 64,
    })
    .unwrap();
    let mut origin = h.claim(0).unwrap();
    let (to_worker, from_origin) = mpsc::sync_channel::<Vec<vesper_alloc::ShardBlock>>(1);
    let (to_origin, from_worker) = mpsc::sync_channel::<(Vec<vesper_alloc::ShardBlock>, u64)>(1);
    let h2 = h.clone();
    let worker = thread::spawn(move || {
        let mut other = h2.claim(1).unwrap();
        while let Ok(mut batch) = from_origin.recv() {
            let ((), n) = count(|| {
                for b in batch.drain(..) {
                    other.free(b).unwrap();
                }
            });
            // Return the (now empty) vector so the origin can reuse it.
            to_origin.send((batch, n)).unwrap();
        }
    });
    let mut batch = Vec::with_capacity(64);
    let mut worker_allocs = Vec::new();
    let mut origin_allocs = Vec::new();
    for round in 0..20 {
        let ((), n) = count(|| {
            origin.drain_remote();
            for _ in 0..64 {
                batch.push(origin.allocate(256, 8).unwrap());
            }
        });
        if round >= 2 {
            origin_allocs.push(n);
        }
        to_worker.send(std::mem::take(&mut batch)).unwrap();
        let (back, n) = from_worker.recv().unwrap();
        batch = back;
        if round >= 2 {
            worker_allocs.push(n);
        }
    }
    drop(to_worker);
    worker.join().unwrap();
    assert!(origin_allocs.iter().all(|n| *n == 0), "{origin_allocs:?}");
    assert!(worker_allocs.iter().all(|n| *n == 0), "{worker_allocs:?}");
    assert_eq!(origin.drain_remote(), 64);
}

#[derive(Clone, Copy, Debug)]
struct Row(u64, u32);
impl TiledRecord for Row {
    type Hot = (u64, u32);
    type Cold = ();
    fn split(self) -> ((u64, u32), ()) {
        ((self.0, self.1), ())
    }
    fn join(h: (u64, u32), (): ()) -> Self {
        Row(h.0, h.1)
    }
}

#[test]
fn tiled_push_within_capacity_and_column_scans_do_not_allocate() {
    let mut t = Tiled::<Row, 16>::with_capacity(1024).unwrap();
    let (hits, n) = count(|| {
        for i in 0..1024u64 {
            t.push(Row(i, (i % 5) as u32)).unwrap();
        }
        t.column(|x| &x.1).flatten().filter(|c| **c == 0).count()
    });
    assert_eq!((hits, n), (205, 0));
}

#[test]
fn virtual_arena_cycles_do_not_allocate_once_warm() {
    let mut a = VirtualArena::new(VirtualArenaConfig::reserve(1 << 20)).unwrap();
    let cycle = |a: &mut VirtualArena| {
        let mut v = a.vec_with_capacity::<u32>(8).unwrap();
        for i in 0..4096 {
            a.vec_push(&mut v, i).unwrap();
        }
        let m = a.checkpoint().unwrap();
        a.alloc_bytes(100, 8).unwrap();
        a.rewind(m).unwrap();
        a.reset();
    };
    cycle(&mut a);
    let ((), n) = count(|| {
        for _ in 0..10 {
            cycle(&mut a);
        }
    });
    assert_eq!(n, 0);
}

#[test]
fn region_ref_access_does_not_allocate() {
    let mut r = Region::new(RegionConfig::bounded(4096)).unwrap();
    let b = r.alloc_value(5u64).unwrap();
    let rr = r.box_ref(&b).unwrap();
    let (v, n) = count(|| {
        for _ in 0..1000 {
            *r.ref_get_mut(rr).unwrap() += 1;
        }
        *r.ref_get(rr).unwrap()
    });
    assert_eq!((v, n), (1005, 0));
}
