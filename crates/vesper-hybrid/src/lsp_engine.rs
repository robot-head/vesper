//! The host side of `vesper lsp`'s Vesper loop (@vesper/lsp-server, #1828
//! part 15c): the `hc_lsp_engine_*` functions of the `HybridCommands`
//! import (packages/vesper-driver-engines/commands-<os>.vsys). The loop owns
//! the client's streams (part 16a), the framing, the envelope refusals and
//! the session gates; this file owns the Rust session behind it
//! (`crate::lsp::Engine`).
//!
//! Re-exported from `crate::commands::host`, the module the driver's
//! `HybridCommands` binding names.
use std::cell::RefCell;

use crate::lsp::{Engine, EngineExit};

/// `hc_lsp_engine_start`'s answers.
const START_OK: u64 = 0;
const START_FAILED: u64 = 1;
/// `hc_lsp_engine_exit`'s answers.
const EXIT_CONTINUE: u64 = 0;
const EXIT_CLEAN: u64 = 1;
const EXIT_UNCLEAN: u64 = 2;
const EXIT_FAILED: u64 = 3;
/// `hc_lsp_engine_deadline`'s answer when no timer can fire.
const NO_TIMER: i64 = -1;

/// The session of one `vesper lsp` run.
struct Host {
    engine: Engine,
    /// The frames the last `frame` or `tick` wrote, and how it ended.
    frames: Vec<u8>,
    exit: EngineExit,
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

/// Start the session (`serve`'s default Lens configuration).
#[unsafe(no_mangle)]
pub extern "C" fn hc_lsp_engine_start() -> u64 {
    let engine = match Engine::new(crate::lsp::LensConfig::default()) {
        Ok(engine) => engine,
        Err(error) => {
            set_error(error);
            return START_FAILED;
        }
    };
    HOST.with(|cell| {
        *cell.borrow_mut() = Some(Host {
            engine,
            frames: Vec::new(),
            exit: EngineExit::Continue,
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

/// How long the loop may wait for client bytes before it ticks.
#[unsafe(no_mangle)]
pub extern "C" fn hc_lsp_engine_deadline() -> i64 {
    with_host(NO_TIMER, |host| host.engine.deadline())
}
