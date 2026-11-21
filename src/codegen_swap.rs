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
//! | `seq` | `Codegen::ir_graph_at` | [`INSTRUCTION`] for `Op::Seq` |
//! | `control` | the same | [`INSTRUCTION`] for closures, handlers and calls of effect operations |
//! | `numeric` | the same | [`INSTRUCTION`] for `Op::Vector` and `Op::Tiled` |
//! | `drops` (fip) | the same | [`INSTRUCTION`] for `Op::Fip` |
//! | `ffi` | the same | [`INSTRUCTION`] for a call of a foreign (`.vsys`) effect operation |
//! | `export` | `native_export::glue::define`, after the sequence runtime and `vesper_box_alloc` | the rest of the v2 glue over a lent module ([`ExportHook`]) |
//! | `freestanding` | `native_freestanding::build_image` | whole image ([`FreestandingHook`]) |
//! | `wasm` | `wasm_backend::compile` | whole component ([`WasmHook`]) |
//! | `ebpf` | `ebpf_backend::compile_world` | whole program ([`EbpfHook`]) |
//! | `verifiers` | `native_verifiers::verify_emitted` | whole report ([`VerifierHook`]) |
//!
//! An [`INSTRUCTION`] request's arguments are the Cranelift value of each
//! operand of the IR instruction, in `ir_inline::operands` order
//! ([`NO_VALUE`] for an operand with no value yet), and its one result is
//! the instruction's value. The [`Site`] names the instruction, so a hook
//! reads the instruction itself from the `Program`.
use crate::ast::Type;
use crate::check::World;
use crate::ir::{Op, Program};
use cranelift_codegen::ir;
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;
use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// An argument word for an operand that has no Cranelift value.
pub const NO_VALUE: u64 = u64::MAX;
/// The request of a family hook at the IR graph walk: emit one Place IR
/// instruction.
pub const INSTRUCTION: u32 = 0;

/// Where a request was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Site<'a> {
    /// The lowered function (its `Program` key) being emitted; empty when
    /// the request is not inside a Place IR graph.
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
    pub freestanding: Option<FreestandingHook>,
    pub wasm: Option<WasmHook>,
    pub ebpf: Option<EbpfHook>,
    pub verifiers: Option<VerifierHook>,
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
            || self.freestanding.is_some()
            || self.wasm.is_some()
            || self.ebpf.is_some()
            || self.verifiers.is_some()
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
