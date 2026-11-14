//! Kernel-tracked arena checkpoints: raw-pointer stores are caught by the
//! MMU (mprotect faults on Linux and macOS, write watches on Windows) and
//! end up in the next delta, which replays onto a `ShardArena` replica and
//! through the flight recorder.
#![cfg(any(target_os = "linux", target_os = "macos", windows))]
use vesper_chrono::arena::{ArenaDelta, ArenaError, ShardArena, rebuild_arena};
use vesper_chrono::ring::{FlightRecorder, RecorderConfig};
use vesper_chrono::tracked_arena::{TrackedArena, TrackedArenaError};
use vesper_fiber_abi::write_watch::page_size;

fn shard() -> usize {
    page_size().max(4096)
}

fn bytes(a: &TrackedArena) -> Vec<u8> {
    let mut out = vec![0; a.len()];
    a.read(0, &mut out).unwrap();
    out
}

fn shard_bytes(a: &ShardArena) -> Vec<u8> {
    let mut out = vec![0; a.len()];
    a.read(0, &mut out).unwrap();
    out
}

#[test]
fn raw_pointer_writes_reach_the_checkpoint_delta() {
    let s = shard();
    let mut arena = TrackedArena::new(6 * s, s).unwrap();
    let mut replica = ShardArena::new(6 * s, s).unwrap();
    let base = arena.as_mut_ptr();
    // SAFETY: in bounds of the live arena; nothing else borrows it.
    unsafe {
        base.add(2 * s + 5).write_volatile(0x42);
        base.add(4 * s).cast::<u32>().write_unaligned(0xdead_beef);
    }
    let d1 = arena.checkpoint().unwrap();
    assert_eq!(d1.epoch, 1);
    let written: Vec<u32> = d1.shards.iter().map(|(i, _)| *i).collect();
    assert_eq!(written, vec![2, 4], "the raw stores were tracked");
    assert_eq!(d1.shards[0].1[5], 0x42);
    ArenaDelta::decode(&d1.encode(), 64)
        .unwrap()
        .apply(&mut replica)
        .unwrap();
    replica.checkpoint();
    assert_eq!(shard_bytes(&replica), bytes(&arena));

    // An epoch with no writes is an empty delta; reads are not writes.
    let _ = bytes(&arena);
    let d2 = arena.checkpoint().unwrap();
    assert!(d2.shards.is_empty());
    d2.apply(&mut replica).unwrap();
    replica.checkpoint();

    // A mix of safe and raw writes, including one spanning two shards.
    arena.write(s - 2, &[7, 7, 7, 7]).unwrap();
    // SAFETY: as above.
    unsafe { arena.as_mut_ptr().add(5 * s + s - 1).write_volatile(9) };
    let d3 = arena.checkpoint().unwrap();
    let written: Vec<u32> = d3.shards.iter().map(|(i, _)| *i).collect();
    assert_eq!(written, vec![0, 1, 5]);
    d3.apply(&mut replica).unwrap();
    replica.checkpoint();
    assert_eq!(shard_bytes(&replica), bytes(&arena));
    if cfg!(unix) {
        assert!(arena.serviced_faults() >= 5);
    }
}

#[test]
fn snapshot_epochs_rebuild_and_restore() {
    let s = shard();
    let mut arena = TrackedArena::new(4 * s, s).unwrap();
    let mut recorder = FlightRecorder::new(RecorderConfig {
        snapshot_slots: 8,
        snapshot_image_bytes: 4 * (s + 4) + 128,
        ..RecorderConfig::small()
    })
    .unwrap();
    let mut images = vec![];
    for round in 0..6u8 {
        // SAFETY: in bounds of the live arena.
        unsafe {
            arena
                .as_mut_ptr()
                .add(usize::from(round % 4) * s + usize::from(round))
                .write_volatile(round + 1)
        };
        let (_, image) = arena.capture(&mut recorder, u64::from(round), 3).unwrap();
        assert_eq!(image.keyframe, round % 3 == 0, "round {round}");
        images.push(image.encode());
    }
    let rebuilt = rebuild_arena(images.iter().map(Vec::as_slice), 64).unwrap();
    assert_eq!(shard_bytes(&rebuilt), bytes(&arena));
    assert_eq!(rebuilt.epoch(), arena.epoch());

    // Restore to the epoch-4 keyframe: later raw writes are discarded and
    // the next delta follows it.
    let key = ArenaDelta::decode(&images[3], 64).unwrap();
    assert!(key.keyframe && key.epoch == 4);
    // SAFETY: as above.
    unsafe { arena.as_mut_ptr().add(3 * s + 1).write_volatile(0xff) };
    arena.restore(&key).unwrap();
    assert_eq!(arena.epoch(), 4);
    let mut replica = ShardArena::new(4 * s, s).unwrap();
    key.apply(&mut replica).unwrap();
    replica.checkpoint();
    assert_eq!(shard_bytes(&replica), bytes(&arena));
    arena.write(2 * s, &[5]).unwrap();
    let next = arena.checkpoint().unwrap();
    assert_eq!(next.epoch, 5);
    assert_eq!(next.shards.len(), 1);
    next.apply(&mut replica).unwrap();
    assert_eq!(shard_bytes(&replica), bytes(&arena));

    // A tracked arena replays deltas too.
    let mut copy = TrackedArena::new(4 * s, s).unwrap();
    copy.apply(&key).unwrap();
    copy.apply(&next).unwrap();
    assert_eq!(bytes(&copy), bytes(&arena));
    assert!(matches!(
        copy.apply(&next),
        Err(TrackedArenaError::Arena(ArenaError::EpochMismatch { .. }))
    ));
}

#[test]
fn bad_geometry_and_bounds_are_typed_errors() {
    let s = shard();
    assert!(matches!(
        TrackedArena::new(4 * s, s / 2),
        Err(TrackedArenaError::Arena(ArenaError::BadGeometry { .. }))
    ));
    assert!(TrackedArena::new(0, s).is_err());
    assert!(TrackedArena::new(s, 3 * s).is_err());
    let mut a = TrackedArena::new(s + 1, s).unwrap();
    assert_eq!(a.shard_count(), 2);
    assert!(matches!(
        a.write(s, &[1, 2]),
        Err(TrackedArenaError::Arena(ArenaError::OutOfBounds { .. }))
    ));
    let other = TrackedArena::new(2 * s, s).unwrap().keyframe().unwrap();
    assert!(matches!(
        a.apply(&other),
        Err(TrackedArenaError::Arena(ArenaError::ShapeMismatch))
    ));
    let delta = a.checkpoint().unwrap();
    assert!(matches!(
        a.restore(&delta),
        Err(TrackedArenaError::Arena(ArenaError::NoKeyframe))
    ));
}
