//! Small platform helpers: no-follow opens, no-replace publication and
//! directory syncing.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Outcome of a no-replace publish.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Publish {
    /// The staged file is now visible at the destination.
    Published,
    /// Something already exists at the destination; the staged file is left
    /// in place for the caller to discard.
    Exists,
}

/// Open an existing file for reading without following a final symlink, and
/// require it to be a regular file.
pub(crate) fn open_regular_nofollow(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            return Err(io::Error::other("refusing to follow a symlink"));
        }
    }
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(file)
}

/// Create a new file exclusively (fails if anything, including a dangling
/// symlink, exists at `path`).
pub(crate) fn create_new(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    opts.open(path)
}

/// A name that is unique within this process and very unlikely to collide
/// with another process. Callers still use exclusive creation.
pub(crate) fn unique_name(prefix: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}{:x}-{seq:x}-{nanos:x}", std::process::id())
}

/// Atomically make the staged file visible at `dest` unless `dest` already
/// exists. This never replaces an existing entry.
///
/// Linux: `renameat2(RENAME_NOREPLACE)`. Elsewhere, or when the file system
/// does not support it: `link(2)` (which also never replaces) followed by
/// unlinking the staged name. If neither primitive is available the call
/// fails with `ErrorKind::Unsupported` instead of falling back to an
/// overwriting rename, because derivation records with different outputs
/// share this path and must never silently replace one another.
pub(crate) fn publish_noreplace(staged: &Path, dest: &Path) -> io::Result<Publish> {
    #[cfg(target_os = "linux")]
    {
        match rename_noreplace_linux(staged, dest) {
            Ok(()) => return Ok(Publish::Published),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(Publish::Exists),
            Err(e) if is_unsupported(&e) => {}
            Err(e) => return Err(e),
        }
    }
    match fs::hard_link(staged, dest) {
        Ok(()) => {
            fs::remove_file(staged)?;
            Ok(Publish::Published)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(Publish::Exists),
        Err(e) if is_unsupported(&e) || e.kind() == io::ErrorKind::PermissionDenied => {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "file system supports neither RENAME_NOREPLACE nor hard links ({e}); \
                     refusing a non-atomic publish"
                ),
            ))
        }
        Err(e) => Err(e),
    }
}

/// Publish a staged workspace directory at `dest` unless `dest` exists.
///
/// Linux uses `renameat2(RENAME_NOREPLACE)`. Directories cannot be hard
/// linked, so other platforms (and Linux file systems without the flag) use
/// an existence check followed by `rename(2)`. POSIX `rename` cannot replace
/// a file or a non-empty directory, so the residual race is limited to a
/// concurrently created *empty* directory at `dest` being replaced.
pub(crate) fn publish_dir_noreplace(staged: &Path, dest: &Path) -> io::Result<Publish> {
    #[cfg(target_os = "linux")]
    {
        match rename_noreplace_linux(staged, dest) {
            Ok(()) => return Ok(Publish::Published),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(Publish::Exists),
            Err(e) if is_unsupported(&e) => {}
            Err(e) => return Err(e),
        }
    }
    match fs::symlink_metadata(dest) {
        Ok(_) => Ok(Publish::Exists),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::rename(staged, dest)?;
            Ok(Publish::Published)
        }
        Err(e) => Err(e),
    }
}

fn is_unsupported(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(
            e.raw_os_error(),
            Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP)
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
fn rename_noreplace_linux(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: both pointers are valid NUL-terminated strings that outlive the
    // call; AT_FDCWD resolves them relative to the working directory exactly
    // like rename(2). The syscall does not retain the pointers.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Flush a directory entry update to stable storage where supported.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Mark a published object read-only.
pub(crate) fn make_readonly(file: &File) -> io::Result<()> {
    let mut perms = file.metadata()?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o444);
    }
    #[cfg(not(unix))]
    perms.set_readonly(true);
    file.set_permissions(perms)
}

/// Permissions for a materialized workspace file.
pub(crate) fn set_workspace_mode(path: &Path, executable: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = executable;
        let mut perms = fs::metadata(path)?.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(path, perms)
    }
}

/// Whether a regular file's metadata marks it executable.
pub(crate) fn is_executable(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}
