//! Compiler Wasm target: checked Place IR to `wasm32-wasip2` components
//! (design §30 and §7).
//!
//! [`compile`] lowers the functions reachable from an entry to one core Wasm
//! module (through `wasm-encoder`) and wraps it as a component with
//! `wit-component`. The supported slice is the native batch subset and more:
//! scalars, `vesper.arith.checked/1` arithmetic with its aborts, tuples,
//! structs and enums with patterns, direct and recursive calls (a call in tail
//! position is a Wasm `return_call`), `inout` arguments and the lexical
//! effects of §4-§6, and closures and function values (a callable block of
//! table slot, evidence snapshot and captures, called through a trampoline):
//!
//! * `Fail` and every clause that drops its continuation abort to their
//!   handler through an explicitly propagated abort flag (portable);
//! * clauses whose every resumption is in tail position run at the
//!   operation site and return the resumed value (portable);
//! * any other one-shot clause captures the continuation with the WasmFX
//!   stack-switching proposal (`cont.new`/`suspend`/`resume`), which
//!   Wasmtime 50 executes on x86_64 Linux and macOS only. With
//!   [`EffectLowering::Portable`] such clauses are split at the resumption
//!   instead ([`HandlerMode::Cps`]), so the output runs on any Wasmtime.
//!
//! Everything else (strings, `f64` remainder,
//! labelled effect rows of ordinary functions, generic effect instances and
//! any IR operation this backend does not know) is a typed
//! [`WasmError::Unsupported`], never a miscompilation.
//!
//! The component exports the WIT interface `vesper:program/entry@0.1.0` with
//! `main`, typed by the entry's signature, and `wasi:cli/run@0.2.0` for
//! entries without parameters ([`WasmWorld::Command`]); or the root function
//! `run(scenario: u32, arg: u64) -> s64` of the Nexus plugin world
//! ([`WasmWorld::NexusPlugin`], `vesper_nexus::PLUGIN_WIT`). It imports
//! only the WASI interfaces of the host capabilities the entry declares
//! (see [`HostCapability`]).
use crate::{ast::Type, check::World};
use std::fmt;

mod analysis;
mod component;
mod host;
mod layout;
mod lower;
mod module;

pub use component::{ENTRY_INTERFACE, WASI_RUN};
pub use host::{HostCapability, WASI_MONOTONIC_CLOCK, WASI_STDOUT, WASI_STREAMS};

/// The target triple routed to this backend by `vesper build --target`.
pub const TARGET: &str = "wasm32-wasip2";
/// Version of the value representation and export conventions.
pub const ABI: &str = "vesper.wasm/1";

/// Which world the component implements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WasmWorld {
    /// `vesper:program/entry` plus `wasi:cli/run` (a WASI P2 command).
    #[default]
    Command,
    /// The Nexus `plugin` world: root export `run(u32, u64) -> s64`.
    NexusPlugin,
}

/// How handlers that are neither aborting nor tail-resumptive are lowered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EffectLowering {
    /// Capture continuations with WasmFX stack switching.
    #[default]
    StackSwitching,
    /// Split such clauses at their resumption ([`HandlerMode::Cps`]), so
    /// the output runs on every Wasmtime platform.
    Portable,
}

#[derive(Clone, Debug)]
pub struct WasmOptions {
    pub world: WasmWorld,
    pub effects: EffectLowering,
    /// Run the Place IR optimizer first (as `vesper build` does by default).
    pub optimize: bool,
    /// Maximum linear memory, in 64 KiB pages; allocation beyond it traps.
    pub max_memory_pages: u32,
    pub max_functions: usize,
    pub max_instructions: usize,
}
impl Default for WasmOptions {
    fn default() -> Self {
        Self {
            world: WasmWorld::Command,
            effects: EffectLowering::StackSwitching,
            optimize: true,
            max_memory_pages: 1024,
            max_functions: 4096,
            max_instructions: 2_000_000,
        }
    }
}

/// A construct this backend rejects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// A function used as a value that the backend did not plan (for
    /// example an external declaration).
    FunctionValue,
    String,
    FloatRemainder,
    Index,
    Projection,
    GenericOperation,
    GenericEffectInstance,
    LabelledEffectRow,
    InoutEffectArgument,
    FirstClassContinuation,
    NonTailResumptiveHandler,
    ExternalFunction,
    ExportType,
    /// An IR operation the backend does not know (including ones added after
    /// it, such as vector operations).
    Operation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WasmError {
    Unsupported {
        construct: Unsupported,
        function: String,
        detail: String,
    },
    /// The entry function cannot be exported.
    Entry(String),
    /// A configured size limit was exceeded.
    Limit(String),
    /// The checked program or IR violates an invariant the backend relies on.
    Invalid(String),
    /// Module or component encoding failed.
    Encode(String),
}
impl WasmError {
    pub(crate) fn unsupported(
        construct: Unsupported,
        function: &str,
        detail: impl Into<String>,
    ) -> Self {
        Self::Unsupported {
            construct,
            function: function.to_owned(),
            detail: detail.into(),
        }
    }
    /// Attach the function being compiled when the error has none.
    pub(crate) fn at(self, name: &str) -> Self {
        match self {
            Self::Unsupported {
                construct,
                function,
                detail,
            } if function.is_empty() => Self::Unsupported {
                construct,
                function: name.to_owned(),
                detail,
            },
            e => e,
        }
    }
}
impl fmt::Display for WasmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported {
                construct,
                function,
                detail,
            } => {
                write!(f, "{TARGET}: unsupported {construct:?}")?;
                if !function.is_empty() {
                    write!(f, " in '{function}'")?;
                }
                write!(f, ": {detail}")
            }
            Self::Entry(e) => write!(f, "{TARGET}: entry: {e}"),
            Self::Limit(e) => write!(f, "{TARGET}: limit: {e}"),
            Self::Invalid(e) => write!(f, "{TARGET}: invalid input: {e}"),
            Self::Encode(e) => write!(f, "{TARGET}: encoding: {e}"),
        }
    }
}
impl std::error::Error for WasmError {}

/// How one operation clause was lowered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerMode {
    /// The clause drops its continuation: the operation aborts to the handler.
    Abort,
    /// Every resumption is in tail position: the clause runs at the operation.
    Tail,
    /// The clause captures its continuation with WasmFX.
    StackSwitching,
    /// Portable one-shot lowering of a clause that resumes before
    /// returning: the clause is split at the resumption, its prefix runs at
    /// the operation and returns the resumed value to the body, and its
    /// suffix (the clause's continuation after `resume`) runs at the handler
    /// once the body has produced the handler's result.
    Cps,
}

/// One lowered handler, for reports and tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandlerLowering {
    pub effect: String,
    pub function: String,
    /// `(operation, mode)` for each operation clause.
    pub clauses: Vec<(String, HandlerMode)>,
}

/// The compiled program.
#[derive(Clone, Debug)]
pub struct WasmArtifact {
    /// The core module (with embedded component-type metadata removed).
    pub core: Vec<u8>,
    /// The `wasm32-wasip2` component.
    pub component: Vec<u8>,
    /// The WIT package the component implements.
    pub wit: String,
    pub world: WasmWorld,
    /// The component export carrying the entry: `main` in
    /// [`ENTRY_INTERFACE`], or the root `run` of the plugin world.
    pub export: String,
    pub params: Vec<Type>,
    pub result: Type,
    /// Whether the module uses the stack-switching proposal.
    pub stack_switching: bool,
    /// Host capabilities the entry declares, imported from WASI.
    pub imports: Vec<HostCapability>,
    pub handlers: Vec<HandlerLowering>,
    pub functions: usize,
}

/// Compile `entry` and everything it reaches to a component.
pub fn compile(
    world: &World,
    entry: &str,
    options: &WasmOptions,
) -> Result<WasmArtifact, WasmError> {
    if options.max_memory_pages == 0 || options.max_memory_pages > 65536 {
        return Err(WasmError::Limit(
            "max_memory_pages must be in 1..=65536".into(),
        ));
    }
    let f = world
        .functions
        .get(entry)
        .ok_or_else(|| WasmError::Entry(format!("unknown entry function '{entry}'")))?;
    let mut program = crate::ir::lower_world(world);
    if options.optimize {
        crate::ir_opt::optimize(world, &mut program).map_err(WasmError::Invalid)?;
    }
    let stack_switching = options.effects == EffectLowering::StackSwitching;
    let plan = analysis::plan(
        world,
        &program,
        entry,
        stack_switching,
        options.max_functions,
    )?;
    let mut l = lower::Lowerer::new(
        world,
        &program,
        &plan,
        stack_switching,
        options.max_instructions,
    )?;
    l.declare_all()?;
    let boundary = component::exports(&mut l, entry, options.world)?;
    l.define_all()?;
    let uses_fx = l.fx.is_some();
    let functions = l.b.function_count();
    let core =
        l.b.finish(options.max_memory_pages)
            .map_err(WasmError::Invalid)?;
    let component = component::encode(core.clone(), &boundary.wit, options.world)?;
    Ok(WasmArtifact {
        core,
        component,
        wit: boundary.wit,
        world: options.world,
        export: boundary.export,
        params: f.params.iter().map(|p| p.ty.clone()).collect(),
        result: crate::numeric::default_type(&f.result),
        stack_switching: uses_fx,
        imports: plan.host.clone(),
        handlers: plan
            .handlers
            .iter()
            .map(|h| HandlerLowering {
                effect: h.effect.name.clone(),
                function: h.site.0.clone(),
                clauses: h
                    .ops
                    .iter()
                    .map(|c| (world.effects[&h.effect.name].ops[c.op].name.clone(), c.mode))
                    .collect(),
            })
            .collect(),
        functions,
    })
}
