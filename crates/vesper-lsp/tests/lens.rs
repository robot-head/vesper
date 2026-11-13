use serde_json::json;
use vesper_lsp::{
    Id,
    lens::{
        CandidateMethod, CompletionTier, LensError, PendingRequests, ProofLedger, ProofStatus,
        ReceiverPass, cancel_params, rank_methods,
    },
};

fn method(name: &str, receiver: ReceiverPass, abortable: &[&str]) -> CandidateMethod {
    CandidateMethod {
        name: name.into(),
        receiver,
        abortable_effects: abortable.iter().map(|e| (*e).to_owned()).collect(),
        detail: format!("fn {name}"),
    }
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
