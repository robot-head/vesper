//! The host side of `vesper lsp`'s Vesper loop (@vesper/lsp-server, #1828
//! part 15c): the `hc_lsp_engine_*` functions of the `HybridCommands`
//! import (packages/vesper-driver-engines/commands-<os>.vsys). The loop owns
//! the framing, the envelope refusals and the session gates; this file
//! owns the Rust session behind it (`crate::lsp::Engine`) and the client's
//! streams. Standard input is read on its own thread, as `serve` reads it,
//! so a read can wait for bytes with a timeout on every target.
//!
//! Re-exported from `crate::commands::host`, the module the driver's
//! `HybridCommands` binding names.
use std::cell::RefCell;
use std::io::{self, Read as _, Write as _};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::lsp::{Engine, EngineExit};

/// `hc_lsp_engine_start`'s answers.
const START_OK: u64 = 0;
const START_FAILED: u64 = 1;
/// `hc_lsp_engine_exit`'s answers.
const EXIT_CONTINUE: u64 = 0;
const EXIT_CLEAN: u64 = 1;
const EXIT_UNCLEAN: u64 = 2;
const EXIT_FAILED: u64 = 3;
/// `hc_lsp_engine_read`'s answers besides a byte count.
const READ_END: i64 = 0;
const READ_WAITED: i64 = -1;
const READ_FAILED: i64 = -2;
/// `hc_lsp_engine_write`'s answers.
const WRITE_OK: u64 = 0;
const WRITE_FAILED: u64 = 1;
/// `hc_lsp_engine_deadline`'s answer when no timer can fire.
const NO_TIMER: i64 = -1;
/// How many bytes the reader thread reads at a time.
const READ_CHUNK: usize = 64 * 1024;

/// The session and the client of one `vesper lsp` run.
struct Host {
    engine: Engine,
    /// The frames the last `frame` or `tick` wrote, and how it ended.
    frames: Vec<u8>,
    exit: EngineExit,
    /// Bytes the reader thread sent that the loop has not taken yet.
    unread: Vec<u8>,
    /// Chunks of standard input; an empty chunk is the end of input.
    input: Receiver<io::Result<Vec<u8>>>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    /// The last error's text, for `hc_lsp_engine_error`.
    static LAST_ERR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn set_error(text: String) {
    LAST_ERR.with(|cell| *cell.borrow_mut() = text.into_bytes());
}

fn with_host<T>(missing: T, f: impl FnOnce(&mut Host) -> T) -> T {
    HOST.with(|cell| match cell.borrow_mut().as_mut() {
        Some(host) => f(host),
        None => {
            set_error("LSP engine not started".into());
            missing
        }
    })
}

/// Copy-out: copies `data` when `buf` holds it, and returns its length.
///
/// # Safety
/// `buf` is a lent writable slice of `len` bytes.
unsafe fn copy_out(data: &[u8], buf: *mut u8, len: usize) -> u64 {
    if data.len() <= len && !buf.is_null() && !data.is_empty() {
        // SAFETY: `buf` holds `len >= data.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len()) };
    }
    data.len() as u64
}

/// The bytes of a lent `nocapture in CSlice[u8]`.
///
/// # Safety
/// `ptr` is valid for `len` bytes for the call.
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 || ptr.is_null() {
        return &[];
    }
    // SAFETY: as documented.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// Record what a `frame` or `tick` returned: the frames' length.
fn ran(host: &mut Host, (frames, exit, error): (Vec<u8>, EngineExit, String)) -> u64 {
    host.frames = frames;
    host.exit = exit;
    if exit == EngineExit::Failed {
        set_error(error);
    }
    host.frames.len() as u64
}

/// Start the session (`serve`'s default Lens configuration) and the
/// standard input reader.
#[unsafe(no_mangle)]
pub extern "C" fn hc_lsp_engine_start() -> u64 {
    let engine = match Engine::new(crate::lsp::LensConfig::default()) {
        Ok(engine) => engine,
        Err(error) => {
            set_error(error);
            return START_FAILED;
        }
    };
    let (chunks, input) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        loop {
            let mut chunk = vec![0; READ_CHUNK];
            match stdin.read(&mut chunk) {
                Ok(n) => {
                    chunk.truncate(n);
                    let end = n == 0;
                    if chunks.send(Ok(chunk)).is_err() || end {
                        return;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    let _ = chunks.send(Err(error));
                    return;
                }
            }
        }
    });
    HOST.with(|cell| {
        *cell.borrow_mut() = Some(Host {
            engine,
            frames: Vec::new(),
            exit: EngineExit::Continue,
            unread: Vec::new(),
            input,
        })
    });
    START_OK
}

/// Handle one frame body the loop's gates passed.
///
/// # Safety
/// `body` is a lent byte slice of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hc_lsp_engine_frame(body: *const u8, len: usize) -> u64 {
    // SAFETY: the `.vsys` contract lends the body.
    let body = unsafe { bytes(body, len) };
    with_host(0, |host| {
        let result = host.engine.frame(body);
        ran(host, result)
    })
}

/// Handle the timers and background runs that finished.
#[unsafe(no_mangle)]
pub extern "C" fn hc_lsp_engine_tick() -> u64 {
    with_host(0, |host| {
        let result = host.engine.tick();
        ran(host, result)
    })
}

/// Copy the last `frame` or `tick`'s frames out.
///
/// # Safety
/// `buf` is a lent writable slice of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hc_lsp_engine_take(buf: *mut u8, len: usize) -> u64 {
    // SAFETY: as `copy_out` requires.
    with_host(0, |host| unsafe { copy_out(&host.frames, buf, len) })
}

/// How the last `frame` or `tick` ended.
#[unsafe(no_mangle)]
pub extern "C" fn hc_lsp_engine_exit() -> u64 {
    with_host(EXIT_FAILED, |host| match host.exit {
        EngineExit::Continue => EXIT_CONTINUE,
        EngineExit::Clean => EXIT_CLEAN,
        EngineExit::Unclean => EXIT_UNCLEAN,
        EngineExit::Failed => EXIT_FAILED,
    })
}

/// Copy the last error's text out.
///
/// # Safety
/// `buf` is a lent writable slice of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hc_lsp_engine_error(buf: *mut u8, len: usize) -> u64 {
    // SAFETY: as `copy_out` requires.
    LAST_ERR.with(|cell| unsafe { copy_out(&cell.borrow(), buf, len) })
}

/// Read client bytes into `buf`, waiting at most `wait_ms` milliseconds
/// (negative: no limit) for them.
///
/// # Safety
/// `buf` is a lent writable slice of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hc_lsp_engine_read(buf: *mut u8, len: usize, wait_ms: i64) -> i64 {
    with_host(READ_FAILED, |host| {
        if host.unread.is_empty() {
            let chunk = match u64::try_from(wait_ms) {
                Ok(ms) => match host.input.recv_timeout(Duration::from_millis(ms)) {
                    Ok(chunk) => chunk,
                    Err(RecvTimeoutError::Timeout) => return READ_WAITED,
                    Err(RecvTimeoutError::Disconnected) => return READ_END,
                },
                Err(_) => match host.input.recv() {
                    Ok(chunk) => chunk,
                    Err(_) => return READ_END,
                },
            };
            match chunk {
                Ok(chunk) => host.unread = chunk,
                Err(error) => {
                    set_error(error.to_string());
                    return READ_FAILED;
                }
            }
            if host.unread.is_empty() {
                return READ_END;
            }
        }
        let n = host.unread.len().min(len);
        if n == 0 || buf.is_null() {
            return READ_WAITED;
        }
        // SAFETY: `buf` holds `len >= n` bytes.
        unsafe { std::ptr::copy_nonoverlapping(host.unread.as_ptr(), buf, n) };
        host.unread.drain(..n);
        n as i64
    })
}

/// Write frames to standard output and flush them.
///
/// # Safety
/// `buf` is a lent byte slice of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hc_lsp_engine_write(buf: *const u8, len: usize) -> u64 {
    // SAFETY: the `.vsys` contract lends the frames.
    let data = unsafe { bytes(buf, len) };
    let mut out = io::stdout().lock();
    match out.write_all(data).and_then(|()| out.flush()) {
        Ok(()) => WRITE_OK,
        Err(error) => {
            set_error(error.to_string());
            WRITE_FAILED
        }
    }
}

/// How long the loop may wait for client bytes before it ticks.
#[unsafe(no_mangle)]
pub extern "C" fn hc_lsp_engine_deadline() -> i64 {
    with_host(NO_TIMER, |host| host.engine.deadline())
}
