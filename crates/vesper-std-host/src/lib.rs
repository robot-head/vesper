//! The host side of `@vesper-std/fs`, `@vesper-std/process` and
//! `@vesper-std/env` (self-hosting wave SH0 lane 3, #1125).
//!
//! Each package imports one `.vsys` library (`StdFs`, `StdProcess`,
//! `StdEnv`, in `packages/vesper-std/<pkg>/<pkg>.vsys`) whose functions are
//! the `extern "C"` functions of this crate. Vesper code calls them through
//! `Ffi[StdFs]` and friends; the JIT binds them through [`symbols`], an
//! object links them by name.
//!
//! The ABI is the `.vsys` one: scalars, opaque handle words (a `Box`
//! pointer here), and byte slices lent as `(ptr, len)`. Paths, arguments
//! and names cross as byte slices, never as NUL-terminated strings, so the
//! shim owns the conversion to the platform's path type (bytes on Unix,
//! UTF-8 then UTF-16 on Windows). A call that can fail writes an error word
//! through an `inout u64`: the [`IoKind`] code in the low 8 bits and the raw
//! OS error (as `u32`, when there is one) in the high 32 bits. Results that
//! do not fit a caller's buffer return the length needed and copy nothing,
//! so the Vesper side grows its buffer and asks again.
//!
//! Every handle kind has a live counter ([`live_handles`]), which the leak
//! tests check is back to zero after each scenario.
#![allow(clippy::missing_safety_doc)]

use std::sync::atomic::{AtomicI64, Ordering};

pub mod env;
pub mod fs;
pub mod process;

/// The error kinds an I/O call reports, as `IoError.kind` in Vesper
/// (`@vesper-std/fs::error`). Stable numbers: they are part of the ABI.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoKind {
    None = 0,
    NotFound = 1,
    PermissionDenied = 2,
    AlreadyExists = 3,
    NotADirectory = 4,
    IsADirectory = 5,
    DirectoryNotEmpty = 6,
    InvalidInput = 7,
    Interrupted = 8,
    UnexpectedEof = 9,
    WriteZero = 10,
    Unsupported = 11,
    OutOfMemory = 12,
    Other = 13,
}

impl IoKind {
    pub fn of(e: &std::io::Error) -> Self {
        use std::io::ErrorKind as K;
        match e.kind() {
            K::NotFound => Self::NotFound,
            K::PermissionDenied => Self::PermissionDenied,
            K::AlreadyExists => Self::AlreadyExists,
            K::NotADirectory => Self::NotADirectory,
            K::IsADirectory => Self::IsADirectory,
            K::DirectoryNotEmpty => Self::DirectoryNotEmpty,
            K::InvalidInput | K::InvalidData | K::InvalidFilename => Self::InvalidInput,
            K::Interrupted => Self::Interrupted,
            K::UnexpectedEof => Self::UnexpectedEof,
            K::WriteZero => Self::WriteZero,
            K::Unsupported => Self::Unsupported,
            K::OutOfMemory => Self::OutOfMemory,
            _ => Self::Other,
        }
    }
}

/// The error word of `e`: kind in the low 8 bits, raw OS error in the high
/// 32.
pub fn error_word(e: &std::io::Error) -> u64 {
    let os = e.raw_os_error().map_or(0, |c| u64::from(c as u32));
    (os << 32) | IoKind::of(e) as u64
}

/// The error word of a kind with no OS error.
pub fn kind_word(kind: IoKind) -> u64 {
    kind as u64
}

/// Write `word` through the caller's `inout u64` error slot.
///
/// # Safety
/// `err` is the `.vsys` `nocapture inout u64`: valid for one write.
pub(crate) unsafe fn set_err(err: *mut u64, word: u64) {
    if !err.is_null() {
        // SAFETY: the caller lends the slot for the call.
        unsafe { *err = word };
    }
}

/// The bytes of a lent `nocapture in CSlice[u8]`.
///
/// # Safety
/// `ptr` is valid for `len` bytes for the call (the `.vsys` contract; it is
/// never NULL, even when empty).
pub(crate) unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 || ptr.is_null() {
        return &[];
    }
    // SAFETY: as documented.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// The bytes of a lent `nocapture inout CSlice[u8]`.
///
/// # Safety
/// As [`bytes`], and the caller lends them exclusively.
pub(crate) unsafe fn bytes_mut<'a>(ptr: *mut u8, len: usize) -> &'a mut [u8] {
    if len == 0 || ptr.is_null() {
        return &mut [];
    }
    // SAFETY: as documented.
    unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}

/// Copy `data` into the caller's buffer when it fits; return its length
/// either way (the "ask again with a bigger buffer" protocol).
pub(crate) fn copy_out(data: &[u8], buf: &mut [u8]) -> u64 {
    if data.len() <= buf.len() {
        buf[..data.len()].copy_from_slice(data);
    }
    data.len() as u64
}

/// A path from the bytes Vesper passes. Unix paths are arbitrary bytes;
/// Windows paths must be UTF-8 (they are converted to UTF-16).
pub fn path_of(raw: &[u8]) -> Result<std::path::PathBuf, std::io::Error> {
    os_string(raw).map(std::path::PathBuf::from)
}

/// An `OsString` from the bytes Vesper passes (see [`path_of`]).
pub fn os_string(raw: &[u8]) -> Result<std::ffi::OsString, std::io::Error> {
    if raw.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "NUL byte in a path, argument or name",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(std::ffi::OsStr::from_bytes(raw).to_owned())
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(raw)
            .map(std::ffi::OsString::from)
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a path, argument or name is not UTF-8",
                )
            })
    }
}

/// The bytes of an `OsStr` as Vesper sees them: the raw bytes on Unix, UTF-8
/// on Windows (unpaired surrogates become U+FFFD).
pub fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        s.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        s.to_string_lossy().into_owned().into_bytes()
    }
}

/// Handle kinds, for the live counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleKind {
    File = 0,
    Dir = 1,
    Command = 2,
    Child = 3,
}

static LIVE: [AtomicI64; 4] = [
    AtomicI64::new(0),
    AtomicI64::new(0),
    AtomicI64::new(0),
    AtomicI64::new(0),
];

pub(crate) fn opened(kind: HandleKind) {
    LIVE[kind as usize].fetch_add(1, Ordering::SeqCst);
}

pub(crate) fn closed(kind: HandleKind) {
    LIVE[kind as usize].fetch_sub(1, Ordering::SeqCst);
}

/// Handles of `kind` created and not yet finalized, process-wide.
pub fn live(kind: HandleKind) -> i64 {
    LIVE[kind as usize].load(Ordering::SeqCst)
}

/// Live handles of every kind: `[file, dir, command, child]`.
pub fn live_handles() -> [i64; 4] {
    [
        live(HandleKind::File),
        live(HandleKind::Dir),
        live(HandleKind::Command),
        live(HandleKind::Child),
    ]
}

/// Box `value` as a handle word of `kind`.
pub(crate) fn new_handle<T>(kind: HandleKind, value: T) -> *mut T {
    opened(kind);
    Box::into_raw(Box::new(value))
}

/// Take back a handle created by [`new_handle`].
///
/// # Safety
/// `h` came from [`new_handle`] with the same `T` and is finalized once
/// (the `sink` import of a linear type).
pub(crate) unsafe fn free_handle<T>(kind: HandleKind, h: *mut T) -> T {
    closed(kind);
    // SAFETY: as documented.
    *unsafe { Box::from_raw(h) }
}

/// Every host function, by its `.vsys` symbol, for
/// `ForeignChecked::jit` (the JIT never resolves symbols from the process).
pub fn symbols() -> Vec<(&'static str, *const u8)> {
    let mut out = fs::symbols();
    out.extend(process::symbols());
    out.extend(env::symbols());
    out
}
