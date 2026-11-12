//! Pinned fiber stacks: an anonymous private mapping with a `PROT_NONE` guard
//! page below the usable region. Sizes round to the host page (4 KiB on
//! typical Linux, 16 KiB on Apple silicon), so the guard is one host page. A guard page turns most overflows into a
//! fault instead of silent corruption; it is not a substitute for stack-size
//! analysis (a frame larger than one page can skip it, and no stack probes are
//! inserted into generated code by this prototype).
use std::io;

pub const DEFAULT_STACK_SIZE: usize = 256 * 1024;

// Linux: do not reserve swap for the (lazily committed) stack. Darwin has no
// overcommit accounting to opt out of; its `MAP_NORESERVE` is a documented
// no-op, so it is not passed.
#[cfg(target_os = "linux")]
const MAP_FLAGS: libc::c_int = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
#[cfg(target_os = "macos")]
const MAP_FLAGS: libc::c_int = libc::MAP_PRIVATE | libc::MAP_ANON;

pub struct FiberStack {
    mapping: *mut u8,
    mapping_len: usize,
    guard_len: usize,
}

fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions.
    let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if n <= 0 { 4096 } else { n as usize }
}

impl FiberStack {
    pub fn new(size: usize) -> io::Result<Self> {
        let page = page_size();
        let usable = size
            .checked_add(page - 1)
            .ok_or_else(|| io::Error::other("stack size overflow"))?
            & !(page - 1);
        if usable < 4 * page {
            return Err(io::Error::other("fiber stack must be at least four pages"));
        }
        let mapping_len = usable
            .checked_add(page)
            .ok_or_else(|| io::Error::other("stack mapping size overflow"))?;
        // SAFETY: a fresh anonymous mapping with no address hint.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapping_len,
                libc::PROT_NONE,
                MAP_FLAGS,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let mapping = mapping.cast::<u8>();
        // SAFETY: the range lies inside the mapping created above.
        let rc = unsafe {
            libc::mprotect(
                mapping.add(page).cast(),
                usable,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        };
        if rc != 0 {
            let error = io::Error::last_os_error();
            // SAFETY: unmapping the mapping we own.
            unsafe { libc::munmap(mapping.cast(), mapping_len) };
            return Err(error);
        }
        Ok(Self {
            mapping,
            mapping_len,
            guard_len: page,
        })
    }
    /// Lowest usable address (just above the guard page).
    pub fn base(&self) -> usize {
        self.mapping as usize + self.guard_len
    }
    /// One past the highest usable address; 16-byte aligned.
    pub fn top(&self) -> usize {
        self.mapping as usize + self.mapping_len
    }
    pub fn len(&self) -> usize {
        self.mapping_len - self.guard_len
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn guard_len(&self) -> usize {
        self.guard_len
    }
    /// Host page size used for rounding and for the guard.
    pub fn page_size() -> usize {
        page_size()
    }
    pub fn contains(&self, address: u64) -> bool {
        (self.base() as u64..self.top() as u64).contains(&address)
    }
}

impl Drop for FiberStack {
    fn drop(&mut self) {
        // SAFETY: the mapping is owned and no frame on it is executing: a
        // fiber is only dropped while created, parked or exited.
        unsafe { libc::munmap(self.mapping.cast(), self.mapping_len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stack_is_aligned_and_guarded() {
        let s = FiberStack::new(64 * 1024).unwrap();
        assert_eq!(s.top() % 16, 0);
        assert_eq!(s.len(), 64 * 1024);
        assert!(s.contains(s.base() as u64));
        assert!(!s.contains(s.top() as u64));
        assert!(!s.contains((s.base() - 1) as u64));
        assert!(FiberStack::new(16).is_err());
        assert!(FiberStack::new(usize::MAX).is_err());
        // Rounding fits, but adding the guard page must also be checked.
        assert!(FiberStack::new(usize::MAX - page_size() + 1).is_err());
    }
}
