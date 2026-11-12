//! Scratch directories and destination publication.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A name unique across threads and processes on this host.
pub(crate) fn unique_name(prefix: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}{}-{t:x}-{n}", std::process::id())
}

/// A private directory removed on drop (success, error or panic).
#[derive(Debug)]
pub(crate) struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    pub(crate) fn create(parent: &Path, prefix: &str) -> io::Result<Self> {
        loop {
            let path = parent.join(unique_name(prefix));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(ScratchDir { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = remove_tree(&self.path);
    }
}

/// Removes a directory tree, first restoring write permission on
/// directories that a step or a read-only policy may have cleared.
pub(crate) fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(m) if !m.is_dir() => return fs::remove_file(path),
        Ok(_) => {}
    }
    make_writable(path);
    fs::remove_dir_all(path)
}

fn make_writable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = fs::symlink_metadata(path)
            && m.is_dir()
        {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
            if let Ok(entries) = fs::read_dir(path) {
                for e in entries.flatten() {
                    make_writable(&e.path());
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Puts the fully built directory `staged` at `dest`, replacing an existing
/// directory there. On Linux the swap is one `renameat2(RENAME_EXCHANGE)`,
/// so `dest` is always either the old or the new tree. Elsewhere the old
/// tree is renamed aside first and restored if the second rename fails.
/// The old tree is deleted afterwards.
pub(crate) fn replace_dir(staged: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        match exchange(staged, dest) {
            Ok(()) => return remove_tree(staged),
            Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOSYS)) => {}
            Err(e) => return Err(e),
        }
    }
    let parent = dest.parent().unwrap_or(Path::new("."));
    let aside = parent.join(unique_name(".vlb-old-"));
    fs::rename(dest, &aside)?;
    if let Err(e) = fs::rename(staged, dest) {
        let _ = fs::rename(&aside, dest);
        return Err(e);
    }
    remove_tree(&aside)
}

#[cfg(target_os = "linux")]
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let ca = CString::new(a.as_os_str().as_bytes())?;
    let cb = CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: both pointers are valid NUL-terminated strings for the call.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            ca.as_ptr(),
            libc::AT_FDCWD,
            cb.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
