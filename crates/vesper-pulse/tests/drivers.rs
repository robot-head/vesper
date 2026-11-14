//! The real scheduler, nursery and drain protocol on every driver this host
//! can start: io_uring (where permitted) and epoll on Linux, kqueue on macOS,
//! IOCP on Windows. Each test prints the driver it ran, so CI logs record
//! which job exercised which driver.
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_pulse::driver::{CompletionDriver, DriverKind, Operation, Request, is_cancelled};
use vesper_pulse::scheduler::{Event, Nursery, Resume, Scheduler, Step};
use vesper_pulse::select::{self, AnyDriver, Preference, SelectError};

/// Every compiled driver that starts here. A denied io_uring is reported and
/// skipped; any other host driver failing to start fails the test.
fn drivers<T>() -> Vec<AnyDriver<T>> {
    let mut out = vec![];
    for &kind in select::host_order() {
        match AnyDriver::start(kind, 32) {
            Ok(d) => out.push(d),
            Err(SelectError::Unavailable(s)) if kind == DriverKind::IoUring => {
                eprintln!("skipping io_uring on this host: {}", s.error);
            }
            Err(e) => panic!("{kind} failed to start: {e}"),
        }
    }
    assert!(!out.is_empty(), "no driver started");
    out
}

fn temp_path(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "vesper-pulse-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn temp_file(tag: &str) -> (TempFile, File) {
    let path = temp_path(tag);
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    (TempFile(path), file)
}

/// A connected stream `(reader, writer)` whose read can stay pending: an
/// anonymous pipe on Unix, an overlapped named pipe on Windows.
fn stream() -> (File, File) {
    #[cfg(unix)]
    {
        let (r, w) = io::pipe().unwrap();
        (
            File::from(std::os::fd::OwnedFd::from(r)),
            File::from(std::os::fd::OwnedFd::from(w)),
        )
    }
    #[cfg(windows)]
    {
        vesper_pulse::iocp::overlapped_pipe().unwrap()
    }
}

fn request<T>(file: File, op: Operation, buffer: Vec<u8>, offset: u64, k: T) -> Request<T> {
    Request {
        file,
        buffer,
        continuation: k,
        operation: op,
        offset,
    }
}

/// Write a payload at an offset, read it back, yield, complete with the bytes.
enum RoundTrip {
    Start(File, Vec<u8>),
    Written(usize),
    Read,
    Yielded(Vec<u8>),
}
impl Resume for RoundTrip {
    fn resume(self, event: Event) -> Step<Self> {
        match (self, event) {
            (RoundTrip::Start(file, payload), Event::Start) => {
                let n = payload.len();
                Step::Io(request(
                    file,
                    Operation::Write,
                    payload,
                    3,
                    RoundTrip::Written(n),
                ))
            }
            (RoundTrip::Written(n), Event::Io(io)) => match io.result {
                Ok(w) if w == n => Step::Io(request(
                    io.file,
                    Operation::Read,
                    vec![0; 64],
                    3,
                    RoundTrip::Read,
                )),
                other => Step::Failed(format!("write: {other:?}")),
            },
            (RoundTrip::Read, Event::Io(io)) => match io.result {
                Ok(n) if n == io.buffer.len() => Step::Yield(RoundTrip::Yielded(io.buffer)),
                other => Step::Failed(format!("read: {other:?}")),
            },
            (RoundTrip::Yielded(bytes), Event::Yield) => Step::Complete(bytes),
            (_, Event::Cancelled(_)) => Step::Failed("cancelled".into()),
            _ => panic!("a continuation received the wrong event or ran twice"),
        }
    }
}

#[test]
fn scheduler_round_trips_files_on_every_host_driver() {
    for driver in drivers() {
        let kind = driver.kind();
        let mut nursery = Nursery::new(driver);
        let mut temps = vec![];
        for i in 0..3u8 {
            let (temp, file) = temp_file("roundtrip");
            temps.push(temp);
            nursery.spawn(RoundTrip::Start(file, vec![b'a' + i; 5 + i as usize]));
        }
        let mut done = nursery.join().unwrap();
        done.sort();
        assert_eq!(
            done,
            vec![b"aaaaa".to_vec(), b"bbbbbb".to_vec(), b"ccccccc".to_vec()]
        );
        eprintln!("{kind}: scheduler file round trip ok");
    }
}

/// Reader parks on an empty stream until a sibling writes into it.
enum Pipe {
    Reader(File),
    Writer(File),
    AwaitRead,
    AwaitWrite,
}
impl Resume for Pipe {
    fn resume(self, event: Event) -> Step<Self> {
        match (self, event) {
            (Pipe::Reader(f), Event::Start) => {
                Step::Io(request(f, Operation::Read, vec![0; 32], 0, Pipe::AwaitRead))
            }
            (Pipe::Writer(f), Event::Start) => Step::Io(request(
                f,
                Operation::Write,
                b"pulse".to_vec(),
                0,
                Pipe::AwaitWrite,
            )),
            (Pipe::AwaitRead, Event::Io(io)) => match io.result {
                Ok(_) => Step::Complete(io.buffer),
                Err(e) => Step::Failed(format!("read: {e}")),
            },
            (Pipe::AwaitWrite, Event::Io(io)) => match io.result {
                Ok(5) => Step::Complete(b"wrote".to_vec()),
                other => Step::Failed(format!("write: {other:?}")),
            },
            (_, Event::Cancelled(_)) => Step::Failed("cancelled".into()),
            _ => panic!("a continuation received the wrong event or ran twice"),
        }
    }
}

#[test]
fn a_stream_read_waits_for_its_writer() {
    for driver in drivers() {
        let kind = driver.kind();
        let (r, w) = stream();
        let mut scheduler = Scheduler::new(driver);
        scheduler.spawn(Pipe::Reader(r));
        // Run the reader until it is parked in the driver, then add the writer.
        scheduler.tick(false).unwrap();
        assert_eq!(
            scheduler.driver().pending(),
            1,
            "{kind}: read must be pending"
        );
        scheduler.spawn(Pipe::Writer(w));
        while !scheduler.is_idle() {
            scheduler.tick(true).unwrap();
        }
        assert!(scheduler.take_failure().is_none());
        let mut done: BTreeSet<_> = BTreeSet::new();
        while let Some(v) = scheduler.take_completed() {
            done.insert(v);
        }
        assert_eq!(done, BTreeSet::from([b"pulse".to_vec(), b"wrote".to_vec()]));
        eprintln!("{kind}: stream readiness/completion ok");
    }
}

#[test]
fn a_closed_writer_reads_as_end_of_file() {
    for mut driver in drivers::<u8>() {
        let kind = driver.kind();
        let (r, w) = stream();
        drop(w);
        driver
            .submit(request(r, Operation::Read, vec![9; 8], 0, 1))
            .unwrap_or_else(|(e, _)| panic!("{kind}: {e}"));
        let mut done = vec![];
        while done.is_empty() {
            done = driver.poll(true).unwrap();
        }
        assert_eq!(*done[0].result.as_ref().unwrap(), 0, "{kind}");
        assert!(done[0].request.buffer.is_empty(), "{kind}");
        eprintln!("{kind}: end of file ok");
    }
}

/// Parks on a read that never completes on its own; on cancellation it
/// completes with a marker so the test can count exact-once resumptions.
struct Parked;
impl Resume for Parked {
    fn resume(self, event: Event) -> Step<Self> {
        match event {
            Event::Io(io) => {
                assert!(
                    io.result.as_ref().is_err_and(is_cancelled),
                    "expected cancellation, got {:?}",
                    io.result
                );
                assert_eq!(
                    io.buffer.len(),
                    16,
                    "a cancelled read keeps its whole buffer"
                );
                Step::Complete(b"cancelled".to_vec())
            }
            Event::Cancelled(_) => Step::Complete(b"never started".to_vec()),
            _ => panic!("unexpected event"),
        }
    }
}
enum Child {
    Start(File),
    Waiting,
}
impl Resume for Child {
    fn resume(self, event: Event) -> Step<Self> {
        match (self, event) {
            (Child::Start(f), Event::Start) => {
                Step::Io(request(f, Operation::Read, vec![0; 16], 0, Child::Waiting))
            }
            (Child::Waiting, e) => match Parked.resume(e) {
                Step::Complete(v) => Step::Complete(v),
                _ => unreachable!(),
            },
            (Child::Start(_), Event::Cancelled(_)) => Step::Complete(b"never started".to_vec()),
            _ => panic!("unexpected event"),
        }
    }
}

#[test]
fn nursery_cancellation_drains_every_in_flight_request() {
    for driver in drivers() {
        let kind = driver.kind();
        let mut writers = vec![];
        let mut nursery = Nursery::new(driver);
        for _ in 0..4 {
            let (r, w) = stream();
            writers.push(w);
            nursery.spawn(Child::Start(r));
        }
        for _ in 0..4 {
            nursery.scheduler().tick(false).unwrap();
        }
        let (r, w) = stream();
        writers.push(w);
        nursery.spawn(Child::Start(r)); // still queued when cancelled
        assert_eq!(nursery.scheduler().driver().pending(), 4, "{kind}");
        nursery.cancel().unwrap();
        let mut done = nursery.join().unwrap();
        done.sort();
        let mut expected = vec![b"cancelled".to_vec(); 4];
        expected.push(b"never started".to_vec());
        expected.sort();
        assert_eq!(done, expected, "{kind}");
        drop(writers);
        eprintln!("{kind}: nursery cancellation drained 4 in-flight + 1 queued");
    }
}

#[test]
fn drain_returns_every_owner_exactly_once() {
    for mut driver in drivers::<u32>() {
        let kind = driver.kind();
        let mut writers = vec![];
        let mut expected = BTreeSet::new();
        for k in 0..3 {
            let (r, w) = stream();
            writers.push(w);
            driver
                .submit(request(r, Operation::Read, vec![0; 8], 0, k))
                .unwrap_or_else(|(e, _)| panic!("{kind}: {e}"));
            expected.insert(k);
        }
        let (_temp, file) = temp_file("drain");
        driver
            .submit(request(file, Operation::Write, b"done".to_vec(), 0, 99))
            .unwrap_or_else(|(e, _)| panic!("{kind}: {e}"));
        expected.insert(99);
        let done = match driver.drain() {
            Ok(done) => done,
            Err(e) => panic!("{kind}: drain failed: {e:?}"),
        };
        let mut seen = BTreeSet::new();
        for c in &done {
            assert!(seen.insert(c.request.continuation), "{kind}: resumed twice");
            if c.request.continuation == 99 {
                // A request that already finished keeps its real result;
                // io_uring may still cancel one it has not yet submitted.
                match &c.result {
                    Ok(n) => assert_eq!(*n, 4, "{kind}"),
                    Err(e) => assert!(is_cancelled(e), "{kind}: {e}"),
                }
            } else {
                assert!(
                    c.result.as_ref().is_err_and(is_cancelled),
                    "{kind}: {:?}",
                    c.result
                );
            }
        }
        assert_eq!(seen, expected, "{kind}");
        drop(writers);
        eprintln!("{kind}: drain returned all 4 owners");
    }
}

#[test]
fn per_request_and_driver_faults_are_typed_errors() {
    for mut driver in drivers::<u8>() {
        let kind = driver.kind();
        // A write to a read-only handle fails that request only.
        let (temp, file) = temp_file("fault");
        drop(file);
        let read_only = File::open(&temp.0).unwrap();
        driver
            .submit(request(read_only, Operation::Write, b"x".to_vec(), 0, 1))
            .unwrap_or_else(|(e, _)| panic!("{kind}: {e}"));
        let mut done = vec![];
        while done.is_empty() {
            done = driver.poll(true).unwrap();
        }
        let error = done[0].result.as_ref().unwrap_err();
        assert!(error.raw_os_error().is_some(), "{kind}: {error}");
        assert_eq!(
            done[0].request.buffer, b"x",
            "{kind}: the buffer comes back"
        );
        eprintln!("{kind}: read-only write -> {error}");
    }
    // A full driver hands the request back unchanged.
    for kind in select::host_order() {
        let Ok(mut driver) = AnyDriver::<u8>::start(*kind, 1) else {
            continue;
        };
        let (r1, _w1) = stream();
        let (r2, _w2) = stream();
        driver
            .submit(request(r1, Operation::Read, vec![0; 4], 0, 1))
            .unwrap_or_else(|(e, _)| panic!("{kind}: {e}"));
        let (error, back) = match driver.submit(request(r2, Operation::Read, vec![0; 4], 0, 2)) {
            Err(rejected) => rejected,
            Ok(_) if *kind == DriverKind::IoUring => continue, // ring sizes round up
            Ok(_) => panic!("{kind}: accepted beyond capacity"),
        };
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{kind}");
        assert_eq!(back.continuation, 2);
        assert_eq!(driver.drain().map(|d| d.len()).unwrap_or(0), 1);
    }
    assert!(matches!(
        AnyDriver::<u8>::start(select::host_order()[0], 0),
        Err(SelectError::InvalidEntries)
    ));
    assert!(matches!(
        Preference::parse("select"),
        Err(SelectError::UnknownDriver(_))
    ));
}

#[test]
fn runtime_selection_reports_the_driver_that_ran() {
    let preference = Preference::from_env().unwrap();
    let (driver, report) = select::open::<RoundTrip>(16, preference).unwrap();
    assert_eq!(driver.kind(), report.selected);
    if let Preference::Only(kind) = preference {
        assert_eq!(report.selected, kind);
    }
    // The selected driver runs the scheduler end to end.
    let (_temp, file) = temp_file("selected");
    let mut nursery = Nursery::new(driver);
    nursery.spawn(RoundTrip::Start(file, b"selected".to_vec()));
    assert_eq!(nursery.join().unwrap(), vec![b"selected".to_vec()]);
    println!("{report}");
    eprintln!("{report}");
}

#[cfg(target_os = "linux")]
#[test]
fn epoll_runs_when_io_uring_is_not_chosen() {
    // The fallback path used on hosts where io_uring_setup returns EPERM is
    // exercised here directly, whether or not this host permits io_uring.
    let (driver, report) =
        select::open::<RoundTrip>(8, Preference::Only(DriverKind::Epoll)).unwrap();
    assert_eq!(report.selected, DriverKind::Epoll);
    let (_temp, file) = temp_file("epoll");
    let mut nursery = Nursery::new(driver);
    nursery.spawn(RoundTrip::Start(file, b"epoll".to_vec()));
    assert_eq!(nursery.join().unwrap(), vec![b"epoll".to_vec()]);
    match select::open::<()>(8, Preference::Only(DriverKind::IoUring)) {
        Ok((_, report)) => eprintln!("io_uring permitted here: {report}"),
        Err(SelectError::Unavailable(s)) => {
            assert_eq!(s.kind, DriverKind::IoUring);
            eprintln!("io_uring denied here: {}", s.error);
        }
        Err(e) => panic!("unexpected selection error: {e}"),
    }
}

#[cfg(windows)]
#[test]
fn iocp_overlapped_files_complete_through_the_port() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    let path = temp_path("overlapped");
    let _temp = TempFile(path.clone());
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(&path)
        .unwrap();
    let mut nursery = Nursery::new(vesper_pulse::iocp::IocpDriver::new(8).unwrap());
    nursery.spawn(RoundTrip::Start(file, b"overlapped".to_vec()));
    assert_eq!(nursery.join().unwrap(), vec![b"overlapped".to_vec()]);
}
