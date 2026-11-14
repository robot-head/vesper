//! Owned cancellation of native fibers at the fiber/continuation-table level:
//! cancel before start, cancellation edges that park again, table-wide
//! cancellation and the counted drop fallback. These need no Pulse scheduler,
//! so they run on every direct-fiber target (Linux x86_64/AArch64, macOS
//! AArch64, Windows x86_64). Scheduler/runtime lifecycle tests stay in `lifecycle.rs`.
#![cfg(vesper_direct_fibers)]
use std::rc::Rc;
use vesper_fiber_abi::abi::exit;
use vesper_fiber_abi::evidence::{EvidenceRegistry, EvidenceVector};
use vesper_fiber_abi::fiber::abandoned_fibers;
use vesper_fiber_abi::prototype::{self, JitPrototype, OptLevel};
use vesper_fiber_abi::{ContinuationTable, Outcome, Parked, ResumeError, Resumption};

fn parked(o: Outcome) -> Parked {
    match o {
        Outcome::Parked(k) => k,
        Outcome::Exited(e) => panic!("expected a park, fiber exited: {e:?}"),
    }
}

fn setup() -> (JitPrototype, Rc<EvidenceVector>, *const u64) {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, clock) = prototype::pulse_read_evidence(&mut registry).unwrap();
    (jit, evidence, clock)
}

#[test]
fn cancel_before_start_runs_no_generated_code() {
    let (jit, evidence, clock) = setup();
    let fiber = jit.spawn_pulse_read(3, evidence.clone()).unwrap();
    let id = fiber.id();
    let e = fiber.cancel();
    assert_eq!(e.fiber_id, id);
    assert_eq!(e.status, exit::CANCELLED);
    assert_eq!((e.result, e.parks), (0, 0));
    assert_eq!(e.word(0), 0, "the entry never touched the counter place");
    assert_eq!(unsafe { *clock }, 0);
}

#[test]
fn cancellation_edge_that_reparks_returns_the_live_owner() {
    for opt in [OptLevel::None, OptLevel::Speed] {
        let jit = JitPrototype::new(opt).unwrap();
        let mut registry = EvidenceRegistry::new();
        let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
        let async_id = evidence.identity(1).unwrap();

        let k = parked(jit.spawn_cancel_repark(evidence.clone()).unwrap().start());
        let first = k.token();
        // Cancelling does not complete the fiber: it parks again in cleanup
        // and the caller still owns it, at a newer generation.
        let mut k = parked(k.cancel());
        assert!(k.token().generation > first.generation);
        assert_eq!(k.request().op, prototype::OP_YIELD);
        assert_eq!(k.request().evidence, async_id);
        assert_eq!(
            u64::from_le_bytes(k.data()[8..16].try_into().unwrap()),
            prototype::CANCEL_NOTE
        );
        // The re-parked data frame is still movable while owned.
        k.relocate_data();
        let e = k
            .resume(Resumption::value(async_id, 0))
            .unwrap()
            .exited()
            .unwrap();
        assert_eq!(e.status, exit::CANCELLED, "{opt:?}");
        assert_eq!(e.result, 1 + prototype::CLEANUP_DONE);
        assert_eq!(e.parks, 2);

        // Cancelling the cleanup park as well takes the abortive edge.
        let k = parked(jit.spawn_cancel_repark(evidence.clone()).unwrap().start());
        let k = parked(k.cancel());
        let e = k.cancel().exited().unwrap();
        assert_eq!(e.status, exit::CANCELLED);
        assert_eq!(e.result, 1);
        assert_eq!(e.word(1), prototype::CANCEL_TWICE_NOTE);

        // Answering without cancelling completes normally.
        let k = parked(jit.spawn_cancel_repark(evidence).unwrap().start());
        let e = k
            .resume(Resumption::value(async_id, 0))
            .unwrap()
            .exited()
            .unwrap();
        assert_eq!((e.status, e.result), (exit::COMPLETE, 11));
    }
}

#[test]
fn table_cancel_all_keeps_reparked_owners_and_rejects_stale_tokens() {
    let (jit, evidence, _) = setup();
    let async_id = evidence.identity(1).unwrap();
    let mut table = ContinuationTable::new();
    let plain = table.insert(parked(
        jit.spawn_pulse_read(1, evidence.clone()).unwrap().start(),
    ));
    let a = table.insert(parked(
        jit.spawn_cancel_repark(evidence.clone()).unwrap().start(),
    ));
    let b = table.insert(parked(
        jit.spawn_cancel_repark(evidence.clone()).unwrap().start(),
    ));

    let report = table.cancel_all();
    assert_eq!(report.exited.len(), 1);
    assert_eq!(report.exited[0].fiber_id, plain.fiber_id);
    assert_eq!(report.exited[0].status, exit::CANCELLED);
    assert_eq!(report.reparked.len(), 2);
    assert_eq!(table.len(), 2);
    assert!(table.cancel_delivered(a.fiber_id) && table.cancel_delivered(b.fiber_id));
    // Pre-cancellation tokens are stale; the finished fiber is unknown.
    for old in [a, b] {
        assert!(matches!(
            table.resume(old, Resumption::value(async_id, 0)),
            Err(ResumeError::StaleToken { .. })
        ));
    }
    assert_eq!(
        table
            .resume(plain, Resumption::value(async_id, 0))
            .unwrap_err(),
        ResumeError::UnknownToken
    );
    // A second cancel_all does not cancel the cleanup parks again.
    let again = table.cancel_all();
    assert!(again.exited.is_empty() && again.reparked.is_empty());
    assert_eq!(table.tokens(), report.reparked);

    // A rejected resume of a cleanup park leaves it owned and marked.
    let new_a = report.reparked[0];
    assert!(matches!(
        table.resume(new_a, Resumption::value(async_id + 999, 0)),
        Err(ResumeError::WrongEvidence { .. })
    ));
    assert!(table.cancel_delivered(new_a.fiber_id));
    let e = table
        .resume(new_a, Resumption::value(async_id, 0))
        .unwrap()
        .exited()
        .unwrap();
    assert_eq!(e.result, 1 + prototype::CLEANUP_DONE);
    assert_eq!(
        table
            .resume(new_a, Resumption::value(async_id, 0))
            .unwrap_err(),
        ResumeError::UnknownToken
    );

    // The remaining owner can be handed back instead of dropped.
    let mut rest = table.into_parked();
    assert_eq!(rest.len(), 1);
    let e = rest
        .pop()
        .unwrap()
        .resume(Resumption::value(async_id, 0))
        .unwrap()
        .exited()
        .unwrap();
    assert_eq!(e.status, exit::CANCELLED);
}

#[test]
fn dropping_a_reparked_cancellation_is_counted_never_completed() {
    let (jit, evidence, _) = setup();
    let code = jit.code_liveness();
    let k = parked(jit.spawn_cancel_repark(evidence.clone()).unwrap().start());
    drop(jit);
    // The parked fiber keeps code and evidence alive.
    assert!(code.upgrade().is_some());
    let evidence_shares = Rc::strong_count(&evidence);
    let before = abandoned_fibers();
    // Drop's single best-effort cancellation re-parks: abandoned and counted.
    drop(k);
    assert_eq!(abandoned_fibers(), before + 1);
    assert!(code.upgrade().is_none(), "code freed with the last stack");
    assert_eq!(Rc::strong_count(&evidence), evidence_shares - 1);
}
