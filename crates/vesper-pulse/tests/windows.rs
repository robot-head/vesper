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
        io::{FromRawHandle, FromRawSocket, IntoRawHandle, IntoRawSocket, RawHandle, RawSocket},
    },
    sync::mpsc,
    time::Duration,
};
use vesper_pulse::driver::{CompletionDriver, Operation, Request, is_cancelled};
use vesper_pulse::iocp::{IocpDriver, rebinds_reused_handles};
use vesper_pulse::pool::{Pool, PoolConfig};
use vesper_pulse::scheduler::{Event, Resume, Scheduler, Step};

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

#[test]
fn reused_handle_values_never_route_packets_to_another_port() {
    let _guard = watchdog("handle reuse");
    let mut d1 = IocpDriver::<u8>::new(4).unwrap();
    let mut d2 = IocpDriver::<u8>::new(4).unwrap();
    let mut d3 = IocpDriver::<u8>::new(4).unwrap();
    let (pa, a) = overlapped_temp("a");
    let a = read_once(&mut d1, a).unwrap();
    let a_value = a.into_raw_handle();
    drop(unsafe { File::from_raw_handle(a_value) });
    // A new file, very likely given the closed handle's value, bound to d2.
    let (pb, b) = overlapped_temp("b");
    let reused = std::os::windows::io::AsRawHandle::as_raw_handle(&b) == a_value;
    let b = read_once(&mut d2, b).unwrap();
    // d3 never bound this value: the foreign binding is a typed error.
    let err = read_once(&mut d3, b.try_clone().unwrap());
    assert!(err.is_err() || !reused);
    if let Err(e) = &err {
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
    }
    if rebinds_reused_handles() {
        // d1 bound this value before: it rebinds the new file to itself, so
        // the packet arrives at d1 instead of d2.
        let b = read_once(&mut d1, b).unwrap();
        drop(b);
        eprintln!("iocp: reused handle value rebound (reused: {reused})");
    } else {
        eprintln!("iocp: FileReplaceCompletionInformation unavailable here; rebind not exercised");
    }
    let _ = std::fs::remove_file(pa);
    let _ = std::fs::remove_file(pb);
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
