//! JIT runtime for `ChunkSeq[T]` (§13): header allocation and growth.
//!
//! A sequence handle points at a three-word header `{len, cap, data}`
//! ([`crate::seq::HEADER_BYTES`]). Generated code reads and writes elements
//! in place; only allocation, growth and drops call into this runtime.
//! Executables define the same symbols in generated code, with the same
//! memory layout (`src/seq/aot_runtime.rs`, which explains it):
//! [`vesper_seq_make`] puts the header and up to [`INLINE_BYTES`] of
//! elements in one `malloc`
//! block, growth moves the elements to a separate buffer that starts
//! [`BUFFER_PREFIX`] bytes into its own block, and `data == header + 24`
//! tells the two apart. Nothing is looked up per sequence.
//!
//! Generated code frees the sequences and strings it drops
//! ([`vesper_seq_free`], [`vesper_str_release`]). [`live_allocations`]
//! counts the live headers and separate buffers of this thread, as the
//! executable's `vesper_seq_live` counter does.
//!
//! The JIT also records each live header and string buffer, so that
//! [`finish`] frees what generated code left (a sequence or string a JIT
//! entry returns is copied out and then freed there) and [`stray_frees`]
//! counts drops of something that is not live, which would be a double free
//! in an executable. Executables and libraries, which the ports measure,
//! keep only their counter.
//!
//! An allocation that cannot be satisfied (including a capacity whose size
//! overflows) ends the process with a message, as an executable exits with
//! status [`crate::seq::FAULT_STATUS`]: generated code has no way to continue
//! without the buffer it asked for.
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::c_void;

/// The most element bytes a sequence keeps inline after its header.
pub const INLINE_BYTES: i64 = 65536;

/// Element bytes the first growth of an empty sequence reserves inline at
/// least (Rust's `Vec` starts at four 8-byte elements on its first push).
pub const NEW_INLINE_BYTES: i64 = 32;

/// Bytes between the start of a separate buffer's `malloc` block and its
/// elements.
pub const BUFFER_PREFIX: i64 = 8;

/// The shared empty header `{0, 0, null}` of every JIT sequence that has
/// never grown (#1021). Generated code only reads it: a length store is
/// skipped when the length does not change, and growth replaces it. It is
/// immutable, so a stray store would fault at once.
static EMPTY: [u64; 3] = [0; 3];

/// The address of the shared empty header, which `ChunkSeq::new` returns.
pub fn empty_header() -> u64 {
    EMPTY.as_ptr() as u64
}

const LEN: usize = 0;
const CAP: usize = 1;
const DATA: usize = 2;
const HEADER: usize = crate::seq::HEADER_BYTES as usize;
const PREFIX: usize = BUFFER_PREFIX as usize;

// The C library allocator, which executables use too.
unsafe extern "C" {
    #[link_name = "malloc"]
    fn c_malloc(size: usize) -> *mut c_void;
    #[link_name = "realloc"]
    fn c_realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
    #[link_name = "free"]
    fn c_free(ptr: *mut c_void);
}

unsafe fn malloc(size: usize) -> *mut u8 {
    // SAFETY: forwarded to the C allocator.
    unsafe { c_malloc(size).cast() }
}

unsafe fn realloc(ptr: *mut u8, size: usize) -> *mut u8 {
    // SAFETY: forwarded to the C allocator.
    unsafe { c_realloc(ptr.cast(), size).cast() }
}

unsafe fn free(ptr: *mut u8) {
    // SAFETY: forwarded to the C allocator.
    unsafe { c_free(ptr.cast()) }
}

thread_local! {
    /// Live headers and separate buffers of this thread.
    static LIVE: Cell<usize> = const { Cell::new(0) };
    /// Live headers.
    static HEADERS: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
    /// Separate buffers grown for values this runtime did
    /// not allocate (native strings, §13), by data address.
    static BUFFERS: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
    static FAULT: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Allocations the last [`finish`] found live because generated code
    /// had not freed them.
    static TEARDOWN: Cell<usize> = const { Cell::new(0) };
    /// Drops of a sequence or string buffer this runtime does not hold: a
    /// double free in an executable.
    static STRAY: Cell<usize> = const { Cell::new(0) };
}

fn live(delta: isize) {
    LIVE.with(|l| l.set(l.get().wrapping_add_signed(delta)));
}

/// Records a recoverable fault (code 1: a slice interval out of bounds;
/// code 2: a string interval off a char boundary). Generated code continues
/// with an empty view; [`finish`] reports it.
pub extern "C" fn vesper_seq_fault(code: u64) -> u64 {
    let message = match code {
        1 => "native ChunkSeq fault: slice interval out of bounds",
        2 => "native String fault: interval is not on char boundaries",
        _ => fault("out of memory"),
    };
    FAULT.with(|f| {
        f.borrow_mut().get_or_insert_with(|| message.into());
    });
    0
}

/// Takes this thread's recorded fault: a scoped worker
/// ([`crate::atomics::workers`]) hands it to the thread that runs the JIT
/// entry, whose [`finish`] reports it.
pub(crate) fn take_fault() -> Option<String> {
    FAULT.with(|f| f.borrow_mut().take())
}

/// Records `message` as this thread's fault unless one is already recorded.
pub(crate) fn record_fault(message: String) {
    FAULT.with(|f| {
        f.borrow_mut().get_or_insert(message);
    });
}

fn fault(message: &str) -> ! {
    eprintln!("vesper: ChunkSeq {message}");
    std::process::abort()
}

/// `cap * max(stride, 1)` bytes, faulting past `isize::MAX`.
fn bytes(cap: u64, stride: u64) -> usize {
    cap.checked_mul(stride.max(1))
        .filter(|&b| b <= i64::MAX as u64)
        .and_then(|b| usize::try_from(b).ok())
        .unwrap_or_else(|| fault("capacity overflows the address space"))
}

fn allocate(size: usize) -> *mut u8 {
    // SAFETY: `malloc` accepts any size; null is handled.
    let block = unsafe { malloc(size.max(1)) };
    if block.is_null() {
        fault("out of memory");
    }
    block
}

/// A separate buffer of `n` bytes (its data address).
fn buffer(n: usize) -> *mut u8 {
    let total = n
        .checked_add(PREFIX)
        .unwrap_or_else(|| fault("capacity overflows the address space"));
    live(1);
    // SAFETY: the block holds `PREFIX + n` bytes.
    unsafe { allocate(total).add(PREFIX) }
}

/// Whether `{cap, data}` of the header at `header` is a separate buffer:
/// owned (capacity not 0), present, and not the header's inline elements.
fn separate(header: *const u64, cap: u64, data: u64) -> bool {
    cap != 0 && data != 0 && data != header as u64 + HEADER as u64
}

fn record(header: *mut u64) {
    HEADERS.with(|h| h.borrow_mut().insert(header as usize));
}

/// Allocates a bare header, and for `cap > 0` a separate buffer for `cap`
/// elements of `stride` bytes (the export ABI's constructor).
pub extern "C" fn vesper_seq_new(cap: u64, stride: u64) -> u64 {
    let header = allocate(HEADER).cast::<u64>();
    live(1);
    let data = if cap == 0 {
        std::ptr::null_mut()
    } else {
        buffer(bytes(cap, stride))
    };
    // SAFETY: `header` is valid for three words.
    unsafe {
        header.add(LEN).write(0);
        header.add(CAP).write(cap);
        header.add(DATA).write(data as u64);
    }
    record(header);
    header as u64
}

/// Allocates a sequence with room for `cap` elements of `stride` bytes,
/// inline after its header when they fit in [`INLINE_BYTES`].
pub extern "C" fn vesper_seq_make(cap: u64, stride: u64) -> u64 {
    if cap == 0 {
        return empty_header();
    }
    let n = bytes(cap, stride);
    if n > INLINE_BYTES as usize {
        return vesper_seq_new(cap, stride);
    }
    let header = allocate(HEADER + n).cast::<u64>();
    live(1);
    let data = header as u64 + HEADER as u64;
    // SAFETY: `header` is valid for three words.
    unsafe {
        header.add(LEN).write(0);
        header.add(CAP).write(cap);
        header.add(DATA).write(data);
    }
    record(header);
    header as u64
}

/// Grows the buffer of `handle` to hold at least `needed` elements,
/// doubling its capacity (at least four elements), and returns the header
/// to use afterwards: `handle` itself, or for the shared empty header a
/// fresh sequence that generated code writes back to its place (#1021).
pub extern "C" fn vesper_seq_grow(handle: u64, stride: u64, needed: u64) -> u64 {
    if handle == empty_header() {
        let room = (NEW_INLINE_BYTES as u64) / stride.max(1);
        return vesper_seq_make(needed.max(room.max(4)), stride);
    }
    let header = handle as *mut u64;
    // SAFETY: generated code passes a live header or `String` value.
    let (len, cap, data) = unsafe {
        (
            header.add(LEN).read(),
            header.add(CAP).read(),
            header.add(DATA).read(),
        )
    };
    if needed <= cap {
        return handle;
    }
    let new_cap = cap.saturating_mul(2).max(4).max(needed);
    let n = bytes(new_cap, stride);
    let grown = if separate(header, cap, data) {
        // SAFETY: `data - PREFIX` is a live block from `malloc`; the size
        // is non-zero.
        let block = unsafe { realloc((data as *mut u8).sub(PREFIX), n + PREFIX) };
        if block.is_null() {
            fault("out of memory");
        }
        // SAFETY: the block holds `PREFIX + n` bytes.
        unsafe { block.add(PREFIX) }
    } else {
        let fresh = buffer(n);
        if data != 0 {
            // Inline elements or borrowed bytes (capacity 0: a string
            // literal or view): copy them before the first write.
            // SAFETY: `len * stride` bytes are readable at `data` and fit in
            // the new buffer (`needed > len`).
            unsafe { std::ptr::copy_nonoverlapping(data as *const u8, fresh, bytes(len, stride)) };
        }
        fresh
    };
    if !HEADERS.with(|h| h.borrow().contains(&(header as usize))) {
        BUFFERS.with(|b| {
            let mut b = b.borrow_mut();
            b.remove(&(data as usize));
            b.insert(grown as usize);
        });
    }
    // SAFETY: `header` is valid for three words.
    unsafe {
        header.add(CAP).write(new_cap);
        header.add(DATA).write(grown as u64);
    }
    handle
}

/// Frees a header and the separate buffer it owns.
///
/// # Safety
/// `header` is live and is freed once.
unsafe fn free_header(header: *mut u64) {
    // SAFETY: the caller passes a live header.
    unsafe {
        let cap = header.add(CAP).read();
        let data = header.add(DATA).read();
        if separate(header, cap, data) {
            free((data as *mut u8).sub(PREFIX));
            live(-1);
        }
        free(header.cast::<u8>());
        live(-1);
    }
}

/// Frees a sequence that generated code drops: its header and buffer. A
/// handle that is not live is ignored (and counted in [`stray_frees`]), so
/// a drop never frees twice.
pub extern "C" fn vesper_seq_free(handle: u64) -> u64 {
    if handle == 0 || handle == empty_header() {
        return 0;
    }
    if !HEADERS.with(|h| h.borrow_mut().remove(&(handle as usize))) {
        STRAY.with(|s| s.set(s.get() + 1));
        return 0;
    }
    // SAFETY: the header was live and is now unrecorded, so it is freed once.
    unsafe { free_header(handle as *mut u64) };
    0
}

/// Frees the buffer an inline `String` value at `value` owns (borrowed
/// literal bytes, capacity 0, are not owned). A buffer this runtime did not
/// grow is ignored.
pub extern "C" fn vesper_str_release(value: u64) -> u64 {
    if value == 0 {
        return 0;
    }
    let header = value as *mut u64;
    // SAFETY: generated code passes the address of a live `String` value.
    let (cap, data) = unsafe { (header.add(CAP).read(), header.add(DATA).read()) };
    if cap == 0 || data == 0 {
        return 0;
    }
    if !BUFFERS.with(|b| b.borrow_mut().remove(&(data as usize))) {
        STRAY.with(|s| s.set(s.get() + 1));
        return 0;
    }
    // SAFETY: the buffer came from `vesper_seq_grow` and is freed once.
    unsafe { free((data as *mut u8).sub(PREFIX)) };
    live(-1);
    0
}

pub fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("vesper_seq_new", vesper_seq_new as *const u8),
        ("vesper_seq_make", vesper_seq_make as *const u8),
        ("vesper_seq_grow", vesper_seq_grow as *const u8),
        ("vesper_seq_fault", vesper_seq_fault as *const u8),
        ("vesper_seq_free", vesper_seq_free as *const u8),
        ("vesper_str_release", vesper_str_release as *const u8),
    ]
}

/// The live sequence headers and separate buffers of this thread's JIT (a
/// header with inline elements counts once, as in executables).
pub fn live_allocations() -> usize {
    LIVE.with(Cell::get)
}

/// How many sequence allocations (as [`live_allocations`] counts them) the
/// last [`finish`] on this thread found live because generated code had not
/// freed them: 0 when every sequence and string was dropped by the program
/// itself.
pub fn teardown_frees() -> usize {
    TEARDOWN.with(Cell::get)
}

/// Drops on this thread of a sequence or string buffer that was not live,
/// which would be a double free in an executable. The count only grows:
/// compare it before and after a run.
pub fn stray_frees() -> usize {
    STRAY.with(Cell::get)
}

/// Ends a JIT run on this thread: frees every sequence and string buffer
/// generated code did not drop, and returns the first recorded fault.
pub fn finish() -> Result<(), String> {
    TEARDOWN.with(|t| t.set(live_allocations()));
    let headers: Vec<usize> = HEADERS.with(|h| h.borrow_mut().drain().collect());
    for header in headers {
        // SAFETY: each recorded header is live and is freed once.
        unsafe { free_header(header as *mut u64) };
    }
    let buffers: Vec<usize> = BUFFERS.with(|b| b.borrow_mut().drain().collect());
    for data in buffers {
        // SAFETY: each recorded buffer came from `vesper_seq_grow` and
        // is freed once.
        unsafe { free((data as *mut u8).sub(PREFIX)) };
    }
    LIVE.with(|l| l.set(0));
    FAULT.with(|f| f.borrow_mut().take()).map_or(Ok(()), Err)
}
