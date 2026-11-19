//! `sys library StdFs` (packages/vesper-std/fs/fs.vsys): files as linear
//! handles, directory listings, metadata, renames and removal.
use crate::*;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::sync::atomic::AtomicU64;

/// An open file (`linear extern type FsFile`).
pub struct FsFile {
    file: File,
}

/// A directory listing read at open (`linear extern type FsDir`): names
/// in the order the OS returned them, and the next one to hand out.
pub struct FsDir {
    entries: Vec<(Vec<u8>, u32)>,
    next: usize,
}

/// The `.vsys` wire struct `FsMeta`: 24 bytes, no padding.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct FsMeta {
    pub size: u64,
    pub mtime_s: i64,
    pub mtime_ns: u32,
    pub kind: u32,
}

/// `FsMeta.kind` and the entry kind of a listing.
pub const KIND_FILE: u32 = 1;
pub const KIND_DIR: u32 = 2;
pub const KIND_SYMLINK: u32 = 3;
pub const KIND_OTHER: u32 = 4;

/// Open modes of [`vstd_fs_open`].
pub const OPEN_READ: u32 = 0;
pub const OPEN_TRUNCATE: u32 = 1;
pub const OPEN_CREATE_NEW: u32 = 2;
pub const OPEN_APPEND: u32 = 3;

fn kind_of(t: std::fs::FileType) -> u32 {
    if t.is_symlink() {
        KIND_SYMLINK
    } else if t.is_dir() {
        KIND_DIR
    } else if t.is_file() {
        KIND_FILE
    } else {
        KIND_OTHER
    }
}

/// Run `f` on the path in `(ptr, len)`; on error write `err` and return
/// `fallback`.
unsafe fn with_path<T>(
    ptr: *const u8,
    len: usize,
    err: *mut u64,
    fallback: T,
    f: impl FnOnce(&std::path::Path) -> std::io::Result<T>,
) -> T {
    // SAFETY: the path slice is lent for the call.
    let raw = unsafe { bytes(ptr, len) };
    match path_of(raw).and_then(|p| f(&p)) {
        Ok(v) => {
            // SAFETY: the error slot is lent for the call.
            unsafe { set_err(err, 0) };
            v
        }
        Err(e) => {
            // SAFETY: as above.
            unsafe { set_err(err, error_word(&e)) };
            fallback
        }
    }
}

/// Open `path` in `mode` ([`OPEN_READ`] and friends). NULL on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_open(
    path: *const u8,
    path_len: usize,
    mode: u32,
    err: *mut u64,
) -> *mut FsFile {
    // SAFETY: the `.vsys` contract lends the path and error slot.
    unsafe {
        with_path(path, path_len, err, std::ptr::null_mut(), |p| {
            let mut o = OpenOptions::new();
            match mode {
                OPEN_READ => o.read(true),
                OPEN_TRUNCATE => o.write(true).create(true).truncate(true),
                OPEN_CREATE_NEW => o.write(true).create_new(true),
                OPEN_APPEND => o.append(true).create(true),
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "unknown open mode",
                    ));
                }
            };
            let file = o.open(p)?;
            Ok(new_handle(HandleKind::File, FsFile { file }))
        })
    }
}

/// The file's current length in bytes (0 if it cannot be read).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_file_len(f: *const FsFile) -> u64 {
    // SAFETY: a live handle lent `in`.
    let f = unsafe { &*f };
    f.file.metadata().map_or(0, |m| m.len())
}

/// Read into the whole buffer, stopping early only at end of file. Returns
/// the bytes read (fewer than the buffer only at end of file).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_read(
    f: *mut FsFile,
    buf: *mut u8,
    len: usize,
    err: *mut u64,
) -> u64 {
    // SAFETY: a live handle lent `inout`, and the buffer and error slot.
    let (f, buf) = unsafe { (&mut *f, bytes_mut(buf, len)) };
    let mut done = 0;
    while done < buf.len() {
        match f.file.read(&mut buf[done..]) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                // SAFETY: lent for the call.
                unsafe { set_err(err, error_word(&e)) };
                return done as u64;
            }
        }
    }
    // SAFETY: lent for the call.
    unsafe { set_err(err, 0) };
    done as u64
}

/// Write the whole buffer. Returns 0 on success, 1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_write(
    f: *mut FsFile,
    buf: *const u8,
    len: usize,
    err: *mut u64,
) -> u32 {
    // SAFETY: a live handle lent `inout`, and the buffer and error slot.
    let (f, buf) = unsafe { (&mut *f, bytes(buf, len)) };
    status(err, f.file.write_all(buf))
}

/// Flush the file's data and metadata to storage (`fsync`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_sync(f: *mut FsFile, err: *mut u64) -> u32 {
    // SAFETY: a live handle lent `inout`.
    let f = unsafe { &mut *f };
    status(err, f.file.sync_all())
}

/// Close the file (the finalizer of `FsFile`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_close(f: *mut FsFile) {
    // SAFETY: `sink` consumes the handle exactly once.
    drop(unsafe { free_handle(HandleKind::File, f) });
}

fn status(err: *mut u64, r: std::io::Result<()>) -> u32 {
    match r {
        Ok(()) => {
            // SAFETY: lent for the call.
            unsafe { set_err(err, 0) };
            0
        }
        Err(e) => {
            // SAFETY: lent for the call.
            unsafe { set_err(err, error_word(&e)) };
            1
        }
    }
}

/// Rename `from` to `to`, replacing `to` if it exists (on Windows too).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_rename(
    from: *const u8,
    from_len: usize,
    to: *const u8,
    to_len: usize,
    err: *mut u64,
) -> u32 {
    // SAFETY: both paths are lent for the call.
    let (a, b) = unsafe { (bytes(from, from_len), bytes(to, to_len)) };
    status(
        err,
        path_of(a).and_then(|a| path_of(b).and_then(|b| std::fs::rename(a, b))),
    )
}

/// Remove a file (not a directory).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_remove_file(path: *const u8, len: usize, err: *mut u64) -> u32 {
    // SAFETY: lent for the call.
    unsafe { with_path(path, len, err, 1, |p| std::fs::remove_file(p).map(|()| 0)) }
}

/// Remove a directory: an empty one, or with `recursive` everything in it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_remove_dir(
    path: *const u8,
    len: usize,
    recursive: bool,
    err: *mut u64,
) -> u32 {
    // SAFETY: lent for the call.
    unsafe {
        with_path(path, len, err, 1, |p| {
            if recursive {
                std::fs::remove_dir_all(p)
            } else {
                std::fs::remove_dir(p)
            }
            .map(|()| 0)
        })
    }
}

/// Create a directory, with `recursive` its missing parents too (and then
/// an existing directory is not an error).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_create_dir(
    path: *const u8,
    len: usize,
    recursive: bool,
    err: *mut u64,
) -> u32 {
    // SAFETY: lent for the call.
    unsafe {
        with_path(path, len, err, 1, |p| {
            if recursive {
                std::fs::create_dir_all(p)
            } else {
                std::fs::create_dir(p)
            }
            .map(|()| 0)
        })
    }
}

/// Metadata of `path` (following symlinks when `follow`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_metadata(
    path: *const u8,
    len: usize,
    follow: bool,
    meta: *mut FsMeta,
    err: *mut u64,
) -> u32 {
    // SAFETY: lent for the call.
    unsafe {
        with_path(path, len, err, 1, |p| {
            let m = if follow {
                std::fs::metadata(p)?
            } else {
                std::fs::symlink_metadata(p)?
            };
            let (mtime_s, mtime_ns) = match m.modified() {
                Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
                    Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
                    Err(e) => {
                        // Before 1970: floor to whole seconds below zero.
                        let d = e.duration();
                        let (s, ns) = (d.as_secs() as i64, d.subsec_nanos());
                        if ns == 0 {
                            (-s, 0)
                        } else {
                            (-s - 1, 1_000_000_000 - ns)
                        }
                    }
                },
                Err(_) => (0, 0),
            };
            *meta = FsMeta {
                size: m.len(),
                mtime_s,
                mtime_ns,
                kind: kind_of(m.file_type()),
            };
            Ok(0)
        })
    }
}

/// Flush a directory's entries to storage after a rename (Unix `fsync` on
/// the directory; a no-op on Windows, where `MoveFileEx` is durable).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_sync_dir(path: *const u8, len: usize, err: *mut u64) -> u32 {
    // SAFETY: lent for the call.
    unsafe {
        with_path(path, len, err, 1, |p| {
            #[cfg(unix)]
            File::open(p)?.sync_all()?;
            #[cfg(not(unix))]
            let _ = p;
            Ok(0)
        })
    }
}

/// The length of the directory part of `path`, its trailing separator
/// included: 4 for `a/b/c` (`a/b/`), 0 for a bare name. `/` separates on
/// every platform; `\` and a drive prefix (`C:`) only on Windows, since on
/// Unix a backslash is an ordinary name byte.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_parent_len(path: *const u8, len: usize) -> u64 {
    // SAFETY: lent for the call.
    let raw = unsafe { bytes(path, len) };
    let windows = cfg!(windows);
    let sep = |b: u8| b == b'/' || (windows && b == b'\\');
    match raw.iter().rposition(|&b| sep(b)) {
        Some(i) => i as u64 + 1,
        None if windows && raw.len() >= 2 && raw[1] == b':' => 2,
        None => 0,
    }
}

static UNIQUE: AtomicU64 = AtomicU64::new(0);

/// A process-unique number for temporary names: the process id in the high
/// 32 bits and a counter in the low 32.
#[unsafe(no_mangle)]
pub extern "C" fn vstd_fs_unique() -> u64 {
    (u64::from(std::process::id()) << 32)
        | (UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xffff_ffff)
}

/// List a directory. NULL on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_read_dir(
    path: *const u8,
    len: usize,
    err: *mut u64,
) -> *mut FsDir {
    // SAFETY: lent for the call.
    unsafe {
        with_path(path, len, err, std::ptr::null_mut(), |p| {
            let mut entries = vec![];
            for e in std::fs::read_dir(p)? {
                let e = e?;
                let kind = e.file_type().map_or(KIND_OTHER, kind_of);
                entries.push((os_bytes(&e.file_name()), kind));
            }
            Ok(new_handle(HandleKind::Dir, FsDir { entries, next: 0 }))
        })
    }
}

/// The next entry's name into `buf` and its kind into `kind`. Returns the
/// name's length, or -1 after the last entry. A name longer than `buf` is
/// not consumed: its length comes back and the call can be repeated with a
/// bigger buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_dir_next(
    d: *mut FsDir,
    buf: *mut u8,
    len: usize,
    kind: *mut u32,
) -> i64 {
    // SAFETY: a live handle lent `inout`; the buffer and kind slot.
    let (d, buf) = unsafe { (&mut *d, bytes_mut(buf, len)) };
    let Some((name, k)) = d.entries.get(d.next) else {
        return -1;
    };
    let n = copy_out(name, buf);
    if n as usize <= buf.len() {
        d.next += 1;
        // SAFETY: lent for the call.
        unsafe { *kind = *k };
    }
    n as i64
}

/// Close a listing (the finalizer of `FsDir`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vstd_fs_dir_close(d: *mut FsDir) {
    // SAFETY: `sink` consumes the handle exactly once.
    drop(unsafe { free_handle(HandleKind::Dir, d) });
}

pub(crate) fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("vstd_fs_open", vstd_fs_open as *const u8),
        ("vstd_fs_file_len", vstd_fs_file_len as *const u8),
        ("vstd_fs_read", vstd_fs_read as *const u8),
        ("vstd_fs_write", vstd_fs_write as *const u8),
        ("vstd_fs_sync", vstd_fs_sync as *const u8),
        ("vstd_fs_close", vstd_fs_close as *const u8),
        ("vstd_fs_rename", vstd_fs_rename as *const u8),
        ("vstd_fs_remove_file", vstd_fs_remove_file as *const u8),
        ("vstd_fs_remove_dir", vstd_fs_remove_dir as *const u8),
        ("vstd_fs_create_dir", vstd_fs_create_dir as *const u8),
        ("vstd_fs_metadata", vstd_fs_metadata as *const u8),
        ("vstd_fs_sync_dir", vstd_fs_sync_dir as *const u8),
        ("vstd_fs_parent_len", vstd_fs_parent_len as *const u8),
        ("vstd_fs_unique", vstd_fs_unique as *const u8),
        ("vstd_fs_read_dir", vstd_fs_read_dir as *const u8),
        ("vstd_fs_dir_next", vstd_fs_dir_next as *const u8),
        ("vstd_fs_dir_close", vstd_fs_dir_close as *const u8),
    ]
}
