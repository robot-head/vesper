//! Software NIC: lifecycle, descriptor validation, backpressure, leak
//! detection and long randomized runs audited after every step.

mod common;

use common::{Echo, Rng, random_fault, random_packet};
use vesper_warp_umem::{
    ConservationError, DeliverOutcome, DescError, DriverLoc, Fault, LeaseMark, Location,
    RetireOutcome, SoftNic, SoftNicMode, SoftXsk, UmemAuditor, UmemConfig, UmemError, XdpNic,
};

fn config(frames: u32) -> UmemConfig {
    UmemConfig {
        frame_count: frames,
        frame_size: 2048,
        headroom: 256,
        fill_size: frames.next_power_of_two(),
        rx_size: 16,
        tx_size: 8,
        completion_size: 8,
    }
}

fn xsk(frames: u32, mode: SoftNicMode) -> SoftXsk {
    let mut xsk = SoftNic::xsk(config(frames), mode).unwrap();
    // Three quarters to the fill ring, the rest free for transmit.
    xsk.replenish_fill(frames - frames / 4);
    xsk
}

fn audit(xsk: &mut SoftXsk) -> vesper_warp_umem::XdpUmemAccounting {
    UmemAuditor::new().audit(xsk).expect("conservation")
}

#[test]
fn scripted_lifecycle_harvest_turn_around_submit_complete_retire() {
    let mut xsk = xsk(8, SoftNicMode::Scripted);
    let acc = audit(&mut xsk);
    assert_eq!(
        (acc.get(Location::FillRing), acc.get(Location::Free)),
        (6, 2)
    );
    assert!(matches!(
        xsk.backend_mut().deliver(b"hello warp"),
        DeliverOutcome::Delivered { .. }
    ));
    assert_eq!(audit(&mut xsk).get(Location::RxRing), 1);

    let frame = xsk.harvest_rx().unwrap().expect("one packet");
    assert_eq!(frame.data_offset(), 256);
    assert_eq!(xsk.view_frame_bytes(&frame).unwrap(), b"hello warp");
    assert_eq!(audit(&mut xsk).get(Location::AppRx), 1);

    let chunk = frame.chunk_idx();
    let mut tx = xsk.turn_around_tx_in_place(frame, 16).unwrap();
    assert_eq!(tx.chunk_idx(), chunk, "same chunk");
    let bytes = xsk.tx_bytes_mut(&mut tx).unwrap();
    assert_eq!(&bytes[..10], b"hello warp");
    assert_eq!(&bytes[10..], &[0; 6], "extension is zeroed, no stale bytes");
    bytes.copy_from_slice(b"reply from warp!");

    let lease = xsk.submit_paced_tx(tx, 41, 1_000_000).unwrap();
    assert_eq!(
        (lease.pkt_num_full(), lease.edt_departure_ns()),
        (41, 1_000_000)
    );
    assert_eq!(lease.chunk_idx(), chunk);
    assert_eq!(audit(&mut xsk).get(Location::TxRing), 1);

    xsk.backend_mut().step(); // tx ring -> NIC
    assert_eq!(audit(&mut xsk).get(Location::NicTx), 1);
    xsk.backend_mut().step(); // NIC -> completion ring
    assert_eq!(audit(&mut xsk).get(Location::CompletionRing), 1);
    let report = xsk.harvest_completions(8);
    assert_eq!(report.completed_leased, 1);
    assert_eq!(audit(&mut xsk).get(Location::InFlight), 1);
    assert_eq!(xsk.retire(lease).unwrap(), RetireOutcome::Freed);
    let acc = audit(&mut xsk);
    assert_eq!(acc.get(Location::Free), 3);
    assert_eq!(xsk.replenish_fill(8), 3);
    assert_eq!(
        audit(&mut xsk).get(Location::FillRing) + audit(&mut xsk).get(Location::NicRx),
        8
    );
}

#[test]
fn loopback_delivers_transmitted_bytes_back_to_rx() {
    let mut xsk = xsk(8, SoftNicMode::Loopback);
    let mut tx = xsk.alloc_tx(5).unwrap();
    xsk.tx_bytes_mut(&mut tx).unwrap().copy_from_slice(b"loop!");
    let lease = xsk.submit_paced_tx(tx, 0, 0).unwrap();
    xsk.backend_mut().step();
    let rx = xsk.harvest_rx().unwrap().expect("looped back");
    assert_eq!(xsk.view_frame_bytes(&rx).unwrap(), b"loop!");
    assert_ne!(rx.chunk_idx(), lease.chunk_idx());
    xsk.recycle_to_fill_ring(rx).unwrap();
    // Retire before the completion: freed when it arrives.
    assert_eq!(
        xsk.retire(lease).unwrap(),
        RetireOutcome::AwaitingCompletion
    );
    xsk.backend_mut().step();
    let report = xsk.harvest_completions(8);
    assert_eq!((report.freed, report.completed_leased), (1, 0));
    let acc = audit(&mut xsk);
    assert_eq!(acc.get(Location::Free), 2);
    assert_eq!(acc.get(Location::InFlight), 0);
}

#[test]
fn malformed_rx_descriptors_are_rejected_without_losing_frames() {
    let cases = [
        (Fault::RxHeadroomOverlap, "headroom"),
        (Fault::RxOverLength, "over-length"),
        (Fault::RxZeroLength, "zero"),
        (Fault::RxOutOfRange, "out-of-umem"),
    ];
    for (fault, what) in cases {
        let mut xsk = xsk(4, SoftNicMode::Scripted);
        xsk.backend_mut().inject(fault);
        xsk.backend_mut().step();
        assert_eq!(xsk.backend().pending_faults(), 0, "{what} applied");
        audit(&mut xsk);
        let rejected = xsk.harvest_rx().expect_err(what);
        match (fault, rejected.reason) {
            (
                Fault::RxHeadroomOverlap,
                DescError::HeadroomOverlap {
                    offset: 255,
                    headroom: 256,
                },
            )
            | (
                Fault::RxOverLength,
                DescError::OverLength {
                    offset: 256,
                    len: 1793,
                    frame_size: 2048,
                },
            )
            | (Fault::RxZeroLength, DescError::ZeroLength)
            | (Fault::RxOutOfRange, DescError::OutOfUmem { .. }) => {}
            other => panic!("{what}: unexpected {other:?}"),
        }
        if fault == Fault::RxOutOfRange {
            assert_eq!((rejected.chunk, rejected.recycled), (None, false));
        } else {
            let chunk = rejected.chunk.expect("identifiable chunk");
            assert!(rejected.recycled);
            assert_eq!(xsk.location(chunk), Some(DriverLoc::FillPosted));
        }
        let acc = audit(&mut xsk);
        assert_eq!(
            acc.get(Location::FillRing) + acc.get(Location::NicRx),
            3,
            "{what}: every posted frame is back on the receive side"
        );
        assert_eq!(xsk.stats().rx_rejected, 1);
        assert!(xsk.harvest_rx().unwrap().is_none());
    }
}

#[test]
fn zero_headroom_overlap_fault_crosses_the_chunk_end() {
    let mut cfg = config(4);
    cfg.headroom = 0;
    let mut xsk = SoftNic::xsk(cfg, SoftNicMode::Scripted).unwrap();
    xsk.replenish_fill(4);
    xsk.backend_mut().inject(Fault::RxHeadroomOverlap);
    xsk.backend_mut().step();
    let rejected = xsk.harvest_rx().unwrap_err();
    assert!(matches!(
        rejected.reason,
        DescError::OverLength {
            offset: 2047,
            len: 2,
            ..
        }
    ));
    assert!(rejected.recycled);
    UmemAuditor::new().audit(&mut xsk).unwrap();
}

#[test]
fn spurious_completions_are_skipped_and_counted() {
    let mut xsk = xsk(4, SoftNicMode::Scripted);
    xsk.backend_mut().inject(Fault::CompletionOutOfRange);
    xsk.backend_mut().step();
    let report = xsk.harvest_completions(8);
    assert_eq!(report.invalid, 1);
    assert_eq!(xsk.stats().completions_invalid, 1);
    audit(&mut xsk);
}

#[test]
fn turn_around_rejects_bad_windows_and_returns_the_frame() {
    let mut xsk = xsk(4, SoftNicMode::Scripted);
    xsk.backend_mut().deliver(b"abc");
    let frame = xsk.harvest_rx().unwrap().unwrap();
    let refused = xsk.turn_around_tx_at(frame, 255, 10).unwrap_err();
    assert_eq!(
        refused.error,
        UmemError::HeadroomOverlap {
            offset: 255,
            headroom: 256
        }
    );
    let frame = refused.into_owner();
    let refused = xsk
        .turn_around_tx_in_place(frame, 2048 - 256 + 1)
        .unwrap_err();
    assert!(matches!(refused.error, UmemError::LengthOutOfFrame { .. }));
    let frame = refused.into_owner();
    let refused = xsk.turn_around_tx_in_place(frame, 0).unwrap_err();
    assert!(matches!(
        refused.error,
        UmemError::LengthOutOfFrame { len: 0, .. }
    ));
    let frame = refused.into_owner();
    audit(&mut xsk);
    // Prepending a header: move the offset back into the payload area only.
    let mut tx = xsk.turn_around_tx_at(frame, 256, 3).unwrap();
    assert_eq!(xsk.tx_bytes_mut(&mut tx).unwrap(), b"abc");
    xsk.abandon_tx(tx).unwrap();
    assert!(xsk.alloc_tx(0).is_err());
    assert!(matches!(
        xsk.alloc_tx(2048 - 255),
        Err(UmemError::LengthOutOfFrame { .. })
    ));
    audit(&mut xsk);
}

#[test]
fn foreign_owners_and_bad_packet_numbers_are_refused() {
    let mut a = xsk(4, SoftNicMode::Scripted);
    let mut b = xsk(4, SoftNicMode::Scripted);
    a.backend_mut().deliver(b"a");
    let frame = a.harvest_rx().unwrap().unwrap();
    assert_eq!(
        b.view_frame_bytes(&frame).unwrap_err(),
        UmemError::ForeignOwner
    );
    let refused = b.recycle_to_fill_ring(frame).unwrap_err();
    assert_eq!(refused.error, UmemError::ForeignOwner);
    let tx = a.turn_around_tx_in_place(refused.into_owner(), 1).unwrap();
    let refused = a.submit_paced_tx(tx, 1 << 62, 0).unwrap_err();
    assert_eq!(
        refused.error,
        UmemError::PacketNumberOutOfRange { pn: 1 << 62 }
    );
    let lease = a
        .submit_paced_tx(refused.into_owner(), (1 << 62) - 1, 0)
        .unwrap();
    let refused = b.retire(lease).unwrap_err();
    assert_eq!(refused.error, UmemError::ForeignOwner);
    assert_eq!(
        a.retire(refused.into_owner()).unwrap(),
        RetireOutcome::AwaitingCompletion
    );
    audit(&mut a);
    audit(&mut b);
}

#[test]
fn full_tx_ring_backpressure_returns_the_frame() {
    let mut xsk = SoftNic::xsk(config(16), SoftNicMode::Scripted).unwrap();
    xsk.backend_mut().inject(Fault::StallTx(3));
    xsk.backend_mut().step();
    let mut leases = Vec::new();
    for pn in 0..8 {
        let tx = xsk.alloc_tx(64).unwrap();
        leases.push(xsk.submit_paced_tx(tx, pn, 0).unwrap());
    }
    let tx = xsk.alloc_tx(64).unwrap();
    let refused = xsk.submit_paced_tx(tx, 8, 0).unwrap_err();
    assert_eq!(refused.error, UmemError::TxRingFull);
    assert_eq!(xsk.stats().tx_ring_full, 1);
    let tx = refused.into_owner();
    assert_eq!(audit(&mut xsk).get(Location::TxRing), 8);
    // Completion stall: the NIC holds at most the completion ring's worth.
    xsk.backend_mut().inject(Fault::StallCompletion(10));
    for _ in 0..4 {
        xsk.backend_mut().step();
        audit(&mut xsk);
    }
    assert_eq!(audit(&mut xsk).get(Location::NicTx), 8);
    let tx_is_full = xsk.submit_paced_tx(tx, 8, 0);
    let lease = tx_is_full.expect("tx ring drained into the NIC");
    leases.push(lease);
    for _ in 0..12 {
        xsk.backend_mut().step();
        xsk.harvest_completions(4);
        audit(&mut xsk);
    }
    for lease in leases {
        assert_eq!(xsk.retire(lease).unwrap(), RetireOutcome::Freed);
    }
    let acc = audit(&mut xsk);
    assert_eq!(acc.get(Location::Free), 16);
}

#[test]
fn dropped_owners_are_detected_and_quarantined() {
    let mut xsk = xsk(8, SoftNicMode::Scripted);
    let mut auditor = UmemAuditor::new();
    xsk.backend_mut().deliver(b"x");
    xsk.backend_mut().deliver(b"y");
    let rx = xsk.harvest_rx().unwrap().unwrap();
    let leaked_rx = rx.chunk_idx();
    drop(rx);
    let err = auditor.audit(&mut xsk).unwrap_err();
    assert_eq!(
        err,
        ConservationError::Leaked {
            count: 1,
            first_chunk: Some(leaked_rx)
        }
    );
    assert_eq!(xsk.location(leaked_rx), Some(DriverLoc::Leaked));
    assert_eq!(xsk.leaked_owners(), 1);
    assert!(
        !xsk.free_list().contains(&leaked_rx),
        "never silently recycled"
    );

    let tx = xsk.alloc_tx(8).unwrap();
    std::mem::drop(tx);
    assert!(matches!(
        auditor.audit(&mut xsk),
        Err(ConservationError::Leaked { count: 2, .. })
    ));

    // A dropped lease before its completion: flagged, then quarantined.
    let rx = xsk.harvest_rx().unwrap().unwrap();
    let tx = xsk.turn_around_tx_in_place(rx, 1).unwrap();
    let lease = xsk.submit_paced_tx(tx, 0, 0).unwrap();
    let chunk = lease.chunk_idx();
    drop(lease);
    assert!(matches!(
        auditor.audit(&mut xsk),
        Err(ConservationError::Leaked { .. })
    ));
    assert_eq!(
        xsk.location(chunk),
        Some(DriverLoc::TxPosted(LeaseMark::Leaked))
    );
    xsk.backend_mut().step();
    xsk.backend_mut().step();
    assert_eq!(xsk.harvest_completions(8).leaked, 1);
    assert_eq!(xsk.location(chunk), Some(DriverLoc::Leaked));
    let census = xsk.census();
    assert_eq!(census.leaked, 3);
    // The totals still add up: leaks are a location, not a disappearance.
    match auditor.audit(&mut xsk) {
        Err(ConservationError::Leaked { count: 3, .. }) => {}
        other => panic!("{other:?}"),
    }
}

/// Long randomized runs. The auditor (exact, chunk by chunk) runs after
/// every NIC step and every application operation.
fn randomized_run(seed: u64, mode: SoftNicMode, steps: u32) {
    let mut rng = Rng(seed);
    let frames = 32 + rng.below(64) as u32;
    let mut cfg = config(frames);
    cfg.headroom = [0, 128, 256][rng.below(3) as usize];
    cfg.rx_size = 1 << (2 + rng.below(4));
    cfg.tx_size = 1 << (2 + rng.below(4));
    cfg.completion_size = 1 << (2 + rng.below(4));
    let mut xsk = SoftNic::xsk(cfg, mode).unwrap();
    {
        let nic = xsk.backend_mut();
        nic.fill_batch = rng.below(16) as u32 + 1;
        nic.tx_batch = rng.below(16) as u32 + 1;
        nic.rx_batch = rng.below(8) as u32 + 1;
    }
    let mut auditor = UmemAuditor::new();
    let mut check = |x: &mut SoftXsk| {
        auditor
            .audit(x)
            .unwrap_or_else(|e| panic!("seed {seed:#x}: {e:?}"));
    };
    xsk.replenish_fill(frames / 2);
    check(&mut xsk);
    let mut echo = Echo::new(frames as usize);
    for _ in 0..steps {
        for _ in 0..rng.below(4) {
            let room = (cfg.frame_size - cfg.headroom) as usize;
            let packet = random_packet(&mut rng, room.min(1500));
            xsk.backend_mut().queue_ingress(packet);
        }
        if rng.chance(15) {
            let fault = random_fault(&mut rng);
            xsk.backend_mut().inject(fault);
        }
        if mode == SoftNicMode::Loopback && rng.chance(10) {
            // Loopback mode still accepts direct deliveries.
            let packet = random_packet(&mut rng, 200);
            xsk.backend_mut().deliver(&packet);
        }
        xsk.backend_mut().step();
        check(&mut xsk);
        echo.step(&mut xsk, &mut rng, &mut check);
    }
    // Quiesce: consume every owner, then let the NIC drain.
    echo.drain(&mut xsk);
    check(&mut xsk);
    for _ in 0..200 {
        xsk.backend_mut().step();
        loop {
            match xsk.harvest_rx() {
                Ok(Some(frame)) => xsk.recycle_to_fill_ring(frame).unwrap(),
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        xsk.harvest_completions(u32::MAX);
        check(&mut xsk);
    }
    let acc = auditor.audit(&mut xsk).unwrap();
    assert_eq!(acc.sum(), u64::from(frames));
    assert_eq!(acc.get(Location::AppRx) + acc.get(Location::AppTx), 0);
    assert_eq!(acc.get(Location::InFlight), 0);
    assert_eq!(acc.get(Location::TxRing) + acc.get(Location::NicTx), 0);
    assert_eq!(xsk.leaked_owners(), 0);
    let s = &echo.stats;
    assert!(s.harvested > 0 && s.replied > 0, "seed {seed:#x}: {s:?}");
}

#[test]
fn randomized_scripted_runs_conserve_frames() {
    for seed in 1..=12u64 {
        randomized_run(0x9e37_79b9_7f4a_7c15 ^ seed, SoftNicMode::Scripted, 400);
    }
}

#[test]
fn randomized_loopback_runs_conserve_frames() {
    for seed in 1..=12u64 {
        randomized_run(0xd1b5_4a32_d192_ed03 ^ seed, SoftNicMode::Loopback, 400);
    }
}

#[test]
fn randomized_runs_exercise_every_path() {
    // Aggregate coverage over a larger run: every rejection and the
    // backpressure path must actually occur.
    let mut rng = Rng(0x1234_5678);
    let mut xsk = SoftNic::xsk(config(48), SoftNicMode::Loopback).unwrap();
    xsk.replenish_fill(48);
    let mut auditor = UmemAuditor::new();
    let mut check = |x: &mut SoftXsk| auditor.audit(x).map(drop).unwrap();
    let mut echo = Echo::new(48);
    for step in 0..3000u32 {
        if step % 7 == 0 {
            xsk.backend_mut().inject(random_fault(&mut rng));
        }
        let packet = random_packet(&mut rng, 1000);
        xsk.backend_mut().deliver(&packet);
        xsk.backend_mut().step();
        check(&mut xsk);
        echo.step(&mut xsk, &mut rng, &mut check);
    }
    echo.drain(&mut xsk);
    check(&mut xsk);
    let kinds = echo.stats.rejected_kinds;
    assert!(
        kinds.iter().all(|&n| n > 0),
        "rejection kinds seen: {kinds:?}"
    );
    assert!(echo.stats.backpressured > 0, "{:?}", echo.stats);
    assert!(echo.stats.retired_early > 0 && echo.stats.retired_late > 0);
    assert!(echo.stats.malformed_recycled > 0);
    let nic = xsk.backend().stats();
    assert!(
        nic.rx_dropped_ring_full + nic.rx_dropped_no_buffer > 0,
        "{nic:?}"
    );
    assert_eq!(nic.tx_invalid, 0);
}

#[test]
fn driver_refuses_a_backend_with_different_geometry() {
    let nic = SoftNic::new(config(8), SoftNicMode::Scripted).unwrap();
    assert!(matches!(
        vesper_warp_umem::Xsk::new(config(16), nic),
        Err(UmemError::InvalidConfig(_))
    ));
}
