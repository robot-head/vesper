//! `sys library CasFs` (vesper/src/lib.ves): the operating-system calls of
//! the Vesper store port, over the platform helpers in `fsutil`. Each call
//! is one system operation; the store's rules (layout, staging,
//! publication, verification, repair) are all in Vesper.
//!
//! Errors: a failing call writes an error word, the kind in the low 8 bits
//! ([`KIND_NOT_FOUND`], [`KIND_EXISTS`], [`KIND_OTHER`]) and the index plus
//! one of the `io::Error`, kept in this thread's error list, above them.
//! The Rust wrapper (src/port.rs) takes the error back by that index, so a
//! `CasError::Io` from the port carries the very `io::Error` the Rust store
//! would have.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fsutil::{self, Publish};

pub(crate) const KIND_NOT_FOUND: u64 = 1;
pub(crate) const KIND_EXISTS: u64 = 2;
pub(crate) const KIND_OTHER: u64 = 9;

thread_local! {
    static ERRORS: RefCell<Vec<io::Error>> = const { RefCell::new(Vec::new()) };
}

/// Forget the errors of earlier calls (at the start of each port call).
pub(crate) fn clear_errors() {
    ERRORS.with(|e| e.borrow_mut().clear());
}

/// The error a call reported with `word`.
pub(crate) fn take_error(word: u64) -> io::Error {
    let index = (word >> 8) as usize;
    ERRORS
        .with(|e| {
            let mut e = e.borrow_mut();
            index
                .checked_sub(1)
                .and_then(|i| e.get_mut(i))
                .map(|slot| std::mem::replace(slot, io::Error::other("error already taken")))
        })
        .unwrap_or_else(|| io::Error::other("unrecorded store error"))
}

fn record(e: io::Error) -> u64 {
    let kind = match e.kind() {
        io::ErrorKind::NotFound => KIND_NOT_FOUND,
        io::ErrorKind::AlreadyExists => KIND_EXISTS,
        _ => KIND_OTHER,
    };
    ERRORS.with(|errors| {
        let mut errors = errors.borrow_mut();
        errors.push(e);
        ((errors.len() as u64) << 8) | kind
    })
}

/// The path in `(ptr, len)`: bytes the port built from paths the crate
/// gave it (their `OsStr` encoded bytes) joined by ASCII separators.
unsafe fn path<'a>(ptr: *const u8, len: usize) -> &'a Path {
    // SAFETY: the `.vsys` contract lends `len` bytes at `ptr` for the call.
    let bytes = unsafe { slice(ptr, len) };
    // SAFETY: the bytes are encoded `OsStr` bytes (see above), split and
    // joined only at ASCII characters.
    Path::new(unsafe { OsStr::from_encoded_bytes_unchecked(bytes) })
}

unsafe fn slice<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: lent for the call (see `path`).
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

unsafe fn slice_mut<'a>(ptr: *mut u8, len: usize) -> &'a mut [u8] {
    if len == 0 {
        &mut []
    } else {
        // SAFETY: lent `inout` for the call.
        unsafe { std::slice::from_raw_parts_mut(ptr, len) }
    }
}

/// Write `err` and turn `r` into a status: 0 on success, 1 on error.
unsafe fn status(err: *mut u64, r: io::Result<()>) -> u32 {
    let (word, s) = match r {
        Ok(()) => (0, 0),
        Err(e) => (record(e), 1),
    };
    // SAFETY: the error slot is lent for the call.
    unsafe { *err = word };
    s
}

unsafe fn value<T>(err: *mut u64, r: io::Result<T>, fallback: T) -> T {
    match r {
        Ok(v) => {
            // SAFETY: lent for the call.
            unsafe { *err = 0 };
            v
        }
        Err(e) => {
            // SAFETY: lent for the call.
            unsafe { *err = record(e) };
            fallback
        }
    }
}

/// An open file (`linear extern type CasFile`).
pub struct CasFile(File);

/// A directory listing (`linear extern type CasDir`): the entry names, or
/// the error that ended the listing, in the order the OS gave them.
pub struct CasDir {
    entries: std::vec::IntoIter<io::Result<Vec<u8>>>,
    pending: Option<Vec<u8>>,
}

/// A reader lent to one port call (`affine extern type Reader`). A panic
/// in the reader cannot unwind through the Vesper frames, so the call
/// reports it as a read error and keeps it here for the wrapper to resume
/// once the port has cleaned up.
pub struct Reader<'a> {
    reader: RefCell<&'a mut dyn Read>,
    panic: RefCell<Option<Box<dyn std::any::Any + Send>>>,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(reader: &'a mut dyn Read) -> Self {
        Reader {
            reader: RefCell::new(reader),
            panic: RefCell::new(None),
        }
    }

    /// Resume a panic the reader raised during the call.
    pub(crate) fn resume_panic(&self) {
        if let Some(p) = self.panic.borrow_mut().take() {
            std::panic::resume_unwind(p);
        }
    }
}

/// `wire struct CasMeta`.
#[repr(C)]
pub struct CasMeta {
    size: u64,
    mtime_s: i64,
    mtime_ns: u32,
    kind: u32,
}

/// `CasMeta.kind`: 1 file, 2 directory, 3 symlink, 4 FIFO, 5 socket, 6
/// device, 7 another special file (the store's `file_kind` names).
fn kind_of(meta: &fs::Metadata) -> u32 {
    let ft = meta.file_type();
    if ft.is_symlink() {
        return 3;
    }
    if ft.is_dir() {
        return 2;
    }
    if ft.is_file() {
        return 1;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if ft.is_fifo() {
            return 4;
        }
        if ft.is_socket() {
            return 5;
        }
        if ft.is_block_device() || ft.is_char_device() {
            return 6;
        }
    }
    7
}

fn boxed<T>(v: T) -> *mut T {
    Box::into_raw(Box::new(v))
}

/// The path separator.
#[unsafe(no_mangle)]
pub extern "C" fn cfs_sep() -> u8 {
    std::path::MAIN_SEPARATOR as u8
}

/// The parts of `fsutil::unique_name`: process id, sequence number and
/// nanoseconds since the Unix epoch, as three little-endian words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_unique(out: *mut u8, len: usize) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // SAFETY: lent `inout` for the call.
    let out = unsafe { slice_mut(out, len) };
    for (i, w) in [u64::from(std::process::id()), seq, nanos]
        .into_iter()
        .enumerate()
    {
        if let Some(dst) = out.get_mut(i * 8..i * 8 + 8) {
            dst.copy_from_slice(&w.to_le_bytes());
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_create_dir_all(p: *const u8, n: usize, err: *mut u64) -> u32 {
    // SAFETY: the `.vsys` contract lends the path and error slot.
    unsafe { status(err, fs::create_dir_all(path(p, n))) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_create_dir(p: *const u8, n: usize, err: *mut u64) -> u32 {
    // SAFETY: as above.
    unsafe { status(err, fs::create_dir(path(p, n))) }
}

/// The canonical form of `path`, written to `out` when it fits. Returns
/// its full length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_canonicalize(
    p: *const u8,
    n: usize,
    out: *mut u8,
    out_len: usize,
    err: *mut u64,
) -> u64 {
    // SAFETY: as above, and `out` is lent `inout`.
    unsafe {
        let r = path(p, n).canonicalize().map(|c| {
            let bytes = c.into_os_string().into_encoded_bytes();
            let out = slice_mut(out, out_len);
            if bytes.len() <= out.len() {
                out[..bytes.len()].copy_from_slice(&bytes);
            }
            bytes.len() as u64
        });
        value(err, r, 0)
    }
}

/// The time now: seconds and nanoseconds since the Unix epoch, as two
/// little-endian words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_now(out: *mut u8, len: usize) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // SAFETY: lent `inout` for the call.
    let out = unsafe { slice_mut(out, len) };
    for (i, w) in [d.as_secs(), u64::from(d.subsec_nanos())]
        .into_iter()
        .enumerate()
    {
        if let Some(dst) = out.get_mut(i * 8..i * 8 + 8) {
            dst.copy_from_slice(&w.to_le_bytes());
        }
    }
}

/// `fs::symlink_metadata`. `mtime_ns` is `u32::MAX` when the modification
/// time is unknown (or before the epoch).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_meta(p: *const u8, n: usize, m: *mut CasMeta, err: *mut u64) -> u32 {
    // SAFETY: as above, and the metadata slot is lent `inout`.
    unsafe {
        let r = fs::symlink_metadata(path(p, n)).map(|meta| {
            let (s, ns) = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or((0, u32::MAX), |d| (d.as_secs() as i64, d.subsec_nanos()));
            *m = CasMeta {
                size: meta.len(),
                mtime_s: s,
                mtime_ns: ns,
                kind: kind_of(&meta),
            };
        });
        status(err, r)
    }
}

/// `fsutil::open_regular_nofollow`. NULL on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_open_regular(p: *const u8, n: usize, err: *mut u64) -> *mut CasFile {
    // SAFETY: as above.
    unsafe {
        let r = fsutil::open_regular_nofollow(path(p, n)).map(|f| boxed(CasFile(f)));
        value(err, r, std::ptr::null_mut())
    }
}

/// `fsutil::create_new`. NULL on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_create_new(p: *const u8, n: usize, err: *mut u64) -> *mut CasFile {
    // SAFETY: as above.
    unsafe {
        let r = fsutil::create_new(path(p, n)).map(|f| boxed(CasFile(f)));
        value(err, r, std::ptr::null_mut())
    }
}

/// Open (creating) the repair lock at `path` and take it exclusively; the
/// lock is released when the handle is closed. NULL on error, with the
/// error's operation in `stage`: 0 open, 1 lock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_open_lock(
    p: *const u8,
    n: usize,
    stage: *mut u64,
    err: *mut u64,
) -> *mut CasFile {
    // SAFETY: as above.
    unsafe {
        *stage = 0;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path(p, n));
        let r = file.and_then(|f| {
            *stage = 1;
            f.lock()?;
            Ok(boxed(CasFile(f)))
        });
        value(err, r, std::ptr::null_mut())
    }
}

/// The open file's length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_file_len(f: *const CasFile, err: *mut u64) -> u64 {
    // SAFETY: a live handle lent `in`, and the error slot.
    unsafe { value(err, (*f).0.metadata().map(|m| m.len()), 0) }
}

/// Whether the open file is marked executable (`fsutil::is_executable`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_file_exec(f: *const CasFile, err: *mut u64) -> u32 {
    // SAFETY: as above.
    unsafe {
        let r = (*f)
            .0
            .metadata()
            .map(|m| u32::from(fsutil::is_executable(&m)));
        value(err, r, 0)
    }
}

/// One read into `buf`, retried on `Interrupted`. 0 at end of file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_read(f: *mut CasFile, buf: *mut u8, len: usize, err: *mut u64) -> u64 {
    // SAFETY: a live handle lent `inout`, the buffer and the error slot.
    unsafe {
        let (f, buf) = (&mut *f, slice_mut(buf, len));
        let r = loop {
            match f.0.read(buf) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                other => break other,
            }
        };
        value(err, r.map(|n| n as u64), 0)
    }
}

/// Write all of `buf`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_write(
    f: *mut CasFile,
    buf: *const u8,
    len: usize,
    err: *mut u64,
) -> u32 {
    // SAFETY: as above.
    unsafe { status(err, (*f).0.write_all(slice(buf, len))) }
}

/// `File::sync_all`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_sync(f: *mut CasFile, err: *mut u64) -> u32 {
    // SAFETY: as above.
    unsafe { status(err, (*f).0.sync_all()) }
}

/// `fsutil::make_readonly`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_make_readonly(f: *mut CasFile, err: *mut u64) -> u32 {
    // SAFETY: as above.
    unsafe { status(err, fsutil::make_readonly(&(*f).0)) }
}

/// Close the file (the finalizer of `CasFile`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_close(f: *mut CasFile) {
    // SAFETY: `sink` hands over the handle `Box::into_raw` made, once.
    drop(unsafe { Box::from_raw(f) });
}

/// `fsutil::publish_noreplace` (`dir` false) or `publish_dir_noreplace`:
/// 0 published, 1 something exists at `dest`, 2 error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_publish(
    a: *const u8,
    an: usize,
    b: *const u8,
    bn: usize,
    dir: bool,
    err: *mut u64,
) -> u32 {
    // SAFETY: as above.
    unsafe {
        let (from, to) = (path(a, an), path(b, bn));
        let r = if dir {
            fsutil::publish_dir_noreplace(from, to)
        } else {
            fsutil::publish_noreplace(from, to)
        };
        let r = r.map(|p| match p {
            Publish::Published => 0,
            Publish::Exists => 1,
        });
        value(err, r, 2)
    }
}

/// `fsutil::remove_staged_file`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_remove_staged(p: *const u8, n: usize, err: *mut u64) -> u32 {
    // SAFETY: as above.
    unsafe { status(err, fsutil::remove_staged_file(path(p, n))) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_remove_file(p: *const u8, n: usize, err: *mut u64) -> u32 {
    // SAFETY: as above.
    unsafe { status(err, fs::remove_file(path(p, n))) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_remove_dir_all(p: *const u8, n: usize, err: *mut u64) -> u32 {
    // SAFETY: as above.
    unsafe { status(err, fs::remove_dir_all(path(p, n))) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_rename(
    a: *const u8,
    an: usize,
    b: *const u8,
    bn: usize,
    err: *mut u64,
) -> u32 {
    // SAFETY: as above.
    unsafe { status(err, fs::rename(path(a, an), path(b, bn))) }
}

/// `fsutil::sync_dir`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_sync_dir(p: *const u8, n: usize, err: *mut u64) -> u32 {
    // SAFETY: as above.
    unsafe { status(err, fsutil::sync_dir(path(p, n))) }
}

/// `fsutil::set_workspace_mode`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_set_workspace_mode(
    p: *const u8,
    n: usize,
    exec: bool,
    err: *mut u64,
) -> u32 {
    // SAFETY: as above.
    unsafe { status(err, fsutil::set_workspace_mode(path(p, n), exec)) }
}

/// `reflink_copy::reflink`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_reflink(
    a: *const u8,
    an: usize,
    b: *const u8,
    bn: usize,
    err: *mut u64,
) -> u32 {
    // SAFETY: as above.
    unsafe { status(err, reflink_copy::reflink(path(a, an), path(b, bn))) }
}

/// List the directory at `path`. NULL when it cannot be opened; an error
/// while listing is reported by `cfs_dir_next` in its place.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_read_dir(p: *const u8, n: usize, err: *mut u64) -> *mut CasDir {
    // SAFETY: as above.
    unsafe {
        let r = fs::read_dir(path(p, n)).map(|entries| {
            let entries: Vec<io::Result<Vec<u8>>> = entries
                .map(|e| e.map(|e| e.file_name().into_encoded_bytes()))
                .collect();
            boxed(CasDir {
                entries: entries.into_iter(),
                pending: None,
            })
        });
        value(err, r, std::ptr::null_mut())
    }
}

/// The next entry's name: its length (written to `name` when it fits;
/// otherwise the entry stays next, for a call with a larger buffer), -1
/// at the end, -2 on an error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_dir_next(
    d: *mut CasDir,
    name: *mut u8,
    len: usize,
    err: *mut u64,
) -> i64 {
    // SAFETY: a live handle lent `inout`, the buffer and the error slot.
    unsafe {
        let d = &mut *d;
        *err = 0;
        let next = match d.pending.take() {
            Some(n) => n,
            None => match d.entries.next() {
                None => return -1,
                Some(Err(e)) => {
                    *err = record(e);
                    return -2;
                }
                Some(Ok(n)) => n,
            },
        };
        let out = slice_mut(name, len);
        let n = next.len() as i64;
        if next.len() <= out.len() {
            out[..next.len()].copy_from_slice(&next);
        } else {
            d.pending = Some(next);
        }
        n
    }
}

/// Close a listing (the finalizer of `CasDir`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_dir_close(d: *mut CasDir) {
    // SAFETY: as for `cfs_close`.
    drop(unsafe { Box::from_raw(d) });
}

/// One read from the lent reader, retried on `Interrupted`. 0 at the end.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfs_reader_read(
    r: *const Reader<'_>,
    buf: *mut u8,
    len: usize,
    err: *mut u64,
) -> u64 {
    // SAFETY: the handle word is the address of the `Reader` src/port.rs
    // lent for this call; the buffer and error slot are lent too.
    unsafe {
        let (r, buf) = (&*r, slice_mut(buf, len));
        let mut reader = r.reader.borrow_mut();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            loop {
                match reader.read(buf) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    other => break other,
                }
            }
        }));
        let res = match res {
            Ok(res) => res,
            Err(p) => {
                *r.panic.borrow_mut() = Some(p);
                Err(io::Error::other("the reader panicked"))
            }
        };
        value(err, res.map(|n| n as u64), 0)
    }
}

/// The path of port bytes, for error values.
pub(crate) fn path_buf(bytes: &[u8]) -> PathBuf {
    // SAFETY: as in `path`.
    PathBuf::from(unsafe { OsStr::from_encoded_bytes_unchecked(bytes) })
}
