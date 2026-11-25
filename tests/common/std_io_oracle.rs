//! The Rust side of the `@vesper-std/fs`, `process` and `env` tests: the
//! `IoError.kind` codes of a `std::io::Error` and the byte forms of paths
//! and `OsStr`s, as `@vesper-std` reports them. These helpers were the
//! retired host shim crates/vesper-std-host (SH6 lane 13, #1408); the std
//! packages now call libc or kernel32 themselves, and only the test oracles
//! (which stay in Rust) use them.
#![allow(dead_code)]

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
