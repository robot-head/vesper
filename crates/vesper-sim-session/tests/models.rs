//! Checked source driven through the version 2 process, WAL and network
//! models. Every test runs real source on the reference evaluator under
//! SimPulse; none touches a real file, socket, clock, random source or sleep.
use serde_json::{Value as Json, json};
use vesper_sim_session::{
    CheckedProgram, LostState, ModelEvent, Outcome, Recording, RecoveredOwner, Report, RunResult,
    Scenario, Session, SessionError, SourceEvent, StaleSource,
    model::wal::{FRAME_HEADER_BYTES, TailDamage},
};

const KV: &str = include_str!("../fixtures/kv.ves");
const KV_SCENARIO: &str = include_str!("../fixtures/kv_ack_first.json");

fn program() -> CheckedProgram {
    CheckedProgram::new(KV).unwrap_or_else(|e| panic!("{e}"))
}
fn base() -> Json {
    serde_json::from_str(KV_SCENARIO).unwrap()
}
fn scenario(json: &Json) -> Scenario {
    Scenario::from_json(&json.to_string()).unwrap()
}
fn complete(result: RunResult<'_>) -> Report {
    match result {
        RunResult::Complete(report) => report,
        other => panic!("expected completion, got {other:?}"),
    }
}
fn record(program: &CheckedProgram, json: &Json) -> Report {
    complete(
        Session::record(program, scenario(json))
            .unwrap()
            .run(u64::MAX),
    )
}
fn interrupted<'w>(result: RunResult<'w>) -> (Session<'w>, SessionError) {
    match result {
        RunResult::Interrupted { session, error } => (session, error),
        other => panic!("expected an interruption, got {other:?}"),
    }
}
fn replay<'w>(program: &'w CheckedProgram, json: &Json, recording: Recording) -> RunResult<'w> {
    Session::replay(program, scenario(json), recording)
        .unwrap()
        .run(u64::MAX)
}
fn output<'r>(report: &'r Report, task: &str) -> Option<&'r Json> {
    report
        .outputs
        .iter()
        .find(|(t, _)| t == task)
        .map(|(_, v)| v)
}
/// The scenario with `server` running `entry`, crashing at `crash_ns` and
/// restarting into `audit` three nanoseconds later.
fn variant(entry: &str, crash_ns: u64) -> Json {
    let mut json = base();
    json["tasks"][1]["entry"] = json!(entry);
    json["model"]["faults"] = json!([
        { "at_ns": crash_ns, "fault": { "crash": { "process": "server" } } },
        { "at_ns": crash_ns + 3, "fault": { "restart": { "process": "server" } } }
    ]);
    json
}
/// The safety property: an acknowledged LSN is durable after restart.
fn acknowledged_but_lost(report: &Report) -> bool {
    let acked = output(report, "client").and_then(Json::as_u64).unwrap();
    let audit = report
        .outputs
        .iter()
        .find(|(t, _)| t.starts_with("audit@"))
        .map(|(_, v)| v["records"].as_u64().unwrap())
        .unwrap_or(0);
    acked != 0 && audit < acked
}

#[test]
fn acknowledging_before_flush_has_a_deterministic_crash_counterexample() {
    let program = program();
    let report = record(&program, &base());
    assert_eq!(output(&report, "client"), Some(&json!(1)));
    assert_eq!(
        output(&report, "audit@1"),
        Some(&json!({"records": 0, "first": 0, "last": 0}))
    );
    assert!(acknowledged_but_lost(&report));
    // The server is lost process state, parked on its in-flight flush; its
    // linear Lease was never destructured and the task has no output.
    assert_eq!(report.lost.len(), 1);
    assert_eq!(report.lost[0].task, "server");
    assert!(matches!(
        &report.lost[0].state,
        LostState::Parked { operation, .. } if operation == "flush"
    ));
    assert!(output(&report, "server").is_none());
    let trace = &report.recording.model_trace;
    assert!(trace.iter().any(|e| matches!(
        e,
        ModelEvent::WalCrash { applied, .. } if applied.in_flight_records == 1 && applied.persisted_records == 0
    )));
    // The flush completion scheduled before the crash arrives at 15 ns and
    // is rejected as stale rather than making the record durable.
    assert!(trace.iter().any(|e| matches!(
        e,
        ModelEvent::StaleRejected {
            source: StaleSource::FlushCompletion,
            epoch: 0,
            at_ns: 15,
            ..
        }
    )));
    // A crash is not source cancellation: no CancelRequested and no resume of
    // the lost task after the crash.
    assert!(
        !report
            .recording
            .trace
            .iter()
            .any(|e| matches!(e, SourceEvent::CancelRequested { .. }))
    );
    assert!(!report.recording.trace.iter().any(|e| matches!(
        e,
        SourceEvent::Resume { task, at_ns, .. } if task == "server" && *at_ns >= 12
    )));
    assert_eq!(report.recording.durable["kv"].bytes_hex, "");

    // Deterministic: same inputs give the same report; replay reproduces it.
    assert_eq!(record(&program, &base()), report);
    assert_eq!(
        complete(replay(&program, &base(), report.recording.clone())),
        report
    );
    let parsed = Recording::from_json(&report.recording.to_json()).unwrap();
    assert_eq!(parsed, report.recording);
}

#[test]
fn bounded_crash_point_search_separates_the_two_protocols() {
    // Re-execute from scratch for each crash time; no owner is cloned.
    let program = program();
    let search = |entry: &str| -> Vec<u64> {
        (0..=30)
            .filter(|&t| acknowledged_but_lost(&record(&program, &variant(entry, t))))
            .collect()
    };
    let unsafe_points = search("serve_ack_first");
    // Between the ack's send (5 ns) and the flush completion (15 ns).
    assert_eq!(unsafe_points, (5..15).collect::<Vec<_>>());
    assert!(search("serve_flush_first").is_empty());
    // The counterexample does not depend on the schedule seed.
    for seed in 0..6 {
        let mut json = variant("serve_ack_first", 12);
        json["seed"] = json!(seed);
        assert!(
            acknowledged_but_lost(&record(&program, &json)),
            "seed {seed}"
        );
    }
}

#[test]
fn flushing_before_acknowledging_retains_acknowledged_data() {
    let program = program();
    // Crash after the flush completed (15 ns) but while the ack is in flight.
    let report = record(&program, &variant("serve_flush_first", 17));
    assert_eq!(output(&report, "client"), Some(&json!(1)));
    assert_eq!(output(&report, "server"), Some(&json!(1)));
    assert_eq!(
        output(&report, "audit@1"),
        Some(&json!({"records": 1, "first": 42, "last": 0}))
    );
    // The server had finished, so the crash lost nothing.
    assert!(report.lost.is_empty());
    let durable = &report.recording.durable["kv"];
    assert_eq!(durable.bytes_hex.len() / 2, FRAME_HEADER_BYTES + 2);
    let snapshot = report.model.as_ref().unwrap();
    assert_eq!(snapshot.logs["kv"].recoverable.len(), 1);
    assert_eq!(snapshot.logs["kv"].recoverable[0].payload, "42");
    assert_eq!(snapshot.processes[1].epoch, 1);

    // Crash during the flush: the client is never acknowledged.
    let report = record(&program, &variant("serve_flush_first", 12));
    assert_eq!(output(&report, "client"), Some(&json!(0)));
    assert_eq!(output(&report, "audit@1").unwrap()["records"], json!(0));
}

#[test]
fn flushed_unflushed_and_torn_records_recover_per_the_policy() {
    let program = program();
    // Unflushed: the lazy server acknowledged and is waiting before its flush.
    let report = record(&program, &variant("serve_lazy", 12));
    assert_eq!(output(&report, "client"), Some(&json!(1)));
    assert_eq!(output(&report, "audit@1").unwrap()["records"], json!(0));
    assert!(report.recording.model_trace.iter().any(|e| matches!(
        e,
        ModelEvent::WalCrash { applied, .. } if applied.volatile_lost == 1 && applied.in_flight_records == 0
    )));
    // Its parked receive was released and the later timeout is stale.
    assert!(matches!(
        &report.lost[0].state,
        LostState::Parked { operation, .. } if operation == "recv"
    ));
    assert!(report.recording.model_trace.iter().any(|e| matches!(
        e,
        ModelEvent::WaitReleased { task, .. } if task == "server"
    )));
    assert!(report.recording.model_trace.iter().any(|e| matches!(
        e,
        ModelEvent::StaleRejected {
            source: StaleSource::RecvTimeout,
            at_ns: 55,
            ..
        }
    )));

    // A two-record flush interrupted with explicit crash choices.
    let batch = |choice: Json| {
        let mut json = variant("serve_batch", 12);
        json["model"]["faults"][0]["fault"]["crash"]["in_flight"] = json!({ "kv": choice });
        record(&program, &json)
    };
    let recovered = |report: &Report| {
        report
            .recording
            .model_trace
            .iter()
            .find_map(|e| match e {
                ModelEvent::WalRecovered {
                    task,
                    records,
                    discarded_bytes,
                    damage,
                    ..
                } if task.starts_with("audit@") => {
                    Some((*records, *discarded_bytes, damage.clone()))
                }
                _ => None,
            })
            .unwrap()
    };
    let report = batch(json!({ "persisted_records": 1, "torn_bytes": 7 }));
    assert_eq!(
        output(&report, "audit@1"),
        Some(&json!({"records": 1, "first": 42, "last": 0}))
    );
    assert_eq!(recovered(&report), (1, 7, Some(TailDamage::TornHeader)));
    // Recovery truncated the torn tail from the medium.
    assert_eq!(
        report.recording.durable["kv"].bytes_hex.len() / 2,
        FRAME_HEADER_BYTES + 2
    );
    // A header plus one payload byte of the first frame (22 bytes in all).
    let report = batch(json!({ "persisted_records": 0, "torn_bytes": 21 }));
    assert_eq!(recovered(&report), (0, 21, Some(TailDamage::TornPayload)));
    let report = batch(json!({ "persisted_records": 2, "torn_bytes": 0 }));
    assert_eq!(
        output(&report, "audit@1"),
        Some(&json!({"records": 2, "first": 42, "last": 43}))
    );
    assert_eq!(recovered(&report), (2, 0, None));
    // Requests beyond the in-flight data are clamped and recorded as applied.
    let report = batch(json!({ "persisted_records": 9, "torn_bytes": 9 }));
    assert!(report.recording.model_trace.iter().any(|e| matches!(
        e,
        ModelEvent::WalCrash { choice, applied, .. }
            if choice.persisted_records == 9 && applied.persisted_records == 2 && applied.torn_bytes == 0
    )));
    // Each variant replays exactly, durable bytes included.
    let json = {
        let mut json = variant("serve_batch", 12);
        json["model"]["faults"][0]["fault"]["crash"]["in_flight"] =
            json!({ "kv": { "persisted_records": 1, "torn_bytes": 7 } });
        json
    };
    let report = record(&program, &json);
    assert_eq!(
        complete(replay(&program, &json, report.recording.clone())),
        report
    );
}

fn partitioned() -> Json {
    let mut json = base();
    json["tasks"][0]["entry"] = json!("client_retry");
    json["tasks"][1]["entry"] = json!("serve_flush_first");
    json["model"]["faults"] = json!([
        { "at_ns": 2, "fault": { "partition": { "network": "lan", "a": 1, "b": 2 } } },
        { "at_ns": 50, "fault": { "heal": { "network": "lan", "a": 2, "b": 1 } } }
    ]);
    json
}

#[test]
fn partitions_heal_and_delayed_failures_replay_exactly() {
    let program = program();
    let json = partitioned();
    let report = record(&program, &json);
    // The first Put is dropped on arrival; the retry after the heal succeeds.
    assert_eq!(output(&report, "client"), Some(&json!(1)));
    assert_eq!(output(&report, "server"), Some(&json!(1)));
    let trace = &report.recording.model_trace;
    assert!(trace.iter().any(|e| matches!(
        e,
        ModelEvent::NetDropped {
            message: 1,
            at_ns: 5,
            reason: vesper_sim_session::model::net::DropReason::Partitioned,
            ..
        }
    )));
    assert!(trace.iter().any(|e| matches!(
        e,
        ModelEvent::Partitioned {
            changed: true,
            at_ns: 2,
            ..
        }
    )));
    assert!(trace.iter().any(|e| matches!(
        e,
        ModelEvent::Healed {
            changed: true,
            at_ns: 50,
            ..
        }
    )));
    assert_eq!(
        complete(replay(&program, &json, report.recording.clone())),
        report
    );

    // A flush failure is delivered after the flush delay; the record stays
    // volatile and the client is told no.
    let mut json = partitioned();
    json["model"]["failures"] =
        json!([{ "target": "kv", "operation": "flush", "occurrence": 1, "code": 5 }]);
    let report = record(&program, &json);
    assert_eq!(output(&report, "client"), Some(&json!(0)));
    assert_eq!(output(&report, "server"), Some(&json!(0)));
    let failed_at = report
        .recording
        .model_trace
        .iter()
        .find_map(|e| match e {
            ModelEvent::WalFlushFailed { code: 5, at_ns, .. } => Some(*at_ns),
            _ => None,
        })
        .unwrap();
    assert_eq!(failed_at, 115);
    assert!(report.recording.trace.iter().any(|e| matches!(
        e,
        SourceEvent::Resume { task, outcome: Some(Outcome::Error { code: 5 }), at_ns: 115, .. } if task == "server"
    )));
    let snapshot = report.model.as_ref().unwrap();
    assert_eq!(snapshot.logs["kv"].volatile_lsns, vec![1]);
    assert!(snapshot.logs["kv"].recoverable.is_empty());
    assert_eq!(
        complete(replay(&program, &json, report.recording.clone())),
        report
    );

    // Injected send failures and unknown destinations are typed errors.
    let mut json = base();
    json["model"]["faults"] = json!([]);
    json["model"]["failures"] =
        json!([{ "target": "lan", "operation": "send", "occurrence": 1, "code": 104 }]);
    let report = record(&program, &json);
    assert_eq!(output(&report, "client"), Some(&json!(0)));
    assert!(report.recording.model_trace.iter().any(|e| matches!(
        e,
        ModelEvent::InjectedFailure {
            code: 104,
            occurrence: 1,
            ..
        }
    )));
    let mut json = base();
    json["model"]["faults"] = json!([]);
    json["tasks"][0]["arguments"] = json!([9, 42]);
    let report = record(&program, &json);
    assert!(
        report
            .recording
            .model_trace
            .iter()
            .any(|e| matches!(e, ModelEvent::NetUnreachable { to: 9, .. }))
    );
    assert!(report.recording.trace.iter().any(|e| matches!(
        e,
        SourceEvent::Resume { task, response, .. } if task == "client" && *response == json!({"Err": 113})
    )));
}

#[test]
fn replay_rejects_changed_model_state_choices_epochs_and_durable_output() {
    let program = program();
    let mut json = variant("serve_batch", 12);
    json["model"]["faults"][0]["fault"]["crash"]["in_flight"] =
        json!({ "kv": { "persisted_records": 1, "torn_bytes": 7 } });
    let report = record(&program, &json);
    let model_mismatch = |forged: Recording| {
        let (session, error) = interrupted(replay(&program, &json, forged));
        assert!(
            matches!(error, SessionError::ModelTraceMismatch { .. }),
            "{error:?}"
        );
        // The checked-out owner of the mismatching step is retained.
        assert!(session.stalled_task().is_some());
    };
    let edit = |f: &dyn Fn(&mut ModelEvent) -> bool| {
        let mut forged = report.recording.clone();
        assert!(forged.model_trace.iter_mut().any(f), "no event edited");
        forged
    };
    // Changed crash choice.
    model_mismatch(edit(&|e| match e {
        ModelEvent::WalCrash { applied, .. } => {
            applied.persisted_records = 2;
            true
        }
        _ => false,
    }));
    // Changed epoch.
    model_mismatch(edit(&|e| match e {
        ModelEvent::Restart { epoch, .. } => {
            *epoch = 2;
            true
        }
        _ => false,
    }));
    // Changed durable state after a flush-free crash.
    model_mismatch(edit(&|e| match e {
        ModelEvent::WalCrash { durable_sha256, .. } => {
            *durable_sha256 = "0".repeat(64);
            true
        }
        _ => false,
    }));
    // Changed recovery result.
    model_mismatch(edit(&|e| match e {
        ModelEvent::WalRecovered { damage, task, .. } if task.starts_with("audit") => {
            *damage = None;
            true
        }
        _ => false,
    }));
    // A missing model suffix: the next transition has no recorded event.
    let mut forged = report.recording.clone();
    forged.model_trace.truncate(5);
    let (_, error) = interrupted(replay(&program, &json, forged));
    assert!(
        matches!(
            error,
            SessionError::ModelTraceMismatch { expected: None, .. }
        ),
        "{error:?}"
    );
    // An extra recorded model event.
    let mut forged = report.recording.clone();
    let extra = forged.model_trace[0].clone();
    forged.model_trace.push(extra);
    let (_, error) = interrupted(replay(&program, &json, forged));
    assert_eq!(error, SessionError::ModelTraceUnconsumed { remaining: 1 });
    // Changed final durable bytes, with every transition unchanged.
    let mut forged = report.recording.clone();
    forged.durable.get_mut("kv").unwrap().bytes_hex = "00".into();
    let (_, error) = interrupted(replay(&program, &json, forged));
    assert_eq!(error, SessionError::DurableMismatch { log: "kv".into() });
    // Changed lost-task record and outputs.
    let mut forged = report.recording.clone();
    forged.lost.clear();
    let (_, error) = interrupted(replay(&program, &json, forged));
    assert_eq!(error, SessionError::LostMismatch);
    let mut forged = report.recording.clone();
    forged.outputs.insert("audit@1".into(), json!(0));
    let (_, error) = interrupted(replay(&program, &json, forged));
    assert_eq!(
        error,
        SessionError::OutputMismatch {
            task: "audit@1".into()
        }
    );
    // Identity: model semantics and the scenario's crash choice.
    let mut forged = report.recording.clone();
    forged.identity.model_semantics = "vesper-sim-models-v0".into();
    match Session::replay(&program, scenario(&json), forged) {
        Err(SessionError::Identity(m)) => assert_eq!(m.field, "model_semantics"),
        other => panic!("expected identity rejection, got {:?}", other.err()),
    }
    let mut changed = json.clone();
    changed["model"]["faults"][0]["fault"]["crash"]["in_flight"]["kv"]["torn_bytes"] = json!(8);
    match Session::replay(&program, scenario(&changed), report.recording.clone()) {
        Err(SessionError::Identity(m)) => assert_eq!(m.field, "scenario_sha256"),
        other => panic!("expected identity rejection, got {:?}", other.err()),
    }
}

#[test]
fn budget_slicing_preserves_model_state_and_matches_uninterrupted_runs() {
    let program = program();
    for json in [base(), partitioned(), variant("serve_lazy", 12)] {
        let whole = record(&program, &json);
        let mut session = Session::record(&program, scenario(&json)).unwrap();
        let mut snapshots = 0;
        let report = loop {
            match session.run(1) {
                RunResult::Budget(next) => {
                    snapshots += usize::from(next.model_snapshot().is_some());
                    session = next;
                }
                other => break complete(other),
            }
        };
        assert!(snapshots > 10);
        assert_eq!(report, whole);
    }
}

#[test]
fn lost_owners_are_explicit_and_never_resumed() {
    let program = program();
    let mut session = Session::record(&program, scenario(&base())).unwrap();
    let mut owners = vec![];
    let report = loop {
        match session.run(1) {
            RunResult::Budget(mut next) => {
                owners.extend(next.take_lost_owners());
                session = next;
            }
            other => break complete(other),
        }
    };
    assert_eq!(owners.len(), 1);
    assert_eq!((owners[0].task.as_str(), owners[0].epoch), ("server", 0));
    let RecoveredOwner::Suspended(continuation) = &owners[0].owner else {
        panic!("the lost server was parked")
    };
    assert_eq!(continuation.request().operation, "flush");
    assert_eq!(report.lost.len(), 1);
}

#[test]
fn model_budgets_interrupt_before_retaining_data_and_keep_the_owner() {
    let program = program();
    // A record larger than the per-record bound.
    let mut json = base();
    json["model"]["logs"][0]["limits"]["max_record_bytes"] = json!(1);
    let (mut session, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert!(
        matches!(error, SessionError::ModelBudget { .. }),
        "{error:?}"
    );
    let snapshot = session.model_snapshot().unwrap();
    assert!(snapshot.logs["kv"].volatile_lsns.is_empty());
    let stalled = session.take_stalled().unwrap();
    let RecoveredOwner::Suspended(continuation) = stalled.owner else {
        panic!("the appending owner is retained")
    };
    assert_eq!(continuation.request().operation, "append");

    // Too many records in the log.
    let mut json = variant("serve_batch", 500);
    json["model"]["logs"][0]["limits"]["max_records"] = json!(1);
    let (_, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert!(
        matches!(&error, SessionError::ModelBudget { message, .. } if message.contains("LogFull")),
        "{error:?}"
    );

    // Oversized messages and a full inbox.
    let mut json = base();
    json["model"]["networks"][0]["max_message_bytes"] = json!(4);
    let (mut session, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert!(
        matches!(error, SessionError::ModelBudget { .. }),
        "{error:?}"
    );
    assert!(
        session.model_snapshot().unwrap().networks["lan"]
            .in_flight
            .is_empty()
    );
    assert!(matches!(
        session.take_stalled().unwrap().owner,
        RecoveredOwner::Suspended(_)
    ));

    // The event budget stops before the event is retained.
    let mut json = base();
    json["model"]["limits"]["max_events"] = json!(6);
    let (mut session, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert_eq!(error, SessionError::EventLimit);
    assert!(session.trace().len() + session.model_trace().len() <= 6);
    assert!(session.take_stalled().is_some());

    // Dispatch (work) and virtual-time limits apply to model timer tasks too.
    let mut json = base();
    json["limits"]["max_virtual_ns"] = json!(150);
    let (mut session, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert_eq!(error, SessionError::VirtualTimeLimit { at_ns: 200 });
    assert!(matches!(
        session.take_stalled().unwrap().owner,
        RecoveredOwner::Internal(_)
    ));
}

const HOST: &str = "
linear struct Ticket { id: u64 }
linear effect Host { fn ask(n: u64) -> u64; }
fn worker(n: u64) -> u64 eff [Host] {
    let ticket = Ticket { id: n };
    let answer = Host::ask(n);
    let Ticket { id } = ticket;
    answer
}";

fn host_scenario() -> Json {
    json!({
        "format": "vesper-sim-scenario", "version": 2, "seed": 1,
        "limits": { "max_operations_per_task": 8, "max_virtual_ns": 100, "max_dispatches": 64 },
        "runtime": { "fuel": 100000, "optimize": false },
        "bindings": [
            { "effect": "Host", "operation": "ask", "params": ["u64"], "result": "u64", "action": "external" }
        ],
        "tasks": [ { "name": "worker", "entry": "worker", "arguments": [7], "process": "p" } ],
        "model": {
            "limits": { "max_events": 256, "max_epochs": 1, "max_instances": 2 },
            "processes": [ { "name": "p", "restart": [ { "name": "rerun", "entry": "worker", "arguments": [8] } ] } ],
            "faults": [
                { "at_ns": 5, "fault": { "crash": { "process": "p" } } },
                { "at_ns": 6, "fault": { "restart": { "process": "p" } } }
            ]
        }
    })
}

fn quiescent(result: RunResult<'_>) -> Session<'_> {
    match result {
        RunResult::Quiescent(session) => session,
        other => panic!("expected quiescence, got {other:?}"),
    }
}

#[test]
fn stale_host_completions_are_rejected_without_losing_the_response() {
    let program = CheckedProgram::new(HOST).unwrap();
    let json = host_scenario();
    let mut session = quiescent(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    // The crash at 5 ns released the first request; the restarted worker
    // waits on a new one.
    let pending = session.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!((pending[0].task.as_str(), pending[0].epoch), ("rerun@1", 1));
    let fresh = pending[0].request;
    let old = fresh - 1;
    let rejected = session
        .complete_request(old, Outcome::Success { bytes: 1 }, json!(70), vec![])
        .unwrap_err();
    assert_eq!(
        rejected.error,
        SessionError::StaleCompletion {
            request: old,
            epoch: 0
        }
    );
    assert_eq!(rejected.response, json!(70));
    assert_eq!(
        session.complete_external("worker", Outcome::Success { bytes: 1 }, json!(70), vec![]),
        Err(SessionError::TaskLost {
            task: "worker".into()
        })
    );
    // An invalid response for the live request hands the response back too.
    let rejected = session
        .complete_request(fresh, Outcome::Success { bytes: 1 }, json!(-1), vec![])
        .unwrap_err();
    assert!(matches!(
        rejected.error,
        SessionError::InvalidResponse { .. }
    ));
    assert_eq!(rejected.response, json!(-1));
    session
        .complete_request(fresh, Outcome::Success { bytes: 1 }, json!(80), vec![])
        .unwrap();
    let report = complete(session.run(u64::MAX));
    assert_eq!(report.outputs, vec![("rerun@1".into(), json!(80))]);
    assert_eq!(report.lost[0].task, "worker");
    assert!(report.recording.model_trace.iter().any(|e| matches!(
        e,
        ModelEvent::StaleRejected {
            source: StaleSource::HostCompletion,
            epoch: 0,
            ..
        }
    )));

    // Replay requires the same stale completion attempt in the same place.
    let replay_with = |stale: bool| {
        let mut session = quiescent(
            Session::replay(&program, scenario(&json), report.recording.clone())
                .unwrap()
                .run(u64::MAX),
        );
        if stale {
            let _ = session.complete_request(old, Outcome::Success { bytes: 1 }, json!(70), vec![]);
        }
        session
            .complete_request(fresh, Outcome::Success { bytes: 1 }, json!(80), vec![])
            .unwrap();
        session.run(u64::MAX)
    };
    assert_eq!(complete(replay_with(true)), report);
    let (_, error) = interrupted(replay_with(false));
    assert_eq!(error, SessionError::ModelTraceUnconsumed { remaining: 1 });
}

#[test]
fn model_scenarios_are_validated_at_load() {
    let program = program();
    let reject = |json: Json| -> String {
        match Scenario::from_json(&json.to_string()) {
            Err(e) => e.to_string(),
            Ok(s) => match Session::record(&program, s) {
                Err(SessionError::Scenario(e)) => e.to_string(),
                Err(other) => panic!("unexpected {other:?}"),
                Ok(_) => panic!("scenario was accepted: {json}"),
            },
        }
    };
    let edit = |f: &dyn Fn(&mut Json)| {
        let mut json = base();
        f(&mut json);
        reject(json)
    };
    assert!(edit(&|j| j["version"] = json!(1)).contains("version 1 has no model"));
    assert!(
        edit(&|j| {
            j.as_object_mut().unwrap().remove("model");
        })
        .contains("requires a model")
    );
    assert!(edit(&|j| j["tasks"][0]["process"] = json!("nobody")).contains("unknown process"));
    assert!(
        edit(&|j| {
            j["tasks"][0].as_object_mut().unwrap().remove("process");
        })
        .contains("no process")
    );
    // A client task may not use the server's log.
    assert!(edit(&|j| j["tasks"][0]["entry"] = json!("audit")).contains("does not own"));
    assert!(
        edit(&|j| j["bindings"][1]["action"] = json!({"wal_append": {"log": "other"}}))
            .contains("unknown model target")
    );
    assert!(
        edit(&|j| j["bindings"][0]["action"] = json!({"wal_append": {"log": "kv"}}))
            .contains("incompatible")
    );
    assert!(
        edit(&|j| j["bindings"][3]["action"] = json!({"net_recv": {"network": "lan"}}))
            .contains("incompatible")
    );
    assert!(edit(&|j| j["model"]["faults"][1]["at_ns"] = json!(5)).contains("alternate"));
    assert!(
        edit(&|j| {
            j["model"]["faults"].as_array_mut().unwrap().remove(0);
        })
        .contains("alternate")
    );
    assert!(edit(&|j| j["model"]["faults"][0]["at_ns"] = json!(5000)).contains("max_virtual_ns"));
    assert!(edit(&|j| j["model"]["limits"]["max_epochs"] = json!(0)).contains("max_epochs"));
    assert!(edit(&|j| j["model"]["limits"]["max_instances"] = json!(2)).contains("instances"));
    assert!(edit(&|j| j["model"]["limits"]["max_events"] = json!(0)).contains("max_events"));
    assert!(
        edit(&|j| j["model"]["logs"][0]["limits"]["max_bytes"] = json!(1 << 30)).contains("limits")
    );
    assert!(edit(&|j| j["model"]["networks"][0]["max_inbox"] = json!(0)).contains("bounds"));
    assert!(
        edit(&|j| j["model"]["processes"][0]["endpoints"]["lan"] = json!(2)).contains("reused")
    );
    assert!(
        edit(
            &|j| j["model"]["faults"][0]["fault"]["crash"]["in_flight"] =
                json!({"nope": {"persisted_records": 1, "torn_bytes": 0}})
        )
        .contains("does not own")
    );
    assert!(
        edit(&|j| j["model"]["faults"][0] =
            json!({"at_ns": 1, "fault": {"partition": {"network": "lan", "a": 1, "b": 9}}}))
        .contains("unknown network or endpoint")
    );
    assert!(
        edit(&|j| j["model"]["failures"] =
            json!([{"target": "lan", "operation": "flush", "occurrence": 1, "code": 5}]))
        .contains("unknown")
    );
    assert!(
        edit(&|j| j["model"]["failures"] =
            json!([{"target": "kv", "operation": "flush", "occurrence": 0, "code": 5}]))
        .contains("occurrences")
    );
    assert!(edit(&|j| j["tasks"][0]["name"] = json!("a@b")).contains("'@'"));
    assert!(edit(&|j| j["model"]["extra"] = json!(1)).contains("unknown field"));
    // Model actions need version 2 even without other model fields.
    let mut json = base();
    json["version"] = json!(1);
    json.as_object_mut().unwrap().remove("model");
    for task in json["tasks"].as_array_mut().unwrap() {
        task.as_object_mut().unwrap().remove("process");
    }
    assert!(reject(json).contains("version 2"));
}
