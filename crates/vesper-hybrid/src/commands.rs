//! The host side of `@vesper/driver-commands` (self-hosting SH5 lane 13,
//! #1336): the `HybridCommands` import (packages/vesper-driver-commands/
//! commands.vsys). The routing in front of every command (help, version,
//! the unknown-command error, the hand-off to `vesper-<tool>`) is Vesper;
//! this prints for it as Rust's macros do and answers the few questions
//! @vesper-std cannot (the running executable's directory, the help text,
//! whether a file is executable).
use std::io;
use std::path::Path;

/// Prints of the Vesper command layers (`hc_print` and `hc_help`);
/// `VESPER_HYBRID_STATS=1` reports it. A command handed back to the Rust
/// CLI makes none.
pub static PRINTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The `io::Error` with @vesper-std's error word (the kind in the low 8
/// bits, the raw OS error in the high 32), as the driver's host builds it.
fn io_error(word: u64) -> io::Error {
    use io::ErrorKind as K;
    let os = (word >> 32) as u32;
    if os != 0 {
        return io::Error::from_raw_os_error(os as i32);
    }
    let kind = match word & 0xff {
        1 => K::NotFound,
        2 => K::PermissionDenied,
        3 => K::AlreadyExists,
        4 => K::NotADirectory,
        5 => K::IsADirectory,
        6 => K::DirectoryNotEmpty,
        7 => K::InvalidInput,
        8 => K::Interrupted,
        9 => K::UnexpectedEof,
        10 => K::WriteZero,
        11 => K::Unsupported,
        12 => K::OutOfMemory,
        _ => K::Other,
    };
    io::Error::from(kind)
}

/// Print as the Rust commands do (`print!`, `println!`, `eprintln!`): a
/// failed write panics with std's own message, and the CLI exits 101 as the
/// Rust CLI does when its thread panics. The panic must not unwind into
/// the Vesper caller, so it is caught here and the process exits.
fn rust_print(print: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(print)).is_err() {
        std::process::exit(101);
    }
}

/// The `HybridCommands` import's symbols (named by `// @export vsys
/// HybridCommands rust=crate::commands::host` in modules/driver).
pub mod host {
    use super::*;

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

    /// Copy `text` into `buf` when it fits; its length (the copy-out
    /// protocol), or -1 for none.
    ///
    /// # Safety
    /// `buf` is a lent writable slice of `len` bytes.
    unsafe fn copy_out(text: Option<&[u8]>, buf: *mut u8, len: usize) -> i64 {
        let Some(text) = text else { return -1 };
        if text.len() <= len && !buf.is_null() {
            // SAFETY: `buf` holds `len >= text.len()` bytes.
            unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), buf, text.len()) };
        }
        i64::try_from(text.len()).unwrap_or(i64::MAX)
    }

    /// Write `text` to stdout (1) or stderr (2).
    ///
    /// # Safety
    /// `text` is a lent byte slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_print(stream: u32, text: *const u8, len: usize) {
        PRINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: lent for the call.
        let text = String::from_utf8_lossy(unsafe { bytes(text, len) });
        if stream == 2 {
            rust_print(|| eprint!("{text}"));
        } else {
            rust_print(|| print!("{text}"));
        }
    }

    /// The CLI's help (no command, `--help` or `-h`).
    #[unsafe(no_mangle)]
    pub extern "C" fn hc_help() {
        PRINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        rust_print(|| print!("{}", crate::HELP));
    }

    /// Text `which`: see `text_*` in the package. `index` is unused.
    ///
    /// # Safety
    /// `buf` is a lent writable slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_text(which: u32, _index: u64, buf: *mut u8, len: usize) -> i64 {
        let exe = std::env::current_exe().ok();
        let dir = |p: Option<std::path::PathBuf>| p.and_then(|p| p.parent().map(Path::to_path_buf));
        let owned = match which {
            4 => dir(exe.clone()),
            5 => dir(exe.and_then(|e| e.canonicalize().ok())),
            _ => None,
        };
        let text: Option<&[u8]> = match which {
            1 => Some(env!("CARGO_PKG_VERSION").as_bytes()),
            2 => Some(std::env::consts::ARCH.as_bytes()),
            3 => Some(std::env::consts::OS.as_bytes()),
            4 | 5 => owned.as_ref().map(|p| p.as_os_str().as_encoded_bytes()),
            _ => None,
        };
        // SAFETY: lent for the call.
        unsafe { copy_out(text, buf, len) }
    }

    /// Rust's `Display` of the `io::Error` with this error word.
    ///
    /// # Safety
    /// `buf` is a lent writable slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_io_error(word: u64, buf: *mut u8, len: usize) -> i64 {
        let text = io_error(word).to_string();
        // SAFETY: lent for the call.
        unsafe { copy_out(Some(text.as_bytes()), buf, len) }
    }

    /// 1 when `path` is a regular file the host would run (on Unix, with an
    /// execute bit), as `tool_commands::executable` decides.
    ///
    /// # Safety
    /// `path` is a lent byte slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_executable(path: *const u8, len: usize) -> u32 {
        // SAFETY: lent for the call; the bytes came from the OS (the
        // executable's directory or PATH) or from the command line.
        let path = unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(bytes(path, len)) };
        let Ok(meta) = Path::new(path).metadata() else {
            return 0;
        };
        #[cfg(unix)]
        let ok = {
            use std::os::unix::fs::PermissionsExt;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let ok = meta.is_file();
        u32::from(ok)
    }
}
