//! Owned cancellation, shutdown and drain of native fibers.
#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::rc::Rc;
use vesper_fiber_abi::abi::exit;
use vesper_fiber_abi::evidence::{EvidenceRegistry, EvidenceVector};
use vesper_fiber_abi::prototype::{self, JitPrototype, OptLevel};
use vesper_fiber_abi::pulse::{CancelPoint, Incomplete, NativeRuntime, NativeTask, TaskOwner};
use vesper_pulse::io_driver::{IoDriver, Request};
use vesper_pulse::scheduler::{Driver, Event, Finished, PollError, Resume, Step};

fn setup() -> (JitPrototype, Rc<EvidenceVector>, *const u64) {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, clock) = prototype::pulse_read_evidence(&mut registry).unwrap();
    (jit, evidence, clock)
}

fn dev_null() -> File {
    File::open("/dev/null").unwrap()
}

// ---- scheduler adapter --------------------------------------------------

/// Fixture driver distinguishing a cancellation *acknowledgement* from the
/// original request's completion: `cancel_all` acknowledges at once but the
/// original request stays owned here for `delay` more polls.
#[derive(Default)]
struct DelayedDriver {
    pending: VecDeque<(usize, Request<NativeTask>)>,
    delay: usize,
    cancelled: bool,
    acks: usize,
    cancel_calls: usize,
    fail_cancel: usize,
    released: usize,
    payload: Vec<u8>,
    fail_submit: usize,
    fail_poll: usize,
    /// Misbehaviours used by fault tests.
    restart_on_submit: bool,
    drop_on_submit: bool,
}

impl Driver<NativeTask> for DelayedDriver {
    fn submit(
        &mut self,
        request: Request<NativeTask>,
    ) -> Result<(), (io::Error, Request<NativeTask>)> {
        if self.fail_submit > 0 {
            self.fail_submit -= 1;
            return Err((io::Error::from_raw_os_error(11), request));
        }
        if self.drop_on_submit {
            drop(request);
            return Ok(());
        }
        if self.restart_on_submit {
            // A buggy driver resuming the continuation itself with the wrong
            // event. The adapter must keep the owner, not drop it.
            let step = request.continuation.resume(Event::Start);
            assert!(matches!(step, Step::Failed(ref m) if m.contains("owner retained")));
            return Ok(());
        }
        self.pending.push_back((self.delay, request));
        Ok(())
    }
    fn poll(&mut self, _: bool) -> Result<Vec<Finished<NativeTask>>, PollError<NativeTask>> {
        if self.fail_poll > 0 {
            self.fail_poll -= 1;
            return Err(PollError {
                error: io::Error::other("poll failed"),
                completed: vec![],
            });
        }
        let mut done = vec![];
        let mut keep = VecDeque::new();
        while let Some((delay, mut request)) = self.pending.pop_front() {
            if delay > 0 {
                keep.push_back((delay - 1, request));
                continue;
            }
            let result = if self.cancelled {
                Err(io::Error::from_raw_os_error(125))
            } else {
                let n = self.payload.len().min(request.buffer.len());
                request.buffer[..n].copy_from_slice(&self.payload[..n]);
                Ok(n)
            };
            self.released += 1;
            done.push(Finished { request, result });
        }
        self.pending = keep;
        Ok(done)
    }
    fn pending(&self) -> usize {
        self.pending.len()
    }
    fn cancel_all(&mut self) -> io::Result<()> {
        self.cancel_calls += 1;
        if self.fail_cancel > 0 {
            self.fail_cancel -= 1;
            return Err(io::Error::other("cancel failed"));
        }
        // Acknowledge the cancellation command for every in-flight request;
        // release nothing.
        self.acks += self.pending.len();
        self.cancelled = true;
        Ok(())
    }
}

#[test]
fn cancellation_before_start_after_yield_and_during_io_is_distinct() {
    let (jit, evidence, clock) = setup();
    let code = jit.code_liveness();
    let mut rt = NativeRuntime::new(DelayedDriver {
        delay: 3,
        payload: vec![1; 8],
        ..Default::default()
    });
    let io_fiber = rt.spawn(
        jit.spawn_pulse_read(1, evidence.clone()).unwrap(),
        Some(dev_null()),
    );
    let yield_fiber = rt.spawn(
        jit.spawn_pulse_read(2, evidence.clone()).unwrap(),
        Some(dev_null()),
    );
    // Start both (each parks on yield), then the first submits its read.
    for _ in 0..3 {
        rt.tick(false).unwrap();
    }
    assert_eq!(rt.driver().pending(), 1);
    let unstarted = rt.spawn(
        jit.spawn_pulse_read(3, evidence.clone()).unwrap(),
        Some(dev_null()),
    );
    rt.cancel().unwrap();
    assert_eq!(rt.driver().acks, 1);
    drop(jit);

    // A small budget resolves the queued tasks but not the in-flight read.
    let incomplete = rt.drain(2, false).unwrap_err();
    let Incomplete::BudgetExhausted { pending } = &incomplete.reason else {
        panic!("{:?}", incomplete.reason)
    };
    assert_eq!(pending, &vec![io_fiber]);
    let mut rt = incomplete.runtime;
    // The acknowledgement released nothing: the driver still owns the
    // request, so the file, buffer, continuation and code are all alive.
    assert_eq!(rt.driver().pending(), 1);
    assert_eq!(rt.driver().released, 0);
    assert!(code.upgrade().is_some());
    assert_eq!(rt.live(), vec![io_fiber]);
    // Cancelling again is idempotent per task.
    rt.cancel().unwrap();

    let drained = rt.drain(16, false).unwrap();
    assert_eq!(
        drained.driver.released, 1,
        "the real completion returned the owners"
    );
    assert!(drained.failures.is_empty(), "{:?}", drained.failures);
    let record = |id| drained.records.iter().find(|r| r.fiber_id == id).unwrap();
    let r = record(unstarted);
    assert_eq!(
        (r.status, r.parks, r.cancelled),
        (exit::CANCELLED, 0, Some(CancelPoint::BeforeStart))
    );
    let r = record(yield_fiber);
    assert_eq!(r.cancelled, Some(CancelPoint::AfterYield));
    assert_eq!((r.status, r.result, r.parks), (exit::CANCELLED, 1, 1));
    let r = record(io_fiber);
    assert_eq!(r.cancelled, Some(CancelPoint::DuringIo));
    // Cancelled at the read: counter = 1 + 10, no bytes delivered.
    assert_eq!((r.status, r.result, r.parks), (exit::CANCELLED, 11, 2));
    assert_eq!(drained.records.len(), 3);
    assert_eq!(drained.completed.len(), 3);
    // Only the two started fibers ever called the inline Clock handler.
    assert_eq!(unsafe { *clock }, 2);
    drop(drained);
    assert!(
        code.upgrade().is_none(),
        "code freed after the last owner exited"
    );
}

#[test]
fn drain_budget_returns_reparked_owners_that_later_complete() {
    let (jit, evidence, _) = setup();
    let code = jit.code_liveness();
    let mut rt = NativeRuntime::new(DelayedDriver::default());
    let ids = [
        rt.spawn(jit.spawn_cancel_repark(evidence.clone()).unwrap(), None),
        rt.spawn(jit.spawn_cancel_repark(evidence.clone()).unwrap(), None),
    ];
    rt.tick(false).unwrap();
    rt.tick(false).unwrap();
    drop(jit);
    // Two ticks deliver both cancellations; both re-park in cleanup.
    let incomplete = rt.shutdown(2, false).unwrap_err();
    let Incomplete::BudgetExhausted { pending } = &incomplete.reason else {
        panic!("{:?}", incomplete.reason)
    };
    assert_eq!(pending, &ids.to_vec());
    let mut rt = incomplete.runtime;
    assert!(rt.records().is_empty(), "nothing reported complete early");
    assert!(code.upgrade().is_some());
    // A repeated shutdown request does not cancel the cleanup parks, and
    // sends no second driver cancel that could hit cleanup I/O.
    rt.cancel().unwrap();
    assert_eq!(rt.driver().cancel_calls, 1);
    let drained = rt.drain(8, false).unwrap();
    assert_eq!(drained.records.len(), 2);
    for r in &drained.records {
        assert_eq!(r.status, exit::CANCELLED);
        assert_eq!(r.result, 1 + prototype::CLEANUP_DONE);
        assert_eq!(r.cancelled, Some(CancelPoint::AfterYield));
    }
    for c in &drained.completed {
        assert_eq!(c.word(1), prototype::CANCEL_NOTE);
    }
    drop(drained);
    assert!(code.upgrade().is_none());
}

#[test]
fn a_failed_driver_cancel_is_retried_then_never_repeated() {
    let (jit, evidence, _) = setup();
    let mut rt = NativeRuntime::new(DelayedDriver {
        delay: 2,
        fail_cancel: 1,
        ..Default::default()
    });
    let id = rt.spawn(
        jit.spawn_pulse_read(1, evidence.clone()).unwrap(),
        Some(dev_null()),
    );
    for _ in 0..2 {
        rt.tick(false).unwrap();
    }
    assert_eq!(rt.driver().pending(), 1);
    let incomplete = rt.shutdown(16, false).unwrap_err();
    assert!(matches!(incomplete.reason, Incomplete::Driver(_)));
    let mut rt = incomplete.runtime;
    assert_eq!(rt.live(), vec![id]);
    rt.cancel().unwrap();
    rt.cancel().unwrap();
    assert_eq!(
        rt.driver().cancel_calls,
        2,
        "one failure, one success, no repeat"
    );
    assert_eq!(rt.driver().acks, 1);
    let drained = rt.drain(16, false).unwrap();
    assert_eq!(drained.records[0].cancelled, Some(CancelPoint::DuringIo));
}

#[test]
fn clean_shutdown_of_an_idle_runtime_and_spawn_after_cancel() {
    let (jit, evidence, clock) = setup();
    let mut rt = NativeRuntime::new(DelayedDriver::default());
    rt.cancel().unwrap();
    let late = rt.spawn(
        jit.spawn_pulse_read(1, evidence.clone()).unwrap(),
        Some(dev_null()),
    );
    let drained = rt.drain(4, false).unwrap();
    assert_eq!(drained.records[0].fiber_id, late);
    assert_eq!(drained.records[0].cancelled, Some(CancelPoint::BeforeStart));
    assert_eq!(unsafe { *clock }, 0);
}

#[test]
fn submission_and_completion_errors_keep_every_owner() {
    let (jit, evidence, _) = setup();
    // Submission error: the request comes back and the fiber takes its
    // error edge (EAGAIN = 11). Nothing is lost or reported complete.
    let mut rt = NativeRuntime::new(DelayedDriver {
        fail_submit: 1,
        ..Default::default()
    });
    let id = rt.spawn(
        jit.spawn_pulse_read(1, evidence.clone()).unwrap(),
        Some(dev_null()),
    );
    let drained = rt.drain(16, false).unwrap();
    assert_eq!(drained.records[0].fiber_id, id);
    assert_eq!(drained.records[0].status, exit::FAILED);
    assert_eq!(drained.records[0].result, 11);
    assert!(drained.failures[0].contains("failed with code 11"));

    // Completion (poll) error: the runtime comes back with its owners.
    let mut rt = NativeRuntime::new(DelayedDriver {
        payload: vec![2; 8],
        ..Default::default()
    });
    let id = rt.spawn(
        jit.spawn_pulse_read(1, evidence.clone()).unwrap(),
        Some(dev_null()),
    );
    for _ in 0..2 {
        rt.tick(false).unwrap();
    }
    assert_eq!(rt.driver().pending(), 1);
    rt.driver().fail_poll = 1;
    let incomplete = rt.drain(16, false).unwrap_err();
    assert!(matches!(incomplete.reason, Incomplete::Driver(_)));
    let rt = incomplete.runtime;
    assert_eq!(rt.live(), vec![id]);
    let drained = rt.drain(16, false).unwrap();
    assert_eq!(drained.records[0].status, exit::COMPLETE);
    assert_eq!(
        drained.records[0].result,
        // The first sub-case already called the shared Clock handler once.
        prototype::expected_pulse_read(1, 1002, &[2; 8])
    );

    // Completion error during shutdown: still recoverable, then cancelled.
    let mut rt = NativeRuntime::new(DelayedDriver {
        delay: 1,
        ..Default::default()
    });
    let id = rt.spawn(jit.spawn_pulse_read(1, evidence).unwrap(), Some(dev_null()));
    for _ in 0..2 {
        rt.tick(false).unwrap();
    }
    rt.driver().fail_poll = 1;
    let rt = rt.shutdown(16, false).unwrap_err().runtime;
    assert_eq!(rt.live(), vec![id]);
    let drained = rt.drain(16, false).unwrap();
    assert_eq!(drained.records[0].cancelled, Some(CancelPoint::DuringIo));
}

#[test]
fn faulted_tasks_are_retained_and_recoverable() {
    let (jit, evidence, _) = setup();
    let async_id = evidence.identity(1).unwrap();
    let mut rt = NativeRuntime::new(DelayedDriver {
        restart_on_submit: true,
        ..Default::default()
    });
    let id = rt.spawn(jit.spawn_pulse_read(1, evidence).unwrap(), Some(dev_null()));
    let incomplete = rt.drain(16, false).unwrap_err();
    let Incomplete::Retained { fiber_ids } = &incomplete.reason else {
        panic!("{:?}", incomplete.reason)
    };
    assert_eq!(fiber_ids, &vec![id]);
    let mut rt = incomplete.runtime;
    let mut retained = rt.take_retained();
    assert_eq!(retained.len(), 1);
    assert!(retained[0].reason.contains("started twice"));
    let task = retained.pop().unwrap().task;
    let (owner, _file) = task.into_owner().unwrap();
    let TaskOwner::Parked(k) = owner else {
        panic!("expected the parked owner")
    };
    // The owner is intact at its read park and can be cancelled explicitly.
    assert_eq!(k.request().op, prototype::OP_READ);
    let e = k.cancel().exited().unwrap();
    assert_eq!((e.status, e.result), (exit::CANCELLED, 11));
    let _ = async_id;
    assert!(rt.drain(4, false).is_ok());
}

#[test]
fn a_driver_that_drops_requests_is_reported_as_lost() {
    let (jit, evidence, _) = setup();
    let mut rt = NativeRuntime::new(DelayedDriver {
        drop_on_submit: true,
        ..Default::default()
    });
    let id = rt.spawn(jit.spawn_pulse_read(1, evidence).unwrap(), Some(dev_null()));
    let incomplete = rt.drain(16, false).unwrap_err();
    let Incomplete::Lost { fiber_ids } = &incomplete.reason else {
        panic!("{:?}", incomplete.reason)
    };
    assert_eq!(fiber_ids, &vec![id]);
}

#[test]
fn unscoped_tasks_keep_the_v1_in_flight_cancellation_contract() {
    // NativeTask::new outside a runtime: an in-flight read cancelled by the
    // driver arrives as ECANCELED on the error edge (unchanged behaviour).
    let (jit, evidence, _) = setup();
    let mut s = vesper_pulse::scheduler::Scheduler::new(DelayedDriver::default());
    s.spawn(NativeTask::new(
        jit.spawn_pulse_read(1, evidence).unwrap(),
        Some(dev_null()),
    ));
    for _ in 0..2 {
        s.tick(false).unwrap();
    }
    s.cancel_all().unwrap();
    while !s.is_idle() {
        s.tick(false).unwrap();
    }
    assert!(s.take_failure().unwrap().contains("code 125"));
}

#[test]
fn kernel_cancellation_during_io_or_reported_unavailable() {
    let driver = match IoDriver::<NativeTask>::new(8) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("io_uring unavailable on this host ({e}); kernel cancellation NOT validated");
            return;
        }
    };
    // A pipe nobody writes to: the read stays in the kernel until cancelled.
    let (reader, writer) = std::io::pipe().unwrap();
    let file = File::from(std::os::fd::OwnedFd::from(reader));
    let (jit, evidence, _) = setup();
    let mut rt = NativeRuntime::new(driver);
    let id = rt.spawn(jit.spawn_pulse_read(1, evidence).unwrap(), Some(file));
    for _ in 0..4 {
        rt.tick(false).unwrap();
    }
    if rt.driver().pending() != 1 {
        eprintln!("kernel read did not stay pending; kernel cancellation NOT validated");
        return;
    }
    let drained = rt.shutdown(64, true).unwrap();
    let r = &drained.records[0];
    assert_eq!(r.fiber_id, id);
    assert_eq!(r.cancelled, Some(CancelPoint::DuringIo));
    assert_eq!(r.status, exit::CANCELLED);
    drop(writer);
    eprintln!("io_uring kernel cancellation during I/O validated");
}
