//! Windows-only IOCP behaviour: AFD socket readiness, synchronous handles on
//! offload threads, and reused handle values. Under Wine these validate the
//! protocol against Wine's reimplementation; the Windows CI job is the real
//! evidence.
#![cfg(windows)]
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    net::{TcpListener, TcpStream},
    os::windows::{
        fs::OpenOptionsExt,
        io::{
            AsRawHandle, FromRawHandle, FromRawSocket, IntoRawHandle, IntoRawSocket, OwnedHandle,
            RawHandle, RawSocket,
        },
    },
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
use vesper_pulse::driver::{CompletionDriver, Operation, Request, is_cancelled};
use vesper_pulse::iocp::{IocpDriver, rebinds_reused_handles};
use vesper_pulse::pool::{Pool, PoolConfig};
use vesper_pulse::scheduler::{Event, Resume, Scheduler, Step};
use windows_sys::Win32::System::IO::CreateIoCompletionPort;

const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;

/// Fail the test instead of hanging the job if a scheduler blocks.
fn watchdog(name: &'static str) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(Duration::from_secs(60)) {
            eprintln!("{name}: blocked for 60 s");
            std::process::exit(101);
        }
    });
    tx
}

fn socket_pair() -> (File, File) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let as_file = |s: TcpStream| unsafe { File::from_raw_handle(s.into_raw_socket() as RawHandle) };
    (as_file(server), as_file(client))
}

/// Close a socket with `closesocket`, not `CloseHandle`.
fn close_socket(file: File) {
    drop(unsafe { TcpStream::from_raw_socket(file.into_raw_handle() as RawSocket) });
}

fn request<T>(file: File, operation: Operation, buffer: Vec<u8>, k: T) -> Request<T> {
    Request {
        file,
        buffer,
        continuation: k,
        operation,
        offset: 0,
    }
}

enum Io {
    Read(Option<File>),
    Write(Option<File>, Vec<u8>),
    Wait,
}
impl Resume for Io {
    fn resume(self, event: Event) -> Step<Self> {
        match (self, event) {
            (Io::Read(f), Event::Start) => {
                Step::Io(request(f.unwrap(), Operation::Read, vec![0; 64], Io::Wait))
            }
            (Io::Write(f, data), Event::Start) => {
                Step::Io(request(f.unwrap(), Operation::Write, data, Io::Wait))
            }
            (Io::Wait, Event::Io(e)) => match e.result {
                Ok(_) => {
                    let mut out = e.buffer;
                    out.extend(b"|");
                    out.extend(format!("{:?}", e.file.into_raw_handle() as usize).bytes());
                    Step::Complete(out)
                }
                Err(err) => Step::Failed(format!("{err}")),
            },
            (Io::Wait, Event::Cancelled(inner)) => match *inner {
                Event::Io(e) if e.result.as_ref().is_err_and(is_cancelled) => {
                    Step::Complete(b"cancelled".to_vec())
                }
                Event::Io(e) => Step::Failed(format!("not cancelled: {:?}", e.result)),
                _ => Step::Failed("cancelled without I/O".into()),
            },
            _ => Step::Failed("wrong event".into()),
        }
    }
}

/// Run a scheduler to idle; return the payloads (before the `|`).
fn run<D: vesper_pulse::scheduler::Driver<Io>>(s: &mut Scheduler<Io, D>) -> Vec<Vec<u8>> {
    while !s.is_idle() {
        s.tick(true).unwrap();
    }
    if let Some(f) = s.take_failure() {
        panic!("{f}");
    }
    let mut out = vec![];
    while let Some(v) = s.take_completed() {
        let end = v.iter().position(|&b| b == b'|').unwrap_or(v.len());
        out.push(v[..end].to_vec());
    }
    out
}

#[test]
fn a_socket_read_waits_on_afd_until_its_peer_writes() {
    let _guard = watchdog("afd socket read");
    let (server, client) = socket_pair();
    let mut driver = IocpDriver::<u8>::new(8).unwrap();
    // Raw protocol first: a pending read on an empty socket.
    let id = driver
        .submit(request(server, Operation::Read, vec![0; 16], 1))
        .map_err(|(e, _)| e)
        .unwrap();
    assert!(driver.poll(false).unwrap().is_empty());
    assert_eq!(driver.in_flight(), 1);
    assert_eq!(driver.socket_polls(), 1, "the read waits on an AFD poll");
    let w = driver
        .submit(request(client, Operation::Write, b"afd".to_vec(), 2))
        .map_err(|(e, _)| e)
        .unwrap();
    let mut done = vec![];
    while done.len() < 2 {
        done.extend(driver.poll(true).unwrap());
    }
    done.sort_by_key(|c| c.id);
    assert_eq!(done[0].id, id);
    assert_eq!(done[0].request.buffer, b"afd");
    assert_eq!(done[1].id, w);
    assert_eq!(*done[1].result.as_ref().unwrap(), 3);
    let mut it = done.into_iter();
    let server = it.next().unwrap().request.file;
    let client = it.next().unwrap().request.file;

    // The same sockets on another driver: sockets are never bound to a port.
    let mut scheduler = Scheduler::new(IocpDriver::new(8).unwrap());
    scheduler.spawn(Io::Read(Some(server)));
    scheduler.tick(false).unwrap();
    assert_eq!(scheduler.driver().in_flight(), 1);
    assert_eq!(scheduler.driver().socket_polls(), 1);
    scheduler.spawn(Io::Write(Some(client), b"second".to_vec()));
    let mut out = run(&mut scheduler);
    out.sort();
    assert_eq!(out, vec![b"second".to_vec(), b"second".to_vec()]);
    eprintln!("iocp: AFD socket readiness ok");
}

#[test]
fn socket_end_of_stream_and_cancellation() {
    let _guard = watchdog("afd eof/cancel");
    let (server, client) = socket_pair();
    let mut driver = IocpDriver::<u8>::new(8).unwrap();
    let id = driver
        .submit(request(server, Operation::Read, vec![0; 16], 1))
        .map_err(|(e, _)| e)
        .unwrap();
    assert!(driver.cancel(id).unwrap());
    let c = loop {
        if let Some(c) = driver.poll(true).unwrap().pop() {
            break c;
        }
    };
    assert!(
        is_cancelled(c.result.as_ref().unwrap_err()),
        "{:?}",
        c.result
    );
    assert_eq!(
        c.request.buffer.len(),
        16,
        "a cancelled read returns its buffer"
    );
    let server = c.request.file;
    // Peer closes: the parked read completes with zero bytes.
    driver
        .submit(request(server, Operation::Read, vec![0; 16], 2))
        .map_err(|(e, _)| e)
        .unwrap();
    close_socket(client);
    let c = loop {
        if let Some(c) = driver.poll(true).unwrap().pop() {
            break c;
        }
    };
    assert_eq!(*c.result.as_ref().unwrap(), 0);
    close_socket(c.request.file);
    eprintln!("iocp: AFD end of stream and cancellation ok");
}

#[test]
fn an_empty_synchronous_pipe_does_not_block_the_scheduler() {
    let _guard = watchdog("sync pipe");
    // std's anonymous pipes are synchronous handles.
    let (r, w) = io::pipe().unwrap();
    let (r, w) = (
        File::from(std::os::windows::io::OwnedHandle::from(r)),
        File::from(std::os::windows::io::OwnedHandle::from(w)),
    );
    let mut scheduler = Scheduler::new(IocpDriver::new(8).unwrap());
    scheduler.spawn(Io::Read(Some(r)));
    // Submitting the read returns at once; the read waits on a thread.
    scheduler.tick(false).unwrap();
    assert_eq!(scheduler.driver().in_flight(), 1);
    assert_eq!(scheduler.driver().offload_threads(), 1);
    for _ in 0..3 {
        scheduler.tick(false).unwrap();
    }
    scheduler.spawn(Io::Write(Some(w), b"sync".to_vec()));
    let mut out = run(&mut scheduler);
    out.sort();
    assert_eq!(out, vec![b"sync".to_vec(), b"sync".to_vec()]);
    // Threads are reused once idle.
    assert!(scheduler.driver().offload_threads() <= 2);
    eprintln!("iocp: synchronous pipe offload ok");
}

#[test]
fn an_offloaded_read_on_an_empty_pipe_can_be_cancelled() {
    let _guard = watchdog("sync cancel");
    let (r, w) = io::pipe().unwrap();
    let r = File::from(std::os::windows::io::OwnedHandle::from(r));
    let mut driver = IocpDriver::<u8>::new(8).unwrap();
    driver
        .submit(request(r, Operation::Read, vec![0; 8], 1))
        .map_err(|(e, _)| e)
        .unwrap();
    // Give the thread time to block in ReadFile, then cancel.
    std::thread::sleep(Duration::from_millis(50));
    let done = driver.drain().unwrap();
    assert_eq!(done.len(), 1);
    assert!(
        is_cancelled(done[0].result.as_ref().unwrap_err()),
        "{:?}",
        done[0].result
    );
    assert_eq!(done[0].request.buffer.len(), 8);
    drop(w);
    eprintln!("iocp: offloaded read cancelled with CancelSynchronousIo");
}

fn overlapped_temp(tag: &str) -> (std::path::PathBuf, File) {
    let path = std::env::temp_dir().join(format!("vesper-iocp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut f = File::create(&path).unwrap();
    f.write_all(b"reuse").unwrap();
    drop(f);
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(&path)
        .unwrap();
    (path, f)
}

fn read_once(d: &mut IocpDriver<u8>, f: File) -> io::Result<File> {
    d.submit(request(f, Operation::Read, vec![0; 5], 0))
        .map_err(|(e, _)| e)?;
    loop {
        if let Some(c) = d.poll(true).unwrap().pop() {
            assert_eq!(c.request.buffer, b"reuse");
            return Ok(c.request.file);
        }
    }
}

const FOREIGN_CHILD_ENV: &str = "VESPER_PULSE_FOREIGN_IOCP_CHILD";
const FOREIGN_CHILD_TEST: &str = "foreign_bound_handle_is_rejected_in_fresh_process";
const FOREIGN_CHILD_SUCCESS: i32 = 73;

fn foreign_binding_case() {
    let _guard = watchdog("isolated foreign completion port");
    let (path, file) = overlapped_temp("foreign");
    let file_handle = file.as_raw_handle();
    // SAFETY: file is live and overlapped; a null existing port creates a new
    // native port and associates this file with it, without any Pulse binding.
    let port = unsafe { CreateIoCompletionPort(file_handle, std::ptr::null_mut(), 0, 1) };
    assert!(
        !port.is_null(),
        "create foreign completion port: {}",
        io::Error::last_os_error()
    );
    // SAFETY: the successful call returned a fresh port owned by this fixture.
    let foreign_port = unsafe { OwnedHandle::from_raw_handle(port) };
    // This fresh child has never submitted anything through Pulse, so no
    // numeric handle value can have a prior entry in its process-wide table.
    let mut driver = IocpDriver::<u8>::new(4).unwrap();
    let (error, rejected) = driver
        .submit(request(file, Operation::Read, vec![0; 5], 0))
        .expect_err("a never-tracked foreign binding must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
    assert_eq!(
        error.to_string(),
        "handle is bound to another completion port"
    );
    assert_eq!(rejected.file.as_raw_handle(), file_handle);
    assert_eq!(rejected.buffer, vec![0; 5]);
    assert_eq!(driver.pending(), 0);
    drop(rejected);
    drop(driver);
    drop(foreign_port);
    std::fs::remove_file(path).unwrap();
}

fn fail_foreign_child(child: &mut Child, reason: &str) -> ! {
    let killed = child.kill();
    let reaped = child.wait();
    panic!("{reason}; kill: {killed:?}; reap: {reaped:?}");
}

#[test]
fn foreign_bound_handle_is_rejected_in_fresh_process() {
    if let Some(parent) = std::env::var_os(FOREIGN_CHILD_ENV) {
        let parent: u32 = parent.to_str().unwrap().parse().unwrap();
        assert_ne!(parent, std::process::id(), "fixture requires a fresh child");
        foreign_binding_case();
        // The private fixture has returned: its files, driver, port and
        // watchdog token have been dropped before this success receipt.
        std::process::exit(FOREIGN_CHILD_SUCCESS);
    }

    let _guard = watchdog("foreign completion port parent");
    let started = Instant::now();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            FOREIGN_CHILD_TEST,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(FOREIGN_CHILD_ENV, std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn required isolated foreign-binding fixture");
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // A stale --exact filter running zero tests exits zero,
                // which is deliberately not this fixture's success receipt.
                assert_eq!(status.code(), Some(FOREIGN_CHILD_SUCCESS), "{status}");
                break;
            }
            Ok(None) if started.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => fail_foreign_child(&mut child, "foreign-binding child exceeded 30 s"),
            Err(error) => {
                fail_foreign_child(&mut child, &format!("poll foreign-binding child: {error}"))
            }
        }
    }
}

#[test]
fn reused_handle_values_never_route_packets_to_another_port() {
    let _guard = watchdog("handle reuse");
    let mut d1 = IocpDriver::<u8>::new(4).unwrap();
    let mut d2 = IocpDriver::<u8>::new(4).unwrap();
    let (pa, a) = overlapped_temp("a");
    let a = read_once(&mut d1, a).unwrap();
    let a_value = a.into_raw_handle();
    drop(unsafe { File::from_raw_handle(a_value) });
    // A new, unbound file, very likely given the closed handle's value:
    // d2 binds it normally even though the table remembers d1.
    let (pb, b) = overlapped_temp("b");
    let reused = std::os::windows::io::AsRawHandle::as_raw_handle(&b) == a_value;
    let b = read_once(&mut d2, b).unwrap();
    if rebinds_reused_handles() {
        // b's value is known to the table with nothing in flight: d1 rebinds
        // it to its own port, so the packet arrives at d1, not d2; and back.
        let b = read_once(&mut d1, b).unwrap();
        let b = read_once(&mut d2, b).unwrap();
        drop(b);
        eprintln!("iocp: handle moved between ports by rebinding (value reused: {reused})");
    } else {
        let err = read_once(&mut d1, b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        eprintln!("iocp: FileReplaceCompletionInformation unavailable here; moving refused");
    }
    let _ = std::fs::remove_file(pa);
    let _ = std::fs::remove_file(pb);
}

/// Unpinned fibers doing several overlapped operations on their own files
/// while being stolen between workers: each operation goes to whichever
/// driver runs the fiber, so the file must move between ports.
enum Hop {
    Start(File, u8),
    Step(u8, u32),
}
impl Resume for Hop {
    fn resume(self, event: Event) -> Step<Self> {
        match (self, event) {
            (Hop::Start(f, tag), Event::Start) => Step::Io(Request {
                file: f,
                buffer: vec![0; 5],
                continuation: Hop::Step(tag, 0),
                operation: Operation::Read,
                offset: 0,
            }),
            (Hop::Step(tag, n), Event::Io(e)) => match e.result {
                Ok(5) if e.buffer == b"reuse" => {
                    if n == 6 {
                        drop(e.file);
                        return Step::Complete(vec![tag]);
                    }
                    // Yield between reads by re-reading after a no-op step.
                    Step::Io(Request {
                        file: e.file,
                        buffer: vec![0; 5],
                        continuation: Hop::Step(tag, n + 1),
                        operation: Operation::Read,
                        offset: 0,
                    })
                }
                other => Step::Failed(format!("{tag}: {other:?}")),
            },
            _ => Step::Failed("wrong event".into()),
        }
    }
}

#[test]
fn overlapped_files_follow_stolen_fibers_between_ports() {
    let _guard = watchdog("stolen overlapped fibers");
    let pool = Pool::new(PoolConfig::new(3).seed(5)).unwrap();
    let mut paths = vec![];
    for t in 0..48u8 {
        let (p, f) = overlapped_temp(&format!("hop{t}"));
        paths.push(p);
        pool.spawn(Hop::Start(f, t));
    }
    let out = pool.run(|_| IocpDriver::new(64)).unwrap();
    let stolen: u64 = out.workers.iter().map(|w| w.stolen).sum();
    if rebinds_reused_handles() {
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(out.completed.len(), 48);
    } else {
        // Without rebinding, a stolen fiber's file cannot follow it; the
        // failure is the typed error, never a hang or a lost packet.
        assert_eq!(out.completed.len() + out.failures.len(), 48);
        for (_, f) in &out.failures {
            assert!(f.contains("cannot rebind") || f.contains("another"), "{f}");
        }
    }
    eprintln!(
        "iocp: {} overlapped fibers done, {} refused, {stolen} stolen",
        out.completed.len(),
        out.failures.len()
    );
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}

/// Echo pairs over TCP across a multi-threaded pool: readers pinned to one
/// worker, writers to another.
#[test]
fn sockets_across_pool_workers() {
    let _guard = watchdog("pool sockets");
    let workers = 3;
    let pool = Pool::new(PoolConfig::new(workers).seed(4)).unwrap();
    let mut n = 0;
    for p in 0..12 {
        let (server, client) = socket_pair();
        pool.spawn_pinned(p % workers, Io::Read(Some(server)))
            .map_err(|_| ())
            .unwrap();
        pool.spawn_pinned(
            (p + 1) % workers,
            Io::Write(Some(client), format!("socket-{p:02}").into_bytes()),
        )
        .map_err(|_| ())
        .unwrap();
        n += 2;
    }
    let out = pool.run(|_| IocpDriver::new(64)).unwrap();
    assert!(out.failures.is_empty(), "{:?}", out.failures);
    assert_eq!(out.completed.len(), n);
    for (_, v) in &out.completed {
        // Close each socket the right way.
        let end = v.iter().position(|&b| b == b'|').unwrap();
        let raw: usize = std::str::from_utf8(&v[end + 1..]).unwrap().parse().unwrap();
        close_socket(unsafe { File::from_raw_handle(raw as RawHandle) });
        assert!(v.starts_with(b"socket-"));
    }
    eprintln!("iocp: {n} socket fibers across {workers} workers ok");
}
