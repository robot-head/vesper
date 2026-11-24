//! Retired host-shim crate for `@vesper-std/fs`, `@vesper-std/env`, and
//! `@vesper-std/process` (#1408).
//!
//! All three std packages now call the platform (`libc` on Unix, `kernel32` on
//! Windows) directly. This crate is kept only for shared test/oracle helpers
//! (`IoKind`, `error_word`, `os_bytes`, `path_of`, `live_handles`, and the
//! `vstd_proc_id` fixture symbol) until all test callers migrate.

/// Handle kinds, kept for [`live_handles`] in existing leak tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleKind {
    File = 0,
    Dir = 1,
    Command = 2,
    Child = 3,
}

/// Live handles of `kind` (always `0` now that the host shims are retired).
pub fn live(_kind: HandleKind) -> i64 {
    0
}

/// Live handles of every kind: `[file, dir, command, child]`.
pub fn live_handles() -> [i64; 4] {
    [0, 0, 0, 0]
}

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

/// The error word of `e`: kind in the low 8 bits, raw OS error in the high 32.
pub fn error_word(e: &std::io::Error) -> u64 {
    let os = e.raw_os_error().map_or(0, |c| u64::from(c as u32));
    (os << 32) | IoKind::of(e) as u64
}

/// A path from the bytes Vesper passes. Unix paths are arbitrary bytes;
/// Windows paths must be UTF-8.
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

/// Minimal fixture symbol kept for `tests/test_runner_cli.rs`.
#[unsafe(no_mangle)]
pub extern "C" fn vstd_proc_id() -> u64 {
    u64::from(std::process::id())
}

/// Host fixture symbols for `ForeignChecked::jit`.
pub fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![("vstd_proc_id", vstd_proc_id as *const u8)]
}
