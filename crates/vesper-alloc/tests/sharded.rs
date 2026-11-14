//! ShardedSlab: per-thread shards, batched remote frees and the Alloc API.
use std::sync::mpsc;
use std::thread;
use vesper_alloc::{
    Alloc, REMOTE_FREE_BATCH, SLAB_BYTES, ShardBlock, ShardSlab, ShardedSlab, ShardedSlabConfig,
    SlabConfig, SlabError, dropped_shard_blocks,
};

fn heap(shards: usize) -> ShardedSlab {
    ShardedSlab::new(ShardedSlabConfig {
        shards,
        max_slabs_per_shard: 8,
        remote_batch: REMOTE_FREE_BATCH,
    })
    .unwrap()
}

#[test]
fn config_is_validated() {
    for bad in [
        ShardedSlabConfig {
            shards: 0,
            ..Default::default()
        },
        ShardedSlabConfig {
            shards: 257,
            ..Default::default()
        },
        ShardedSlabConfig {
            max_slabs_per_shard: 0,
            ..Default::default()
        },
        ShardedSlabConfig {
            remote_batch: 0,
            ..Default::default()
        },
    ] {
        assert!(matches!(
            ShardedSlab::new(bad),
            Err(SlabError::InvalidConfig(_))
        ));
    }
}

#[test]
fn local_free_reuses_the_slot_with_a_new_generation() {
    let h = heap(1);
    let mut s = h.claim(0).unwrap();
    let mut a = s.allocate(24, 8).unwrap();
    assert_eq!(&a[..], &[0; 24]);
    a.copy_from_slice(&[7; 24]);
    let (addr, generation) = (a.as_ptr(), a.generation());
    assert_eq!(a.capacity(), 24);
    s.free(a).unwrap();
    let b = s.allocate(20, 4).unwrap();
    assert_eq!(b.as_ptr(), addr, "same class reuses the freed slot");
    assert!(b.generation() > generation);
    assert_eq!(&b[..], &[0; 20], "reused bytes are zeroed");
    s.free(b).unwrap();
    let st = s.stats();
    assert_eq!((st.slabs, st.live_blocks, st.local_frees), (1, 0, 2));
    assert_eq!(st.reserved_bytes, SLAB_BYTES);
}

#[test]
fn unsupported_layouts_and_slab_limits_are_typed_errors() {
    let h = ShardedSlab::new(ShardedSlabConfig {
        shards: 1,
        max_slabs_per_shard: 1,
        remote_batch: 4,
    })
    .unwrap();
    let mut s = h.claim(0).unwrap();
    assert!(matches!(
        s.allocate(16 * 1024 + 1, 8),
        Err(SlabError::UnsupportedLayout { .. })
    ));
    assert!(matches!(
        s.allocate(8, 3),
        Err(SlabError::InvalidAlignment { align: 3 })
    ));
    let mut blocks = Vec::new();
    for _ in 0..4 {
        blocks.push(s.allocate(16 * 1024, 16).unwrap());
    }
    assert!(matches!(
        s.allocate(16 * 1024, 16),
        Err(SlabError::CapacityExceeded { max_slabs: 1 })
    ));
    assert!(matches!(
        s.allocate(8, 8),
        Err(SlabError::CapacityExceeded { .. })
    ));
    for b in blocks {
        s.free(b).unwrap();
    }
}

#[test]
fn a_shard_has_one_claimant_and_parks_on_drop() {
    let h = heap(2);
    let mut s = h.claim(1).unwrap();
    assert!(matches!(h.claim(1), Err(SlabError::Busy)));
    assert!(matches!(h.claim(2), Err(SlabError::InvalidConfig(_))));
    let b = s.allocate(64, 64).unwrap();
    drop(s);
    // The block outlives the claim; the next claimant can free it locally.
    let mut again = h.claim(1).unwrap();
    assert_eq!(again.stats().live_blocks, 1);
    again.free(b).unwrap();
    assert_eq!(again.stats().live_blocks, 0);
}

#[test]
fn remote_frees_are_batched_and_drained_by_the_origin() {
    let h = ShardedSlab::new(ShardedSlabConfig {
        shards: 2,
        max_slabs_per_shard: 8,
        remote_batch: 8,
    })
    .unwrap();
    let mut origin = h.claim(0).unwrap();
    let blocks: Vec<ShardBlock> = (0..20).map(|_| origin.allocate(32, 8).unwrap()).collect();
    assert!(blocks.iter().all(|b| b.shard() == 0));
    let (tx, rx) = mpsc::channel();
    let h2 = h.clone();
    let worker = thread::spawn(move || {
        let mut other = h2.claim(1).unwrap();
        let mut sent = Vec::new();
        for b in blocks {
            other.free(b).unwrap();
            sent.push(other.pending_outgoing());
        }
        tx.send(()).unwrap();
        // Dropping the claim flushes the final partial batch (20 = 8 + 8 + 4).
        sent
    });
    rx.recv().unwrap();
    let pending = worker.join().unwrap();
    assert_eq!(pending[6], 7);
    assert_eq!(pending[7], 0, "the eighth free flushes the batch");
    assert_eq!(pending[19], 4);
    assert_eq!(
        origin.stats().live_blocks,
        20,
        "frees are queued, not applied"
    );
    assert_eq!(origin.stats().queued_remote_frees, 20);
    assert_eq!(origin.drain_remote(), 20);
    let st = origin.stats();
    assert_eq!(
        (st.live_blocks, st.remote_frees, st.remote_batches),
        (0, 20, 1)
    );
    assert_eq!(origin.drain_remote(), 0);
}

#[test]
fn allocation_slow_path_drains_remote_frees_before_provisioning() {
    let h = ShardedSlab::new(ShardedSlabConfig {
        shards: 2,
        max_slabs_per_shard: 1,
        remote_batch: 1,
    })
    .unwrap();
    let mut origin = h.claim(0).unwrap();
    let all: Vec<_> = (0..4)
        .map(|_| origin.allocate(16 * 1024, 8).unwrap())
        .collect();
    let h2 = h.clone();
    thread::spawn(move || {
        let mut other = h2.claim(1).unwrap();
        for b in all {
            other.free(b).unwrap();
        }
    })
    .join()
    .unwrap();
    // The class is full and the shard may not provision another slab, so the
    // allocation succeeds only by draining the queue.
    let b = origin.allocate(16 * 1024, 8).unwrap();
    assert_eq!(origin.stats().remote_frees, 4);
    origin.free(b).unwrap();
}

#[test]
fn unbound_threads_free_into_the_remote_queue() {
    let h = heap(1);
    let mut s = h.claim(0).unwrap();
    let b = s.allocate(100, 4).unwrap();
    let h2 = h.clone();
    thread::spawn(move || h2.free_unbound(b).unwrap())
        .join()
        .unwrap();
    assert_eq!(s.drain_remote(), 1);
    assert_eq!(s.stats().live_blocks, 0);
}

#[test]
fn foreign_blocks_are_returned_unchanged() {
    let (h1, h2) = (heap(1), heap(1));
    let mut a = h1.claim(0).unwrap();
    let mut b = h2.claim(0).unwrap();
    let mut block = a.allocate(8, 8).unwrap();
    block[0] = 42;
    let err = b.free(block).unwrap_err();
    assert!(matches!(err.error, SlabError::ForeignHeap { .. }));
    let err = h2.free_unbound(err.owner).unwrap_err();
    assert_eq!(err.owner[0], 42);
    a.free(err.owner).unwrap();
}

#[test]
fn dropped_blocks_are_counted_and_keep_close_failing() {
    let h = heap(1);
    let before = dropped_shard_blocks();
    {
        let mut s = h.claim(0).unwrap();
        let leaked = s.allocate(8, 8).unwrap();
        drop(leaked);
    }
    assert!(dropped_shard_blocks() > before);
    let err = h.close().unwrap_err();
    assert_eq!(err.error, SlabError::LiveBlocks { count: 1 });
    // Closing a heap with only freed blocks succeeds.
    let clean = heap(2);
    {
        let mut s = clean.claim(0).unwrap();
        let b = s.allocate(8, 8).unwrap();
        s.free(b).unwrap();
    }
    let stats = clean.close().unwrap();
    assert_eq!(stats.live_blocks, 0);
}

#[test]
fn close_drains_queued_remote_frees() {
    let h = heap(2);
    let b = {
        let mut s = h.claim(0).unwrap();
        s.allocate(8, 8).unwrap()
    };
    {
        let mut other = h.claim(1).unwrap();
        other.free(b).unwrap();
    }
    assert_eq!(h.stats().unwrap().queued_remote_frees, 1);
    assert!(matches!(h.claim(0).map(|s| s.index()), Ok(0)));
    h.close().unwrap();
}

#[test]
fn blocks_outlive_the_heap_handle_and_stay_usable() {
    let h = heap(1);
    let mut s = h.claim(0).unwrap();
    let mut b = s.allocate(16, 8).unwrap();
    drop(h);
    b[3] = 9;
    assert_eq!(b[3], 9);
    s.free(b).unwrap();
}

#[test]
fn many_threads_free_each_others_blocks() {
    const T: usize = 4;
    const PER: usize = 300;
    let h = ShardedSlab::new(ShardedSlabConfig {
        shards: T,
        max_slabs_per_shard: 64,
        remote_batch: REMOTE_FREE_BATCH,
    })
    .unwrap();
    let mut handles = Vec::new();
    let (txs, rxs): (Vec<_>, Vec<_>) = (0..T).map(|_| mpsc::channel::<ShardBlock>()).unzip();
    let mut rxs: Vec<_> = rxs.into_iter().map(Some).collect();
    for i in 0..T {
        let h = h.clone();
        let rx = rxs[i].take().unwrap();
        let next = txs[(i + 1) % T].clone();
        handles.push(thread::spawn(move || {
            let mut s = h.claim(i).unwrap();
            for n in 0..PER {
                let mut b = s.allocate(8 + n % 200, 8).unwrap();
                b[0] = i as u8;
                next.send(b).unwrap();
            }
            drop(next);
            let mut freed = 0;
            while let Ok(b) = rx.recv() {
                assert_eq!(b[0] as usize, (i + T - 1) % T);
                s.free(b).unwrap();
                freed += 1;
            }
            freed
        }));
    }
    drop(txs);
    let freed: usize = handles.into_iter().map(|j| j.join().unwrap()).sum();
    assert_eq!(freed, T * PER);
    let stats = h.close().unwrap();
    assert_eq!(stats.remote_frees, (T * PER) as u64);
}

fn round_trip<A: Alloc>(a: &mut A) -> Result<(), SlabError>
where
    A::Block: std::ops::DerefMut<Target = [u8]>,
{
    let mut b = a.alloc_raw(40, 8)?;
    b[39] = 1;
    a.dealloc_raw(b).map_err(|e| e.error)
}

#[test]
fn alloc_capability_is_implemented_by_both_slabs() {
    let h = heap(1);
    let mut s = h.claim(0).unwrap();
    round_trip(&mut s).unwrap();
    let mut local = ShardSlab::new(SlabConfig::default()).unwrap();
    round_trip(&mut local).unwrap();
    assert_eq!(local.stats().unwrap().live_blocks, 0);
}

#[test]
fn freeing_every_block_of_a_multi_slab_class_does_not_overflow_the_free_stack() {
    let h = ShardedSlab::new(ShardedSlabConfig {
        shards: 1,
        max_slabs_per_shard: 3,
        remote_batch: 4,
    })
    .unwrap();
    let mut s = h.claim(0).unwrap();
    // 16 KiB blocks: four per slab, so twelve blocks span three slabs.
    let blocks: Vec<_> = (0..12).map(|_| s.allocate(16 * 1024, 8).unwrap()).collect();
    assert_eq!(s.stats().slabs, 3);
    for b in blocks {
        s.free(b).unwrap();
    }
    assert_eq!(s.stats().live_blocks, 0);
    let again: Vec<_> = (0..12).map(|_| s.allocate(16 * 1024, 8).unwrap()).collect();
    assert_eq!(s.stats().slabs, 3, "freed slots are reused");
    for b in again {
        s.free(b).unwrap();
    }
}
