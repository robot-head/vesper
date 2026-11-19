//! The native handler ABI (`vesper.native.handler-abi/1`, wave nine lane 9,
//! #586): user effects with resuming handlers, and function values, compiled
//! once and passed at run time instead of being specialized into callers.
//!
//! The specializing native backend (`crate::native`) inlines every function
//! with a user effect into each handler that reaches it, and resolves every
//! closure statically. This backend compiles each source function, closure
//! body, handled body and handler clause exactly once:
//!
//! * **Capabilities.** A function receives one capability word per
//!   operation-carrying effect in its row, after the run-time context, in a
//!   canonical order (by effect name and arguments, so that a closure built
//!   with an inferred row and called through a written type agree). A capability is a pointer to the run-time handler record the
//!   enclosing `handle` installed. Capability-only effects (`Realtime`,
//!   `ConstTime`, ...) have no operations and pass nothing, as in the
//!   wave-seven capability ABI.
//! * **Function values.** A function value is a pointer to a closure record
//!   `[code, captured words..]`; calling it passes the record, then the
//!   capabilities of the function type's row, then the arguments. A source
//!   function used as a value gets a record whose code forwards to it.
//! * **Handlers.** `handle` runs its body on a fresh fiber
//!   (`vesper-fiber-abi`); operations park that fiber; clauses run on the
//!   handler's stack and receive the one-shot continuation; `resume` returns
//!   the value of the rest of the `handle` (deep handlers). See
//!   [`runtime`] for the protocol.
//!
//! The supported slice is scalar: `i64`, `u64`, `u32`, `u8`, `bool`, unit and
//! function values, checked arithmetic raising `Fail[ArithOverflow]`, `let`,
//! `if`, literal `match`, assignment to locals, closures, calls, effect
//! operations with up to four arguments, stateful and stateless handlers.
//! Everything else is rejected with [`HandlerAbiError::Unsupported`]; there is
//! no fallback to the specializing backend inside this one.
mod lower;
pub mod runtime;

use crate::check::World;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_jit::{JITBuilder, JITModule};
use std::fmt;
use std::rc::Rc;

pub use lower::{Kind, effect_key};

/// Version of this ABI. Additive to native/fiber ABI v2: it reuses fiber ABI
/// v2 contexts and parks, and changes nothing the specializing backend emits.
pub const ABI: &str = "vesper.native.handler-abi/1";

/// The specializing backend (`crate::native`) inlines each handler clause at
/// its operation and resumes by jumping back into the handled body. A clause
/// that ends in its resumption (`op(v, k) => k(..)`) needs nothing after the
/// resumption returns, so its operation may run any number of times from a
/// loop of the handled body. A clause with work after `k(..)` keeps that work
/// in one frame per operation site, which a nested resumption from a later
/// loop iteration would overwrite, so the backend refuses it there instead of
/// returning to the wrong point.
pub const LOOP_RESUME_REFUSAL: &str = "native handlers: an operation performed inside a loop of its handled body needs a clause that resumes in tail position (`op(v, k) => k(..)`); this clause has work after its resumption";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerAbiError {
    /// The program uses something outside the supported slice.
    Unsupported(String),
    /// The checked program or the code generator is inconsistent.
    Invalid(String),
    /// A run-time check failed (for example a continuation resumed twice).
    Runtime(String),
}
impl fmt::Display for HandlerAbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(s) => write!(f, "handler ABI: unsupported: {s}"),
            Self::Invalid(s) => write!(f, "handler ABI: invalid: {s}"),
            Self::Runtime(s) => write!(f, "handler ABI: run-time fault: {s}"),
        }
    }
}
impl std::error::Error for HandlerAbiError {}

/// The outcome of a run.
#[derive(Debug, Clone)]
pub struct Run {
    pub value: crate::runtime::Value,
    /// Fibers started (one per `handle` evaluated).
    pub fibers: u64,
}

/// Machine code per compiled function, in bytes.
#[derive(Debug, Clone, Default)]
pub struct CodeSize {
    pub functions: std::collections::BTreeMap<String, usize>,
}
impl CodeSize {
    pub fn total(&self) -> usize {
        self.functions.values().sum()
    }
}

fn jit_module() -> Result<JITModule, HandlerAbiError> {
    let mut flags = settings::builder();
    for (k, v) in [("use_colocated_libcalls", "false"), ("is_pic", "false")] {
        flags
            .set(k, v)
            .map_err(|e| HandlerAbiError::Invalid(e.to_string()))?;
    }
    let isa = cranelift_native::builder()
        .map_err(|e| HandlerAbiError::Unsupported(e.to_string()))?
        .finish(settings::Flags::new(flags))
        .map_err(|e| HandlerAbiError::Invalid(e.to_string()))?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    builder.symbol("vesper_handlers_handle", runtime::handle as *const u8);
    builder.symbol("vesper_handlers_perform", runtime::perform as *const u8);
    builder.symbol("vesper_handlers_resume", runtime::resume as *const u8);
    builder.symbol("vesper_handlers_alloc", runtime::alloc as *const u8);
    builder.symbol("vesper_handlers_fault", runtime::fault as *const u8);
    Ok(JITModule::new(builder))
}

/// Compile `entry` (no parameters, no operation-carrying effects, scalar
/// result) and everything it reaches; report the code size.
pub fn code_size(world: &World, entry: &str) -> Result<CodeSize, HandlerAbiError> {
    let mut l = lower::Lowering::new(world, jit_module()?)?;
    l.compile(entry)?;
    let sizes = std::mem::take(&mut l.sizes);
    // SAFETY: nothing compiled was run or finalized.
    unsafe { l.module.free_memory() };
    Ok(CodeSize { functions: sizes })
}

/// Compile and run `entry` under the handler ABI.
pub fn jit(world: &World, entry: &str) -> Result<Run, HandlerAbiError> {
    let f = world
        .functions
        .get(entry)
        .ok_or_else(|| HandlerAbiError::Invalid(format!("unknown entry {entry}")))?;
    let kind = Kind::of(&f.result);
    let mut l = lower::Lowering::new(world, jit_module()?)?;
    let id = l.compile(entry)?;
    let mut module = l.module;
    module
        .finalize_definitions()
        .map_err(|e| HandlerAbiError::Invalid(e.to_string()))?;
    let code = module.get_finalized_function(id);
    let rt = runtime::Rt::new(Rc::new(()))?;
    // SAFETY: `code` is the finalized entry, signature `(rt) -> word`; the
    // module stays alive until after the call and every fiber has ended
    // (each `handle` drives its fiber to exit or cancellation).
    let word = unsafe {
        let entry: extern "C" fn(*const runtime::Rt) -> u64 = std::mem::transmute(code);
        entry(&*rt)
    };
    let fibers = rt.fibers();
    let error = rt.error();
    drop(rt);
    // SAFETY: no generated code runs any more.
    unsafe { module.free_memory() };
    if let Some(e) = error {
        return Err(HandlerAbiError::Runtime(e));
    }
    use crate::runtime::Value;
    let value = match kind {
        Kind::I64 => Value::Int(word as i64),
        Kind::U64 => Value::UInt(word, 64),
        Kind::U32 => Value::UInt(word, 32),
        Kind::U8 => Value::UInt(word, 8),
        Kind::Bool => Value::Bool(word != 0),
        Kind::Unit => Value::Tuple(vec![]),
        k => {
            return Err(HandlerAbiError::Unsupported(format!(
                "an entry returning {k:?}"
            )));
        }
    };
    Ok(Run { value, fibers })
}
