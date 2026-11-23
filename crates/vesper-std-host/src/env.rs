//! `sys library StdEnv` on Windows (packages/vesper-std/env/env-windows.vsys).
//!
//! On Linux and macOS `@vesper-std/env` imports the C library itself
//! (env-linux.vsys, env-macos.vsys; SH6 lane 13, #1408), so nothing here
//! runs there. Windows keeps these few functions until Vesper calls Win32
//! itself (gap G7, UTF-16 strings): each has the C signature of the libc
//! function its `.vsys` declaration names, over UTF-8 text.
//!
//! - `vstd_getenv` is `getenv`: the value stays valid until the next call
//!   on the same thread, which the lowering copies before.
//! - `vstd_getcwd` is `getcwd`: on a short buffer it returns NULL with
//!   `ERROR_INSUFFICIENT_BUFFER` (122) as the thread's last error.
//! - `vstd_dlsym` answers the two names src/lib.ves looks up, laid out as
//!   glibc does: `__libc_stack_end` (a word pointing at `argc`, then
//!   `argv`) and `environ` (a word pointing at the `NULL`-terminated
//!   `KEY=VALUE` array, rebuilt on each lookup). Other names are NULL.
//! - `vstd_memcpy`, `vstd_strchr` and `vstd_strstr` are `memcpy`, `strchr`
//!   and `strstr`.
use crate::*;
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::OnceLock;

/// Win32 reports a short directory buffer through GetLastError.
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

#[cfg(windows)]
fn set_last_error(code: u32) {
    unsafe extern "system" {
        fn SetLastError(code: u32);
    }
    // SAFETY: sets the calling thread's last-error value.
    unsafe { SetLastError(code) };
}

#[cfg(not(windows))]
fn set_last_error(_code: u32) {}

thread_local! {
    static VALUE: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// `getenv(name)`: the variable's UTF-8 value, or NULL when it is unset.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_getenv(name: *const c_char) -> *const c_char {
    // SAFETY: the lowering passes a NUL-terminated copy for the call.
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    let Ok(name) = os_string(name) else {
        return std::ptr::null();
    };
    let Some(value) = std::env::var_os(name) else {
        return std::ptr::null();
    };
    let Ok(value) = CString::new(os_bytes(&value)) else {
        return std::ptr::null();
    };
    VALUE.with(|v| {
        let mut v = v.borrow_mut();
        *v = Some(value);
        v.as_ref().map_or(std::ptr::null(), |s| s.as_ptr())
    })
}

/// `getcwd(buf, size)`: the current directory, NUL-terminated, in `buf`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_getcwd(buf: *mut u8, size: usize) -> *mut u8 {
    match std::env::current_dir() {
        Ok(d) => {
            let text = os_bytes(d.as_os_str());
            if text.len() >= size {
                set_last_error(ERROR_INSUFFICIENT_BUFFER);
                return std::ptr::null_mut();
            }
            // SAFETY: the caller lends `size` bytes for the call.
            let out = unsafe { bytes_mut(buf, size) };
            out[..text.len()].copy_from_slice(&text);
            out[text.len()] = 0;
            buf
        }
        Err(e) => {
            set_last_error(e.raw_os_error().map_or(0, |c| c as u32));
            std::ptr::null_mut()
        }
    }
}

/// A leaked `NULL`-terminated array of C strings.
fn table(strings: impl Iterator<Item = Vec<u8>>) -> Vec<usize> {
    let mut out: Vec<usize> = strings
        .filter_map(|s| CString::new(s).ok())
        .map(|s| s.into_raw() as usize)
        .collect();
    out.push(0);
    out
}

/// `argc` then the `argv` array, once per process.
fn stack_top() -> &'static [usize] {
    static TOP: OnceLock<Vec<usize>> = OnceLock::new();
    TOP.get_or_init(|| {
        let argv = table(std::env::args_os().map(|a| os_bytes(&a)));
        let mut top = vec![argv.len() - 1];
        top.extend(argv);
        top
    })
}

/// The process argument table is immutable. Environment lookups return a
/// thread-local snapshot, valid until that thread's next environment lookup.
static STACK_END: OnceLock<usize> = OnceLock::new();

#[derive(Default)]
struct Environment {
    strings: Vec<CString>,
    pointers: Vec<usize>,
    address: usize,
}

thread_local! {
    static ENVIRON: RefCell<Environment> = RefCell::default();
}

/// `dlsym(handle, name)` for the two variables src/lib.ves reads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_dlsym(_handle: u64, name: *const c_char) -> *const c_void {
    // SAFETY: the lowering passes a NUL-terminated copy for the call.
    match unsafe { CStr::from_ptr(name) }.to_bytes() {
        b"__libc_stack_end" => {
            let end = STACK_END.get_or_init(|| stack_top().as_ptr() as usize);
            end as *const usize as *const c_void
        }
        b"environ" => ENVIRON.with(|cell| {
            let mut env = cell.borrow_mut();
            env.strings = std::env::vars_os()
                .filter_map(|(k, v)| {
                    let mut text = os_bytes(&k);
                    text.push(b'=');
                    text.extend(os_bytes(&v));
                    CString::new(text).ok()
                })
                .collect();
            env.pointers = env
                .strings
                .iter()
                .map(|s| s.as_ptr() as usize)
                .chain(std::iter::once(0))
                .collect();
            env.address = env.pointers.as_ptr() as usize;
            &env.address as *const usize as *const c_void
        }),
        _ => std::ptr::null(),
    }
}

/// `memcpy(dst, src, n)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    // SAFETY: the caller passes `n` readable bytes at `src` and writable
    // ones at `dst`, not overlapping.
    unsafe { std::ptr::copy_nonoverlapping(src, dst, n) };
    dst
}

/// `strchr(s, c)`: the first `c` byte in `s`, or NULL (`c` 0 finds the NUL).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_strchr(s: *const c_char, c: i32) -> *const c_char {
    // SAFETY: `s` is a NUL-terminated string for the call.
    let text = unsafe { CStr::from_ptr(s) }.to_bytes_with_nul();
    let byte = c as u8;
    match text.iter().position(|&b| b == byte) {
        // SAFETY: `i` is inside the string or at its NUL.
        Some(i) => unsafe { s.add(i) },
        None => std::ptr::null(),
    }
}

/// `strstr(haystack, needle)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_strstr(
    haystack: *const c_char,
    needle: *const c_char,
) -> *const c_char {
    // SAFETY: both are NUL-terminated strings for the call.
    let (h, n) = unsafe {
        (
            CStr::from_ptr(haystack).to_bytes(),
            CStr::from_ptr(needle).to_bytes(),
        )
    };
    if n.is_empty() {
        return haystack;
    }
    match h.windows(n.len()).position(|w| w == n) {
        // SAFETY: `i` is inside the haystack.
        Some(i) => unsafe { haystack.add(i) },
        None => std::ptr::null(),
    }
}

pub(crate) fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("vstd_getenv", vstd_getenv as *const u8),
        ("vstd_getcwd", vstd_getcwd as *const u8),
        ("vstd_dlsym", vstd_dlsym as *const u8),
        ("vstd_memcpy", vstd_memcpy as *const u8),
        ("vstd_strchr", vstd_strchr as *const u8),
        ("vstd_strstr", vstd_strstr as *const u8),
    ]
}
