use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vesper_simpulse::{
    Error, IoResult, Journal, Simulator, TaskId, TaskState, TraceEvent, WakeReason,
};

// No Clone implementation: all runs exercise moved ownership, not snapshots.
struct Owner {
    label: u64,
    drops: Arc<AtomicUsize>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn owner(label: u64, drops: &Arc<AtomicUsize>) -> Owner {
    Owner {
        label,
        drops: drops.clone(),
    }
}

fn exercise(sim: &mut Simulator<Owner>, drops: &Arc<AtomicUsize>) -> Vec<(u64, u64, WakeReason)> {
    for label in 0..3 {
        sim.spawn(owner(label, drops)).unwrap();
    }
    let mut events = vec![];
    for _ in 0..3 {
        let token = sim.next_task().unwrap().unwrap();
        events.push((token.task().get(), sim.now_ns(), token.reason()));
        sim.yield_task(token).unwrap();
    }
    for _ in 0..3 {
        let token = sim.next_task().unwrap().unwrap();
        events.push((token.task().get(), sim.now_ns(), token.reason()));
        let label = token.payload().label;
        sim.schedule_io(
            token,
            10 + label,
            if label == 1 {
                IoResult::Error { code: 5 }
            } else {
                IoResult::Success { bytes: label }
            },
        )
        .unwrap();
    }
    while let Some(token) = sim.next_task().unwrap() {
        events.push((token.task().get(), sim.now_ns(), token.reason()));
        drop(sim.finish(token).unwrap());
    }
    assert!(sim.is_complete());
    events
}

#[test]
fn seeded_schedule_and_serialized_replay_are_identical() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut first = Simulator::new(123);
    let expected = exercise(&mut first, &drops);
    let journal = Journal::from_json(&first.journal().to_json().unwrap()).unwrap();
    let mut same_seed = Simulator::new(123);
    assert_eq!(exercise(&mut same_seed, &drops), expected);
    assert_eq!(same_seed.journal(), &journal);
    let mut replay = Simulator::replay(journal.clone()).unwrap();
    assert_eq!(exercise(&mut replay, &drops), expected);
    replay.finish_replay().unwrap();
    assert_eq!(replay.journal(), &journal);
    assert_eq!(drops.load(Ordering::SeqCst), 9);
}

#[test]
fn each_fixed_runnable_task_runs_once_per_round() {
    let mut sim = Simulator::new(5);
    let ids = (0..8).map(|n| sim.spawn(n).unwrap()).collect::<Vec<_>>();
    for _ in 0..50 {
        let mut round = vec![];
        for _ in 0..8 {
            let token = sim.next_task().unwrap().unwrap();
            round.push(token.task());
            sim.yield_task(token).unwrap();
        }
        round.sort();
        assert_eq!(round, ids);
        assert_eq!(sim.now_ns(), 0);
    }
}

#[test]
fn seed_changes_choices_but_does_not_change_fairness() {
    fn order(seed: u64) -> Vec<TaskId> {
        let mut sim = Simulator::new(seed);
        for n in 0..16 {
            sim.spawn(n).unwrap();
        }
        (0..16)
            .map(|_| {
                let token = sim.next_task().unwrap().unwrap();
                let id = token.task();
                sim.finish(token).unwrap();
                id
            })
            .collect()
    }
    assert_ne!(order(1), order(2));
}

#[test]
fn virtual_clock_only_jumps_when_nothing_can_run() {
    let mut sim = Simulator::new(8);
    sim.spawn(0).unwrap();
    sim.spawn(1).unwrap();
    let sleeping = sim.next_task().unwrap().unwrap();
    sim.sleep(sleeping, 86_400_000_000_000).unwrap();
    for _ in 0..10 {
        let runnable = sim.next_task().unwrap().unwrap();
        assert_eq!(sim.now_ns(), 0);
        sim.yield_task(runnable).unwrap();
    }
    let runnable = sim.next_task().unwrap().unwrap();
    assert_eq!(sim.now_ns(), 0);
    sim.finish(runnable).unwrap();
    let awakened = sim.next_task().unwrap().unwrap();
    assert_eq!(sim.now_ns(), 86_400_000_000_000);
    assert!(matches!(awakened.reason(), WakeReason::Timer(_)));
    sim.finish(awakened).unwrap();
}

#[test]
fn zero_duration_timer_is_delivered_without_clock_jump() {
    let mut sim = Simulator::new(8);
    sim.spawn(()).unwrap();
    let token = sim.next_task().unwrap().unwrap();
    let timer = sim.sleep(token, 0).unwrap();
    let awakened = sim.next_task().unwrap().unwrap();
    assert_eq!(awakened.reason(), WakeReason::Timer(timer));
    assert_eq!(sim.now_ns(), 0);
    assert!(
        !sim.journal()
            .events
            .iter()
            .any(|e| matches!(e, TraceEvent::ClockAdvance { .. }))
    );
    sim.finish(awakened).unwrap();
}

#[test]
fn external_io_quiescence_is_not_completion_or_time_advance() {
    let mut sim = Simulator::new(0);
    let id = sim.spawn(42).unwrap();
    let token = sim.next_task().unwrap().unwrap();
    let request = sim.park_io(token).unwrap();
    assert!(sim.next_task().unwrap().is_none());
    assert!(!sim.is_complete());
    assert_eq!(sim.pending_count(), 1);
    assert_eq!(sim.now_ns(), 0);
    assert_eq!(
        sim.task_state(id),
        Some(TaskState::WaitingIo {
            completion: request
        })
    );
    sim.complete_io(request, IoResult::Success { bytes: 42 })
        .unwrap();
    let token = sim.next_task().unwrap().unwrap();
    assert_eq!(sim.finish(token).unwrap(), 42);
}

#[test]
fn cancellation_does_not_release_io_and_success_can_win_the_race() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut sim = Simulator::new(0);
    let id = sim.spawn(owner(42, &drops)).unwrap();
    let token = sim.next_task().unwrap().unwrap();
    let request = sim.park_io(token).unwrap();
    assert!(sim.request_cancel(id).unwrap());
    assert!(!sim.request_cancel(id).unwrap());
    assert!(sim.next_task().unwrap().is_none());
    assert_eq!(sim.pending_count(), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    sim.complete_io(request, IoResult::Success { bytes: 1 })
        .unwrap();
    let token = sim.next_task().unwrap().unwrap();
    assert!(token.cancel_requested());
    assert_eq!(
        token.reason(),
        WakeReason::Io {
            completion: request,
            result: IoResult::Success { bytes: 1 }
        }
    );
    drop(sim.finish(token).unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn duplicate_and_timer_completions_do_not_consume_owners() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut sim = Simulator::new(0);
    sim.spawn(owner(1, &drops)).unwrap();
    let token = sim.next_task().unwrap().unwrap();
    let timer = sim.sleep(token, 10).unwrap();
    assert_eq!(
        sim.complete_io(timer, IoResult::Cancelled),
        Err(Error::UnknownCompletion(timer))
    );
    assert_eq!(sim.pending_count(), 1);
    let token = sim.next_task().unwrap().unwrap();
    let io = sim.park_io(token).unwrap();
    sim.complete_io(io, IoResult::Cancelled).unwrap();
    assert_eq!(
        sim.complete_io(io, IoResult::Cancelled),
        Err(Error::UnknownCompletion(io))
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let token = sim.next_task().unwrap().unwrap();
    assert_eq!(
        token.reason(),
        WakeReason::Io {
            completion: io,
            result: IoResult::Cancelled
        }
    );
    drop(sim.finish(token).unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn manual_completion_removes_delayed_injection() {
    let mut sim = Simulator::new(0);
    sim.spawn(()).unwrap();
    let token = sim.next_task().unwrap().unwrap();
    let id = sim
        .schedule_io(token, 100, IoResult::Error { code: 5 })
        .unwrap();
    sim.complete_io(id, IoResult::Success { bytes: 1 }).unwrap();
    let token = sim.next_task().unwrap().unwrap();
    sim.finish(token).unwrap();
    assert!(sim.next_task().unwrap().is_none());
    assert_eq!(sim.now_ns(), 0);
    assert_eq!(sim.pending_count(), 0);
}

#[test]
fn tokens_cannot_cross_simulators_and_errors_return_the_payload() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut a = Simulator::new(0);
    let mut b = Simulator::new(0);
    let id = a.spawn(owner(42, &drops)).unwrap();
    let token = a.next_task().unwrap().unwrap();
    let failed = b.yield_task(token).unwrap_err();
    assert_eq!(failed.error, Error::ForeignToken);
    assert_eq!(failed.token.payload().label, 42);
    assert_eq!(a.task_state(id), Some(TaskState::Running));
    assert_eq!(a.next_task().err(), Some(Error::TaskStillRunning(id)));
    drop(a.finish(failed.token).unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn corrupt_dispatch_batch_preserves_clock_pending_timer_and_payload() {
    let mut record = Simulator::new(0);
    record.spawn(()).unwrap();
    let token = record.next_task().unwrap().unwrap();
    let timer = record.sleep(token, 10).unwrap();
    let token = record.next_task().unwrap().unwrap();
    record.finish(token).unwrap();
    let mut journal = record.journal().clone();
    let last_dispatch = journal
        .events
        .iter_mut()
        .rev()
        .find(|e| matches!(e, TraceEvent::Dispatch { .. }))
        .unwrap();
    if let TraceEvent::Dispatch { at_ns, .. } = last_dispatch {
        *at_ns = 11;
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let mut replay = Simulator::replay(journal).unwrap();
    let id = replay.spawn(owner(42, &drops)).unwrap();
    let token = replay.next_task().unwrap().unwrap();
    assert_eq!(replay.sleep(token, 10).unwrap(), timer);
    assert!(matches!(
        replay.next_task(),
        Err(Error::ReplayMismatch { .. })
    ));
    assert_eq!(replay.now_ns(), 0);
    assert_eq!(replay.pending_count(), 1);
    assert_eq!(
        replay.task_state(id),
        Some(TaskState::Sleeping {
            completion: timer,
            deadline_ns: 10
        })
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    // Repeated rejected transitions neither consume the trace nor release data.
    assert!(matches!(
        replay.next_task(),
        Err(Error::ReplayMismatch { .. })
    ));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(replay);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn rejected_replay_transition_and_spawn_return_owned_inputs() {
    let mut record = Simulator::new(0);
    record.spawn(()).unwrap();
    let token = record.next_task().unwrap().unwrap();
    record.finish(token).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut replay = Simulator::replay(record.journal().clone()).unwrap();
    replay.spawn(owner(1, &drops)).unwrap();
    let token = replay.next_task().unwrap().unwrap();
    let error = replay.yield_task(token).unwrap_err();
    assert!(matches!(error.error, Error::ReplayMismatch { .. }));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(replay.finish(error.token).unwrap());
    replay.finish_replay().unwrap();
    let error = replay.spawn(owner(2, &drops)).unwrap_err();
    assert!(matches!(
        error.error,
        Error::ReplayMismatch { expected: None, .. }
    ));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    drop(error.payload);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}

#[test]
fn journal_versions_unknown_fields_and_unconsumed_suffix_are_rejected() {
    let mut sim = Simulator::new(0);
    sim.spawn(()).unwrap();
    let journal = sim.journal().clone();
    let replay = Simulator::<()>::replay(journal.clone()).unwrap();
    assert_eq!(
        replay.finish_replay(),
        Err(Error::ReplayUnconsumed { remaining: 1 })
    );
    let mut unsupported = journal;
    unsupported.version = 2;
    assert!(matches!(
        Simulator::<()>::replay(unsupported),
        Err(Error::UnsupportedVersion(2))
    ));
    assert!(matches!(
        Journal::from_json(r#"{"version":1,"seed":0,"events":[],"new_field":1}"#),
        Err(Error::InvalidJournal(_))
    ));
    assert!(matches!(
        Journal::from_json("not JSON"),
        Err(Error::InvalidJournal(_))
    ));
}

#[test]
fn running_task_blocks_virtual_time_progress() {
    let mut sim = Simulator::new(0);
    let id = sim.spawn(()).unwrap();
    let token = sim.next_task().unwrap().unwrap();
    assert_eq!(sim.next_task().err(), Some(Error::TaskStillRunning(id)));
    assert_eq!(sim.now_ns(), 0);
    sim.finish(token).unwrap();
    assert_eq!(sim.request_cancel(id), Err(Error::TaskFinished(id)));
}

#[test]
fn cancelled_io_failure_replays_without_early_release() {
    fn run(sim: &mut Simulator<()>) {
        let task = sim.spawn(()).unwrap();
        let token = sim.next_task().unwrap().unwrap();
        let completion = sim.schedule_io(token, 50, IoResult::Cancelled).unwrap();
        sim.request_cancel(task).unwrap();
        let token = sim.next_task().unwrap().unwrap();
        assert_eq!(sim.now_ns(), 50);
        assert_eq!(
            token.reason(),
            WakeReason::Io {
                completion,
                result: IoResult::Cancelled
            }
        );
        assert!(token.cancel_requested());
        sim.finish(token).unwrap();
    }
    let mut recording = Simulator::new(321);
    run(&mut recording);
    let mut replay = Simulator::replay(recording.journal().clone()).unwrap();
    run(&mut replay);
    replay.finish_replay().unwrap();
    assert!(replay.is_complete());
}

#[test]
fn clock_overflow_returns_the_running_owner() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut sim = Simulator::new(0);
    sim.spawn(owner(42, &drops)).unwrap();
    let token = sim.next_task().unwrap().unwrap();
    sim.sleep(token, u64::MAX).unwrap();
    let token = sim.next_task().unwrap().unwrap();
    assert_eq!(sim.now_ns(), u64::MAX);
    let failed = sim.sleep(token, 1).unwrap_err();
    assert_eq!(failed.error, Error::ClockOverflow);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(sim.finish(failed.token).unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn simultaneous_due_completions_are_delivered_before_dispatch_in_id_order() {
    let mut sim = Simulator::new(42);
    for _ in 0..3 {
        sim.spawn(()).unwrap();
    }
    let mut ids = vec![];
    for _ in 0..3 {
        let token = sim.next_task().unwrap().unwrap();
        ids.push(
            sim.schedule_io(token, 10, IoResult::Success { bytes: 1 })
                .unwrap(),
        );
    }
    let token = sim.next_task().unwrap().unwrap();
    assert_eq!(sim.pending_count(), 0);
    let at_ten = sim
        .journal()
        .events
        .iter()
        .filter(|e| match e {
            TraceEvent::IoCompleted { at_ns, .. } | TraceEvent::Dispatch { at_ns, .. } => {
                *at_ns == 10
            }
            _ => false,
        })
        .collect::<Vec<_>>();
    assert_eq!(at_ten.len(), 4);
    for (event, expected) in at_ten.iter().take(3).zip(ids) {
        assert!(
            matches!(event,TraceEvent::IoCompleted { completion,.. } if *completion == expected)
        );
    }
    assert!(matches!(at_ten[3], TraceEvent::Dispatch { .. }));
    sim.finish(token).unwrap();
    while let Some(token) = sim.next_task().unwrap() {
        sim.finish(token).unwrap();
    }
    assert!(sim.is_complete());
}

#[test]
fn unknown_event_fields_are_not_silently_ignored() {
    assert!(matches!(
        Journal::from_json(
            r#"{"version":1,"seed":0,"events":[{"Spawn":{"task":1,"at_ns":0,"extra":42}}]}"#
        ),
        Err(Error::InvalidJournal(_))
    ));
}
