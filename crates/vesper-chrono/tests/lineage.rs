//! Obligation lineage: monotonic ids, full chains with ancestry, single
//! terminal, leak and stall reporting, truncated logs, session leak detection.

use vesper_chrono::{
    Bundle, BundleKey, BundleLimits, FlightRecorder, HopKind, LineageError, ObligationHop,
    ObligationLineage, ObligationStatus, RecorderConfig, RedactionPolicy, ReplaySession,
    leaked_sessions,
};

fn hop(seq: u64, kind: HopKind) -> ObligationHop {
    ObligationHop {
        lamport_seq: seq,
        fiber_id: seq % 3,
        source_span_id: seq as u32,
        kind,
    }
}

#[test]
fn chains_ancestry_and_terminal_rules() {
    let mut l = ObligationLineage::new();
    let txn = l.mint(1, 0, 10, None).unwrap();
    assert_eq!(txn, 1);
    l.record(txn, hop(2, HopKind::SinkMove)).unwrap();
    l.record(txn, hop(3, HopKind::Projection { place_hash: 0xF1 }))
        .unwrap();
    let page = l.mint(4, 1, 11, Some(txn)).unwrap();
    assert_eq!(page, 2, "ids are monotonic");
    l.record(page, hop(5, HopKind::ChannelHandoff { to_fiber: 9 }))
        .unwrap();
    l.record(txn, hop(6, HopKind::UnpackDischarge)).unwrap();

    assert_eq!(
        l.record(txn, hop(7, HopKind::SinkMove)),
        Err(LineageError::AfterTerminal { obligation_id: 1 })
    );
    assert_eq!(
        l.record(txn, hop(7, HopKind::UnpackDischarge)),
        Err(LineageError::AfterTerminal { obligation_id: 1 }),
        "double discharge"
    );
    assert_eq!(
        l.record(99, hop(7, HopKind::Leak)),
        Err(LineageError::UnknownObligation { obligation_id: 99 })
    );
    assert_eq!(
        l.record(5, hop(7, HopKind::Mint { parent: None })),
        Err(LineageError::NonMonotonicId {
            expected: 3,
            got: 5
        })
    );
    assert_eq!(
        l.record(3, hop(7, HopKind::Mint { parent: Some(3) })),
        Err(LineageError::UnknownParent { parent: 3 })
    );
    assert_eq!(
        l.record(page, hop(6, HopKind::Leak)),
        Err(LineageError::OutOfOrder {
            previous: 6,
            got: 6
        })
    );

    // The page is still live: a stall. "Where did it come from?"
    assert_eq!(l.status(page).unwrap(), ObligationStatus::Live);
    assert_eq!(l.open_obligations(), vec![page]);
    let p = l.provenance(page).unwrap();
    assert_eq!(p.evicted_ancestor, None);
    let ids: Vec<_> = p.chains.iter().map(|(id, c)| (*id, c.len())).collect();
    assert_eq!(ids, vec![(txn, 4), (page, 2)], "ancestor chain first");
    assert_eq!(p.hops().count(), 6);

    l.record(page, hop(8, HopKind::Leak)).unwrap();
    assert_eq!(l.status(page).unwrap(), ObligationStatus::Leaked);
    assert_eq!(l.leaked(), vec![page]);
    assert!(l.open_obligations().is_empty());
    assert!(l.audit_complete());
}

#[test]
fn wire_tags_roundtrip_and_malformed_records_fail() {
    for kind in [
        HopKind::Mint { parent: None },
        HopKind::Mint { parent: Some(4) },
        HopKind::SinkMove,
        HopKind::ChannelHandoff { to_fiber: 3 },
        HopKind::UnpackDischarge,
        HopKind::Projection { place_hash: 77 },
        HopKind::Leak,
    ] {
        assert_eq!(HopKind::from_record(&kind.to_record(5, 1)).unwrap(), kind);
    }
    let mut bad = HopKind::SinkMove.to_record(1, 0);
    bad.aux = 1;
    assert!(HopKind::from_record(&bad).is_err());
    bad.hop_kind_tag = 6;
    assert!(HopKind::from_record(&bad).is_err());
}

fn key() -> BundleKey {
    BundleKey::new([3; 32])
}

#[test]
fn truncated_ring_reports_evicted_creations() {
    let mut rec = FlightRecorder::new(RecorderConfig {
        ring_bytes: 4096,
        max_records: 8,
        max_payload: 64,
        snapshot_slots: 0,
        snapshot_image_bytes: 0,
    })
    .unwrap();
    let old = rec.mint_obligation(0, 0, 1, None).unwrap();
    for t in 1..10 {
        rec.record_effect(0, t, 1, 0, b"filler").unwrap();
    }
    let child = rec.mint_obligation(1, 10, 2, Some(old)).unwrap();
    rec.record_obligation_hop(1, 11, old, HopKind::SinkMove, 3)
        .unwrap();
    rec.record_obligation_hop(1, 12, child, HopKind::Leak, 4)
        .unwrap();
    assert!(rec.evicted_records() > 0);
    let bytes = Bundle::export(&rec, &RedactionPolicy::new(), b"", &key()).unwrap();
    let session = ReplaySession::open_bundle(&bytes, &key(), &BundleLimits::default()).unwrap();
    let lineage = session.lineage();
    assert_eq!(
        session.query_linear_lineage(old).unwrap_err(),
        vesper_chrono::ReplayError::Lineage(LineageError::CreationEvicted { obligation_id: old })
    );
    let p = lineage.provenance(child).unwrap();
    assert_eq!(p.evicted_ancestor, Some(old));
    assert_eq!(p.chains.len(), 1);
    assert_eq!(lineage.truncated().collect::<Vec<_>>(), vec![old]);
    assert_eq!(lineage.leaked(), vec![child]);
    let _ = session.close();
}

#[test]
fn dropping_an_unclosed_session_is_detected() {
    let mut rec = FlightRecorder::new(RecorderConfig::small()).unwrap();
    rec.record_effect(0, 0, 1, 0, b"x").unwrap();
    let bytes = Bundle::export(&rec, &RedactionPolicy::new(), b"", &key()).unwrap();
    let before = leaked_sessions();
    let session = ReplaySession::open_bundle(&bytes, &key(), &BundleLimits::default()).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(session)));
    assert_eq!(
        result.is_err(),
        cfg!(debug_assertions),
        "debug builds panic"
    );
    assert!(leaked_sessions() > before, "leak counted in every build");
    let closed = ReplaySession::open_bundle(&bytes, &key(), &BundleLimits::default()).unwrap();
    let count = leaked_sessions();
    let bundle = closed.close();
    assert_eq!(bundle.records.len(), 1);
    assert_eq!(leaked_sessions(), count, "closing is not a leak");
}
