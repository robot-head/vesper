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
    /// Bytes at the start of the mapping, below the guard page, that hold the
    /// fiber context of a fixed-address stack (zero otherwise).
    header_len: usize,
}

fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions.
    let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if n <= 0 { 4096 } else { n as usize }
}

fn usable_len(size: usize, page: usize) -> io::Result<usize> {
    let usable = size
        .checked_add(page - 1)
        .ok_or_else(|| io::Error::other("stack size overflow"))?
        & !(page - 1);
    if usable < 4 * page {
        return Err(io::Error::other("fiber stack must be at least four pages"));
    }
    Ok(usable)
}

/// Total reservation length of a stack of `size` usable bytes:
/// `(mapping_len, header_len)`.
fn layout(size: usize, fixed: bool) -> io::Result<(usize, usize)> {
    let page = page_size();
    let usable = usable_len(size, page)?;
    let header = if fixed { page } else { 0 };
    let mapping_len = usable
        .checked_add(page)
        .and_then(|n| n.checked_add(header))
        .ok_or_else(|| io::Error::other("stack mapping size overflow"))?;
    Ok((mapping_len, header))
}

impl FiberStack {
    pub fn new(size: usize) -> io::Result<Self> {
        let (mapping_len, _) = layout(size, false)?;
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
        Self::protect(mapping.cast(), mapping_len, 0)
    }

    /// A stack whose reservation starts exactly at `reservation_base`, laid
    /// out from low to high as one context page (see
    /// [`Self::context_slot`]), the guard page and `size` usable bytes
    /// (rounded up to the page). This is what lets a parked fiber be
    /// restored from a byte image at the same addresses.
    ///
    /// Linux maps with `MAP_FIXED_NOREPLACE`: an address range that is
    /// already mapped fails with [`io::ErrorKind::AlreadyExists`] and nothing
    /// is replaced. macOS has no such flag; the address is passed as a hint
    /// without `MAP_FIXED` and any other placement is unmapped and reported
    /// the same way. Kernels older than Linux 4.17 treat the flag as a hint
    /// too, which the same check catches.
    pub fn with_fixed_base(reservation_base: usize, size: usize) -> io::Result<Self> {
        let page = page_size();
        if reservation_base == 0 || reservation_base % page != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixed stack base must be a non-null multiple of the page size",
            ));
        }
        let (mapping_len, header_len) = layout(size, true)?;
        if reservation_base.checked_add(mapping_len).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixed stack reservation overflows the address space",
            ));
        }
        #[cfg(target_os = "linux")]
        let flags = MAP_FLAGS | libc::MAP_FIXED_NOREPLACE;
        #[cfg(target_os = "macos")]
        let flags = MAP_FLAGS;
        // SAFETY: MAP_FIXED_NOREPLACE (or a plain hint) never replaces an
        // existing mapping, so no memory owned by anyone else is affected.
        let mapping = unsafe {
            libc::mmap(
                reservation_base as *mut libc::c_void,
                mapping_len,
                libc::PROT_NONE,
                flags,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        if mapping as usize != reservation_base {
            // SAFETY: unmapping the mapping just created at another address.
            unsafe { libc::munmap(mapping, mapping_len) };
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "fixed stack address range is in use",
            ));
        }
        Self::protect(mapping.cast(), mapping_len, header_len)
    }

    /// Make the header (if any) and the usable range read/write; the guard
    /// page between them stays `PROT_NONE`.
    fn protect(mapping: *mut u8, mapping_len: usize, header_len: usize) -> io::Result<Self> {
        let page = page_size();
        let usable = mapping_len - header_len - page;
        let mut ranges = vec![(header_len + page, usable)];
        if header_len != 0 {
            ranges.push((0, header_len));
        }
        for (offset, len) in ranges {
            // SAFETY: the range lies inside the mapping created by the caller.
            let rc = unsafe {
                libc::mprotect(
                    mapping.add(offset).cast(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                )
            };
            if rc != 0 {
                let error = io::Error::last_os_error();
                // SAFETY: unmapping the mapping we own.
                unsafe { libc::munmap(mapping.cast(), mapping_len) };
                return Err(error);
            }
        }
        Ok(Self {
            mapping,
            mapping_len,
            guard_len: page,
            header_len,
        })
    }

    /// An address range that a fixed stack of `size` usable bytes could be
    /// reserved at right now (found by mapping and unmapping one). Another
    /// mapping may take it before it is used: then
    /// [`Self::with_fixed_base`] fails rather than replacing anything.
    pub fn free_fixed_base(size: usize) -> io::Result<usize> {
        let (mapping_len, _) = layout(size, true)?;
        // SAFETY: a fresh anonymous mapping, released immediately.
        unsafe {
            let probe = libc::mmap(
                std::ptr::null_mut(),
                mapping_len,
                libc::PROT_NONE,
                MAP_FLAGS,
                -1,
                0,
            );
            if probe == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            libc::munmap(probe, mapping_len);
            Ok(probe as usize)
        }
    }

    /// Lowest usable address (just above the guard page).
    pub fn base(&self) -> usize {
        self.mapping as usize + self.header_len + self.guard_len
    }
    /// One past the highest usable address; 16-byte aligned.
    pub fn top(&self) -> usize {
        self.mapping as usize + self.mapping_len
    }
    pub fn len(&self) -> usize {
        self.mapping_len - self.guard_len - self.header_len
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
    /// Start of the whole reservation (context page, guard and stack).
    pub fn reservation_base(&self) -> usize {
        self.mapping as usize
    }
    /// Length of the whole reservation.
    pub fn reservation_len(&self) -> usize {
        self.mapping_len
    }
    /// For a fixed-address stack, the page-aligned, read/write start of its
    /// context page: the fiber context lives there so that it, too, has the
    /// same address in every image restored onto this reservation.
    pub fn context_slot(&self) -> Option<*mut u8> {
        (self.header_len != 0).then_some(self.mapping)
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
