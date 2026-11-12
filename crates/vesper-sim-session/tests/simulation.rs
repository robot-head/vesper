//! Real source programs, checked and executed on the reference evaluator under
//! SimPulse. No test sleeps, opens sockets or reads the real clock.
use serde_json::{Value as Json, json};
use vesper::runtime::{Execution, Runtime, Value};
use vesper_sim_session::{
    ArgumentWrite, CheckedProgram, Outcome, Recording, RecoveredOwner, Report, RunResult, Scenario,
    Session, SessionError, SourceEvent,
};
use vesper_simpulse::{IoResult, TraceEvent};

const PIPELINE: &str = include_str!("../fixtures/pipeline.ves");
const PIPELINE_SCENARIO: &str = include_str!("../fixtures/pipeline.json");

fn program(source: &str) -> CheckedProgram {
    CheckedProgram::new(source).unwrap_or_else(|e| panic!("{e}"))
}
fn scenario_json() -> Json {
    serde_json::from_str(PIPELINE_SCENARIO).unwrap()
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
fn replay<'w>(
    program: &'w CheckedProgram,
    json: &Json,
    recording: Recording,
) -> Result<RunResult<'w>, SessionError> {
    Ok(Session::replay(program, scenario(json), recording)?.run(u64::MAX))
}

#[test]
fn two_tasks_interleave_virtual_delays_and_scripted_io() {
    let program = program(PIPELINE);
    let report = record(&program, &scenario_json());
    assert_eq!(
        report.outputs,
        vec![
            (
                "alpha".into(),
                json!({"ticket": 1, "started": 0, "woke": 5, "received": 1234, "finished": 14, "cancelled": false})
            ),
            (
                "beta".into(),
                json!({"ticket": 2, "started": 0, "woke": 3, "received": 0, "finished": 8, "cancelled": false})
            ),
        ]
    );
    assert_eq!(report.virtual_ns, 14);
    // Both tasks are parked at once and their scripted I/O interleaves.
    let order: Vec<(&str, u64)> = report
        .recording
        .trace
        .iter()
        .filter_map(|e| match e {
            SourceEvent::Resume {
                task,
                at_ns,
                outcome: Some(_),
                ..
            } => Some((task.as_str(), *at_ns)),
            _ => None,
        })
        .collect();
    assert_eq!(
        order,
        vec![("beta", 4), ("beta", 8), ("alpha", 12), ("alpha", 14)]
    );
    // The inout write reached the caller's place before resumption.
    assert!(report.recording.trace.iter().any(|e| matches!(
        e,
        SourceEvent::Resume { task, writes, .. } if task == "alpha" && writes == &vec![(0, json!(1234))]
    )));
    assert!(report.recording.journal.events.iter().any(|e| matches!(
        e,
        TraceEvent::ClockAdvance {
            from_ns: 5,
            to_ns: 8
        }
    )));
}

#[test]
fn same_inputs_reproduce_and_replay_matches() {
    let program = program(PIPELINE);
    let first = record(&program, &scenario_json());
    let second = record(&program, &scenario_json());
    assert_eq!(first, second);
    let replayed = complete(replay(&program, &scenario_json(), first.recording.clone()).unwrap());
    assert_eq!(replayed, first);
    // A recording survives JSON serialization.
    let parsed = Recording::from_json(&first.recording.to_json()).unwrap();
    assert_eq!(parsed, first.recording);
}

#[test]
fn seeds_change_the_schedule_but_not_the_checked_results() {
    let program = program(PIPELINE);
    let base = record(&program, &scenario_json());
    let mut differs = false;
    for seed in 0..16 {
        let mut json = scenario_json();
        json["seed"] = json!(seed);
        let report = record(&program, &json);
        assert_eq!(report.outputs, base.outputs);
        differs |= report.recording.trace != base.recording.trace;
    }
    assert!(
        differs,
        "some seed should dispatch the tasks in another order"
    );
}

#[test]
fn budget_exits_retain_every_owner_and_resume_identically() {
    let program = program(PIPELINE);
    let whole = record(&program, &scenario_json());
    let mut session = Session::record(&program, scenario(&scenario_json())).unwrap();
    let mut exits = 0;
    let report = loop {
        match session.run(1) {
            RunResult::Budget(next) => {
                exits += 1;
                session = next;
            }
            other => break complete(other),
        }
    };
    assert!(exits > 4);
    assert_eq!(report, whole);
}

#[test]
fn dispatch_and_virtual_time_limits_interrupt_with_owners_retained() {
    let program = program(PIPELINE);
    let mut json = scenario_json();
    json["limits"]["max_dispatches"] = json!(3);
    let (session, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert_eq!(error, SessionError::DispatchLimit);
    assert!(!session.pending().is_empty());
    assert!(session.stalled_task().is_none());

    let mut json = scenario_json();
    json["limits"]["max_virtual_ns"] = json!(6);
    let (mut session, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert_eq!(error, SessionError::VirtualTimeLimit { at_ns: 8 });
    let stalled = session.take_stalled().expect("the woken owner is retained");
    assert_eq!(stalled.task, "beta");
    let RecoveredOwner::Suspended(continuation) = stalled.owner else {
        panic!("beta was parked on fill")
    };
    assert_eq!(continuation.request().operation, "fill");
    // A retained session refuses to continue rather than inventing progress.
    let (_, again) = interrupted(session.run(10));
    assert_eq!(again, SessionError::Interrupted);
}

/// Drive a recovered continuation directly with fixed host answers.
fn finish_by_hand(mut execution: Execution<'_>) -> Value {
    loop {
        match execution {
            Execution::Complete(value) => return value,
            Execution::Suspended(mut pending) => {
                let response = match pending.request().operation.as_str() {
                    "now" => Value::UInt(99, 64),
                    "cancelled" => Value::Bool(false),
                    "fill" => {
                        pending.write_argument(0, Value::UInt(7, 64)).unwrap();
                        Value::Bool(true)
                    }
                    _ => Value::Tuple(vec![]),
                };
                execution = pending.resume(response).unwrap();
            }
        }
    }
}

#[test]
fn script_mismatches_stop_before_resuming_and_return_a_usable_owner() {
    let program = program(PIPELINE);
    // Reordered completions: alpha's script lists fill before recv.
    let mut json = scenario_json();
    let script = json["tasks"][0]["script"].as_array_mut().unwrap();
    script.swap(0, 1);
    let (mut session, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert!(
        matches!(&error, SessionError::ScriptMismatch { task, index: 0, .. } if task == "alpha"),
        "{error:?}"
    );
    let stalled = session.take_stalled().unwrap();
    let RecoveredOwner::Suspended(continuation) = stalled.owner else {
        panic!("expected the routed continuation")
    };
    assert_eq!(continuation.request().operation, "recv");
    let ok = Value::Data {
        ty: "Result".into(),
        constructor: "Ok".into(),
        fields: [("0".into(), Value::UInt(5, 64))].into(),
    };
    let value = finish_by_hand(continuation.resume(ok).unwrap());
    assert!(value.to_string().contains("Report"), "{value}");

    // Changed arguments.
    let mut json = scenario_json();
    json["tasks"][1]["script"][0]["arguments"] = json!([82]);
    let (_, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert!(
        matches!(error, SessionError::ScriptMismatch { .. }),
        "{error:?}"
    );

    // Missing completion.
    let mut json = scenario_json();
    json["tasks"][1]["script"].as_array_mut().unwrap().pop();
    let (mut session, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert!(
        matches!(error, SessionError::MissingScript { .. }),
        "{error:?}"
    );
    assert!(matches!(
        session.take_stalled().unwrap().owner,
        RecoveredOwner::Suspended(_)
    ));

    // Extra completion.
    let mut json = scenario_json();
    let extra = json["tasks"][1]["script"][1].clone();
    json["tasks"][1]["script"]
        .as_array_mut()
        .unwrap()
        .push(extra);
    let (_, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert_eq!(
        error,
        SessionError::UnconsumedScript {
            task: "beta".into(),
            remaining: 1
        }
    );
}

#[test]
fn changed_programs_scenarios_and_identity_are_rejected_before_spawning() {
    let program = program(PIPELINE);
    let report = record(&program, &scenario_json());
    let identity_error = |result: Result<RunResult<'_>, SessionError>| match result {
        Err(SessionError::Identity(mismatch)) => mismatch.field,
        Err(other) => panic!("expected identity mismatch, got {other:?}"),
        Ok(other) => panic!("expected identity mismatch, got {other:?}"),
    };

    let changed = program_with_comment();
    assert_eq!(
        identity_error(replay(&changed, &scenario_json(), report.recording.clone())),
        "source_sha256"
    );

    let mut json = scenario_json();
    json["tasks"][0]["script"][0]["response"] = json!({"Ok": 98});
    assert_eq!(
        identity_error(replay(&program, &json, report.recording.clone())),
        "scenario_sha256"
    );
    let mut json = scenario_json();
    json["runtime"]["fuel"] = json!(999_999);
    assert_eq!(
        identity_error(replay(&program, &json, report.recording.clone())),
        "scenario_sha256"
    );

    let mut forged = report.recording.clone();
    forged.identity.executable_sha256 = "0".repeat(64);
    assert_eq!(
        identity_error(replay(&program, &scenario_json(), forged)),
        "executable_sha256"
    );
    let mut forged = report.recording.clone();
    forged.identity.semantics = "vesper 0.1.0".into();
    assert_eq!(
        identity_error(replay(&program, &scenario_json(), forged)),
        "semantics"
    );
    let mut forged = report.recording.clone();
    forged.identity.bindings_sha256 = "0".repeat(64);
    assert_eq!(
        identity_error(replay(&program, &scenario_json(), forged)),
        "bindings_sha256"
    );
    let mut forged = report.recording.clone();
    forged.identity.options_sha256 = "0".repeat(64);
    assert_eq!(
        identity_error(replay(&program, &scenario_json(), forged)),
        "options_sha256"
    );
    // The executable digest is that of this test binary.
    assert_eq!(
        report.recording.identity.executable_sha256,
        vesper_sim_session::executable_sha256().unwrap()
    );
    assert_eq!(report.recording.identity.executable_sha256.len(), 64);
}

fn program_with_comment() -> CheckedProgram {
    program(&format!("{PIPELINE}\n// changed\n"))
}

#[test]
fn altered_journals_and_trace_data_are_explicit_mismatches() {
    let program = program(PIPELINE);
    let report = record(&program, &scenario_json());
    let events = &report.recording.journal.events;

    // Altered completion result in the SimPulse journal.
    let mut forged = report.recording.clone();
    let index = events
        .iter()
        .position(|e| matches!(e, TraceEvent::SubmitIo { .. }))
        .unwrap();
    if let TraceEvent::SubmitIo { scheduled, .. } = &mut forged.journal.events[index] {
        *scheduled = scheduled.map(|(t, _)| (t, IoResult::Error { code: 5 }));
    }
    let (session, error) = interrupted(replay(&program, &scenario_json(), forged).unwrap());
    assert!(
        matches!(
            error,
            SessionError::Simulator(vesper_simpulse::Error::ReplayMismatch { .. })
        ),
        "{error:?}"
    );
    assert!(
        session.stalled_task().is_some(),
        "the submitting owner is retained"
    );

    // Reordered completions.
    let mut forged = report.recording.clone();
    let completions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, TraceEvent::IoCompleted { .. }))
        .map(|(i, _)| i)
        .collect();
    forged.journal.events.swap(completions[0], completions[1]);
    let (session, error) = interrupted(replay(&program, &scenario_json(), forged).unwrap());
    assert!(matches!(error, SessionError::Simulator(_)), "{error:?}");
    assert!(
        !session.pending().is_empty(),
        "parked owners stay in the simulator"
    );

    // Missing suffix and extra events.
    let mut forged = report.recording.clone();
    forged.journal.events.truncate(events.len() - 3);
    let (_, error) = interrupted(replay(&program, &scenario_json(), forged).unwrap());
    assert!(matches!(error, SessionError::Simulator(_)), "{error:?}");
    let mut forged = report.recording.clone();
    let duplicate = forged.journal.events[completions[0]].clone();
    forged.journal.events.push(duplicate);
    let (_, error) = interrupted(replay(&program, &scenario_json(), forged).unwrap());
    assert_eq!(
        error,
        SessionError::Simulator(vesper_simpulse::Error::ReplayUnconsumed { remaining: 1 })
    );

    // Altered response and error data in the source trace.
    for (needle, replacement) in [
        (json!({"Ok": 99}), json!({"Ok": 100})),
        (json!({"Err": 111}), json!({"Err": 110})),
    ] {
        let mut forged = report.recording.clone();
        for event in &mut forged.trace {
            if let SourceEvent::Resume { response, .. } = event
                && *response == needle
            {
                *response = replacement.clone();
            }
        }
        let (mut session, error) = interrupted(replay(&program, &scenario_json(), forged).unwrap());
        assert!(
            matches!(error, SessionError::TraceMismatch { .. }),
            "{error:?}"
        );
        assert!(matches!(
            session.take_stalled().unwrap().owner,
            RecoveredOwner::Suspended(_)
        ));
    }
    let mut forged = report.recording.clone();
    forged.trace.pop();
    let (_, error) = interrupted(replay(&program, &scenario_json(), forged).unwrap());
    assert!(
        matches!(error, SessionError::TraceMismatch { expected: None, .. }),
        "{error:?}"
    );
    let mut forged = report.recording.clone();
    forged.outputs.insert("alpha".into(), json!(0));
    let (_, error) = interrupted(replay(&program, &scenario_json(), forged).unwrap());
    assert_eq!(
        error,
        SessionError::OutputMismatch {
            task: "alpha".into()
        }
    );
}

#[test]
fn acknowledged_cancellation_keeps_the_request_until_its_completion() {
    let program = program(PIPELINE);
    let mut json = scenario_json();
    json["tasks"][0]["cancel"] = json!({"after_submissions": 1});
    json["tasks"][0]["script"][0] = json!({
        "effect": "Net", "operation": "recv", "arguments": [80], "delay_ns": 50,
        "outcome": "cancelled", "response": {"Err": 125}
    });
    let mut session = Session::record(&program, scenario(&json)).unwrap();
    // Step until the cancellation is acknowledged but before the completion.
    loop {
        session = match session.run(1) {
            RunResult::Budget(session) => session,
            other => panic!("unexpected {other:?}"),
        };
        if session
            .trace()
            .iter()
            .any(|e| matches!(e, SourceEvent::CancelRequested { .. }))
        {
            break;
        }
    }
    let pending = session.pending();
    let recv = pending.iter().find(|p| p.task == "alpha").unwrap();
    assert_eq!(
        (recv.operation.as_str(), &recv.arguments),
        ("recv", &json!([80]))
    );
    assert!(session.now_ns() < 55);
    let report = complete(session.run(u64::MAX));
    assert_eq!(
        report.outputs[0].1,
        json!({"ticket": 1, "started": 0, "woke": 5, "received": 1234, "finished": 57, "cancelled": true})
    );
    assert!(report.recording.trace.iter().any(|e| matches!(
        e,
        SourceEvent::Resume { task, at_ns: 55, outcome: Some(Outcome::Cancelled), .. } if task == "alpha"
    )));
    let replayed = complete(replay(&program, &json, report.recording.clone()).unwrap());
    assert_eq!(replayed, report);

    // A cancelled completion without a cancellation request is a policy error.
    json["tasks"][0]["cancel"] = Json::Null;
    let (mut session, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert_eq!(
        error,
        SessionError::CancelledWithoutRequest {
            task: "alpha".into()
        }
    );
    assert!(matches!(
        session.take_stalled().unwrap().owner,
        RecoveredOwner::Suspended(_)
    ));
}

const EXTERNAL: &str = "
linear struct Ticket { id: u64 }
linear effect Net { fn recv(port: u64) -> Result[u64, u32]; }
linear effect Sched { fn cancelled() -> bool; }
fn got(r: Result[u64, u32]) -> u64 { match r { Ok(n) => n, Err(_) => 0 } }
fn main(port: u64) -> (u64, u64, bool) eff [Net, Sched] {
    let ticket = Ticket { id: port };
    let n = got(Net::recv(port));
    let c = Sched::cancelled();
    let Ticket { id } = ticket;
    (id, n, c)
}";

fn external_scenario() -> Json {
    json!({
        "format": "vesper-sim-scenario", "version": 1, "seed": 3,
        "limits": { "max_operations_per_task": 8, "max_virtual_ns": 100, "max_dispatches": 16 },
        "runtime": { "fuel": 100000, "optimize": false },
        "bindings": [
            { "effect": "Net", "operation": "recv", "params": ["u64"], "result": "Result[u64, u32]", "action": "external" },
            { "effect": "Sched", "operation": "cancelled", "params": [], "result": "bool", "action": "cancel_requested" }
        ],
        "tasks": [ { "name": "only", "entry": "main", "arguments": [9] } ]
    })
}

#[test]
fn host_completions_are_validated_once_before_resuming() {
    let program = program(EXTERNAL);
    let json = external_scenario();
    let mut session = match Session::record(&program, scenario(&json))
        .unwrap()
        .run(u64::MAX)
    {
        RunResult::Quiescent(session) => session,
        other => panic!("expected quiescence, got {other:?}"),
    };
    assert_eq!(session.pending()[0].arguments, json!([9]));
    let ok = |s: &mut Session<'_>, r: Json| {
        s.complete_external("only", Outcome::Success { bytes: 1 }, r, vec![])
    };
    assert!(matches!(
        ok(&mut session, json!({"Ok": -1})),
        Err(SessionError::InvalidResponse { .. })
    ));
    assert!(matches!(
        ok(&mut session, json!({"Maybe": 1})),
        Err(SessionError::InvalidResponse { .. })
    ));
    assert!(matches!(
        session.complete_external(
            "only",
            Outcome::Success { bytes: 1 },
            json!({"Ok": 1}),
            vec![ArgumentWrite {
                argument: 0,
                value: json!(1)
            }]
        ),
        Err(SessionError::InvalidResponse { .. })
    ));
    assert_eq!(
        session.complete_external("only", Outcome::Cancelled, json!({"Err": 125}), vec![]),
        Err(SessionError::CancelledWithoutRequest {
            task: "only".into()
        })
    );
    assert_eq!(
        session.complete_external("other", Outcome::Cancelled, json!(null), vec![]),
        Err(SessionError::UnknownTask("other".into()))
    );
    ok(&mut session, json!({"Ok": 31})).unwrap();
    assert_eq!(
        ok(&mut session, json!({"Ok": 31})),
        Err(SessionError::DuplicateCompletion {
            task: "only".into()
        })
    );
    let report = complete(session.run(u64::MAX));
    assert_eq!(report.outputs[0].1, json!([9, 31, false]));

    // Replay requires the host to deliver the same completion again.
    let replay_with = |response: Json| {
        let session = Session::replay(&program, scenario(&json), report.recording.clone()).unwrap();
        let RunResult::Quiescent(mut session) = session.run(u64::MAX) else {
            panic!("replay should wait for the host")
        };
        ok(&mut session, response).unwrap();
        session.run(u64::MAX)
    };
    assert_eq!(complete(replay_with(json!({"Ok": 31}))), report);
    let (_, error) = interrupted(replay_with(json!({"Ok": 32})));
    assert!(
        matches!(error, SessionError::TraceMismatch { .. }),
        "{error:?}"
    );
}

#[test]
fn interpreter_failures_after_consuming_transitions_are_terminal() {
    let program = program(
        "linear effect Clock { fn now() -> u64; }
         fn spin(n: u64) -> u64 { spin(n) }
         fn eager() -> u64 eff [Clock] { spin(1) }
         fn late() -> u64 eff [Clock] { let t = Clock::now(); spin(t) }",
    );
    let base = |entry: &str| {
        json!({
            "format": "vesper-sim-scenario", "version": 1, "seed": 1,
            "limits": { "max_operations_per_task": 8, "max_virtual_ns": 100, "max_dispatches": 16 },
            "runtime": { "fuel": 2000, "optimize": false },
            "bindings": [ { "effect": "Clock", "operation": "now", "params": [], "result": "u64", "action": "clock" } ],
            "tasks": [ { "name": "t", "entry": entry, "arguments": [] } ]
        })
    };
    for entry in ["eager", "late"] {
        match Session::record(&program, scenario(&base(entry)))
            .unwrap()
            .run(u64::MAX)
        {
            RunResult::Terminal(failure) => {
                assert_eq!(failure.task, "t");
                assert!(failure.message.contains("fuel"), "{}", failure.message);
                let resumed = failure
                    .trace
                    .iter()
                    .any(|e| matches!(e, SourceEvent::Resume { .. }));
                assert_eq!(resumed, entry == "late");
            }
            other => panic!("expected a terminal failure, got {other:?}"),
        }
    }
}

const NESTED: &str = "
copy struct Triple { asked: u64, slot: u64, after: u64 }
linear effect Ask { fn ask() -> u64; }
linear effect Net { fn recv(port: u64) -> Result[u64, u32]; fn fill(inout slot: u64) -> bool; }
fn got(r: Result[u64, u32]) -> u64 { match r { Ok(n) => n, Err(_) => 0 } }
// Handler captures are snapshots, so the non-tail clause returns its later
// external result instead of mutating an outer variable.
fn program() -> Triple eff [Net] {
    handle Ask in {
        let asked = Ask::ask();
        let slot: u64 = 5;
        let filled = Net::fill(inout slot);
        let kept = if filled { slot } else { asked };
        Triple { asked: asked, slot: kept, after: 0 }
    } with {
        return t => t,
        ask(k) => {
            let n = got(Net::recv(1));
            let r = k(n);
            let later = got(Net::recv(2));
            Triple { asked: r.asked, slot: r.slot, after: later }
        },
    }
}
fn direct() -> Triple {
    handle Net in { program() } with {
        return t => t,
        recv(port, k) => k(if port == 1 { Ok(40) } else { Ok(2) }),
        fill(slot, k) => { slot = 77; k(true) },
    }
}";

#[test]
fn nested_non_tail_handlers_and_inout_match_direct_evaluation() {
    let program = program(NESTED);
    let direct = Runtime::new(program.world()).run("direct", vec![]).unwrap();
    let json = json!({
        "format": "vesper-sim-scenario", "version": 1, "seed": 11,
        "limits": { "max_operations_per_task": 8, "max_virtual_ns": 100, "max_dispatches": 16 },
        "runtime": { "fuel": 100000, "optimize": true },
        "bindings": [
            { "effect": "Net", "operation": "recv", "params": ["u64"], "result": "Result[u64, u32]", "action": "script" },
            { "effect": "Net", "operation": "fill", "params": ["inout u64"], "result": "bool", "action": "script" }
        ],
        "tasks": [ { "name": "nested", "entry": "program", "arguments": [], "script": [
            { "effect": "Net", "operation": "recv", "arguments": [1], "delay_ns": 3, "outcome": { "success": { "bytes": 8 } }, "response": { "Ok": 40 } },
            { "effect": "Net", "operation": "fill", "arguments": [5], "delay_ns": 1, "outcome": { "success": { "bytes": 8 } }, "response": true, "writes": [ { "argument": 0, "value": 77 } ] },
            { "effect": "Net", "operation": "recv", "arguments": [2], "delay_ns": 2, "outcome": { "success": { "bytes": 8 } }, "response": { "Ok": 2 } }
        ] } ]
    });
    let report = record(&program, &json);
    let mut unoptimized = json.clone();
    unoptimized["runtime"]["optimize"] = json!(false);
    assert_eq!(record(&program, &unoptimized).outputs, report.outputs);
    assert_eq!(
        report.outputs[0].1,
        json!({"asked": 40, "slot": 77, "after": 2})
    );
    let direct_json = vesper_sim_session::value::encode(
        program.world(),
        &vesper::ast::Type::named("Triple"),
        &direct,
    )
    .unwrap();
    assert_eq!(report.outputs[0].1, direct_json);
    assert_eq!(report.virtual_ns, 6);
}

#[test]
fn malformed_scenarios_are_rejected_at_load() {
    let program = program(PIPELINE);
    let reject = |json: Json| -> String {
        match Scenario::from_json(&json.to_string()) {
            Err(e) => e.to_string(),
            Ok(s) => match Session::record(&program, s) {
                Err(SessionError::Scenario(e)) => e.to_string(),
                Err(other) => panic!("unexpected {other:?}"),
                Ok(_) => panic!("scenario was accepted"),
            },
        }
    };
    let edit = |f: &dyn Fn(&mut Json)| {
        let mut json = scenario_json();
        f(&mut json);
        reject(json)
    };
    assert!(edit(&|j| j["version"] = json!(2)).contains("version"));
    assert!(edit(&|j| j["format"] = json!("other")).contains("format"));
    assert!(edit(&|j| j["extra"] = json!(1)).contains("unknown field"));
    assert!(edit(&|j| j["bindings"][4]["params"] = json!(["u32"])).contains("checked signature"));
    assert!(edit(&|j| j["bindings"][4]["result"] = json!("u64")).contains("checked signature"));
    assert!(edit(&|j| j["bindings"][5]["params"] = json!(["u64"])).contains("checked signature"));
    assert!(
        edit(&|j| j["bindings"][0]["operation"] = json!("later")).contains("unknown operation")
    );
    assert!(edit(&|j| j["bindings"][0]["effect"] = json!("Disk")).contains("unknown effect"));
    assert!(
        edit(&|j| {
            j["bindings"].as_array_mut().unwrap().remove(3);
        })
        .contains("no binding")
    );
    assert!(
        edit(&|j| {
            let b = j["bindings"][0].clone();
            j["bindings"].as_array_mut().unwrap().push(b);
        })
        .contains("duplicate")
    );
    assert!(edit(&|j| j["bindings"][4]["action"] = json!("clock")).contains("incompatible"));
    assert!(
        edit(&|j| j["bindings"][1]["action"] = json!({"sleep": {"duration_argument": 1}}))
            .contains("incompatible")
    );
    assert!(edit(&|j| j["tasks"][0]["entry"] = json!("missing")).contains("unknown entry"));
    assert!(edit(&|j| j["tasks"][0]["arguments"] = json!([1, 2])).contains("arguments"));
    assert!(edit(&|j| j["tasks"][0]["arguments"][0] = json!(-1)).contains("expected"));
    assert!(edit(&|j| j["tasks"][1]["name"] = json!("alpha")).contains("unique"));
    assert!(
        edit(&|j| j["tasks"][0]["script"][0]["response"] = json!({"Ok": "x"})).contains("expected")
    );
    assert!(
        edit(&|j| j["tasks"][0]["script"][0]["response"] = json!({"Err": 4294967296u64}))
            .contains("out of range")
    );
    assert!(
        edit(&|j| j["tasks"][0]["script"][0]["response"] = json!({"Ok": 1, "Err": 1}))
            .contains("expected")
    );
    assert!(
        edit(&|j| j["tasks"][0]["script"][0]["writes"] = json!([{"argument": 0, "value": 1}]))
            .contains("inout")
    );
    assert!(
        edit(&|j| j["tasks"][0]["script"][1]["writes"] = json!([{"argument": 0, "value": true}]))
            .contains("expected")
    );
    assert!(
        edit(&|j| j["tasks"][0]["script"][0]["operation"] = json!("now"))
            .contains("not a scripted")
    );
    assert!(
        edit(&|j| j["tasks"][0]["cancel"] = json!({"after_submissions": 0})).contains("at least 1")
    );
    assert!(edit(&|j| j["runtime"]["fuel"] = json!(0)).contains("fuel"));
    assert!(edit(&|j| j["limits"]["max_dispatches"] = json!(0)).contains("max_dispatches"));
    assert!(edit(&|j| j["tasks"] = json!([])).contains("tasks"));
}

#[test]
fn unchecked_source_and_unsupported_entries_are_rejected() {
    assert!(CheckedProgram::new("fn main() -> u64 { true }").is_err());
    assert!(CheckedProgram::new("fn main( -> u64 { 1 }").is_err());
    let program = program(
        "effect Plain { fn get() -> u64; }
         fn main() -> u64 eff [Plain] { Plain::get() }",
    );
    let json = json!({
        "format": "vesper-sim-scenario", "version": 1, "seed": 1,
        "limits": { "max_operations_per_task": 8, "max_virtual_ns": 100, "max_dispatches": 16 },
        "runtime": { "fuel": 1000, "optimize": false },
        "bindings": [ { "effect": "Plain", "operation": "get", "params": [], "result": "u64", "action": "clock" } ],
        "tasks": [ { "name": "t", "entry": "main", "arguments": [] } ]
    });
    match Session::record(&program, scenario(&json)) {
        Err(SessionError::Scenario(e)) => assert!(e.to_string().contains("not linear"), "{e}"),
        other => panic!("expected rejection, got {:?}", other.err()),
    }
}

#[test]
fn operation_limits_interrupt_with_the_requesting_owner() {
    let program = program(PIPELINE);
    let mut json = scenario_json();
    json["limits"]["max_operations_per_task"] = json!(2);
    let (mut session, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert!(
        matches!(error, SessionError::OperationLimit { .. }),
        "{error:?}"
    );
    assert!(matches!(
        session.take_stalled().unwrap().owner,
        RecoveredOwner::Suspended(_)
    ));
}
