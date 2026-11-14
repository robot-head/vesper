//! Checked Vesper source compiled to native fibers that park on declared
//! linear external effects. Native results are compared with the reference
//! evaluator, which is used only as an oracle here.
#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
use std::{cell::RefCell, collections::VecDeque, fs::File, io, rc::Rc};
use vesper::{
    check,
    layout::{Layout, LayoutKind},
    native::session::*,
    parser,
    runtime::{Execution, Runtime, Value},
};
use vesper_fiber_abi::{
    abi::exit,
    fiber::{Outcome, Resumption},
    prototype::{OP_READ, OP_YIELD},
    pulse::{CancelPoint, Incomplete, NativeRuntime, NativeTask},
};
use vesper_pulse::{
    io_driver::Request,
    scheduler::{Driver, Finished, PollError},
};

/// A linear ticket is held across three parks (read, pause, read) and
/// consumed once; an aggregate inout tally and a read buffer are updated
/// through helper calls between the parks.
const PULSE: &str = include_str!("../examples/native-pulse/ticket_tally.ves");

/// Cancellation cleanup that parks: every path releases the linear lease
/// through `release`, which itself pauses (a cleanup park).
const LEASE: &str = include_str!("../examples/native-pulse/lease_cleanup.ves");

/// Typed arithmetic and runtime failures escape the entry; I/O errors are
/// ordinary source values.
const FAILING: &str = include_str!("../examples/native-pulse/typed_failures.ves");

fn world(source: &str) -> check::World {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    checked.world
}

fn pulse_bindings() -> Vec<EffectBinding> {
    vec![EffectBinding {
        effect: "Pulse".into(),
        ops: vec![
            OperationBinding::new(
                "pause",
                Operation::Yield,
                "Ticked",
                "TickFailed",
                Cancellation::Constructor("TickCancelled".into()),
            ),
            OperationBinding::new(
                "read",
                Operation::Read,
                "Got",
                "IoFailed",
                Cancellation::Constructor("IoCancelled".into()),
            ),
        ],
    }]
}

fn failing_bindings() -> Vec<EffectBinding> {
    vec![EffectBinding {
        effect: "Source".into(),
        ops: vec![OperationBinding::new(
            "read",
            Operation::Read,
            "Got",
            "IoFailed",
            Cancellation::ErrorCode(ECANCELED),
        )],
    }]
}

/// One host answer per park, in order.
#[derive(Clone, Debug)]
enum Answer {
    /// Yield: `Value(0)`. Read: deliver these bytes, `Value(len)`.
    Value(Vec<u8>),
    Error(u64),
    Cancel,
}

fn word(n: u64) -> Vec<u8> {
    n.to_le_bytes().to_vec()
}

fn words(ns: &[u64]) -> Vec<u8> {
    ns.iter().flat_map(|n| n.to_le_bytes()).collect()
}

/// Little-endian byte image of a word-buffer value.
fn encode(world: &check::World, ty: &vesper::ast::Type, value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::UInt(n, _) => out.extend_from_slice(&n.to_le_bytes()),
        Value::Int(n) => out.extend_from_slice(&n.to_le_bytes()),
        Value::Data {
            fields,
            resource_identity,
            ..
        } => {
            assert!(
                resource_identity.is_none(),
                "tracked identity cannot cross an uninstrumented native buffer"
            );
            let layout = Layout::of(world, ty).unwrap();
            let LayoutKind::Product(fs) = &layout.kind else {
                panic!("not a word buffer")
            };
            for f in fs {
                encode(world, &f.ty, &fields[&f.name], out);
            }
        }
        other => panic!("not a word buffer: {other}"),
    }
}

/// The reference evaluator, answering each operation from `script` the way
/// the binding defines: cancellation produces the cancellation constructor.
fn oracle(source: &str, script: &[Answer]) -> String {
    let world = world(source);
    let mut execution = Runtime::new(&world).begin_external("main", vec![]).unwrap();
    let mut script = script.iter();
    loop {
        match execution {
            Execution::Complete(value) => return value.to_string(),
            Execution::Suspended(mut k) => {
                let request = k.request();
                let operation = request.operation.clone();
                let answer = script.next().expect("oracle script too short").clone();
                let (ty, value, error, cancel) = match operation.as_str() {
                    "read" => ("Io", "Got", "IoFailed", "IoCancelled"),
                    "pause" => ("Tick", "Ticked", "TickFailed", "TickCancelled"),
                    other => panic!("unexpected operation {other}"),
                };
                let data = |constructor: &str, field: Option<Value>| Value::Data {
                    ty: ty.into(),
                    constructor: constructor.into(),
                    fields: field.into_iter().map(|v| ("0".to_string(), v)).collect(),
                    resource_identity: None,
                };
                // `IoFailed` carries u64 codes, `TickFailed` u32 codes.
                let error_bits = if operation == "read" { 64 } else { 32 };
                let resumed = match answer {
                    Answer::Value(bytes) if operation == "read" => {
                        let decl = &world.effects[&request.effect.name];
                        let param = decl.ops.iter().find(|f| f.name == "read").unwrap().params[0]
                            .ty
                            .clone();
                        let mut image = vec![];
                        encode(&world, &param, &request.arguments[0], &mut image);
                        image[..bytes.len()].copy_from_slice(&bytes);
                        let updated = vesper::layout::decode(&world, &param, &image).unwrap();
                        k.write_argument(0, updated).unwrap();
                        data(value, Some(Value::UInt(bytes.len() as u64, 64)))
                    }
                    Answer::Value(_) => data(value, None),
                    Answer::Error(code) => data(error, Some(Value::UInt(code, error_bits))),
                    Answer::Cancel => data(cancel, None),
                };
                execution = k.resume(resumed).unwrap();
            }
        }
    }
}

/// Drive a session fiber directly, relocating its data frame at every park
/// when `relocate` is set. Returns the exit and the observed park ops.
fn run(
    session: &NativeSession,
    script: &[Answer],
    relocate: bool,
) -> (vesper_fiber_abi::fiber::Exit, Vec<u32>) {
    let mut outcome = session.fiber().unwrap().start();
    let mut ops = vec![];
    let mut script = script.iter();
    loop {
        let mut k = match outcome {
            Outcome::Exited(e) => return (e, ops),
            Outcome::Parked(k) => k,
        };
        let request = k.request();
        ops.push(request.op);
        if relocate {
            let before = k.data_base();
            k.relocate_data();
            assert_ne!(before, k.data_base(), "relocation must move the frame");
        }
        let r = match script.next().expect("native script too short").clone() {
            Answer::Value(bytes) if request.op == OP_READ => {
                assert!(bytes.len() as u64 <= request.arg0);
                let at = request.arg1 as usize;
                k.data_mut()[at..at + bytes.len()].copy_from_slice(&bytes);
                Resumption::value(request.evidence, bytes.len() as u64)
            }
            Answer::Value(_) => Resumption::value(request.evidence, 0),
            Answer::Error(code) => Resumption::error(request.evidence, code),
            Answer::Cancel => Resumption::cancelled(request.evidence),
        };
        outcome = k.resume(r).unwrap();
    }
}

fn returned(
    world: &check::World,
    session: &NativeSession,
    exit: &vesper_fiber_abi::fiber::Exit,
) -> String {
    match session.decode_exit(world, exit).unwrap() {
        SessionResult::Returned(v) => v.to_string(),
        other => panic!("expected a returned value, got {other:?}"),
    }
}

#[test]
fn linear_ticket_aggregates_and_inout_places_agree_with_the_evaluator_across_parks() {
    let world = world(PULSE);
    let scripts: Vec<Vec<Answer>> = vec![
        // Full read, yield, short read: the short read keeps the old high word.
        vec![
            Answer::Value(words(&[40, 2])),
            Answer::Value(vec![]),
            Answer::Value(word(500)),
        ],
        // I/O error, failed yield, cancelled read.
        vec![Answer::Error(5), Answer::Error(9), Answer::Cancel],
        // Cancelled read, cancelled yield, I/O error.
        vec![Answer::Cancel, Answer::Cancel, Answer::Error(104)],
        // A three-byte read only changes the low bytes of `lo`.
        vec![
            Answer::Value(vec![0xff, 0xff, 0xff]),
            Answer::Value(vec![]),
            Answer::Value(words(&[1, 1])),
        ],
    ];
    for optimize in [true, false] {
        let session =
            NativeSession::compile_with_options(&world, "main", &pulse_bindings(), optimize)
                .unwrap();
        for script in &scripts {
            let expected = oracle(PULSE, script);
            for relocate in [false, true] {
                let (exit, ops) = run(&session, script, relocate);
                assert_eq!(exit.status, exit::COMPLETE);
                assert_eq!(ops, vec![OP_READ, OP_YIELD, OP_READ]);
                assert_eq!(exit.parks, 3);
                if relocate {
                    assert_eq!(exit.data_relocations, 3);
                }
                assert_eq!(
                    returned(&world, &session, &exit),
                    expected,
                    "script {script:?}, relocate {relocate}, optimize {optimize}"
                );
            }
        }
    }
    // Spot-check the oracle itself for the first script.
    let first = oracle(PULSE, &scripts[0]);
    assert!(first.contains("ticket: 207"), "{first}");
    assert!(first.contains("lo: 500"), "{first}");
    assert!(first.contains("hi: 2"), "{first}");
}

#[test]
fn session_plan_states_the_supported_frame_abi() {
    let world = world(PULSE);
    let plan = plan(&world, "main", &pulse_bindings()).unwrap();
    assert_eq!(plan.effects(), vec!["Pulse"]);
    let frame = plan.frame();
    // Summary is 9 words: ticket, paused, five tally words, two block words.
    assert_eq!(frame.result_offset, 0);
    assert_eq!(frame.result_size, 72);
    assert_eq!(frame.failure_size, 0);
    assert_eq!(frame.staging_offset, 72);
    assert_eq!(frame.staging_size, 16);
    assert_eq!(frame.bytes, 88);
    let session = NativeSession::compile(&world, "main", &pulse_bindings()).unwrap();
    let k = session.fiber().unwrap().start().parked().unwrap();
    let request = k.request();
    assert_eq!((request.op, request.arg0, request.arg1), (OP_READ, 16, 72));
    // The read staging area holds the buffer's current contents.
    assert_eq!(&k.data()[72..88], words(&[1, 2]).as_slice());
    assert!(session.clif().contains("vesper_session_main"));
    let _ = k.cancel();
}

#[test]
fn typed_arithmetic_and_runtime_failures_stay_distinct_from_io_errors() {
    let world = world(FAILING);
    let session = NativeSession::compile(&world, "main", &failing_bindings()).unwrap();
    assert_eq!(session.plan().failures().len(), 2);
    let decode = |script: &[Answer]| {
        let (exit, _) = run(&session, script, true);
        (exit.status, session.decode_exit(&world, &exit).unwrap())
    };
    // 40 + 40 = 80, read 8 bytes: 80 / 8.
    let (status, result) = decode(&[Answer::Value(word(40))]);
    assert_eq!(status, exit::COMPLETE);
    assert!(
        matches!(result, SessionResult::Returned(Value::UInt(10, 64))),
        "{result:?}"
    );
    // 2^63 + 2^63 overflows: Fail[ArithOverflow] is instance 0.
    let (status, result) = decode(&[Answer::Value(word(1 << 63))]);
    assert_eq!(status, exit::FAILED);
    match result {
        SessionResult::Failed { instance, error } => {
            assert_eq!(instance, 0);
            assert_eq!(error.to_string(), "ArithOverflow");
        }
        other => panic!("{other:?}"),
    }
    // A source-handled I/O error with code 0 divides by zero: instance 1.
    let (status, result) = decode(&[Answer::Error(0)]);
    assert_eq!(status, exit::FAILED);
    match result {
        SessionResult::Failed { instance, error } => {
            assert_eq!(instance, 1);
            assert_eq!(error.to_string(), "DivisionByZero");
        }
        other => panic!("{other:?}"),
    }
    // A nonzero I/O error is just a value: 0 / 5.
    let (_, result) = decode(&[Answer::Error(5)]);
    assert!(
        matches!(result, SessionResult::Returned(Value::UInt(0, 64))),
        "{result:?}"
    );
    // Cancellation binds to the error constructor with ECANCELED: 0 / 125.
    let (_, result) = decode(&[Answer::Cancel]);
    assert!(
        matches!(result, SessionResult::Returned(Value::UInt(0, 64))),
        "{result:?}"
    );
}

#[test]
fn cancellation_cleanup_reparks_and_repeated_cancellation_is_abortive() {
    let world = world(LEASE);
    let session = NativeSession::compile(&world, "main", &pulse_bindings_for_lease()).unwrap();
    // Cancel during the read: the source releases the lease, which parks on a
    // cleanup pause; answering it normally completes the cleanup.
    let k = session.fiber().unwrap().start().parked().unwrap();
    assert_eq!(k.request().op, OP_READ);
    let cleanup = k.cancel().parked().expect("cleanup must park");
    assert_eq!(cleanup.request().op, OP_YIELD);
    let evidence = cleanup.request().evidence;
    let exit = cleanup
        .resume(Resumption::value(evidence, 0))
        .unwrap()
        .exited()
        .unwrap();
    assert_eq!(returned(&world, &session, &exit), "6010");
    assert_eq!(
        oracle(LEASE, &[Answer::Cancel, Answer::Value(vec![])]),
        "6010"
    );
    // A second cancellation at the cleanup park is abortive: the pause
    // produces its cancellation result, and source still consumes the lease.
    let k = session.fiber().unwrap().start().parked().unwrap();
    let cleanup = k.cancel().parked().unwrap();
    let exit = cleanup.cancel().exited().expect("abortive cleanup exits");
    assert_eq!(exit.status, exit::COMPLETE);
    assert_eq!(exit.parks, 2);
    assert_eq!(returned(&world, &session, &exit), "6030");
    assert_eq!(oracle(LEASE, &[Answer::Cancel, Answer::Cancel]), "6030");
    // Before start nothing runs and nothing is owned yet.
    let exit = session.fiber().unwrap().cancel();
    assert!(matches!(
        session.decode_exit(&world, &exit).unwrap(),
        SessionResult::CancelledBeforeStart
    ));
}

fn pulse_bindings_for_lease() -> Vec<EffectBinding> {
    pulse_bindings()
}

/// Deterministic fixture driver. Requests complete in submission order from
/// a script; `Hold` keeps a request in flight until `release`. A cancel
/// command is only recorded: it releases no request.
#[derive(Clone, Debug)]
enum Reply {
    Bytes(Vec<u8>),
    Errno(i32),
    Hold,
}

#[derive(Default)]
struct Fixture {
    script: VecDeque<Reply>,
    inflight: VecDeque<Request<NativeTask>>,
    held: Vec<Request<NativeTask>>,
    release: Option<io::Result<usize>>,
    reject_submit: Option<i32>,
    log: Rc<RefCell<Vec<String>>>,
}

impl Fixture {
    fn new(script: Vec<Reply>) -> Self {
        Self {
            script: script.into(),
            ..Default::default()
        }
    }
    fn release(&mut self, result: io::Result<usize>) {
        self.release = Some(result);
    }
}

impl Driver<NativeTask> for Fixture {
    fn submit(
        &mut self,
        request: Request<NativeTask>,
    ) -> Result<(), (io::Error, Request<NativeTask>)> {
        let id = request.continuation.fiber_id();
        if let Some(code) = self.reject_submit {
            self.log.borrow_mut().push(format!("reject {id}"));
            return Err((io::Error::from_raw_os_error(code), request));
        }
        self.log.borrow_mut().push(format!("submit {id}"));
        self.inflight.push_back(request);
        Ok(())
    }
    fn poll(&mut self, _: bool) -> Result<Vec<Finished<NativeTask>>, PollError<NativeTask>> {
        let mut done = vec![];
        if let Some(result) = self.release.take() {
            for request in self.held.drain(..) {
                let id = request.continuation.fiber_id();
                self.log
                    .borrow_mut()
                    .push(format!("complete {id} {}", describe(&result)));
                let result = match &result {
                    Ok(n) => Ok(*n),
                    Err(e) => Err(io::Error::from_raw_os_error(e.raw_os_error().unwrap())),
                };
                done.push(Finished { request, result });
            }
        }
        while let Some(mut request) = self.inflight.pop_front() {
            let id = request.continuation.fiber_id();
            let result = match self.script.pop_front().unwrap_or(Reply::Hold) {
                Reply::Bytes(bytes) => {
                    let n = bytes.len().min(request.buffer.len());
                    request.buffer[..n].copy_from_slice(&bytes[..n]);
                    Ok(n)
                }
                Reply::Errno(code) => Err(io::Error::from_raw_os_error(code)),
                Reply::Hold => {
                    self.log.borrow_mut().push(format!("hold {id}"));
                    self.held.push(request);
                    continue;
                }
            };
            self.log
                .borrow_mut()
                .push(format!("complete {id} {}", describe(&result)));
            done.push(Finished { request, result });
        }
        Ok(done)
    }
    fn pending(&self) -> usize {
        self.inflight.len() + self.held.len()
    }
    fn cancel_all(&mut self) -> io::Result<()> {
        self.log.borrow_mut().push("cancel".into());
        Ok(())
    }
}

fn describe(result: &io::Result<usize>) -> String {
    match result {
        Ok(n) => format!("ok {n}"),
        Err(e) => format!("err {}", e.raw_os_error().unwrap()),
    }
}

fn zero() -> Option<File> {
    Some(File::open("/dev/zero").unwrap())
}

#[test]
fn runtime_runs_sessions_to_owned_shutdown_with_the_fixture_driver() {
    let world = world(PULSE);
    let session = NativeSession::compile(&world, "main", &pulse_bindings()).unwrap();
    let mut runtime = NativeRuntime::new(Fixture::new(vec![
        Reply::Bytes(words(&[40, 2])),
        Reply::Bytes(word(500)),
    ]));
    let id = session.spawn(&mut runtime, zero()).unwrap();
    let drained = runtime.drain(64, false).unwrap();
    assert!(drained.failures.is_empty(), "{:?}", drained.failures);
    assert_eq!(drained.records.len(), 1);
    assert_eq!(drained.records[0].fiber_id, id);
    assert_eq!(drained.records[0].status, exit::COMPLETE);
    assert_eq!(drained.records[0].parks, 3);
    let result = session
        .decode_completed(&world, &drained.completed[0])
        .unwrap();
    let SessionResult::Returned(value) = result else {
        panic!("{result:?}")
    };
    assert_eq!(
        value.to_string(),
        oracle(
            PULSE,
            &[
                Answer::Value(words(&[40, 2])),
                Answer::Value(vec![]),
                Answer::Value(word(500))
            ]
        )
    );
}

#[test]
fn runtime_cancellation_before_start_after_yield_and_during_io() {
    let world = world(PULSE);
    let session = NativeSession::compile(&world, "main", &pulse_bindings()).unwrap();

    // Before start: no generated code runs.
    let mut runtime = NativeRuntime::new(Fixture::new(vec![]));
    session.spawn(&mut runtime, zero()).unwrap();
    let drained = runtime.shutdown(8, false).unwrap();
    assert_eq!(drained.records[0].cancelled, Some(CancelPoint::BeforeStart));
    assert_eq!(drained.records[0].parks, 0);
    assert!(matches!(
        session
            .decode_completed(&world, &drained.completed[0])
            .unwrap(),
        SessionResult::CancelledBeforeStart
    ));

    // After yield: the pause observes TickCancelled; the second read still
    // runs because the source chose to continue.
    let mut runtime = NativeRuntime::new(Fixture::new(vec![
        Reply::Bytes(words(&[40, 2])),
        Reply::Bytes(words(&[3, 4])),
    ]));
    session.spawn(&mut runtime, zero()).unwrap();
    runtime.tick(false).unwrap(); // start: read submitted
    runtime.tick(false).unwrap(); // completion: resumed, parks on pause
    let drained = runtime.shutdown(64, false).unwrap();
    assert_eq!(drained.records[0].cancelled, Some(CancelPoint::AfterYield));
    let SessionResult::Returned(value) = session
        .decode_completed(&world, &drained.completed[0])
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        value.to_string(),
        oracle(
            PULSE,
            &[
                Answer::Value(words(&[40, 2])),
                Answer::Cancel,
                Answer::Value(words(&[3, 4]))
            ]
        )
    );
    assert!(value.to_string().contains("paused: 3"));

    // During I/O: the cancel command releases nothing. Only the original
    // completion returns the request owners, then the read resumes Cancelled.
    let mut runtime = NativeRuntime::new(Fixture::new(vec![Reply::Hold, Reply::Bytes(word(9))]));
    let log = runtime.driver().log.clone();
    let id = session.spawn(&mut runtime, zero()).unwrap();
    runtime.tick(false).unwrap();
    let incomplete = runtime.shutdown(4, false).unwrap_err();
    assert!(
        matches!(incomplete.reason, Incomplete::BudgetExhausted { ref pending } if pending == &vec![id])
    );
    let mut runtime = incomplete.runtime;
    assert_eq!(runtime.live(), vec![id]);
    assert_eq!(runtime.driver().pending(), 1);
    runtime
        .driver()
        .release(Err(io::Error::from_raw_os_error(125)));
    let drained = runtime.drain(64, false).unwrap();
    assert_eq!(drained.records[0].cancelled, Some(CancelPoint::DuringIo));
    let SessionResult::Returned(value) = session
        .decode_completed(&world, &drained.completed[0])
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        value.to_string(),
        oracle(
            PULSE,
            &[
                Answer::Cancel,
                Answer::Value(vec![]),
                Answer::Value(word(9))
            ]
        )
    );
    assert_eq!(
        *log.borrow(),
        vec![
            // The cancel command precedes the first poll, which holds the
            // read; only its later completion resumes the fiber.
            format!("submit {id}"),
            "cancel".to_string(),
            format!("hold {id}"),
            format!("complete {id} err 125"),
            format!("submit {id}"),
            format!("complete {id} ok 8"),
        ]
    );
}

#[test]
fn cleanup_parks_complete_after_an_incomplete_budgeted_shutdown() {
    let world = world(LEASE);
    let session = NativeSession::compile(&world, "main", &pulse_bindings()).unwrap();
    let mut runtime = NativeRuntime::new(Fixture::new(vec![Reply::Hold]));
    let id = session.spawn(&mut runtime, zero()).unwrap();
    runtime.tick(false).unwrap();
    // The read is in flight; one tick cannot finish it.
    let incomplete = runtime.shutdown(1, false).unwrap_err();
    assert!(matches!(
        incomplete.reason,
        Incomplete::BudgetExhausted { .. }
    ));
    let mut runtime = incomplete.runtime;
    assert!(runtime.is_cancelled());
    // A short read that won the race still resumes the cancellation edge.
    runtime.driver().release(Ok(8));
    // One tick resumes the cancellation edge, which parks on the cleanup
    // pause; the exit needs another.
    let incomplete = runtime.drain(1, false).unwrap_err();
    assert!(
        matches!(incomplete.reason, Incomplete::BudgetExhausted { ref pending } if pending == &vec![id])
    );
    let drained = incomplete.runtime.drain(8, false).unwrap();
    let record = &drained.records[0];
    assert_eq!(record.cancelled, Some(CancelPoint::DuringIo));
    assert_eq!(record.parks, 2, "read park plus the cleanup park");
    assert_eq!(
        session
            .decode_completed(&world, &drained.completed[0])
            .unwrap()
            .to_string_value(),
        "6010"
    );
}

trait ReturnedText {
    fn to_string_value(&self) -> String;
}
impl ReturnedText for SessionResult {
    fn to_string_value(&self) -> String {
        match self {
            SessionResult::Returned(v) => v.to_string(),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn submission_failure_and_io_errors_resume_the_typed_error_edge_in_order() {
    let world = world(LEASE);
    let session = NativeSession::compile(&world, "main", &pulse_bindings()).unwrap();
    // A rejected submission returns the request and resumes IoFailed(11).
    let mut fixture = Fixture::new(vec![]);
    fixture.reject_submit = Some(11);
    let mut runtime = NativeRuntime::new(fixture);
    session.spawn(&mut runtime, zero()).unwrap();
    let drained = runtime.drain(16, false).unwrap();
    assert_eq!(
        session
            .decode_completed(&world, &drained.completed[0])
            .unwrap()
            .to_string_value(),
        oracle(LEASE, &[Answer::Error(11), Answer::Value(vec![])])
    );
    assert_eq!(drained.completed[0].result, 1000 + 10 + 1100);

    // Two sessions: an I/O error and a success complete in driver order.
    let mut runtime =
        NativeRuntime::new(Fixture::new(vec![Reply::Errno(5), Reply::Bytes(word(32))]));
    let log = runtime.driver().log.clone();
    let a = session.spawn(&mut runtime, zero()).unwrap();
    let b = session.spawn(&mut runtime, zero()).unwrap();
    let drained = runtime.drain(32, false).unwrap();
    let order = drained
        .records
        .iter()
        .map(|r| r.fiber_id)
        .collect::<Vec<_>>();
    assert_eq!(order, vec![a, b]);
    let results = drained
        .completed
        .iter()
        .map(|c| {
            session
                .decode_completed(&world, c)
                .unwrap()
                .to_string_value()
        })
        .collect::<Vec<_>>();
    assert_eq!(results, vec!["1510".to_string(), "1050".to_string()]);
    assert_eq!(
        *log.borrow(),
        vec![
            // One scheduler turn per tick: `a` completes before `b` starts.
            format!("submit {a}"),
            format!("complete {a} err 5"),
            format!("submit {b}"),
            format!("complete {b} ok 8"),
        ]
    );
}

#[test]
fn kernel_io_uring_read_resumes_a_source_session() {
    use vesper_pulse::io_driver::IoDriver;
    let world = world(PULSE);
    let session = NativeSession::compile(&world, "main", &pulse_bindings()).unwrap();
    let driver = match IoDriver::<NativeTask>::new(8) {
        Ok(driver) => driver,
        Err(error) if matches!(error.raw_os_error(), Some(1 | 12 | 38 | 95)) => {
            eprintln!(
                "io_uring UNAVAILABLE on this kernel ({error}); kernel evidence not collected"
            );
            return;
        }
        Err(error) => panic!("io_uring setup failed: {error}"),
    };
    let path = std::env::temp_dir().join(format!("vesper-session-{}.bin", std::process::id()));
    let bytes = words(&[40, 2, 500, 7]);
    std::fs::write(&path, &bytes).unwrap();
    let mut runtime = NativeRuntime::new(driver);
    session
        .spawn(&mut runtime, Some(File::open(&path).unwrap()))
        .unwrap();
    let drained = runtime.drain(64, true).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(drained.failures.is_empty(), "{:?}", drained.failures);
    let value = session
        .decode_completed(&world, &drained.completed[0])
        .unwrap()
        .to_string_value();
    assert_eq!(
        value,
        oracle(
            PULSE,
            &[
                Answer::Value(words(&[40, 2])),
                Answer::Value(vec![]),
                Answer::Value(words(&[500, 7]))
            ]
        )
    );
    eprintln!("io_uring kernel read resumed source session: {value}");
}

/// Parks inside a closure body and inside a lexical handler clause whose
/// continuation is resumed after the park; a lexical `handle Pulse` still
/// shadows the external binding.
const SHAPES: &[&str] = &[
    "fn main() -> u64 eff [Pulse] { let f = || tick(); wrapping_add(f(), wrapping_mul(f(), 10)) }",
    "fn main() -> u64 eff [Pulse] { handle Ask in { wrapping_add(Ask::ask(), wrapping_mul(Ask::ask(), 100)) } with { return v => v, ask(k) => { let x = tick(); k(wrapping_add(x, 10)) }, } }",
    "fn main() -> u64 eff [Pulse] { let outer = tick(); let inner: u64 = handle Pulse in { tick() } with { return v => wrapping_add(v, 100), pause(k) => k(TickCancelled), read(buf, k) => k(IoCancelled), }; wrapping_add(wrapping_mul(outer, 1000), inner) }",
];

#[test]
fn closures_and_handler_clauses_park_and_agree_with_the_evaluator() {
    let prefix = "copy struct Block { lo: u64, hi: u64 } copy enum Io { Got(u64), IoFailed(u64), IoCancelled } copy enum Tick { Ticked, TickFailed(u32), TickCancelled } linear effect Pulse { fn pause() -> Tick; fn read(inout buf: Block) -> Io; } linear effect Ask { fn ask() -> u64; } fn tick() -> u64 eff [Pulse] { match Pulse::pause() { Ticked => 1, TickFailed(c) => 2, TickCancelled => 3, } }";
    for (body, parks) in SHAPES.iter().zip([2, 2, 1]) {
        let source = format!("{prefix} {body}");
        let world = world(&source);
        let session = NativeSession::compile(&world, "main", &pulse_bindings()).unwrap();
        for script in [
            vec![Answer::Value(vec![]), Answer::Cancel],
            vec![Answer::Error(4), Answer::Value(vec![])],
        ] {
            let script = &script[..parks];
            let (exit, ops) = run(&session, script, true);
            assert_eq!(ops, vec![OP_YIELD; parks], "{body}");
            assert_eq!(
                returned(&world, &session, &exit),
                oracle(&source, script),
                "{body}"
            );
        }
    }
}
