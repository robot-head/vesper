//! `sys library StdEnv` (packages/vesper-std/env/env.vsys): arguments,
//! environment variables and the current directory. Every text result
//! uses the copy-out protocol: the call returns the length and copies only
//! when the buffer is big enough.
use crate::*;

/// The number of process arguments, the program name included.
#[unsafe(no_mangle)]
pub extern "C" fn vstd_env_arg_count() -> u64 {
    std::env::args_os().count() as u64
}

/// Argument `index` into `buf`. Returns its length, or -1 past the last.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_env_arg(index: u64, buf: *mut u8, len: usize) -> i64 {
    // SAFETY: lent for the call.
    let buf = unsafe { bytes_mut(buf, len) };
    match std::env::args_os().nth(index as usize) {
        Some(a) => copy_out(&os_bytes(&a), buf) as i64,
        None => -1,
    }
}

/// Variable `key` into `buf`. Returns its length, or -1 when it is unset
/// (or `key` is empty, holds `=` or NUL).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_env_var(
    key: *const u8,
    key_len: usize,
    buf: *mut u8,
    len: usize,
) -> i64 {
    // SAFETY: lent for the call.
    let (key, buf) = unsafe { (bytes(key, key_len), bytes_mut(buf, len)) };
    if key.is_empty() || key.contains(&b'=') {
        return -1;
    }
    let Ok(key) = os_string(key) else {
        return -1;
    };
    match std::env::var_os(key) {
        Some(v) => copy_out(&os_bytes(&v), buf) as i64,
        None => -1,
    }
}

/// The number of environment variables now.
#[unsafe(no_mangle)]
pub extern "C" fn vstd_env_var_count() -> u64 {
    std::env::vars_os().count() as u64
}

/// Variable `index` (in the order the OS keeps them) as `KEY=VALUE` into
/// `buf`. Returns its length, or -1 past the last.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_env_var_at(index: u64, buf: *mut u8, len: usize) -> i64 {
    // SAFETY: lent for the call.
    let buf = unsafe { bytes_mut(buf, len) };
    match std::env::vars_os().nth(index as usize) {
        Some((k, v)) => {
            let mut text = os_bytes(&k);
            text.push(b'=');
            text.extend(os_bytes(&v));
            copy_out(&text, buf) as i64
        }
        None => -1,
    }
}

/// The current directory into `buf`. Returns its length, or -1 on error
/// (written to `err`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_env_current_dir(buf: *mut u8, len: usize, err: *mut u64) -> i64 {
    // SAFETY: lent for the call.
    let buf = unsafe { bytes_mut(buf, len) };
    match std::env::current_dir() {
        Ok(d) => {
            // SAFETY: lent for the call.
            unsafe { set_err(err, 0) };
            copy_out(&os_bytes(d.as_os_str()), buf) as i64
        }
        Err(e) => {
            // SAFETY: lent for the call.
            unsafe { set_err(err, error_word(&e)) };
            -1
        }
    }
}

/// The platform: 1 Linux, 2 macOS, 3 Windows, 0 other.
#[unsafe(no_mangle)]
pub extern "C" fn vstd_env_os() -> u32 {
    if cfg!(target_os = "linux") {
        1
    } else if cfg!(target_os = "macos") {
        2
    } else if cfg!(windows) {
        3
    } else {
        0
    }
}

pub(crate) fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("vstd_env_arg_count", vstd_env_arg_count as *const u8),
        ("vstd_env_arg", vstd_env_arg as *const u8),
        ("vstd_env_var", vstd_env_var as *const u8),
        ("vstd_env_var_count", vstd_env_var_count as *const u8),
        ("vstd_env_var_at", vstd_env_var_at as *const u8),
        ("vstd_env_current_dir", vstd_env_current_dir as *const u8),
        ("vstd_env_os", vstd_env_os as *const u8),
    ]
}
