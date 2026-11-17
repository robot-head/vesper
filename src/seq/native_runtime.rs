//! JIT runtime for `ChunkSeq[T]` (§13): header allocation and growth.
//!
//! A sequence handle points at a three-word header `{len, cap, data}`
//! ([`crate::seq::HEADER_BYTES`]). Generated code reads and writes elements
//! in place; only allocation and growth call into this runtime. The JIT
//! records every header on this thread and frees it in [`finish`].
//! Executables define the same two symbols in generated code
//! (`src/seq/aot_runtime.rs`).
//!
//! An allocation that cannot be satisfied (including a capacity whose size
//! overflows) ends the process with a message, as an executable exits with
//! status [`crate::seq::FAULT_STATUS`]: generated code has no way to continue
//! without the buffer it asked for.
use std::alloc::{Layout, alloc, dealloc, realloc};
use std::cell::RefCell;

const LEN: usize = 0;
const CAP: usize = 1;
const DATA: usize = 2;

thread_local! {
    static HEADERS: RefCell<Vec<(*mut u64, u64)>> = const { RefCell::new(Vec::new()) };
    static FAULT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Records a recoverable fault (code 1: a slice interval out of bounds).
/// Generated code continues with an empty view; [`finish`] reports it.
pub extern "C" fn vesper_seq_fault(code: u64) -> u64 {
    if code != 1 {
        fault("out of memory");
    }
    FAULT.with(|f| {
        f.borrow_mut()
            .get_or_insert_with(|| "native ChunkSeq fault: slice interval out of bounds".into());
    });
    0
}

fn fault(message: &str) -> ! {
    eprintln!("vesper: ChunkSeq {message}");
    std::process::abort()
}

fn data_layout(cap: u64, stride: u64) -> Option<Layout> {
    let bytes = cap.checked_mul(stride.max(1))?;
    let bytes = usize::try_from(bytes).ok()?;
    Layout::from_size_align(bytes.max(1), 8).ok()
}

/// Allocates a header with room for `cap` elements of `stride` bytes.
pub extern "C" fn vesper_seq_new(cap: u64, stride: u64) -> u64 {
    let header_layout = Layout::new::<[u64; 3]>();
    // SAFETY: the header layout has a non-zero size.
    let header = unsafe { alloc(header_layout) }.cast::<u64>();
    if header.is_null() {
        fault("out of memory");
    }
    let data = if cap == 0 {
        std::ptr::null_mut()
    } else {
        let layout = data_layout(cap, stride)
            .unwrap_or_else(|| fault("capacity overflows the address space"));
        // SAFETY: `layout` has a non-zero size.
        let data = unsafe { alloc(layout) };
        if data.is_null() {
            fault("out of memory");
        }
        data
    };
    // SAFETY: `header` is valid for three words.
    unsafe {
        header.add(LEN).write(0);
        header.add(CAP).write(cap);
        header.add(DATA).write(data as u64);
    }
    HEADERS.with(|h| h.borrow_mut().push((header, stride)));
    header as u64
}

/// Grows the buffer of `handle` to hold at least `needed` elements,
/// doubling its capacity (at least four elements).
pub extern "C" fn vesper_seq_grow(handle: u64, stride: u64, needed: u64) -> u64 {
    let header = handle as *mut u64;
    // SAFETY: generated code passes a live handle from `vesper_seq_new`.
    let (cap, data) = unsafe { (header.add(CAP).read(), header.add(DATA).read() as *mut u8) };
    if needed <= cap {
        return 0;
    }
    let new_cap = cap.saturating_mul(2).max(4).max(needed);
    let layout = data_layout(new_cap, stride)
        .unwrap_or_else(|| fault("capacity overflows the address space"));
    let grown = if data.is_null() {
        // SAFETY: `layout` has a non-zero size.
        unsafe { alloc(layout) }
    } else {
        let old = data_layout(cap, stride).unwrap_or_else(|| fault("has a corrupt capacity"));
        // SAFETY: `data` was allocated with `old`, and the new size is non-zero.
        unsafe { realloc(data, old, layout.size()) }
    };
    if grown.is_null() {
        fault("out of memory");
    }
    // SAFETY: `header` is valid for three words.
    unsafe {
        header.add(CAP).write(new_cap);
        header.add(DATA).write(grown as u64);
    }
    0
}

pub fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("vesper_seq_new", vesper_seq_new as *const u8),
        ("vesper_seq_grow", vesper_seq_grow as *const u8),
        ("vesper_seq_fault", vesper_seq_fault as *const u8),
    ]
}

/// Frees every sequence this thread's JIT allocated and returns the first
/// recorded fault.
pub fn finish() -> Result<(), String> {
    HEADERS.with(|h| {
        for (header, stride) in h.borrow_mut().drain(..) {
            // SAFETY: each header came from `vesper_seq_new` and is freed once.
            unsafe {
                let cap = header.add(CAP).read();
                let data = header.add(DATA).read() as *mut u8;
                if !data.is_null()
                    && let Some(layout) = data_layout(cap, stride)
                {
                    dealloc(data, layout);
                }
                dealloc(header.cast::<u8>(), Layout::new::<[u64; 3]>());
            }
        }
    });
    FAULT.with(|f| f.borrow_mut().take()).map_or(Ok(()), Err)
}
