//! Native fibers scheduled by the unmodified `vesper_pulse` scheduler.
//!
//! Runs on every direct-fiber target (Linux x86_64/AArch64, macOS AArch64,
//! Windows x86_64); kernel round trips use whichever `vesper_pulse` driver
//! the host starts (`select::open`).
#![cfg(vesper_direct_fibers)]
use std::fs::File;
use std::io::{self, Write};
use vesper_fiber_abi::abi::exit;
use vesper_fiber_abi::evidence::EvidenceRegistry;
use vesper_fiber_abi::prototype::{self, JitPrototype, OptLevel};
use vesper_fiber_abi::pulse::{Completed, NativeTask};
use vesper_pulse::driver::DriverKind;
use vesper_pulse::io_driver::Request;
use vesper_pulse::scheduler::{Driver, Finished, PollError, Scheduler};
use vesper_pulse::select::{self, AnyDriver, Preference};

/// Local fixture driver: completes reads with a fixed payload, or an error.
struct FixtureDriver {
    requests: Vec<Request<NativeTask>>,
    payload: Vec<u8>,
    error: Option<i32>,
    cancelled: bool,
    relocate_while_pending: bool,
    submitted: usize,
    completion_bytes: Option<usize>,
}

impl FixtureDriver {
    fn new(payload: &[u8]) -> Self {
        Self {
            requests: vec![],
            payload: payload.to_vec(),
            error: None,
            cancelled: false,
            relocate_while_pending: false,
            submitted: 0,
            completion_bytes: None,
        }
    }
}

impl Driver<NativeTask> for FixtureDriver {
    fn submit(
        &mut self,
        mut request: Request<NativeTask>,
    ) -> Result<(), (io::Error, Request<NativeTask>)> {
        self.submitted += 1;
        if self.relocate_while_pending {
            // The continuation is owned by the driver while I/O is pending;
            // its suspended data storage can move under that ownership.
            request.continuation.parked_mut().unwrap().relocate_data();
        }
        self.requests.push(request);
        Ok(())
    }
    fn poll(&mut self, _: bool) -> Result<Vec<Finished<NativeTask>>, PollError<NativeTask>> {
        Ok(self
            .requests
            .drain(..)
            .map(|mut request| {
                let result = if self.cancelled {
                    Err(vesper_pulse::driver::cancelled_error())
                } else if let Some(code) = self.error {
                    Err(io::Error::from_raw_os_error(code))
                } else {
                    let n = self.payload.len().min(request.buffer.len());
                    request.buffer[..n].copy_from_slice(&self.payload[..n]);
                    Ok(self.completion_bytes.unwrap_or(n))
                };
                Finished { request, result }
            })
            .collect())
    }
    fn pending(&self) -> usize {
        self.requests.len()
    }
    fn cancel_all(&mut self) -> io::Result<()> {
        self.cancelled = true;
        Ok(())
    }
}

fn dev_null() -> File {
    vesper_fiber_abi::pulse::null_file().unwrap()
}

fn drain<D: Driver<NativeTask>>(s: &mut Scheduler<NativeTask, D>) -> (Vec<Completed>, Vec<String>) {
    while !s.is_idle() {
        s.tick(true).unwrap();
    }
    let mut done = vec![];
    while let Some(bytes) = s.take_completed() {
        done.push(Completed::decode(&bytes).unwrap());
    }
    let mut failed = vec![];
    while let Some(f) = s.take_failure() {
        failed.push(f);
    }
    (done, failed)
}

#[test]
fn scheduler_interleaves_native_fibers_through_yield_and_io() {
    let payload = [10u8, 20, 30, 40, 50, 60, 70, 80];
    for opt in [OptLevel::None, OptLevel::Speed] {
        let jit = JitPrototype::new(opt).unwrap();
        let mut registry = EvidenceRegistry::new();
        let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
        let mut driver = FixtureDriver::new(&payload);
        driver.relocate_while_pending = true;
        let mut s = Scheduler::new(driver);
        for arg in [1u64, 2, 3] {
            s.spawn(NativeTask::new(
                jit.spawn_pulse_read(arg, evidence.clone()).unwrap(),
                Some(dev_null()),
            ));
        }
        // First three ticks start every fiber; all park on yield before any
        // performs I/O, so the scheduler genuinely interleaves them.
        for _ in 0..3 {
            s.tick(false).unwrap();
        }
        assert_eq!(s.driver().submitted, 0);
        let (done, failed) = drain(&mut s);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(s.driver().submitted, 3);
        let mut results: Vec<u64> = done.iter().map(|c| c.result).collect();
        results.sort();
        let expected: Vec<u64> = [1u64, 2, 3]
            .iter()
            .enumerate()
            .map(|(i, a)| prototype::expected_pulse_read(*a, 1001 + i as u64, &payload))
            .collect();
        assert_eq!(results, expected, "{opt:?}");
        for c in &done {
            assert_eq!(c.status, exit::COMPLETE);
            assert_eq!(c.word(0), prototype::PULSE_READ_COUNTER);
            assert_eq!(&c.data[prototype::INBOX..prototype::INBOX + 8], &payload);
        }
    }
}

#[test]
fn io_errors_resume_the_error_edge() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let mut driver = FixtureDriver::new(&[]);
    driver.error = Some(5);
    let mut s = Scheduler::new(driver);
    s.spawn(NativeTask::new(
        jit.spawn_pulse_read(1, evidence).unwrap(),
        Some(dev_null()),
    ));
    let (done, failed) = drain(&mut s);
    assert!(done.is_empty());
    assert_eq!(failed.len(), 1);
    assert!(failed[0].contains("failed with code 5"), "{failed:?}");
}

#[test]
fn oversized_completion_is_an_io_error_instead_of_a_partial_success() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let mut driver = FixtureDriver::new(&[1; 8]);
    driver.completion_bytes = Some(9);
    let mut s = Scheduler::new(driver);
    s.spawn(NativeTask::new(
        jit.spawn_pulse_read(1, evidence).unwrap(),
        Some(dev_null()),
    ));
    let (done, failed) = drain(&mut s);
    assert!(done.is_empty());
    assert_eq!(failed.len(), 1);
    assert!(failed[0].contains("failed with code 5"), "{failed:?}");
}

#[test]
fn missing_file_resumes_with_ebadf_without_submitting() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let mut s = Scheduler::new(FixtureDriver::new(&[1]));
    s.spawn(NativeTask::new(
        jit.spawn_pulse_read(1, evidence).unwrap(),
        None,
    ));
    let (_, failed) = drain(&mut s);
    assert_eq!(s.driver().submitted, 0);
    assert!(failed[0].contains("failed with code 9"), "{failed:?}");
}

#[test]
fn unsupported_operations_resume_with_enosys() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let evidence = prototype::nested_evidence(&mut registry).unwrap();
    let mut s = Scheduler::new(FixtureDriver::new(&[]));
    s.spawn(NativeTask::new(
        jit.spawn_nested(1, evidence).unwrap(),
        None,
    ));
    let (done, failed) = drain(&mut s);
    assert!(done.is_empty());
    // The nested entry's error edge exits FAILED with result 0.
    assert!(failed[0].contains("failed with code 0"), "{failed:?}");
}

#[test]
fn cancellation_resumes_each_parked_fiber_exactly_once() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, clock_calls) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let mut s = Scheduler::new(FixtureDriver::new(&[1; 8]));
    for arg in 0..3 {
        s.spawn(NativeTask::new(
            jit.spawn_pulse_read(arg, evidence.clone()).unwrap(),
            Some(dev_null()),
        ));
    }
    // Fiber 0 reaches I/O; fibers 1 and 2 are parked on yield in the queue.
    s.tick(false).unwrap();
    s.tick(false).unwrap();
    s.tick(false).unwrap();
    s.tick(false).unwrap();
    assert_eq!(s.driver().pending(), 1);
    s.cancel_all().unwrap();
    s.cancel_all().unwrap();
    let (done, failed) = drain(&mut s);
    // Queued fibers take their cancellation edge; the in-flight read reports
    // ECANCELED and takes the error edge. No fiber ran twice.
    assert_eq!(done.len(), 2);
    for c in &done {
        assert_eq!(c.status, exit::CANCELLED);
        assert_eq!(c.word(1), prototype::CANCEL_NOTE);
        assert_eq!(c.result, 1);
    }
    assert_eq!(failed.len(), 1);
    assert!(
        failed[0].contains(&format!(
            "code {}",
            vesper_fiber_abi::pulse::cancelled_code()
        )),
        "{failed:?}"
    );
    assert_eq!(unsafe { *clock_calls }, 3);
}

#[test]
fn unstarted_fiber_cancelled_in_queue_never_runs() {
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, clock_calls) = prototype::pulse_read_evidence(&mut registry).unwrap();
    let mut s = Scheduler::new(FixtureDriver::new(&[]));
    s.spawn(NativeTask::new(
        jit.spawn_pulse_read(0, evidence.clone()).unwrap(),
        None,
    ));
    s.cancel_all().unwrap();
    let (done, failed) = drain(&mut s);
    assert!(failed.is_empty());
    assert_eq!(done[0].status, exit::CANCELLED);
    assert_eq!(unsafe { *clock_calls }, 0);
}

/// Every Pulse driver this host can start (io_uring and epoll on Linux,
/// kqueue on macOS, IOCP on Windows); unavailable ones are reported.
fn host_drivers() -> Vec<(DriverKind, AnyDriver<NativeTask>)> {
    let mut out = vec![];
    for &kind in select::host_order() {
        match AnyDriver::start(kind, 8) {
            Ok(d) => out.push((kind, d)),
            Err(e) => eprintln!("{kind} unavailable on this host ({e}); NOT validated"),
        }
    }
    // `select::open` picks the first of these; it must start on every host
    // the fibers support.
    let (_, report) = select::open::<NativeTask>(8, Preference::Auto).unwrap();
    eprintln!("{report}");
    assert!(!out.is_empty(), "no Pulse driver started");
    out
}

#[test]
fn kernel_round_trip_on_every_host_driver() {
    let dir = std::env::temp_dir().join(format!("vesper-fiber-abi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("payload");
    File::create(&path)
        .unwrap()
        .write_all(&[2, 7, 1, 8, 2, 8, 1, 8])
        .unwrap();
    let jit = JitPrototype::new(OptLevel::Speed).unwrap();
    let mut registry = EvidenceRegistry::new();
    let (evidence, _) = prototype::pulse_read_evidence(&mut registry).unwrap();
    // The shared Clock handler counts every call across drivers.
    let mut now = 1000;
    for (kind, driver) in host_drivers() {
        now += 2;
        let mut s = Scheduler::new(driver);
        s.spawn(NativeTask::new(
            jit.spawn_pulse_read(4, evidence.clone()).unwrap(),
            Some(File::open(&path).unwrap()),
        ));
        let (done, failed) = drain(&mut s);
        assert!(failed.is_empty(), "{kind}: {failed:?}");
        assert_eq!(
            done[0].result,
            prototype::expected_pulse_read(4, now - 1, &[2, 7, 1, 8, 2, 8, 1, 8]),
            "{kind}"
        );
        // The null file reads as end of file on every driver.
        s.spawn(NativeTask::new(
            jit.spawn_pulse_read(4, evidence.clone()).unwrap(),
            Some(dev_null()),
        ));
        let (done, failed) = drain(&mut s);
        assert!(failed.is_empty(), "{kind}: {failed:?}");
        assert_eq!(
            done[0].result,
            prototype::expected_pulse_read(4, now, &[]),
            "{kind}"
        );
        eprintln!("{kind} kernel round trip validated");
    }
    let _ = std::fs::remove_dir_all(dir);
}
