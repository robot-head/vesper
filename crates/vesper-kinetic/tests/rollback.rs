//! Rollback ring: save, restore and re-simulate with bit-identical results;
//! linear-ticket discipline.

mod common;

use vesper_kinetic::{RollbackError, SnapshotRing, World, hash_world};

/// Deterministic "remote input": a kick to one body on some ticks.
fn input(tick: u64, world: &mut World) {
    if tick % 7 == 3 {
        let body = (tick as usize * 13) % 144;
        world
            .apply_velocity_change(body, [0.25, 1.5, -0.125])
            .expect("dynamic body");
    }
}

fn run(world: &mut World, ticks: u64, log: &mut Vec<[u8; 32]>) {
    for _ in 0..ticks {
        input(world.tick(), world);
        world.step().unwrap();
        log.push(hash_world(world.tick(), world.bodies()).sha256);
    }
}

#[test]
fn rewind_and_resimulate_reproduces_bit_identical_state() {
    let mut world = common::scene();
    let mut ring = SnapshotRing::new(&world, 8).unwrap();
    let mut first = Vec::new();
    run(&mut world, 30, &mut first);
    let ticket = ring.capture(&world).unwrap();
    assert_eq!(ticket.saved_tick(), 30);
    let mut tail = Vec::new();
    run(&mut world, 60, &mut tail);
    let final_hash = hash_world(world.tick(), world.bodies());

    // Rewind and replay the same 60 ticks of input.
    let mut replay = Vec::new();
    let end = ring
        .rewind_and_resimulate(ticket, &mut world, 60, |tick, w| {
            input(tick, w);
        })
        .unwrap();
    assert_eq!(end, 90);
    assert_eq!(hash_world(world.tick(), world.bodies()), final_hash);
    // And tick by tick from a second checkpoint.
    let ticket = ring.capture(&world).unwrap();
    let mut world_b = common::scene();
    let mut ring_b = SnapshotRing::new(&world_b, 2).unwrap();
    run(&mut world_b, 30, &mut replay);
    let t = ring_b.capture(&world_b).unwrap();
    replay.clear();
    run(&mut world_b, 60, &mut replay);
    assert_eq!(replay, tail, "independent run matches tick by tick");
    assert_eq!(ring_b.rewind(t, &mut world_b).unwrap(), 30);
    replay.clear();
    run(&mut world_b, 60, &mut replay);
    assert_eq!(replay, tail, "re-simulation matches tick by tick");
    assert_eq!(ring.confirm(ticket).unwrap(), 90);
    assert_eq!(ring.occupied(), 0);
    assert_eq!(ring.leaked_tickets(), 0);
}

#[test]
fn corrected_late_input_diverges_then_replays_exactly() {
    let mut world = common::scene();
    let mut ring = SnapshotRing::new(&world, 4).unwrap();
    let ticket = ring.capture(&world).unwrap();
    let mut predicted = Vec::new();
    run(&mut world, 20, &mut predicted);
    // A late input for tick 5 arrives: rewind and re-simulate with it.
    let corrected = |tick: u64, w: &mut World| {
        input(tick, w);
        if tick == 5 {
            w.apply_velocity_change(0, [0.0, 3.0, 0.0]).unwrap();
        }
    };
    ring.rewind_and_resimulate(ticket, &mut world, 20, corrected)
        .unwrap();
    let corrected_hash = hash_world(world.tick(), world.bodies()).sha256;
    assert_ne!(corrected_hash, *predicted.last().unwrap());
    let mut again = common::scene();
    for _ in 0..20 {
        corrected(again.tick(), &mut again);
        again.step().unwrap();
    }
    assert_eq!(
        hash_world(again.tick(), again.bodies()).sha256,
        corrected_hash
    );
}

#[test]
fn tickets_are_linear_and_refusals_return_them() {
    let world = common::scene();
    let mut ring = SnapshotRing::new(&world, 2).unwrap();
    let mut other = SnapshotRing::new(&world, 2).unwrap();
    let a = ring.capture(&world).unwrap();
    let b = ring.capture(&world).unwrap();
    assert_eq!(ring.capture(&world).unwrap_err(), RollbackError::RingFull);
    assert_ne!(a.ring_slot(), b.ring_slot());

    // A foreign ticket is refused and handed back intact.
    let (a, err) = other.confirm(a).unwrap_err();
    assert_eq!(err, RollbackError::ForeignTicket);
    let mut small = World::new(
        common::config(),
        &[common::dynamic([0.0, 1.0, 0.0], [0.0; 3])],
        &[vesper_kinetic::SphereCollider { radius: 0.25 }],
        &[],
    )
    .unwrap();
    let (a, err) = ring.rewind(a, &mut small).unwrap_err();
    assert!(matches!(err, RollbackError::WorldShape { .. }));
    assert_eq!(ring.confirm(a).unwrap(), 0);

    // Dropping a ticket is a counted leak and keeps its slot pinned.
    drop(b);
    assert_eq!(ring.leaked_tickets(), 1);
    assert_eq!(other.leaked_tickets(), 0);
    assert_eq!(ring.occupied(), 1);
    let c = ring.capture(&world).unwrap();
    assert_eq!(ring.capture(&world).unwrap_err(), RollbackError::RingFull);
    ring.confirm(c).unwrap();
    assert!(SnapshotRing::new(&world, 0).is_err());
}

#[test]
fn snapshot_capture_timing_is_reported() {
    // Not a benchmark claim: prints the measured copy time for this scene.
    let world = common::scene();
    let mut ring = SnapshotRing::new(&world, 1).unwrap();
    let start = std::time::Instant::now();
    for _ in 0..1000 {
        let t = ring.capture(&world).unwrap();
        ring.confirm(t).unwrap();
    }
    eprintln!(
        "{} bodies: {:?} per capture+confirm (debug={})",
        world.bodies().len(),
        start.elapsed() / 1000,
        cfg!(debug_assertions)
    );
}
