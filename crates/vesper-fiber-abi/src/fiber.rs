//! One-shot fibers running generated code on pinned stacks.
//!
//! Ownership summary (see the RFC for the full contract):
//! * [`Fiber`] owns the context, stack, data frame, a share of the evidence
//!   vector and a share of the code that the stack may return into.
//! * [`Parked`] is the one-shot continuation. It is move-only; `resume`
//!   consumes it, and a rejected resume hands it back in [`Rejected`].
//! * [`ContinuationTable`] maps raw `(fiber_id, generation)` tokens (the value
//!   a completion queue carries) back to their owned continuation and rejects
//!   unknown, repeated and stale tokens without consuming anything.
use crate::abi::{
    ABI_VERSION, CONTEXT_MAGIC, EvidenceId, FLAG_STACK_PINNED, FiberContext, FiberState,
    ParkRequest, ResumeKind, ResumeSignal, exit,
};
use crate::data::DataFrame;
use crate::evidence::EvidenceVector;
use crate::stack::FiberStack;
use crate::switch::{initial_stack, vesper_fiber_switch};
use std::any::Any;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use vesper_pulse::frames::{FrameError, FrameStack};

/// Generated fiber entry: `(ctx, arg) -> exit status`.
pub type EntryFn = unsafe extern "C" fn(*mut FiberContext, u64) -> u64;

static NEXT_FIBER: AtomicU64 = AtomicU64::new(1);

fn allocate_fiber_id(counter: &AtomicU64) -> io::Result<u64> {
    let mut id = counter.load(Ordering::Relaxed);
    loop {
        let next = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("fiber identities exhausted"))?;
        match counter.compare_exchange_weak(id, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(id),
            Err(current) => id = current,
        }
    }
}

fn next_generation(generation: u64) -> Option<u64> {
    generation.checked_add(1)
}
thread_local! {
    static ABANDONED: Cell<u64> = const { Cell::new(0) };
}

/// Number of fibers on this thread that were dropped while still parked after
/// their cancellation resume (their linear obligations were not discharged).
pub fn abandoned_fibers() -> u64 {
    ABANDONED.with(Cell::get)
}

/// Park entry called by generated code after it filled `ctx.park`.
///
/// Returns a pointer to `ctx.resume` once resumed, or null if `ctx` is not a
/// running ABI v1 context (generated code must then exit with
/// [`exit::ABI_VIOLATION`]).
///
/// # Safety
/// `ctx` must be null or the context passed to the running generated entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vesper_fiber_park(ctx: *mut FiberContext) -> *const ResumeSignal {
    if ctx.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller passes its own live context; fields are accessed
    // through the raw pointer only.
    unsafe {
        if (*ctx).magic != CONTEXT_MAGIC
            || (*ctx).abi_version != ABI_VERSION
            || (*ctx).state != FiberState::Running as u32
        {
            return std::ptr::null();
        }
        let Some(generation) = next_generation((*ctx).generation) else {
            return std::ptr::null();
        };
        (*ctx).state = FiberState::Parked as u32;
        (*ctx).generation = generation;
        vesper_fiber_switch(&raw mut (*ctx).saved_sp, (*ctx).scheduler_sp, 0);
        &raw const (*ctx).resume
    }
}

unsafe extern "C" fn fiber_main(ctx: *mut FiberContext, _arg: u64) -> ! {
    // SAFETY: `ctx` was installed by `Fiber::new` and outlives the fiber.
    unsafe {
        let entry: EntryFn = std::mem::transmute::<u64, EntryFn>((*ctx).entry);
        let status = entry(ctx, (*ctx).entry_arg);
        (*ctx).exit_status = status;
        (*ctx).state = FiberState::Exited as u32;
        let mut discarded = 0u64;
        vesper_fiber_switch(&mut discarded, (*ctx).scheduler_sp, 0);
    }
    // An exited context is never switched to again.
    std::process::abort()
}

/// Parameters for a new fiber.
#[derive(Clone, Copy, Debug)]
pub struct FiberConfig {
    pub stack_size: usize,
    pub data_bytes: usize,
}

impl Default for FiberConfig {
    fn default() -> Self {
        Self {
            stack_size: crate::stack::DEFAULT_STACK_SIZE,
            data_bytes: 64,
        }
    }
}

pub struct Fiber {
    ctx: NonNull<FiberContext>,
    stack: FiberStack,
    data: DataFrame,
    evidence: Rc<EvidenceVector>,
    /// Keeps the machine code alive while any frame on `stack` may return into
    /// it. Dropped only together with the stack.
    _code: Rc<dyn Any>,
    parks: u64,
    cancel_on_drop: bool,
}

impl fmt::Debug for Fiber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fiber")
            .field("id", &self.id())
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl Fiber {
    /// Prepare a fiber that will call `entry(ctx, arg)` on its own stack.
    ///
    /// # Safety
    /// `entry` must follow the ABI v1 entry contract and must be machine code
    /// owned (directly or transitively) by `code`.
    pub unsafe fn new(
        entry: EntryFn,
        arg: u64,
        evidence: Rc<EvidenceVector>,
        code: Rc<dyn Any>,
        config: FiberConfig,
    ) -> io::Result<Self> {
        let fiber_id = allocate_fiber_id(&NEXT_FIBER)?;
        let stack = FiberStack::new(config.stack_size)?;
        let data = DataFrame::new(config.data_bytes);
        let ctx = Box::new(FiberContext {
            magic: CONTEXT_MAGIC,
            abi_version: ABI_VERSION,
            state: FiberState::Created as u32,
            flags: FLAG_STACK_PINNED,
            generation: 0,
            fiber_id,
            saved_sp: 0,
            scheduler_sp: 0,
            evidence: evidence.header(),
            data_base: data.base(),
            data_len: data.len() as u64,
            park: ParkRequest::default(),
            resume: ResumeSignal::default(),
            entry: entry as *const () as u64,
            entry_arg: arg,
            exit_status: 0,
            result: 0,
            stack_base: stack.base() as u64,
            stack_len: stack.len() as u64,
        });
        let ctx = NonNull::new(Box::into_raw(ctx)).unwrap();
        // SAFETY: the stack is fresh, aligned and exclusively ours.
        let sp = unsafe { initial_stack(stack.top() as u64, ctx.as_ptr(), fiber_main) };
        // SAFETY: just allocated.
        unsafe { (*ctx.as_ptr()).saved_sp = sp };
        Ok(Self {
            ctx,
            stack,
            data,
            evidence,
            _code: code,
            parks: 0,
            cancel_on_drop: true,
        })
    }
    fn ctx(&self) -> &FiberContext {
        // SAFETY: owned; no generated code runs while `&self` is observable.
        unsafe { self.ctx.as_ref() }
    }
    pub fn id(&self) -> u64 {
        self.ctx().fiber_id
    }
    pub fn state(&self) -> FiberState {
        FiberState::from_raw(self.ctx().state).unwrap_or(FiberState::Poisoned)
    }
    pub fn context_ptr(&self) -> *const FiberContext {
        self.ctx.as_ptr()
    }
    pub fn data(&self) -> &[u8] {
        self.data.bytes()
    }
    pub fn data_mut(&mut self) -> &mut [u8] {
        self.data.bytes_mut()
    }
    pub fn evidence(&self) -> &EvidenceVector {
        &self.evidence
    }
    /// Start the fiber. Consumes the created fiber; returns the continuation
    /// if it parks.
    pub fn start(mut self) -> Outcome {
        debug_assert_eq!(self.state(), FiberState::Created);
        self.run(None);
        Box::new(self).into_outcome()
    }
    /// Switch onto the fiber stack until it parks or exits.
    fn run(&mut self, signal: Option<ResumeSignal>) {
        let c = self.ctx.as_ptr();
        // SAFETY: the fiber is Created or Parked (checked by callers), so its
        // saved_sp holds a valid switch frame on the pinned stack.
        unsafe {
            let mut signal = signal.unwrap_or_default();
            signal.data_base = self.data.base();
            (*c).resume = signal;
            (*c).data_base = self.data.base();
            (*c).data_len = self.data.len() as u64;
            (*c).state = FiberState::Running as u32;
            // Created generations are zero; parked generations are even.
            // A running generation may reach u64::MAX, but its next park is
            // rejected above rather than wrapping and revalidating old tokens.
            (*c).generation = next_generation((*c).generation)
                .expect("created or parked generation must have a successor");
            vesper_fiber_switch(&raw mut (*c).scheduler_sp, (*c).saved_sp, 0);
        }
        // The fiber has run past every reload point that could observe a
        // retired allocation.
        self.data.release_retired();
        if self.state() == FiberState::Parked {
            self.parks += 1;
        }
    }
    fn into_outcome(self: Box<Self>) -> Outcome {
        match self.state() {
            FiberState::Parked => Outcome::Parked(Parked { fiber: self }),
            _ => {
                let ctx = self.ctx();
                Outcome::Exited(Exit {
                    fiber_id: ctx.fiber_id,
                    status: ctx.exit_status,
                    result: ctx.result,
                    data: self.data.bytes().to_vec(),
                    parks: self.parks,
                    data_relocations: self.data.relocations(),
                })
            }
        }
    }
}

impl Drop for Fiber {
    fn drop(&mut self) {
        // A dropped continuation still resumes exactly once, along its
        // cancellation edge. If it parks again it is abandoned; freeing the
        // stack is memory safe (no frame on it is executing and nothing points
        // into it) but its remaining obligations are lost and counted.
        if self.state() == FiberState::Parked && self.cancel_on_drop {
            self.cancel_on_drop = false;
            let evidence = self.ctx().park.evidence;
            self.run(Some(ResumeSignal {
                kind: ResumeKind::Cancelled as u32,
                evidence,
                ..Default::default()
            }));
            if self.state() == FiberState::Parked {
                ABANDONED.with(|a| a.set(a.get() + 1));
            }
        }
        // SAFETY: allocated by Box::into_raw in `new`; not running.
        drop(unsafe { Box::from_raw(self.ctx.as_ptr()) });
    }
}

/// Result of starting or resuming a fiber.
#[derive(Debug)]
pub enum Outcome {
    Parked(Parked),
    Exited(Exit),
}

impl Outcome {
    pub fn parked(self) -> Option<Parked> {
        match self {
            Outcome::Parked(p) => Some(p),
            Outcome::Exited(_) => None,
        }
    }
    pub fn exited(self) -> Option<Exit> {
        match self {
            Outcome::Exited(e) => Some(e),
            Outcome::Parked(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    pub fiber_id: u64,
    /// One of [`exit`].
    pub status: u64,
    pub result: u64,
    /// Final data frame image.
    pub data: Vec<u8>,
    pub parks: u64,
    pub data_relocations: u64,
}

impl Exit {
    pub fn completed(&self) -> bool {
        self.status == exit::COMPLETE
    }
    pub fn word(&self, index: usize) -> u64 {
        u64::from_le_bytes(self.data[index * 8..index * 8 + 8].try_into().unwrap())
    }
}

/// How the resumer answers a park.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resumption {
    pub evidence: EvidenceId,
    pub kind: ResumeKind,
    pub value: u64,
}

impl Resumption {
    pub fn value(evidence: EvidenceId, value: u64) -> Self {
        Self {
            evidence,
            kind: ResumeKind::Value,
            value,
        }
    }
    pub fn error(evidence: EvidenceId, code: u64) -> Self {
        Self {
            evidence,
            kind: ResumeKind::Error,
            value: code,
        }
    }
    pub fn cancelled(evidence: EvidenceId) -> Self {
        Self {
            evidence,
            kind: ResumeKind::Cancelled,
            value: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeError {
    /// The resumption names a different handler installation than the park.
    WrongEvidence {
        parked: EvidenceId,
        offered: EvidenceId,
    },
    /// No parked continuation with this fiber id is owned by the table (it was
    /// already resumed, or never inserted).
    UnknownToken,
    /// The fiber is parked, but at a later generation than the token.
    StaleToken { current: u64, offered: u64 },
}

impl fmt::Display for ResumeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ResumeError {}

/// A rejected resume returns ownership of the untouched continuation.
#[derive(Debug)]
pub struct Rejected {
    pub error: ResumeError,
    pub continuation: Parked,
}

/// The raw one-shot continuation token, e.g. carried in `sqe.user_data`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawContinuation {
    pub fiber_id: u64,
    pub generation: u64,
}

/// A parked one-shot continuation (`k :_linear^own`).
///
/// ```compile_fail
/// fn twice(k: vesper_fiber_abi::Parked, r: vesper_fiber_abi::Resumption) {
///     let _ = k.resume(r);
///     let _ = k.resume(r); // error[E0382]: use of moved value
/// }
/// ```
#[derive(Debug)]
pub struct Parked {
    fiber: Box<Fiber>,
}

impl Parked {
    pub fn request(&self) -> ParkRequest {
        self.fiber.ctx().park
    }
    pub fn token(&self) -> RawContinuation {
        RawContinuation {
            fiber_id: self.fiber.id(),
            generation: self.fiber.ctx().generation,
        }
    }
    pub fn fiber_id(&self) -> u64 {
        self.fiber.id()
    }
    pub fn fiber(&self) -> &Fiber {
        &self.fiber
    }
    /// The suspended data frame. Writes are observed by the fiber after it
    /// resumes (this is how completions deliver into mutable places).
    pub fn data(&self) -> &[u8] {
        self.fiber.data()
    }
    pub fn data_mut(&mut self) -> &mut [u8] {
        self.fiber.data_mut()
    }
    pub fn data_base(&self) -> u64 {
        self.fiber.data.base()
    }
    /// Move the suspended data frame to a new allocation.
    pub fn relocate_data(&mut self) {
        self.fiber.data.relocate();
    }
    /// Move the suspended data frame through a `vesper_pulse` frame stack.
    pub fn relocate_data_through(&mut self, frames: &mut FrameStack) -> Result<(), FrameError> {
        self.fiber.data.relocate_through(frames)
    }
    /// Words between the saved stack pointer and the stack top whose values
    /// point into the same physical stack, as `(address, value)` pairs. A
    /// non-empty result shows the stack cannot be moved by copying bytes.
    pub fn stack_self_pointers(&self) -> Vec<(u64, u64)> {
        let sp = self.fiber.ctx().saved_sp;
        let top = self.fiber.stack.top() as u64;
        let mut found = vec![];
        let mut at = sp;
        while at + 8 <= top {
            // SAFETY: [saved_sp, top) is initialized, mapped, and not running.
            let value = unsafe { (at as *const u64).read() };
            if self.fiber.stack.contains(value) {
                found.push((at, value));
            }
            at += 8;
        }
        found
    }
    /// Resume exactly once. On rejection the continuation is returned intact.
    pub fn resume(mut self, resumption: Resumption) -> Result<Outcome, Rejected> {
        let parked = self.fiber.ctx().park.evidence;
        if resumption.evidence != parked {
            return Err(Rejected {
                error: ResumeError::WrongEvidence {
                    parked,
                    offered: resumption.evidence,
                },
                continuation: self,
            });
        }
        self.fiber.run(Some(ResumeSignal {
            kind: resumption.kind as u32,
            value: resumption.value,
            evidence: resumption.evidence,
            ..Default::default()
        }));
        Ok(self.fiber.into_outcome())
    }
}

/// Owns parked continuations keyed by raw token, as a completion table does.
#[derive(Default, Debug)]
pub struct ContinuationTable {
    parked: BTreeMap<u64, Parked>,
}

impl ContinuationTable {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.parked.len()
    }
    pub fn is_empty(&self) -> bool {
        self.parked.is_empty()
    }
    pub fn insert(&mut self, k: Parked) -> RawContinuation {
        let token = k.token();
        self.parked.insert(token.fiber_id, k);
        token
    }
    fn check(&self, token: RawContinuation) -> Result<(), ResumeError> {
        let k = self
            .parked
            .get(&token.fiber_id)
            .ok_or(ResumeError::UnknownToken)?;
        let current = k.token().generation;
        if current != token.generation {
            return Err(ResumeError::StaleToken {
                current,
                offered: token.generation,
            });
        }
        Ok(())
    }
    pub fn get_mut(&mut self, token: RawContinuation) -> Result<&mut Parked, ResumeError> {
        self.check(token)?;
        Ok(self.parked.get_mut(&token.fiber_id).unwrap())
    }
    /// Remove a continuation without resuming it.
    pub fn take(&mut self, token: RawContinuation) -> Result<Parked, ResumeError> {
        self.check(token)?;
        Ok(self.parked.remove(&token.fiber_id).unwrap())
    }
    /// Resume the continuation named by `token`. Any rejection leaves the
    /// table unchanged.
    pub fn resume(
        &mut self,
        token: RawContinuation,
        resumption: Resumption,
    ) -> Result<Outcome, ResumeError> {
        let k = self.take(token)?;
        match k.resume(resumption) {
            Ok(outcome) => Ok(outcome),
            Err(rejected) => {
                self.parked.insert(token.fiber_id, rejected.continuation);
                Err(rejected.error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_identities_are_never_reused() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_fiber_id(&counter).unwrap(), u64::MAX - 1);
        assert!(allocate_fiber_id(&counter).is_err());
        assert!(allocate_fiber_id(&counter).is_err());
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn generations_cannot_wrap_to_an_old_token() {
        assert_eq!(next_generation(u64::MAX - 1), Some(u64::MAX));
        assert_eq!(next_generation(u64::MAX), None);
    }
}
