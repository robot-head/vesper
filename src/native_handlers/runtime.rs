//! The run-time half of the handler ABI: handler records, one-shot
//! continuations on fiber stacks, and the calls generated code makes into
//! them (`vesper_handlers_*`).
//!
//! A `handle` expression runs its body on a fresh fiber (`vesper-fiber-abi`).
//! Performing an operation parks the innermost running fiber with a request
//! naming the target handler. The code that started or resumed that fiber
//! then either runs the matching operation clause on its own stack, passing
//! the parked fiber as the continuation, or, when the request is for an
//! outer handler, performs the same operation itself (parking its own fiber)
//! and resumes the inner fiber with the answer. `resume(v)` resumes the
//! fiber and keeps routing its requests until the handled body finishes, so
//! it returns the value of the rest of the `handle` expression, as deep
//! handlers require. A continuation that a clause does not resume is
//! cancelled when the clause returns: the parked operation returns with the
//! `unwinding` flag set and every generated frame on that fiber returns at
//! once, up to the fiber's entry.
use super::HandlerAbiError;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use vesper_fiber_abi::abi::{FiberContext, ParkRequest, ResumeKind, exit};
use vesper_fiber_abi::evidence::{EvidenceRegistry, EvidenceVector};
use vesper_fiber_abi::fiber::{Fiber, FiberConfig, Outcome, Parked, Resumption, vesper_fiber_park};

/// Stack size of each handled body's fiber. Deep handlers nest one
/// `drive` per resumed operation on the stack that runs the handler, so
/// a deep recursion that performs at every level needs room; the mapping
/// is reserved lazily (`MAP_NORESERVE`), so unused pages cost nothing.
const STACK_SIZE: usize = 16 << 20;

/// Byte offsets into a handle descriptor written by generated code (words).
pub mod descriptor {
    pub const BODY: i32 = 0;
    pub const ENV: i32 = 8;
    pub const RET: i32 = 16;
    pub const STATEFUL: i32 = 24;
    pub const STATE: i32 = 32;
    pub const CLAUSES: i32 = 40;
    /// Followed by the clause code pointers, one per operation.
    pub const CLAUSE0: i32 = 48;
}

/// The run-time context every generated function receives first. Generated
/// code reads only `unwinding` (offset 0); the rest is private to Rust.
#[repr(C)]
pub struct Rt {
    pub unwinding: Cell<u64>,
    state: RefCell<State>,
    error: RefCell<Option<String>>,
    evidence: Rc<EvidenceVector>,
    evidence_id: u64,
    code: Rc<dyn Any>,
}

#[derive(Default)]
struct State {
    /// Heap words allocated by generated code (closure and environment
    /// records); freed when the run ends.
    arena: Vec<Box<[u64]>>,
    /// Every handler installed in this run; kept until the run ends so that
    /// a stale capability is detected instead of dangling. Boxed: a
    /// capability is the record's address, which must not move.
    #[allow(clippy::vec_box)]
    handlers: Vec<Box<HandlerRec>>,
    /// Contexts of the fibers being run, innermost last.
    running: Vec<*mut FiberContext>,
    /// The operation the parking fiber requested.
    pending: Option<Request>,
    fibers: u64,
}

struct Request {
    target: *const HandlerRec,
    op: u32,
    args: Vec<u64>,
}

pub struct HandlerRec {
    env: u64,
    ret: u64,
    clauses: Vec<u64>,
    stateful: bool,
    state: Cell<u64>,
    active: Cell<bool>,
}

/// What the fiber trampoline runs, and where it leaves the result.
struct Start {
    rt: *const Rt,
    body: u64,
    env: u64,
    cap: u64,
    result: Cell<u64>,
}

/// A one-shot continuation handed to an operation clause.
struct Cont {
    parked: Option<Parked>,
    handler: *const HandlerRec,
    start: *const Start,
}

impl Rt {
    pub fn new(code: Rc<dyn Any>) -> Result<Box<Self>, HandlerAbiError> {
        let mut registry = EvidenceRegistry::new();
        let evidence = EvidenceVector::install(&mut registry, &[])
            .map_err(|e| HandlerAbiError::Runtime(format!("{e:?}")))?;
        let evidence_id = registry
            .fresh()
            .map_err(|e| HandlerAbiError::Runtime(format!("{e:?}")))?;
        Ok(Box::new(Self {
            unwinding: Cell::new(0),
            state: RefCell::default(),
            error: RefCell::default(),
            evidence: Rc::new(evidence),
            evidence_id,
            code,
        }))
    }
    pub fn error(&self) -> Option<String> {
        self.error.borrow().clone()
    }
    /// Fibers started during the run.
    pub fn fibers(&self) -> u64 {
        self.state.borrow().fibers
    }
    fn fault(&self, message: impl Into<String>) -> u64 {
        let mut error = self.error.borrow_mut();
        if error.is_none() {
            *error = Some(message.into());
        }
        self.unwinding.set(1);
        0
    }
    fn faulted(&self) -> bool {
        self.error.borrow().is_some()
    }
    fn push(&self, ctx: *const FiberContext) {
        self.state.borrow_mut().running.push(ctx.cast_mut());
    }
    fn pop(&self) {
        self.state.borrow_mut().running.pop();
    }
    fn resumption(&self, value: u64) -> Resumption {
        Resumption::value(self.evidence_id, value)
    }

    /// Run the fiber until it parks or exits, with its context on the
    /// running stack meanwhile.
    fn run_fiber(&self, fiber: Fiber) -> Outcome {
        self.push(fiber.context_ptr());
        let outcome = fiber.start();
        self.pop();
        outcome
    }
    fn resume_fiber(&self, parked: Parked, value: u64) -> Option<Outcome> {
        self.push(parked.fiber().context_ptr());
        let outcome = parked.resume(self.resumption(value));
        self.pop();
        match outcome {
            Ok(outcome) => Some(outcome),
            Err(rejected) => {
                self.fault(format!("resume rejected: {:?}", rejected.error));
                None
            }
        }
    }
    /// Cancel a continuation nobody resumed: its pending operation returns
    /// with `unwinding` set, and its generated frames return to the entry.
    fn cancel(&self, parked: Parked) {
        self.push(parked.fiber().context_ptr());
        let outcome = parked.cancel();
        self.pop();
        if let Outcome::Parked(again) = outcome {
            // Generated code never parks while unwinding; this is the fiber
            // crate's documented abandonment fallback.
            drop(again);
            self.fault("a cancelled continuation parked again");
        }
    }

    /// Route the requests of a handled body's fiber until the body finishes
    /// (the return clause's value) or an operation clause for this handler
    /// returns (that clause's value).
    fn drive(&self, handler: *const HandlerRec, start: *const Start, mut outcome: Outcome) -> u64 {
        // SAFETY: handler records live in `State::handlers` until the run
        // ends; `start` lives in the `handle` call frame, which is below
        // every frame that can reach this loop for that handler.
        let h = unsafe { &*handler };
        loop {
            match outcome {
                Outcome::Exited(exit) => {
                    if self.faulted() || exit.status != exit::COMPLETE {
                        return self.fault_or_zero();
                    }
                    h.active.set(false);
                    let result = unsafe { (*start).result.get() };
                    return call(self, h.ret, &[h.env, result, h.state.get()]);
                }
                Outcome::Parked(parked) => {
                    let Some(request) = self.state.borrow_mut().pending.take() else {
                        self.cancel(parked);
                        return self.fault("a fiber parked without a request");
                    };
                    if std::ptr::eq(request.target, handler) {
                        let Some(&clause) = h.clauses.get(request.op as usize) else {
                            self.cancel(parked);
                            return self.fault("unknown operation");
                        };
                        let k = Box::into_raw(Box::new(Cont {
                            parked: Some(parked),
                            handler,
                            start,
                        }));
                        let mut args = vec![h.env, k as u64, h.state.get()];
                        args.extend(&request.args);
                        let value = call(self, clause, &args);
                        // SAFETY: `k` came from `Box::into_raw` above; the
                        // clause only borrowed it through `resume`.
                        let k = unsafe { Box::from_raw(k) };
                        if let Some(parked) = k.parked {
                            self.cancel(parked);
                        }
                        return if self.faulted() { 0 } else { value };
                    }
                    // An outer handler's operation: perform it here and hand
                    // the answer back to the inner fiber.
                    let value = self.perform_now(request.target, request.op, request.args);
                    if self.unwinding.get() != 0 {
                        self.cancel(parked);
                        return 0;
                    }
                    match self.resume_fiber(parked, value) {
                        Some(next) => outcome = next,
                        None => return 0,
                    }
                }
            }
        }
    }
    fn fault_or_zero(&self) -> u64 {
        if !self.faulted() && self.unwinding.get() == 0 {
            return self.fault("a handled body exited abnormally");
        }
        0
    }

    /// Park the running fiber with an operation for `target`; the value it
    /// is resumed with is the operation's result.
    fn perform_now(&self, target: *const HandlerRec, op: u32, args: Vec<u64>) -> u64 {
        // SAFETY: as in `drive`.
        if !unsafe { &*target }.active.get() {
            return self.fault("operation performed on a handler that is no longer installed");
        }
        let Some(&ctx) = self.state.borrow().running.last() else {
            return self.fault("unhandled effect operation");
        };
        self.state.borrow_mut().pending = Some(Request { target, op, args });
        // SAFETY: `ctx` is the context of the fiber running this code (the
        // innermost entry of `running`), so it is the running generated
        // entry's context, as `vesper_fiber_park` requires.
        let signal = unsafe {
            (*ctx).park = ParkRequest {
                evidence: self.evidence_id,
                op,
                ..ParkRequest::default()
            };
            vesper_fiber_park(ctx)
        };
        if signal.is_null() {
            return self.fault("fiber park rejected");
        }
        // SAFETY: a non-null result points at the context's resume signal.
        let signal = unsafe { *signal };
        if signal.kind == ResumeKind::Cancelled as u32 {
            self.unwinding.set(1);
            return 0;
        }
        signal.value
    }
}

/// Call generated code `f` with `rt` and `args` (all words).
fn call(rt: &Rt, f: u64, args: &[u64]) -> u64 {
    let r = rt as *const Rt as u64;
    type F0 = unsafe extern "C" fn(u64) -> u64;
    type F1 = unsafe extern "C" fn(u64, u64) -> u64;
    type F2 = unsafe extern "C" fn(u64, u64, u64) -> u64;
    type F3 = unsafe extern "C" fn(u64, u64, u64, u64) -> u64;
    type F4 = unsafe extern "C" fn(u64, u64, u64, u64, u64) -> u64;
    type F5 = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64;
    type F6 = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64, u64) -> u64;
    type F7 = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64;
    // SAFETY: `f` is a finalized function of this run's module whose
    // signature is `(rt, words..) -> word` with exactly `args.len()` words
    // (the lowering checks operation arity against `MAX_OPERATION_ARGS`).
    unsafe {
        match *args {
            [] => std::mem::transmute::<u64, F0>(f)(r),
            [a] => std::mem::transmute::<u64, F1>(f)(r, a),
            [a, b] => std::mem::transmute::<u64, F2>(f)(r, a, b),
            [a, b, c] => std::mem::transmute::<u64, F3>(f)(r, a, b, c),
            [a, b, c, d] => std::mem::transmute::<u64, F4>(f)(r, a, b, c, d),
            [a, b, c, d, e] => std::mem::transmute::<u64, F5>(f)(r, a, b, c, d, e),
            [a, b, c, d, e, g] => std::mem::transmute::<u64, F6>(f)(r, a, b, c, d, e, g),
            [a, b, c, d, e, g, h] => std::mem::transmute::<u64, F7>(f)(r, a, b, c, d, e, g, h),
            _ => rt.fault("too many words for a generated call"),
        }
    }
}

/// Words a generated operation may carry (clauses take `env, k, state` too).
pub const MAX_OPERATION_ARGS: usize = 4;

unsafe extern "C" fn trampoline(_ctx: *mut FiberContext, arg: u64) -> u64 {
    // SAFETY: `arg` is the `Start` the `handle` call frame owns; it outlives
    // the fiber (the frame waits for the body to exit or be cancelled).
    let start = unsafe { &*(arg as *const Start) };
    let rt = unsafe { &*start.rt };
    let value = call(rt, start.body, &[start.env, start.cap]);
    start.result.set(value);
    if rt.unwinding.get() != 0 {
        if rt.faulted() {
            return exit::FAILED;
        }
        // A cancellation ends at the cancelled fiber's own entry.
        rt.unwinding.set(0);
        return exit::CANCELLED;
    }
    exit::COMPLETE
}

/// `vesper_handlers_handle(rt, descriptor) -> word`: install a handler, run
/// the body on a fiber, and route its operations.
///
/// # Safety
///
/// Called only by code this module's lowering generated, with the `rt` it
/// was entered with and pointers it wrote (descriptors, argument arrays,
/// continuations and capabilities this runtime handed out).
pub unsafe extern "C" fn handle(rt: *const Rt, desc: *const u64) -> u64 {
    // SAFETY: generated code passes its own `rt` and a descriptor it wrote
    // (`descriptor` layout, `CLAUSES` words of clause pointers).
    let rt = unsafe { &*rt };
    if rt.unwinding.get() != 0 {
        return 0;
    }
    let word = |offset: i32| unsafe { *desc.add(offset as usize / 8) };
    let count = word(descriptor::CLAUSES) as usize;
    let clauses = (0..count)
        .map(|i| word(descriptor::CLAUSE0 + 8 * i as i32))
        .collect();
    let record = Box::new(HandlerRec {
        env: word(descriptor::ENV),
        ret: word(descriptor::RET),
        clauses,
        stateful: word(descriptor::STATEFUL) != 0,
        state: Cell::new(word(descriptor::STATE)),
        active: Cell::new(true),
    });
    let handler: *const HandlerRec = &*record;
    {
        let mut state = rt.state.borrow_mut();
        state.handlers.push(record);
        state.fibers += 1;
    }
    let start = Start {
        rt,
        body: word(descriptor::BODY),
        env: word(descriptor::ENV),
        cap: handler as u64,
        result: Cell::new(0),
    };
    // SAFETY: the trampoline is a valid entry; `start` outlives the fiber
    // (this frame drives it to exit or cancellation before returning).
    let fiber = unsafe {
        Fiber::new(
            trampoline,
            &start as *const Start as u64,
            rt.evidence.clone(),
            rt.code.clone(),
            FiberConfig {
                stack_size: STACK_SIZE,
                ..FiberConfig::default()
            },
        )
    };
    let fiber = match fiber {
        Ok(fiber) => fiber,
        Err(e) => return rt.fault(format!("fiber: {e}")),
    };
    let outcome = rt.run_fiber(fiber);
    let value = rt.drive(handler, &start, outcome);
    // SAFETY: handler records are kept until the run ends.
    unsafe { &*handler }.active.set(false);
    value
}

/// `vesper_handlers_perform(rt, cap, op, args, count) -> word`.
///
/// # Safety
///
/// Called only by code this module's lowering generated, with the `rt` it
/// was entered with and pointers it wrote (descriptors, argument arrays,
/// continuations and capabilities this runtime handed out).
pub unsafe extern "C" fn perform(
    rt: *const Rt,
    cap: u64,
    op: u64,
    args: *const u64,
    count: u64,
) -> u64 {
    // SAFETY: generated code passes its `rt` and `count` words at `args`.
    let rt = unsafe { &*rt };
    if rt.unwinding.get() != 0 {
        return 0;
    }
    let args = (0..count as usize)
        .map(|i| unsafe { *args.add(i) })
        .collect();
    rt.perform_now(cap as *const HandlerRec, op as u32, args)
}

/// `vesper_handlers_resume(rt, k, value, state) -> word`: resume a one-shot
/// continuation and return the value of the rest of its `handle`.
///
/// # Safety
///
/// Called only by code this module's lowering generated, with the `rt` it
/// was entered with and pointers it wrote (descriptors, argument arrays,
/// continuations and capabilities this runtime handed out).
pub unsafe extern "C" fn resume(rt: *const Rt, k: u64, value: u64, state: u64) -> u64 {
    // SAFETY: `k` is the continuation `drive` passed to the running clause.
    let rt = unsafe { &*rt };
    if rt.unwinding.get() != 0 {
        return 0;
    }
    let k = unsafe { &mut *(k as *mut Cont) };
    let Some(parked) = k.parked.take() else {
        return rt.fault("a one-shot continuation was resumed twice");
    };
    let h = unsafe { &*k.handler };
    if h.stateful {
        h.state.set(state);
    }
    match rt.resume_fiber(parked, value) {
        Some(outcome) => rt.drive(k.handler, k.start, outcome),
        None => 0,
    }
}

/// `vesper_handlers_alloc(rt, words) -> pointer` to zeroed words that live
/// until the run ends.
///
/// # Safety
///
/// Called only by code this module's lowering generated, with the `rt` it
/// was entered with and pointers it wrote (descriptors, argument arrays,
/// continuations and capabilities this runtime handed out).
pub unsafe extern "C" fn alloc(rt: *const Rt, words: u64) -> u64 {
    let rt = unsafe { &*rt };
    let mut block = vec![0u64; words.max(1) as usize].into_boxed_slice();
    let ptr = block.as_mut_ptr() as u64;
    rt.state.borrow_mut().arena.push(block);
    ptr
}

/// `vesper_handlers_fault(rt, code)`: a run-time check failed.
///
/// # Safety
///
/// Called only by code this module's lowering generated, with the `rt` it
/// was entered with and pointers it wrote (descriptors, argument arrays,
/// continuations and capabilities this runtime handed out).
pub unsafe extern "C" fn fault(rt: *const Rt, code: u64) -> u64 {
    let rt = unsafe { &*rt };
    rt.fault(match code {
        1 => "unhandled operation".to_owned(),
        2 => "raise resumed".to_owned(),
        other => format!("run-time fault {other}"),
    })
}
