//! The host side of `@vesper/driver-commands` (self-hosting SH5 lane 13,
//! #1336): the `HybridCommands` import (packages/vesper-driver-commands/
//! commands.vsys), which reaches the engines the command layers call that
//! are still Rust. The command layers themselves (arguments, checks,
//! messages, output) are Vesper; the driver hands them every command that
//! is not on the compile path.
use std::cell::RefCell;
use std::io::{self, Write};

/// Prints of the Vesper command layers (`hc_print` and `hc_help`);
/// `VESPER_HYBRID_STATS=1` reports it. A command handed back to the Rust
/// CLI makes none.
pub static PRINTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

thread_local! {
    /// The scanner `hc_simd_compile` made, and the last error message.
    static STATE: RefCell<State> = const { RefCell::new(State { scanner: None, error: String::new() }) };
}

struct State {
    scanner: Option<vesper_simd::JitScanner>,
    error: String,
}

/// `IsaProfileKind` in the order of `profile_code` in the package.
fn profile(code: u32) -> Option<vesper_simd::IsaProfileKind> {
    use vesper_simd::IsaProfileKind as K;
    Some(match code {
        0 => K::Baseline128,
        1 => K::Avx2,
        2 => K::Avx512,
        3 => K::Sve2,
        4 => K::Rvv1,
        _ => return None,
    })
}

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
        let text = unsafe { bytes(text, len) };
        let _ = if stream == 2 {
            io::stderr().lock().write_all(text)
        } else {
            io::stdout().lock().write_all(text)
        };
    }

    /// The CLI's help (no command, `--help` or `-h`).
    #[unsafe(no_mangle)]
    pub extern "C" fn hc_help() {
        PRINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        print!("{}", crate::HELP);
    }

    /// Text `which` (item `index` of a list): see `text_*` in the package.
    ///
    /// # Safety
    /// `buf` is a lent writable slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_text(which: u32, index: u64, buf: *mut u8, len: usize) -> i64 {
        STATE.with_borrow(|st| {
            let index = usize::try_from(index).unwrap_or(usize::MAX);
            let meta = st.scanner.as_ref().map(|s| s.metadata());
            let text: Option<&[u8]> = match which {
                0 => Some(st.error.as_bytes()),
                1 => Some(env!("CARGO_PKG_VERSION").as_bytes()),
                2 => Some(std::env::consts::ARCH.as_bytes()),
                3 => Some(std::env::consts::OS.as_bytes()),
                10 => meta.map(|m| m.target.as_bytes()),
                11 => meta.map(|m| m.tail_strategy.as_bytes()),
                12 => meta.map(|m| m.abi.as_bytes()),
                13 => meta.map(|m| m.codegen_version.as_bytes()),
                14 => meta
                    .and_then(|m| m.codegen_settings.get(index))
                    .map(|s| s.as_bytes()),
                15 => meta
                    .and_then(|m| m.enabled_isa_flags.get(index))
                    .map(|s| s.as_bytes()),
                _ => None,
            };
            // SAFETY: lent for the call.
            unsafe { copy_out(text, buf, len) }
        })
    }

    /// Count `which`: the scanner's vector bits (1), its settings (2) or
    /// its enabled ISA flags (3); 0 without a scanner.
    #[unsafe(no_mangle)]
    pub extern "C" fn hc_count(which: u32) -> u64 {
        STATE.with_borrow(|st| {
            let Some(m) = st.scanner.as_ref().map(|s| s.metadata()) else {
                return 0;
            };
            match which {
                1 => u64::from(m.vector_bits),
                2 => m.codegen_settings.len() as u64,
                3 => m.enabled_isa_flags.len() as u64,
                _ => 0,
            }
        })
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

    /// `JitScanner::compile_profile`: 0, or -1 with the error as text 0.
    #[unsafe(no_mangle)]
    pub extern "C" fn hc_simd_compile(code: u32) -> i64 {
        STATE.with_borrow_mut(|st| {
            let Some(kind) = profile(code) else {
                st.error = format!("unknown SIMD profile {code}");
                return -1;
            };
            match vesper_simd::JitScanner::compile_profile(kind) {
                Ok(scanner) => {
                    st.scanner = Some(scanner);
                    0
                }
                Err(e) => {
                    st.error = e.to_string();
                    -1
                }
            }
        })
    }

    /// One `scan` of the compiled scanner: `matches << 8 | xor` (a chunk
    /// is at most 64 KiB, so the matches fit).
    ///
    /// # Safety
    /// `data` is a lent byte slice.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn hc_simd_scan(data: *const u8, len: usize, delimiter: u32) -> u64 {
        // SAFETY: lent for the call.
        let data = unsafe { bytes(data, len) };
        STATE.with_borrow(|st| match &st.scanner {
            Some(scanner) => {
                let r = scanner.scan(data, delimiter as u8);
                (r.matches << 8) | u64::from(r.xor)
            }
            None => 0,
        })
    }
}
