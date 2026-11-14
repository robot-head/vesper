//! Copy-on-write shard checkpoints of a sharded arena (§31.1 item 2):
//! checkpoints share shards, the first write after one copies only that
//! shard, restores bring old contents back, and incremental deltas replay
//! onto the previous state and survive a snapshot-epoch round trip.

use vesper_chrono::arena::{
    ARENA_DELTA_MAGIC, ArenaDelta, ArenaError, ShardArena, WriteTracking, capture_arena_epoch,
    probe_write_tracking, rebuild_arena,
};
use vesper_chrono::{
    Bundle, BundleKey, BundleLimits, FlightRecorder, RecorderConfig, RedactionPolicy,
};

const SHARD: usize = 4096;

fn read(a: &ShardArena, offset: usize, len: usize) -> Vec<u8> {
    let mut out = vec![0; len];
    a.read(offset, &mut out).unwrap();
    out
}

#[test]
fn checkpoints_share_shards_and_writes_copy_one_shard() {
    let mut a = ShardArena::new(64 * SHARD + 100, SHARD).unwrap();
    assert_eq!(a.shard_count(), 65);
    a.write(10, b"hello").unwrap();
    a.write(SHARD * 3 - 2, b"span").unwrap(); // crosses shards 2 and 3
    assert_eq!(a.dirty(), &[0, 2, 3]);
    let c1 = a.checkpoint();
    assert_eq!(c1.epoch, 1);
    assert_eq!(c1.dirty, vec![0, 2, 3]);
    assert!(a.dirty().is_empty());
    // A checkpoint copies nothing: every shard is shared.
    assert_eq!(c1.shared_with(&a), 65);
    let copies = a.stats().shard_copies;

    a.write(20, b"world").unwrap();
    assert_eq!(a.stats().shard_copies, copies + 1, "one shard copied");
    a.write(30, b"again").unwrap();
    assert_eq!(a.stats().shard_copies, copies + 1, "no second copy");
    assert_eq!(c1.shared_with(&a), 64);
    assert_eq!(
        &c1.shard(0).unwrap()[20..25],
        &[0; 5],
        "checkpoint unchanged"
    );
    assert_eq!(read(&a, 20, 5), b"world");

    let c2 = a.checkpoint();
    assert_eq!(c2.dirty, vec![0]);
    a.write(64 * SHARD + 95, b"tail!").unwrap();
    assert!(a.write(64 * SHARD + 96, b"tail!").is_err());
    assert!(matches!(
        a.write(usize::MAX, b"x"),
        Err(ArenaError::OutOfBounds { .. })
    ));

    a.restore(&c1).unwrap();
    assert_eq!(read(&a, 10, 5), b"hello");
    assert_eq!(read(&a, 20, 5), [0; 5]);
    assert_eq!(read(&a, SHARD * 3 - 2, 4), b"span");
    assert_eq!(read(&a, 64 * SHARD + 95, 5), [0; 5]);
    assert_eq!(a.epoch(), 1);
    assert!(a.dirty().is_empty());
    a.restore(&c2).unwrap();
    assert_eq!(read(&a, 30, 5), b"again");
    let other = ShardArena::new(SHARD, SHARD).unwrap();
    let mut other = other;
    assert_eq!(other.restore(&c1), Err(ArenaError::ShapeMismatch));
}

#[test]
fn deltas_replay_epochs_onto_a_replica() {
    let mut live = ShardArena::new(16 * SHARD, SHARD).unwrap();
    let mut replica = ShardArena::new(16 * SHARD, SHARD).unwrap();
    let mut deltas = Vec::new();
    for epoch in 0..5u8 {
        for k in 0..3usize {
            let offset = ((epoch as usize * 5 + k * 7) % 16) * SHARD + k;
            live.write(offset, &[epoch + 1; 9]).unwrap();
        }
        let c = live.checkpoint();
        let d = c.delta();
        assert_eq!(d.shards.len(), c.dirty.len());
        let bytes = d.encode();
        assert_eq!(&bytes[..8], &ARENA_DELTA_MAGIC);
        // Incremental: only the dirty shards are in the image.
        assert!(bytes.len() <= 72 + d.shards.len() * (SHARD + 4));
        deltas.push(bytes);
    }
    for bytes in &deltas {
        let d = ArenaDelta::decode(bytes, 16).unwrap();
        d.apply(&mut replica).unwrap();
        replica.checkpoint();
    }
    assert_eq!(read(&replica, 0, 16 * SHARD), read(&live, 0, 16 * SHARD));
    // Out of order, wrong shape, damaged and truncated deltas are rejected.
    let mut fresh = ShardArena::new(16 * SHARD, SHARD).unwrap();
    assert_eq!(
        ArenaDelta::decode(&deltas[1], 16)
            .unwrap()
            .apply(&mut fresh),
        Err(ArenaError::EpochMismatch {
            expected: 1,
            found: 2
        })
    );
    let mut small = ShardArena::new(8 * SHARD, SHARD).unwrap();
    assert_eq!(
        ArenaDelta::decode(&deltas[0], 16)
            .unwrap()
            .apply(&mut small),
        Err(ArenaError::ShapeMismatch)
    );
    let d = &deltas[0];
    for i in (0..d.len()).step_by(97) {
        let mut bad = d.clone();
        bad[i] ^= 4;
        assert!(ArenaDelta::decode(&bad, 16).is_err(), "byte {i}");
    }
    for n in [0, 8, 40, d.len() - 1] {
        assert!(ArenaDelta::decode(&d[..n], 16).is_err());
    }
    assert!(matches!(
        ArenaDelta::decode(d, 1),
        Err(ArenaError::BadShard(_))
    ));
    // A forged delta naming a shard twice or past the end (valid digest).
    let forged = ArenaDelta {
        keyframe: false,
        epoch: 1,
        len: (16 * SHARD) as u64,
        shard_len: SHARD as u32,
        shards: vec![(3, vec![0; SHARD]), (3, vec![1; SHARD])],
    };
    assert_eq!(
        ArenaDelta::decode(&forged.encode(), 16),
        Err(ArenaError::BadShard(3))
    );
    let forged = ArenaDelta {
        shards: vec![(16, vec![0; SHARD])],
        ..forged
    };
    assert_eq!(
        ArenaDelta::decode(&forged.encode(), 16),
        Err(ArenaError::BadShard(16))
    );
}

#[test]
fn bad_geometry_is_rejected() {
    for (len, shard) in [(0, 4096), (4096, 100), (4096, 32), (4096, 2 << 20)] {
        assert!(matches!(
            ShardArena::new(len, shard),
            Err(ArenaError::BadGeometry { .. })
        ));
    }
}

#[test]
fn arena_deltas_are_snapshot_epochs() {
    let mut rec = FlightRecorder::new(RecorderConfig {
        snapshot_image_bytes: 12 * SHARD,
        ..RecorderConfig::small()
    })
    .unwrap();
    let mut a = ShardArena::new(8 * SHARD, SHARD).unwrap();
    a.write(5, b"epoch one").unwrap();
    let e1 = capture_arena_epoch(&mut rec, 10, &a.checkpoint(), 8).unwrap();
    a.write(3 * SHARD, b"epoch two").unwrap();
    let e2 = capture_arena_epoch(&mut rec, 20, &a.checkpoint(), 8).unwrap();
    let key = BundleKey::new([9; 32]);
    let bytes = Bundle::export(&rec, &RedactionPolicy::new(), b"", &key).unwrap();
    let bundle = Bundle::open(&bytes, &key, &BundleLimits::default()).unwrap();
    let mut replica = ShardArena::new(8 * SHARD, SHARD).unwrap();
    for id in [e1, e2] {
        let s = bundle.snapshots.iter().find(|s| s.epoch_id == id).unwrap();
        ArenaDelta::decode(&s.image, 8)
            .unwrap()
            .apply(&mut replica)
            .unwrap();
        replica.checkpoint();
    }
    assert_eq!(read(&replica, 0, 8 * SHARD), read(&a, 0, 8 * SHARD));
}

#[test]
fn kernel_write_tracking_is_probed_and_reported() {
    let probes = probe_write_tracking();
    assert!(probes.iter().all(|p| !p.used));
    assert!(
        probes
            .iter()
            .any(|p| p.mechanism == WriteTracking::MprotectFaults)
    );
    #[cfg(target_os = "linux")]
    {
        let sd = probes
            .iter()
            .find(|p| p.mechanism == WriteTracking::SoftDirty)
            .unwrap();
        assert!(sd.exercised);
        assert!(sd.host_permits.is_some());
        assert!(
            probes
                .iter()
                .any(|p| p.mechanism == WriteTracking::UserfaultfdWriteProtect)
        );
    }
    for p in &probes {
        println!(
            "{:?}: permits={:?} exercised={} {}",
            p.mechanism, p.host_permits, p.exercised, p.detail
        );
    }
}

#[test]
fn keyframes_keep_the_newest_state_rebuildable_after_slot_eviction() {
    const SLOTS: usize = 4;
    let mut rec = FlightRecorder::new(RecorderConfig {
        snapshot_slots: SLOTS,
        snapshot_image_bytes: 16 * SHARD + 4096,
        ..RecorderConfig::small()
    })
    .unwrap();
    let mut a = ShardArena::new(8 * SHARD, SHARD).unwrap();
    // Shard 7 is written only in epoch 1, whose image is evicted below.
    a.write(7 * SHARD, b"only in epoch one").unwrap();
    let mut ids = Vec::new();
    for epoch in 1..=11u64 {
        if epoch > 1 {
            a.write((epoch as usize % 7) * SHARD + 3, &epoch.to_le_bytes())
                .unwrap();
        }
        let c = a.checkpoint();
        assert_eq!(c.epoch, epoch);
        ids.push(capture_arena_epoch(&mut rec, epoch * 10, &c, SLOTS as u64).unwrap());
    }
    let key = BundleKey::new([3; 32]);
    let bytes = Bundle::export(&rec, &RedactionPolicy::new(), b"", &key).unwrap();
    let bundle = Bundle::open(&bytes, &key, &BundleLimits::default()).unwrap();
    let mut kept: Vec<_> = bundle.snapshots.iter().collect();
    kept.sort_by_key(|s| s.epoch_id);
    assert_eq!(kept.len(), SLOTS, "older images were evicted");
    let images: Vec<&[u8]> = kept.iter().map(|s| s.image.as_slice()).collect();
    let rebuilt = rebuild_arena(images.iter().copied(), 8).unwrap();
    assert_eq!(read(&rebuilt, 0, 8 * SHARD), read(&a, 0, 8 * SHARD));
    assert_eq!(read(&rebuilt, 7 * SHARD, 17), b"only in epoch one");
    // Deltas alone, without their keyframe, cannot be rebuilt.
    let deltas: Vec<&[u8]> = images
        .iter()
        .copied()
        .filter(|b| !ArenaDelta::decode(b, 8).unwrap().keyframe)
        .collect();
    assert_eq!(
        rebuild_arena(deltas.iter().copied(), 8).unwrap_err(),
        ArenaError::NoKeyframe
    );
    let c = a.checkpoint();
    assert_eq!(
        capture_arena_epoch(&mut rec, 999, &c, 0).unwrap_err(),
        ArenaError::NoKeyframe
    );
    // A keyframe applies onto an arena at any epoch.
    let mut other = ShardArena::new(8 * SHARD, SHARD).unwrap();
    other.write(0, b"stale").unwrap();
    other.checkpoint();
    other.checkpoint();
    c.keyframe().apply(&mut other).unwrap();
    assert_eq!(read(&other, 0, 8 * SHARD), read(&a, 0, 8 * SHARD));
}
