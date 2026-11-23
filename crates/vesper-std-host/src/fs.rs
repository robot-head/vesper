//! `sys library StdFs` on Windows (packages/vesper-std/fs/
//! fs-windows-x86_64.vsys).
//!
//! On Linux and macOS `@vesper-std/fs` imports the C library itself
//! (fs-linux-*.vsys, fs-macos-aarch64.vsys; SH6 lane 13, #1408), so nothing
//! here runs there. Windows keeps these functions until Vesper calls Win32
//! itself (gap G7, UTF-16 paths): each has the C signature of the libc
//! function its `.vsys` declaration names, over UTF-8 paths and `std::fs`.
//!
//! - A descriptor indexes this module's table of open files and of the
//!   directories `openat` opened (by path: `remove_dir_all` walks the tree
//!   by name under them, and a directory link is refused, not entered).
//! - `stat`, `lstat` and `fstat` fill the Linux x86_64 `struct stat` fields
//!   src/sys.ves reads (`st_mode` at byte 24, `st_size` at 48, `st_mtim`
//!   at 88), and `readdir` returns a record laid out as glibc's `struct
//!   dirent` (`d_type` at byte 18, `d_name` at 19).
//! - A failure returns -1 (or NULL) with the Win32 error as the thread's
//!   last error, or `ERROR_INVALID_NAME` (123) when the path is not UTF-8.
//! - `vstd_memcpy` and `vstd_strchr` are `memcpy` and `strchr`, which
//!   src/sys.ves reads `struct dirent` records with.
use crate::*;
use std::ffi::{CStr, c_char};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::sync::Mutex;

/// `ERROR_INVALID_NAME`: a path that is not UTF-8.
const ERROR_INVALID_NAME: u32 = 123;
/// `ERROR_CALL_NOT_IMPLEMENTED`: `fcntl`.
const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
/// `ERROR_INVALID_HANDLE`: a descriptor that is not open.
const ERROR_INVALID_HANDLE: u32 = 6;
/// `ERROR_DIRECTORY` and `ERROR_CANT_RESOLVE_FILENAME`: what `openat` with
/// `O_DIRECTORY | O_NOFOLLOW` reports for a file and for a link (the
/// `ENOTDIR` and `ELOOP` of fs-windows-x86_64.vsys).
const ERROR_DIRECTORY: u32 = 267;
const ERROR_CANT_RESOLVE_FILENAME: u32 = 1921;

/// The `open` flags fs-windows-x86_64.vsys declares (Linux's values).
const O_WRONLY: i32 = 1;
const O_CREAT: i32 = 64;
const O_EXCL: i32 = 128;
const O_TRUNC: i32 = 512;
const O_APPEND: i32 = 1024;
const O_DIRECTORY: i32 = 65536;
const AT_FDCWD: i32 = -100;
const AT_REMOVEDIR: i32 = 512;

/// The `st_mode` file types and `d_type` values src/sys.ves decodes.
const S_IFDIR: u64 = 0o040000;
const S_IFREG: u64 = 0o100000;
const S_IFLNK: u64 = 0o120000;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;
const DT_FIFO: u8 = 1;

/// What a descriptor names.
enum Open {
    File(File),
    Dir(std::path::PathBuf),
}

/// Open files and directories, indexed by descriptor.
static FILES: Mutex<Vec<Option<Open>>> = Mutex::new(Vec::new());

fn files() -> std::sync::MutexGuard<'static, Vec<Option<Open>>> {
    FILES.lock().unwrap_or_else(|e| e.into_inner())
}

/// A new descriptor for `open`.
fn add(open: Open) -> i32 {
    let mut table = files();
    let fd = match table.iter().position(Option::is_none) {
        Some(i) => {
            table[i] = Some(open);
            i
        }
        None => {
            table.push(Some(open));
            table.len() - 1
        }
    };
    fd as i32
}

/// `name` under the directory `dirfd` (`AT_FDCWD`: the current one).
fn under(dirfd: i32, name: std::path::PathBuf) -> std::io::Result<std::path::PathBuf> {
    if dirfd == AT_FDCWD {
        return Ok(name);
    }
    let table = files();
    match usize::try_from(dirfd).ok().and_then(|i| table.get(i)) {
        Some(Some(Open::Dir(d))) => Ok(d.join(name)),
        _ => Err(std::io::Error::from_raw_os_error(
            ERROR_INVALID_HANDLE as i32,
        )),
    }
}

/// Record `e` as the thread's last error.
fn fail(e: &std::io::Error) {
    set_last_error(e.raw_os_error().map_or(ERROR_INVALID_NAME, |c| c as u32));
}

/// 0, or -1 with the last error set.
fn status(r: std::io::Result<()>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            fail(&e);
            -1
        }
    }
}

/// The path at `path`.
///
/// # Safety
/// `path` is a NUL-terminated string for the call.
unsafe fn path(path: *const c_char) -> std::io::Result<std::path::PathBuf> {
    // SAFETY: as documented.
    path_of(unsafe { CStr::from_ptr(path) }.to_bytes())
}

/// Run `f` on the open file `fd`.
fn with_file<T>(fd: i32, fallback: T, f: impl FnOnce(&mut File) -> T) -> T {
    let mut table = files();
    match usize::try_from(fd).ok().and_then(|i| table.get_mut(i)) {
        Some(Some(Open::File(file))) => f(file),
        _ => {
            set_last_error(ERROR_INVALID_HANDLE);
            fallback
        }
    }
}

/// `open(path, flags, mode)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_open(p: *const c_char, flags: i32, _mode: i64) -> i32 {
    // SAFETY: the lowering passes a NUL-terminated copy for the call.
    let opened = unsafe { path(p) }.and_then(|p| {
        let mut o = OpenOptions::new();
        if flags & O_WRONLY == 0 {
            o.read(true);
        } else if flags & O_APPEND != 0 {
            o.append(true);
        } else {
            o.write(true);
        }
        if flags & O_CREAT != 0 {
            if flags & O_EXCL != 0 {
                o.create_new(true);
            } else {
                o.create(true);
            }
        }
        if flags & O_TRUNC != 0 {
            o.truncate(true);
        }
        o.open(p)
    });
    match opened {
        Ok(file) => add(Open::File(file)),
        Err(e) => {
            fail(&e);
            -1
        }
    }
}

/// `openat(dirfd, name, flags, mode)`: with `O_DIRECTORY` (as
/// `remove_dir_all` opens with `O_NOFOLLOW`) the directory itself, refusing
/// a link and a file; otherwise a file, as [`vstd_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_openat(dirfd: i32, p: *const c_char, flags: i32, mode: i64) -> i32 {
    // SAFETY: the lowering passes a NUL-terminated copy for the call.
    let full = match unsafe { path(p) }.and_then(|p| under(dirfd, p)) {
        Ok(full) => full,
        Err(e) => {
            fail(&e);
            return -1;
        }
    };
    if flags & O_DIRECTORY == 0 {
        let Ok(c) = std::ffi::CString::new(os_bytes(full.as_os_str())) else {
            set_last_error(ERROR_INVALID_NAME);
            return -1;
        };
        // SAFETY: `c` is a NUL-terminated string.
        return unsafe { vstd_open(c.as_ptr(), flags, mode) };
    }
    match std::fs::symlink_metadata(&full) {
        Ok(m) if m.file_type().is_symlink() => {
            set_last_error(ERROR_CANT_RESOLVE_FILENAME);
            -1
        }
        Ok(m) if !m.is_dir() => {
            set_last_error(ERROR_DIRECTORY);
            -1
        }
        Ok(_) => add(Open::Dir(full)),
        Err(e) => {
            fail(&e);
            -1
        }
    }
}

/// `unlinkat(dirfd, name, flags)`: the directory `name` with
/// `AT_REMOVEDIR`, else the file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_unlinkat(dirfd: i32, p: *const c_char, flags: i32) -> i32 {
    // SAFETY: the lowering passes a NUL-terminated copy for the call.
    status(
        unsafe { path(p) }
            .and_then(|p| under(dirfd, p))
            .and_then(|full| {
                if flags & AT_REMOVEDIR != 0 {
                    std::fs::remove_dir(full)
                } else {
                    std::fs::remove_file(full)
                }
            }),
    )
}

/// `read(fd, buf, n)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_read(fd: i32, buf: *mut u8, n: usize) -> i64 {
    // SAFETY: the caller lends `n` writable bytes for the call.
    let buf = unsafe { bytes_mut(buf, n) };
    with_file(fd, -1, |f| match f.read(buf) {
        Ok(n) => n as i64,
        Err(e) => {
            fail(&e);
            -1
        }
    })
}

/// `write(fd, buf, n)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_write(fd: i32, buf: *const u8, n: usize) -> i64 {
    // SAFETY: the caller lends `n` readable bytes for the call.
    let buf = unsafe { bytes(buf, n) };
    with_file(fd, -1, |f| match f.write(buf) {
        Ok(n) => n as i64,
        Err(e) => {
            fail(&e);
            -1
        }
    })
}

/// `fsync(fd)`.
#[unsafe(no_mangle)]
pub extern "C" fn vstd_fsync(fd: i32) -> i32 {
    with_file(fd, -1, |f| status(f.sync_all()))
}

/// `fcntl(fd, cmd, arg)`: src/sys.ves calls it only on macOS.
#[unsafe(no_mangle)]
pub extern "C" fn vstd_fcntl(_fd: i32, _cmd: i32, _arg: i64) -> i32 {
    set_last_error(ERROR_CALL_NOT_IMPLEMENTED);
    -1
}

/// `close(fd)`.
#[unsafe(no_mangle)]
pub extern "C" fn vstd_close(fd: i32) -> i32 {
    let mut table = files();
    match usize::try_from(fd).ok().and_then(|i| table.get_mut(i)) {
        Some(slot @ Some(_)) => {
            *slot = None;
            0
        }
        _ => {
            set_last_error(ERROR_INVALID_HANDLE);
            -1
        }
    }
}

/// Fill the words of `st` src/sys.ves reads from `m`.
///
/// # Safety
/// `st` points at the 18-word `Stat` the caller lends for the call.
unsafe fn fill(st: *mut u64, m: &std::fs::Metadata) {
    let t = m.file_type();
    let mode = if t.is_symlink() {
        S_IFLNK
    } else if t.is_dir() {
        S_IFDIR
    } else if t.is_file() {
        S_IFREG
    } else {
        0
    };
    let (secs, nanos) = match m.modified() {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => (d.as_secs() as i64, u64::from(d.subsec_nanos())),
            Err(e) => {
                // Before 1970: whole seconds below zero, then nanoseconds.
                let d = e.duration();
                let (s, ns) = (d.as_secs() as i64, d.subsec_nanos());
                if ns == 0 {
                    (-s, 0)
                } else {
                    (-s - 1, u64::from(1_000_000_000 - ns))
                }
            }
        },
        Err(_) => (0, 0),
    };
    // SAFETY: as documented; the words written are inside the 18.
    unsafe {
        std::ptr::write_bytes(st, 0, 18);
        *st.add(3) = mode;
        *st.add(6) = m.len();
        *st.add(11) = secs as u64;
        *st.add(12) = nanos;
    }
}

fn stat_with(
    p: *const c_char,
    st: *mut u64,
    get: impl FnOnce(&std::path::Path) -> std::io::Result<std::fs::Metadata>,
) -> i32 {
    // SAFETY: the lowering passes a NUL-terminated copy for the call.
    match unsafe { path(p) }.and_then(|p| get(&p)) {
        Ok(m) => {
            // SAFETY: the caller lends the `Stat`.
            unsafe { fill(st, &m) };
            0
        }
        Err(e) => {
            fail(&e);
            -1
        }
    }
}

/// `stat(path, st)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_stat(p: *const c_char, st: *mut u64) -> i32 {
    stat_with(p, st, |p| std::fs::metadata(p))
}

/// `lstat(path, st)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_lstat(p: *const c_char, st: *mut u64) -> i32 {
    stat_with(p, st, |p| std::fs::symlink_metadata(p))
}

/// `fstat(fd, st)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fstat(fd: i32, st: *mut u64) -> i32 {
    with_file(fd, -1, |f| match f.metadata() {
        Ok(m) => {
            // SAFETY: the caller lends the `Stat`.
            unsafe { fill(st, &m) };
            0
        }
        Err(e) => {
            fail(&e);
            -1
        }
    })
}

/// `rename(from, to)`, replacing `to` if it exists.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_rename(from: *const c_char, to: *const c_char) -> i32 {
    // SAFETY: both are NUL-terminated copies for the call.
    let (a, b) = unsafe { (path(from), path(to)) };
    status(a.and_then(|a| b.and_then(|b| std::fs::rename(a, b))))
}

/// `unlink(path)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_unlink(p: *const c_char) -> i32 {
    // SAFETY: a NUL-terminated copy for the call.
    status(unsafe { path(p) }.and_then(std::fs::remove_file))
}

/// `rmdir(path)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_rmdir(p: *const c_char) -> i32 {
    // SAFETY: a NUL-terminated copy for the call.
    status(unsafe { path(p) }.and_then(std::fs::remove_dir))
}

/// `mkdir(path, mode)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_mkdir(p: *const c_char, _mode: u32) -> i32 {
    // SAFETY: a NUL-terminated copy for the call.
    status(unsafe { path(p) }.and_then(std::fs::create_dir))
}

/// `getpid()`.
#[unsafe(no_mangle)]
pub extern "C" fn vstd_getpid() -> i32 {
    std::process::id() as i32
}

/// A listing read at `opendir`: each entry as a `struct dirent` record,
/// and the next one to hand out.
pub struct DirState {
    records: Vec<Vec<u8>>,
    next: usize,
}

/// `opendir(path)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_opendir(p: *const c_char) -> *mut DirState {
    // SAFETY: a NUL-terminated copy for the call.
    open_dir(unsafe { path(p) })
}

/// `fdopendir(fd)`: the listing of the directory `openat` opened, which
/// now owns the descriptor.
#[unsafe(no_mangle)]
pub extern "C" fn vstd_fdopendir(fd: i32) -> *mut DirState {
    let taken = {
        let mut table = files();
        match usize::try_from(fd).ok().and_then(|i| table.get_mut(i)) {
            Some(slot @ Some(Open::Dir(_))) => match slot.take() {
                Some(Open::Dir(d)) => Ok(d),
                _ => unreachable!(),
            },
            _ => Err(std::io::Error::from_raw_os_error(
                ERROR_INVALID_HANDLE as i32,
            )),
        }
    };
    open_dir(taken)
}

/// A listing of the directory at `p`, or NULL with the last error set.
fn open_dir(p: std::io::Result<std::path::PathBuf>) -> *mut DirState {
    let listed = p.and_then(|p| {
        let mut records = vec![];
        for e in std::fs::read_dir(p)? {
            let e = e?;
            let t = match e.file_type() {
                Ok(t) if t.is_symlink() => DT_LNK,
                Ok(t) if t.is_dir() => DT_DIR,
                Ok(t) if t.is_file() => DT_REG,
                _ => DT_FIFO,
            };
            let mut r = vec![0u8; 19];
            r[18] = t;
            r.extend(os_bytes(&e.file_name()));
            r.push(0);
            // src/sys.ves reads the 8 bytes from byte 16 of every record.
            r.resize(r.len().max(24), 0);
            records.push(r);
        }
        Ok(DirState { records, next: 0 })
    });
    match listed {
        Ok(d) => new_handle(HandleKind::Dir, d),
        Err(e) => {
            fail(&e);
            std::ptr::null_mut()
        }
    }
}

/// `readdir(dir)`: the next record, NULL after the last.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_readdir(d: *mut DirState) -> *const u8 {
    // SAFETY: a live listing lent `inout`.
    let d = unsafe { &mut *d };
    match d.records.get(d.next) {
        Some(r) => {
            d.next += 1;
            r.as_ptr()
        }
        None => {
            // The end, not an error: the listing was read at `opendir`.
            set_last_error(0);
            std::ptr::null()
        }
    }
}

/// `closedir(dir)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_closedir(d: *mut DirState) -> i32 {
    // SAFETY: `sink` consumes the handle exactly once.
    drop(unsafe { free_handle(HandleKind::Dir, d) });
    0
}

thread_local! {
    static ERRNO: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The word src/sys.ves clears before `readdir` (the shim reports errors
/// through the last error instead).
#[unsafe(no_mangle)]
pub extern "C" fn vstd_errno_address() -> *mut u32 {
    ERRNO.with(|e| e.as_ptr())
}

/// `memset(dst, byte, n)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_memset(dst: *mut u8, byte: i32, n: usize) -> *mut u8 {
    // SAFETY: the caller passes `n` writable bytes at `dst`.
    unsafe { std::ptr::write_bytes(dst, byte as u8, n) };
    dst
}

/// `bcopy(src, dst, n)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_bcopy(src: *const u8, dst: *mut u8, n: usize) {
    // SAFETY: the caller passes `n` readable bytes at `src` and lends `n`
    // writable ones at `dst`; `bcopy` allows them to overlap.
    unsafe { std::ptr::copy(src, dst, n) };
}

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

pub(crate) fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("vstd_memcpy", vstd_memcpy as *const u8),
        ("vstd_strchr", vstd_strchr as *const u8),
        ("vstd_open", vstd_open as *const u8),
        ("vstd_read", vstd_read as *const u8),
        ("vstd_write", vstd_write as *const u8),
        ("vstd_fsync", vstd_fsync as *const u8),
        ("vstd_fcntl", vstd_fcntl as *const u8),
        ("vstd_close", vstd_close as *const u8),
        ("vstd_fstat", vstd_fstat as *const u8),
        ("vstd_stat", vstd_stat as *const u8),
        ("vstd_lstat", vstd_lstat as *const u8),
        ("vstd_rename", vstd_rename as *const u8),
        ("vstd_unlink", vstd_unlink as *const u8),
        ("vstd_rmdir", vstd_rmdir as *const u8),
        ("vstd_mkdir", vstd_mkdir as *const u8),
        ("vstd_getpid", vstd_getpid as *const u8),
        ("vstd_opendir", vstd_opendir as *const u8),
        ("vstd_readdir", vstd_readdir as *const u8),
        ("vstd_closedir", vstd_closedir as *const u8),
        ("vstd_bcopy", vstd_bcopy as *const u8),
        ("vstd_openat", vstd_openat as *const u8),
        ("vstd_unlinkat", vstd_unlinkat as *const u8),
        ("vstd_fdopendir", vstd_fdopendir as *const u8),
        ("vstd_errno_address", vstd_errno_address as *const u8),
        ("vstd_memset", vstd_memset as *const u8),
    ]
}
