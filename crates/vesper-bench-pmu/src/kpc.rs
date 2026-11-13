//! macOS AArch64 fixed thread counters through the private kperf framework.
//!
//! The framework is loaded with `dlopen` and its `kpc_*` symbols resolved
//! before anything else, so a missing library is reported as
//! [`UnavailableReason::LibraryMissing`]. Counting requires root: a non-root
//! process gets [`UnavailableReason::PermissionDenied`] before any kpc state
//! is changed. With root, the fixed counter class is enabled for counting and
//! thread counting; Apple Silicon fixed counter 0 is core cycles and 1 is
//! retired instructions. Drop turns thread counting back off.
use crate::{CounterDelta, Provider, Unavailable, UnavailableReason, checked_delta};
use std::{
    ffi::{CStr, c_int, c_void},
    marker::PhantomData,
    rc::Rc,
    time::Instant,
};
use vesper_bench::{Invocation, WindowCompletion, WindowObserver};

const KPERF: &CStr = c"/System/Library/PrivateFrameworks/kperf.framework/kperf";
const KPC_CLASS_FIXED_MASK: u32 = 1;
const MAX_COUNTERS: usize = 32;

type GetCount = unsafe extern "C" fn(u32) -> u32;
type SetClasses = unsafe extern "C" fn(u32) -> c_int;
type GetThreadCounters = unsafe extern "C" fn(u32, u32, *mut u64) -> c_int;

struct Symbols {
    handle: *mut c_void,
    get_counter_count: GetCount,
    set_counting: SetClasses,
    set_thread_counting: SetClasses,
    get_thread_counters: GetThreadCounters,
}
impl Drop for Symbols {
    fn drop(&mut self) {
        // SAFETY: handle came from a successful dlopen and is closed once.
        unsafe {
            libc::dlclose(self.handle);
        }
    }
}
fn load() -> Result<Symbols, Unavailable> {
    // SAFETY: KPERF is a NUL-terminated path; dlopen has no other inputs.
    let handle = unsafe { libc::dlopen(KPERF.as_ptr(), libc::RTLD_LAZY) };
    if handle.is_null() {
        return Err(Unavailable::new(
            UnavailableReason::LibraryMissing,
            "dlopen kperf",
        ));
    }
    let symbol = |name: &CStr| {
        // SAFETY: live handle and NUL-terminated symbol name.
        let p = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if p.is_null() {
            Err(Unavailable::new(
                UnavailableReason::LibraryMissing,
                "dlsym kpc",
            ))
        } else {
            Ok(p)
        }
    };
    let resolved = (|| {
        Ok::<_, Unavailable>((
            symbol(c"kpc_get_counter_count")?,
            symbol(c"kpc_set_counting")?,
            symbol(c"kpc_set_thread_counting")?,
            symbol(c"kpc_get_thread_counters")?,
        ))
    })();
    let (count, counting, thread_counting, thread_counters) = match resolved {
        Ok(s) => s,
        Err(e) => {
            // SAFETY: close the handle opened above on the failure path.
            unsafe {
                libc::dlclose(handle);
            }
            return Err(e);
        }
    };
    // SAFETY: these are the documented C signatures of the kperf exports
    // (used by Apple's own tools and xnu tests); pointers are nonnull.
    unsafe {
        Ok(Symbols {
            handle,
            get_counter_count: std::mem::transmute::<*mut c_void, GetCount>(count),
            set_counting: std::mem::transmute::<*mut c_void, SetClasses>(counting),
            set_thread_counting: std::mem::transmute::<*mut c_void, SetClasses>(thread_counting),
            get_thread_counters: std::mem::transmute::<*mut c_void, GetThreadCounters>(
                thread_counters,
            ),
        })
    }
}

/// Current-thread fixed counters. Not Send/Sync.
pub struct KpcCounters {
    symbols: Symbols,
    fixed: u32,
    origin: u64,
    generation: u64,
    active: Option<u64>,
    thread: u64,
    _thread_bound: PhantomData<Rc<()>>,
}
pub struct KpcSnapshot {
    origin: u64,
    generation: u64,
    counts: [u64; 2],
    start: Instant,
    _thread_bound: PhantomData<Rc<()>>,
}

fn thread_id() -> u64 {
    let mut id = 0u64;
    // SAFETY: null thread means the calling thread; id is a valid output.
    unsafe {
        libc::pthread_threadid_np(0 as libc::pthread_t, &mut id);
    }
    id
}

static NEXT_ORIGIN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl KpcCounters {
    pub fn open() -> Result<Self, Unavailable> {
        let symbols = load()?;
        // SAFETY: geteuid has no arguments.
        if unsafe { libc::geteuid() } != 0 {
            return Err(Unavailable::new(
                UnavailableReason::PermissionDenied,
                "kpc requires root",
            ));
        }
        // SAFETY: resolved kperf functions with plain integer arguments.
        let fixed = unsafe { (symbols.get_counter_count)(KPC_CLASS_FIXED_MASK) };
        if !(2..=MAX_COUNTERS as u32).contains(&fixed) {
            return Err(Unavailable::new(
                UnavailableReason::InvalidMetadata,
                "kpc fixed counter count",
            ));
        }
        // SAFETY: as above.
        if unsafe { (symbols.set_counting)(KPC_CLASS_FIXED_MASK) } != 0 {
            return Err(Unavailable::new(
                UnavailableReason::PermissionDenied,
                "kpc_set_counting",
            ));
        }
        // SAFETY: as above.
        if unsafe { (symbols.set_thread_counting)(KPC_CLASS_FIXED_MASK) } != 0 {
            return Err(Unavailable::new(
                UnavailableReason::PermissionDenied,
                "kpc_set_thread_counting",
            ));
        }
        let origin = NEXT_ORIGIN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let counters = Self {
            symbols,
            fixed,
            origin,
            generation: 0,
            active: None,
            thread: thread_id(),
            _thread_bound: PhantomData,
        };
        counters.read()?;
        Ok(counters)
    }
    fn read(&self) -> Result<[u64; 2], Unavailable> {
        let mut buffer = [0u64; MAX_COUNTERS];
        // SAFETY: buffer holds MAX_COUNTERS >= fixed entries; tid 0 is the
        // calling thread.
        let status =
            unsafe { (self.symbols.get_thread_counters)(0, self.fixed, buffer.as_mut_ptr()) };
        if status != 0 {
            return Err(Unavailable::new(
                UnavailableReason::OsError,
                "kpc_get_thread_counters",
            ));
        }
        Ok([buffer[0], buffer[1]])
    }
}
impl Drop for KpcCounters {
    fn drop(&mut self) {
        // SAFETY: resolved function; turning thread counting off is idempotent.
        unsafe {
            (self.symbols.set_thread_counting)(0);
        }
    }
}
impl WindowObserver for KpcCounters {
    type Token = KpcSnapshot;
    type Observation = CounterDelta;
    type Error = Unavailable;
    fn before_window(&mut self, _: Invocation, _: usize) -> Result<KpcSnapshot, Unavailable> {
        if thread_id() != self.thread {
            return Err(Unavailable::new(
                UnavailableReason::WrongProcessOrThread,
                "owning thread",
            ));
        }
        if self.active.is_some() {
            return Err(Unavailable::new(
                UnavailableReason::InvalidState,
                "before_window",
            ));
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| Unavailable::new(UnavailableReason::Exhausted, "window generation"))?;
        let start = Instant::now();
        let counts = self.read()?;
        self.generation = generation;
        self.active = Some(generation);
        Ok(KpcSnapshot {
            origin: self.origin,
            generation,
            counts,
            start,
            _thread_bound: PhantomData,
        })
    }
    fn after_window(
        &mut self,
        token: KpcSnapshot,
        _: WindowCompletion,
    ) -> Result<CounterDelta, Unavailable> {
        if token.origin != self.origin || self.active != Some(token.generation) {
            return Err(Unavailable::new(
                UnavailableReason::ForeignOrStaleToken,
                "after_window",
            ));
        }
        self.active = None;
        let after = self.read()?;
        let nanos = u64::try_from(token.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        checked_delta(Provider::MacosKpc, token.counts, after, nanos)
    }
}
