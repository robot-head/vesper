//! The native backend's codegen swap point (self-hosting SH4 lane 1, #1275).
//!
//! Native lowering is one `Codegen` (src/native.rs and its family files)
//! that emits one function at a time into a Cranelift module. SH4 ports it
//! to Vesper one family at a time: the Rust `Codegen` lends its live
//! `FunctionBuilder` and module to a Vesper hook for one request and takes
//! them back. Each family has one optional hook here, called where the
//! Rust code dispatches that family; the hybrid Stage 2 build installs the
//! Vesper ones (crates/vesper-hybrid, module `codegen`), through the shim's
//! lent handles (`vesper_cranelift_sys::lend`).
//!
//! **Nothing in the default compiler installs a hook**, so every call here
//! returns `None` after one relaxed atomic load and the default compiler's
//! objects are byte-identical to what they were. Hooks are called on the
//! native path only: the verified backend (src/verified_codegen,
//! crates/vesper-codegen-verified) never reaches this module (the Stage 1
//! guard).
//!
//! # The request protocol
//!
//! A hook gets a [`Lent`]: the builder and module, the request (`op` and
//! argument words) and where it was made ([`Site`]), plus the `World`, the
//! `Program` being emitted and, for requests about a type, the type. Words
//! are Cranelift entity numbers (values, blocks, stack slots), immediates
//! or type codes ([`ty_code`], the shim's `lane | log2(lanes) << 8`). The
//! hook returns:
//!
//! - `Ok(true)`: handled; it emitted the request's code and pushed its
//!   result words (in the order the op documents) to `results`;
//! - `Ok(false)`: declined (nothing emitted); the Rust code emits it;
//! - `Err(e)`: the compile fails with `e`.
//!
//! A hook must emit exactly what the Rust code would (the differential of
//! crates/vesper-codegen-swap-test compares per-function CLIF text, object
//! bytes and JIT runs). The Rust bookkeeping around an emission (value
//! infos, pointer parents) stays in Rust and still runs.
//!
//! # Families
//!
//! | Hook | Dispatch site | Requests |
//! |---|---|---|
//! | `scalar` | `Codegen::ir_graph_at`, each instruction | [`INSTRUCTION`] for constants, unit, reads, writes, binds, unary and binary operators, `try`, loop and exit |
//! | `call` | the same, and `Codegen::call_function_into` | [`INSTRUCTION`] for `Op::Call` and `Op::Indirect`; the [`call`] ops |
//! | `aggregate` | the same, and the layout helpers | [`INSTRUCTION`] for tuples, data, projections and indexing; the [`aggregate`] ops |
//! | `drops` | `Codegen::drop_box_value` | [`drops::DROP_BOX`] |
//! | `seq` | `Codegen::ir_graph_at`, and the sequence helpers | [`INSTRUCTION`] for `Op::Seq`; the [`seq`] ops |
//! | `control` | the same | [`INSTRUCTION`] for closures, handlers and calls of effect operations |
//! | `numeric` | the same | [`INSTRUCTION`] for `Op::Vector` and `Op::Tiled` |
//! | `drops` (fip) | the same | [`INSTRUCTION`] for `Op::Fip` |
//! | `ffi` | the same | [`INSTRUCTION`] for a call of a foreign (`.vsys`) effect operation |
//! | `export` | `native_export::glue::define`, after the sequence runtime and `vesper_box_alloc` | the rest of the v2 glue over a lent module ([`ExportHook`]) |
//! | `plan` | `native_export::plan_with` | the whole library interface ([`PlanHook`]) |
//! | `interface_json` | `native_export::interface_json` | the whole interface file ([`InterfaceJsonHook`]) |
//! | `freestanding` | `native_freestanding::build_image` | whole image ([`FreestandingHook`]) |
//! | `wasm` | `wasm_backend::compile` | whole component ([`WasmHook`]) |
//! | `component` | `wasm_backend::component::encode` | the component around a core module and its WIT ([`ComponentHook`]) |
//! | `ebpf` | `ebpf_backend::compile_world` | whole program ([`EbpfHook`]) |
//! | `verifiers` | `native_verifiers::verify_emitted` | whole report ([`VerifierHook`]) |
//! | `body` | `native::define_instances_observed_with_trace`, after the entry block | a whole function body ([`body::FUNCTION`]) |
//!
//! An [`INSTRUCTION`] request's arguments are the Cranelift value of each
//! operand of the IR instruction, in `ir_inline::operands` order
//! ([`NO_VALUE`] for an operand with no value yet), and its one result is
//! the instruction's value. The [`Site`] names the instruction, so a hook
//! reads the instruction itself from the `Program`.
//!
//! # Walk state and re-entry
//!
//! An [`INSTRUCTION`] request also lends the graph walk's own state for
//! the length of the request ([`walk`], SH5 lane 1): queries of the
//! instruction's facts and the walk's maps (variables, home slots,
//! register locals, split words, the walk's plans), effects that keep the
//! Rust bookkeeping of what the hook emitted (`use_var`, `def_var`,
//! `store`, `jump`, `ir_temp`), and continuation re-entry: a hook asks
//! Rust to emit the instruction or its place, declines, and is asked again
//! with the value kept. A hook that never calls them is unaffected.
use crate::ast::Type;
use crate::check::World;
use crate::ir::{Op, Program};
use cranelift_codegen::ir;
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;
use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub mod walk;

/// An argument word for an operand that has no Cranelift value.
pub const NO_VALUE: u64 = u64::MAX;
/// The request of a family hook at the IR graph walk: emit one Place IR
/// instruction.
pub const INSTRUCTION: u32 = 0;

/// Where a request was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Site<'a> {
    /// The lowered function (its `Program` key) being emitted; empty when
    /// the request is not inside a Place IR graph, or the walk emits a
    /// rewritten copy of the graph (small enums as words, #1330) whose
    /// indices are not the program's.
    pub function: &'a str,
    /// The block and instruction index in that function's graph
    /// (`u32::MAX` when the request is not one instruction).
    pub block: u32,
    pub inst: u32,
}

impl Site<'_> {
    /// A site outside any Place IR instruction.
    pub const NONE: Site<'static> = Site {
        function: "",
        block: u32::MAX,
        inst: u32::MAX,
    };
}

/// What the Rust `Codegen` lends a family hook for one request.
pub struct Lent<'a, 'f> {
    /// The builder of the function being emitted, positioned where the
    /// Rust code would emit.
    pub builder: &'a mut FunctionBuilder<'f>,
    /// The module the function is emitted into (an object or JIT module).
    pub module: &'a mut (dyn Module + 'a),
    pub world: &'a World,
    /// The program being emitted (after the native IR pipeline).
    pub program: &'a Program,
    pub site: Site<'a>,
    /// The request: [`INSTRUCTION`] or one of the family's ops.
    pub op: u32,
    pub args: &'a [u64],
    /// The type the request is about, when it is about one.
    pub ty: Option<&'a Type>,
    /// The result words; push them in the order the op documents.
    pub results: &'a mut Vec<u64>,
    /// Whether `module` is a JIT module (else an object module). Hooks
    /// lend it with `vesper_cranelift_sys::lend_as` so the Vesper side reads
    /// it back with `vcl_module_kind` and chooses linkage as Rust does
    /// (`Preemptible` for JIT definitions, `Local` or `Export` in objects).
    pub jit: bool,
    /// Whether this is an optimized build (the Place IR optimizer ran and
    /// the native backend uses its optimized lowerings, such as register
    /// locals and the internal calling conventions). A hook must emit what
    /// the Rust code emits for the same setting.
    pub optimize: bool,
}

/// A family hook (see the module documentation for the protocol).
pub type Hook = for<'a, 'f> fn(&mut Lent<'a, 'f>) -> Result<bool, String>;

/// The export glue hook, called by `native_export::glue::define` over the
/// lent module (`vesper_cranelift_sys::lend_module`) once the sequence
/// runtime and `vesper_box_alloc` are defined. The hook finds those by name
/// and must not redefine them. `None` declines.
pub type ExportHook = fn(
    &mut dyn Module,
    &World,
    &crate::native::export::LibraryInterface,
) -> Option<Result<(), String>>;
/// The export interface planner hook: `native_export::plan_with`'s
/// signature (`native_export::plan_native` is the Rust planner); `None`
/// declines.
pub type PlanHook = fn(
    &World,
    &std::collections::BTreeSet<String>,
    &str,
    &crate::native::export::Directives,
) -> Option<
    Result<crate::native::export::LibraryInterface, Vec<crate::native::export::ExportError>>,
>;
/// The interface file hook: `native_export::interface_json`'s text (the
/// pretty JSON and its trailing newline); `None` declines.
pub type InterfaceJsonHook = fn(&crate::native::export::LibraryInterface) -> Option<String>;
/// The whole-image hook: `native::freestanding::build_image`'s signature;
/// `None` declines.
pub type FreestandingHook = fn(
    &World,
    &str,
    &crate::native::freestanding::ImageOptions,
) -> Option<
    Result<
        crate::native::freestanding::FreestandingImage,
        crate::native::freestanding::FreestandingError,
    >,
>;
/// The wasm backend hook: `wasm_backend::compile`'s signature.
pub type WasmHook =
    fn(
        &World,
        &str,
        &crate::wasm_backend::WasmOptions,
    ) -> Option<Result<crate::wasm_backend::WasmArtifact, crate::wasm_backend::WasmError>>;
/// The component wrapping hook (SH5 lane 11, #1334): the component
/// wit-component would write for a core module, its WIT world and the world
/// kind, or `None` to leave it to wit-component.
pub type ComponentHook = fn(&[u8], &str, crate::wasm_backend::WasmWorld) -> Option<Vec<u8>>;
/// The eBPF backend hook: `ebpf_backend::compile_world`'s signature.
pub type EbpfHook =
    fn(
        &World,
        Option<&str>,
        &crate::ebpf_backend::Options,
    ) -> Option<Result<crate::ebpf_backend::Compiled, crate::ebpf_backend::EbpfBackendError>>;
/// The native verifiers hook: `native_verifiers::verify_emitted`'s
/// signature.
pub type VerifierHook = fn(
    &World,
    &[crate::native::EmittedFunction],
    Option<&str>,
    &std::collections::BTreeSet<crate::native_verifiers::NativeVerifier>,
    &crate::native_verifiers::NativeVerifierPolicy,
)
    -> Option<Result<crate::native_verifiers::NativeVerifierReport, String>>;
/// The native walk's register-local plan (`native::register_locals`),
/// called once per lowered graph on the optimize path (`#1509` part 2):
/// `None` leaves the decision to Rust.
pub type RegisterLocalsHook = fn(
    &World,
    &crate::ir::FunctionIr,
    &std::collections::BTreeMap<String, Type>,
    bool,
) -> Option<std::collections::BTreeSet<String>>;

/// The native walk's entry plans (`#1509` parts 3b, 4a and 4b): the
/// instances an entry compile defines (`instances_for_entry`), each one's
/// entry block (`native::frame_plan`) and internal convention
/// (`NativeAbi::plan`). Called once per entry compile with the world's
/// first lowering, the entry, whether the compile optimizes and whether it
/// is a Beacon trace host (`trace_ok`, part 5 of #1509). An instance the frames leave out keeps the Rust frame plan;
/// the conventions are used only when the frames cover exactly the
/// instances the compile defines. `None`, or no instances, keeps every Rust
/// plan.
pub type EntryPlansHook =
    fn(&World, &crate::ir::Program, &str, bool, bool) -> Option<crate::native::EntryPlans>;

/// The block plan of one graph (`native::block_plan`, #2094): called with
/// each block's successors, once per graph the walk lowers. `None`, or a
/// plan that does not order every block, keeps the Rust plan.
pub type BlockPlanHook = fn(&[Vec<usize>]) -> Option<crate::native::BlockPlan>;

/// [`EntryPlansHook`] for a library object (`native_export::object`, part
/// 5 of #1509): the instances every export root reaches, their entry blocks
/// and internal conventions, every export a boundary. Called with the
/// world's first lowering, the export names in order and whether the build
/// optimizes; the same fallbacks apply.
pub type ExportPlansHook =
    fn(&World, &crate::ir::Program, &[&str], bool) -> Option<crate::native::EntryPlans>;

/// The SH4 families. Each port lane installs the hooks of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    /// Lane 6: the IR graph walk, expressions, operators, control flow.
    Scalar,
    /// Lane 7: instances, the native ABI and calls.
    Call,
    /// Lane 8: aggregates, arrays, patterns, equality.
    Aggregate,
    /// Lane 8: drop glue and fip.
    Drops,
    /// Lane 9: sequences, bytes and strings.
    Seq,
    /// Lane 10: closures, handlers, effects, `dyn` and sessions.
    Control,
    /// Lane 11: vectors, tiles, floats, wide integers, regions, atomics.
    Numeric,
    /// Lane 13: foreign (`.vsys`) calls.
    Ffi,
}

impl Family {
    pub const ALL: [Family; 8] = [
        Family::Scalar,
        Family::Call,
        Family::Aggregate,
        Family::Drops,
        Family::Seq,
        Family::Control,
        Family::Numeric,
        Family::Ffi,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Family::Scalar => "scalar",
            Family::Call => "call",
            Family::Aggregate => "aggregate",
            Family::Drops => "drops",
            Family::Seq => "seq",
            Family::Control => "control",
            Family::Numeric => "numeric",
            Family::Ffi => "ffi",
        }
    }
    /// The family that emits Place IR operation `op` at the graph walk.
    pub fn of(world: &World, op: &Op) -> Family {
        match op {
            Op::Call { callee, .. } => match callee.split_once("::") {
                Some((effect, _)) if crate::ffi_lowering::is_foreign_effect(effect) => Family::Ffi,
                Some((effect, _)) if world.effects.contains_key(effect) => Family::Control,
                _ => Family::Call,
            },
            Op::Indirect { .. } => Family::Call,
            Op::Tuple(_) | Op::Data(..) | Op::Project(..) | Op::Index(..) => Family::Aggregate,
            Op::Fip { .. } => Family::Drops,
            Op::Seq { .. } => Family::Seq,
            Op::Vector { .. } | Op::Tiled { .. } => Family::Numeric,
            Op::Closure { .. } | Op::Handle { .. } => Family::Control,
            _ => Family::Scalar,
        }
    }
}

/// Every hook; `None` runs the Rust code.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodegenHooks {
    pub scalar: Option<Hook>,
    pub call: Option<Hook>,
    pub aggregate: Option<Hook>,
    pub drops: Option<Hook>,
    pub seq: Option<Hook>,
    pub control: Option<Hook>,
    pub numeric: Option<Hook>,
    pub ffi: Option<Hook>,
    pub export: Option<ExportHook>,
    pub plan: Option<PlanHook>,
    pub interface_json: Option<InterfaceJsonHook>,
    pub freestanding: Option<FreestandingHook>,
    pub wasm: Option<WasmHook>,
    pub component: Option<ComponentHook>,
    pub ebpf: Option<EbpfHook>,
    pub verifiers: Option<VerifierHook>,
    pub register_locals: Option<RegisterLocalsHook>,
    pub entry_plans: Option<EntryPlansHook>,
    pub export_plans: Option<ExportPlansHook>,
    pub block_plan: Option<BlockPlanHook>,
    /// The native walk's whole-function hook (`#2094`, part 6 of `#1509`):
    /// asked for each function body before the Rust walk ([`body`]).
    pub body: Option<Hook>,
}

impl CodegenHooks {
    /// The hook of `family`.
    pub fn family(&self, family: Family) -> Option<Hook> {
        match family {
            Family::Scalar => self.scalar,
            Family::Call => self.call,
            Family::Aggregate => self.aggregate,
            Family::Drops => self.drops,
            Family::Seq => self.seq,
            Family::Control => self.control,
            Family::Numeric => self.numeric,
            Family::Ffi => self.ffi,
        }
    }
    fn any(&self) -> bool {
        Family::ALL.iter().any(|f| self.family(*f).is_some())
            || self.export.is_some()
            || self.plan.is_some()
            || self.interface_json.is_some()
            || self.freestanding.is_some()
            || self.wasm.is_some()
            || self.component.is_some()
            || self.ebpf.is_some()
            || self.verifiers.is_some()
            || self.register_locals.is_some()
            || self.entry_plans.is_some()
            || self.export_plans.is_some()
            || self.block_plan.is_some()
            || self.body.is_some()
    }
}

static INSTALLED: OnceLock<CodegenHooks> = OnceLock::new();
/// Whether any hook may be installed (process-wide or scoped): the default
/// compiler's fast path is this one load.
static ACTIVE: AtomicBool = AtomicBool::new(false);
thread_local! {
    static SCOPED: Cell<Option<CodegenHooks>> = const { Cell::new(None) };
    /// Whether the module being emitted into is a JIT module
    /// ([`module_kind_scope`]).
    static JIT: Cell<bool> = const { Cell::new(false) };
    /// Whether the build being emitted is optimized ([`module_kind_scope`]).
    static OPTIMIZE: Cell<bool> = const { Cell::new(false) };
}

/// Restores the previous module kind and optimize setting when dropped.
pub struct ModuleKindScope(bool, bool);
impl Drop for ModuleKindScope {
    fn drop(&mut self) {
        JIT.with(|j| j.set(self.0));
        OPTIMIZE.with(|o| o.set(self.1));
    }
}

/// Records, for the requests of one `define_instances` call, whether its
/// module is a JIT module (`Lent::jit`) and whether the build is optimized
/// (`Lent::optimize`). The native backend is generic over the module, which
/// a hook only sees as `dyn Module`.
pub fn module_kind_scope(jit: bool, optimize: bool) -> ModuleKindScope {
    ModuleKindScope(
        JIT.with(|j| j.replace(jit)),
        OPTIMIZE.with(|o| o.replace(optimize)),
    )
}

/// Installs `hooks` for the rest of the process (the hybrid build, from its
/// static constructor). Only the first call takes effect; a later one
/// returns its table back.
// Called once per process, so returning the whole table by value is fine.
#[allow(clippy::result_large_err)]
pub fn install(hooks: CodegenHooks) -> Result<(), CodegenHooks> {
    INSTALLED.set(hooks)?;
    if hooks.any() {
        ACTIVE.store(true, Ordering::Release);
    }
    Ok(())
}

/// Runs `f` with `hooks` installed on this thread only, over any
/// process-wide table (the differential harness compares a compile with
/// and without a hook in one process).
pub fn with_hooks<R>(hooks: CodegenHooks, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<CodegenHooks>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SCOPED.with(|s| s.set(self.0));
        }
    }
    if hooks.any() {
        ACTIVE.store(true, Ordering::Release);
    }
    let _restore = Restore(SCOPED.with(|s| s.replace(Some(hooks))));
    f()
}

/// The hooks in effect on this thread (all `None` in the default compiler).
pub fn installed() -> CodegenHooks {
    if !ACTIVE.load(Ordering::Acquire) {
        return CodegenHooks::default();
    }
    SCOPED
        .with(Cell::get)
        .or_else(|| INSTALLED.get().copied())
        .unwrap_or_default()
}

/// The hook of `family`, if one is in effect.
pub fn hook(family: Family) -> Option<Hook> {
    if !ACTIVE.load(Ordering::Acquire) {
        return None;
    }
    installed().family(family)
}

/// The [`body::FUNCTION`] request of `function`: whether the `body` hook
/// emitted its body (`false` when no hook is in effect or it declined).
pub fn body_request(
    builder: &mut FunctionBuilder<'_>,
    module: &mut dyn Module,
    world: &World,
    program: &Program,
    function: &str,
    args: &[u64],
) -> Result<bool, String> {
    if !ACTIVE.load(Ordering::Acquire) {
        return Ok(false);
    }
    let Some(hook) = installed().body else {
        return Ok(false);
    };
    let mut results = Vec::new();
    hook(&mut Lent {
        builder,
        module,
        world,
        program,
        site: Site {
            function,
            ..Site::NONE
        },
        op: body::FUNCTION,
        args,
        ty: None,
        results: &mut results,
        jit: JIT.with(Cell::get),
        optimize: OPTIMIZE.with(Cell::get),
    })
}

/// A family request: `None` when no hook is in effect or it declined (the
/// Rust code emits), else the hook's result words.
#[allow(clippy::too_many_arguments)]
pub fn request(
    family: Family,
    builder: &mut FunctionBuilder<'_>,
    module: &mut dyn Module,
    world: &World,
    program: &Program,
    site: Site<'_>,
    op: u32,
    args: &[u64],
    ty: Option<&Type>,
) -> Result<Option<Vec<u64>>, String> {
    let Some(hook) = hook(family) else {
        return Ok(None);
    };
    let mut results = Vec::new();
    let handled = hook(&mut Lent {
        builder,
        module,
        world,
        program,
        site,
        op,
        args,
        ty,
        results: &mut results,
        jit: JIT.with(Cell::get),
        optimize: OPTIMIZE.with(Cell::get),
    })?;
    Ok(handled.then_some(results))
}

/// The one result of a request that answers with a Cranelift value.
pub fn value_of(results: &[u64]) -> Result<ir::Value, String> {
    match results {
        [v] => u32::try_from(*v)
            .map(ir::Value::from_u32)
            .map_err(|_| format!("codegen hook answered {v}, which is not a value")),
        _ => Err(format!(
            "codegen hook answered {} words for a one-value request",
            results.len()
        )),
    }
}

thread_local! {
    /// The first hook error of a request whose call site cannot fail
    /// (the layout helpers): the next fallible request reports it.
    static ERROR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Records a hook error from an infallible call site.
pub fn record(e: String) {
    ERROR.with(|r| {
        r.borrow_mut().get_or_insert(e);
    });
}

/// The recorded hook error, taken.
pub fn take_error() -> Result<(), String> {
    match ERROR.with(|r| r.borrow_mut().take()) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// [`request`] from an infallible call site outside the graph walk: an
/// error is recorded ([`take_error`]) and the Rust code emits instead.
pub fn request_or_record(
    family: Family,
    builder: &mut FunctionBuilder<'_>,
    module: &mut dyn Module,
    world: &World,
    program: &Program,
    op: u32,
    args: &[u64],
) -> Option<Vec<u64>> {
    hook(family)?;
    match request(
        family,
        builder,
        module,
        world,
        program,
        Site::NONE,
        op,
        args,
        None,
    ) {
        Ok(r) => r,
        Err(e) => {
            record(e);
            None
        }
    }
}

/// [`value_of`], recording a malformed answer.
pub fn value_or_record(results: &[u64]) -> Option<ir::Value> {
    value_of(results).map_err(record).ok()
}

/// Packs `bytes` as its byte length followed by little-endian 8-byte words
/// (zero-padded in the tail word) into `out`.
pub fn pack_bytes_words(bytes: &[u8], out: &mut Vec<u64>) {
    out.push(bytes.len() as u64);
    for chunk in bytes.chunks(8) {
        let mut word = [0u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        out.push(u64::from_le_bytes(word));
    }
}

/// Dispatches a `control` request from a helper that holds a builder and
/// module without a [`crate::native::Codegen`] context (object/boxed drops).
pub(crate) fn request_control_builder(
    builder: &mut FunctionBuilder<'_>,
    module: &mut dyn Module,
    op: u32,
    args: &[u64],
) -> Result<bool, String> {
    if hook(Family::Control).is_none() {
        return Ok(false);
    }
    static EMPTY_WORLD: OnceLock<World> = OnceLock::new();
    static EMPTY_PROGRAM: OnceLock<Program> = OnceLock::new();
    let world = EMPTY_WORLD.get_or_init(World::default);
    let program = EMPTY_PROGRAM.get_or_init(Program::default);
    Ok(request(
        Family::Control,
        builder,
        module,
        world,
        program,
        Site::NONE,
        op,
        args,
        None,
    )?
    .is_some())
}

/// Dispatches a `numeric` request from a module-level runtime helper that
/// holds only a module without a [`crate::native::Codegen`] context.
pub(crate) fn request_numeric_module(
    module: &mut dyn Module,
    op: u32,
    args: &[u64],
) -> Result<Option<Vec<u64>>, String> {
    if hook(Family::Numeric).is_none() {
        return Ok(None);
    }
    static EMPTY_WORLD: OnceLock<World> = OnceLock::new();
    static EMPTY_PROGRAM: OnceLock<Program> = OnceLock::new();
    let world = EMPTY_WORLD.get_or_init(World::default);
    let program = EMPTY_PROGRAM.get_or_init(Program::default);
    let mut func = cranelift_codegen::ir::Function::new();
    let mut fb_ctx = cranelift_frontend::FunctionBuilderContext::new();
    let mut builder = cranelift_frontend::FunctionBuilder::new(&mut func, &mut fb_ctx);
    request(
        Family::Numeric,
        &mut builder,
        module,
        world,
        program,
        Site::NONE,
        op,
        args,
        None,
    )
}

/// The graph walk's request for one Place IR instruction: `inst` (block
/// `block`, index `index` of lowered function `function`), whose operands
/// have the Cranelift values in `values`, and whose type is `ty`. `None`
/// when no hook of the instruction's family is in effect or it declined.
#[allow(clippy::too_many_arguments)]
pub fn instruction(
    builder: &mut FunctionBuilder<'_>,
    module: &mut dyn Module,
    world: &World,
    program: &Program,
    (function, block, index, inst): (&str, usize, usize, &crate::ir::Instruction),
    values: &std::collections::BTreeMap<crate::ir::ValueId, ir::Value>,
    ty: &Type,
) -> Result<Option<ir::Value>, String> {
    if !ACTIVE.load(Ordering::Acquire) {
        return Ok(None);
    }
    take_error()?;
    let family = Family::of(world, &inst.op);
    if hook(family).is_none() {
        return Ok(None);
    }
    let args: Vec<u64> = crate::ir_inline::operands(&inst.op)
        .iter()
        .map(|id| values.get(id).map_or(NO_VALUE, |v| word(*v)))
        .collect();
    let site = Site {
        function,
        block: block as u32,
        inst: index as u32,
    };
    match request(
        family,
        builder,
        module,
        world,
        program,
        site,
        INSTRUCTION,
        &args,
        Some(ty),
    )? {
        Some(results) => value_of(&results).map(Some),
        None => Ok(None),
    }
}

/// The argument word of a Cranelift value.
pub fn word(v: ir::Value) -> u64 {
    u64::from(v.as_u32())
}

/// The shim's code of a Cranelift type: `lane | log2(lanes) << 8` with the
/// lanes I8 1, I16 2, I32 3, I64 4, I128 5, F32 6, F64 7
/// (`vesper_cranelift_sys::codes::ty_code`; a test keeps the two equal).
pub fn ty_code(t: ir::Type) -> u64 {
    use cranelift_codegen::ir::types;
    let lane = match t.lane_type() {
        types::I8 => 1,
        types::I16 => 2,
        types::I32 => 3,
        types::I64 => 4,
        types::I128 => 5,
        types::F32 => 6,
        types::F64 => 7,
        _ => 0,
    };
    lane | u64::from(t.log2_lane_count()) << 8
}

/// The `aggregate` family's layout-helper requests (the lane 1 pilot: the
/// census lowering family). Each is a primitive the Rust `Codegen` uses to
/// lay out and test aggregates; the argument and result words are listed
/// per op. Sizes and offsets are bytes; flags are `MemFlags::new()`.
pub mod aggregate {
    /// `alloc`: a zeroed explicit stack slot of `max(size, 8)` bytes,
    /// aligned to 8. Args `[size]`; result `[address]`.
    pub const ALLOC: u32 = 1;
    /// `alloc_uninit`: the same slot, not zeroed. Args `[size]`; result
    /// `[address]`.
    pub const ALLOC_UNINIT: u32 = 2;
    /// `copy_bytes`: copy `size` bytes word by word. Args
    /// `[from, to, size]`; no result.
    pub const COPY_BYTES: u32 = 3;
    /// `load` of a scalar field. Args `[address, type code]`; result
    /// `[value]`.
    pub const LOAD: u32 = 4;
    /// `store` of a scalar field. Args `[address, value]`; no result.
    pub const STORE: u32 = 5;
    /// `offset`: a field's address. Args `[address, offset]` (offset as an
    /// `i32`'s two's complement); result `[address]`.
    pub const OFFSET: u32 = 6;
    /// `data_into`'s tag store. Args `[address, tag]`; no result.
    pub const TAG: u32 = 7;
    /// `data_into`'s zero stores (optimize path). Args
    /// `[address, offset...]`; no result.
    pub const ZERO_WORDS: u32 = 8;
    /// `pattern_condition`'s constant true (`iconst.i8 1`). No args;
    /// result `[value]`.
    pub const TRUE: u32 = 9;
    /// `pattern_condition`'s enum tag test. Args `[address, tag]`; result
    /// `[condition]`.
    pub const TAG_TEST: u32 = 10;
    /// `pattern_condition`'s field diamond, opened: the `ok`/`fail`/`done`
    /// blocks, `brif`, the `fail` arm's zero. Args `[condition]`; result
    /// `[done block]`, emitting into `ok`.
    pub const OPEN: u32 = 11;
    /// The diamond, closed: jump to `done` with the result, switch to it.
    /// Args `[done block, result]`; result `[done's parameter]`.
    pub const CLOSE: u32 = 12;
    /// `pattern_condition`'s conjunction (`band`). Args `[a, b]`; result
    /// `[value]`.
    pub const AND: u32 = 13;
}

/// The `drops` family's requests.
pub mod drops {
    /// Drop the `Box` contents a value owns (`Codegen::drop_box_value`).
    /// Args `[value]`, with the value's type; no result.
    pub const DROP_BOX: u32 = 1;
}

/// The `call` family's requests (SH4 lane 7, #1281), made by
/// `Codegen::call_function_into` once it has decided the callee, its ABI
/// and every argument. Each answers the call's result values in order
/// (`inst_results`); a call whose signature has no result is not requested.
pub mod call {
    /// `call` of a function already imported into the function
    /// (`declare_func_in_func`). Args `[func_ref, value...]`.
    pub const CALL: u32 = 1;
    /// `call_indirect` of a vtable slot's target under an imported
    /// signature. Args `[sig_ref, target, value...]`.
    pub const CALL_INDIRECT: u32 = 2;
}

/// The `control` family's leaf requests (SH4 lane 10, #1284): the
/// builder sequences of closure tags, `invoke_tag`'s dispatch chain and
/// the inlined handlers' resumption points. The `Codegen` keeps the state
/// they read and write (the closure table, target sets, variables and the
/// `HandlerControl`), so a hook only emits. Blocks and stack slots pass as
/// their entity numbers.
pub mod control {
    /// A closure or function tag: `iconst.i64 id`. Args `[id]`; result
    /// `[value]`.
    pub const TAG: u32 = 1;
    /// A captured variable's explicit stack slot of `size` bytes, aligned
    /// to 8, and its address. Args `[size]`; result `[slot, address]`.
    pub const CAPTURE_SLOT: u32 = 2;
    /// `qualify_tag`'s re-numbering of one descriptor:
    /// `select(icmp_imm eq tag old, iconst new, acc)`. Args
    /// `[tag, old, new, acc]`; result `[value]`.
    pub const QUALIFY: u32 = 3;
    /// One candidate of `invoke_tag`: the `yes` and `no` blocks, then a
    /// jump to `yes` when `single` is 1, else `brif (icmp_imm eq tag
    /// index), yes, no`; emission continues in `yes`. Args
    /// `[tag, index, single]`; result `[yes, no]`.
    pub const DISPATCH: u32 = 4;
    /// The end of a dispatch chain: `trap user(code)` (3 for a closure
    /// call, 4 for a handler's return selector). Args `[code]`; no result.
    pub const TRAP: u32 = 5;
    /// A non-tail resumption's save: an 8-byte slot holding `selector`,
    /// the `returned` block with one parameter of `ty_code`, and
    /// `iconst.i64 index`. Args `[selector, ty_code, index]`; result
    /// `[slot, returned, selected]`.
    pub const RESUME_SAVE: u32 = 6;
    /// The resumption's restore in `returned`: `stack_load.i64 slot`.
    /// Args `[slot]`; result `[value]`.
    pub const RESUME_RESTORE: u32 = 7;
    /// One test of a handler's dispatch chain: a `next` block, then
    /// `brif (icmp_imm eq selector index), target(output), next`;
    /// emission continues in `next`. Args
    /// `[selector, index, target, output]`; no result.
    pub const RETURN_TO: u32 = 8;
    /// A join block with one parameter of `ty_code`. Args `[ty_code]`;
    /// result `[block]`.
    pub const JOIN_BLOCK: u32 = 9;
    /// Switch to `block` and read its parameter `index`. Args
    /// `[block, index]`; result `[value]`.
    pub const SWITCH_PARAM: u32 = 10;
    /// `jump block(arg0, ...)`. Args `[block, arg0...]`; no result.
    pub const JUMP_ARGS: u32 = 11;
    /// A tail resumption: `jump entry(arg0, ...)`, then a new `unreachable`
    /// block with one parameter of `ty_code`, switched to it. Args
    /// `[entry, ty_code, arg0...]`; result `[value]`.
    pub const TAIL_RESUME: u32 = 12;
    /// `stack_addr pointer_type, slot, offset`. Args `[slot, offset]`;
    /// result `[ptr]`.
    pub const STACK_ADDR: u32 = 13;
    /// Initial `Handle` blocks and selector variable: `exit` and `dispatch`
    /// blocks each with one parameter of `ty_code`, plus `selector` variable
    /// initialized to `zero = iconst.i64 0`. Args `[ty_code]`; result
    /// `[exit, dispatch, selector, zero]`.
    pub const HANDLE_SETUP: u32 = 14;
    /// A handler clause resumption `entry` block with one parameter per
    /// `ty_code`. Args `[ty_code0, ...]`; result `[entry]`.
    pub const HANDLER_ENTRY: u32 = 15;
    /// `reraise_status` guard: branch `icmp_imm ne tag, 0` to `bad` else
    /// `good`, and switch to `bad`. Args `[tag]`; result `[bad, good]`.
    pub const RERAISE_GUARD: u32 = 16;
    /// One `reraise_each` instance branch: `icmp_imm eq tag, index_plus_1`
    /// to `bad` else `good`, switch to `bad`, `iadd_imm record, 8`. Args
    /// `[tag, index_plus_1, record]`; result `[bad, good, payload]`.
    pub const RERAISE_EACH: u32 = 17;
    /// `free_sink_block` branch: `icmp_imm ne block, 0` to `free` else
    /// `done`, switch to `free`. Args `[block]`; result `[free, done]`.
    pub const FREE_SINK_BRANCH: u32 = 18;
    /// `fail_exit` tag and payload preparation: `tag = iconst.i64 index_plus_1`,
    /// optional `store tag, record+0` when `status_abi == 0`, `iadd_imm
    /// record, 8`. Args `[status_abi, record, index_plus_1]`; result
    /// `[tag, payload]`.
    pub const FAIL_EXIT_PREP: u32 = 19;
    /// `fail_exit` return and dead block: `return [placeholder, tag]` if
    /// `status_abi != 0` else `return [placeholder]`, create/switch `dead`,
    /// `iconst.i64 0`. Args `[status_abi, tag, placeholder]`; result `[zero]`.
    pub const FAIL_EXIT_RETURN: u32 = 20;
    /// `return_value`: return `words` loads from `buffer` when `has_buffer != 0`,
    /// `[value, 0]` when `status_abi != 0`, else `[value]`. Args
    /// `[has_buffer, buffer, words, status_abi, value]`; no result.
    pub const RETURN_VALUES: u32 = 21;
    /// `check_trace_failed`: load `flag`, branch `!= 0` to `bad` else `good`,
    /// switch to `bad`. Args `[flag]`; result `[bad, good]`.
    pub const TRACE_CHECK: u32 = 22;
    /// `trace_call`: declare/import `symbol` with param types of `args` and
    /// `i64` return, call and return `status`. Args
    /// `[arg_count, args..., sym_len, sym_words...]`; result `[status]`.
    pub const TRACE_CALL: u32 = 23;
    /// `abort_on_nonzero_status`: branch `status != 0` to `bad` else `good`,
    /// switch to `bad`, store `1` to `flag` if `has_flag != 0`. Args
    /// `[status, has_flag, flag]`; result `[bad, good]`.
    pub const TRACE_ABORT: u32 = 24;
    /// Switch to `block`. Args `[block]`; no result.
    pub const SWITCH_BLOCK: u32 = 25;
    /// Write `{data, table}` into a 16-byte `dyn` pair slot. Args
    /// `[pair, data, table]`; no result.
    pub const DYN_PAIR_INIT: u32 = 26;
    /// Store one word `value` at `ptr + 0`. Args `[ptr, value]`; no result.
    pub const DYN_STORE_WORD: u32 = 27;
    /// `dyn_dispatch`: load `data` (`pair+0`), `table` (`pair+8`), and slot
    /// target (`table+offset`). Args `[pair, offset]`; result `[data, target]`.
    pub const DYN_DISPATCH: u32 = 28;
    /// `dyn_dispatch_boxed`: load `table` (`block+0`), `data = block+header`,
    /// and slot target (`table+offset`). Args `[block, header, offset]`;
    /// result `[data, target]`.
    pub const DYN_DISPATCH_BOXED: u32 = 29;
    /// `dyn_lend_boxed` emits via [`DYN_BOXED_PARTS`] + `DYN_PAIR_INIT`.
    /// `dyn_box_new` header setup: store `table` at `block+0`, `data =
    /// block+header`. Args `[block, table, header]`; result `[data]`.
    pub const DYN_BOX_INIT: u32 = 30;
    /// Extract boxed `dyn` `{data, table}`. Args `[block, header]`; result
    /// `[data, table]`.
    pub const DYN_BOXED_PARTS: u32 = 31;
    /// Extract `{data, table}` from a `dyn` pair. Args `[pair]`; result
    /// `[data, table]`.
    pub const DYN_PAIR_PARTS: u32 = 32;
    /// One vtable candidate in `dyn_call_effectful`: branch `table == address`
    /// to `yes` else `no`, switch to `yes`. Args `[table, address]`; result
    /// `[yes, no]`.
    pub const DYN_CANDIDATE: u32 = 33;
    /// Define or look up a `dyn` vtable data object and return its
    /// `symbol_value`. Args `[words, self_size, has_drop, drop_id, slot_count,
    /// (slot, func_id)..., sym_len, sym_words...]`; result `[addr]`.
    pub const DYN_VTABLE: u32 = 34;
    /// Declare and define a by-value scalar `dyn` slot adapter function. Args
    /// `[target_id, recv_idx, loaded_ty, ptr_ty, param_count, param_tys...,
    /// ret_count, ret_tys..., sym_len, sym_words...]`; result `[func_id]`.
    pub const DYN_SLOT_ADAPTER: u32 = 35;
    /// `emit_drop_object`: drop `{data, table}` pair via vtable word 0. Args
    /// `[pair]`; no result.
    pub const DYN_DROP_OBJECT: u32 = 36;
    /// `emit_drop_boxed`: drop boxed object via vtable word 0 and free block.
    /// Args `[block, header, box_free_id]`; no result.
    pub const DYN_DROP_BOXED: u32 = 37;
    /// `native_session::session_park` prep: load evidence identity and optional
    /// staging address. Args `[ptr_code, ctx, depth, has_buffer, staging,
    /// ctx_evidence_off, ev_slots_off, slot_size, slot_identity_off,
    /// ctx_data_base_off]`; answers `[identity, staged]`.
    pub const SESSION_PARK_PREP: u32 = 38;
    /// `native_session::session_park` emit: fill `ctx.park`, call `park`,
    /// check signal/identity, branch on resume kind, switch to `on_value`.
    /// Args `[ptr_code, park_ref, ctx, identity, op_tag, arg0, arg1,
    /// violation_blk, has_buffer, staging, park_at, park_evidence_off,
    /// park_op_off, park_flags_off, park_arg0_off, park_arg1_off,
    /// resume_evidence_off, resume_kind_off, resume_value_off,
    /// resume_data_base_off, kind_value, kind_error, kind_cancelled]`;
    /// answers `[park_value, park_data_base, on_error, on_cancel, join,
    /// staged_after]`.
    pub const SESSION_PARK_EMIT: u32 = 39;
    /// `native_session`: load `ctx.data_base` and add `offset`.
    /// Args `[ptr_code, ctx, ctx_data_base_off, offset]`; answers `[dest]`.
    pub const SESSION_DATA_OFFSET: u32 = 40;
    /// `native_session`: store `ctx.result` (with optional f64/f32 bitcast)
    /// and return `status`, optionally creating & switching to a dead block.
    /// Args `[ctx, ctx_result_off, status, result_kind, value, create_dead]`;
    /// answers `[dead]` if `create_dead == 1`.
    pub const SESSION_EXIT: u32 = 41;
    /// `native_session`: switch to `violation` block and return `status`.
    /// Args `[violation_blk, status]`; no result.
    pub const SESSION_VIOLATION: u32 = 42;
    /// `dyn_drop_helper` head: create entry block, append params, switch to
    /// entry, load scalar if `scalar_ty != 0` else keep `data`. Args
    /// `[scalar_ty]`; answers `[value]`.
    pub const DYN_DROP_HELPER_HEAD: u32 = 43;
    /// `dyn_drop_helper` tail: emit `return_(&[])` and `seal_all_blocks`.
    /// Args `[]`; no result.
    pub const DYN_DROP_HELPER_TAIL: u32 = 44;
}

/// The `ffi` family's requests (SH4 lane 13, #1287): one foreign (`.vsys`)
/// call, emitted in steps by `Codegen::foreign_perform`
/// (src/ffi_lowering/native.rs). The Rust code keeps what needs its own
/// state (argument evaluation, the callee's declaration, `inout` restores,
/// the `errno` pair and a refused call's `Ok` store); the hook emits the
/// marshalling, the call, the copy-backs and the result mapping.
///
/// A call starts with [`BEGIN`](ffi::BEGIN). A hook that declines it
/// leaves the whole call to Rust; one that handles it must handle every
/// later step of the call (a later decline fails the compile).
///
/// Word encodings, used by the steps below:
/// - scalar `S`: `bits | signed << 8`;
/// - value type `V`: `[0]` unit, `[1, S]`, `[2]` bool,
///   `[3, S, n, raw...]` enum (raw values in variant order), `[4]`
///   address, `[5, nullable]` handle, `[6]` non-null handle;
/// - wire struct `W`: `[size, n, field...]`; a field is
///   `[0, offset, S, endian, native]` integer (endian 0 native, 1 little,
///   2 big), `[1, offset, n, native...]` bytes (one native offset per
///   copied byte), `[2, offset]` padding or `[3, offset, native, W]` a
///   nested struct, where `native` is the field's offset in the source
///   layout;
/// - pointee `P`: `[0, S]` or `[1, W]`.
pub mod ffi {
    /// Starts a call. Args `[handler, parameters]` (handler 0 direct, 1
    /// `isolated(mpk)`); no result.
    pub const BEGIN: u32 = 1;
    /// Marshals argument `index` after Rust evaluated it. Args `[index,
    /// source kind, source, little-endian, kind, ...]`: source kind 0 a
    /// scalar value, 1 an address of the source value, 2 a slice view's
    /// value. Kinds: `[0, V]` value, `[1]` handle, `[2]` C string,
    /// `[3, writes, nullable, some, offset, P]` pointer, `[4, writes,
    /// nullable, some, offset, n, element size, P, element offset...]`
    /// copied slice (`P` with offsets relative to an element), `[5,
    /// stride]` a slice lent in place. Result `[k, c argument..., m, copy
    /// word..., s, string word...]`: the C arguments, the copy-back record
    /// to pass to [`CALL`] (empty when none) and a C string's `[buffer,
    /// NUL offset]`.
    pub const ARG: u32 = 2;
    /// A call with C string arguments: the refusal of a string holding a
    /// NUL byte. Args `[size, err tag, err offset, NUL offset...]`; result
    /// `[slot, join block]`, emitting into the call block.
    pub const REFUSE: u32 = 3;
    /// The call. Args `[callee, errno, returns, little-endian, n, c
    /// argument..., copies, (m, copy word...)...]`, errno 0 none, 1
    /// `__errno_location`, 2 `__error`, 3 `GetLastError`, returns 1 when
    /// the C function has a result; result `[raw, os error]` (`NO_VALUE` for none), after the
    /// copy-backs. A copy-back record is `[guard, buffer, target, shape]`
    /// with shape `[0, S]`, `[1, W]` or `[2, n, element size, P, element
    /// offset...]` (guard `NO_VALUE` for none).
    pub const CALL: u32 = 4;
    /// The source value of the result. Args `[raw, V, layout...]`, the
    /// layout words of the source result type that `V` needs (see
    /// `foreign_result_layout`); result `[value]`.
    pub const RESULT: u32 = 5;
    /// Frees the C string buffers. Args `[buffer...]`; no result.
    pub const FINISH: u32 = 6;
    /// Indirect call through a function pointer (`is_indirect()`):
    /// `[sig_ref, pointer, errno, returns, little, len_word, len_i64, reg_id,
    /// released_n, released..., args_n, args..., copies_n, (len, copy...)...]`;
    /// answers `[raw, os_error]`.
    pub const CALL_INDIRECT: u32 = 7;
    /// `isolated(mpk)` entry guard: `[data_id, pointer_is_i64]`; answers `[saved]`.
    pub const ISOLATE: u32 = 8;
}

/// The `numeric` family's requests (lane 11, #1285). `Op::Vector` and
/// `Op::Tiled` instructions arrive as [`INSTRUCTION`] first; a hook that
/// declines those gets the typed requests below from `Codegen::vector_op`
/// (src/native_vector.rs) and `Codegen::tiled_op` (src/tiled/native.rs).
pub mod numeric {
    /// One `TiledSeq` operation, with the tiled runtime already defined in
    /// an object module. Args `[kind, jit, ...]`, where `kind` is one of the
    /// `TILED_*` codes below and `jit` is 1 in a JIT module (the runtime
    /// is then imported, not local). The rest, by kind (`v` is a value):
    ///
    /// - `NEW`: `[tile width, tile stride bytes, cold words per row]`;
    /// - `WITH_CAPACITY`: those, then `[v capacity]`;
    /// - `WITH_REGION`: those, then `[region variable, v capacity]`;
    /// - `LEN`, `TILE_COUNT`: `[v seq]`;
    /// - `PUSH`: `[v seq, v record]`, `GET`: `[v seq, v row]`, `SET`:
    ///   `[v seq, v row, v record]`; each followed by the row layout:
    ///   `[record size, hot count, (record offset, tile offset, element
    ///   bytes)..., cold count, (aggregate, words, record offset, word
    ///   offset)...]`;
    /// - `COL_GET`: `[v seq, v row]`, `COL_SET`: `[v seq, v row, v value]`;
    ///   then `[0, tile offset, element bytes, type]` for a hot column or
    ///   `[1, aggregate, size, words, word offset, type]` for a cold field
    ///   (type is a [`super::ty_code`], 0 for an aggregate);
    /// - `LOAD_TILE_VEC`: `[v seq, v tile, tile offset, vector bytes,
    ///   chunks]`.
    ///
    /// Result `[value]`.
    pub const TILED: u32 = 1;
    /// One vector operation (`Codegen::vector_op`, src/native_vector.rs):
    /// `[intrinsic, element, lanes, constant count, constants...,
    /// v operands...]`, with the codes of `request_intrinsic` and
    /// `request_elem` there. Result `[value]`.
    pub const VECTOR: u32 = 2;
    /// `Isa::supported`: `[enabled]`. Result `[value]` (`iconst.i64`).
    pub const ISA_SUPPORTED: u32 = 3;
    pub const TILED_NEW: u32 = 1;
    pub const TILED_WITH_CAPACITY: u32 = 2;
    pub const TILED_WITH_REGION: u32 = 3;
    pub const TILED_LEN: u32 = 4;
    pub const TILED_TILE_COUNT: u32 = 5;
    pub const TILED_PUSH: u32 = 6;
    pub const TILED_GET: u32 = 7;
    pub const TILED_SET: u32 = 8;
    pub const TILED_COL_GET: u32 = 9;
    pub const TILED_COL_SET: u32 = 10;
    pub const TILED_LOAD_TILE_VEC: u32 = 11;
    /// A float math intrinsic (`Codegen::math`, src/float_math/native.rs):
    /// `[op, v operands...]`, with `op` the position of the intrinsic in
    /// `MathOp::ALL` from 1 (`sqrt` 1 ... `fma` 9). Result `[value]`.
    pub const FLOAT_MATH: u32 = 4;
    /// A conversion to a float kind: `[to, from, v]`, `to` a float word
    /// ([`FROM_F32`] or [`FROM_F64`]) and `from` one of the `FROM_*` classes
    /// (the Rust code emits nothing for a same-kind conversion). Result
    /// `[value]`.
    pub const FLOAT_CONVERT: u32 = 5;
    /// The saturating truncation of a float to an integer kind's canonical
    /// 64-bit lane (`float_to_integer`): `[signed, bits, min, max, v]`, with
    /// the kind's bounds as two's complement words. Result `[value]`.
    pub const FLOAT_TO_INT: u32 = 6;
    /// `to_bits_k` (`to_bits` 1) or `from_bits_k` (0) of float kind `kind`:
    /// `[to_bits, kind, v]`. Result `[value]`.
    pub const FLOAT_BITS: u32 = 7;
    /// The range test of a checked float narrowing: `[kind, lo, hi, v]`,
    /// with the truncation bounds as `f64` bit patterns. Result
    /// `[overflow]`; the Rust code raises.
    pub const FLOAT_NARROW_TEST: u32 = 8;
    /// The software flush-to-zero of a subnormal float (`Codegen::flush`,
    /// requested only when the flush applies): `[v]`. Result `[value]`.
    pub const FLOAT_FLUSH: u32 = 9;
    /// One step of a region block (src/regions/native.rs), with the region
    /// runtime already defined in an object module: `[kind, jit, ...]`,
    /// `kind` one of the `REGION_*` codes below and `jit` 1 in a JIT module
    /// (the runtime is then imported). By kind (`v` is a value; a payload
    /// is `[size, aggregate, type]`, with `type` a [`super::ty_code`], 0
    /// for an aggregate; a 128-bit payload is never requested):
    ///
    /// - `OPEN`: `[capacity]`; result the region id;
    /// - `CLOSE`, `CHECKPOINT`, `USED`: `[v id]`;
    /// - `REWIND`, `COMMIT`: `[v id, v mark]`;
    /// - `BOX`: `[v id, v value, payload...]`; result the handle;
    /// - `GET`, `TAKE`: `[v id, v handle, payload...]`; result the value;
    /// - `SET`: `[v id, v handle, v value, payload...]`; result `0`.
    ///
    /// Result `[value]`.
    pub const REGION: u32 = 10;
    pub const REGION_OPEN: u64 = 1;
    pub const REGION_CLOSE: u64 = 2;
    pub const REGION_BOX: u64 = 3;
    pub const REGION_GET: u64 = 4;
    pub const REGION_TAKE: u64 = 5;
    pub const REGION_SET: u64 = 6;
    pub const REGION_CHECKPOINT: u64 = 7;
    pub const REGION_REWIND: u64 = 8;
    pub const REGION_COMMIT: u64 = 9;
    pub const REGION_USED: u64 = 10;
    /// The element address of an `Atomic[T]` receiver (`atomic_call`):
    /// `[v receiver]`. Result `[address]` (two trusted loads).
    pub const ATOMIC_CELL: u32 = 11;
    /// One atomic operation: `[op, type, signed, v address, v operands...]`,
    /// `op` one of the `ATOMIC_*` codes below (`ATOMIC_CAS` appends
    /// `[v out_ptr, ok_tag, err_tag, payload_offset]`), `type` the narrow
    /// lane's [`super::ty_code`] and the operands already narrowed. Result
    /// `[value]`, the old value as a canonical lane (`0` for a store, or
    /// `out_ptr` for `ATOMIC_CAS`).
    pub const ATOMIC: u32 = 12;
    pub const ATOMIC_LOAD: u64 = 1;
    pub const ATOMIC_STORE: u64 = 2;
    pub const ATOMIC_SWAP: u64 = 3;
    pub const ATOMIC_ADD: u64 = 4;
    pub const ATOMIC_SUB: u64 = 5;
    pub const ATOMIC_AND: u64 = 6;
    pub const ATOMIC_OR: u64 = 7;
    pub const ATOMIC_XOR: u64 = 8;
    pub const ATOMIC_CAS: u64 = 9;
    /// The `vesper_parallel_for(body, data, n)` call of `parallel_for_call`:
    /// `[body FuncRef, v data, v n]`. Result `[value]` (`0`).
    pub const PARALLEL_FOR: u32 = 13;
    /// Define an AOT runtime on the lent module (`regions::aot_runtime::define`,
    /// `tiled::aot_runtime::define`, `fip::aot_runtime::declare_or_define[_free]`):
    /// `[kind]` one of `RUNTIME_*`. No result; callers look up any needed
    /// `FuncId` by symbol name on `m`.
    pub const RUNTIME: u32 = 14;
    pub const RUNTIME_REGIONS: u64 = 1;
    pub const RUNTIME_TILED: u64 = 2;
    pub const RUNTIME_BOX_ALLOC: u64 = 3;
    pub const RUNTIME_BOX_FREE: u64 = 4;
    /// The classes of a [`FLOAT_CONVERT`] operand (`FROM_F32` and
    /// `FROM_F64` are also the float words of the other requests).
    pub const FROM_F32: u64 = 1;
    pub const FROM_F64: u64 = 2;
    pub const FROM_SIGNED: u64 = 3;
    pub const FROM_UNSIGNED: u64 = 4;
}

/// The `scalar` family's requests besides [`INSTRUCTION`] (SH4 lane 6,
/// #1280): the run-time tests of `expr_at`'s raising arms. The hook
/// answers the test's flag; the Rust code raises and emits the rest as it
/// always has, so the hook never needs `Codegen`'s raise machinery.
pub mod scalar {
    use crate::numeric::IntegerKind;
    /// `Codegen::checked_arithmetic`'s overflow test of a one-word kind.
    /// Args `[op, kind, a, b]` (op [`ADD`], [`SUB`], [`MUL`] or [`DIV`];
    /// kind [`kind_word`]); result `[overflow]`, an `i8` flag. A decline
    /// leaves the test to the Rust code.
    pub const OVERFLOW: u32 = 1;
    /// `Codegen::check_divisor`'s test. Args `[divisor]`; result
    /// `[nonzero]`, an `i8` flag.
    pub const DIVISOR: u32 = 2;
    /// `expr_at`'s checked negation of a one-word kind. Args `[kind, v]`;
    /// result `[overflow]` (`v` is the signed minimum, or any nonzero
    /// unsigned value).
    pub const NEGATE: u32 = 3;
    /// `Codegen::shift`'s range test. Args `[amount, bits]`; result
    /// `[bad]` (the amount, as an unsigned lane, is at least `bits`).
    pub const SHIFT_RANGE: u32 = 4;
    /// `Codegen::convert`'s checked narrowing bounds. Args
    /// `[from kind, to kind, v]`; result `[overflow]`. The hook declines
    /// when the target holds every value of the source (no test).
    pub const NARROW: u32 = 5;
    /// The read of a plain scalar local (no home, pointer or split):
    /// `use_var`, then `coerce` to the read's type. Args
    /// `[var, kind]` (kind the [`kind_word`] of a kind narrower than 64
    /// bits, else 0); result `[value, raw]`, the coerced value and the
    /// `use_var` value (the Rust code keeps the variable's facts on it).
    /// `raw` must be the `use_var` result itself, not a copy or a coerce
    /// of it: any other value silently loses the variable's facts rather
    /// than failing.
    pub const READ_LOCAL: u32 = 6;
    /// A fresh local bound to a value: `declare_var` of the value's type,
    /// then `def_var`. Args `[value]`; result `[var]`.
    pub const BIND_LOCAL: u32 = 7;
    /// The definition of a plain local (no home or pointer): `def_var`.
    /// Args `[var, value]`; no result.
    pub const WRITE_LOCAL: u32 = 8;
    /// [`OVERFLOW`]'s operators.
    pub const ADD: u64 = 1;
    pub const SUB: u64 = 2;
    pub const MUL: u64 = 3;
    pub const DIV: u64 = 4;
    /// The word of operator `op`, when [`OVERFLOW`] takes it.
    pub fn op_word(op: &str) -> Option<u64> {
        match op {
            "+" => Some(ADD),
            "-" => Some(SUB),
            "*" => Some(MUL),
            "/" => Some(DIV),
            _ => None,
        }
    }
    /// The word of a one-word integer kind: its position in
    /// [`crate::numeric::INTEGER_KINDS`] from 1 (`u8` 1 ... `isize` 10).
    pub fn kind_word(kind: IntegerKind) -> Option<u64> {
        crate::numeric::INTEGER_KINDS
            .iter()
            .position(|k| *k == kind)
            .map(|i| i as u64 + 1)
    }
}

/// The native walk's whole-function request (`#2094`, part 6 of `#1509`).
///
/// `native::define_instances_observed_with_trace` asks the `body` hook
/// once per function, after it has built the entry block (the frame plan:
/// the entry block's parameters, the parameters' variables) and before the
/// Rust walk (`Codegen::ir_function`). The hook emits the whole body on the
/// lent builder and answers `Ok(true)`, after which Rust only seals and
/// finalizes the function, or declines and the Rust walk emits it.
///
/// Rust asks only for a function its frame leaves plain: the optimize
/// path, no generic variables, no trace or `Fail`
/// record parameter, no caller slot, no `sret` or result buffer, no
/// function-value or `inout` parameters, no capabilities, not a `fip`
/// function and no `fip` side tables. Everything else about the body
/// (which instructions, types and terminators it covers) is the hook's to
/// decide, and it declines what it does not cover.
pub mod body {
    /// Emit one function body. [`super::Site`] names the function (the
    /// `Program` key) with no block or instruction; args
    /// `[status, locations, n, var of each of the n parameters..., m,
    /// callee of each of the m calls...]`, `status` 1 when the function
    /// returns a status word beside its value (`FnAbi::status`),
    /// `locations` 1 when instructions carry their source locations
    /// (`CodegenOptions::source_locations`), the variables in parameter
    /// order (a `fail_only` function's failure record last, counted in `n`),
    /// and for each `Op::Call` of the body in instruction order the
    /// `FuncId` the walk would call directly with nothing else around the
    /// call (part 6b), or [`NO_CALLEE`]; then `sret, h, h values`: the
    /// result pointer's value word, or [`NO_VALUE`] for a function that
    /// returns its value, and the values (by index in the graph's values)
    /// that `aggregates::plan` builds in it (#2411). A function that
    /// returns its result in registers (`FnAbi::result_words`, #2581)
    /// builds it in a local buffer: its `sret` word is the buffer's value
    /// with the result's word count at [`RESULT_WORDS_SHIFT`]. No result.
    pub const FUNCTION: u32 = 1;

    /// Where a [`FUNCTION`] request's `sret` word carries the number of
    /// words a register result returns from its buffer (0 for a result
    /// pointer); the value's own word is below it.
    pub const RESULT_WORDS_SHIFT: u32 = 32;

    /// A [`FUNCTION`] request's `sret` word when the function has no result
    /// pointer.
    pub const NO_VALUE: u64 = u64::MAX;

    /// A call [`FUNCTION`]'s hook cannot emit as one direct `call`: the
    /// hook declines the body.
    pub const NO_CALLEE: u64 = u64::MAX;

    /// A callee word's flag for a `fail_only` callee (#2425): the call
    /// passes a failure record after the arguments and raises the
    /// callee's failure again. The low 32 bits are the `FuncId`.
    pub const FAIL_CALL: u64 = 1 << 32;

    /// With [`FAIL_CALL`]: the callee returns its status word in a register
    /// (`FnAbi::status`) instead of storing a tag in the record.
    pub const STATUS_CALL: u64 = 1 << 33;

    /// [`FUNCTION`]'s leading flags, `status` and `locations`: the words
    /// before the parameter count.
    pub const FLAGS: usize = 2;

    /// The callee words of a [`FUNCTION`] request's arguments (after the
    /// parameters and the callee count), or `None` when the arguments do
    /// not have that layout.
    pub fn callees(args: &[u64]) -> Option<&[u64]> {
        layout(args).map(|(callees, _, _)| callees)
    }

    /// A [`FUNCTION`] request's callees, `sret` word and result homes, or
    /// `None` when the arguments do not have that layout.
    pub fn layout(args: &[u64]) -> Option<(&[u64], u64, &[u64])> {
        let params = usize::try_from(*args.get(FLAGS)?).ok()?;
        // The parameter count, then the parameter variables.
        let count_at = FLAGS + 1 + params;
        let count = usize::try_from(*args.get(count_at)?).ok()?;
        let sret_at = count_at + 1 + count;
        let callees = args.get(count_at + 1..sret_at)?;
        let sret = *args.get(sret_at)?;
        let homes = usize::try_from(*args.get(sret_at + 1)?).ok()?;
        args.get(sret_at + 2..)
            .filter(|rest| rest.len() == homes)
            .map(|rest| (callees, sret, rest))
    }
}

/// The `seq` family's helper requests (self-hosting SH4 lane 9, #1283).
pub mod seq {
    /// Define the AOT sequence runtime (`seq::aot_runtime::define`: the
    /// live counter, the shared empty header, the fault messages and
    /// `vesper_seq_fault`, `_new`, `_make`, `_grow`, `_free` and
    /// `vesper_str_release`) in the lent module, which has none of it yet
    /// and a 64-bit pointer type. Args [`runtime_args`]; no result. The
    /// lent builder is not used.
    pub const RUNTIME: u32 = 1;

    /// [`RUNTIME`]'s argument words: whether the target is Windows (no
    /// fault message is written there), then the runtime's constants
    /// `INLINE_BYTES`, `NEW_INLINE_BYTES`, `BUFFER_PREFIX`, `FAULT_STATUS`
    /// and `HEADER_BYTES`, so the two emitters cannot drift apart.
    pub fn runtime_args(windows: bool) -> [u64; 6] {
        use crate::seq::native_runtime::{BUFFER_PREFIX, INLINE_BYTES, NEW_INLINE_BYTES};
        [
            u64::from(windows),
            INLINE_BYTES as u64,
            NEW_INLINE_BYTES as u64,
            BUFFER_PREFIX as u64,
            crate::seq::FAULT_STATUS as u64,
            crate::seq::HEADER_BYTES as u64,
        ]
    }

    // SH4 lane 9 part 2b (#1283): seq/str/byte/hoist/fused/drop emission ops.
    pub const STRING_LITERAL: u32 = 2;
    pub const STR_OP: u32 = 3;
    pub const STR_COMPARE: u32 = 4;
    pub const BOUNDS_GUARD: u32 = 5;
    pub const HOISTED_ACCESS: u32 = 6;
    pub const INVARIANT_WORD: u32 = 7;
    pub const EMPTY_HEADER: u32 = 8;
    pub const SEQ_OP: u32 = 9;
    pub const BYTE_OP: u32 = 10;
    pub const FUSED_TRY_GET: u32 = 11;
    pub const FUSED_CONDITION: u32 = 12;
    pub const DROP_OP: u32 = 13;

    // `STR_OP` sub-ops.
    pub const STR_LEN: u64 = 1;
    pub const STR_BYTE_VALID: u64 = 2;
    pub const STR_BYTE: u64 = 3;
    pub const STR_TRY_BYTE_AT: u64 = 4;
    pub const STR_AS_BYTES: u64 = 5;
    pub const STR_SUB_STR: u64 = 6;
    pub const STR_PUSH_STR: u64 = 7;
    pub const STR_PUSH_CHAR: u64 = 8;
    pub const STR_CLEAR: u64 = 9;
    pub const STR_RESERVE: u64 = 10;

    // `HOISTED_ACCESS` kinds.
    pub const HOIST_GET: u64 = 0;
    pub const HOIST_SET: u64 = 1;
    pub const HOIST_TRY_GET: u64 = 2;
    pub const HOIST_TRY_SET: u64 = 3;

    // `SEQ_OP` sub-ops.
    pub const SEQ_WITH_CAPACITY: u64 = 1;
    pub const SEQ_LEN: u64 = 2;
    pub const SEQ_SLICE_LEN: u64 = 3;
    pub const SEQ_PUSH_TAIL: u64 = 4;
    pub const SEQ_POP: u64 = 5;
    pub const SEQ_GET: u64 = 6;
    pub const SEQ_SET: u64 = 7;
    pub const SEQ_TRY_GET: u64 = 8;
    pub const SEQ_TRY_SET: u64 = 9;
    pub const SEQ_LAST: u64 = 10;
    pub const SEQ_CLEAR: u64 = 11;
    pub const SEQ_TRUNCATE: u64 = 12;
    pub const SEQ_EXTEND_TAIL: u64 = 13;
    pub const SEQ_RESIZE_CMP: u64 = 14;
    pub const SEQ_RESIZE_SHRINK: u64 = 15;
    pub const SEQ_RESIZE_GROW_TAIL: u64 = 16;
    pub const SEQ_EXTEND_WITHIN_TAIL: u64 = 17;
    pub const SEQ_EXTEND_REVERSED_TAIL: u64 = 18;
    pub const SEQ_FINISH: u64 = 19;
    pub const SEQ_AS_SLICE: u64 = 20;
    pub const SEQ_SUB_SLICE: u64 = 21;
    pub const SEQ_CHECK_INDEX: u64 = 22;
    pub const SEQ_CHECK_EXTEND_WITHIN: u64 = 23;
    pub const SEQ_CHECK_FINISH: u64 = 24;
    pub const SEQ_RESERVE_BEGIN: u64 = 25;
    pub const SEQ_RESERVE_ONE: u64 = 26;
    pub const SEQ_HOIST_HEADER: u64 = 27;
    pub const SEQ_ELEM_ADDR: u64 = 28;
    pub const SEQ_LOAD_ELEM: u64 = 29;
    pub const SEQ_STORE_ELEM: u64 = 30;
    pub const SEQ_ALLOC: u64 = 31;
    pub const SEQ_RESERVE_GROW: u64 = 32;

    // `BYTE_OP` sub-ops.
    pub const BYTE_COPY_FROM: u64 = 1;
    pub const BYTE_COPY_WITHIN: u64 = 2;
    pub const BYTE_FILL: u64 = 3;
    pub const BYTE_FILL_RANGE: u64 = 4;
    pub const BYTE_WIDE: u64 = 5;
    pub const BYTE_SUM_BE16: u64 = 6;
    pub const BYTE_BYTES_EQ: u64 = 7;
    pub const BYTE_BYTES_CMP: u64 = 8;
    pub const BYTE_FIND_BYTE: u64 = 9;
    pub const BYTE_FIND_EITHER: u64 = 10;
    pub const BYTE_FIND_NOT_IN: u64 = 11;
    pub const BYTE_EXTEND_LOWER: u64 = 12;
    pub const BYTE_MAKE_LOWER: u64 = 13;
    pub const BYTE_CHECK_COPY_FROM: u64 = 14;
    pub const BYTE_CHECK_COPY_WITHIN: u64 = 15;
    pub const BYTE_CHECK_FILL_RANGE: u64 = 16;
    pub const BYTE_CHECK_WIDE: u64 = 17;
    pub const BYTE_CHECK_MAKE_LOWER: u64 = 18;

    // `DROP_OP` sub-ops.
    pub const DROP_SLOTS_STR: u64 = 1;
    pub const DROP_SLOTS_FIELDS: u64 = 2;
    pub const DROP_MOVED_ZERO: u64 = 3;
    pub const DROP_MOVED_CLEAR: u64 = 4;
    pub const DROP_FIELD_MOVED: u64 = 5;
    pub const DROP_REBIND_COPY: u64 = 6;
    pub const DROP_FREE_GUARD: u64 = 7;
    pub const DROP_GLUE: u64 = 8;

    // `DROP_GLUE` tree node tags.
    pub const DROP_NODE_SEQ: u64 = 1;
    pub const DROP_NODE_STR: u64 = 2;
    pub const DROP_NODE_DYN: u64 = 3;
    pub const DROP_NODE_PRODUCT: u64 = 4;
    pub const DROP_NODE_ENUM: u64 = 5;
}
