use serde_json::json;
use std::collections::BTreeSet;
use vesper_lsp::{
    Id,
    lens::{
        CandidateMethod, CompletionTier, LensError, PendingRequests, ProofLedger, ProofStatus,
        ReceiverPass, cancel_params, ceiling_grants, rank_methods, rank_methods_within,
    },
};

fn method(name: &str, receiver: ReceiverPass, abortable: &[&str]) -> CandidateMethod {
    CandidateMethod {
        name: name.into(),
        receiver,
        abortable_effects: abortable.iter().map(|e| (*e).to_owned()).collect(),
        effects: abortable.iter().map(|e| (*e).to_owned()).collect(),
        detail: format!("fn {name}"),
    }
}

#[test]
fn methods_outside_the_capability_ceiling_rank_last_with_the_missing_effects() {
    let mut net = method("send", ReceiverPass::Sink, &[]);
    net.effects = vec!["Net".into(), "Storage".into()];
    let mut ffi = method("compress", ReceiverPass::In, &[]);
    ffi.effects = vec!["Ffi[LibLz4]".into()];
    let mut store = method("flush", ReceiverPass::Inout, &[]);
    store.effects = vec!["Storage[Wal]".into()];
    let methods = [
        net,
        ffi,
        store,
        method("commit", ReceiverPass::Sink, &[]),
        method("log", ReceiverPass::In, &["Fail"]),
    ];
    let ceiling: BTreeSet<String> = ["Storage".to_owned(), "Fail".to_owned()].into();
    let ranked = rank_methods_within(true, &[], &methods, Some(&ceiling));
    let order: Vec<_> = ranked.iter().map(|r| (r.label.as_str(), r.tier)).collect();
    assert_eq!(
        order,
        [
            ("commit", CompletionTier::DischargesLiveLinear),
            ("flush", CompletionTier::ValidFrameCompatible),
            ("log", CompletionTier::BlockedByFrame),
            ("compress", CompletionTier::ExceedsCeiling),
            ("send", CompletionTier::ExceedsCeiling),
        ]
    );
    assert_eq!(
        ranked[4].badge.as_deref(),
        Some("exceeds Loom.toml capability ceiling: Net")
    );
    assert_eq!(
        CompletionTier::ExceedsCeiling.as_str(),
        "exceeds_capability_ceiling"
    );
    // An exact rendering grants only itself; a bare name grants every instance.
    let exact: BTreeSet<String> = ["Ffi[LibLz4]".to_owned()].into();
    assert!(ceiling_grants(&exact, "Ffi[LibLz4]"));
    assert!(!ceiling_grants(&exact, "Ffi[LibZstd]"));
    assert!(ceiling_grants(&ceiling, "Storage[Wal]"));
    // No manifest: exactly the unconstrained ranking.
    assert_eq!(
        rank_methods_within(true, &[], &methods, None),
        rank_methods(true, &[], &methods)
    );
    // An empty ceiling (a pure package) refuses every effectful method.
    let pure = BTreeSet::new();
    let ranked = rank_methods_within(false, &[], &methods, Some(&pure));
    assert_eq!(
        ranked
            .iter()
            .filter(|r| r.tier == CompletionTier::ExceedsCeiling)
            .count(),
        4
    );
}

#[test]
fn consuming_methods_rank_first_only_for_live_linear_receivers() {
    let methods = [
        method("slot", ReceiverPass::In, &[]),
        method("commit", ReceiverPass::Sink, &[]),
        method("abort", ReceiverPass::Sink, &[]),
        method("touch", ReceiverPass::Inout, &[]),
    ];
    let ranked = rank_methods(true, &[], &methods);
    let order: Vec<_> = ranked.iter().map(|r| (r.label.as_str(), r.tier)).collect();
    assert_eq!(
        order,
        [
            ("abort", CompletionTier::DischargesLiveLinear),
            ("commit", CompletionTier::DischargesLiveLinear),
            ("slot", CompletionTier::ValidFrameCompatible),
            ("touch", CompletionTier::ValidFrameCompatible),
        ]
    );
    assert_eq!(
        ranked[0].badge.as_deref(),
        Some("discharges linear obligation")
    );
    // A consumed or non-linear receiver has nothing to discharge.
    assert!(
        rank_methods(false, &[], &methods)
            .iter()
            .all(|r| r.tier == CompletionTier::ValidFrameCompatible && r.badge.is_none())
    );
}

#[test]
fn abortable_methods_are_blocked_while_a_linear_value_would_stay_live() {
    let methods = [
        method("persist", ReceiverPass::In, &["Storage"]),
        method("commit_io", ReceiverPass::Sink, &["Storage"]),
    ];
    // The receiver itself stays live across a borrowing abortable call.
    let ranked = rank_methods(true, &[], &methods);
    assert_eq!(ranked[0].label, "commit_io");
    assert_eq!(ranked[0].tier, CompletionTier::DischargesLiveLinear);
    assert_eq!(ranked[1].tier, CompletionTier::BlockedByFrame);
    assert!(ranked[1].badge.as_ref().unwrap().contains("the receiver"));
    // Another live binding blocks even the consuming call.
    let ranked = rank_methods(true, &["'other'".into()], &methods);
    assert!(
        ranked
            .iter()
            .all(|r| r.tier == CompletionTier::BlockedByFrame)
    );
    assert!(ranked[0].badge.as_ref().unwrap().contains("'other'"));
    // Nothing linear is live: abortable effects are fine.
    assert!(
        rank_methods(false, &[], &methods)
            .iter()
            .all(|r| r.tier == CompletionTier::ValidFrameCompatible)
    );
}

#[test]
fn ledger_is_keyed_by_source_identity_and_bounded() {
    assert_eq!(ProofLedger::new(0).unwrap_err(), LensError::InvalidLimits);
    let mut ledger = ProofLedger::new(2).unwrap();
    let a = [1u8; 32];
    let b = [2u8; 32];
    assert_eq!(ledger.status(a, "f"), ProofStatus::NotAttempted);
    ledger.record(
        a,
        "f",
        ProofStatus::Proved {
            detail: "ok".into(),
        },
    );
    assert_eq!(ledger.status(a, "f").as_str(), "proved");
    assert_eq!(ledger.status(b, "f"), ProofStatus::NotAttempted);
    ledger.record(
        a,
        "g",
        ProofStatus::Failed {
            stage: "kernel".into(),
            reason: "x".into(),
        },
    );
    ledger.record(b, "f", ProofStatus::NotAttempted);
    assert_eq!(ledger.len(), 2);
    // The oldest entry was evicted.
    assert_eq!(ledger.status(a, "f"), ProofStatus::NotAttempted);
    assert_eq!(ledger.status(a, "g").as_str(), "failed");
}

#[test]
fn pending_requests_answer_once() {
    assert!(PendingRequests::new(0).is_err());
    let mut pending = PendingRequests::new(2).unwrap();
    let token = pending.begin(Id::Integer(7)).unwrap();
    assert_eq!(
        pending.begin(Id::Integer(7)).unwrap_err(),
        LensError::DuplicateRequest
    );
    let other = pending.begin(Id::String("x".into())).unwrap();
    assert_eq!(
        pending.begin(Id::Integer(9)).unwrap_err(),
        LensError::TooManyPending
    );
    assert!(!token.is_cancelled());
    assert!(pending.cancel(&Id::Integer(7)));
    assert!(token.is_cancelled());
    // Cancelled work is never answered again, and a second cancel is a no-op.
    assert!(!pending.finish(&Id::Integer(7), &token));
    assert!(!pending.cancel(&Id::Integer(7)));
    // A client may reuse a cancelled ID; the old job's completion must not
    // answer the new request.
    let reused = pending.begin(Id::Integer(7)).unwrap();
    assert!(!pending.finish(&Id::Integer(7), &token));
    assert!(pending.finish(&Id::Integer(7), &reused));
    assert!(pending.finish(&Id::String("x".into()), &other));
    assert!(!other.is_cancelled());
    let last = pending.begin(Id::Integer(1)).unwrap();
    assert_eq!(pending.cancel_all(), [Id::Integer(1)]);
    assert!(last.is_cancelled() && pending.is_empty());
}

#[test]
fn cancel_parameters_are_typed() {
    assert_eq!(cancel_params(Some(&json!({"id": 4}))), Ok(Id::Integer(4)));
    assert_eq!(
        cancel_params(Some(&json!({"id": "a"}))),
        Ok(Id::String("a".into()))
    );
    for bad in [
        json!({}),
        json!({"id": 1.5}),
        json!({"id": 1u64 << 40}),
        json!({"id": null}),
    ] {
        assert_eq!(cancel_params(Some(&bad)), Err(LensError::InvalidCancel));
    }
    assert_eq!(cancel_params(None), Err(LensError::InvalidCancel));
}

#[test]
fn debouncer_arms_one_timer_per_burst_and_fires_after_idle() {
    use std::time::{Duration, Instant};
    use vesper_lsp::lens::{CancelToken, Debouncer, TimerAction};
    let delay = Duration::from_millis(150);
    let mut debounce = Debouncer::new(delay, 2).unwrap();
    let t0 = Instant::now();
    // The first edit arms a timer; later edits in the window only move the
    // deadline back.
    assert_eq!(debounce.touch("a", t0), TimerAction::Arm(delay));
    assert_eq!(
        debounce.touch("a", t0 + Duration::from_millis(100)),
        TimerAction::Idle
    );
    // The timer fires early relative to the moved deadline: re-arm for the rest.
    assert_eq!(
        debounce.timer("a", t0 + delay),
        TimerAction::Arm(Duration::from_millis(100))
    );
    let generation = match debounce.timer("a", t0 + Duration::from_millis(260)) {
        TimerAction::Fire { generation } => generation,
        other => panic!("{other:?}"),
    };
    assert_eq!(generation, 2);
    assert!(debounce.is_current("a", generation));
    // A second firing for the same generation does nothing.
    assert_eq!(
        debounce.timer("a", t0 + Duration::from_secs(1)),
        TimerAction::Idle
    );
    // A run in progress is cancelled by the next edit.
    let token = CancelToken::new();
    debounce.started("a", generation, token.clone());
    assert!(!token.is_cancelled());
    assert_eq!(
        debounce.touch("a", t0 + Duration::from_secs(2)),
        TimerAction::Arm(delay)
    );
    assert!(token.is_cancelled());
    assert!(!debounce.is_current("a", generation));
    // A run started for a stale generation is cancelled at once.
    let stale = CancelToken::new();
    debounce.started("a", generation, stale.clone());
    assert!(stale.is_cancelled());
    // Bounded: a third document is not tracked.
    assert_eq!(debounce.touch("b", t0), TimerAction::Arm(delay));
    assert_eq!(debounce.touch("c", t0), TimerAction::Idle);
    assert_eq!(debounce.len(), 2);
    let running = CancelToken::new();
    let generation = match debounce.timer("b", t0 + delay) {
        TimerAction::Fire { generation } => generation,
        other => panic!("{other:?}"),
    };
    debounce.started("b", generation, running.clone());
    debounce.forget("b");
    assert!(running.is_cancelled());
    debounce.cancel_all();
    assert!(debounce.is_empty());
    assert!(Debouncer::new(Duration::from_secs(61), 1).is_err());
    assert!(Debouncer::new(delay, 0).is_err());
}

#[test]
fn pending_requests_check_without_answering() {
    use vesper_lsp::{Id, lens::PendingRequests};
    let mut pending = PendingRequests::new(4).unwrap();
    let token = pending.begin(Id::Integer(1)).unwrap();
    assert!(pending.is_pending(&Id::Integer(1), &token));
    assert!(pending.is_pending(&Id::Integer(1), &token));
    assert!(pending.cancel(&Id::Integer(1)));
    assert!(!pending.is_pending(&Id::Integer(1), &token));
    assert!(token.is_cancelled());
}
