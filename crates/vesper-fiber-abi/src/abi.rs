//! Versioned `repr(C)` layouts shared by generated code and the runtime.
//!
//! Every field offset that generated code depends on is exported from
//! [`offsets`] and checked at compile time against `offset_of!`. Changing any
//! layout below requires bumping [`ABI_VERSION`].
use std::mem::{align_of, offset_of, size_of};

/// Version 2 of the native Pulse continuation ABI.
///
/// * v1: initial context/evidence layouts; switch frame saved only the
///   general-purpose and vector callee-saved registers.
/// * v2: the switch frame also saves the floating-point control state
///   ([`crate::fpenv`], Linux hosts) and new contexts carry
///   [`FLAG_FP_ENV_SAVED`]. The `repr(C)` layouts and offsets below are
///   unchanged from v1, so generated code and objects are unchanged; the
///   version still changes because the runtime-side switch frame and the
///   lifecycle contract changed.
pub const ABI_VERSION: u32 = 2;
/// `b"VFIB"` little endian. A context with another magic is not a fiber.
pub const CONTEXT_MAGIC: u32 = u32::from_le_bytes(*b"VFIB");

/// Nonzero, never reused handler-installation identity (`EvidenceId`).
/// Zero is reserved for "no evidence".
pub type EvidenceId = u64;

/// Direct (tail-resumptive) handler entry: `(state_ptr, op, arg) -> result`.
/// Runs on the performing fiber's stack without suspension. It must not
/// unwind; Rust `extern "C"` aborts on panic.
pub type HandlerFn = unsafe extern "C" fn(*mut u8, u32, u64) -> u64;

/// One lexical capability slot. Generated code finds a slot by its static
/// lexical depth: `vector.slots + depth * size_of::<EvidenceSlot>()`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EvidenceSlot {
    /// Identity of this handler installation. Two nested installations of the
    /// same effect always have distinct identities.
    pub identity: EvidenceId,
    /// Static effect tag (assigned by the compiler/adapter, not interpreted
    /// by the runtime).
    pub effect: u32,
    /// Index of this slot in its vector; redundant, checked by validators.
    pub depth: u32,
    /// `Some` for a handler that answers inline without suspension. `None`
    /// means operations on this slot park the fiber with [`ParkRequest`].
    pub handler_fn: Option<HandlerFn>,
    /// Handler state passed to `handler_fn`. Owned by the installer and valid
    /// for the lifetime of the installation.
    pub state_ptr: *mut u8,
}

/// Header of an evidence vector. Slots are owned by the installer (see
/// [`crate::evidence::EvidenceVector`]) and outlive every fiber that uses it.
#[repr(C)]
#[derive(Debug)]
pub struct EvidenceVectorHeader {
    pub abi_version: u32,
    pub len: u32,
    pub slots: *const EvidenceSlot,
}

/// Lifecycle of a fiber context. Stored as `u32` in [`FiberContext::state`].
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FiberState {
    /// Stack prepared; entry not yet started.
    Created = 0,
    /// Executing on its own stack. Exactly one owner (the resumer) is waiting.
    Running = 1,
    /// Suspended in `vesper_fiber_park`; the park request is valid.
    Parked = 2,
    /// The entry function returned; `exit_status`/`result` are valid.
    Exited = 3,
    /// A previous ABI violation poisoned the context; it can never run again.
    Poisoned = 4,
}

impl FiberState {
    pub fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            0 => Self::Created,
            1 => Self::Running,
            2 => Self::Parked,
            3 => Self::Exited,
            4 => Self::Poisoned,
            _ => return None,
        })
    }
}

/// How a parked operation is resumed. Stored as `u32` in [`ResumeSignal::kind`].
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeKind {
    /// The operation produced `value`.
    Value = 0,
    /// The operation failed; `value` carries an error code (errno for I/O).
    Error = 1,
    /// The scheduler is cancelling the fiber. The fiber still resumes exactly
    /// once and must take its cancellation edge (discharging obligations).
    Cancelled = 2,
}

/// Exit status returned by a generated fiber entry in the return register.
pub mod exit {
    pub const COMPLETE: u64 = 0;
    pub const FAILED: u64 = 1;
    pub const CANCELLED: u64 = 2;
    /// Generated code detected a violated ABI invariant (for example a resume
    /// signal for a different evidence identity) and stopped.
    pub const ABI_VIOLATION: u64 = 3;
}

/// Written by generated code into [`FiberContext::park`] before calling
/// `vesper_fiber_park`. Read by the scheduler while the fiber is parked.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParkRequest {
    /// Identity of the evidence slot the operation was performed on.
    pub evidence: EvidenceId,
    /// Effect operation tag, interpreted by the handler/adapter.
    pub op: u32,
    pub flags: u32,
    pub arg0: u64,
    pub arg1: u64,
}

/// Written by the resumer into [`FiberContext::resume`] before switching back.
/// `vesper_fiber_park` returns a pointer to it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResumeSignal {
    pub kind: u32,
    pub _reserved: u32,
    pub value: u64,
    /// Current base of the relocatable data frame. The previous base is invalid
    /// after any park; generated code must reload this field.
    pub data_base: u64,
    /// Must equal the [`ParkRequest::evidence`] of the matching park.
    pub evidence: EvidenceId,
}

/// Intrusive one-shot continuation header (`FiberHeader` in design §12.7).
///
/// Ownership: the context is heap-allocated and owned by the runtime's
/// `Fiber`/`Parked` value, never by generated code. Generated code receives a
/// borrowed `*mut FiberContext` valid until its entry returns. While `Parked`,
/// only the holder of the one-shot continuation may mutate it.
///
/// There is no `saved_ip` field: the resume address is the trampoline return
/// address stored on the pinned fiber stack at `saved_sp`. `saved_sp` points
/// into that stack, so moving the stack bytes elsewhere would leave this and
/// every saved frame pointer dangling (see the relocation invariant in the
/// RFC). The physical stack is therefore pinned for the context's lifetime.
#[repr(C, align(64))]
#[derive(Debug)]
pub struct FiberContext {
    pub magic: u32,
    pub abi_version: u32,
    pub state: u32,
    pub flags: u32,
    /// Incremented on every park and every resume. A raw continuation token is
    /// `(fiber_id, generation)` and is valid only for one parked generation.
    pub generation: u64,
    pub fiber_id: u64,
    /// Fiber stack pointer while parked or created (pinned, physical).
    pub saved_sp: u64,
    /// Resumer stack pointer while the fiber runs.
    pub scheduler_sp: u64,
    pub evidence: *const EvidenceVectorHeader,
    /// Base of the relocatable data frame as of the latest (re)entry.
    pub data_base: u64,
    pub data_len: u64,
    pub park: ParkRequest,
    pub resume: ResumeSignal,
    /// Generated entry: `extern "C" fn(*mut FiberContext, u64) -> u64`.
    pub entry: u64,
    pub entry_arg: u64,
    /// Exit status (see [`exit`]) once `Exited`.
    pub exit_status: u64,
    /// Result word written by generated code before returning.
    pub result: u64,
    pub stack_base: u64,
    pub stack_len: u64,
}

/// `flags` bit: the physical stack must not move while this context lives.
/// Always set since ABI v1.
pub const FLAG_STACK_PINNED: u32 = 1;
/// `flags` bit: the switch frame saves and restores the target ABI's
/// floating-point control state, so each context owns its environment.
/// Always set since ABI v2.
pub const FLAG_FP_ENV_SAVED: u32 = 2;

/// Byte offsets used by generated code. Checked against the Rust layout below.
pub mod offsets {
    pub const CTX_MAGIC: i32 = 0;
    pub const CTX_ABI_VERSION: i32 = 4;
    pub const CTX_STATE: i32 = 8;
    pub const CTX_FLAGS: i32 = 12;
    pub const CTX_GENERATION: i32 = 16;
    pub const CTX_FIBER_ID: i32 = 24;
    pub const CTX_SAVED_SP: i32 = 32;
    pub const CTX_SCHEDULER_SP: i32 = 40;
    pub const CTX_EVIDENCE: i32 = 48;
    pub const CTX_DATA_BASE: i32 = 56;
    pub const CTX_DATA_LEN: i32 = 64;
    pub const CTX_PARK: i32 = 72;
    pub const CTX_RESUME: i32 = 104;
    pub const CTX_ENTRY: i32 = 136;
    pub const CTX_ENTRY_ARG: i32 = 144;
    pub const CTX_EXIT_STATUS: i32 = 152;
    pub const CTX_RESULT: i32 = 160;
    pub const CTX_STACK_BASE: i32 = 168;
    pub const CTX_STACK_LEN: i32 = 176;
    pub const CTX_SIZE: usize = 192;

    pub const PARK_EVIDENCE: i32 = 0;
    pub const PARK_OP: i32 = 8;
    pub const PARK_FLAGS: i32 = 12;
    pub const PARK_ARG0: i32 = 16;
    pub const PARK_ARG1: i32 = 24;

    pub const RESUME_KIND: i32 = 0;
    pub const RESUME_VALUE: i32 = 8;
    pub const RESUME_DATA_BASE: i32 = 16;
    pub const RESUME_EVIDENCE: i32 = 24;

    pub const EV_ABI_VERSION: i32 = 0;
    pub const EV_LEN: i32 = 4;
    pub const EV_SLOTS: i32 = 8;

    pub const SLOT_IDENTITY: i32 = 0;
    pub const SLOT_EFFECT: i32 = 8;
    pub const SLOT_DEPTH: i32 = 12;
    pub const SLOT_HANDLER_FN: i32 = 16;
    pub const SLOT_STATE_PTR: i32 = 24;
    pub const SLOT_SIZE: i64 = 32;
}

macro_rules! check_offset {
    ($ty:ty, $field:ident, $expected:expr) => {
        const _: () = assert!(offset_of!($ty, $field) == $expected as usize);
    };
}
check_offset!(FiberContext, magic, offsets::CTX_MAGIC);
check_offset!(FiberContext, abi_version, offsets::CTX_ABI_VERSION);
check_offset!(FiberContext, state, offsets::CTX_STATE);
check_offset!(FiberContext, flags, offsets::CTX_FLAGS);
check_offset!(FiberContext, generation, offsets::CTX_GENERATION);
check_offset!(FiberContext, fiber_id, offsets::CTX_FIBER_ID);
check_offset!(FiberContext, saved_sp, offsets::CTX_SAVED_SP);
check_offset!(FiberContext, scheduler_sp, offsets::CTX_SCHEDULER_SP);
check_offset!(FiberContext, evidence, offsets::CTX_EVIDENCE);
check_offset!(FiberContext, data_base, offsets::CTX_DATA_BASE);
check_offset!(FiberContext, data_len, offsets::CTX_DATA_LEN);
check_offset!(FiberContext, park, offsets::CTX_PARK);
check_offset!(FiberContext, resume, offsets::CTX_RESUME);
check_offset!(FiberContext, entry, offsets::CTX_ENTRY);
check_offset!(FiberContext, entry_arg, offsets::CTX_ENTRY_ARG);
check_offset!(FiberContext, exit_status, offsets::CTX_EXIT_STATUS);
check_offset!(FiberContext, result, offsets::CTX_RESULT);
check_offset!(FiberContext, stack_base, offsets::CTX_STACK_BASE);
check_offset!(FiberContext, stack_len, offsets::CTX_STACK_LEN);
check_offset!(ParkRequest, evidence, offsets::PARK_EVIDENCE);
check_offset!(ParkRequest, op, offsets::PARK_OP);
check_offset!(ParkRequest, flags, offsets::PARK_FLAGS);
check_offset!(ParkRequest, arg0, offsets::PARK_ARG0);
check_offset!(ParkRequest, arg1, offsets::PARK_ARG1);
check_offset!(ResumeSignal, kind, offsets::RESUME_KIND);
check_offset!(ResumeSignal, value, offsets::RESUME_VALUE);
check_offset!(ResumeSignal, data_base, offsets::RESUME_DATA_BASE);
check_offset!(ResumeSignal, evidence, offsets::RESUME_EVIDENCE);
check_offset!(EvidenceVectorHeader, abi_version, offsets::EV_ABI_VERSION);
check_offset!(EvidenceVectorHeader, len, offsets::EV_LEN);
check_offset!(EvidenceVectorHeader, slots, offsets::EV_SLOTS);
check_offset!(EvidenceSlot, identity, offsets::SLOT_IDENTITY);
check_offset!(EvidenceSlot, effect, offsets::SLOT_EFFECT);
check_offset!(EvidenceSlot, depth, offsets::SLOT_DEPTH);
check_offset!(EvidenceSlot, handler_fn, offsets::SLOT_HANDLER_FN);
check_offset!(EvidenceSlot, state_ptr, offsets::SLOT_STATE_PTR);
const _: () = assert!(size_of::<FiberContext>() == offsets::CTX_SIZE);
const _: () = assert!(align_of::<FiberContext>() == 64);
const _: () = assert!(size_of::<ParkRequest>() == 32);
const _: () = assert!(size_of::<ResumeSignal>() == 32);
const _: () = assert!(size_of::<EvidenceSlot>() == offsets::SLOT_SIZE as usize);
const _: () = assert!(size_of::<EvidenceVectorHeader>() == 16);
// ABI v1/v2 are defined for 64-bit targets only; pointers are stored as u64.
const _: () = assert!(size_of::<usize>() == 8);
