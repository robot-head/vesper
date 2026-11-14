//! Structured-exception reporting for faults inside fibers (Windows x64).
//!
//! Windows dispatches a hardware exception (access violation, stack
//! overflow, illegal instruction, integer divide by zero, …) on the stack of
//! the faulting code, which for a fiber is its pinned stack. Because the
//! switch installs each fiber's TEB stack limits, that dispatch is
//! well-formed: the vectored handlers run on the fiber stack (after an
//! overflow, in the stack's committed overflow reserve), and frame-based
//! handlers are searched only up to the fiber's base, where the zero return
//! address of `vesper_fiber_start` ends the walk. The scheduler's frames are
//! never visited, and nothing on the scheduler stack is modified.
//!
//! The policy is *report, then fail*: a fault inside a fiber is never
//! resumed, skipped or converted into a value. The vectored handler
//! installed here recognises a fiber stack from the TEB (the header written
//! by `switch::initial_stack`), marks the fiber's context
//! [`FiberState::Poisoned`], records the fault for [`last_fault`], writes one
//! line naming the fiber, the exception code and the faulting address to
//! standard error, and then continues the search. With no other handler the
//! process terminates with the exception code as its exit status, exactly as
//! an unhandled fault on an ordinary thread stack does. Faults outside a
//! fiber are not touched.
//!
//! Recovering a faulted fiber (unwinding or abandoning its frames and
//! returning a status to the scheduler) is not supported: its Rust frames may
//! hold half-updated state, and the Vesper source rules give no meaning to a
//! hardware fault in a linear context.
use crate::abi::{FiberContext, FiberState};
use crate::switch::current_fiber_context;
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_ARRAY_BOUNDS_EXCEEDED, EXCEPTION_DATATYPE_MISALIGNMENT,
    EXCEPTION_FLT_DIVIDE_BY_ZERO, EXCEPTION_FLT_INVALID_OPERATION, EXCEPTION_ILLEGAL_INSTRUCTION,
    EXCEPTION_IN_PAGE_ERROR, EXCEPTION_INT_DIVIDE_BY_ZERO, EXCEPTION_INT_OVERFLOW,
    EXCEPTION_PRIV_INSTRUCTION, EXCEPTION_STACK_OVERFLOW, STATUS_GUARD_PAGE_VIOLATION,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_POINTERS,
};

/// `EXCEPTION_CONTINUE_SEARCH`.
const CONTINUE_SEARCH: i32 = 0;

/// One fault observed on a fiber stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FiberFault {
    pub fiber_id: u64,
    /// The `NTSTATUS` exception code, for example `0xC0000005`.
    pub code: u32,
    /// Faulting instruction address.
    pub address: u64,
}

static LAST_ID: AtomicU64 = AtomicU64::new(0);
static LAST_CODE: AtomicU64 = AtomicU64::new(0);
static LAST_ADDRESS: AtomicU64 = AtomicU64::new(0);

/// The most recent fault reported on any fiber in this process. A reported
/// fault is fatal unless another handler intervenes, so this is mainly for
/// handlers and diagnostics that run after this module's.
pub fn last_fault() -> Option<FiberFault> {
    match LAST_ID.load(Ordering::Acquire) {
        0 => None,
        fiber_id => Some(FiberFault {
            fiber_id,
            code: LAST_CODE.load(Ordering::Relaxed) as u32,
            address: LAST_ADDRESS.load(Ordering::Relaxed),
        }),
    }
}

/// Whether `code` is a hardware fault this module reports. Software
/// exceptions (C++ exceptions, Rust panics on their way to the abort at the
/// entry boundary, breakpoints and other debugger notifications) pass
/// through untouched.
pub fn is_fault(code: i32) -> bool {
    [
        EXCEPTION_ACCESS_VIOLATION,
        EXCEPTION_ARRAY_BOUNDS_EXCEEDED,
        EXCEPTION_DATATYPE_MISALIGNMENT,
        EXCEPTION_FLT_DIVIDE_BY_ZERO,
        EXCEPTION_FLT_INVALID_OPERATION,
        EXCEPTION_ILLEGAL_INSTRUCTION,
        EXCEPTION_IN_PAGE_ERROR,
        EXCEPTION_INT_DIVIDE_BY_ZERO,
        EXCEPTION_INT_OVERFLOW,
        EXCEPTION_PRIV_INSTRUCTION,
        EXCEPTION_STACK_OVERFLOW,
        STATUS_GUARD_PAGE_VIOLATION,
    ]
    .contains(&code)
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    let Some(ctx) = current_fiber_context() else {
        return CONTINUE_SEARCH;
    };
    // SAFETY: the dispatcher passes valid exception pointers.
    let (code, address) = unsafe {
        let record = &*(*info).ExceptionRecord;
        (record.ExceptionCode, record.ExceptionAddress as u64)
    };
    if !is_fault(code) {
        return CONTINUE_SEARCH;
    }
    report(ctx, code as u32, address);
    CONTINUE_SEARCH
}

fn report(ctx: *mut FiberContext, code: u32, address: u64) {
    // SAFETY: the header names the context of the fiber running on this
    // stack; it is owned by a live `Fiber` for as long as the fiber can run.
    let fiber_id = unsafe {
        (*ctx).state = FiberState::Poisoned as u32;
        (*ctx).fiber_id
    };
    LAST_CODE.store(u64::from(code), Ordering::Relaxed);
    LAST_ADDRESS.store(address, Ordering::Relaxed);
    LAST_ID.store(fiber_id, Ordering::Release);
    // Formatting allocates nothing; standard error is unbuffered. This runs
    // in the fiber's overflow reserve after a stack overflow.
    eprintln!(
        "vesper fiber {fiber_id} faulted: exception {code:#010x} at {address:#x}; \
         the fiber is poisoned and the fault is not recovered"
    );
}

static INSTALL: Once = Once::new();

/// Install the reporting handler (idempotent). Called whenever a fiber stack
/// is created; it is first in the vectored handler list so that it reports
/// before handlers that terminate the process.
pub fn install() {
    INSTALL.call_once(|| {
        // SAFETY: `handler` is a valid vectored handler for the process
        // lifetime; it is never removed.
        let handle = unsafe { AddVectoredExceptionHandler(1, Some(handler)) };
        assert!(!handle.is_null(), "AddVectoredExceptionHandler failed");
    });
}
