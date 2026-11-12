//! Steady-state lookup, batch update, idle scan, insert/remove and compaction
//! perform zero heap allocations after warm-up (counting global allocator).
//! The counter is per thread so other test threads cannot perturb it.

mod common;

use common::{FixedHasher, Rng};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use vesper_warp_shard::rss::RssInput;
use vesper_warp_shard::{
    ConnEvent, ConnEventDesc, ConnState, ConnTable, ConnectionId, FiveTuple, HashFields,
    IndirectionTable, MAX_BATCH, MICROSOFT_RSS_KEY, ShardSet, SlotHandle, TableConfig,
};

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

// SAFETY: forwards every call to `System` unchanged; the counter is a
// const-initialised thread-local `Cell` that never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.with(|c| c.set(c.get() + 1));
        // SAFETY: caller upholds `GlobalAlloc::alloc` preconditions.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by `System` via `alloc` above.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        ALLOCS.with(|c| c.set(c.get() + 1));
        // SAFETY: caller upholds `GlobalAlloc::realloc` preconditions.
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocs() -> u64 {
    ALLOCS.with(|c| c.get())
}

#[test]
fn steady_state_paths_do_not_allocate() {
    const N: u32 = 4096;
    let mut t = ConnTable::with_hasher(TableConfig::new(N, 8), FixedHasher::default()).unwrap();
    let mut handles = vec![SlotHandle::from_raw(0, 0); N as usize];
    let mut batch = Vec::with_capacity(MAX_BATCH);
    let mut expired = vec![SlotHandle::from_raw(0, 0); 256];
    let shards = ShardSet::new(
        &MICROSOFT_RSS_KEY,
        IndirectionTable::round_robin(128, 4).unwrap(),
        HashFields::AddressesAndPorts,
        TableConfig::new(16, 8),
        |_| FixedHasher::default(),
    )
    .unwrap();
    let mut rng = Rng(77);

    let run = |t: &mut ConnTable<FixedHasher>,
               handles: &mut [SlotHandle],
               batch: &mut Vec<ConnEventDesc>,
               expired: &mut [SlotHandle],
               rng: &mut Rng,
               round: u64| {
        // Insert/remove churn plus lookups.
        for i in 0..N {
            let cid = ConnectionId::from_u64(round << 32 | u64::from(i));
            let st = ConnState::new(ConnectionId::from_u64(rng.next()), 14_720, rng.below(100));
            handles[i as usize] = t.insert(&cid, &st).unwrap();
        }
        for i in 0..N {
            let cid = (round << 32 | u64::from(i)).to_be_bytes();
            assert_eq!(t.lookup(&cid).unwrap(), Some(handles[i as usize]));
        }
        for i in (0..N).step_by(256) {
            let cid = (round << 32 | u64::from(i)).to_be_bytes();
            assert!(t.lookup_cid_simd(&cid).unwrap().is_some());
        }
        for _ in 0..200 {
            batch.clear();
            for _ in 0..MAX_BATCH {
                let slot = handles[rng.below(u64::from(N)) as usize];
                let event = match rng.below(3) {
                    0 => ConnEvent::PacketSent { bytes: 1200 },
                    1 => ConnEvent::PacketReceived {
                        pn: rng.below(1 << 40),
                    },
                    _ => ConnEvent::CwndUpdate { cwnd_bytes: 20_000 },
                };
                batch.push(ConnEventDesc { slot, event });
            }
            t.apply_batch(batch).unwrap();
            t.apply_batch_scalar(batch).unwrap();
        }
        let mut at = 0;
        loop {
            let r = t.scan_idle(50, at, expired);
            match r.resume_at {
                Some(next) => at = next,
                None => break,
            }
        }
        for h in handles.iter().step_by(3) {
            t.remove(*h).unwrap();
        }
        t.compact(|_| {});
        for h in handles.iter() {
            let _ = t.remove(*h);
        }
        let mut live = [SlotHandle::from_raw(0, 0); 64];
        while t.live_handles(&mut live) > 0 {
            for h in live {
                let _ = t.remove(h);
            }
        }
        assert!(t.is_empty());
    };

    // Warm-up round (also touches every page once).
    run(&mut t, &mut handles, &mut batch, &mut expired, &mut rng, 1);
    let before = allocs();
    for round in 2..6 {
        run(
            &mut t,
            &mut handles,
            &mut batch,
            &mut expired,
            &mut rng,
            round,
        );
    }
    let tuple = FiveTuple {
        src: "192.0.2.1".parse().unwrap(),
        dst: "198.51.100.7".parse().unwrap(),
        src_port: 5000,
        dst_port: 443,
        protocol: 17,
    };
    for _ in 0..1000 {
        shards.steer(&tuple).unwrap();
        RssInput::new(&tuple, HashFields::Addresses).unwrap();
    }
    assert_eq!(allocs() - before, 0, "steady state allocated");
}
