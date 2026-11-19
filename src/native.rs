//! Cranelift SSA JIT/object backend. Scalar functions, recursion, mutation, and
//! statically tail-resumptive lexical handlers compile without runtime allocation.
use crate::{
    ast::*,
    check::World,
    layout::{Layout, LayoutField, LayoutKind},
};
use cranelift_codegen::{
    ir::{
        self, AbiParam, InstBuilder,
        condcodes::{FloatCC, IntCC},
        types,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::{BTreeMap, BTreeSet};
use std::{cell::RefCell, rc::Rc};

/// `vesper build --lib`: C-ABI library objects (port P0 lane 6, #878).
#[path = "native_aggregates.rs"]
mod aggregates;
/// Fixed-size array writes and constant tables (§5, #1091).
#[path = "native_arrays.rs"]
mod native_arrays;

#[path = "dyn_trait/native.rs"]
mod dyn_native;
#[path = "native_export.rs"]
pub mod export;
/// Foreign calls of imported `.vsys` libraries (§13, `crate::ffi_lowering`).
#[path = "ffi_lowering/native.rs"]
mod ffi;
/// `f32`, float intrinsics and conversions (`vesper.float.ieee754/1`).
#[path = "float_math/native.rs"]
mod float_math;
/// Lane 27: bare-metal RISC-V and AArch64 images with no runtime.
#[path = "native_freestanding.rs"]
pub mod freestanding;
/// Cranelift SIMD lowering of `Op::Vector` (lane 28).
#[path = "native_vector.rs"]
mod native_vector;
/// Source Strata region blocks lowered to `vesper_region_*` runtime calls.
#[path = "regions/native.rs"]
mod regions;
/// `ChunkSeq[T]` and `Slice[T]` native lowering (§13).
#[path = "seq/native.rs"]
mod seq_native;
use seq_native::SeqDropSlot;
/// Checked source entries with declared linear external effects compiled to
/// ABI v2 fiber entries that park through `vesper-fiber-abi`.
#[path = "native_session.rs"]
pub mod session;
/// `String` and `Str` native lowering (§13 strings).
#[path = "strings/native.rs"]
mod strings_native;
/// AoSoA `tiled(N) struct` and `TiledSeq[T]` native lowering (§12.5).
#[path = "tiled/native.rs"]
mod tiled;
use float_math::is_float_type;
/// Windows x64 COFF objects with unwind tables, and the MSVC link driver.
#[path = "native_windows.rs"]
pub mod windows;

fn scalar(t: &Type, generics: &[Generic]) -> Result<ir::Type, String> {
    match t {
        Type::IntLiteral(_) => Ok(types::I64),
        Type::Named(n, _) if n == "f64" => Ok(types::F64),
        Type::Named(n, _) if n == "f32" => Ok(types::F32),
        Type::Named(n, _)
            if (crate::numeric::is_scalar_name(n) && !crate::numeric::is_float_name(n))
                || generics.iter().any(|g| g.name == *n) =>
        {
            Ok(types::I64)
        }
        Type::Tuple(ts) if ts.is_empty() => Ok(types::I64),
        Type::Infer | Type::Function { .. } => Ok(types::I64),
        // Region box, watermark, TiledSeq, and Box[T] handles are one word (§12.4, §12.5, §15).
        Type::Named(n, _) if crate::regions::is_handle_name(n) => Ok(types::I64),
        Type::Named(n, args) if n == crate::fip::BOX_TYPE && args.len() == 1 => Ok(types::I64),
        t if crate::tiled::is_seq_type(t) => Ok(types::I64),
        t if crate::seq::is_seq_type(t) => Ok(types::I64),
        _ => Err(format!(
            "native backend does not yet lower aggregate type {t}"
        )),
    }
}
fn uses_trace_fail(world: &World, trace_host: bool, key: &str, entry: &str, f: &Function) -> bool {
    trace_host && (key == entry || trace_host_direct(world, f))
}
fn signature<M: Module>(m: &M, world: &World, f: &Function) -> Result<ir::Signature, String> {
    signature_with_trace(m, world, f, false)
}
fn signature_with_trace<M: Module>(
    m: &M,
    world: &World,
    f: &Function,
    trace_fail: bool,
) -> Result<ir::Signature, String> {
    signature_with_abi(m, world, f, trace_fail, FnAbi::default())
}
/// [`signature_with_trace`] for an instance compiled with `abi`
/// ([`NativeAbi`]): a register-status function returns its failure status as
/// a last `i64` value, and a register-result function returns its small
/// aggregate result as `i64` words instead of through a leading pointer.
fn signature_with_abi<M: Module>(
    m: &M,
    world: &World,
    f: &Function,
    trace_fail: bool,
    abi: FnAbi,
) -> Result<ir::Signature, String> {
    let mut sig = m.make_signature();
    let result = Layout::of(world, &f.result)?;
    if result.aggregate() && abi.result_words.is_none() {
        sig.params
            .push(AbiParam::new(m.target_config().pointer_type()));
    }
    for p in &f.params {
        let layout = Layout::of(world, &p.ty)?;
        sig.params.push(AbiParam::new(
            if layout.aggregate() || p.pass == Pass::Inout {
                m.target_config().pointer_type()
            } else {
                scalar(&p.ty, &f.generics)?
            },
        ));
    }
    if crate::fip::uses_caller_slot(world, f) {
        sig.params
            .push(AbiParam::new(m.target_config().pointer_type()));
    }
    if fail_only(world, f) || trace_fail {
        sig.params
            .push(AbiParam::new(m.target_config().pointer_type()));
    }
    if let Some(words) = abi.result_words {
        for _ in 0..words {
            sig.returns.push(AbiParam::new(types::I64));
        }
    } else {
        sig.returns.push(AbiParam::new(if result.aggregate() {
            types::I64
        } else {
            scalar(&f.result, &f.generics)?
        }));
    }
    if abi.status {
        sig.returns.push(AbiParam::new(types::I64));
    }
    Ok(sig)
}
fn instance_key(name: &str, vars: &BTreeMap<String, Type>) -> String {
    if vars.is_empty() {
        name.into()
    } else if vars.keys().all(|k| k.starts_with(FN_ARG)) {
        // A function-value instance: `apply$fn[f=triple]`.
        let bound = function_arguments(vars)
            .into_iter()
            .map(|(p, f)| format!("{p}={f}"))
            .collect::<Vec<_>>();
        format!("{name}$fn[{}]", bound.join(","))
    } else {
        format!("{name}${vars:?}")
    }
}
/// Instance-variable prefix binding a function-value parameter to a
/// top-level function ([`function_arguments`]): `"$fn:<param>"` maps to
/// `Type::Named(<function>, [])`. `$` never starts a generic name, so
/// substitution ignores these entries.
const FN_ARG: &str = "$fn:";
/// The function-value parameters of an instance bound to top-level
/// functions, `param -> function` (empty for other instances; pass
/// [`EmittedFunction::vars`]).
pub fn function_arguments(vars: &BTreeMap<String, Type>) -> BTreeMap<String, String> {
    vars.iter()
        .filter_map(|(k, t)| Some((k.strip_prefix(FN_ARG)?.to_string(), t.name().to_string())))
        .collect()
}
/// Whether calls to `f` whose function-value arguments are all top-level
/// functions get an instance of their own, with those parameters bound at
/// compile time (lane 23, #346). Only functions whose row is non-empty and
/// contains only capability effects qualify, so the Stage-5 verifiers can
/// check a capability function that takes a function value on its own;
/// every other function taking a function value is still specialized into
/// its callers, as before.
fn function_value_instance(world: &World, f: &Function) -> bool {
    !f.effects.instances.is_empty()
        && f.effects.tail.is_none()
        && capability_only(f)
        && f.generics.is_empty()
        && !contains_function(world, &f.result)
        && f.params
            .iter()
            .any(|p| matches!(p.ty, Type::Function { .. }))
        && f.params.iter().all(|p| match &p.ty {
            Type::Function { .. } => p.pass != Pass::Inout,
            t => !contains_function(world, t),
        })
}
fn concrete(world: &World, name: &str, vars: &BTreeMap<String, Type>) -> Result<Function, String> {
    let mut f = world
        .functions
        .get(name)
        .ok_or_else(|| format!("unknown function '{name}'"))?
        .clone();
    let bound = function_arguments(vars);
    if !bound.is_empty() {
        // Function-value parameters bound at compile time are not passed.
        f.params.retain(|p| !bound.contains_key(&p.name));
    }
    for p in &mut f.params {
        p.ty = substitute(&p.ty, vars);
    }
    f.result = substitute(&f.result, vars);
    f.effects = substitute_effects(&f.effects, vars);
    Ok(f)
}
fn call_vars(world: &World, e: &Expr, vars: &BTreeMap<String, Type>) -> BTreeMap<String, Type> {
    world
        .calls
        .get(&(e.span.start, e.span.end))
        .map(|vs| {
            vs.iter()
                .map(|(n, t)| (n.clone(), substitute(t, vars)))
                .collect()
        })
        .unwrap_or_default()
}
fn visit(e: &Expr, callback: &mut impl FnMut(&Expr)) {
    callback(e);
    match &e.kind {
        ExprKind::Tuple(es) => {
            for e in es {
                visit(e, callback)
            }
        }
        ExprKind::Struct { fields, .. } => {
            for (_, e) in fields {
                visit(e, callback)
            }
        }
        ExprKind::Block { statements, tail } => {
            for s in statements {
                visit(
                    match s {
                        Statement::Let { value, .. } => value,
                        Statement::Expr(e) => e,
                    },
                    callback,
                )
            }
            if let Some(e) = tail {
                visit(e, callback)
            }
        }
        ExprKind::Call { callee, args, .. } => {
            visit(callee, callback);
            for a in args {
                visit(&a.expr, callback)
            }
        }
        ExprKind::Unary(_, e) | ExprKind::Field(e, _) | ExprKind::Try(e) => visit(e, callback),
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Assign(_, a, b) => {
            visit(a, callback);
            visit(b, callback)
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            visit(condition, callback);
            visit(then, callback);
            if let Some(e) = otherwise {
                visit(e, callback)
            }
        }
        ExprKind::Match { value, arms } => {
            visit(value, callback);
            for (_, e) in arms {
                visit(e, callback)
            }
        }
        ExprKind::Closure { body, .. } => visit(body, callback),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, e)) = state {
                visit(e, callback)
            }
            visit(body, callback);
            visit(&ret.body, callback);
            for c in ops {
                visit(&c.body, callback)
            }
        }
        k @ (ExprKind::Loop(_)
        | ExprKind::For { .. }
        | ExprKind::Range { .. }
        | ExprKind::Break(_)
        | ExprKind::Return(_)) => {
            for c in k.control_children().into_iter().flatten() {
                visit(c, callback)
            }
        }
        _ => {}
    }
}
pub(crate) fn contains_function(world: &World, t: &Type) -> bool {
    fn visit(world: &World, t: &Type, seen: &mut BTreeSet<String>) -> bool {
        match t {
            Type::Function { .. } => true,
            Type::Tuple(ts) => ts.iter().any(|t| visit(world, t, seen)),
            Type::Named(n, args) => {
                if !seen.insert(n.clone()) {
                    return args.iter().any(|t| matches!(t, Type::Function { .. }));
                }
                let result = world.adts.get(n).is_some_and(|a| {
                    let vars = a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    a.variants
                        .iter()
                        .flat_map(|v| &v.fields)
                        .any(|f| visit(world, &substitute(&f.ty, &vars), seen))
                });
                seen.remove(n);
                result
            }
            _ => false,
        }
    }
    visit(world, t, &mut BTreeSet::new())
}
fn specialized_function(world: &World, f: &Function) -> bool {
    (!capability_only(f)
        || f.effects.tail.is_some()
        || contains_function(world, &f.result)
        || f.params.iter().any(|p| contains_function(world, &p.ty)))
        && !fail_only(world, f)
}
/// Whether every effect in `f`'s row is a built-in capability effect
/// (`ConstTime`, `Realtime`, `DetFloat`, lane 15), including the empty row.
/// Capability effects have no operations and no handlers: they constrain
/// the body, not its calling convention, so such a function is compiled once
/// like a pure function instead of being inlined into its callers. That is
/// what lets the Stage-5 verifiers (lane 24) check its code on its own.
/// Foreign-library effects (`Ffi[Lib]`, §13) count too: their operations
/// lower to direct native calls and need no handler evidence.
fn capability_only(f: &Function) -> bool {
    f.effects.instances.iter().all(|e| {
        crate::capability_effects::is_capability_effect(&e.name)
            || crate::ffi_lowering::is_foreign_effect(&e.name)
    })
}
/// Whether the program needs Cranelift's NaN canonicalisation: any
/// `eff [DetFloat]` function promises bit-identical floats on x86-64 and
/// AArch64, whose default NaN bits differ (§36.2).
pub fn needs_nan_canonicalization(world: &World) -> bool {
    world.functions.values().any(|f| {
        crate::capability_effects::capabilities(&f.effects)
            .contains(&crate::capability_effects::Capability::DetFloat)
            || closure_capabilities(f).contains(&crate::capability_effects::Capability::DetFloat)
    })
}
/// Capability effects declared on closure literals anywhere in `f`'s body.
/// Closures are specialized into the function that contains them, so their
/// code is part of that function's Cranelift function.
pub fn closure_capabilities(f: &Function) -> BTreeSet<crate::capability_effects::Capability> {
    let mut out = BTreeSet::new();
    if let Some(body) = &f.body {
        visit(body, &mut |e| {
            if let ExprKind::Closure { effects, .. } = &e.kind {
                out.extend(crate::capability_effects::capabilities(effects));
            }
        });
    }
    out
}
/// A function whose only effects are unlabeled `Fail` instances, such as a
/// recursive function using checked arithmetic. `raise` never resumes (its
/// continuation takes `Never`), so instead of inlining the function into its
/// handlers it is compiled once with a hidden failure-record pointer: the
/// callee stores the instance index and error value and returns, and the
/// caller re-raises through the capability it would have passed.
///
/// Capability effects (`ConstTime`, `Realtime`, `DetFloat`) may share the
/// row (lane 23, #346): they have no operations, so the failure record is
/// the only capability such a function is passed, and `[Realtime, Fail[E]]`
/// functions are compiled once like `[Fail[E]]` ones instead of being
/// specialized into their callers. That is the capability-passing ABI
/// ([`CAPABILITY_ABI`]); rows without a capability effect compile exactly
/// as before.
/// The locals of `f` that the optimize path keeps in registers: numeric and
/// `bool` locals that no closure or handler mentions and that are borrowed as
/// places only by calls with an empty effect row. Such a call returns
/// normally, so `call_function` restores the value from its scratch slot
/// right after it; a call that may fail or perform an effect could leave
/// through a handler first, so its places keep a home slot that the callee
/// writes through.
fn register_locals(
    world: &World,
    f: &crate::ir::FunctionIr,
    generics: &BTreeMap<String, Type>,
) -> BTreeSet<String> {
    use crate::ir::Op;
    let mut memory = BTreeSet::new();
    let place_roots = |args: &[crate::ir::Argument], memory: &mut BTreeSet<String>| {
        for a in args {
            if let Some(p) = &a.place {
                memory.insert(p.root.clone());
            }
        }
    };
    for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &inst.op {
            Op::Call { callee, args, .. } => {
                let pure = world
                    .functions
                    .get(callee)
                    .is_some_and(|g| g.effects.instances.is_empty() && g.effects.tail.is_none());
                if !pure {
                    place_roots(args, &mut memory);
                }
            }
            // Sequence and `Box` operations take their operands as values.
            Op::Indirect { args, .. } | Op::Tiled { args, .. } => place_roots(args, &mut memory),
            Op::Closure {
                expr,
                aliases,
                moves,
            }
            | Op::Handle {
                expr,
                aliases,
                moves,
                ..
            } => {
                memory.extend(crate::check::free_names(expr));
                memory.extend(aliases.keys().cloned());
                memory.extend(aliases.values().cloned());
                memory.extend(moves.iter().map(|p| p.root.clone()));
            }
            _ => {}
        }
    }
    f.locals
        .iter()
        .filter(|(n, t)| {
            let t = crate::numeric::default_type(&substitute(t, generics));
            !memory.contains(*n)
                && matches!(&t, Type::Named(name, args) if args.is_empty()
                    && (crate::numeric::is_integer_name(name)
                        || matches!(name.as_str(), "f32" | "f64" | "bool")))
        })
        .map(|(n, _)| n.clone())
        .chain(in_param_registers(world, f, generics))
        .collect()
}
/// The `in` parameters of `f` whose word (a sequence, string or other
/// handle; aggregates already arrive as pointers) can stay in the incoming
/// argument register instead of a home slot (port speed lane 3, #1023).
///
/// An `in` parameter is a shared borrow for the whole call: the checker
/// rejects every write to it and every `inout` or `sink` use of it, and
/// while the caller lends it as `in` no other path may change it either. So
/// the word read at entry is the word at every later read, and a use that
/// needs an address gets a scratch copy ([`Codegen::address`]), which is
/// never written. A name that the body rebinds, writes, moves or lends
/// mutably, or that a closure or handler captures, keeps its slot, and so
/// does every parameter of a function that calls.
fn in_param_registers<'a>(
    world: &'a World,
    f: &'a crate::ir::FunctionIr,
    generics: &'a BTreeMap<String, Type>,
) -> impl Iterator<Item = String> + 'a {
    use crate::ir::Op;
    let mut kept = BTreeSet::new();
    let lent = |args: &[crate::ir::Argument], kept: &mut BTreeSet<String>| {
        for a in args {
            if a.pass != Pass::In
                && let Some(p) = &a.place
            {
                kept.insert(p.root.clone());
            }
        }
    };
    for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &inst.op {
            Op::Write { place, .. } => {
                kept.insert(place.split('.').next().unwrap_or(place).to_owned());
            }
            Op::Bind { pattern, .. } => bound_names(pattern, &mut kept),
            Op::Read { root, pass, .. } if *pass != Pass::In => {
                kept.insert(root.clone());
            }
            Op::Call { args, .. }
            | Op::Indirect { args, .. }
            | Op::Tiled { args, .. }
            | Op::Fip { args, .. }
            | Op::Seq { args, .. } => lent(args, &mut kept),
            Op::Closure {
                expr,
                aliases,
                moves,
            }
            | Op::Handle {
                expr,
                aliases,
                moves,
                ..
            } => {
                kept.extend(crate::check::free_names(expr));
                kept.extend(aliases.keys().cloned());
                kept.extend(aliases.values().cloned());
                kept.extend(moves.iter().map(|p| p.root.clone()));
            }
            _ => {}
        }
    }
    // Only in a function that calls nothing: across a call the incoming
    // word would need a callee-saved register (saved and restored around
    // the whole body) or a spill, which costs more than the home slot's
    // store and reloads (Quarry's planner ran 0.07% more instructions).
    if f.blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .any(|i| crate::bounds_hoist::calls_out(&i.op))
    {
        kept.extend(f.params.iter().map(|p| p.name.clone()));
    }
    f.params.iter().filter_map(move |p| {
        let ty = crate::numeric::default_type(&substitute(&p.ty, generics));
        (p.pass == Pass::In
            && !kept.contains(&p.name)
            && f.locals.contains_key(&p.name)
            && !matches!(ty, Type::Function { .. })
            && Layout::of(world, &ty).is_ok_and(|l| !l.aggregate()))
        .then(|| p.name.clone())
    })
}
fn bound_names(p: &Pattern, out: &mut BTreeSet<String>) {
    match &p.kind {
        PatternKind::Bind(n) => {
            out.insert(n.clone());
        }
        PatternKind::Tuple(ps) => ps.iter().for_each(|p| bound_names(p, out)),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().for_each(|(_, p)| bound_names(p, out))
        }
        PatternKind::Wild | PatternKind::Literal(_) => {}
    }
}
fn fail_only(world: &World, f: &Function) -> bool {
    f.effects.tail.is_none()
        && f.effects.instances.iter().any(|e| e.name == "Fail")
        && f.effects.instances.iter().all(|e| {
            (e.name == "Fail" && e.label.is_none() && e.args.len() == 1)
                || ((crate::capability_effects::is_capability_effect(&e.name)
                    || crate::ffi_lowering::is_foreign_effect(&e.name))
                    && e.label.is_none()
                    && e.args.is_empty())
        })
        && !contains_function(world, &f.result)
        && !f.params.iter().any(|p| contains_function(world, &p.ty))
}
/// The `Fail` instances of a `fail_only` row, in row order: failure-record
/// tag `k + 1` is the `k`-th of them.
fn fail_instances(f: &Function) -> impl Iterator<Item = &Effect> {
    f.effects.instances.iter().filter(|e| e.name == "Fail")
}
/// Whether `f` uses the capability-passing ABI: a `fail_only` row that also
/// names a capability effect.
fn capability_abi(world: &World, f: &Function) -> bool {
    fail_only(world, f)
        && f.effects
            .instances
            .iter()
            .any(|e| crate::capability_effects::is_capability_effect(&e.name))
}
/// Version of the capability-passing ABI. Objects with at least one such
/// function carry a local data symbol [`CAPABILITY_ABI_SYMBOL`] whose bytes
/// are this string, a newline, and the symbols compiled with it, one per
/// line; objects without one are unchanged.
pub const CAPABILITY_ABI: &str = "vesper.native.capability-abi/1";
pub const CAPABILITY_ABI_SYMBOL: &str = "__vesper_capability_abi";
/// Instance keys of `entry`'s program compiled with the capability-passing
/// ABI (empty for programs without `[Capability, Fail[E]]` rows).
pub fn capability_abi_functions(world: &World, entry: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for (key, (name, vars)) in instances(world, entry)? {
        if capability_abi(world, &concrete(world, &name, &vars)?) {
            out.push(key);
        }
    }
    Ok(out)
}
/// Bytes of a failure record: an instance tag followed by the error payload.
fn fail_record_size(world: &World, f: &Function) -> Result<u32, String> {
    let mut payload = 8;
    for e in fail_instances(f) {
        payload = payload.max(Layout::of(world, &e.args[0])?.size);
    }
    Ok(8 + payload.div_ceil(8) * 8)
}
/// How one instance is called, beyond the default ABI ([`NativeAbi`]).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct FnAbi {
    /// Register-status Fail ABI: the failure status is the last return value.
    status: bool,
    /// Register-result ABI: the aggregate result is returned as this many
    /// `i64` words (1 or 2) instead of through a leading result pointer.
    result_words: Option<u32>,
}
/// The internal calling conventions of one compilation (Stage 1 perf lane
/// 5, #935). Only optimized builds use them (a native-only change behind
/// `optimize`, so the Stage 1 corpus, built unoptimized, keeps its Cranelift
/// artifacts), and never for boundary symbols: `is_boundary` names the entry
/// or the library exports, which foreign callers reach with the documented
/// export ABI.
///
/// * Register status: a `fail_only` function returns its failure status as
///   a last `i64` value, 0 for success and `k` when it raised the `k`-th
///   `Fail` of its row. The hidden pointer it takes is caller storage for
///   the error payload only (at the same offset as in a failure record), so
///   a successful call stores nothing: the caller neither zeroes a record
///   before the call nor loads a tag after it, and tests the returned
///   register instead. Capability-ABI functions ([`CAPABILITY_ABI`]) keep
///   their documented record.
/// * Register result: a function that does not fail and returns a copy
///   aggregate of at most two words (a small `Result` or `Option`, a pair)
///   returns its words in registers. The caller receives them into
///   uninitialized stack storage instead of zeroing a result buffer for the
///   callee to fill. Two `i64` returns fit the return registers of every
///   calling convention Cranelift targets here (`rax`/`rdx`, `x0`/`x1`).
#[derive(Default)]
struct NativeAbi {
    functions: BTreeMap<String, FnAbi>,
    /// Whether the internal conventions apply (optimized builds).
    optimize: bool,
}
impl NativeAbi {
    fn plan(
        world: &World,
        instances: &InstanceMap,
        optimize: bool,
        is_boundary: impl Fn(&str) -> bool,
    ) -> Result<Self, String> {
        let mut functions = BTreeMap::new();
        if !optimize {
            return Ok(Self {
                functions,
                optimize,
            });
        }
        for (key, (name, vars)) in instances {
            if is_boundary(key) {
                continue;
            }
            let f = concrete(world, name, vars)?;
            let abi = Self::internal(world, &f)?;
            if abi != FnAbi::default() {
                functions.insert(key.clone(), abi);
            }
        }
        Ok(Self {
            functions,
            optimize,
        })
    }
    /// The internal convention of a non-boundary function `f`.
    fn internal(world: &World, f: &Function) -> Result<FnAbi, String> {
        let mut abi = FnAbi::default();
        if fail_only(world, f) && !capability_abi(world, f) {
            abi.status = true;
        } else if f.effects.instances.is_empty() && f.effects.tail.is_none() {
            abi.result_words = register_result_words(world, f)?;
        }
        Ok(abi)
    }
    /// The convention of a vtable slot whose stub is `f`: the one every
    /// non-boundary method instance of that signature uses.
    fn slot(&self, world: &World, f: &Function) -> Result<FnAbi, String> {
        if self.optimize {
            Self::internal(world, f)
        } else {
            Ok(FnAbi::default())
        }
    }
    fn of(&self, key: &str) -> FnAbi {
        self.functions.get(key).copied().unwrap_or_default()
    }
}
/// Words of `f`'s result when it can be returned in registers: an aggregate
/// of one or two words that holds no function value and is not built in a
/// caller-provided `Box` slot.
fn register_result_words(world: &World, f: &Function) -> Result<Option<u32>, String> {
    let layout = Layout::of(world, &f.result)?;
    if !layout.aggregate()
        || layout.size == 0
        || layout.size > 16
        || contains_function(world, &f.result)
        || crate::fip::uses_caller_slot(world, f)
        || f.fip
    {
        return Ok(None);
    }
    Ok(Some(layout.size.div_ceil(8)))
}
/// No internal conventions: every function keeps the default ABI.
static DEFAULT_ABI: std::sync::LazyLock<NativeAbi> = std::sync::LazyLock::new(NativeAbi::default);
type InstanceMap = BTreeMap<String, (String, BTreeMap<String, Type>)>;
fn instances(world: &World, entry: &str) -> Result<InstanceMap, String> {
    instances_for_entry(world, entry, false)
}
fn trace_host_direct(world: &World, f: &Function) -> bool {
    crate::trace_effects::active(world)
        && f.effects.tail.is_none()
        && !contains_function(world, &f.result)
        && f.params.iter().all(|p| !contains_function(world, &p.ty))
        && f.effects.instances.iter().all(|e| {
            crate::capability_effects::is_capability_effect(&e.name)
                || (e.name == crate::trace_effects::TRACE && e.label.is_none() && e.args.is_empty())
        })
}
fn instances_for_entry(
    world: &World,
    entry: &str,
    include_entry: bool,
) -> Result<InstanceMap, String> {
    let program = crate::ir::lower_world(world);
    let references = program
        .functions
        .values()
        .flat_map(|f| {
            f.blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .filter_map(|i| {
                    if let crate::ir::Op::Read { root, .. } = &i.op
                        && world.functions.contains_key(root)
                        && !f.locals.contains_key(root)
                    {
                        return Some((i.span.start, i.span.end, root.clone()));
                    }
                    None
                })
        })
        .collect::<BTreeSet<_>>();
    let mut todo = vec![(entry.to_string(), BTreeMap::new())];
    let mut seen = BTreeSet::new();
    let mut result = BTreeMap::new();
    while let Some((name, vars)) = todo.pop() {
        let key = instance_key(&name, &vars);
        if !seen.insert(key.clone()) {
            continue;
        }
        if seen.len() > 1024 {
            return Err("native monomorphization exceeds 1024 function instances".into());
        }
        let f = concrete(world, &name, &vars)?;
        if (include_entry && (key == entry || trace_host_direct(world, &f)))
            || !specialized_function(world, &f)
            || !function_arguments(&vars).is_empty()
        {
            result.insert(key, (name.clone(), vars.clone()));
        }
        if let Some(body) = &f.body {
            visit(body, &mut |e| {
                if let ExprKind::Name(n) = &e.kind
                    && references.contains(&(e.span.start, e.span.end, n.clone()))
                    && world.functions[n].generics.is_empty()
                {
                    todo.push((n.clone(), BTreeMap::new()));
                }
                if let ExprKind::Call { callee, .. } = &e.kind {
                    let target = match &callee.kind {
                        ExprKind::Name(n)
                            if world.functions.contains_key(n)
                                && !world
                                    .types
                                    .contains_key(&(callee.span.start, callee.span.end)) =>
                        {
                            Some(n.clone())
                        }
                        ExprKind::Field(base, method) => world
                            .types
                            .get(&(base.span.start, base.span.end))
                            .map(|t| format!("{}::{method}", substitute(t, &vars).name()))
                            .filter(|n| world.functions.contains_key(n)),
                        _ => None,
                    };
                    // A boxed object (§7.3) needs its vtable's methods.
                    if let ExprKind::Call { args, .. } = &e.kind
                        && matches!(&callee.kind, ExprKind::Name(n) if n == crate::fip::BOX_NEW)
                        && let Some(ty) = world.types.get(&(e.span.start, e.span.end))
                        && let Some(object) = crate::dyn_trait::boxed_object(&substitute(ty, &vars))
                        && let Some(arg) = args.first()
                        && let Some(actual) =
                            world.types.get(&(arg.expr.span.start, arg.expr.span.end))
                        && let Some(vtable) =
                            world.dyn_objects.vtable(object, &substitute(actual, &vars))
                    {
                        for (method, vars) in vtable.methods.clone() {
                            todo.push((method, vars));
                        }
                    }
                    if let Some(name) = target {
                        let mut instance = call_vars(world, e, &vars);
                        if let ExprKind::Call { args, .. } = &e.kind
                            && let Some(callee) = world.functions.get(&name)
                            && function_value_instance(world, callee)
                        {
                            let mut bound = BTreeMap::new();
                            for (p, a) in callee.params.iter().zip(args) {
                                if !matches!(p.ty, Type::Function { .. }) {
                                    continue;
                                }
                                match &a.expr.kind {
                                    ExprKind::Name(n)
                                        if references.contains(&(
                                            a.expr.span.start,
                                            a.expr.span.end,
                                            n.clone(),
                                        )) && world.functions[n].generics.is_empty() =>
                                    {
                                        bound.insert(
                                            format!("{FN_ARG}{}", p.name),
                                            Type::Named(n.clone(), vec![]),
                                        );
                                    }
                                    _ => {
                                        bound.clear();
                                        break;
                                    }
                                }
                            }
                            instance.extend(bound);
                        }
                        // A `dyn` argument needs its vtable's methods; the
                        // `dyn` function has one instance, its `dyn`
                        // parameters erased to their object types.
                        for (_, vtable) in world.dyn_objects.coercions(&name, &instance) {
                            for (method, vars) in
                                vtable.map(|v| v.methods.clone()).unwrap_or_default()
                            {
                                todo.push((method, vars));
                            }
                        }
                        let instance = world.dyn_objects.erase(&name, &instance);
                        if world.dyn_objects.stubs.contains_key(&name) {
                            // A vtable stub is called through its slot.
                            return;
                        }
                        todo.push((name, instance));
                    }
                }
            });
        }
    }
    Ok(result)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryEffects {
    Handled,
    FailOnly,
    CapabilityOrFail,
    TraceHost,
}
fn compile<M: Module>(
    m: &mut M,
    world: &World,
    entry: &str,
    optimize: bool,
    observer: Option<&mut dyn FnMut(EmittedFunction)>,
    instrument: Option<Instrument<'_>>,
    options: CodegenOptions,
) -> Result<(BTreeMap<String, FuncId>, String), String> {
    compile_entry(
        m,
        world,
        entry,
        optimize,
        observer,
        instrument,
        options,
        EntryEffects::Handled,
        DefinitionLinkage::Object,
    )
}
fn compile_jit(
    m: &mut JITModule,
    world: &World,
    entry: &str,
    optimize: bool,
    observer: Option<&mut dyn FnMut(EmittedFunction)>,
    instrument: Option<Instrument<'_>>,
    options: CodegenOptions,
) -> Result<(BTreeMap<String, FuncId>, String), String> {
    compile_entry(
        m,
        world,
        entry,
        optimize,
        observer,
        instrument,
        options,
        EntryEffects::Handled,
        DefinitionLinkage::Jit,
    )
}
/// [`compile`]; `entry_effects` controls whether an entry may leave `Fail`
/// (`FailOnly`, lane 27), closed built-in capability effects or `Fail`
/// (`CapabilityOrFail`, for offline verification and WCSD stack measurement),
/// or the built-in Beacon `Trace` effect (`TraceHost`) unhandled at the entry
/// boundary.
#[allow(clippy::too_many_arguments)]
fn compile_entry<M: Module>(
    m: &mut M,
    world: &World,
    entry: &str,
    optimize: bool,
    observer: Option<&mut dyn FnMut(EmittedFunction)>,
    instrument: Option<Instrument<'_>>,
    options: CodegenOptions,
    entry_effects: EntryEffects,
    linkage: DefinitionLinkage,
) -> Result<(BTreeMap<String, FuncId>, String), String> {
    let entry_fn = world.functions.get(entry).ok_or("unknown entry")?;
    // Foreign capabilities are granted by the package's `.vsys` import.
    let trace_ok = entry_effects == EntryEffects::TraceHost
        && crate::trace_effects::active(world)
        && entry_fn.effects.tail.is_none()
        && entry_fn.effects.instances.iter().any(|e| {
            e.name == crate::trace_effects::TRACE && e.label.is_none() && e.args.is_empty()
        })
        && entry_fn.effects.instances.iter().all(|e| {
            crate::ffi_lowering::is_foreign_effect(&e.name)
                || (e.name == crate::trace_effects::TRACE && e.label.is_none() && e.args.is_empty())
        });
    if (entry_fn
        .effects
        .instances
        .iter()
        .any(|e| !crate::ffi_lowering::is_foreign_effect(&e.name))
        || entry_fn.effects.tail.is_some())
        && !((entry_effects == EntryEffects::FailOnly
            || entry_effects == EntryEffects::CapabilityOrFail)
            && fail_only(world, entry_fn))
        && !(entry_effects == EntryEffects::CapabilityOrFail
            && entry_fn.effects.tail.is_none()
            && capability_only(entry_fn))
        && !trace_ok
    {
        return Err("native entry must handle all effects".into());
    }
    if contains_function(world, &entry_fn.result)
        || entry_fn
            .params
            .iter()
            .any(|p| contains_function(world, &p.ty))
    {
        return Err("native entry cannot expose specialized closure values; invoke them within the entry function".into());
    }
    let instances = instances_for_entry(world, entry, trace_ok)?;
    let mut program = crate::ir::lower_world(world);
    crate::integer_widths::lower_bit_builtin_calls(world, &mut program);
    let mut inlined_into = BTreeMap::new();
    if optimize {
        crate::ir_opt::optimize(world, &mut program)?;
        // Executable objects inline too (#959). The decision depends only on
        // the world, never on the observer or instrument hooks, so a
        // verified build stays byte-identical to the plain one and the
        // verifiers check the code that ships.
        inlined_into =
            crate::ir_inline::inline(world, &mut program, &crate::ir_inline::Budget::default())
                .inlined_into;
    } else {
        crate::ir_opt::fip_reuse_pass(world, &mut program)?;
    }
    crate::fip::elaborate_non_fip_drops(world, &mut program);
    if optimize {
        // Native only, never the verified backend (Stage 1 perf lane 3).
        crate::bounds_hoist::run(&mut program);
    }
    let abi = NativeAbi::plan(world, &instances, optimize, |key| key == entry)?;
    let (ids, trace_entry_id) =
        declare_instances_with_trace(m, world, entry, &instances, linkage, trace_ok, &abi)?;
    let trace_entry = trace_entry_id.map(|id| (entry, id));
    // Observers learn which bodies each function holds (inline provenance).
    let observed = observer.is_some();
    let mut observer = observer;
    let mut with_provenance = |mut f: EmittedFunction| {
        if let Some(into) = inlined_into
            .get(&f.key)
            .or_else(|| inlined_into.get(&f.source))
        {
            f.inlined = into.clone();
        }
        if let Some(observe) = observer.as_mut() {
            observe(f);
        }
    };
    let observer: Option<&mut dyn FnMut(EmittedFunction)> = if observed {
        Some(&mut with_provenance)
    } else {
        None
    };
    let clif = define_instances_observed_with_trace(
        m,
        world,
        &instances,
        &ids,
        &program,
        observer,
        instrument,
        options,
        linkage,
        trace_ok,
        trace_entry,
        optimize,
        &abi,
    )?;
    let mut abi = Vec::new();
    for (key, (name, vars)) in &instances {
        if capability_abi(world, &concrete(world, name, vars)?) {
            let id = ids[key];
            abi.push(
                m.declarations()
                    .get_function_decl(id)
                    .linkage_name(id)
                    .into_owned(),
            );
        }
    }
    if !abi.is_empty() {
        let mut bytes = format!("{CAPABILITY_ABI}\n").into_bytes();
        for symbol in abi {
            bytes.extend_from_slice(symbol.as_bytes());
            bytes.push(b'\n');
        }
        let data = m
            .declare_data(CAPABILITY_ABI_SYMBOL, Linkage::Local, false, false)
            .map_err(|e| e.to_string())?;
        let mut description = cranelift_module::DataDescription::new();
        description.define(bytes.into_boxed_slice());
        m.define_data(data, &description)
            .map_err(|e| e.to_string())?;
    }
    Ok((ids, clif))
}
/// Lane-23 instrumentation hook ([`crate::native_coverage`]): rewrites each
/// finalized function, by instance key, before the CLIF text, the observer
/// and machine-code generation see it. Only explicit coverage entry points
/// pass one; every other build passes `None` and emits unchanged code.
pub type Instrument<'a> = &'a mut dyn FnMut(&str, &mut ir::Function) -> Result<(), String>;
/// One Cranelift function emitted by this backend, handed to the
/// integration-B verifier hook ([`crate::native_verifiers`]) after the body
/// is finalized and before machine code is generated from it. The hook only
/// observes a clone: compilation and the emitted code are unchanged.
///
/// `function.name` is set to `u0:<func_id>`, the name direct calls between
/// emitted functions use, so call graphs resolve across the set.
#[derive(Clone)]
pub struct EmittedFunction {
    /// Monomorphized instance key (the source name for non-generic functions).
    pub key: String,
    /// Source function name of the instance.
    pub source: String,
    /// Object/JIT symbol (`entry` keeps its name, others are local).
    pub symbol: String,
    /// `cranelift_module::FuncId` index, the `u0:<index>` user name.
    pub func_id: u32,
    pub function: ir::Function,
    /// `(func_id, symbol)` of every function this one references, defined
    /// or imported (for example `fmod`).
    pub callees: Vec<(u32, String)>,
    /// Generic substitution of this instance (empty for a non-generic
    /// function). [`EmittedFunction::instance`] applies it.
    pub vars: BTreeMap<String, Type>,
    /// The build ran the Place IR optimizer, so hosted objects compile this
    /// function at Cranelift `opt_level=speed` ([`object_isa_optimized`]).
    /// The stack verifier recompiles with the same ISA (#959).
    pub optimized: bool,
    /// Functions whose bodies the inliner copied into this one, directly or
    /// through other inlined callees ([`crate::ir_inline::Report`]). Policy
    /// facts declared for them apply to this function too (#959).
    pub inlined: std::collections::BTreeSet<String>,
}
impl EmittedFunction {
    /// The source function with this instance's generic arguments
    /// substituted into its parameters, result and effects: the concrete
    /// types whose [`Layout`]s the emitted signature was built from.
    pub fn instance(&self, world: &World) -> Result<Function, String> {
        concrete(world, &self.source, &self.vars)
    }
    /// Source byte offset of instruction `inst` ([`source_offset`]).
    pub fn source_offset(&self, inst: ir::Inst) -> Option<usize> {
        source_offset(&self.function, inst)
    }
}
#[derive(Clone, Copy)]
enum DefinitionLinkage {
    Object,
    Jit,
}
impl DefinitionLinkage {
    /// JIT code, generated helper bodies and data need not share an allocation.
    /// Apply after instrumentation and before observers or machine-code lowering.
    fn normalize_references(self, function: &mut ir::Function) {
        if matches!(self, Self::Object) {
            return;
        }
        for target in function.dfg.ext_funcs.values_mut() {
            target.colocated = false;
        }
        for global in function.global_values.values_mut() {
            if let ir::GlobalValueData::Symbol { colocated, .. } = global {
                *colocated = false;
            }
        }
    }
    fn function(self, entry: bool) -> Linkage {
        match self {
            Self::Object if entry => Linkage::Export,
            Self::Object => Linkage::Local,
            // JIT allocations need not be within a signed 32-bit displacement
            // of one another. A final linkage makes Cranelift assume they are
            // colocated; Preemptible is definable without that assumption.
            // JITModule resolves our defined functions to their compiled ptrs.
            Self::Jit => Linkage::Preemptible,
        }
    }
}
/// Declare every monomorphized ordinary function. Object entries are exported
/// under their source name; JIT definitions use distance-independent calls.
fn declare_instances<M: Module>(
    m: &mut M,
    world: &World,
    entry: &str,
    instances: &InstanceMap,
    linkage: DefinitionLinkage,
) -> Result<BTreeMap<String, FuncId>, String> {
    let (ids, _) =
        declare_instances_with_trace(m, world, entry, instances, linkage, false, &DEFAULT_ABI)?;
    Ok(ids)
}
fn declare_instances_with_trace<M: Module>(
    m: &mut M,
    world: &World,
    entry: &str,
    instances: &InstanceMap,
    linkage: DefinitionLinkage,
    trace_host: bool,
    abi: &NativeAbi,
) -> Result<(BTreeMap<String, FuncId>, Option<FuncId>), String> {
    let mut ids = BTreeMap::new();
    let mut trace_entry_id = None;
    for (i, (key, (name, vars))) in instances.iter().enumerate() {
        let f = concrete(world, name, vars)?;
        if trace_host && key == entry {
            let public_id = m
                .declare_function(entry, linkage.function(true), &signature(m, world, &f)?)
                .map_err(|e| e.to_string())?;
            let internal_id = m
                .declare_function(
                    &format!("__vesper_trace_entry_{i}"),
                    linkage.function(false),
                    &signature_with_trace(m, world, &f, true)?,
                )
                .map_err(|e| e.to_string())?;
            ids.insert(key.clone(), public_id);
            trace_entry_id = Some(internal_id);
        } else {
            let pass_trace = uses_trace_fail(world, trace_host, key, entry, &f);
            let symbol = if key == entry {
                entry.to_owned()
            } else {
                format!("vesper_function_{i}")
            };
            let id = m
                .declare_function(
                    &symbol,
                    linkage.function(key == entry),
                    &signature_with_abi(m, world, &f, pass_trace, abi.of(key))?,
                )
                .map_err(|e| e.to_string())?;
            ids.insert(key.clone(), id);
        }
    }
    Ok((ids, trace_entry_id))
}
/// Define the bodies of the instances declared by [`declare_instances`].
fn define_instances<M: Module>(
    m: &mut M,
    world: &World,
    instances: &InstanceMap,
    ids: &BTreeMap<String, FuncId>,
    program: &crate::ir::Program,
    linkage: DefinitionLinkage,
    optimize: bool,
) -> Result<String, String> {
    define_instances_observed_with_trace(
        m,
        world,
        instances,
        ids,
        program,
        None,
        None,
        CodegenOptions::default(),
        linkage,
        false,
        None,
        optimize,
        &DEFAULT_ABI,
    )
}
#[allow(clippy::too_many_arguments)]
fn define_instances_observed_with_trace<M: Module>(
    m: &mut M,
    world: &World,
    instances: &InstanceMap,
    ids: &BTreeMap<String, FuncId>,
    program: &crate::ir::Program,
    mut observer: Option<&mut dyn FnMut(EmittedFunction)>,
    mut instrument: Option<Instrument<'_>>,
    options: CodegenOptions,
    linkage: DefinitionLinkage,
    trace_host: bool,
    trace_entry: Option<(&str, FuncId)>,
    operands_only: bool,
    abi: &NativeAbi,
) -> Result<String, String> {
    let mut clif = String::new();
    let denorm = crate::capability_effects::const_denorm_spans(world);
    for (key, (name, generic_vars)) in instances {
        let f = concrete(world, name, generic_vars)?;
        let entry_name = trace_entry.map(|(e, _)| e).unwrap_or("");
        let pass_trace = uses_trace_fail(world, trace_host, key, entry_name, &f);
        let define_id = if let Some((entry_name, internal_id)) = trace_entry
            && key == entry_name
        {
            internal_id
        } else {
            ids[key]
        };
        let mut ctx = m.make_context();
        let own_abi = abi.of(key);
        ctx.func.signature = signature_with_abi(m, world, &f, pass_trace, own_abi)?;
        let mut fbctx = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let mut values = b.block_params(entry).to_vec();
            let trace_fail = if pass_trace { values.pop() } else { None };
            let fail_record = if fail_only(world, &f) {
                values.pop()
            } else {
                None
            };
            let caller_slot = if crate::fip::uses_caller_slot(world, &f) {
                values.pop()
            } else {
                None
            };
            let result_layout = Layout::of(world, &f.result)?;
            let own_abi = abi.of(key);
            let sret = if let Some(words) = own_abi.result_words {
                // Register result: build the value in a local buffer and
                // return its words.
                let slot = b.create_sized_stack_slot(ir::StackSlotData::new(
                    ir::StackSlotKind::ExplicitSlot,
                    words * 8,
                    3,
                ));
                Some(
                    b.ins()
                        .stack_addr(m.target_config().pointer_type(), slot, 0),
                )
            } else if result_layout.aggregate() {
                Some(values.remove(0))
            } else {
                None
            };
            let mut vars = BTreeMap::new();
            let mut local_types = BTreeMap::new();
            let mut pointers = BTreeMap::new();
            for (p, v) in f.params.iter().zip(values) {
                let pointer = v;
                let layout = Layout::of(world, &p.ty)?;
                let value = if p.pass == Pass::Inout && !layout.aggregate() {
                    b.ins()
                        .load(scalar(&p.ty, &f.generics)?, ir::MemFlags::new(), pointer, 0)
                } else {
                    v
                };
                let var = b.declare_var(b.func.dfg.value_type(value));
                b.def_var(var, value);
                vars.insert(p.name.clone(), var);
                local_types.insert(p.name.clone(), p.ty.clone());
                if p.pass == Pass::Inout && !layout.aggregate() {
                    let address = b.declare_var(b.func.dfg.value_type(pointer));
                    b.def_var(address, pointer);
                    pointers.insert(var.as_u32(), address);
                }
            }
            let mut c = Codegen {
                b,
                m,
                world,
                ids,
                program,
                capabilities: BTreeMap::new(),
                next_capability: 0,
                closures: vec![],
                infos: BTreeMap::new(),
                variable_infos: BTreeMap::new(),
                pointer_parents: BTreeMap::new(),
                vars,
                local_types,
                generic_vars: generic_vars.clone(),
                pointers,
                handlers: vec![],
                exits: vec![],
                inline_depth: 0,
                fail_record: fail_record.map(|record| {
                    (
                        record,
                        fail_instances(&f).map(|e| e.args[0].clone()).collect(),
                    )
                }),
                caller_slot,
                reuse_slots: BTreeMap::new(),
                register_locals: BTreeSet::new(),
                current_call_slot: None,
                in_fip_fn: f.fip,
                trace_fail,
                trace_entry: trace_entry.map(|(e, id)| (e.to_owned(), id)),
                session: None,
                denorm: &denorm,
                source_locations: options.source_locations,
                srcloc: ir::SourceLoc::default(),
                operands_only,
                abi,
                own_abi,
                result_buffer: own_abi.result_words.and(sret),
                forced_call_vars: None,
            };
            c.set_location(f.span.start);
            // Function-value parameters bound to top-level functions by
            // this instance: bind each to that function's tag, as an
            // inlining caller would have.
            for (param, function) in function_arguments(generic_vars) {
                let ty = world.functions[name]
                    .params
                    .iter()
                    .find(|p| p.name == param)
                    .map(|p| substitute(&p.ty, generic_vars))
                    .ok_or("function-value instance names an unknown parameter")?;
                let tag = c.function_tag(&function, &ty)?;
                let var = c.b.declare_var(c.b.func.dfg.value_type(tag));
                c.def_var(var, tag);
                c.vars.insert(param.clone(), var);
                c.local_types.insert(param, ty);
            }
            if fail_record.is_some() {
                let mut fails = 0;
                for (index, effect) in f.effects.instances.iter().enumerate() {
                    if effect.name != "Fail" {
                        // A capability effect: no operations, no evidence.
                        continue;
                    }
                    let identity = format!("native-fail-exit:{fails}");
                    fails += 1;
                    c.capabilities
                        .insert(format!("$effect:{}", effect.name), identity.clone());
                    c.capabilities
                        .insert(format!("$static:$fncap:{name}:{index}"), identity.clone());
                    if let Some(key) = crate::numeric::instance_key(&effect.name, &effect.args) {
                        c.capabilities.insert(key, identity);
                    }
                }
            }
            if trace_host && crate::trace_effects::active(world) {
                for (index, effect) in f.effects.instances.iter().enumerate() {
                    if effect.name == crate::trace_effects::TRACE
                        && effect.label.is_none()
                        && effect.args.is_empty()
                    {
                        let identity = "native-trace-host".to_string();
                        c.capabilities
                            .insert(format!("$effect:{}", effect.name), identity.clone());
                        c.capabilities
                            .insert(format!("$static:$fncap:{name}:{index}"), identity.clone());
                    }
                }
            }
            c.ir_function(
                program
                    .functions
                    .get(name)
                    .ok_or("missing lowered native function")?,
                sret,
            )?;
            c.b.seal_all_blocks();
            c.b.finalize();
        }
        if let Some(rewrite) = instrument.as_mut() {
            rewrite(key, &mut ctx.func)?;
        }
        linkage.normalize_references(&mut ctx.func);
        clif.push_str(&format!("; {key}\n{}\n", ctx.func.display()));
        if let Some(observe) = observer.as_mut() {
            let mut function = ctx.func.clone();
            function.name = ir::UserFuncName::user(0, define_id.as_u32());
            let declarations = m.declarations();
            let callees = function
                .params
                .user_named_funcs()
                .values()
                .filter(|name| name.namespace == 0)
                .map(|name| {
                    let callee = FuncId::from_u32(name.index);
                    (
                        name.index,
                        declarations
                            .get_function_decl(callee)
                            .linkage_name(callee)
                            .into_owned(),
                    )
                })
                .collect();
            observe(EmittedFunction {
                key: key.clone(),
                source: name.clone(),
                symbol: m
                    .declarations()
                    .get_function_decl(define_id)
                    .linkage_name(define_id)
                    .into_owned(),
                func_id: define_id.as_u32(),
                function,
                callees,
                vars: generic_vars.clone(),
                optimized: operands_only,
                inlined: Default::default(),
            });
        }
        m.define_function(define_id, &mut ctx)
            .map_err(|e| format!("native codegen '{name}': {e}"))?;
        m.clear_context(&mut ctx);
    }
    if let Some((entry_name, internal_id)) = trace_entry {
        let f = world.functions.get(entry_name).ok_or("unknown entry")?;
        let public_id = ids[entry_name];
        let mut ctx = m.make_context();
        ctx.func.signature = signature(m, world, f)?;
        let mut fbctx = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
            let block = b.create_block();
            b.append_block_params_for_function_params(block);
            b.switch_to_block(block);
            let mut args = b.block_params(block).to_vec();
            let slot = b.create_sized_stack_slot(ir::StackSlotData::new(
                ir::StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let zero = b.ins().iconst(types::I64, 0);
            b.ins().stack_store(zero, slot, 0);
            let flag = b
                .ins()
                .stack_addr(m.target_config().pointer_type(), slot, 0);
            args.push(flag);
            let callee = m.declare_func_in_func(internal_id, b.func);
            let call = b.ins().call(callee, &args);
            let result = b.inst_results(call)[0];
            b.ins().return_(&[result]);
            b.seal_all_blocks();
            b.finalize();
        }
        linkage.normalize_references(&mut ctx.func);
        clif.push_str(&format!(
            "; {entry_name} (trace wrapper)\n{}\n",
            ctx.func.display()
        ));
        m.define_function(public_id, &mut ctx)
            .map_err(|e| format!("native codegen '{entry_name}': {e}"))?;
        m.clear_context(&mut ctx);
    }
    Ok(clif)
}
pub fn jit(world: &World, entry: &str) -> Result<crate::runtime::Value, String> {
    jit_with_options(world, entry, true)
}
pub fn jit_with_options(
    world: &World,
    entry: &str,
    optimize: bool,
) -> Result<crate::runtime::Value, String> {
    jit_run_entry(world, entry, optimize, EntryEffects::Handled)
}
/// [`jit_with_options`] for an entry carrying `eff [Trace]` lowered to the
/// Beacon native host (`vesper_trace_*` runtime symbols).
pub fn jit_traced_with_options(
    world: &World,
    entry: &str,
    optimize: bool,
) -> Result<crate::runtime::Value, String> {
    jit_run_entry(world, entry, optimize, EntryEffects::TraceHost)
}
/// A JIT-compiled function whose parameters and result are each one
/// machine word (`in` aggregates are their addresses), for host code that
/// calls compiled Vesper from several threads at once (source atomics,
/// [`crate::atomics`]). The module owns the code; the function pointer is
/// valid while this value lives.
pub struct WordFunction {
    _module: JITModule,
    code: usize,
    arity: usize,
}
// SAFETY: finalized JIT code is immutable and the module is only dropped
// with the value; calling the code needs no access to the module.
unsafe impl Sync for WordFunction {}
// SAFETY: as above; nothing in the module is tied to the creating thread.
unsafe impl Send for WordFunction {}
impl WordFunction {
    /// Compiles `entry` of `world` (optimized when `optimize`). Every
    /// parameter and the result must have an eight-byte native lane or be
    /// an `in` aggregate, and the row must be empty.
    pub fn compile(world: &World, entry: &str, optimize: bool) -> Result<Self, String> {
        let f = world.functions.get(entry).ok_or("unknown entry")?;
        if !f.effects.instances.is_empty() || f.effects.tail.is_some() || f.params.len() > 3 {
            return Err("a word function takes at most three words and has no effects".into());
        }
        if Layout::of(world, &f.result)?.aggregate() {
            return Err("a word function returns one word".into());
        }
        let mut flags = settings::builder();
        flags
            .set("use_colocated_libcalls", "false")
            .map_err(|e| e.to_string())?;
        flags.set("is_pic", "false").map_err(|e| e.to_string())?;
        if optimize {
            flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
        }
        let isa = cranelift_native::builder()
            .map_err(str::to_string)?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        register_jit_runtime_symbols(&mut builder, false);
        let mut m = JITModule::new(builder);
        let (ids, _) = compile_entry(
            &mut m,
            world,
            entry,
            optimize,
            None,
            None,
            CodegenOptions::default(),
            EntryEffects::Handled,
            DefinitionLinkage::Jit,
        )?;
        let sig = signature(&m, world, f)?;
        if sig.params.len() != f.params.len()
            || sig.returns.len() != 1
            || sig
                .params
                .iter()
                .chain(&sig.returns)
                .any(|p| p.value_type != types::I64)
            || sig.call_conv != m.target_config().default_call_conv
        {
            return Err("word function ABI mismatch".into());
        }
        m.finalize_definitions().map_err(|e| e.to_string())?;
        let code = m.get_finalized_function(ids[entry]) as usize;
        Ok(Self {
            _module: m,
            code,
            arity: f.params.len(),
        })
    }
    /// Calls the function with `args` (one word per parameter).
    ///
    /// # Safety
    ///
    /// Every word must be a valid value of its parameter: an `in` aggregate
    /// is the address of a live value in the native layout, which the call
    /// only reads (atomic cells are updated through their shared element).
    pub unsafe fn call(&self, args: &[u64]) -> Result<u64, String> {
        if args.len() != self.arity {
            return Err("word function arity mismatch".into());
        }
        // SAFETY: compile() checked a host-ABI signature of `arity` words
        // returning one word; the caller vouches for the arguments.
        unsafe {
            let code = self.code as *const u8;
            Ok(match *args {
                [] => std::mem::transmute::<*const u8, extern "C" fn() -> u64>(code)(),
                [a] => std::mem::transmute::<*const u8, extern "C" fn(u64) -> u64>(code)(a),
                [a, b] => {
                    std::mem::transmute::<*const u8, extern "C" fn(u64, u64) -> u64>(code)(a, b)
                }
                [a, b, c] => std::mem::transmute::<*const u8, extern "C" fn(u64, u64, u64) -> u64>(
                    code,
                )(a, b, c),
                _ => unreachable!("arity checked at compile"),
            })
        }
    }
}
pub(crate) fn register_jit_runtime_symbols(builder: &mut JITBuilder, trace_host: bool) {
    builder.symbol("fmod", native_float_remainder as *const u8);
    builder.symbol("fmodf", float_math::native_float_remainder_f32 as *const u8);
    builder.symbols(crate::regions::native_runtime::symbols());
    if trace_host {
        builder.symbols(crate::trace_effects::native_runtime::symbols());
    }
    builder.symbols(crate::tiled::native_runtime::symbols());
    builder.symbols(crate::seq::native_runtime::symbols());
    builder.symbols(crate::fip::native_runtime::symbols());
    builder.symbols(crate::atomics::workers::symbols());
    builder.symbols(crate::ffi_lowering::jit_symbols());
}
fn jit_run_entry(
    world: &World,
    entry: &str,
    optimize: bool,
    entry_effects: EntryEffects,
) -> Result<crate::runtime::Value, String> {
    let f = world.functions.get(entry).ok_or("missing entry function")?;
    if !f.params.is_empty() {
        return Err("JIT entry must take no arguments".into());
    }
    if crate::seq::holds_sequence(world, &f.result) {
        return Err(format!(
            "JIT entry '{entry}' cannot return a ChunkSeq: its sequences are freed when the entry returns"
        ));
    }
    let mut flags = settings::builder();
    flags
        .set("use_colocated_libcalls", "false")
        .map_err(|e| e.to_string())?;
    flags.set("is_pic", "false").map_err(|e| e.to_string())?;
    // Optimized builds run Cranelift's egraph optimizer (#931); unoptimized
    // builds keep the default `none`.
    if optimize {
        flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
    }
    if needs_nan_canonicalization(world) {
        flags
            .set("enable_nan_canonicalization", "true")
            .map_err(|e| e.to_string())?;
    }
    let isa = cranelift_native::builder()
        .map_err(str::to_string)?
        .finish(settings::Flags::new(flags))
        .map_err(|e| e.to_string())?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    register_jit_runtime_symbols(&mut builder, entry_effects == EntryEffects::TraceHost);
    let mut m = JITModule::new(builder);
    let (ids, _) = compile_entry(
        &mut m,
        world,
        entry,
        optimize,
        None,
        None,
        CodegenOptions::default(),
        entry_effects,
        DefinitionLinkage::Jit,
    )?;
    m.finalize_definitions().map_err(|e| e.to_string())?;
    let code = m.get_finalized_function(ids[entry]);
    // SAFETY: compile() verified the signature and entry arity above. The module
    // owns executable code and remains alive through this call.
    let value = unsafe {
        if Layout::of(world, &f.result)?.aggregate() {
            let layout = Layout::of(world, &f.result)?;
            let mut result = vec![0u64; layout.size as usize / 8];
            let call: unsafe extern "C" fn(*mut u64) -> i64 = std::mem::transmute(code);
            call(result.as_mut_ptr());
            if entry_effects == EntryEffects::TraceHost
                && crate::trace_effects::native_runtime::has_failed()
            {
                crate::runtime::Value::Tuple(vec![])
            } else {
                crate::layout::decode(
                    world,
                    &f.result,
                    std::slice::from_raw_parts(result.as_ptr().cast::<u8>(), layout.size as usize),
                )?
            }
        } else if f.result.name() == "f64" {
            let call: unsafe extern "C" fn() -> f64 = std::mem::transmute(code);
            crate::runtime::Value::Float(call())
        } else if f.result.name() == "f32" {
            let call: unsafe extern "C" fn() -> f32 = std::mem::transmute(code);
            crate::runtime::Value::Float32(call())
        } else {
            let n = if crate::fip::uses_caller_slot(world, f) {
                let inner_size = crate::fip::box_inner(world, &f.result)
                    .and_then(|t| Layout::of(world, t).ok())
                    .map_or(8u64, |l| u64::from(l.size));
                let slot = crate::fip::native_runtime::vesper_box_alloc(inner_size);
                let call: unsafe extern "C" fn(u64) -> i64 = std::mem::transmute(code);
                call(slot)
            } else {
                let call: unsafe extern "C" fn() -> i64 = std::mem::transmute(code);
                call()
            };
            if crate::fip::is_box_type(world, &f.result) {
                crate::layout::decode(world, &f.result, &n.to_le_bytes())?
            } else if f.result.name() == "bool" {
                crate::runtime::Value::Bool(n != 0)
            } else if f.result == Type::unit() || f.result.name() == "Unit" {
                crate::runtime::Value::Tuple(vec![])
            } else {
                if let Some(k) = crate::numeric::IntegerKind::from_type(&f.result) {
                    crate::runtime::integer_from_word(k, n as u64)
                } else {
                    crate::runtime::Value::Int(n)
                }
            }
        }
    };
    let sequences = crate::seq::native_runtime::finish();
    crate::regions::native_runtime::finish().map_err(|e| format!("native region fault: {e}"))?;
    crate::tiled::native_runtime::finish().map_err(|e| format!("native tiled fault: {e}"))?;
    crate::fip::native_runtime::finish();
    sequences?;
    Ok(value)
}
pub fn object(world: &World, entry: &str) -> Result<(Vec<u8>, String), String> {
    object_for_target(world, entry, None)
}
pub fn object_for_target(
    world: &World,
    entry: &str,
    target: Option<&str>,
) -> Result<(Vec<u8>, String), String> {
    object_with_options(world, entry, target, true)
}
pub fn object_with_options(
    world: &World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
) -> Result<(Vec<u8>, String), String> {
    object_with_observer(world, entry, target, optimize, None)
}
/// The ISA (target and codegen flags) object emission compiles with. The
/// stack verifier measures frames with exactly this ISA.
pub fn object_isa(
    target: Option<&str>,
) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>, String> {
    isa_with_flags(target, false, false)
}
/// [`object_isa`] for `world`: NaN canonicalisation is enabled when the
/// program has a DetFloat function ([`needs_nan_canonicalization`]).
pub fn object_isa_for(
    world: &World,
    target: Option<&str>,
) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>, String> {
    isa_with_flags(target, needs_nan_canonicalization(world), false)
}
/// [`object_isa_for`] for an optimized build: Cranelift runs at
/// `opt_level=speed` when `optimize` is on, for library objects
/// ([`export::object`], #931) and executable objects (#959). The stack
/// verifier recompiles emitted functions with this ISA.
pub fn object_isa_optimized(
    world: &World,
    target: Option<&str>,
    optimize: bool,
) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>, String> {
    isa_with_flags(target, needs_nan_canonicalization(world), optimize)
}
fn isa_with_flags(
    target: Option<&str>,
    canonicalize_nans: bool,
    speed: bool,
) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>, String> {
    let mut flags = settings::builder();
    flags.set("is_pic", "true").map_err(|e| e.to_string())?;
    if speed {
        flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
    }
    if canonicalize_nans {
        flags
            .set("enable_nan_canonicalization", "true")
            .map_err(|e| e.to_string())?;
    }
    let builder = if let Some(target) = target {
        let triple: target_lexicon::Triple =
            target.parse().map_err(|e| format!("invalid target: {e}"))?;
        if !matches!(
            triple.architecture,
            target_lexicon::Architecture::X86_64 | target_lexicon::Architecture::Aarch64(_)
        ) {
            return Err("native targets must be x86_64 or AArch64".into());
        }
        cranelift_codegen::isa::lookup(triple).map_err(|e| e.to_string())?
    } else {
        cranelift_native::builder().map_err(str::to_string)?
    };
    let isa = builder
        .finish(settings::Flags::new(flags))
        .map_err(|e| e.to_string())?;
    Ok(isa)
}
/// [`object_with_options`] that also hands every emitted Cranelift function
/// to `observer` (the integration-B verifier hook). With `None` it is
/// exactly `object_with_options`.
pub fn object_with_observer(
    world: &World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
    observer: Option<&mut dyn FnMut(EmittedFunction)>,
) -> Result<(Vec<u8>, String), String> {
    object_instrumented(world, entry, target, optimize, observer, None)
}
/// Lane 27: [`object_with_observer`] with a caller-built ISA, so
/// [`freestanding`] can emit `riscv64imac-unknown-none-elf` and
/// `aarch64-unknown-none` objects without widening [`object_isa`]'s hosted
/// target check. The caller owns every flag of `isa`. An entry whose only
/// effects are `Fail` instances is accepted and takes the hidden
/// failure-record pointer as its last parameter.
pub fn object_for_isa(
    world: &World,
    entry: &str,
    isa: std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>,
    optimize: bool,
    observer: Option<&mut dyn FnMut(EmittedFunction)>,
) -> Result<(Vec<u8>, String), String> {
    let builder = ObjectBuilder::new(isa, "vesper", cranelift_module::default_libcall_names())
        .map_err(|e| e.to_string())?;
    let mut m = ObjectModule::new(builder);
    let (_, clif) = compile_entry(
        &mut m,
        world,
        entry,
        optimize,
        observer,
        None,
        CodegenOptions::default(),
        EntryEffects::FailOnly,
        DefinitionLinkage::Object,
    )?;
    let bytes = m.finish().emit().map_err(|e| e.to_string())?;
    Ok((bytes, clif))
}
/// [`object_with_options`] for an entry carrying `eff [Trace]` lowered to
/// `vesper_trace_*` runtime imports.
pub fn object_traced_with_options(
    world: &World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
) -> Result<(Vec<u8>, String), String> {
    let isa = object_isa_optimized(world, target, optimize)?;
    let builder = ObjectBuilder::new(isa, "vesper", cranelift_module::default_libcall_names())
        .map_err(|e| e.to_string())?;
    let mut m = ObjectModule::new(builder);
    let (_, clif) = compile_entry(
        &mut m,
        world,
        entry,
        optimize,
        None,
        None,
        CodegenOptions::default(),
        EntryEffects::TraceHost,
        DefinitionLinkage::Object,
    )?;
    let bytes = m.finish().emit().map_err(|e| e.to_string())?;
    Ok((bytes, clif))
}
/// Compile a standalone entry whose effects are closed built-in capabilities
/// (or `Fail` + capabilities) and emit its finalized Cranelift functions to
/// `observer` without linking/finishing an executable object.
pub fn emit_capability_entry(
    world: &World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
    observer: &mut dyn FnMut(EmittedFunction),
) -> Result<(), String> {
    let isa = object_isa_optimized(world, target, optimize)?;
    let builder = ObjectBuilder::new(isa, "vesper", cranelift_module::default_libcall_names())
        .map_err(|e| e.to_string())?;
    let mut m = ObjectModule::new(builder);
    let _ = compile_entry(
        &mut m,
        world,
        entry,
        optimize,
        Some(observer),
        None,
        CodegenOptions::default(),
        EntryEffects::CapabilityOrFail,
        DefinitionLinkage::Object,
    )?;
    Ok(())
}
/// [`object_with_observer`] with the lane-23 [`Instrument`] hook. With
/// `instrument: None` it is exactly `object_with_observer`.
pub fn object_instrumented(
    world: &World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
    observer: Option<&mut dyn FnMut(EmittedFunction)>,
    instrument: Option<Instrument<'_>>,
) -> Result<(Vec<u8>, String), String> {
    object_with_codegen_options(
        world,
        entry,
        target,
        optimize,
        observer,
        instrument,
        CodegenOptions::default(),
    )
}
/// [`object_instrumented`] with explicit [`CodegenOptions`]. The default
/// options are what every other entry point uses; turning
/// `source_locations` off is for the identity test that shows locations do
/// not change object bytes.
pub fn object_with_codegen_options(
    world: &World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
    observer: Option<&mut dyn FnMut(EmittedFunction)>,
    instrument: Option<Instrument<'_>>,
    options: CodegenOptions,
) -> Result<(Vec<u8>, String), String> {
    let isa = object_isa_optimized(world, target, optimize)?;
    if windows::is_windows(isa.triple()) {
        return windows::object(world, entry, isa, optimize, observer, instrument, options);
    }
    let builder = ObjectBuilder::new(isa, "vesper", cranelift_module::default_libcall_names())
        .map_err(|e| e.to_string())?;
    let mut m = ObjectModule::new(builder);
    let (_, clif) = compile(
        &mut m, world, entry, optimize, observer, instrument, options,
    )?;
    let bytes = m.finish().emit().map_err(|e| e.to_string())?;
    Ok((bytes, clif))
}
/// A JIT module compiled once with the lane-23 [`Instrument`] hook, whose
/// zero-argument `i64` entry can be called repeatedly (`vesper fuzz
/// --native-coverage`). Code pointers never escape; calls borrow the owner
/// exclusively and dropping it frees the module after every call returned.
///
/// Crate-private: the hook can embed arbitrary addresses, so only
/// [`crate::native_coverage::NativeCoverage`], which owns the buffer the
/// instrumentation writes and drops it after this owner, constructs one.
pub(crate) struct InstrumentedJit {
    module: Option<JITModule>,
    code: *const u8,
    _single_thread: std::marker::PhantomData<Rc<()>>,
}
impl InstrumentedJit {
    /// Compile `entry` (no parameters, `i64` result, every effect handled)
    /// and everything it reaches, unoptimized at the IR level, rewriting
    /// each function through `instrument`.
    ///
    /// # Safety
    ///
    /// Every memory access `instrument` inserts must target memory that is
    /// valid for that access for as long as the returned owner can be
    /// [`run`](Self::run), and must not alias Rust data except through
    /// atomics or interior mutability. Cranelift's verifier does not check
    /// embedded addresses.
    pub(crate) unsafe fn compile(
        world: &World,
        entry: &str,
        instrument: Instrument<'_>,
    ) -> Result<Self, String> {
        let f = world.functions.get(entry).ok_or("missing entry function")?;
        if !f.params.is_empty() || f.result.name() != "i64" {
            return Err("instrumented JIT entry must be `fn() -> i64`".into());
        }
        let mut flags = settings::builder();
        flags
            .set("use_colocated_libcalls", "false")
            .map_err(|e| e.to_string())?;
        flags.set("is_pic", "false").map_err(|e| e.to_string())?;
        if needs_nan_canonicalization(world) {
            flags
                .set("enable_nan_canonicalization", "true")
                .map_err(|e| e.to_string())?;
        }
        let isa = cranelift_native::builder()
            .map_err(str::to_string)?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        register_jit_runtime_symbols(&mut builder, false);
        let mut owned = Self {
            module: Some(JITModule::new(builder)),
            code: std::ptr::null(),
            _single_thread: std::marker::PhantomData,
        };
        let m = owned.module.as_mut().expect("owned JIT module");
        let (ids, _) = compile_jit(
            m,
            world,
            entry,
            false,
            None,
            Some(instrument),
            CodegenOptions::default(),
        )?;
        let sig = signature(m, world, f)?;
        if !sig.params.is_empty()
            || sig.returns.len() != 1
            || sig.returns[0].value_type != types::I64
            || sig.call_conv != m.target_config().default_call_conv
        {
            return Err("instrumented JIT ABI mismatch".into());
        }
        m.finalize_definitions().map_err(|e| e.to_string())?;
        owned.code = m.get_finalized_function(ids[entry]);
        Ok(owned)
    }
    /// Call the entry once.
    pub(crate) fn run(&mut self) -> i64 {
        // SAFETY: compile() checked that the entry takes no arguments and
        // returns one i64 in the host C ABI; the exclusively borrowed owner
        // keeps the executable memory alive and the pointer never escapes.
        // Instrumented accesses are valid by compile()'s safety contract.
        unsafe {
            let call: unsafe extern "C" fn() -> i64 = std::mem::transmute(self.code);
            call()
        }
    }
}
impl Drop for InstrumentedJit {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            // SAFETY: the code pointer is private, calls need &mut self and
            // return an owned scalar, and this owner is !Send/!Sync.
            unsafe {
                module.free_memory();
            }
        }
    }
}

#[derive(Clone)]
struct Handler {
    identity: String,
    capabilities: BTreeMap<String, String>,
    effect: Effect,
    state: Option<Variable>,
    /// The open region of a region block (its runtime region id).
    region: Option<Variable>,
    ops: Vec<Clause>,
    captured: BTreeMap<String, Variable>,
    captured_types: BTreeMap<String, Type>,
    state_type: Option<Type>,
    output_type: Type,
    generic_vars: BTreeMap<String, Type>,
    control: HandlerControl,
}
#[derive(Clone)]
struct HandlerControl {
    selector: Variable,
    dispatch: ir::Block,
    returns: Rc<RefCell<Vec<ir::Block>>>,
}
#[derive(Clone, Default)]
struct NativeInfo {
    targets: BTreeSet<usize>,
    fields: BTreeMap<i32, NativeInfo>,
}
impl NativeInfo {
    fn merge(&mut self, other: &Self) {
        self.targets.extend(&other.targets);
        for (offset, info) in &other.fields {
            self.fields.entry(*offset).or_default().merge(info);
        }
    }
}
#[derive(Clone)]
struct NativeClosure {
    ty: Type,
    target: ClosureTarget,
    captures: Vec<(String, Type, ir::StackSlot, NativeInfo)>,
    generics: BTreeMap<String, Type>,
    capabilities: BTreeMap<String, String>,
}
#[derive(Clone)]
enum ClosureTarget {
    Function(String),
    Body {
        name: String,
        params: Vec<Param>,
        result: Type,
    },
    Continuation {
        entry: ir::Block,
        control: HandlerControl,
    },
}
/// Where a `return` from a graph's owner continues: its caller's join block
/// (an inlined graph) or the Cranelift return, through `sret` when present.
#[derive(Clone)]
struct ExitTarget {
    owner: String,
    join: Option<ir::Block>,
    sret: Option<ir::Value>,
    result: Type,
    /// The sequence and string slots this graph frees on exit
    /// ([`crate::fip::seq_drop_plan`]).
    drops: Vec<SeqDropSlot>,
    /// The writes of this graph that free what they replace, by (block,
    /// instruction) ([`crate::fip::replace_drops`]).
    replace: BTreeSet<(usize, usize)>,
    /// `try_get` reads of this graph that its `match` consumes without an
    /// `Option` in memory ([`seq_native::fused_reads`]).
    fused: BTreeMap<crate::ir::ValueId, Option<seq_native::FusedRead>>,
    /// Header words of sequences read once before a loop that keeps them
    /// ([`crate::bounds_hoist::header_hoists`]), by handle and offset.
    header_words: BTreeMap<(ir::Value, i32), ir::Value>,
}
struct Codegen<'a, 'b, M: Module> {
    b: FunctionBuilder<'a>,
    m: &'b mut M,
    world: &'b World,
    ids: &'b BTreeMap<String, FuncId>,
    program: &'b crate::ir::Program,
    capabilities: BTreeMap<String, String>,
    next_capability: u64,
    closures: Vec<NativeClosure>,
    infos: BTreeMap<ir::Value, NativeInfo>,
    variable_infos: BTreeMap<u32, NativeInfo>,
    pointer_parents: BTreeMap<ir::Value, (ir::Value, i32)>,
    vars: BTreeMap<String, Variable>,
    local_types: BTreeMap<String, Type>,
    generic_vars: BTreeMap<String, Type>,
    pointers: BTreeMap<u32, Variable>,
    handlers: Vec<Handler>,
    /// The graphs being emitted, innermost last: the targets of `return`
    /// ([`crate::ir::Op::Exit`]) by lowered function or closure name.
    exits: Vec<ExitTarget>,
    inline_depth: usize,
    /// The failure-record pointer of a `fail_only` function being compiled,
    /// with the error type of each of its `Fail` instances.
    fail_record: Option<(ir::Value, Vec<Type>)>,
    /// Caller-supplied `$caller_slot` buffer pointer when compiling an `@fip`
    /// function that constructs its returned `Box[T]` via `Box::new_in_caller`.
    caller_slot: Option<ir::Value>,
    /// ReuseSlot pointers unpacked from `Op::Bind` (`(bind_dest, slot_index)` -> `Variable`).
    reuse_slots: BTreeMap<(crate::ir::ValueId, usize), Variable>,
    /// Scalar locals kept in registers on the optimize path
    /// ([`register_locals`]): a place use spills them to a scratch slot.
    register_locals: BTreeSet<u32>,
    /// Selected caller-slot source for the active `Op::Call`, if optimized by Stage 4.
    current_call_slot: Option<crate::fip::CallSlot>,
    /// True while emitting code for an `@fip` function (prevents emitting non-FIP drops/frees).
    in_fip_fn: bool,
    trace_fail: Option<ir::Value>,
    trace_entry: Option<(String, FuncId)>,
    /// Present only while compiling a suspending session entry: external
    /// operations park instead of resolving to a lexical handler.
    session: Option<session::SessionCodegen>,
    /// Spans of `eff [ConstDenorm]` code, whose float operations flush
    /// subnormal operands and results to zero.
    denorm: &'b [crate::ast::Span],
    /// Attach source byte offsets to emitted instructions.
    source_locations: bool,
    /// The location instructions are currently emitted at.
    srcloc: ir::SourceLoc,
    /// Read only an instruction's operands from their frontend variables
    /// instead of every IR value of the graph (optimized builds, #931).
    /// Reading every value at every instruction gives each block a phi for
    /// each value of the graph, which inlining makes quadratic.
    operands_only: bool,
    /// The internal calling conventions of this compilation ([`NativeAbi`]).
    abi: &'b NativeAbi,
    /// The convention of the function being compiled.
    own_abi: FnAbi,
    /// The local buffer a register-result function builds its value in.
    result_buffer: Option<ir::Value>,
    /// Type arguments of the next [`Codegen::call_function_into`], which
    /// otherwise reads them from its call site: a call through an effectful
    /// vtable slot names the selected conformance method directly
    /// ([`Codegen::dyn_call_effectful`]).
    forced_call_vars: Option<BTreeMap<String, Type>>,
}
/// The Cranelift source location of source byte `offset`. Locations are
/// byte offsets into the checked module's source; the all-ones value is
/// Cranelift's "no location", so larger offsets saturate below it.
fn source_loc(offset: usize) -> ir::SourceLoc {
    ir::SourceLoc::new(
        u32::try_from(offset)
            .unwrap_or(u32::MAX - 1)
            .min(u32::MAX - 1),
    )
}
/// The source byte offset Cranelift instruction `inst` of `function` was
/// emitted for, when the backend attached one ([`CodegenOptions`]).
pub fn source_offset(function: &ir::Function, inst: ir::Inst) -> Option<usize> {
    if inst.as_u32() as usize >= function.dfg.num_insts() {
        return None;
    }
    let loc = function.srcloc(inst);
    (!loc.is_default()).then(|| loc.bits() as usize)
}
/// Code-generation switches that do not change machine code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodegenOptions {
    /// Attach the source byte offset of the Place-IR instruction (or source
    /// expression) each Cranelift instruction was lowered from as its
    /// Cranelift `SourceLoc` (lane 23, #346). Locations appear in CLIF text
    /// as `@xxxx` and in [`EmittedFunction::function`]; object bytes do not
    /// change, because no debug information is emitted.
    pub source_locations: bool,
}
impl Default for CodegenOptions {
    fn default() -> Self {
        Self {
            source_locations: true,
        }
    }
}
impl<M: Module> Codegen<'_, '_, M> {
    /// Whether float operations at `span` run under `eff [ConstDenorm]`.
    fn flushes(&self, span: &crate::ast::Span) -> bool {
        crate::capability_effects::flushes_subnormals(self.denorm, span)
    }
    /// Software flush-to-zero of a float value when `flush` holds: a
    /// subnormal (magnitude below the kind's `MIN_POSITIVE`) becomes a zero
    /// of the same sign. Other values pass through.
    fn flush(&mut self, flush: bool, v: ir::Value) -> ir::Value {
        let ty = self.b.func.dfg.value_type(v);
        if !flush || !is_float_type(ty) {
            return v;
        }
        let magnitude = self.b.ins().fabs(v);
        let smallest = if ty == types::F32 {
            self.b.ins().f32const(f32::MIN_POSITIVE)
        } else {
            self.b.ins().f64const(f64::MIN_POSITIVE)
        };
        let tiny = self.b.ins().fcmp(FloatCC::LessThan, magnitude, smallest);
        let zero = self.float_const(ty, 0.0);
        let signed_zero = self.b.ins().fcopysign(zero, v);
        self.b.ins().select(tiny, signed_zero, v)
    }
    fn info(&self, value: ir::Value) -> NativeInfo {
        self.infos
            .get(&value)
            .or_else(|| self.infos.get(&self.b.func.dfg.resolve_aliases(value)))
            .cloned()
            .unwrap_or_default()
    }
    fn write_info(&mut self, value: ir::Value, info: NativeInfo) {
        self.infos.insert(value, info.clone());
        if let Some((parent, offset)) = self.pointer_parents.get(&value).copied() {
            let mut updated = self.info(parent);
            updated.fields.insert(offset, info);
            self.write_info(parent, updated);
        }
    }
    fn def_var(&mut self, variable: Variable, value: ir::Value) {
        let info = self.info(value);
        self.variable_infos
            .entry(variable.as_u32())
            .or_default()
            .merge(&info);
        self.b.def_var(variable, value);
        if let Some(address) = self.pointers.get(&variable.as_u32()).copied() {
            let pointer = self.b.use_var(address);
            self.b.ins().store(ir::MemFlags::new(), value, pointer, 0);
            self.write_info(pointer, self.info(value));
        }
    }
    fn use_var(&mut self, variable: Variable) -> ir::Value {
        let mut value = self.b.use_var(variable);
        if let Some(address) = self.pointers.get(&variable.as_u32()).copied() {
            let pointer = self.b.use_var(address);
            let ty = self.b.func.dfg.value_type(value);
            value = self.b.ins().load(ty, ir::MemFlags::new(), pointer, 0);
        }
        let mut info = self.info(value);
        if let Some(stored) = self.variable_infos.get(&variable.as_u32()) {
            info.merge(stored);
        }
        self.infos.insert(value, info);
        value
    }
    fn set_pointer(&mut self, variable: Variable, pointer: ir::Value) {
        let address = self.b.declare_var(self.b.func.dfg.value_type(pointer));
        self.b.def_var(address, pointer);
        self.pointers.insert(variable.as_u32(), address);
    }
    fn pointer(&mut self, variable: Variable) -> Option<ir::Value> {
        let address = self.pointers.get(&variable.as_u32()).copied()?;
        Some(self.b.use_var(address))
    }
    fn edge_info(&mut self, block: ir::Block, args: &[ir::BlockArg]) {
        for (param, arg) in self.b.block_params(block).to_vec().into_iter().zip(args) {
            if let ir::BlockArg::Value(value) = arg {
                let mut info = self.info(param);
                info.merge(&self.info(*value));
                self.infos.insert(param, info);
            }
        }
    }
    fn jump(&mut self, block: ir::Block, args: &[ir::BlockArg]) {
        self.edge_info(block, args);
        self.b.ins().jump(block, args);
    }
    fn branch(
        &mut self,
        condition: ir::Value,
        yes: ir::Block,
        yes_args: &[ir::BlockArg],
        no: ir::Block,
        no_args: &[ir::BlockArg],
    ) {
        self.edge_info(yes, yes_args);
        self.edge_info(no, no_args);
        self.b.ins().brif(condition, yes, yes_args, no, no_args);
    }
    fn callable_shape(a: &Type, b: &Type) -> bool {
        match (a, b) {
            (
                Type::Function {
                    params: a,
                    result: ar,
                    ..
                },
                Type::Function {
                    params: b,
                    result: br,
                    ..
                },
            ) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|((ap, a), (bp, b))| ap == bp && a.to_string() == b.to_string())
                    && ar.to_string() == br.to_string()
            }
            _ => false,
        }
    }
    fn bind_type_evidence(&self, caps: &mut BTreeMap<String, String>, ty: &Type, replace: bool) {
        if let Type::Function { effects, .. } = ty {
            for e in &effects.instances {
                if let Some(label) = &e.label
                    && let Some(id) = self.capabilities.get(&format!("$static:{label}"))
                {
                    let key = format!("$effect:{}", e.name);
                    if replace {
                        caps.insert(key, id.clone());
                    } else {
                        caps.entry(key).or_insert_with(|| id.clone());
                    }
                    caps.insert(format!("$static:{label}"), id.clone());
                }
            }
        }
    }
    fn function_tag(&mut self, function: &str, ty: &Type) -> Result<ir::Value, String> {
        if !self.world.functions[function].generics.is_empty() {
            return Err("first-class generic functions require a concrete wrapper".into());
        }
        let mut caps = BTreeMap::new();
        self.bind_type_evidence(&mut caps, ty, true);
        for e in &self.world.functions[function].effects.instances {
            if let Some(label) = &e.label
                && let Some(id) = caps.get(&format!("$effect:{}", e.name)).cloned()
            {
                caps.insert(format!("$label:{label}"), id);
            }
        }
        let id = self.closures.len() + 1;
        self.closures.push(NativeClosure {
            ty: ty.clone(),
            target: ClosureTarget::Function(function.into()),
            captures: vec![],
            generics: BTreeMap::new(),
            capabilities: caps,
        });
        let tag = self.b.ins().iconst(types::I64, id as i64);
        self.write_info(
            tag,
            NativeInfo {
                targets: BTreeSet::from([id]),
                fields: BTreeMap::new(),
            },
        );
        Ok(tag)
    }
    fn closure_tag(
        &mut self,
        e: &Expr,
        params: &[Param],
        body: &Expr,
    ) -> Result<ir::Value, String> {
        let ty = self.ty(e);
        let Type::Function {
            params: typed,
            result,
            ..
        } = &ty
        else {
            return Err("native closure lacks a concrete arrow type".into());
        };
        let mut actual = params.to_vec();
        for (p, (pass, t)) in actual.iter_mut().zip(typed) {
            p.pass = *pass;
            p.ty = t.clone();
        }
        let mut free = crate::check::free_names(body);
        for p in params {
            free.remove(&p.name);
        }
        let mut captures = vec![];
        for name in free {
            if let Some(var) = self.vars.get(&name).copied() {
                let ty = self.local_types[&name].clone();
                let layout = Layout::of(self.world, &ty)?;
                let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
                    ir::StackSlotKind::ExplicitSlot,
                    layout.size,
                    3,
                ));
                let pointer =
                    self.b
                        .ins()
                        .stack_addr(self.m.target_config().pointer_type(), slot, 0);
                let value = self.use_var(var);
                self.store(pointer, value, &ty)?;
                captures.push((name, ty, slot, self.info(value)));
            }
        }
        let mut caps = self.capabilities.clone();
        self.bind_type_evidence(&mut caps, &ty, true);
        let id = self.closures.len() + 1;
        self.closures.push(NativeClosure {
            ty: ty.clone(),
            target: ClosureTarget::Body {
                name: crate::ir::nested_name("closure", &body.span),
                params: actual,
                result: *result.clone(),
            },
            captures,
            generics: self.generic_vars.clone(),
            capabilities: caps,
        });
        let tag = self.b.ins().iconst(types::I64, id as i64);
        self.write_info(
            tag,
            NativeInfo {
                targets: BTreeSet::from([id]),
                fields: BTreeMap::new(),
            },
        );
        Ok(tag)
    }
    fn qualify_tag(&mut self, tag: ir::Value, ty: &Type) -> ir::Value {
        let mut result = tag;
        let targets = self.info(tag).targets;
        let mut mapped = targets.clone();
        let descriptors = self.closures.clone();
        for (index, mut closure) in descriptors.into_iter().enumerate() {
            if !targets.is_empty() && !targets.contains(&(index + 1)) {
                continue;
            }
            if !Self::callable_shape(&closure.ty, ty) {
                continue;
            }
            let mut caps = closure.capabilities.clone();
            self.bind_type_evidence(&mut caps, ty, false);
            if let ClosureTarget::Function(name) = &closure.target {
                for e in &self.world.functions[name].effects.instances {
                    if let Some(label) = &e.label
                        && let Some(id) = caps.get(&format!("$effect:{}", e.name)).cloned()
                    {
                        caps.insert(format!("$label:{label}"), id);
                    }
                }
            }
            if closure.capabilities == caps {
                continue;
            }
            closure.capabilities = caps;
            closure.ty = ty.clone();
            let id = self.closures.len() + 1;
            self.closures.push(closure);
            mapped.remove(&(index + 1));
            mapped.insert(id);
            let matches = self.b.ins().icmp_imm(IntCC::Equal, tag, (index + 1) as i64);
            let qualified = self.b.ins().iconst(types::I64, id as i64);
            result = self.b.ins().select(matches, qualified, result);
        }
        if !mapped.is_empty() {
            self.write_info(
                result,
                NativeInfo {
                    targets: mapped,
                    fields: BTreeMap::new(),
                },
            );
        }
        result
    }
    fn invoke_tag(
        &mut self,
        tag: ir::Value,
        ty: &Type,
        args: &[Arg],
        site: &Expr,
    ) -> Result<ir::Value, String> {
        let normalized = crate::numeric::default_type(ty);
        let ty = &normalized;
        if self.inline_depth >= 64 {
            return Err("recursive closure specialization exceeds 64 inline frames".into());
        }
        let Type::Function { result, params, .. } = ty else {
            return Err("native indirect call lacks an arrow type".into());
        };
        let mut prepared = vec![];
        for (index, (arg, (pass, t))) in args.iter().zip(params).enumerate() {
            if *pass == Pass::Inout {
                prepared.push(arg.clone());
                continue;
            }
            let value = self.expr(&arg.expr)?;
            let name = format!("$closure_arg{}_{}", value.as_u32(), index);
            let variable = self.b.declare_var(self.b.func.dfg.value_type(value));
            self.def_var(variable, value);
            self.vars.insert(name.clone(), variable);
            self.local_types.insert(name.clone(), t.clone());
            prepared.push(Arg {
                pass: *pass,
                explicit: true,
                expr: Expr {
                    kind: ExprKind::Name(name),
                    span: 0..0,
                },
            });
        }
        let join = self.b.create_block();
        let layout = Layout::of(self.world, result)?;
        self.b.append_block_param(
            join,
            if layout.aggregate() {
                self.m.target_config().pointer_type()
            } else {
                scalar(result, &[])?
            },
        );
        let descriptors = self.closures.clone();
        let targets = self.info(tag).targets;
        let scope = self.vars.clone();
        let types_scope = self.local_types.clone();
        let generic_scope = self.generic_vars.clone();
        let caps_scope = self.capabilities.clone();
        let mut branches = 0;
        for (index, closure) in descriptors.into_iter().enumerate() {
            if !targets.is_empty() && !targets.contains(&(index + 1)) {
                continue;
            }
            if !Self::callable_shape(&closure.ty, ty) {
                continue;
            }
            let yes = self.b.create_block();
            let no = self.b.create_block();
            let condition = self.b.ins().icmp_imm(IntCC::Equal, tag, (index + 1) as i64);
            self.branch(condition, yes, &[], no, &[]);
            self.b.switch_to_block(yes);
            self.inline_depth += 1;
            let value = match &closure.target {
                ClosureTarget::Function(name) => {
                    let mut selected = self.capabilities.clone();
                    selected.extend(closure.capabilities.clone());
                    self.capabilities = selected;
                    self.call_function(name, &prepared, site)
                }
                ClosureTarget::Body { name, params, .. } => {
                    self.invoke_closure_body(&closure, name, params, &prepared)
                }
                ClosureTarget::Continuation { entry, control } => {
                    self.invoke_continuation(*entry, control, result, &prepared)
                }
            };
            self.inline_depth -= 1;
            let value = value?;
            self.jump(join, &[value.into()]);
            branches += 1;
            self.vars = scope.clone();
            self.local_types = types_scope.clone();
            self.generic_vars = generic_scope.clone();
            self.capabilities = caps_scope.clone();
            self.b.switch_to_block(no);
        }
        self.b.ins().trap(ir::TrapCode::unwrap_user(3));
        if branches == 0 {
            return Err("native call has no statically known closure target".into());
        }
        self.b.switch_to_block(join);
        Ok(self.b.block_params(join)[0])
    }
    fn invoke_continuation(
        &mut self,
        entry: ir::Block,
        control: &HandlerControl,
        result: &Type,
        args: &[Arg],
    ) -> Result<ir::Value, String> {
        let values = args
            .iter()
            .map(|a| self.expr(&a.expr))
            .collect::<Result<Vec<_>, _>>()?;
        let saved = self.b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            8,
            3,
        ));
        let selector = self.use_var(control.selector);
        self.b.ins().stack_store(selector, saved, 0);
        let returned = self.b.create_block();
        let layout = Layout::of(self.world, result)?;
        self.b.append_block_param(
            returned,
            if layout.aggregate() {
                self.m.target_config().pointer_type()
            } else {
                scalar(result, &[])?
            },
        );
        let index = control.returns.borrow().len();
        control.returns.borrow_mut().push(returned);
        let selected = self.b.ins().iconst(types::I64, index as i64);
        self.def_var(control.selector, selected);
        self.jump(
            entry,
            &values
                .into_iter()
                .map(ir::BlockArg::from)
                .collect::<Vec<_>>(),
        );
        self.b.switch_to_block(returned);
        let selector = self.b.ins().stack_load(types::I64, saved, 0);
        self.def_var(control.selector, selector);
        Ok(self.b.block_params(returned)[0])
    }
    fn shared_control(&mut self, kind: &str, body: &Expr) -> Result<ir::Value, String> {
        let name = crate::ir::nested_name(kind, &body.span);
        let graph = self
            .program
            .functions
            .get(&name)
            .ok_or("native control body lacks shared IR")?
            .clone();
        let result = crate::numeric::default_type(&substitute(&graph.result, &self.generic_vars));
        let layout = Layout::of(self.world, &result)?;
        let join = self.b.create_block();
        self.b.append_block_param(
            join,
            if layout.aggregate() {
                self.m.target_config().pointer_type()
            } else {
                scalar(&result, &[])?
            },
        );
        self.ir_graph(&graph, None, Some(join))?;
        self.b.switch_to_block(join);
        Ok(self.b.block_params(join)[0])
    }
    fn invoke_closure_body(
        &mut self,
        closure: &NativeClosure,
        name: &str,
        params: &[Param],
        args: &[Arg],
    ) -> Result<ir::Value, String> {
        let mut actuals = vec![];
        let mut restores = vec![];
        for (arg, param) in args.iter().zip(params) {
            let value = if param.pass == Pass::Inout {
                let ptr = self.address(&arg.expr)?;
                if !Layout::of(self.world, &param.ty)?.aggregate()
                    && let ExprKind::Name(n) = &arg.expr.kind
                {
                    restores.push((self.vars[n], ptr, param.ty.clone()));
                }
                ptr
            } else {
                self.expr(&arg.expr)?
            };
            actuals.push(value);
        }
        self.vars = BTreeMap::new();
        self.local_types = BTreeMap::new();
        self.generic_vars = closure.generics.clone();
        let mut caps = self.capabilities.clone();
        caps.extend(closure.capabilities.clone());
        self.capabilities = caps;
        for (name, ty, slot, info) in &closure.captures {
            let ptr = self
                .b
                .ins()
                .stack_addr(self.m.target_config().pointer_type(), *slot, 0);
            self.write_info(ptr, info.clone());
            let value = self.load(ptr, ty)?;
            let layout = Layout::of(self.world, ty)?;
            let value = if layout.aggregate() {
                let copy = self.alloc(layout.size);
                self.copy_bytes(value, copy, layout.size);
                copy
            } else {
                value
            };
            let variable = self.b.declare_var(self.b.func.dfg.value_type(value));
            self.def_var(variable, value);
            self.vars.insert(name.clone(), variable);
            self.local_types.insert(name.clone(), ty.clone());
        }
        for (p, value) in params.iter().zip(actuals) {
            let pointer = value;
            let value = if p.pass == Pass::Inout && !Layout::of(self.world, &p.ty)?.aggregate() {
                self.load(value, &p.ty)?
            } else {
                value
            };
            let variable = self.b.declare_var(self.b.func.dfg.value_type(value));
            self.def_var(variable, value);
            self.vars.insert(p.name.clone(), variable);
            self.local_types.insert(p.name.clone(), p.ty.clone());
            if p.pass == Pass::Inout && !Layout::of(self.world, &p.ty)?.aggregate() {
                self.set_pointer(variable, pointer);
            }
        }
        let ClosureTarget::Body { result, .. } = &closure.target else {
            unreachable!()
        };
        let join = self.b.create_block();
        let layout = Layout::of(self.world, result)?;
        self.b.append_block_param(
            join,
            if layout.aggregate() {
                self.m.target_config().pointer_type()
            } else {
                scalar(result, &[])?
            },
        );
        let graph = self
            .program
            .functions
            .get(name)
            .ok_or("native closure body lacks shared IR")?
            .clone();
        let fip_scope = std::mem::replace(&mut self.in_fip_fn, false);
        let graph_res = self.ir_graph(&graph, None, Some(join));
        self.in_fip_fn = fip_scope;
        graph_res?;
        self.b.switch_to_block(join);
        let result = self.b.block_params(join)[0];
        for (variable, ptr, ty) in restores {
            let value = self.load(ptr, &ty)?;
            self.def_var(variable, value);
        }
        Ok(result)
    }
    fn zero(&mut self) -> ir::Value {
        self.b.ins().iconst(types::I64, 0)
    }
    fn ir_temp(
        &mut self,
        id: crate::ir::ValueId,
        values: &BTreeMap<crate::ir::ValueId, ir::Value>,
        types: &BTreeMap<crate::ir::ValueId, Type>,
    ) -> Result<Expr, String> {
        let value = *values
            .get(&id)
            .ok_or_else(|| format!("undefined native IR value %{id}"))?;
        let ty = substitute(
            types.get(&id).ok_or("IR value lacks a type")?,
            &self.generic_vars,
        );
        let name = format!("$ir_value{id}");
        let var = self.b.declare_var(self.b.func.dfg.value_type(value));
        self.def_var(var, value);
        self.vars.insert(name.clone(), var);
        self.local_types.insert(name.clone(), ty);
        Ok(Expr {
            kind: ExprKind::Name(name),
            span: 0..0,
        })
    }
    fn bind_ir(
        &mut self,
        bind_dest: crate::ir::ValueId,
        slot_index: &mut usize,
        inside_box: bool,
        p: &Pattern,
        value: ir::Value,
        ty: &Type,
    ) -> Result<(), String> {
        if crate::fip::is_box_pattern(self.world, p) {
            let inner = crate::fip::box_inner(self.world, ty)
                .cloned()
                .ok_or("Box pattern expects Box[T]")?;
            let idx = *slot_index;
            *slot_index += 1;
            let var = self.b.declare_var(self.b.func.dfg.value_type(value));
            self.def_var(var, value);
            self.reuse_slots.insert((bind_dest, idx), var);
            let inner_val = self.load(value, &inner)?;
            let PatternKind::Constructor { fields, .. } = &p.kind else {
                unreachable!();
            };
            self.bind_ir(bind_dest, slot_index, true, &fields[0].1, inner_val, &inner)?;
            if !self.in_fip_fn {
                self.fip_box_free(value)?;
            }
            return Ok(());
        }
        match &p.kind {
            PatternKind::Bind(n) => {
                let var = *self.vars.get(n).ok_or("IR binding lacks a declared slot")?;
                let bound_value = if inside_box {
                    let layout = Layout::of(self.world, ty)?;
                    if layout.aggregate() {
                        let copy = self.alloc(layout.size);
                        self.copy_bytes(value, copy, layout.size);
                        copy
                    } else {
                        value
                    }
                } else {
                    value
                };
                self.def_var(var, bound_value);
                Ok(())
            }
            PatternKind::Tuple(ps) => {
                if ps.is_empty() {
                    return Ok(());
                }
                let layout = Layout::of(self.world, ty)?;
                let LayoutKind::Product(fs) = layout.kind else {
                    return Err("IR tuple pattern needs a product".into());
                };
                for (p, f) in ps.iter().zip(fs) {
                    let ptr = self.offset(value, f.offset);
                    let v = self.load(ptr, &f.ty)?;
                    self.bind_ir(bind_dest, slot_index, inside_box, p, v, &f.ty)?;
                }
                Ok(())
            }
            PatternKind::Constructor { name, fields, .. } => {
                let layout = Layout::of(self.world, ty)?;
                let (_, fs) = layout.variant(name).ok_or("unknown IR variant")?;
                for (n, p) in fields {
                    let f = fs
                        .iter()
                        .find(|f| &f.name == n)
                        .ok_or("unknown IR pattern field")?;
                    let ptr = self.offset(value, f.offset);
                    let v = self.load(ptr, &f.ty)?;
                    self.bind_ir(bind_dest, slot_index, inside_box, p, v, &f.ty)?;
                }
                if !self.in_fip_fn {
                    for f in fs {
                        if !fields.iter().any(|(n, _)| n == &f.name)
                            && crate::fip::type_contains_box(self.world, &f.ty)
                        {
                            let ptr = self.offset(value, f.offset);
                            let v = self.load(ptr, &f.ty)?;
                            self.drop_box_value(v, &f.ty)?;
                        }
                    }
                }
                Ok(())
            }
            PatternKind::Wild => {
                if !self.in_fip_fn && crate::fip::type_contains_box(self.world, ty) {
                    self.drop_box_value(value, ty)?;
                }
                Ok(())
            }
            PatternKind::Literal(_) => Ok(()),
        }
    }
    fn ir_function(
        &mut self,
        f: &crate::ir::FunctionIr,
        sret: Option<ir::Value>,
    ) -> Result<(), String> {
        self.ir_graph(f, sret, None)
    }
    fn ir_graph(
        &mut self,
        f: &crate::ir::FunctionIr,
        sret: Option<ir::Value>,
        return_to: Option<ir::Block>,
    ) -> Result<(), String> {
        // An inlined graph restores the caller's location when it is done.
        let saved = self.srcloc;
        let result = self.ir_graph_at(f, sret, return_to);
        if return_to.is_some() && self.source_locations {
            self.srcloc = saved;
            self.b.set_srcloc(saved);
        }
        result
    }
    fn ir_graph_at(
        &mut self,
        f: &crate::ir::FunctionIr,
        sret: Option<ir::Value>,
        return_to: Option<ir::Block>,
    ) -> Result<(), String> {
        use crate::ir::{Op, Terminator};
        let entry = self
            .b
            .current_block()
            .ok_or("native IR lacks an entry block")?;
        let entry = if return_to.is_some() {
            let graph_entry = self.b.create_block();
            self.jump(graph_entry, &[]);
            self.b.switch_to_block(graph_entry);
            graph_entry
        } else {
            entry
        };
        let mut blocks = vec![entry];
        for _ in 1..f.blocks.len() {
            blocks.push(self.b.create_block());
        }
        // Locals this graph declares itself (a nested graph, such as a
        // handled body, shares the names its caller already has).
        let mut fresh_locals = BTreeSet::new();
        let registers_ok = if self.operands_only {
            register_locals(self.world, f, &self.generic_vars)
        } else {
            BTreeSet::new()
        };
        for (n, t) in &f.locals {
            let ty = crate::numeric::default_type(&substitute(t, &self.generic_vars));
            let layout = Layout::of(self.world, &ty)?;
            let existing = self.vars.get(n).copied();
            let var = if let Some(var) = existing {
                var
            } else {
                fresh_locals.insert(n.clone());
                let native_ty = if layout.aggregate() {
                    self.m.target_config().pointer_type()
                } else {
                    scalar(&ty, &[])?
                };
                let var = self.b.declare_var(native_ty);
                let zero = self.scalar_zero(native_ty);
                self.def_var(var, zero);
                self.vars.insert(n.clone(), var);
                self.local_types.insert(n.clone(), ty.clone());
                var
            };
            // On the optimize path a numeric local that only pure calls
            // borrow stays a frontend SSA variable (a register): a place
            // argument spills it to a scratch slot for that one call, and
            // the call site restores it ([`register_locals`]). Every other
            // scalar local keeps a zeroed home slot.
            // A function's own by-value parameters qualify too; a nested
            // graph keeps whatever its enclosing graph chose for a shared
            // name.
            let owned = existing.is_none()
                || (return_to.is_none() && !self.pointers.contains_key(&var.as_u32()));
            if owned && registers_ok.contains(n) {
                self.register_locals.insert(var.as_u32());
            } else if !layout.aggregate()
                && !self.pointers.contains_key(&var.as_u32())
                && !self.register_locals.contains(&var.as_u32())
            {
                let value = self.use_var(var);
                let ptr = self.alloc(8);
                self.store(ptr, value, &ty)?;
                self.set_pointer(var, ptr);
            }
        }
        // Where constructed aggregates are built (lane 7, #990).
        let homes = if self.operands_only && !self.in_fip_fn {
            aggregates::plan(self.world, f, &self.generic_vars, return_to.is_some())
        } else {
            BTreeMap::new()
        };
        let mut home_pointers = aggregates::HomePointers::new();
        // Sequence headers read once before the loops that keep them.
        let header_hoists = if self.operands_only && !self.in_fip_fn {
            crate::bounds_hoist::header_hoists(f)
        } else {
            vec![]
        };
        let mut hoisted_reads = BTreeMap::new();
        let mut values = BTreeMap::new();
        // A resumption can return through a shared handler dispatcher. Keep
        // virtual registers in frontend SSA variables so return edges receive
        // the appropriate phis instead of referring to non-dominating values.
        let mut registers = BTreeMap::new();
        for (id, ty) in &f.values {
            let ty = crate::numeric::default_type(&substitute(ty, &self.generic_vars));
            let layout = Layout::of(self.world, &ty)?;
            let native_ty = if layout.aggregate() {
                self.m.target_config().pointer_type()
            } else {
                scalar(&ty, &[])?
            };
            let register = self.b.declare_var(native_ty);
            let zero = self.scalar_zero(native_ty);
            self.def_var(register, zero);
            registers.insert(*id, register);
        }
        let mut parameters = BTreeMap::new();
        for (i, block) in f.blocks.iter().enumerate() {
            for id in &block.params {
                let ty =
                    crate::numeric::default_type(&substitute(&f.values[id], &self.generic_vars));
                let layout = Layout::of(self.world, &ty)?;
                let value = self.b.append_block_param(
                    blocks[i],
                    if layout.aggregate() {
                        self.m.target_config().pointer_type()
                    } else {
                        scalar(&ty, &[])?
                    },
                );
                values.insert(*id, value);
                parameters.insert(*id, value);
            }
        }
        let ptr_ty = self.m.target_config().pointer_type();
        let mut join_slot_params: BTreeMap<usize, Vec<_>> = BTreeMap::new();
        for (&block_idx, joins) in &f.fip_join_slots {
            let mut entries = Vec::with_capacity(joins.len());
            for join in joins {
                let param = self.b.append_block_param(blocks[block_idx], ptr_ty);
                entries.push((join.dest_slot, param));
            }
            join_slot_params.insert(block_idx, entries);
        }
        let mut graph = petgraph::graphmap::DiGraphMap::<usize, ()>::new();
        for id in 0..f.blocks.len() {
            graph.add_node(id);
        }
        for (id, block) in f.blocks.iter().enumerate() {
            let targets = match &block.terminator {
                Terminator::Jump { block, .. } => vec![*block],
                Terminator::Branch { yes, no, .. } => vec![*yes, *no],
                Terminator::Match { arms, .. } => arms.iter().map(|(_, b)| *b).collect(),
                _ => vec![],
            };
            for target in targets {
                graph.add_edge(id, target, ());
            }
        }
        // Blocks after a `break` or `return` (`crate::loops`) have no
        // predecessors; they are emitted last so the entry stays first.
        // Acyclic graphs keep their topological order; a loop's back edges
        // need a reverse postorder from the entry instead. Frontend
        // variables join values across either order.
        let mut postorder = vec![];
        let mut dfs = petgraph::visit::DfsPostOrder::new(&graph, 0);
        while let Some(n) = dfs.next(&graph) {
            postorder.push(n);
        }
        let reached = postorder.iter().copied().collect::<BTreeSet<_>>();
        let unreached = (0..f.blocks.len()).filter(|n| !reached.contains(n));
        let mut live = graph.clone();
        for n in unreached.clone() {
            live.remove_node(n);
        }
        let mut order = petgraph::algo::toposort(&live, None).unwrap_or_else(|_| {
            postorder.reverse();
            postorder
        });
        order.extend(unreached);
        // Box drop elaboration (`fip::elaborate_non_fip_drops`) assumes a
        // forward-only graph without early exits; refuse rather than leak.
        if f.blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .any(|i| matches!(i.op, Op::Loop | Op::Exit { .. }))
            && f.values
                .values()
                .chain(f.locals.values())
                .any(|t| crate::fip::mentions_box(self.world, t))
            && !crate::fip::loop_boxes_reclaimed(self.world, f)
        {
            return Err(format!(
                "native '{}': {}",
                f.name,
                crate::loops::BOX_REFUSAL
            ));
        }
        let result_type = crate::numeric::default_type(&substitute(&f.result, &self.generic_vars));
        // Sequence and string drops: a function's own graph owns its slots;
        // a nested graph (a handled body or clause) owns only the locals it
        // declares, such as those of a call the inliner spliced into it.
        let drops = if self.in_fip_fn {
            vec![]
        } else if return_to.is_none() {
            self.seq_drop_slots(f, None)?
        } else {
            self.seq_drop_slots(f, Some(&fresh_locals))?
        };
        // Writes that free what they replace, and temporaries never bound.
        let (replace, temporaries) = if self.drops_enabled() {
            let fresh = return_to.is_some().then_some(&fresh_locals);
            (
                crate::fip::replace_drops(self.world, f, fresh),
                crate::fip::temporary_drops(self.world, f),
            )
        } else {
            Default::default()
        };
        self.exits.push(ExitTarget {
            owner: f.name.clone(),
            join: return_to,
            sret,
            result: result_type,
            drops,
            replace,
            fused: seq_native::fused_reads(f),
            header_words: BTreeMap::new(),
        });
        for i in order {
            let block = &f.blocks[i];
            if self.b.current_block() != Some(blocks[i]) {
                self.b.switch_to_block(blocks[i]);
            }
            for id in &block.params {
                self.def_var(registers[id], parameters[id]);
                values.insert(*id, parameters[id]);
                // Every value reaching a homed parameter was built in its
                // home (`aggregates::plan`), so its consumer skips the copy.
                if homes.contains_key(id) {
                    self.home_pointer(*id, &homes, &mut home_pointers, sret)?;
                }
            }
            self.drop_temporaries(&temporaries, f, i, crate::fip::AT_BLOCK_ENTRY, &values)?;
            if let Some(entries) = join_slot_params.get(&i) {
                for &(dest_slot, param) in entries {
                    let var = self.b.declare_var(ptr_ty);
                    self.def_var(var, param);
                    self.reuse_slots.insert(dest_slot, var);
                }
            }
            let before_inst_drops = f.box_drops_before_inst.get(&i);
            let after_inst_drops = f.box_drops_after_inst.get(&i);
            for (inst_idx, inst) in block.instructions.iter().enumerate() {
                if !self.operands_only || before_inst_drops.is_some() || after_inst_drops.is_some()
                {
                    for (id, register) in &registers {
                        values.insert(*id, self.use_var(*register));
                    }
                } else {
                    for id in crate::ir_inline::operands(&inst.op) {
                        if let Some(register) = registers.get(&id) {
                            values.insert(id, self.use_var(*register));
                        }
                    }
                }
                if !self.in_fip_fn
                    && let Some(drops) = before_inst_drops
                {
                    for (d_idx, drop) in drops {
                        if *d_idx == inst_idx {
                            self.emit_fip_drop(drop, &values, inst.span.clone())?;
                        }
                    }
                }
                if inst.span.end != 0 {
                    self.set_location(inst.span.start);
                }
                let ty = crate::numeric::default_type(&substitute(&inst.ty, &self.generic_vars));
                let make = |kind| Expr {
                    kind,
                    span: inst.span.clone(),
                };
                let handler_state = if let Op::Handle { state_value, .. } = &inst.op {
                    *state_value
                } else {
                    None
                };
                let value = match &inst.op {
                    Op::Loop => self.zero(),
                    Op::Exit { value, owner } => {
                        let v = values
                            .get(value)
                            .copied()
                            .ok_or_else(|| format!("undefined native IR value %{value}"))?;
                        let ty = f.values.get(value).cloned().unwrap_or(Type::Infer);
                        // A value built in the result buffer is not copied.
                        let placed = aggregates::placed(&home_pointers, *value);
                        self.exit_to(owner, v, &ty, placed)?;
                        self.zero()
                    }
                    Op::Constant(l) => self.expr(&make(ExprKind::Literal(l.clone())))?,
                    Op::Unit => self.zero(),
                    Op::Read { .. } if hoisted_reads.contains_key(&inst.dest) => {
                        hoisted_reads[&inst.dest]
                    }
                    // The one read of a pass-through local yields the
                    // storage its `let` bound (`aggregates::plan`).
                    Op::Read { root, .. }
                        if homes.contains_key(&inst.dest) && self.vars.contains_key(root) =>
                    {
                        self.home_pointer(inst.dest, &homes, &mut home_pointers, sret)?;
                        let var = self.vars[root];
                        self.use_var(var)
                    }
                    // An array read only indexed right away (§5): the
                    // element is read from the place, not from a copy.
                    Op::Read { root, path, .. }
                        if native_arrays::read_in_place(f, i, inst_idx)
                            && Layout::of(self.world, &ty).is_ok_and(|l| l.aggregate()) =>
                    {
                        let p = crate::check::Place {
                            root: root.clone(),
                            path: path.clone(),
                        };
                        self.address(&ir_place_expr(&p, inst.span.clone()))?
                    }
                    Op::Read {
                        root, path, pass, ..
                    } => {
                        let p = crate::check::Place {
                            root: root.clone(),
                            path: path.clone(),
                        };
                        let e = ir_place_expr(&p, inst.span.clone());
                        if path.is_empty()
                            && self.world.functions.contains_key(root)
                            && !self.vars.contains_key(root)
                        {
                            self.function_tag(root, &ty)?
                        } else if *pass != Pass::Sink
                            && (Layout::of(self.world, &ty)?.aggregate()
                                || matches!(ty, Type::Function { .. }))
                        {
                            self.address(&e)?
                        } else {
                            let value = self.expr(&e)?;
                            if *pass == Pass::Sink && path.is_empty() {
                                self.seq_drop_moved(root);
                            }
                            value
                        }
                    }
                    Op::Bind { pattern, value } => {
                        let t = crate::numeric::default_type(&substitute(
                            &f.values[value],
                            &self.generic_vars,
                        ));
                        let mut slot_index = 0;
                        if !self.seq_fused_bind(inst.dest, pattern, *value)? {
                            let bound = self.seq_drop_rebind(pattern, values[value])?;
                            self.bind_ir(inst.dest, &mut slot_index, false, pattern, bound, &t)?;
                            self.seq_drop_bound(pattern)?;
                        }
                        self.zero()
                    }
                    // Built in the destination (`aggregates::drop_unsafe_places`
                    // keeps the rule): the write is the construction itself.
                    Op::Write { value, .. } if aggregates::placed(&home_pointers, *value) => {
                        self.zero()
                    }
                    // An array element write (§5) takes its index and value
                    // straight from the pair built only for it.
                    Op::Write { place, op, value }
                        if op.starts_with(crate::arrays::WRITE_PREFIX)
                            && native_arrays::written_pair(f, *value).is_some() =>
                    {
                        let (index, element) = native_arrays::written_pair(f, *value).unwrap();
                        let mut path = place.split('.');
                        let p = crate::check::Place {
                            root: path.next().ok_or("empty IR place")?.into(),
                            path: path.map(str::to_owned).collect(),
                        };
                        let left = ir_place_expr(&p, inst.span.clone());
                        let index = *values
                            .get(&index)
                            .ok_or_else(|| format!("undefined native IR value %{index}"))?;
                        let element = *values
                            .get(&element)
                            .ok_or_else(|| format!("undefined native IR value %{element}"))?;
                        let element_ty = match f.values.get(value) {
                            Some(Type::Tuple(pair)) if pair.len() == 2 => {
                                substitute(&pair[1], &self.generic_vars)
                            }
                            _ => {
                                return Err(
                                    "native array write requires an (index, value) pair".into()
                                );
                            }
                        };
                        self.array_store(&inst.span, op, &left, index, element, &element_ty)?
                    }
                    Op::Write { place, op, value } => {
                        self.drop_replaced(place, op, (i, inst_idx))?;
                        self.seq_drop_overwrite(place)?;
                        let mut path = place.split('.');
                        let p = crate::check::Place {
                            root: path.next().ok_or("empty IR place")?.into(),
                            path: path.map(str::to_owned).collect(),
                        };
                        let left = ir_place_expr(&p, inst.span.clone());
                        let value_ty = f.values.get(value).map(|t| {
                            crate::numeric::default_type(&substitute(t, &self.generic_vars))
                        });
                        if self.operands_only
                            && op == "="
                            && let Some(value_ty) = value_ty
                            && Layout::of(self.world, &value_ty)?.aggregate()
                            && self.vars.contains_key(&p.root)
                        {
                            // A whole aggregate is copied from its storage
                            // into the destination once, not through a
                            // temporary copy of the operand. Distinct places
                            // of one type are equal or disjoint (a value
                            // cannot contain itself without a `Box`), so the
                            // forward word copy is exact.
                            let source = *values
                                .get(value)
                                .ok_or_else(|| format!("undefined native IR value %{value}"))?;
                            let destination = self.address(&left)?;
                            self.store(destination, source, &value_ty)?;
                            self.zero()
                        } else {
                            let right = self.ir_temp(*value, &values, &f.values)?;
                            self.expr(&make(ExprKind::Assign(
                                op.clone(),
                                Box::new(left),
                                Box::new(right),
                            )))?
                        }
                    }
                    Op::Unary(op, v) => {
                        let v = self.ir_temp(*v, &values, &f.values)?;
                        self.expr(&make(ExprKind::Unary(op.clone(), Box::new(v))))?
                    }
                    Op::Binary(op, a, b) => {
                        let a = self.ir_temp(*a, &values, &f.values)?;
                        let b = self.ir_temp(*b, &values, &f.values)?;
                        self.expr(&make(ExprKind::Binary(
                            op.clone(),
                            Box::new(a),
                            Box::new(b),
                        )))?
                    }
                    // Built only for an array element write, which reads
                    // its operands directly (`native_arrays::written_pair`).
                    Op::Tuple(_) if native_arrays::written_pair(f, inst.dest).is_some() => {
                        self.zero()
                    }
                    Op::Tuple(ids) => {
                        let es = ids
                            .iter()
                            .map(|id| self.ir_temp(*id, &values, &f.values))
                            .collect::<Result<Vec<_>, _>>()?;
                        let into =
                            self.home_pointer(inst.dest, &homes, &mut home_pointers, sret)?;
                        let built = ids
                            .iter()
                            .enumerate()
                            .filter(|(_, id)| aggregates::placed(&home_pointers, **id))
                            .map(|(i, _)| i.to_string())
                            .collect();
                        self.data_into(
                            None,
                            es.iter()
                                .enumerate()
                                .map(|(i, e)| (i.to_string(), e))
                                .collect(),
                            &ty,
                            into,
                            &built,
                        )?
                    }
                    Op::Data(name, fields) => {
                        let es = fields
                            .iter()
                            .map(|(n, id)| Ok((n.clone(), self.ir_temp(*id, &values, &f.values)?)))
                            .collect::<Result<Vec<_>, String>>()?;
                        let into =
                            self.home_pointer(inst.dest, &homes, &mut home_pointers, sret)?;
                        let built = fields
                            .iter()
                            .filter(|(_, id)| aggregates::placed(&home_pointers, *id))
                            .map(|(n, _)| n.clone())
                            .collect();
                        self.data_into(
                            Some(name),
                            es.iter().map(|(n, e)| (n.clone(), e)).collect(),
                            &ty,
                            into,
                            &built,
                        )?
                    }
                    Op::Project(base, field) => {
                        let base = self.ir_temp(*base, &values, &f.values)?;
                        self.expr(&make(ExprKind::Field(Box::new(base), field.clone())))?
                    }
                    Op::Index(base, index) => {
                        let base = self.ir_temp(*base, &values, &f.values)?;
                        let index = self.ir_temp(*index, &values, &f.values)?;
                        self.expr(&make(ExprKind::Index(Box::new(base), Box::new(index))))?
                    }
                    Op::Call {
                        callee,
                        args,
                        capability,
                        types: instance,
                    } => {
                        let es = args
                            .iter()
                            .map(|a| {
                                // A slice view argument is a value, not a place,
                                // and so is a boxed object lent from a
                                // sequence element (§7.3).
                                let view = a.place.is_none()
                                    && f.values.get(&a.value).is_some_and(|t| {
                                        crate::seq::is_slice_type(t)
                                            || crate::dyn_trait::is_boxed(t)
                                    });
                                let expr = if !view
                                    && (a.pass == Pass::Inout
                                        || (a.pass == Pass::In
                                            && !self.world.functions.contains_key(callee)
                                            && a.place.is_some()))
                                {
                                    ir_place_expr(
                                        a.place
                                            .as_ref()
                                            .ok_or("mutable IR argument lacks place")?,
                                        inst.span.clone(),
                                    )
                                } else {
                                    self.ir_temp(a.value, &values, &f.values)?
                                };
                                Ok(Arg {
                                    pass: a.pass,
                                    explicit: true,
                                    expr,
                                })
                            })
                            .collect::<Result<Vec<_>, String>>()?;
                        let site = make(ExprKind::Tuple(vec![]));
                        if let Some(table) = self.constant_table(callee, f, inst.dest)? {
                            table
                        } else if self.world.functions.contains_key(callee) {
                            self.current_call_slot = f.fip_call_slots.get(&inst.dest).cloned();
                            let into =
                                self.home_pointer(inst.dest, &homes, &mut home_pointers, sret)?;
                            let call_res = self.call_function_into(callee, &es, &site, into);
                            self.current_call_slot = None;
                            call_res?
                        } else if let Some((effect, op)) = callee.split_once("::") {
                            self.perform(effect, capability.as_deref(), instance, op, &es)?
                        } else if let Some(v) = self.float_call(callee, &es, &site)? {
                            // `fma` stays a call in the Place IR.
                            v
                        } else {
                            return Err("unknown IR native callee".into());
                        }
                    }
                    Op::Indirect { callee, args } => {
                        let es = args
                            .iter()
                            .map(|a| {
                                let expr = if a.pass == Pass::Inout {
                                    ir_place_expr(
                                        a.place
                                            .as_ref()
                                            .ok_or("mutable closure argument lacks place")?,
                                        inst.span.clone(),
                                    )
                                } else {
                                    self.ir_temp(a.value, &values, &f.values)?
                                };
                                Ok(Arg {
                                    pass: a.pass,
                                    explicit: true,
                                    expr,
                                })
                            })
                            .collect::<Result<Vec<_>, String>>()?;
                        let callable = substitute(&f.values[callee], &self.generic_vars);
                        self.invoke_tag(
                            values[callee],
                            &callable,
                            &es,
                            &make(ExprKind::Tuple(vec![])),
                        )?
                    }
                    Op::Try(v) => {
                        let v = self.ir_temp(*v, &values, &f.values)?;
                        self.expr(&make(ExprKind::Try(Box::new(v))))?
                    }
                    Op::Vector { op, args } => {
                        let operands = args
                            .iter()
                            .map(|id| {
                                values
                                    .get(id)
                                    .copied()
                                    .ok_or_else(|| format!("undefined native IR value %{id}"))
                            })
                            .collect::<Result<Vec<_>, String>>()?;
                        self.vector_op(op, &operands)?
                    }
                    Op::Tiled {
                        op,
                        args,
                        capability,
                    } => {
                        let operands = args
                            .iter()
                            .map(|a| {
                                values.get(&a.value).copied().ok_or_else(|| {
                                    format!("undefined native IR value %{}", a.value)
                                })
                            })
                            .collect::<Result<Vec<_>, String>>()?;
                        self.tiled_op(op, &operands, capability.as_deref())?
                    }
                    Op::Seq { op, args } if self.seq_is_fused(inst.dest) => {
                        let receiver = args.first().ok_or("native try_get lacks a receiver")?;
                        let index = args.get(1).ok_or("native try_get lacks an index")?;
                        let operands = [values[&receiver.value], values[&index.value]];
                        let receiver_type = crate::numeric::default_type(&substitute(
                            &f.values[&receiver.value],
                            &self.generic_vars,
                        ));
                        // A hoisted read is in bounds by its loop's guard.
                        let checked = !matches!(op, crate::seq::SeqOp::Hoisted(_));
                        self.seq_fused_try_get(inst.dest, operands, &receiver_type, checked)?
                    }
                    Op::Seq { op, args } if op.place_primitive() => {
                        // `swap` and `replace` write their `inout` places.
                        let [a, b] = args.as_slice() else {
                            return Err(format!("native {} needs two operands", op.name()));
                        };
                        let place = |a: &crate::ir::Argument| {
                            a.place
                                .as_ref()
                                .map(|p| ir_place_expr(p, inst.span.clone()))
                                .ok_or_else(|| format!("native {} lacks a place", op.name()))
                        };
                        let first = place(a)?;
                        let second = if *op == crate::seq::SeqOp::Swap {
                            place(b)?
                        } else {
                            self.ir_temp(b.value, &values, &f.values)?
                        };
                        let ty = f
                            .values
                            .get(&a.value)
                            .map(|t| substitute(t, &self.generic_vars));
                        self.seq_place_primitive(*op, &[&first, &second], ty)?
                    }
                    Op::Seq { op, args } => {
                        let mut operands = Vec::new();
                        let mut operand_types = Vec::new();
                        for a in args {
                            operands.push(values.get(&a.value).copied().ok_or_else(|| {
                                format!("undefined native IR value %{}", a.value)
                            })?);
                            operand_types.push(crate::numeric::default_type(&substitute(
                                f.values.get(&a.value).ok_or("IR value lacks a type")?,
                                &self.generic_vars,
                            )));
                        }
                        // A growing operation writes its new handle back to the
                        // receiver's place (#1021).
                        let receiver = args
                            .first()
                            .and_then(|a| a.place.as_ref())
                            .filter(|_| op.has_receiver() && op.mutates())
                            .map(|p| ir_place_expr(p, inst.span.clone()));
                        self.seq_op(*op, &operands, &operand_types, &ty, receiver.as_ref())?
                    }
                    Op::Fip { op, args } => {
                        let operands = args
                            .iter()
                            .map(|a| {
                                values.get(&a.value).copied().ok_or_else(|| {
                                    format!("undefined native IR value %{}", a.value)
                                })
                            })
                            .collect::<Result<Vec<_>, String>>()?;
                        let actual = args.first().and_then(|a| f.values.get(&a.value)).map(|t| {
                            crate::numeric::default_type(&substitute(t, &self.generic_vars))
                        });
                        self.fip_op(op, &operands, &ty, actual.as_ref())?
                    }
                    Op::Closure { expr, aliases, .. } | Op::Handle { expr, aliases, .. } => {
                        let old_vars = self.vars.clone();
                        let old_types = self.local_types.clone();
                        let mut scoped_vars = BTreeMap::new();
                        let mut scoped_types = BTreeMap::new();
                        for (name, slot) in aliases {
                            if let Some(v) = old_vars.get(slot) {
                                scoped_vars.insert(name.clone(), *v);
                                scoped_types.insert(name.clone(), old_types[slot].clone());
                            }
                        }
                        self.vars = scoped_vars;
                        self.local_types = scoped_types;
                        let mut expr = expr.clone();
                        if let Some(id) = handler_state {
                            let name = format!("$handler_init{}", inst.dest);
                            let value = values[&id];
                            let ty = substitute(&f.values[&id], &self.generic_vars);
                            let variable = self.b.declare_var(self.b.func.dfg.value_type(value));
                            self.def_var(variable, value);
                            self.vars.insert(name.clone(), variable);
                            self.local_types.insert(name.clone(), ty);
                            if let ExprKind::Handle {
                                state: Some((_, init)),
                                ..
                            } = &mut expr.kind
                            {
                                **init = Expr {
                                    kind: ExprKind::Name(name),
                                    span: 0..0,
                                };
                            }
                        }
                        let result = self.expr(&expr);
                        self.vars = old_vars;
                        self.local_types = old_types;
                        result?
                    }
                };
                let value = self.coerce(value, Some(&ty));
                let value = if matches!(ty, Type::Function { .. })
                    && matches!(inst.op, crate::ir::Op::Read { .. })
                {
                    self.qualify_tag(value, &ty)
                } else {
                    value
                };
                values.insert(inst.dest, value);
                self.def_var(registers[&inst.dest], value);
                if inst.ty.name() != "Never" {
                    self.drop_temporaries(&temporaries, f, i, inst_idx, &values)?;
                }
                if !self.in_fip_fn
                    && inst.ty.name() != "Never"
                    && let Some(drops) = after_inst_drops
                {
                    for (d_idx, drop) in drops {
                        if *d_idx == inst_idx {
                            self.emit_fip_drop(drop, &values, inst.span.clone())?;
                        }
                    }
                }
            }
            if !self.operands_only || f.box_drops_before_term.contains_key(&i) {
                for (id, register) in &registers {
                    values.insert(*id, self.use_var(*register));
                }
            } else {
                for id in crate::ir_inline::terminator_operands(&block.terminator) {
                    if let Some(register) = registers.get(&id) {
                        values.insert(id, self.use_var(*register));
                    }
                }
            }
            if !self.in_fip_fn
                && let Some(drops) = f.box_drops_before_term.get(&i)
            {
                for drop in drops {
                    self.emit_fip_drop(drop, &values, Span::default())?;
                }
            }
            for hoist in header_hoists.iter().filter(|h| h.preheader == i) {
                let receiver = self.seq_hoist_header(&hoist.place, &hoist.ty)?;
                for read in &hoist.reads {
                    hoisted_reads.insert(*read, receiver);
                }
            }
            match &block.terminator {
                Terminator::Return(id) => {
                    self.seq_drop_exit(self.exits.len() - 1)?;
                    let ty =
                        crate::numeric::default_type(&substitute(&f.result, &self.generic_vars));
                    let value = if f.values[id].name() == "Never" {
                        self.default_value(&ty)?
                    } else {
                        values[id]
                    };
                    let value = if let Some(output) = sret {
                        if !aggregates::placed(&home_pointers, *id) {
                            let layout = Layout::of(self.world, &ty)?;
                            self.copy_bytes(value, output, layout.size);
                        }
                        self.zero()
                    } else {
                        value
                    };
                    if let Some(join) = return_to {
                        self.jump(join, &[value.into()]);
                    } else {
                        self.return_value(value);
                    }
                }
                Terminator::Jump { block, args } => {
                    let mut vs = vec![];
                    for (id, target) in args.iter().zip(&f.blocks[*block].params) {
                        let ty = crate::numeric::default_type(&substitute(
                            &f.values[target],
                            &self.generic_vars,
                        ));
                        vs.push(if f.values[id].name() == "Never" {
                            self.default_value(&ty)?
                        } else {
                            values[id]
                        });
                    }
                    let mut edge_args: Vec<ir::BlockArg> = vs.iter().map(|v| (*v).into()).collect();
                    if let Some(joins) = f.fip_join_slots.get(block) {
                        for join in joins {
                            let val = if let Some((_, sb, si)) =
                                join.incoming.iter().find(|&&(p, _, _)| p == i)
                            {
                                self.use_var(self.reuse_slots[&(*sb, *si)])
                            } else {
                                self.zero()
                            };
                            edge_args.push(val.into());
                        }
                    }
                    self.jump(blocks[*block], &edge_args);
                }
                Terminator::Branch { condition, yes, no } => {
                    let mut yes_args = Vec::new();
                    if let Some(joins) = f.fip_join_slots.get(yes) {
                        for join in joins {
                            let val = if let Some((_, sb, si)) =
                                join.incoming.iter().find(|&&(p, _, _)| p == i)
                            {
                                self.use_var(self.reuse_slots[&(*sb, *si)])
                            } else {
                                self.zero()
                            };
                            yes_args.push(val.into());
                        }
                    }
                    let mut no_args = Vec::new();
                    if let Some(joins) = f.fip_join_slots.get(no) {
                        for join in joins {
                            let val = if let Some((_, sb, si)) =
                                join.incoming.iter().find(|&&(p, _, _)| p == i)
                            {
                                self.use_var(self.reuse_slots[&(*sb, *si)])
                            } else {
                                self.zero()
                            };
                            no_args.push(val.into());
                        }
                    }
                    self.branch(
                        values[condition],
                        blocks[*yes],
                        &yes_args,
                        blocks[*no],
                        &no_args,
                    );
                }
                Terminator::Match { value, arms } => {
                    let ty = crate::numeric::default_type(&substitute(
                        &f.values[value],
                        &self.generic_vars,
                    ));
                    let mut fused_arms = vec![];
                    let mut closed = false;
                    for (p, target) in arms {
                        // The last arm a fused read can reach takes the edge
                        // without testing the bounds check again.
                        fused_arms.push(p);
                        let last = self.seq_fused_exhaustive(*value, &fused_arms);
                        if last && !f.fip_join_slots.contains_key(target) {
                            self.jump(blocks[*target], &[]);
                            closed = true;
                            break;
                        }
                        let next = self.b.create_block();
                        let condition = match self.seq_fused_condition(*value, p) {
                            Some(condition) => condition,
                            None => self.pattern_condition(p, values[value], &ty)?,
                        };
                        let mut arm_args = Vec::new();
                        if let Some(joins) = f.fip_join_slots.get(target) {
                            for join in joins {
                                let val = if let Some((_, sb, si)) =
                                    join.incoming.iter().find(|&&(p_idx, _, _)| p_idx == i)
                                {
                                    self.use_var(self.reuse_slots[&(*sb, *si)])
                                } else {
                                    self.zero()
                                };
                                arm_args.push(val.into());
                            }
                        }
                        self.branch(condition, blocks[*target], &arm_args, next, &[]);
                        self.b.switch_to_block(next);
                    }
                    if !closed {
                        self.b.ins().trap(ir::TrapCode::unwrap_user(1));
                    }
                }
                Terminator::Unreachable => {
                    self.b.ins().trap(ir::TrapCode::unwrap_user(1));
                }
            }
        }
        self.exits.pop();
        Ok(())
    }
    /// `return value` from the graph lowered as `owner` (`Op::Exit`).
    fn exit_to(
        &mut self,
        owner: &str,
        value: ir::Value,
        ty: &Type,
        placed: bool,
    ) -> Result<(), String> {
        let target = self
            .exits
            .iter()
            .rev()
            .find(|t| t.owner == owner)
            .cloned()
            .ok_or_else(|| format!("native 'return' target '{owner}' is not being emitted"))?;
        let depth = self
            .exits
            .iter()
            .rposition(|t| t.owner == owner)
            .unwrap_or(0);
        self.seq_drop_exit(depth)?;
        let value = if ty.name() == "Never" {
            self.default_value(&target.result)?
        } else {
            value
        };
        let value = if let Some(output) = target.sret {
            if !placed {
                let layout = Layout::of(self.world, &target.result)?;
                self.copy_bytes(value, output, layout.size);
            }
            self.zero()
        } else {
            value
        };
        if let Some(join) = target.join {
            self.jump(join, &[value.into()]);
        } else {
            self.return_value(value);
        }
        // Whatever follows the jump is emitted into a block nothing reaches.
        let dead = self.b.create_block();
        self.b.switch_to_block(dead);
        Ok(())
    }
    fn default_value(&mut self, ty: &Type) -> Result<ir::Value, String> {
        let l = Layout::of(self.world, ty)?;
        Ok(if l.aggregate() {
            self.alloc(l.size)
        } else if ty.name() == "f64" {
            self.b.ins().f64const(0.0)
        } else if ty.name() == "f32" {
            self.b.ins().f32const(0.0)
        } else {
            self.zero()
        })
    }
    fn copy_bindings(
        &mut self,
        vars: &BTreeMap<String, Variable>,
        types: &BTreeMap<String, Type>,
    ) -> Result<BTreeMap<String, Variable>, String> {
        let mut copy = BTreeMap::new();
        for (n, var) in vars {
            let value = self.use_var(*var);
            let layout = Layout::of(self.world, types.get(n).ok_or("capture lacks a type")?)?;
            let value = if layout.aggregate() {
                let ptr = self.alloc(layout.size);
                self.copy_bytes(value, ptr, layout.size);
                ptr
            } else {
                value
            };
            let fresh = self.b.declare_var(self.b.func.dfg.value_type(value));
            self.def_var(fresh, value);
            copy.insert(n.clone(), fresh);
        }
        Ok(copy)
    }
    fn ty(&self, e: &Expr) -> Type {
        if !matches!(&e.kind,ExprKind::Name(n) if n.starts_with('$') || n.len() != e.span.end - e.span.start)
            && let Some(t @ Type::Function { .. }) =
                self.world.types.get(&(e.span.start, e.span.end))
        {
            return substitute(t, &self.generic_vars);
        }
        if let ExprKind::Name(n) = &e.kind
            && let Some(t) = self.local_types.get(n)
        {
            return t.clone();
        }
        if let ExprKind::Field(base, field) = &e.kind
            && let Some(t) = self
                .world
                .field(&self.ty(base), std::slice::from_ref(field))
        {
            return t;
        }
        crate::numeric::default_type(&substitute(
            self.world
                .types
                .get(&(e.span.start, e.span.end))
                .unwrap_or(&Type::Infer),
            &self.generic_vars,
        ))
    }
    /// Stack storage that is written before it is read, so it is not zeroed.
    fn alloc_uninit(&mut self, size: u32) -> ir::Value {
        let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            size.max(8),
            3,
        ));
        self.b
            .ins()
            .stack_addr(self.m.target_config().pointer_type(), slot, 0)
    }
    fn alloc(&mut self, size: u32) -> ir::Value {
        let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            size.max(8),
            3,
        ));
        let ptr = self
            .b
            .ins()
            .stack_addr(self.m.target_config().pointer_type(), slot, 0);
        for off in (0..size.max(8)).step_by(8) {
            let zero = self.zero();
            self.b
                .ins()
                .store(ir::MemFlags::new(), zero, ptr, off as i32);
        }
        ptr
    }
    fn copy_bytes(&mut self, from: ir::Value, to: ir::Value, size: u32) {
        self.write_info(to, self.info(from));
        for off in (0..size).step_by(8) {
            let v = self
                .b
                .ins()
                .load(types::I64, ir::MemFlags::new(), from, off as i32);
            self.b.ins().store(ir::MemFlags::new(), v, to, off as i32);
        }
    }
    fn load(&mut self, ptr: ir::Value, ty: &Type) -> Result<ir::Value, String> {
        if Layout::of(self.world, ty)?.aggregate() {
            Ok(ptr)
        } else {
            let value = self
                .b
                .ins()
                .load(scalar(ty, &[])?, ir::MemFlags::new(), ptr, 0);
            self.write_info(value, self.info(ptr));
            Ok(value)
        }
    }
    fn store(&mut self, ptr: ir::Value, value: ir::Value, ty: &Type) -> Result<(), String> {
        self.write_info(ptr, self.info(value));
        let layout = Layout::of(self.world, ty)?;
        if layout.aggregate() {
            self.copy_bytes(value, ptr, layout.size);
        } else {
            let value = self.coerce(value, Some(ty));
            self.b.ins().store(ir::MemFlags::new(), value, ptr, 0);
        }
        Ok(())
    }
    fn offset(&mut self, ptr: ir::Value, offset: i32) -> ir::Value {
        let field = self.b.ins().iadd_imm(ptr, i64::from(offset));
        self.infos.insert(
            field,
            self.info(ptr)
                .fields
                .get(&offset)
                .cloned()
                .unwrap_or_default(),
        );
        self.pointer_parents.insert(field, (ptr, offset));
        field
    }
    fn address(&mut self, e: &Expr) -> Result<ir::Value, String> {
        match &e.kind {
            ExprKind::Name(n) => {
                let var = *self.vars.get(n).ok_or("unknown native place")?;
                let t = self.local_types.get(n).ok_or("native place lacks type")?;
                if Layout::of(self.world, t)?.aggregate() {
                    Ok(self.use_var(var))
                } else if let Some(ptr) = self.pointer(var) {
                    Ok(ptr)
                } else if self.register_locals.contains(&var.as_u32()) {
                    // A scratch copy for this one use: the variable stays in
                    // a register, and a mutable use restores it from the
                    // slot afterwards (`call_function`, `perform`,
                    // `invoke_closure_body`).
                    let value = self.use_var(var);
                    let ptr = self.alloc_uninit(8);
                    self.b.ins().store(ir::MemFlags::new(), value, ptr, 0);
                    self.write_info(ptr, self.info(value));
                    Ok(ptr)
                } else {
                    let value = self.use_var(var);
                    let ptr = self.alloc(8);
                    self.b.ins().store(ir::MemFlags::new(), value, ptr, 0);
                    self.write_info(ptr, self.info(value));
                    self.set_pointer(var, ptr);
                    Ok(ptr)
                }
            }
            ExprKind::Field(base, field) => {
                let layout = Layout::of(self.world, &self.ty(base))?;
                let f = layout.field(field).ok_or_else(|| {
                    format!("unknown native field '{field}' in {}", self.ty(base))
                })?;
                let ptr = self.address(base)?;
                Ok(self.offset(ptr, f.offset))
            }
            _ => Err("native mutable argument requires a place".into()),
        }
    }
    fn bind(&mut self, p: &Pattern, value: ir::Value, ty: &Type) -> Result<(), String> {
        self.bind_with_box(false, p, value, ty)
    }
    fn bind_with_box(
        &mut self,
        inside_box: bool,
        p: &Pattern,
        value: ir::Value,
        ty: &Type,
    ) -> Result<(), String> {
        match &p.kind {
            PatternKind::Bind(n) => {
                let bound_value = if inside_box {
                    let layout = Layout::of(self.world, ty)?;
                    if layout.aggregate() {
                        let copy = self.alloc(layout.size);
                        self.copy_bytes(value, copy, layout.size);
                        copy
                    } else {
                        value
                    }
                } else {
                    value
                };
                let var = self.b.declare_var(self.b.func.dfg.value_type(bound_value));
                self.def_var(var, bound_value);
                self.vars.insert(n.clone(), var);
                self.local_types.insert(n.clone(), ty.clone());
                Ok(())
            }
            PatternKind::Wild => {
                if !self.in_fip_fn && crate::fip::type_contains_box(self.world, ty) {
                    self.drop_box_value(value, ty)?;
                }
                Ok(())
            }
            PatternKind::Literal(_) => Ok(()),
            PatternKind::Tuple(ps) => {
                if ps.is_empty() {
                    return Ok(());
                }
                let layout = Layout::of(self.world, ty)?;
                let LayoutKind::Product(fields) = layout.kind else {
                    return Err("native tuple pattern requires product layout".into());
                };
                for (p, f) in ps.iter().zip(fields) {
                    let ptr = self.offset(value, f.offset);
                    let v = self.load(ptr, &f.ty)?;
                    self.bind_with_box(inside_box, p, v, &f.ty)?;
                }
                Ok(())
            }
            PatternKind::Constructor { name, fields, .. } => {
                if crate::fip::is_box_pattern(self.world, p) {
                    let inner = crate::fip::box_inner(self.world, ty)
                        .cloned()
                        .ok_or("Box pattern expects Box[T]")?;
                    let inner_val = self.load(value, &inner)?;
                    self.bind_with_box(true, &fields[0].1, inner_val, &inner)?;
                    if !self.in_fip_fn {
                        self.fip_box_free(value)?;
                    }
                    return Ok(());
                }
                let layout = Layout::of(self.world, ty)?;
                let (_, fs) = layout
                    .variant(name)
                    .ok_or("native pattern has unknown variant")?;
                for (n, p) in fields {
                    let f = fs
                        .iter()
                        .find(|f| &f.name == n)
                        .ok_or("native pattern has unknown field")?;
                    let ptr = self.offset(value, f.offset);
                    let v = self.load(ptr, &f.ty)?;
                    self.bind_with_box(inside_box, p, v, &f.ty)?;
                }
                if !self.in_fip_fn {
                    for f in fs {
                        if !fields.iter().any(|(n, _)| n == &f.name)
                            && crate::fip::type_contains_box(self.world, &f.ty)
                        {
                            let ptr = self.offset(value, f.offset);
                            let v = self.load(ptr, &f.ty)?;
                            self.drop_box_value(v, &f.ty)?;
                        }
                    }
                }
                Ok(())
            }
        }
    }
    fn bind_alias(&mut self, p: &Pattern, ptr: ir::Value, ty: &Type) -> Result<(), String> {
        match &p.kind {
            PatternKind::Bind(n) => {
                let value = self.load(ptr, ty)?;
                self.bind(p, value, ty)?;
                if !Layout::of(self.world, ty)?.aggregate() {
                    self.set_pointer(self.vars[n], ptr);
                }
            }
            PatternKind::Tuple(ps) => {
                if let LayoutKind::Product(fs) = Layout::of(self.world, ty)?.kind {
                    for (p, f) in ps.iter().zip(fs) {
                        let field = self.offset(ptr, f.offset);
                        self.bind_alias(p, field, &f.ty)?;
                    }
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                if crate::fip::is_box_pattern(self.world, p) {
                    let inner = crate::fip::box_inner(self.world, ty)
                        .cloned()
                        .ok_or("Box pattern expects Box[T]")?;
                    let box_ptr = self.load(ptr, ty)?;
                    return self.bind_alias(&fields[0].1, box_ptr, &inner);
                }
                let layout = Layout::of(self.world, ty)?;
                let (_, fs) = layout
                    .variant(name)
                    .ok_or("unknown borrowed pattern constructor")?;
                for (n, p) in fields {
                    let f = fs
                        .iter()
                        .find(|f| &f.name == n)
                        .ok_or("unknown borrowed pattern field")?;
                    let field = self.offset(ptr, f.offset);
                    self.bind_alias(p, field, &f.ty)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn pattern_condition(
        &mut self,
        p: &Pattern,
        value: ir::Value,
        ty: &Type,
    ) -> Result<ir::Value, String> {
        Ok(match &p.kind {
            PatternKind::Wild | PatternKind::Bind(_) => self.b.ins().iconst(types::I8, 1),
            PatternKind::Literal(l) => match l {
                Literal::Int(n) => self.b.ins().icmp_imm(IntCC::Equal, value, *n as i64),
                Literal::Bool(b) => self.b.ins().icmp_imm(IntCC::Equal, value, i64::from(*b)),
                Literal::Float(n) => {
                    let ty = self.b.func.dfg.value_type(value);
                    let n = self.float_const(ty, *n);
                    self.b.ins().fcmp(FloatCC::Equal, value, n)
                }
                Literal::String(s) => {
                    let literal = self.string(s)?;
                    self.equal(value, literal, ty)?
                }
            },
            PatternKind::Tuple(ps) => {
                let layout = Layout::of(self.world, ty)?;
                let LayoutKind::Product(fs) = layout.kind else {
                    return Ok(self.b.ins().iconst(types::I8, i64::from(ps.is_empty())));
                };
                let mut result = self.b.ins().iconst(types::I8, 1);
                for (p, f) in ps.iter().zip(fs) {
                    let ptr = self.offset(value, f.offset);
                    let v = self.load(ptr, &f.ty)?;
                    let c = self.pattern_condition(p, v, &f.ty)?;
                    result = self.b.ins().band(result, c);
                }
                result
            }
            PatternKind::Constructor { name, fields, .. } => {
                if crate::fip::is_box_pattern(self.world, p) {
                    let inner = crate::fip::box_inner(self.world, ty)
                        .cloned()
                        .ok_or("Box pattern expects Box[T]")?;
                    let inner_val = self.load(value, &inner)?;
                    return self.pattern_condition(&fields[0].1, inner_val, &inner);
                }
                let layout = Layout::of(self.world, ty)?;
                let (tag, fs) = layout
                    .variant(name)
                    .ok_or("native pattern has unknown variant")?;
                if matches!(layout.kind, LayoutKind::Enum(_)) {
                    let actual = self.b.ins().load(types::I64, ir::MemFlags::new(), value, 0);
                    let matches_tag = self.b.ins().icmp_imm(IntCC::Equal, actual, tag as i64);
                    if fields.is_empty() {
                        matches_tag
                    } else {
                        let ok = self.b.create_block();
                        let fail = self.b.create_block();
                        let done = self.b.create_block();
                        self.b.append_block_param(done, types::I8);
                        self.b.ins().brif(matches_tag, ok, &[], fail, &[]);

                        self.b.switch_to_block(fail);
                        let zero = self.b.ins().iconst(types::I8, 0);
                        self.b.ins().jump(done, &[zero.into()]);

                        self.b.switch_to_block(ok);
                        let mut result = self.b.ins().iconst(types::I8, 1);
                        for (n, p) in fields {
                            let f = fs
                                .iter()
                                .find(|f| &f.name == n)
                                .ok_or("native pattern has unknown field")?;
                            let ptr = self.offset(value, f.offset);
                            let v = self.load(ptr, &f.ty)?;
                            let c = self.pattern_condition(p, v, &f.ty)?;
                            result = self.b.ins().band(result, c);
                        }
                        self.b.ins().jump(done, &[result.into()]);

                        self.b.switch_to_block(done);
                        self.b.block_params(done)[0]
                    }
                } else {
                    let mut result = self.b.ins().iconst(types::I8, 1);
                    for (n, p) in fields {
                        let f = fs
                            .iter()
                            .find(|f| &f.name == n)
                            .ok_or("native pattern has unknown field")?;
                        let ptr = self.offset(value, f.offset);
                        let v = self.load(ptr, &f.ty)?;
                        let c = self.pattern_condition(p, v, &f.ty)?;
                        result = self.b.ins().band(result, c);
                    }
                    result
                }
            }
        })
    }
    fn string(&mut self, text: &str) -> Result<ir::Value, String> {
        self.string_literal(text)
    }
    fn data(
        &mut self,
        name: Option<&str>,
        fields: Vec<(String, &Expr)>,
        ty: &Type,
    ) -> Result<ir::Value, String> {
        self.data_into(name, fields, ty, None, &BTreeSet::new())
    }
    /// Builds constructor `name` of `ty` in `into` (fresh storage when
    /// `None`); the fields in `built` were already built in place there
    /// ([`aggregates::plan`]).
    fn data_into(
        &mut self,
        name: Option<&str>,
        fields: Vec<(String, &Expr)>,
        ty: &Type,
        into: Option<ir::Value>,
        built: &BTreeSet<String>,
    ) -> Result<ir::Value, String> {
        let layout = Layout::of(self.world, ty)?;
        if !layout.aggregate() {
            return Ok(self.zero());
        }
        let (tag, fs) = if let Some(name) = name {
            let (tag, fs) = layout.variant(name).ok_or("unknown native constructor")?;
            (
                matches!(layout.kind, LayoutKind::Enum(_)).then_some(tag),
                fs,
            )
        } else {
            let LayoutKind::Product(fs) = &layout.kind else {
                return Err("tuple requires product layout".into());
            };
            (None, fs.as_slice())
        };
        let ptr = if self.operands_only {
            // Zero only the words of this constructor that no field (or
            // the tag) overwrites whole, such as the word of an `f32` field,
            // whose store writes only its low half. The words past the
            // variant's own fields are its inactive payload: no operation
            // reads them (manuscript §16.6, "inactive payload"), so they
            // are left as they are.
            let ptr = match into {
                Some(ptr) => ptr,
                None => self.alloc_uninit(layout.size),
            };
            let mut covered = BTreeSet::new();
            if tag.is_some() {
                covered.insert(0);
            }
            for (n, _) in &fields {
                if let Some(f) = fs.iter().find(|f| f.name == *n)
                    && f.ty.name() != "f32"
                {
                    let start = f.offset as u32;
                    covered.extend((start..start + f.layout.size).step_by(8));
                }
            }
            let end = if tag.is_some() {
                fs.iter()
                    .map(|f| f.offset as u32 + f.layout.size)
                    .max()
                    .unwrap_or(8)
            } else {
                layout.size.max(8)
            };
            for off in (0..end).step_by(8) {
                if !covered.contains(&off) {
                    let zero = self.zero();
                    self.b
                        .ins()
                        .store(ir::MemFlags::new(), zero, ptr, off as i32);
                }
            }
            ptr
        } else {
            self.alloc(layout.size)
        };
        if let Some(tag) = tag {
            let tag = self.b.ins().iconst(types::I64, tag as i64);
            self.b.ins().store(ir::MemFlags::new(), tag, ptr, 0);
        }
        for (n, e) in fields {
            if built.contains(&n) {
                continue;
            }
            let f = fs
                .iter()
                .find(|f| f.name == n)
                .ok_or("unknown native constructor field")?;
            // An aggregate local is copied straight into its field; reading
            // it as an expression would first copy it into a temporary.
            let direct = match &e.kind {
                ExprKind::Name(local)
                    if self.operands_only
                        && f.layout.aggregate()
                        && !self.world.functions.contains_key(local)
                        && self.world.constructor(local).is_none() =>
                {
                    self.vars.get(local).copied()
                }
                _ => None,
            };
            let value = match direct {
                Some(var) => self.use_var(var),
                None => self.expr(e)?,
            };
            let dest = self.offset(ptr, f.offset);
            self.store(dest, value, &f.ty)?;
        }
        Ok(ptr)
    }
    fn equal(&mut self, a: ir::Value, b: ir::Value, ty: &Type) -> Result<ir::Value, String> {
        emit_equal(&mut self.b, self.m, self.world, a, b, ty)
    }
    /// Lower `e` with its source span as the location of the instructions
    /// it emits ([`CodegenOptions::source_locations`]); the enclosing
    /// location is restored afterwards, so the rest of the parent's code
    /// keeps the parent's span. Synthetic expressions (`0..0`) keep the
    /// enclosing location.
    fn expr(&mut self, e: &Expr) -> Result<ir::Value, String> {
        if !self.source_locations || e.span.end == 0 {
            return self.expr_at(e);
        }
        let saved = self.srcloc;
        self.set_location(e.span.start);
        let result = self.expr_at(e);
        self.srcloc = saved;
        self.b.set_srcloc(saved);
        result
    }
    fn set_location(&mut self, offset: usize) {
        if self.source_locations {
            self.srcloc = source_loc(offset);
            self.b.set_srcloc(self.srcloc);
        }
    }
    fn expr_at(&mut self, e: &Expr) -> Result<ir::Value, String> {
        let value = match &e.kind {
            ExprKind::Literal(l) => match l {
                Literal::Int(n) => self.b.ins().iconst(types::I64, *n as i64),
                // An f32 literal is the f32 nearest its f64 value
                // (`vesper.float.ieee754/1`).
                Literal::Float(n) if self.ty(e).name() == "f32" => self.b.ins().f32const(*n as f32),
                Literal::Float(n) => self.b.ins().f64const(*n),
                Literal::Bool(b) => self.b.ins().iconst(types::I64, i64::from(*b)),
                Literal::String(s) => self.string(s)?,
            },
            ExprKind::Name(n) => {
                if self.world.functions.contains_key(n) && !self.vars.contains_key(n) {
                    return self.function_tag(n, &self.ty(e));
                }
                if self.world.constructor(n).is_some() {
                    return self.data(Some(n), vec![], &self.ty(e));
                }
                let v = self
                    .vars
                    .get(n)
                    .ok_or_else(|| format!("native value '{n}' is not a scalar local"))?;
                let value = self.use_var(*v);
                let ty = self.ty(e);
                let layout = Layout::of(self.world, &ty)?;
                if layout.aggregate() {
                    // The copy writes every word, so the optimize path
                    // does not zero it first.
                    let ptr = if self.operands_only {
                        self.alloc_uninit(layout.size)
                    } else {
                        self.alloc(layout.size)
                    };
                    self.copy_bytes(value, ptr, layout.size);
                    ptr
                } else if matches!(ty, Type::Function { .. }) {
                    self.qualify_tag(value, &ty)
                } else {
                    value
                }
            }
            ExprKind::Tuple(es) => self.data(
                None,
                es.iter()
                    .enumerate()
                    .map(|(i, e)| (i.to_string(), e))
                    .collect(),
                &self.ty(e),
            )?,
            ExprKind::Struct { name, fields } => self.data(
                Some(name),
                fields.iter().map(|(n, e)| (n.clone(), e)).collect(),
                &self.ty(e),
            )?,
            ExprKind::Field(base, field) => {
                let ptr = if crate::check::Place::from_expr(e).is_some() {
                    self.address(e)?
                } else {
                    let layout = Layout::of(self.world, &self.ty(base))?;
                    let f = layout.field(field).ok_or_else(|| {
                        format!("unknown native field '{field}' in {}", self.ty(base))
                    })?;
                    let base = self.expr(base)?;
                    self.offset(base, f.offset)
                };
                let ty = self.ty(e);
                let layout = Layout::of(self.world, &ty)?;
                let value = self.load(ptr, &ty)?;
                if layout.aggregate() {
                    let copy = self.alloc(layout.size);
                    self.copy_bytes(value, copy, layout.size);
                    copy
                } else {
                    value
                }
            }
            ExprKind::Index(..) if let Some(result) = self.seq_ast_expr(e) => result?,
            ExprKind::Index(base, index) => {
                let layout = Layout::of(self.world, &self.ty(base))?;
                let LayoutKind::Product(fs) = layout.kind else {
                    return Err("native indexing requires tuple".into());
                };
                // An element of an array place (§5) is read where it lies,
                // without copying the whole array first.
                let ptr = if crate::check::Place::from_expr(base).is_some() {
                    self.address(base)?
                } else {
                    self.expr(base)?
                };
                let index = self.expr(index)?;
                let valid = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::UnsignedLessThan, index, fs.len() as i64);
                let good = self.b.create_block();
                let bad = self.b.create_block();
                self.branch(valid, good, &[], bad, &[]);
                self.b.switch_to_block(bad);
                if self.world.partial_ops.contains(&(e.span.start, e.span.end)) {
                    self.primitive_failure("IndexOutOfBounds")?;
                    self.jump(good, &[]);
                } else {
                    self.b.ins().trap(ir::TrapCode::unwrap_user(2));
                }
                self.b.switch_to_block(good);
                let f = fs.first().ok_or("index into empty tuple")?;
                let offset = self.b.ins().imul_imm(index, f.layout.size as i64);
                let address = self.b.ins().iadd(ptr, offset);
                let mut info = NativeInfo::default();
                for field in self.info(ptr).fields.values() {
                    info.merge(field);
                }
                self.write_info(address, info);
                let value = self.load(address, &f.ty)?;
                if f.layout.aggregate() {
                    let copy = self.alloc_uninit(f.layout.size);
                    self.copy_bytes(value, copy, f.layout.size);
                    copy
                } else {
                    value
                }
            }
            ExprKind::Block { statements, tail } => {
                let scope = self.vars.clone();
                let type_scope = self.local_types.clone();
                for s in statements {
                    match s {
                        Statement::Let { pat, value, .. } => {
                            let v = self.expr(value)?;
                            let ty = self.ty(value);
                            self.bind(pat, v, &ty)?;
                        }
                        Statement::Expr(statement) => {
                            self.expr(statement)?;
                            if self.ty(statement).name() == "Never" {
                                self.vars = scope;
                                self.local_types = type_scope;
                                return self.default_value(&self.ty(e));
                            }
                        }
                    }
                }
                let out = if let Some(t) = tail {
                    self.expr(t)?
                } else {
                    self.zero()
                };
                self.vars = scope;
                self.local_types = type_scope;
                out
            }
            ExprKind::Unary(op, v) if op == "~" => {
                let v = self.expr(v)?;
                self.b.ins().bnot(v)
            }
            ExprKind::Unary(op, v)
                if crate::numeric::float::is_float_intrinsic(op)
                    || (crate::numeric::conversion(op).is_some()
                        && crate::numeric::FloatKind::from_type(&self.ty(v)).is_some()) =>
            {
                // Place-IR form of a one-operand float intrinsic.
                let from = self.ty(v);
                let value = self.expr(v)?;
                let r = self
                    .float_unary(op, &from, value, &e.span)?
                    .ok_or("invalid native float intrinsic")?;
                self.coerce(r, Some(&self.ty(e)))
            }
            ExprKind::Binary(op, a, b) if crate::numeric::float::math_intrinsic(op).is_some() => {
                // Place-IR form of `min`/`max`.
                let flush = self.flushes(&e.span);
                let av = self.expr(a)?;
                let av = self.flush(flush, av);
                let bv = self.expr(b)?;
                let bv = self.flush(flush, bv);
                let op = crate::numeric::float::math_intrinsic(op).unwrap();
                let r = self.math(op, &[av, bv])?;
                self.flush(flush, r)
            }
            ExprKind::Unary(op, v) if crate::integer_widths::bit_builtin(op).is_some() => {
                // Place-IR form of a bit count (`integer_widths`).
                let value = self.expr(v)?;
                self.bit_builtin(op, value, None)?
            }
            ExprKind::Unary(op, v) if crate::numeric::conversion(op).is_some() => {
                let from = crate::numeric::IntegerKind::from_type(&self.ty(v));
                let value = self.expr(v)?;
                self.convert(op, from, value, &e.span)?
            }
            ExprKind::Unary(op, v) => {
                let kind = crate::numeric::IntegerKind::from_type(&self.ty(v));
                let v = self.expr(v)?;
                let float = is_float_type(self.b.func.dfg.value_type(v));
                let flush = float && self.flushes(&e.span);
                let v = self.flush(flush, v);
                if op == "-"
                    && let Some(kind) = kind
                    && self
                        .world
                        .overflow_ops
                        .contains(&(e.span.start, e.span.end))
                {
                    // Signed MIN and every nonzero unsigned value overflow.
                    let overflow = if kind.unsigned() {
                        self.b.ins().icmp_imm(IntCC::NotEqual, v, 0)
                    } else {
                        self.b.ins().icmp_imm(IntCC::Equal, v, kind.min() as i64)
                    };
                    self.check_overflow(overflow)?;
                }
                match (op.as_str(), float) {
                    ("-", false) => self.b.ins().ineg(v),
                    ("-", true) => {
                        let r = self.b.ins().fneg(v);
                        self.flush(flush, r)
                    }
                    ("!", false) => {
                        let b = self.b.ins().icmp_imm(IntCC::Equal, v, 0);
                        self.b.ins().uextend(types::I64, b)
                    }
                    // Place-IR form of `widen_u64`: unsigned values are
                    // already zero-extended in I64.
                    (crate::capability_effects::WIDEN_INTRINSIC, false) => v,
                    _ => return Err("invalid native unary op".into()),
                }
            }
            ExprKind::Binary(op, a, b) if crate::integer_widths::bit_builtin(op).is_some() => {
                // Place-IR form of `wrapping_shl_k`/`wrapping_shr_k`.
                let av = self.expr(a)?;
                let bv = self.expr(b)?;
                self.bit_builtin(op, av, Some(bv))?
            }
            ExprKind::Binary(op, a, b) if op == "&&" || op == "||" => {
                let lit = Expr {
                    kind: ExprKind::Literal(Literal::Bool(op == "||")),
                    span: e.span.clone(),
                };
                let (then, otherwise) = if op == "&&" {
                    (b.clone(), Box::new(lit))
                } else {
                    (Box::new(lit), b.clone())
                };
                self.expr(&Expr {
                    kind: ExprKind::If {
                        condition: a.clone(),
                        then,
                        otherwise: Some(otherwise),
                    },
                    span: e.span.clone(),
                })?
            }
            ExprKind::Binary(op, a, b)
                if crate::numeric::is_shift(op)
                    || crate::numeric::rotate_intrinsic(op).is_some() =>
            {
                let kind = crate::numeric::IntegerKind::from_type(&self.ty(a))
                    .ok_or("native shift of a non-integer value")?;
                let av = self.expr(a)?;
                let bv = self.expr(b)?;
                self.shift(op, kind, av, bv, &e.span)?
            }
            ExprKind::Binary(op, a, b) if matches!(op.as_str(), "&" | "|" | "^") => {
                let av = self.expr(a)?;
                let bv = self.expr(b)?;
                match op.as_str() {
                    "&" => self.b.ins().band(av, bv),
                    "|" => self.b.ins().bor(av, bv),
                    _ => self.b.ins().bxor(av, bv),
                }
            }
            ExprKind::Binary(op, a, b)
                if crate::strings::is_comparison(op)
                    && (crate::strings::is_text(&self.ty(a))
                        || crate::strings::is_text(&self.ty(b))) =>
            {
                let av = self.expr(a)?;
                let bv = self.expr(b)?;
                self.str_compare(op, av, bv)?
            }
            ExprKind::Binary(op, a, b) => {
                let ty = self.ty(a);
                let av = self.expr(a)?;
                let bv = self.expr(b)?;
                if matches!(op.as_str(), "==" | "!=")
                    && (Layout::of(self.world, &ty)?.aggregate()
                        || matches!(ty, Type::Function { .. })
                        || crate::fip::is_box_type(self.world, &ty))
                {
                    let eq = self.equal(av, bv, &ty)?;
                    let eq = if op == "!=" {
                        self.b.ins().icmp_imm(IntCC::Equal, eq, 0)
                    } else {
                        eq
                    };
                    self.b.ins().uextend(types::I64, eq)
                } else {
                    let kind = crate::numeric::IntegerKind::from_type(&ty);
                    let unsigned = kind.is_some_and(|k| k.unsigned());
                    if matches!(op.as_str(), "/" | "%")
                        && self.world.partial_ops.contains(&(e.span.start, e.span.end))
                    {
                        self.check_divisor(bv)?;
                    }
                    if let Some(kind) = kind
                        && self
                            .world
                            .overflow_ops
                            .contains(&(e.span.start, e.span.end))
                    {
                        self.checked_arithmetic(op, kind, av, bv)
                    } else if crate::capability_effects::bit_intrinsic(op).is_some() {
                        // Place-IR form of a bitwise intrinsic call.
                        let v = self.bitwise(op, kind, av, bv)?;
                        Ok(self.coerce(v, Some(&ty)))
                    } else {
                        let flush = self.flushes(&e.span);
                        let av = self.flush(flush, av);
                        let bv = self.flush(flush, bv);
                        let r = self.binary(op, av, bv, unsigned)?;
                        Ok(self.flush(flush, r))
                    }?
                }
            }
            ExprKind::Assign(op, l, r) if op.starts_with(crate::arrays::WRITE_PREFIX) => {
                self.array_write(e, op, l, r)?
            }
            ExprKind::Assign(op, l, r) => {
                let ty = self.ty(l);
                let layout = Layout::of(self.world, &ty)?;
                let value = self.expr(r)?;
                if matches!(op.as_str(), "/=" | "%=")
                    && self.world.partial_ops.contains(&(e.span.start, e.span.end))
                {
                    self.check_divisor(value)?;
                }
                // Checked compound assignment raises before its store.
                let checked = crate::numeric::IntegerKind::from_type(&ty).filter(|_| {
                    self.world
                        .overflow_ops
                        .contains(&(e.span.start, e.span.end))
                });
                if let ExprKind::Name(n) = &l.kind {
                    let var = *self.vars.get(n).ok_or("unknown native local")?;
                    let value = if op == "=" {
                        value
                    } else {
                        let old = self.use_var(var);
                        let unsigned = crate::numeric::IntegerKind::from_type(&ty)
                            .is_some_and(|k| k.unsigned());
                        let v = if let Some(kind) = checked {
                            self.checked_arithmetic(&op[..1], kind, old, value)?
                        } else {
                            let flush = self.flushes(&e.span);
                            let old = self.flush(flush, old);
                            let value = self.flush(flush, value);
                            let r = self.binary(&op[..1], old, value, unsigned)?;
                            self.flush(flush, r)
                        };
                        self.coerce(v, Some(&ty))
                    };
                    if layout.aggregate() {
                        let ptr = self.use_var(var);
                        self.store(ptr, value, &ty)?;
                    } else {
                        self.def_var(var, value);
                        if let Some(destination) = self.pointer(var) {
                            self.store(destination, value, &ty)?;
                        }
                    }
                } else {
                    let ptr = self.address(l)?;
                    let value = if op == "=" {
                        value
                    } else {
                        let old = self.load(ptr, &ty)?;
                        let unsigned = crate::numeric::IntegerKind::from_type(&ty)
                            .is_some_and(|k| k.unsigned());
                        if let Some(kind) = checked {
                            self.checked_arithmetic(&op[..1], kind, old, value)?
                        } else {
                            let flush = self.flushes(&e.span);
                            let old = self.flush(flush, old);
                            let value = self.flush(flush, value);
                            let r = self.binary(&op[..1], old, value, unsigned)?;
                            self.flush(flush, r)
                        }
                    };
                    self.store(ptr, value, &ty)?;
                }
                self.zero()
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let condition = self.expr(condition)?;
                let yes = self.b.create_block();
                let no = self.b.create_block();
                let join = self.b.create_block();
                self.branch(condition, yes, &[], no, &[]);
                let scope = self.vars.clone();
                self.b.switch_to_block(yes);
                let a = self.expr(then)?;
                let a = if self.ty(then).name() == "Never" {
                    self.default_value(&self.ty(e))?
                } else {
                    a
                };
                let ty = self.b.func.dfg.value_type(a);
                self.b.append_block_param(join, ty);
                self.jump(join, &[a.into()]);
                self.vars = scope.clone();
                self.b.switch_to_block(no);
                let b = if let Some(other) = otherwise {
                    self.expr(other)?
                } else {
                    self.zero()
                };
                let b = if otherwise
                    .as_ref()
                    .is_some_and(|e| self.ty(e).name() == "Never")
                {
                    self.default_value(&self.ty(e))?
                } else {
                    b
                };
                if self.b.func.dfg.value_type(b) != ty {
                    return Err("native branch type mismatch".into());
                }
                self.jump(join, &[b.into()]);
                self.vars = scope;
                self.b.switch_to_block(join);
                self.b.block_params(join)[0]
            }
            ExprKind::Match { value, arms } => {
                let ty = self.ty(value);
                let v = self.expr(value)?;
                let join = self.b.create_block();
                let scope = self.vars.clone();
                let type_scope = self.local_types.clone();
                let mut result_ty = None;
                for (p, body) in arms {
                    let yes = self.b.create_block();
                    let no = self.b.create_block();
                    let condition = self.pattern_condition(p, v, &ty)?;
                    self.branch(condition, yes, &[], no, &[]);
                    self.b.switch_to_block(yes);
                    self.bind(p, v, &ty)?;
                    let result = self.expr(body)?;
                    let result = if self.ty(body).name() == "Never" {
                        self.default_value(&self.ty(e))?
                    } else {
                        result
                    };
                    let ty = self.b.func.dfg.value_type(result);
                    if let Some(expected) = result_ty {
                        if expected != ty {
                            return Err("native match type mismatch".into());
                        }
                    } else {
                        self.b.append_block_param(join, ty);
                        result_ty = Some(ty);
                    }
                    self.jump(join, &[result.into()]);
                    self.vars = scope.clone();
                    self.local_types = type_scope.clone();
                    self.b.switch_to_block(no);
                }
                self.b.ins().trap(ir::TrapCode::unwrap_user(1));
                if result_ty.is_none() {
                    return Err("native empty match has no reachable result".into());
                }
                self.b.switch_to_block(join);
                self.vars = scope;
                self.local_types = type_scope;
                self.b.block_params(join)[0]
            }
            ExprKind::Call {
                callee,
                args,
                types: instance,
            } => {
                if let Some(result) = self.tiled_ast_call(callee, instance, args, e) {
                    return result;
                }
                if let Some(result) = self.seq_ast_expr(e) {
                    return result;
                }
                if let ExprKind::Name(n) = &callee.kind {
                    if let Some(var) = self.vars.get(n) {
                        let tag = self.use_var(*var);
                        return self.invoke_tag(tag, &self.ty(callee), args, e);
                    }
                    if !self.world.functions.contains_key(n)
                        && let Some(op) = crate::vector::VectorOp::resolve(
                            n,
                            instance,
                            &args.iter().map(|a| &a.expr).collect::<Vec<_>>(),
                            &args.iter().map(|a| self.ty(&a.expr)).collect::<Vec<_>>(),
                            &self.ty(e),
                        )
                    {
                        let mut operands = vec![];
                        for (i, a) in args.iter().enumerate() {
                            if crate::vector::is_operand(n, i, args.len()) {
                                operands.push(self.expr(&a.expr)?);
                            }
                        }
                        return self.vector_op(&op, &operands);
                    }
                    if crate::numeric::wrapping_intrinsic(n).is_some()
                        && !self.world.functions.contains_key(n)
                        && args.len() == 2
                    {
                        let a = self.expr(&args[0].expr)?;
                        let b = self.expr(&args[1].expr)?;
                        let v = self.binary(n, a, b, false)?;
                        return Ok(self.coerce(v, Some(&self.ty(e))));
                    }
                    // Bitwise intrinsics of the secret operations
                    // (`capability_effects`).
                    if !self.world.functions.contains_key(n) {
                        if crate::capability_effects::bit_intrinsic(n).is_some() && args.len() == 2
                        {
                            let kind = crate::numeric::IntegerKind::from_type(&self.ty(e));
                            let a = self.expr(&args[0].expr)?;
                            let b = self.expr(&args[1].expr)?;
                            let v = self.bitwise(n, kind, a, b)?;
                            return Ok(self.coerce(v, Some(&self.ty(e))));
                        }
                        if let Some(v) = self.float_call(n, args, e)? {
                            return Ok(v);
                        }
                        if crate::numeric::conversion(n).is_some() && args.len() == 1 {
                            let from =
                                crate::numeric::IntegerKind::from_type(&self.ty(&args[0].expr));
                            let value = self.expr(&args[0].expr)?;
                            let v = self.convert(n, from, value, &e.span)?;
                            return Ok(self.coerce(v, Some(&self.ty(e))));
                        }
                        if crate::numeric::rotate_intrinsic(n).is_some() && args.len() == 2 {
                            let kind =
                                crate::numeric::IntegerKind::from_type(&self.ty(&args[0].expr))
                                    .ok_or("native rotation of a non-integer value")?;
                            let a = self.expr(&args[0].expr)?;
                            let b = self.expr(&args[1].expr)?;
                            let v = self.shift(n, kind, a, b, &e.span)?;
                            return Ok(self.coerce(v, Some(&self.ty(e))));
                        }
                    }
                    if crate::fip::active(self.world)
                        && (n == crate::fip::BOX_NEW || n == crate::fip::BOX_NEW_IN_CALLER)
                        && args.len() == 1
                    {
                        let val = self.expr(&args[0].expr)?;
                        let fip_op = if n == crate::fip::BOX_NEW {
                            crate::fip::FipOp::BoxNew
                        } else {
                            crate::fip::FipOp::BoxNewInCaller
                        };
                        let actual = self.ty(&args[0].expr);
                        return self.fip_op(&fip_op, &[val], &self.ty(e), Some(&actual));
                    }
                    if self.world.constructor(n).is_some() {
                        self.data(
                            Some(n),
                            args.iter()
                                .enumerate()
                                .map(|(i, a)| (i.to_string(), &a.expr))
                                .collect(),
                            &self.ty(e),
                        )?
                    } else if self.world.functions.contains_key(n) {
                        self.call_function(n, args, e)?
                    } else if let Some((effect, op)) = n.split_once("::") {
                        self.perform(effect, None, instance, op, args)?
                    } else {
                        return Err(format!("unknown native callee '{n}'"));
                    }
                } else if let ExprKind::Field(base, method) = &callee.kind {
                    let label = if let ExprKind::Name(n) = &base.kind {
                        Some(n.as_str())
                    } else {
                        None
                    };
                    if let Some(effect) = self
                        .handlers
                        .iter()
                        .rev()
                        .find(|h| {
                            label.is_some_and(|l| {
                                self.capabilities.get(&format!("$label:{l}")) == Some(&h.identity)
                            })
                        })
                        .map(|h| h.effect.name.clone())
                    {
                        self.perform(&effect, label, &[], method, args)?
                    } else {
                        let target = format!("{}::{method}", self.ty(base).name());
                        if !self.world.functions.contains_key(&target) {
                            return Err(format!("unknown native method '{target}'"));
                        }
                        let mut actual = vec![Arg {
                            pass: Pass::In,
                            explicit: false,
                            expr: *base.clone(),
                        }];
                        actual.extend(args.clone());
                        self.call_function(&target, &actual, e)?
                    }
                } else {
                    return Err("native indirect calls are not yet available".into());
                }
            }
            ExprKind::Try(value) => {
                let ty = self.ty(value);
                let layout = Layout::of(self.world, &ty)?;
                let (_, ok) = layout.variant("Ok").ok_or("'?' requires Result")?;
                let (_, err) = layout.variant("Err").ok_or("'?' requires Result")?;
                let ptr = self.expr(value)?;
                let tag = self.b.ins().load(types::I64, ir::MemFlags::new(), ptr, 0);
                let condition = self.b.ins().icmp_imm(IntCC::Equal, tag, 0);
                let yes = self.b.create_block();
                let no = self.b.create_block();
                let join = self.b.create_block();
                let result_ty = if Layout::of(self.world, &ok[0].ty)?.aggregate() {
                    self.m.target_config().pointer_type()
                } else {
                    scalar(&ok[0].ty, &[])?
                };
                self.b.append_block_param(join, result_ty);
                self.branch(condition, yes, &[], no, &[]);
                self.b.switch_to_block(yes);
                let value_ptr = self.offset(ptr, ok[0].offset);
                let result = self.load(value_ptr, &ok[0].ty)?;
                self.jump(join, &[result.into()]);
                self.b.switch_to_block(no);
                let error_ptr = self.offset(ptr, err[0].offset);
                let error = self.load(error_ptr, &err[0].ty)?;
                let instance = [err[0].ty.clone()];
                self.perform_values("Fail", None, &instance, "raise", vec![error], vec![])?;
                let unreachable = self.default_value(&ok[0].ty)?;
                self.jump(join, &[unreachable.into()]);
                self.b.switch_to_block(join);
                self.b.block_params(join)[0]
            }
            ExprKind::Closure { params, body, .. } => self.closure_tag(e, params, body)?,
            ExprKind::Loop(_)
            | ExprKind::For { .. }
            | ExprKind::Range { .. }
            | ExprKind::Break(_)
            | ExprKind::Return(_) => {
                return Err("native loops and early exits are lowered from Place IR".into());
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                let output_type = self.ty(e);
                let output_layout = Layout::of(self.world, &output_type)?;
                let exit = self.b.create_block();
                self.b.append_block_param(
                    exit,
                    if output_layout.aggregate() {
                        self.m.target_config().pointer_type()
                    } else {
                        scalar(&output_type, &[])?
                    },
                );
                let control = {
                    let dispatch = self.b.create_block();
                    self.b.append_block_param(
                        dispatch,
                        self.b.func.dfg.value_type(self.b.block_params(exit)[0]),
                    );
                    let selector = self.b.declare_var(types::I64);
                    let zero = self.zero();
                    self.def_var(selector, zero);
                    HandlerControl {
                        selector,
                        dispatch,
                        returns: Rc::new(RefCell::new(vec![exit])),
                    }
                };
                let captured = self.vars.clone();
                let captured_types = self.local_types.clone();
                let state_type = state.as_ref().map(|(_, s)| self.ty(s));
                let state = if let Some((_, s)) = state {
                    let value = self.expr(s)?;
                    let var = self.b.declare_var(self.b.func.dfg.value_type(value));
                    self.def_var(var, value);
                    Some(var)
                } else {
                    None
                };
                let mut effect = effect.clone();
                effect.args = effect
                    .args
                    .iter()
                    .map(|t| substitute(t, &self.generic_vars))
                    .collect();
                let mut free = crate::check::clause_free_names(ret);
                for op in ops {
                    free.extend(crate::check::clause_free_names(op));
                }
                let capture_types = captured_types
                    .iter()
                    .filter(|(n, _)| free.contains(*n))
                    .map(|(n, t)| (n.clone(), t.clone()))
                    .collect::<BTreeMap<_, _>>();
                let capture_variables = captured
                    .iter()
                    .filter(|(n, _)| free.contains(*n))
                    .map(|(n, v)| (n.clone(), *v))
                    .collect::<BTreeMap<_, _>>();
                let capture_variables = self.copy_bindings(&capture_variables, &capture_types)?;
                let outer_caps = self.capabilities.clone();
                let identity = format!("native-cap{}", self.next_capability);
                self.next_capability += 1;
                self.capabilities
                    .insert(format!("$effect:{}", effect.name), identity.clone());
                if let Some(key) = crate::numeric::instance_key(&effect.name, &effect.args) {
                    self.capabilities.insert(key, identity.clone());
                }
                self.capabilities
                    .insert(format!("$static:$cap{}", e.span.start), identity.clone());
                if let Some(label) = &effect.label {
                    self.capabilities
                        .insert(format!("$label:{label}"), identity.clone());
                }
                let region = self.region_enter(&effect, ret)?;
                self.handlers.push(Handler {
                    identity,
                    capabilities: outer_caps.clone(),
                    output_type: output_type.clone(),
                    effect,
                    state,
                    region,
                    state_type: state_type.clone(),
                    ops: ops.clone(),
                    captured: capture_variables.clone(),
                    captured_types: capture_types.clone(),
                    generic_vars: self.generic_vars.clone(),
                    control: control.clone(),
                });
                let body_value = self.shared_control("handler_body", body)?;
                self.region_exit(region)?;
                let body_type = self.ty(body);
                self.handlers.pop();
                self.capabilities = outer_caps;
                self.vars = self.copy_bindings(&capture_variables, &capture_types)?;
                self.local_types = capture_types;
                if let Some(state) = state {
                    let st = state_type.unwrap();
                    let tuple_type = Type::Tuple(vec![body_type.clone(), st.clone()]);
                    let layout = Layout::of(self.world, &tuple_type)?;
                    let ptr = self.alloc(layout.size);
                    let LayoutKind::Product(fs) = layout.kind else {
                        unreachable!()
                    };
                    let a = self.offset(ptr, fs[0].offset);
                    self.store(a, body_value, &body_type)?;
                    let b = self.offset(ptr, fs[1].offset);
                    let value = self.use_var(state);
                    self.store(b, value, &st)?;
                    self.bind(&ret.pats[0], ptr, &tuple_type)?;
                } else {
                    self.bind(&ret.pats[0], body_value, &body_type)?;
                }
                let result = self.shared_control("handler_clause", &ret.body)?;
                let result = if body_type.name() == "Never" {
                    self.default_value(&output_type)?
                } else {
                    result
                };
                {
                    self.jump(control.dispatch, &[result.into()]);
                    self.b.switch_to_block(control.dispatch);
                    let output = self.b.block_params(control.dispatch)[0];
                    let selector = self.use_var(control.selector);
                    for (index, target) in control.returns.borrow().iter().enumerate() {
                        let next = self.b.create_block();
                        let condition = self.b.ins().icmp_imm(IntCC::Equal, selector, index as i64);
                        self.branch(condition, *target, &[output.into()], next, &[]);
                        self.b.switch_to_block(next);
                    }
                    self.b.ins().trap(ir::TrapCode::unwrap_user(4));
                }
                self.b.switch_to_block(exit);
                self.vars = captured;
                self.local_types = captured_types;
                self.b.block_params(exit)[0]
            }
        };
        let ty = self.ty(e);
        Ok(self.coerce(value, Some(&ty)))
    }
    /// Reduce a 64-bit lane to its canonical `t` form: narrow unsigned kinds
    /// are zero-extended and narrow signed kinds sign-extended.
    fn coerce(&mut self, v: ir::Value, t: Option<&Type>) -> ir::Value {
        if let Some(kind) = t.and_then(crate::numeric::IntegerKind::from_type)
            && kind.bits() < 64
            && self.b.func.dfg.value_type(v) == types::I64
        {
            if self.operands_only {
                // A narrow move (`mov r32, r32`, `movzx`, `movsx`) instead of
                // a mask the x86-64 encoder loads from the constant pool.
                let narrow = match kind.bits() {
                    8 => types::I8,
                    16 => types::I16,
                    _ => types::I32,
                };
                let low = self.b.ins().ireduce(narrow, v);
                return if kind.unsigned() {
                    self.b.ins().uextend(types::I64, low)
                } else {
                    self.b.ins().sextend(types::I64, low)
                };
            }
            if kind.unsigned() {
                return self.b.ins().band_imm(v, kind.mask() as i64);
            }
            let up = i64::from(64 - kind.bits());
            let high = self.b.ins().ishl_imm(v, up);
            return self.b.ins().sshr_imm(high, up);
        }
        v
    }
    /// `<<`, `>>` (raising `Fail[ArithOverflow]` when the checker could not
    /// prove the amount in range) and `rotate_left`/`rotate_right` on a
    /// canonical `kind` lane. Callers reduce the result with `coerce`.
    fn shift(
        &mut self,
        op: &str,
        kind: crate::numeric::IntegerKind,
        a: ir::Value,
        n: ir::Value,
        span: &Span,
    ) -> Result<ir::Value, String> {
        if crate::numeric::is_shift(op) && self.world.overflow_ops.contains(&(span.start, span.end))
        {
            // Negative amounts are huge as unsigned lanes.
            let bad =
                self.b
                    .ins()
                    .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, n, i64::from(kind.bits()));
            self.check_overflow(bad)?;
        }
        Ok(match op {
            "<<" => self.b.ins().ishl(a, n),
            ">>" if kind.unsigned() => self.b.ins().ushr(a, n),
            ">>" => self.b.ins().sshr(a, n),
            _ => {
                let left = crate::numeric::rotate_intrinsic(op).ok_or("unknown native shift")?;
                // Rotate in the kind's own width; Cranelift takes the
                // amount modulo the width, matching `rem_euclid`.
                let narrow = match kind.bits() {
                    8 => types::I8,
                    16 => types::I16,
                    32 => types::I32,
                    _ => types::I64,
                };
                let x = if narrow == types::I64 {
                    a
                } else {
                    self.b.ins().ireduce(narrow, a)
                };
                let r = if left {
                    self.b.ins().rotl(x, n)
                } else {
                    self.b.ins().rotr(x, n)
                };
                if narrow == types::I64 {
                    r
                } else if kind.unsigned() {
                    self.b.ins().uextend(types::I64, r)
                } else {
                    self.b.ins().sextend(types::I64, r)
                }
            }
        })
    }
    /// A bit builtin of `integer_widths` (`leading_zeros_k`,
    /// `trailing_zeros_k`, `count_ones_k`, `wrapping_shl_k`,
    /// `wrapping_shr_k`) on a canonical lane: one `clz`, `ctz`, `popcnt`,
    /// `ishl`, `ushr` or `sshr` in the kind's own width (`lzcnt`, `tzcnt`
    /// and `popcnt` where the host has them, `clz`, `rbit`+`clz` and `cnt`
    /// on AArch64). Counts are `u32` lanes; shifts take the amount modulo
    /// the width and return a canonical `k` lane.
    fn bit_builtin(
        &mut self,
        name: &str,
        a: ir::Value,
        n: Option<ir::Value>,
    ) -> Result<ir::Value, String> {
        use crate::integer_widths::BitOp;
        let (op, kind) =
            crate::integer_widths::bit_builtin(name).ok_or("unknown native bit builtin")?;
        let narrow = match kind.bits() {
            8 => types::I8,
            16 => types::I16,
            32 => types::I32,
            _ => types::I64,
        };
        let x = if narrow == types::I64 {
            a
        } else {
            self.b.ins().ireduce(narrow, a)
        };
        let (r, signed) = match op {
            BitOp::LeadingZeros => (self.b.ins().clz(x), false),
            BitOp::TrailingZeros => (self.b.ins().ctz(x), false),
            BitOp::CountOnes => (self.b.ins().popcnt(x), false),
            BitOp::WrappingShl | BitOp::WrappingShr => {
                // Cranelift shifts take the amount modulo the width of `x`.
                let n = n.ok_or("native bit shift without an amount")?;
                let r = match op {
                    BitOp::WrappingShl => self.b.ins().ishl(x, n),
                    _ if kind.unsigned() => self.b.ins().ushr(x, n),
                    _ => self.b.ins().sshr(x, n),
                };
                (r, kind.signed())
            }
        };
        Ok(if narrow == types::I64 {
            r
        } else if signed {
            self.b.ins().sextend(types::I64, r)
        } else {
            self.b.ins().uextend(types::I64, r)
        })
    }
    /// `parallel_for(in data, n, body)` on the JIT: hands the compiled
    /// `body` (`fn(data: u64, i: u64) -> u64`) to the Pulse pool runtime
    /// `vesper_parallel_for`, which returns once every iteration has run.
    /// `None` when the body or data does not fit that word ABI, so the call
    /// falls back to the prelude's sequential loop.
    fn parallel_for_call(
        &mut self,
        args: &[Arg],
        site: &Expr,
    ) -> Result<Option<ir::Value>, String> {
        // The body is a top-level function value without captures (the
        // optimize path hands it over as a renamed operand, so resolve the
        // value rather than the name).
        let body = self.expr(&args[2].expr)?;
        let info = self.info(body);
        let [target] = info.targets.iter().copied().collect::<Vec<_>>()[..] else {
            return Ok(None);
        };
        let Some(closure) = target.checked_sub(1).and_then(|i| self.closures.get(i)) else {
            return Ok(None);
        };
        let ClosureTarget::Function(function) = &closure.target else {
            return Ok(None);
        };
        if !closure.captures.is_empty() {
            return Ok(None);
        }
        let Some(f) = self.world.functions.get(function) else {
            return Ok(None);
        };
        if !f.generics.is_empty() || !f.effects.instances.is_empty() || f.effects.tail.is_some() {
            return Ok(None);
        }
        let Some(&id) = self.ids.get(&instance_key(function, &BTreeMap::new())) else {
            return Ok(None);
        };
        let vars = call_vars(self.world, site, &self.generic_vars);
        let Some(data_ty) = vars.get("A") else {
            return Ok(None);
        };
        if matches!(data_ty, Type::Named(n, a) if a.is_empty() && (n == "f64" || n == "f32"))
            || crate::atomics::thread_affine(self.world, function, data_ty)
        {
            return Ok(None);
        }
        let layout = Layout::of(self.world, data_ty)?;
        let fun = self.m.declare_func_in_func(id, self.b.func);
        let sig = &self.b.func.dfg.signatures[self.b.func.dfg.ext_funcs[fun].signature];
        let word = AbiParam::new(types::I64);
        if sig.params != [word, word]
            || sig.returns.len() > 1
            || sig.returns.iter().any(|r| *r != word)
        {
            return Ok(None);
        }
        // From here on the call is committed: `data` and `n` are evaluated
        // once, in order.
        let data = if layout.aggregate() && crate::check::Place::from_expr(&args[0].expr).is_some()
        {
            self.address(&args[0].expr)?
        } else {
            self.expr(&args[0].expr)?
        };
        if self.b.func.dfg.value_type(data) != types::I64 {
            return Err("native parallel_for data is not one word".into());
        }
        let n = self.expr(&args[1].expr)?;
        let pointer = self.b.ins().func_addr(types::I64, fun);
        let mut runtime = self.m.make_signature();
        runtime.params.extend([word, word, word]);
        runtime.returns.push(word);
        let runtime = self
            .m
            .declare_function("vesper_parallel_for", Linkage::Import, &runtime)
            .map_err(|e| e.to_string())?;
        let runtime = self.m.declare_func_in_func(runtime, self.b.func);
        self.b.ins().call(runtime, &[pointer, data, n]);
        Ok(Some(self.zero()))
    }
    /// An atomic operation of `atomics` on the one element of the receiver's
    /// cell (`args[0]`, an `Atomic[T]`): one `atomic_load`, `atomic_store`,
    /// `atomic_rmw` or `atomic_cas` at the width of `T` (`bool` and `u8` are
    /// one byte), on the element at offset 0 of the sequence data. Operands
    /// and results are canonical lanes. Cranelift atomics are sequentially
    /// consistent, so the order arguments (literals, `E0820`) need no code.
    fn atomic_call(
        &mut self,
        op: crate::atomics::AtomicOp,
        args: &[Arg],
        site: &Expr,
        into: Option<ir::Value>,
    ) -> Result<ir::Value, String> {
        use crate::atomics::AtomicOp as A;
        use cranelift_codegen::ir::AtomicRmwOp as Rmw;
        let vars = call_vars(self.world, site, &self.generic_vars);
        let cell = vars.get("T").ok_or("native atomic without a cell type")?;
        let kind = crate::atomics::cell_kind(cell)
            .ok_or_else(|| format!("native atomics are not defined on Atomic[{cell}]"))?;
        let (narrow, signed) = match kind {
            None => (types::I8, false),
            Some(k) => (
                match k.bits() {
                    8 => types::I8,
                    16 => types::I16,
                    32 => types::I32,
                    _ => types::I64,
                },
                k.signed(),
            ),
        };
        if args.len() != 1 + op.values() + op.orders() {
            return Err("native atomic operation with the wrong arity".into());
        }
        let flags = ir::MemFlags::trusted();
        // `Atomic { cell }` -> sequence header `{len, cap, data}` -> element.
        let receiver = self.expr(&args[0].expr)?;
        let handle = self.b.ins().load(types::I64, flags, receiver, 0);
        let data = self.b.ins().load(types::I64, flags, handle, 16);
        let mut operands = vec![];
        for a in &args[1..1 + op.values()] {
            let v = self.expr(&a.expr)?;
            operands.push(if narrow == types::I64 {
                v
            } else {
                self.b.ins().ireduce(narrow, v)
            });
        }
        let widen = |c: &mut Self, v: ir::Value| {
            if narrow == types::I64 {
                v
            } else if signed {
                c.b.ins().sextend(types::I64, v)
            } else {
                c.b.ins().uextend(types::I64, v)
            }
        };
        let rmw = match op {
            A::Load => {
                let v = self.b.ins().atomic_load(narrow, flags, data);
                return Ok(widen(self, v));
            }
            A::Store => {
                self.b.ins().atomic_store(flags, operands[0], data);
                return Ok(self.zero());
            }
            A::CompareExchange => {
                let old = self
                    .b
                    .ins()
                    .atomic_cas(flags, data, operands[0], operands[1]);
                let same = self.b.ins().icmp(IntCC::Equal, old, operands[0]);
                let old = widen(self, old);
                let ty = Type::Named("Result".into(), vec![cell.clone(), cell.clone()]);
                let layout = Layout::of(self.world, &ty)?;
                let (ok, ok_fields) = layout.variant("Ok").ok_or("native Result without Ok")?;
                let (err, _) = layout.variant("Err").ok_or("native Result without Err")?;
                let offset = ok_fields.first().map_or(8, |f| f.offset);
                let ok = self.b.ins().iconst(types::I64, ok as i64);
                let err = self.b.ins().iconst(types::I64, err as i64);
                let tag = self.b.ins().select(same, ok, err);
                let ptr = match into {
                    Some(out) => out,
                    None => self.alloc(layout.size),
                };
                self.b.ins().store(ir::MemFlags::new(), tag, ptr, 0);
                self.b.ins().store(ir::MemFlags::new(), old, ptr, offset);
                return Ok(ptr);
            }
            A::Swap => Rmw::Xchg,
            A::FetchAdd => Rmw::Add,
            A::FetchSub => Rmw::Sub,
            A::FetchAnd => Rmw::And,
            A::FetchOr => Rmw::Or,
            A::FetchXor => Rmw::Xor,
        };
        let old = self
            .b
            .ins()
            .atomic_rmw(narrow, flags, rmw, data, operands[0]);
        Ok(widen(self, old))
    }
    /// `widen_k`, `narrow_k` and `wrapping_k` on a canonical `from` lane.
    /// In-range values have the same lane in every kind, so only the range
    /// check of a narrowing and the reduction of a wrapping emit code.
    fn convert(
        &mut self,
        name: &str,
        from: Option<crate::numeric::IntegerKind>,
        v: ir::Value,
        span: &Span,
    ) -> Result<ir::Value, String> {
        use crate::numeric::Conversion;
        let (conversion, to) =
            crate::numeric::conversion(name).ok_or("unknown native conversion")?;
        let from = from.ok_or("native conversion of a non-integer value")?;
        match conversion {
            Conversion::Widen => Ok(v),
            Conversion::Wrapping => Ok(self.coerce(v, Some(&to.ty()))),
            Conversion::Narrow => {
                if self.world.overflow_ops.contains(&(span.start, span.end)) {
                    let (low, high) = if from.unsigned() {
                        // The lane is the value as a u64; the target
                        // minimum is at most 0.
                        let high = (to.max() < from.max()).then(|| {
                            self.b
                                .ins()
                                .icmp_imm(IntCC::UnsignedGreaterThan, v, to.max() as i64)
                        });
                        (None, high)
                    } else {
                        let low = (to.min() > from.min()).then(|| {
                            self.b
                                .ins()
                                .icmp_imm(IntCC::SignedLessThan, v, to.min() as i64)
                        });
                        let high = (to.max() < from.max()).then(|| {
                            self.b
                                .ins()
                                .icmp_imm(IntCC::SignedGreaterThan, v, to.max() as i64)
                        });
                        (low, high)
                    };
                    let overflow = match (low, high) {
                        (Some(l), Some(h)) => Some(self.b.ins().bor(l, h)),
                        (l, h) => l.or(h),
                    };
                    if let Some(overflow) = overflow {
                        self.check_overflow(overflow)?;
                    }
                }
                Ok(v)
            }
        }
    }
    fn primitive_failure(&mut self, variant: &str) -> Result<(), String> {
        let ty = Type::named("RuntimeError");
        let error = self.data(Some(&format!("RuntimeError::{variant}")), vec![], &ty)?;
        self.perform_values("Fail", None, &[ty], "raise", vec![error], vec![])?;
        Ok(())
    }
    /// Raise `Fail[ArithOverflow]` when `overflow` is set, else continue.
    fn check_overflow(&mut self, overflow: ir::Value) -> Result<(), String> {
        let good = self.b.create_block();
        let bad = self.b.create_block();
        self.branch(overflow, bad, &[], good, &[]);
        self.b.switch_to_block(bad);
        let ty = Type::named(crate::numeric::OVERFLOW_TYPE);
        let error = self.data(Some(crate::numeric::OVERFLOW_TYPE), vec![], &ty)?;
        self.perform_values("Fail", None, &[ty], "raise", vec![error], vec![])?;
        self.jump(good, &[]);
        self.b.switch_to_block(good);
        Ok(())
    }
    /// `vesper.arith.checked/1` for one integer kind. Narrow unsigned kinds
    /// are zero-extended i64 values, so their exact sums and products fit.
    fn checked_arithmetic(
        &mut self,
        op: &str,
        kind: crate::numeric::IntegerKind,
        a: ir::Value,
        b: ir::Value,
    ) -> Result<ir::Value, String> {
        use crate::numeric::IntegerKind as K;
        let overflow = match (op, kind) {
            ("+" | "-" | "*", K::I8 | K::I16 | K::I32) => {
                // Exact in 64 bits; overflow iff the result leaves the kind.
                let r = match op {
                    "+" => self.b.ins().iadd(a, b),
                    "-" => self.b.ins().isub(a, b),
                    _ => self.b.ins().imul(a, b),
                };
                let up = i64::from(64 - kind.bits());
                let high = self.b.ins().ishl_imm(r, up);
                let canonical = self.b.ins().sshr_imm(high, up);
                Some(self.b.ins().icmp(IntCC::NotEqual, canonical, r))
            }
            ("/", K::I8 | K::I16 | K::I32) => {
                let min = self.b.ins().icmp_imm(IntCC::Equal, a, kind.min() as i64);
                let minus_one = self.b.ins().icmp_imm(IntCC::Equal, b, -1);
                Some(self.b.ins().band(min, minus_one))
            }
            ("+" | "*", K::U8 | K::U16 | K::U32) => {
                let r = if op == "+" {
                    self.b.ins().iadd(a, b)
                } else {
                    self.b.ins().imul(a, b)
                };
                Some(
                    self.b
                        .ins()
                        .icmp_imm(IntCC::UnsignedGreaterThan, r, kind.mask() as i64),
                )
            }
            ("-", K::U8 | K::U16 | K::U32 | K::U64 | K::Usize) => {
                Some(self.b.ins().icmp(IntCC::UnsignedLessThan, a, b))
            }
            ("+", K::U64 | K::Usize) => {
                let r = self.b.ins().iadd(a, b);
                Some(self.b.ins().icmp(IntCC::UnsignedLessThan, r, a))
            }
            ("*", K::U64 | K::Usize) => {
                let high = self.b.ins().umulhi(a, b);
                Some(self.b.ins().icmp_imm(IntCC::NotEqual, high, 0))
            }
            ("+" | "-", K::I64 | K::Isize) => {
                let r = if op == "+" {
                    self.b.ins().iadd(a, b)
                } else {
                    self.b.ins().isub(a, b)
                };
                // Signed overflow iff the result's sign differs from both
                // addends (addition) or from the minuend while the operand
                // signs differ (subtraction).
                let x = self.b.ins().bxor(a, r);
                let y = if op == "+" {
                    self.b.ins().bxor(b, r)
                } else {
                    self.b.ins().bxor(a, b)
                };
                let both = self.b.ins().band(x, y);
                Some(self.b.ins().icmp_imm(IntCC::SignedLessThan, both, 0))
            }
            ("*", K::I64 | K::Isize) => {
                let r = self.b.ins().imul(a, b);
                let high = self.b.ins().smulhi(a, b);
                let sign = self.b.ins().sshr_imm(r, 63);
                Some(self.b.ins().icmp(IntCC::NotEqual, high, sign))
            }
            ("/", K::I64 | K::Isize) => {
                let min = self.b.ins().icmp_imm(IntCC::Equal, a, i64::MIN);
                let minus_one = self.b.ins().icmp_imm(IntCC::Equal, b, -1);
                Some(self.b.ins().band(min, minus_one))
            }
            _ => None,
        };
        if let Some(overflow) = overflow {
            self.check_overflow(overflow)?;
        }
        self.binary(op, a, b, kind.unsigned())
    }
    /// After a call to a `fail_only` function, raise its recorded failure
    /// through the capability selected for that effect instance.
    /// `tag` is the record's status word, or the status register of a
    /// register-status call ([`NativeAbi`]). The error payload is at
    /// offset 8 of `record` either way.
    fn reraise_status(
        &mut self,
        f: &Function,
        record: ir::Value,
        tag: ir::Value,
    ) -> Result<(), String> {
        if fail_instances(f).count() > 1 {
            // Several instances: one test of the status first, so the
            // success path takes a single branch whatever the row's size.
            let failed = self.b.ins().icmp_imm(IntCC::NotEqual, tag, 0);
            let bad = self.b.create_block();
            let good = self.b.create_block();
            self.branch(failed, bad, &[], good, &[]);
            self.b.switch_to_block(bad);
            self.reraise_each(f, record, tag)?;
            self.jump(good, &[]);
            self.b.switch_to_block(good);
            return Ok(());
        }
        self.reraise_each(f, record, tag)
    }
    fn reraise_each(
        &mut self,
        f: &Function,
        record: ir::Value,
        tag: ir::Value,
    ) -> Result<(), String> {
        for (index, effect) in fail_instances(f).enumerate() {
            let failed = self.b.ins().icmp_imm(IntCC::Equal, tag, index as i64 + 1);
            let bad = self.b.create_block();
            let good = self.b.create_block();
            self.branch(failed, bad, &[], good, &[]);
            self.b.switch_to_block(bad);
            let payload = self.offset(record, 8);
            let error = self.load(payload, &effect.args[0])?;
            self.perform_values("Fail", None, &effect.args, "raise", vec![error], vec![])?;
            self.jump(good, &[]);
            self.b.switch_to_block(good);
        }
        Ok(())
    }
    fn check_divisor(&mut self, divisor: ir::Value) -> Result<(), String> {
        let nonzero = self.b.ins().icmp_imm(IntCC::NotEqual, divisor, 0);
        let good = self.b.create_block();
        let bad = self.b.create_block();
        self.branch(nonzero, good, &[], bad, &[]);
        self.b.switch_to_block(bad);
        self.primitive_failure("DivisionByZero")?;
        self.jump(good, &[]);
        self.b.switch_to_block(good);
        Ok(())
    }
    /// A bitwise intrinsic on 64-bit lanes holding `kind` values. Shift
    /// amounts are taken modulo the kind's width; callers reduce narrow
    /// results with `coerce`.
    fn bitwise(
        &mut self,
        name: &str,
        kind: Option<crate::numeric::IntegerKind>,
        a: ir::Value,
        b: ir::Value,
    ) -> Result<ir::Value, String> {
        let kind = kind.ok_or("native bitwise intrinsic on a non-integer type")?;
        if self.b.func.dfg.value_type(a) != types::I64
            || self.b.func.dfg.value_type(b) != types::I64
        {
            return Err("native bitwise intrinsic on a non-integer value".into());
        }
        let op = crate::capability_effects::bit_intrinsic(name)
            .ok_or_else(|| format!("unknown native bitwise intrinsic '{name}'"))?;
        let amount = |s: &mut Self| s.b.ins().band_imm(b, i64::from(kind.bits()) - 1);
        Ok(match op {
            "&" => self.b.ins().band(a, b),
            "|" => self.b.ins().bor(a, b),
            "^" => self.b.ins().bxor(a, b),
            "<<" => {
                let n = amount(self);
                self.b.ins().ishl(a, n)
            }
            _ => {
                let n = amount(self);
                if kind.unsigned() {
                    self.b.ins().ushr(a, n)
                } else {
                    self.b.ins().sshr(a, n)
                }
            }
        })
    }
    fn binary(
        &mut self,
        op: &str,
        a: ir::Value,
        b: ir::Value,
        unsigned: bool,
    ) -> Result<ir::Value, String> {
        if self.b.func.dfg.value_type(a) != self.b.func.dfg.value_type(b) {
            return Err("native numeric type mismatch".into());
        }
        let float = is_float_type(self.b.func.dfg.value_type(a));
        Ok(if matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=") {
            let v = if float {
                let cc = match op {
                    "==" => FloatCC::Equal,
                    "!=" => FloatCC::NotEqual,
                    "<" => FloatCC::LessThan,
                    "<=" => FloatCC::LessThanOrEqual,
                    ">" => FloatCC::GreaterThan,
                    _ => FloatCC::GreaterThanOrEqual,
                };
                self.b.ins().fcmp(cc, a, b)
            } else {
                let cc = match op {
                    "==" => IntCC::Equal,
                    "!=" => IntCC::NotEqual,
                    "<" => {
                        if unsigned {
                            IntCC::UnsignedLessThan
                        } else {
                            IntCC::SignedLessThan
                        }
                    }
                    "<=" => {
                        if unsigned {
                            IntCC::UnsignedLessThanOrEqual
                        } else {
                            IntCC::SignedLessThanOrEqual
                        }
                    }
                    ">" => {
                        if unsigned {
                            IntCC::UnsignedGreaterThan
                        } else {
                            IntCC::SignedGreaterThan
                        }
                    }
                    _ => {
                        if unsigned {
                            IntCC::UnsignedGreaterThanOrEqual
                        } else {
                            IntCC::SignedGreaterThanOrEqual
                        }
                    }
                };
                self.b.ins().icmp(cc, a, b)
            };
            self.b.ins().uextend(types::I64, v)
        } else {
            match (op, float) {
                // Wrapping intrinsics share the 64-bit operation; callers
                // reduce narrow unsigned results with `coerce`.
                ("+" | "wrapping_add", false) => self.b.ins().iadd(a, b),
                ("-" | "wrapping_sub", false) => self.b.ins().isub(a, b),
                ("*" | "wrapping_mul", false) => self.b.ins().imul(a, b),
                ("/", false) => {
                    if unsigned {
                        self.b.ins().udiv(a, b)
                    } else {
                        let min = self.b.ins().icmp_imm(IntCC::Equal, a, i64::MIN);
                        let minus_one = self.b.ins().icmp_imm(IntCC::Equal, b, -1);
                        let overflow = self.b.ins().band(min, minus_one);
                        let wrap = self.b.create_block();
                        let divide = self.b.create_block();
                        let join = self.b.create_block();
                        self.b.append_block_param(join, types::I64);
                        self.branch(overflow, wrap, &[], divide, &[]);
                        self.b.switch_to_block(wrap);
                        self.jump(join, &[a.into()]);
                        self.b.switch_to_block(divide);
                        let result = self.b.ins().sdiv(a, b);
                        self.jump(join, &[result.into()]);
                        self.b.switch_to_block(join);
                        self.b.block_params(join)[0]
                    }
                }
                ("%", false) => {
                    if unsigned {
                        self.b.ins().urem(a, b)
                    } else {
                        self.b.ins().srem(a, b)
                    }
                }
                ("+", true) => self.b.ins().fadd(a, b),
                ("-", true) => self.b.ins().fsub(a, b),
                ("*", true) => self.b.ins().fmul(a, b),
                ("/", true) => self.b.ins().fdiv(a, b),
                ("%", true) => {
                    let ty = self.b.func.dfg.value_type(a);
                    let mut signature = self.m.make_signature();
                    signature
                        .params
                        .extend([AbiParam::new(ty), AbiParam::new(ty)]);
                    signature.returns.push(AbiParam::new(ty));
                    let name = if ty == types::F32 { "fmodf" } else { "fmod" };
                    let id = self
                        .m
                        .declare_function(name, Linkage::Import, &signature)
                        .map_err(|e| e.to_string())?;
                    let reference = self.m.declare_func_in_func(id, self.b.func);
                    let call = self.b.ins().call(reference, &[a, b]);
                    self.b.inst_results(call)[0]
                }
                _ => return Err("unsupported native operator".into()),
            }
        })
    }
    fn call_function(
        &mut self,
        name: &str,
        args: &[Arg],
        site: &Expr,
    ) -> Result<ir::Value, String> {
        self.call_function_into(name, args, site, None)
    }
    /// A call whose aggregate result is written to `into` when given
    /// ([`aggregates::plan`]): the callee's output, or a copy into it.
    fn call_function_into(
        &mut self,
        name: &str,
        args: &[Arg],
        site: &Expr,
        into: Option<ir::Value>,
    ) -> Result<ir::Value, String> {
        if name == crate::integer_widths::MUL_WIDE && args.len() == 2 {
            // The reserved prelude product (`integer_widths`): `(low, high)`.
            let a = self.expr(&args[0].expr)?;
            let b = self.expr(&args[1].expr)?;
            let low = self.b.ins().imul(a, b);
            let high = self.b.ins().umulhi(a, b);
            let ptr = match into {
                Some(out) => out,
                None => self.alloc(16),
            };
            self.b.ins().store(ir::MemFlags::new(), low, ptr, 0);
            self.b.ins().store(ir::MemFlags::new(), high, ptr, 8);
            return Ok(ptr);
        }
        if let Some(op) = crate::atomics::intercepted(self.world, name) {
            // An atomic operation (`atomics`): one Cranelift atomic.
            return self.atomic_call(op, args, site, into);
        }
        if name == crate::atomics::PARALLEL_FOR
            && self.world.workers_prelude
            && args.len() == 3
            && std::any::type_name::<M>().contains("JITModule")
            && let Some(done) = self.parallel_for_call(args, site)?
        {
            // A fork-join loop (`atomics` scoped workers) on the Pulse pool.
            return Ok(done);
        }
        if let Some((op, _)) = crate::integer_widths::bit_builtin(name)
            && args.len() == op.arity()
            && args.iter().all(|a| a.pass == Pass::In)
        {
            // A reserved bit builtin (`integer_widths`): one instruction.
            let a = self.expr(&args[0].expr)?;
            let n = match args.get(1) {
                Some(n) => Some(self.expr(&n.expr)?),
                None => None,
            };
            return self.bit_builtin(name, a, n);
        }
        // `dyn` parameters take their object types: a `dyn` function has
        // one instance, and each concrete argument is coerced to an object.
        let raw_vars = match self.forced_call_vars.take() {
            Some(vars) => vars,
            None => call_vars(self.world, site, &self.generic_vars),
        };
        let vars = self.world.dyn_objects.erase(name, &raw_vars);
        let dyn_slot = self
            .world
            .dyn_objects
            .stubs
            .get(name)
            .map(|(_, slot)| *slot);
        let f = concrete(self.world, name, &vars)?;
        if let Some(slot) = dyn_slot
            && specialized_function(self.world, &f)
        {
            // A slot that performs handler effects reaches the caller's
            // handlers through the object's vtable identity (§7.3).
            return self.dyn_call_effectful(slot, &f, args, site, into);
        }
        let result_layout = Layout::of(self.world, &f.result)?;
        let mut values = vec![];
        let mut restores = vec![];
        let mut sink_blocks = vec![];
        let declared = self.world.functions[name].params.clone();
        for ((arg, param), declared) in args.iter().zip(&f.params).zip(&declared) {
            let layout = Layout::of(self.world, &param.ty)?;
            if let Type::Named(g, _) = &declared.ty
                && crate::dyn_trait::is_dyn_generic(g)
                && let Some(actual) = raw_vars.get(g)
                && !crate::dyn_trait::is_object(actual)
            {
                let pair = if crate::dyn_trait::is_boxed(actual) {
                    let (pair, block) = self.dyn_lend_boxed(arg, param.pass)?;
                    if param.pass == Pass::Sink {
                        sink_blocks.push(block);
                    }
                    pair
                } else {
                    self.dyn_coerce(arg, param.pass, actual, &param.ty, &mut restores)?
                };
                values.push(pair);
            } else if dyn_slot.is_some()
                && values.is_empty()
                && crate::dyn_trait::is_boxed(&param.ty)
            {
                // A boxed receiver (§7.3) is its block, whatever the pass:
                // the call mutates or consumes the object, never the box.
                values.push(self.expr(&arg.expr)?);
            } else if param.pass == Pass::Inout && crate::seq::is_slice_type(&param.ty) {
                // A slice view is passed as the address of its value.
                values.push(self.expr(&arg.expr)?);
            } else if param.pass == Pass::Inout {
                let ptr = self.address(&arg.expr)?;
                if !layout.aggregate()
                    && let ExprKind::Name(n) = &arg.expr.kind
                {
                    let var = *self.vars.get(n).ok_or("unknown mutable scalar")?;
                    let value = self.use_var(var);
                    self.b.ins().store(ir::MemFlags::new(), value, ptr, 0);
                    restores.push((var, ptr, param.ty.clone()));
                }
                values.push(ptr);
            } else if layout.aggregate()
                && param.pass == Pass::In
                && crate::check::Place::from_expr(&arg.expr).is_some()
            {
                values.push(self.address(&arg.expr)?);
            } else {
                values.push(self.expr(&arg.expr)?);
            }
        }
        let bound = self.bound_function_arguments(name, &f, &vars, &values);
        let (f, vars, mut values) = match bound {
            Some((vars, values)) => (concrete(self.world, name, &vars)?, vars, values),
            None => (f, vars, values),
        };
        let call_slot_arg = if crate::fip::uses_caller_slot(self.world, &f) {
            let slot_ptr = match self.current_call_slot.take() {
                Some(crate::fip::CallSlot::CallerSlot) => {
                    if let Some(slot) = self.caller_slot {
                        slot
                    } else {
                        let inner_ty = crate::fip::box_inner(self.world, &f.result)
                            .ok_or("Box::new_in_caller function must return Box[T]")?;
                        let inner_layout = Layout::of(self.world, inner_ty)?;
                        self.fip_box_alloc(inner_layout.size)?
                    }
                }
                Some(crate::fip::CallSlot::ReuseSlot {
                    bind_dest,
                    slot_index,
                    ..
                }) => {
                    let var = *self
                        .reuse_slots
                        .get(&(bind_dest, slot_index))
                        .ok_or_else(|| format!("missing reuse slot ({bind_dest}, {slot_index})"))?;
                    self.use_var(var)
                }
                None => {
                    let inner_ty = crate::fip::box_inner(self.world, &f.result)
                        .ok_or("Box::new_in_caller function must return Box[T]")?;
                    let inner_layout = Layout::of(self.world, inner_ty)?;
                    self.fip_box_alloc(inner_layout.size)?
                }
            };
            Some(slot_ptr)
        } else {
            self.current_call_slot = None;
            None
        };
        // A vtable stub is called through the receiver's slot. A boxed
        // receiver (§7.3) is reached through its header; one consumed by a
        // `sink self` method leaves its block to free after the call.
        let mut f = f;
        let mut boxed_free = None;
        let dyn_target = match dyn_slot {
            Some(slot) => {
                let receiver = f.params.first().cloned();
                match receiver {
                    Some(p) if crate::dyn_trait::is_boxed(&p.ty) => {
                        let object = crate::dyn_trait::boxed_object(&p.ty)
                            .cloned()
                            .unwrap_or(Type::Infer);
                        f.params[0].ty = object;
                        let (target, block) = self.dyn_dispatch_boxed(&mut values, slot)?;
                        if p.pass == Pass::Sink {
                            boxed_free = Some(block);
                        }
                        Some(target)
                    }
                    _ => Some(self.dyn_dispatch(&mut values, slot)?),
                }
            }
            None => None,
        };
        let call_key = instance_key(name, &vars);
        let trace_entry_name = self
            .trace_entry
            .as_ref()
            .map(|(e, _)| e.as_str())
            .unwrap_or("");
        let direct_trace = self
            .capabilities
            .get(&format!("$effect:{}", crate::trace_effects::TRACE))
            .is_some_and(|id| id == "native-trace-host")
            && self.ids.contains_key(&call_key)
            && (trace_host_direct(self.world, &f) || call_key == trace_entry_name);
        let result = if specialized_function(self.world, &f)
            && !direct_trace
            && dyn_target.is_none()
        {
            if !sink_blocks.is_empty() {
                // The inlined body reads the object in its block, and an
                // abort through a handler would skip the free after it.
                return Err(format!(
                    "native: a boxed object lent `sink` to '{name}', whose effects are specialized at the call, is not supported in Stage 1; lend it to a function without handler effects"
                ));
            }
            if self.inline_depth >= 64 {
                return Err("recursive effect specialization exceeds 64 inline frames".into());
            }
            let scope = std::mem::take(&mut self.vars);
            let type_scope = std::mem::take(&mut self.local_types);
            let generic_scope = std::mem::replace(&mut self.generic_vars, vars);
            let caller_slot_scope = std::mem::replace(&mut self.caller_slot, call_slot_arg);
            let reuse_scope = std::mem::take(&mut self.reuse_slots);
            let fip_scope = std::mem::replace(&mut self.in_fip_fn, f.fip);
            let mut incoming_caps = BTreeMap::new();
            for (index, effect) in f.effects.instances.iter().enumerate() {
                let key = if effect.label.is_none() {
                    self.instance_capability_key(&effect.name, &effect.args)
                } else {
                    effect_key(effect)
                };
                if let Some(id) = self.capabilities.get(&key) {
                    incoming_caps.insert(key.clone(), id.clone());
                    incoming_caps.insert(format!("$effect:{}", effect.name), id.clone());
                    if let Some(typed) = crate::numeric::instance_key(&effect.name, &effect.args) {
                        incoming_caps.insert(typed, id.clone());
                    }
                    incoming_caps.insert(format!("$static:$fncap:{name}:{index}"), id.clone());
                    if let Some(label) = &effect.label {
                        incoming_caps.insert(format!("$label:{label}"), id.clone());
                    }
                }
            }
            if f.effects.tail.is_some() {
                incoming_caps.extend(self.capabilities.clone());
            }
            let capability_scope = std::mem::replace(&mut self.capabilities, incoming_caps);
            for (p, v) in f.params.iter().zip(&values) {
                let value = if p.pass == Pass::Inout && !Layout::of(self.world, &p.ty)?.aggregate()
                {
                    self.load(*v, &p.ty)?
                } else {
                    *v
                };
                let var = self.b.declare_var(self.b.func.dfg.value_type(value));
                self.def_var(var, value);
                self.vars.insert(p.name.clone(), var);
                self.local_types.insert(p.name.clone(), p.ty.clone());
                if p.pass == Pass::Inout && !Layout::of(self.world, &p.ty)?.aggregate() {
                    self.set_pointer(var, *v);
                }
            }
            self.inline_depth += 1;
            let result = (|| {
                let join = self.b.create_block();
                self.b.append_block_param(
                    join,
                    if result_layout.aggregate() {
                        self.m.target_config().pointer_type()
                    } else {
                        scalar(&f.result, &[])?
                    },
                );
                let graph = self
                    .program
                    .functions
                    .get(name)
                    .ok_or("effectful function lacks IR")?
                    .clone();
                self.ir_graph(&graph, None, Some(join))?;
                self.b.switch_to_block(join);
                Ok::<_, String>(self.b.block_params(join)[0])
            })();
            self.inline_depth -= 1;
            self.vars = scope;
            self.local_types = type_scope;
            self.generic_vars = generic_scope;
            self.caller_slot = caller_slot_scope;
            self.reuse_slots = reuse_scope;
            self.in_fip_fn = fip_scope;
            self.capabilities = capability_scope;
            let result = result?;
            match into {
                Some(out) if result_layout.aggregate() => {
                    self.copy_bytes(result, out, result_layout.size);
                    out
                }
                _ => result,
            }
        } else {
            let key = instance_key(name, &vars);
            let id = match (&self.trace_entry, dyn_target) {
                (_, Some(_)) => None,
                (Some((entry_name, internal_id)), None) if &key == entry_name => Some(*internal_id),
                _ => Some(
                    *self
                        .ids
                        .get(&key)
                        .ok_or_else(|| format!("missing monomorphized function '{key}'"))?,
                ),
            };
            let callee_abi = if dyn_target.is_some() {
                self.abi.slot(self.world, &f)?
            } else {
                self.abi.of(&key)
            };
            let output = if callee_abi.result_words.is_some() {
                // Register result: the callee returns the words.
                None
            } else if result_layout.aggregate() {
                // The callee writes every word of its result; storage that
                // holds no drop flag needs no zero (optimize path).
                Some(match into {
                    Some(out) => out,
                    None if self.operands_only && aggregates::plain_data(self.world, &f.result) => {
                        self.alloc_uninit(result_layout.size)
                    }
                    None => self.alloc(result_layout.size),
                })
            } else {
                None
            };
            if let Some(out) = output {
                values.insert(0, out);
            }
            if let Some(slot_ptr) = call_slot_arg {
                values.push(slot_ptr);
            }
            let status_call = callee_abi.status;
            let record = if fail_only(self.world, &f) {
                let size = fail_record_size(self.world, &f)?;
                // A status-ABI callee writes only the error payload, and
                // only when it fails: its storage needs no zeroing.
                let record = if status_call {
                    self.alloc_uninit(size)
                } else {
                    self.alloc(size)
                };
                values.push(record);
                Some(record)
            } else {
                None
            };
            let pass_trace = dyn_target.is_none()
                && uses_trace_fail(
                    self.world,
                    self.trace_fail.is_some(),
                    &key,
                    self.trace_entry
                        .as_ref()
                        .map(|(e, _)| e.as_str())
                        .unwrap_or(""),
                    &f,
                );
            if pass_trace {
                values.push(self.trace_fail.ok_or("missing native trace failure slot")?);
            }
            let (sig, fun) = match id {
                Some(id) => {
                    let fun = self.m.declare_func_in_func(id, self.b.func);
                    (self.b.func.dfg.ext_funcs[fun].signature, Some(fun))
                }
                None => {
                    let sig = signature_with_abi(&*self.m, self.world, &f, false, callee_abi)?;
                    (self.b.import_signature(sig), None)
                }
            };
            let params = &self.b.func.dfg.signatures[sig].params;
            if params.len() != values.len()
                || params
                    .iter()
                    .zip(&values)
                    .any(|(p, v)| p.value_type != self.b.func.dfg.value_type(*v))
            {
                return Err(format!("native call ABI mismatch in '{name}'"));
            }
            let call = match (fun, dyn_target) {
                (Some(fun), _) => self.b.ins().call(fun, &values),
                (None, Some(target)) => self.b.ins().call_indirect(sig, target, &values),
                (None, None) => return Err("native call lacks a callee".into()),
            };
            // The callee owns a `sink` object's value, not the block of the
            // box it was lent from. Free the blocks before a failure status
            // can leave this continuation (through `fail_exit` or a
            // handler's dispatch block).
            for block in boxed_free.take().into_iter().chain(sink_blocks.drain(..)) {
                self.fip_box_free(block)?;
            }
            let result = if let Some(words) = callee_abi.result_words {
                let buffer = match into {
                    Some(out) => out,
                    None => self.alloc_uninit(words * 8),
                };
                for word in 0..words {
                    let value = self.b.inst_results(call)[word as usize];
                    self.b
                        .ins()
                        .store(ir::MemFlags::new(), value, buffer, (word * 8) as i32);
                }
                buffer
            } else {
                output.unwrap_or_else(|| self.b.inst_results(call)[0])
            };
            if pass_trace {
                self.check_trace_failed();
            }
            if let Some(record) = record {
                if status_call {
                    let status = self.b.inst_results(call)[1];
                    self.reraise_status(&f, record, status)?;
                } else {
                    let tag = self
                        .b
                        .ins()
                        .load(types::I64, ir::MemFlags::new(), record, 0);
                    self.reraise_status(&f, record, tag)?;
                }
            }
            result
        };
        for (var, ptr, ty) in restores {
            let value = self.load(ptr, &ty)?;
            self.def_var(var, value);
            if let Some(destination) = self.pointer(var) {
                self.b
                    .ins()
                    .store(ir::MemFlags::new(), value, destination, 0);
            }
        }
        Ok(result)
    }
    fn fip_box_alloc(&mut self, size: u32) -> Result<ir::Value, String> {
        let jit = std::any::type_name::<M>().contains("JITModule");
        let id = if jit {
            let mut sig = self.m.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I64));
            self.m
                .declare_function("vesper_box_alloc", Linkage::Import, &sig)
                .map_err(|e| e.to_string())?
        } else {
            if self.m.isa().triple().operating_system == target_lexicon::OperatingSystem::None_ {
                return Err(
                    "Box[T] in a freestanding image needs a box runtime; the generated one uses the C library"
                        .into(),
                );
            }
            crate::fip::aot_runtime::declare_or_define(self.m)?
        };
        let size_val = self.b.ins().iconst(types::I64, i64::from(size.max(8)));
        let reference = self.m.declare_func_in_func(id, self.b.func);
        let call = self.b.ins().call(reference, &[size_val]);
        Ok(self.b.inst_results(call)[0])
    }
    fn fip_box_free(&mut self, ptr: ir::Value) -> Result<(), String> {
        emit_fip_box_free(&mut self.b, self.m, ptr)
    }
    fn drop_box_value(&mut self, value: ir::Value, ty: &Type) -> Result<(), String> {
        emit_drop_box_value(&mut self.b, self.m, self.world, value, ty)
    }
    fn emit_fip_drop(
        &mut self,
        drop: &crate::fip::FipDrop,
        values: &BTreeMap<crate::ir::ValueId, ir::Value>,
        _span: Span,
    ) -> Result<(), String> {
        match drop {
            crate::fip::FipDrop::Value(val_id, ty) => {
                let ty = crate::numeric::default_type(&substitute(ty, &self.generic_vars));
                if crate::fip::type_contains_box(self.world, &ty)
                    && let Some(&val) = values.get(val_id)
                {
                    self.drop_box_value(val, &ty)?;
                }
                Ok(())
            }
            crate::fip::FipDrop::Place(place, ty) => {
                let ty = crate::numeric::default_type(&substitute(ty, &self.generic_vars));
                if let (Some(&var), Some(root_ty)) = (
                    self.vars.get(&place.root),
                    self.local_types.get(&place.root).cloned(),
                ) && crate::fip::type_contains_box(self.world, &ty)
                {
                    let mut cur_val = self.use_var(var);
                    let mut cur_ty = root_ty;
                    for field in &place.path {
                        let layout = Layout::of(self.world, &cur_ty)?;
                        let f = layout
                            .field(field)
                            .ok_or_else(|| format!("unknown native field '{field}' in {cur_ty}"))?;
                        let ptr = self.offset(cur_val, f.offset);
                        cur_val = self.load(ptr, &f.ty)?;
                        cur_ty = f.ty.clone();
                    }
                    self.drop_box_value(cur_val, &ty)?;
                }
                Ok(())
            }
            crate::fip::FipDrop::OtherFields { value, ty, keep } => {
                let ty = crate::numeric::default_type(&substitute(ty, &self.generic_vars));
                if crate::fip::type_contains_box(self.world, &ty)
                    && let Some(&base_val) = values.get(value)
                {
                    let layout = Layout::of(self.world, &ty)?;
                    let fs = match &layout.kind {
                        LayoutKind::Product(fs) => fs.as_slice(),
                        LayoutKind::Enum(vs) if vs.len() == 1 => vs[0].fields.as_slice(),
                        _ => return Ok(()),
                    };
                    for f in fs {
                        if &f.name != keep && crate::fip::type_contains_box(self.world, &f.ty) {
                            let ptr = self.offset(base_val, f.offset);
                            let v = self.load(ptr, &f.ty)?;
                            self.drop_box_value(v, &f.ty)?;
                        }
                    }
                }
                Ok(())
            }
        }
    }
    fn fip_op(
        &mut self,
        op: &crate::fip::FipOp,
        operands: &[ir::Value],
        ty: &Type,
        actual: Option<&Type>,
    ) -> Result<ir::Value, String> {
        let [value] = operands else {
            return Err("Box constructor expects exactly one argument".into());
        };
        if let Some(object) = crate::dyn_trait::boxed_object(ty)
            && let Some(actual) = actual.filter(|t| !crate::dyn_trait::is_object(t))
        {
            if !matches!(op, crate::fip::FipOp::BoxNew) {
                return Err(format!("a boxed object `{ty}` is built by `Box::new` only"));
            }
            let object = object.clone();
            return self.dyn_box_new(*value, actual, &object);
        }
        let inner = crate::fip::box_inner(self.world, ty)
            .ok_or_else(|| format!("Box constructor expects Box[T], got {ty}"))?;
        let inner_layout = Layout::of(self.world, inner)?;
        let ptr = match op {
            crate::fip::FipOp::BoxNew => self.fip_box_alloc(inner_layout.size)?,
            crate::fip::FipOp::BoxNewInCaller => self
                .caller_slot
                .ok_or("Box::new_in_caller requires a caller slot")?,
            crate::fip::FipOp::BoxReuse {
                bind_dest,
                slot_index,
                ..
            } => {
                let var = *self
                    .reuse_slots
                    .get(&(*bind_dest, *slot_index))
                    .ok_or_else(|| format!("missing Box reuse slot ({bind_dest}, {slot_index})"))?;
                self.use_var(var)
            }
        };
        self.store(ptr, *value, inner)?;
        Ok(ptr)
    }
    /// When `name` takes function values ([`function_value_instance`]) and
    /// every function-value argument of this call is statically one
    /// top-level function, the instance with those parameters bound and the
    /// remaining argument values, provided the instance was declared.
    #[allow(clippy::type_complexity)]
    fn bound_function_arguments(
        &self,
        name: &str,
        f: &Function,
        vars: &BTreeMap<String, Type>,
        values: &[ir::Value],
    ) -> Option<(BTreeMap<String, Type>, Vec<ir::Value>)> {
        let source = self.world.functions.get(name)?;
        if !function_value_instance(self.world, source) || f.params.len() != values.len() {
            return None;
        }
        let mut bound = vars.clone();
        let mut rest = Vec::new();
        for (p, v) in f.params.iter().zip(values) {
            if !matches!(p.ty, Type::Function { .. }) {
                rest.push(*v);
                continue;
            }
            let info = self.info(*v);
            let [id] = info.targets.iter().copied().collect::<Vec<_>>()[..] else {
                return None;
            };
            let closure = self.closures.get(id.checked_sub(1)?)?;
            let ClosureTarget::Function(function) = &closure.target else {
                return None;
            };
            if !closure.captures.is_empty() {
                return None;
            }
            bound.insert(
                format!("{FN_ARG}{}", p.name),
                Type::Named(function.clone(), vec![]),
            );
        }
        self.ids
            .contains_key(&instance_key(name, &bound))
            .then_some((bound, rest))
    }
    /// Abort the current `fail_only` function: record the raised instance and
    /// error value for the caller, then return a placeholder result.
    fn fail_exit(
        &mut self,
        index: i64,
        effect: &str,
        instance: &[Type],
        values: Vec<ir::Value>,
    ) -> Result<ir::Value, String> {
        let (record, errors) = self
            .fail_record
            .clone()
            .ok_or("native failure exit outside its function")?;
        if effect != "Fail" || values.len() != 1 {
            return Err("native failure exit requires Fail::raise".into());
        }
        let _ = instance;
        let ty = errors
            .get(index as usize)
            .cloned()
            .ok_or("native failure exit names an unknown instance")?;
        let tag = self.b.ins().iconst(types::I64, index + 1);
        if !self.own_abi.status {
            self.b.ins().store(ir::MemFlags::new(), tag, record, 0);
        }
        let payload = self.offset(record, 8);
        self.store(payload, values[0], &ty)?;
        self.seq_drop_exit(0)?;
        let returns = self.b.func.signature.returns[0].value_type;
        let placeholder = if is_float_type(returns) {
            self.float_const(returns, 0.0)
        } else {
            self.b.ins().iconst(returns, 0)
        };
        if self.own_abi.status {
            // The register-status ABI: the status is the second return value.
            self.b.ins().return_(&[placeholder, tag]);
        } else {
            self.b.ins().return_(&[placeholder]);
        }
        let dead = self.b.create_block();
        self.b.switch_to_block(dead);
        Ok(self.zero())
    }
    /// Typed instance selection: the innermost capability with exactly these
    /// concrete arguments when one is in scope, else the innermost by name.
    fn instance_capability_key(&self, effect: &str, args: &[Type]) -> String {
        let args = args
            .iter()
            .map(|t| substitute(t, &self.generic_vars))
            .collect::<Vec<_>>();
        crate::numeric::instance_key(effect, &args)
            .filter(|key| self.capabilities.contains_key(key))
            .unwrap_or_else(|| format!("$effect:{effect}"))
    }
    fn perform(
        &mut self,
        effect: &str,
        label: Option<&str>,
        instance: &[Type],
        op: &str,
        args: &[Arg],
    ) -> Result<ir::Value, String> {
        if effect == crate::regions::REGION_EFFECT && crate::regions::active(self.world) {
            return self.region_perform(label, op, args);
        }
        if crate::ffi_lowering::is_foreign_effect(effect) {
            return self.foreign_perform(effect, op, args);
        }
        let signature = self
            .world
            .effects
            .get(effect)
            .and_then(|e| e.ops.iter().find(|f| f.name == op))
            .ok_or("unknown effect operation")?
            .clone();
        let mut values = vec![];
        let mut refs = vec![];
        let mut restores = vec![];
        for (arg, param) in args.iter().zip(&signature.params) {
            let value = self.expr(&arg.expr)?;
            values.push(value);
            let ptr = if param.pass != Pass::Sink
                && crate::check::Place::from_expr(&arg.expr).is_some()
            {
                let ptr = self.address(&arg.expr)?;
                if let ExprKind::Name(n) = &arg.expr.kind {
                    let ty = self.ty(&arg.expr);
                    if !Layout::of(self.world, &ty)?.aggregate() {
                        restores.push((self.vars[n], ptr, ty));
                    }
                }
                Some(ptr)
            } else {
                None
            };
            refs.push(ptr);
        }
        let result = self.perform_values(effect, label, instance, op, values, refs)?;
        for (var, ptr, ty) in restores {
            let value = self.load(ptr, &ty)?;
            self.def_var(var, value);
            if let Some(destination) = self.pointer(var) {
                self.b
                    .ins()
                    .store(ir::MemFlags::new(), value, destination, 0);
            }
        }
        Ok(result)
    }
    fn perform_values(
        &mut self,
        effect: &str,
        label: Option<&str>,
        instance: &[Type],
        op: &str,
        values: Vec<ir::Value>,
        refs: Vec<Option<ir::Value>>,
    ) -> Result<ir::Value, String> {
        let argument_count = values.len();
        let key = if let Some(l) = label {
            format!("$label:{l}")
        } else {
            self.instance_capability_key(effect, instance)
        };
        let identity = self
            .capabilities
            .get(&key)
            .ok_or("native effect lacks scoped evidence")?
            .clone();
        if let Some(result) = self.session_operation(&identity, effect, op, &values, &refs)? {
            return Ok(result);
        }
        if effect == crate::trace_effects::TRACE && identity == "native-trace-host" {
            return self.trace_perform(op, &values);
        }
        if let Some(index) = identity.strip_prefix("native-fail-exit:") {
            let index = index.parse::<i64>().map_err(|e| e.to_string())?;
            return self.fail_exit(index, effect, instance, values);
        }
        let index = self
            .handlers
            .iter()
            .rposition(|h| h.effect.name == effect && h.identity == identity)
            .ok_or("native effect lacks lexical evidence")?;
        let h = self.handlers[index].clone();
        let clause = h
            .ops
            .iter()
            .find(|c| c.name == op)
            .ok_or("missing native handler clause")?;
        let continuation_pattern = clause.pats.last().ok_or("missing continuation")?;
        let captures = self.copy_bindings(&h.captured, &h.captured_types)?;
        let old_vars = std::mem::replace(&mut self.vars, captures);
        let old_types = std::mem::replace(&mut self.local_types, h.captured_types.clone());
        let old_generics = std::mem::replace(&mut self.generic_vars, h.generic_vars.clone());
        let old_capabilities = std::mem::replace(&mut self.capabilities, h.capabilities.clone());
        let decl = &self.world.effects[effect];
        let sig = decl
            .ops
            .iter()
            .find(|f| f.name == op)
            .ok_or("unknown effect operation")?;
        let substitutions = decl
            .generics
            .iter()
            .zip(&h.effect.args)
            .map(|(g, t)| (g.name.clone(), t.clone()))
            .collect();
        for (i, ((p, v), param)) in clause.pats.iter().zip(values).zip(&sig.params).enumerate() {
            let ty = substitute(&param.ty, &substitutions);
            if param.pass != Pass::Sink && refs.get(i).copied().flatten().is_some() {
                self.bind_alias(
                    p,
                    refs.get(i)
                        .copied()
                        .flatten()
                        .ok_or("mutable effect argument requires address")?,
                    &ty,
                )?;
            } else {
                self.bind(p, v, &ty)?;
            }
        }
        if let Some(state) = h.state {
            let value = self.use_var(state);
            self.bind(
                &clause.pats[argument_count],
                value,
                h.state_type.as_ref().unwrap(),
            )?;
        }
        let entry = self.b.create_block();
        let mut params = vec![(Pass::Sink, substitute(&sig.result, &substitutions))];
        if let Some(ty) = &h.state_type {
            params.push((Pass::Sink, ty.clone()));
        }
        for (_, ty) in &params {
            let layout = Layout::of(self.world, ty)?;
            self.b.append_block_param(
                entry,
                if layout.aggregate() {
                    self.m.target_config().pointer_type()
                } else {
                    scalar(ty, &[])?
                },
            );
        }
        if let PatternKind::Bind(k) = &continuation_pattern.kind {
            let graph = &self.program.functions
                [&crate::ir::nested_name("handler_clause", &clause.body.span)];
            let ty = crate::numeric::default_type(&substitute(
                &graph
                    .params
                    .iter()
                    .find(|p| &p.name == k)
                    .ok_or("continuation lacks its checked type")?
                    .ty,
                &self.generic_vars,
            ));
            let id = self.closures.len() + 1;
            self.closures.push(NativeClosure {
                ty: ty.clone(),
                target: ClosureTarget::Continuation {
                    entry,
                    control: h.control.clone(),
                },
                captures: vec![],
                generics: self.generic_vars.clone(),
                capabilities: self.capabilities.clone(),
            });
            let tag = self.b.ins().iconst(types::I64, id as i64);
            self.write_info(
                tag,
                NativeInfo {
                    targets: BTreeSet::from([id]),
                    fields: BTreeMap::new(),
                },
            );
            self.bind(continuation_pattern, tag, &ty)?;
        }
        let saved = self.handlers.split_off(index);
        let output = self.shared_control("handler_clause", &clause.body)?;
        let output = self.coerce(output, Some(&h.output_type));
        self.jump(h.control.dispatch, &[output.into()]);
        self.b.switch_to_block(entry);
        if let Some(state) = h.state {
            self.def_var(state, self.b.block_params(entry)[1]);
        }
        let result = self.b.block_params(entry)[0];
        self.handlers.extend(saved);
        self.vars = old_vars;
        self.local_types = old_types;
        self.generic_vars = old_generics;
        self.capabilities = old_capabilities;
        Ok(result)
    }
    fn return_trace_placeholder(&mut self) {
        // A trace failure leaves the whole function; it cannot fail here.
        let _ = self.seq_drop_exit(0);
        let returns = self.b.func.signature.returns[0].value_type;
        let placeholder = if is_float_type(returns) {
            self.float_const(returns, 0.0)
        } else {
            self.b.ins().iconst(returns, 0)
        };
        self.return_value(placeholder);
    }
    /// Return `value` from the function being compiled, with a zero
    /// (success) status when it uses the register-status Fail ABI.
    fn return_value(&mut self, value: ir::Value) {
        if let (Some(words), Some(buffer)) = (self.own_abi.result_words, self.result_buffer) {
            let mut out = vec![];
            for word in 0..words {
                out.push(self.b.ins().load(
                    types::I64,
                    ir::MemFlags::new(),
                    buffer,
                    (word * 8) as i32,
                ));
            }
            self.b.ins().return_(&out);
            return;
        }
        if self.own_abi.status {
            let ok = self.b.ins().iconst(types::I64, 0);
            self.b.ins().return_(&[value, ok]);
        } else {
            self.b.ins().return_(&[value]);
        }
    }
    fn abort_on_nonzero_status(&mut self, status: ir::Value) {
        let failed = self.b.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let bad = self.b.create_block();
        let good = self.b.create_block();
        self.branch(failed, bad, &[], good, &[]);
        self.b.switch_to_block(bad);
        if let Some(flag) = self.trace_fail {
            let one = self.b.ins().iconst(types::I64, 1);
            self.b.ins().store(ir::MemFlags::new(), one, flag, 0);
        }
        self.return_trace_placeholder();
        self.b.switch_to_block(good);
    }
    fn check_trace_failed(&mut self) {
        if let Some(flag) = self.trace_fail {
            let flagged = self.b.ins().load(types::I64, ir::MemFlags::new(), flag, 0);
            let cond = self.b.ins().icmp_imm(IntCC::NotEqual, flagged, 0);
            let bad = self.b.create_block();
            let good = self.b.create_block();
            self.branch(cond, bad, &[], good, &[]);
            self.b.switch_to_block(bad);
            self.return_trace_placeholder();
            self.b.switch_to_block(good);
        }
    }
    fn trace_call(&mut self, symbol: &str, args: &[ir::Value]) -> Result<ir::Value, String> {
        let mut signature = self.m.make_signature();
        for arg in args {
            signature
                .params
                .push(AbiParam::new(self.b.func.dfg.value_type(*arg)));
        }
        signature.returns.push(AbiParam::new(types::I64));
        let id = self
            .m
            .declare_function(symbol, Linkage::Import, &signature)
            .map_err(|e| e.to_string())?;
        let reference = self.m.declare_func_in_func(id, self.b.func);
        let call = self.b.ins().call(reference, args);
        let status = self.b.inst_results(call)[0];
        self.abort_on_nonzero_status(status);
        Ok(status)
    }
    fn trace_perform(&mut self, op: &str, values: &[ir::Value]) -> Result<ir::Value, String> {
        match (op, values) {
            ("current_ctx", []) => {
                let layout =
                    Layout::of(self.world, &Type::named(crate::trace_effects::SPAN_CONTEXT))?;
                let out = self.alloc(layout.size);
                self.trace_call("vesper_trace_current_ctx", &[out])?;
                Ok(out)
            }
            ("start_span", [schema, name]) => {
                let layout =
                    Layout::of(self.world, &Type::named(crate::trace_effects::ACTIVE_SPAN))?;
                let out = self.alloc(layout.size);
                self.trace_call("vesper_trace_start_span", &[*schema, *name, out])?;
                Ok(out)
            }
            ("start_child", [parent, schema, name]) => {
                let layout =
                    Layout::of(self.world, &Type::named(crate::trace_effects::ACTIVE_SPAN))?;
                let out = self.alloc(layout.size);
                self.trace_call("vesper_trace_start_child", &[*parent, *schema, *name, out])?;
                Ok(out)
            }
            ("emit_ok", [span, attr]) => {
                self.trace_call("vesper_trace_emit_ok", &[*span, *attr])?;
                Ok(self.zero())
            }
            ("emit_err", [span, code, msg]) => {
                self.trace_call("vesper_trace_emit_err", &[*span, *code, *msg])?;
                Ok(self.zero())
            }
            ("emit_attr", [span, attr]) => {
                self.trace_call("vesper_trace_emit_attr", &[*span, *attr])?;
                Ok(self.zero())
            }
            _ => Err(format!("unknown native Trace operation '{op}'")),
        }
    }
}
fn effect_key(e: &Effect) -> String {
    match &e.label {
        Some(label) if label.starts_with('$') => format!("$static:{label}"),
        Some(label) => format!("$label:{label}"),
        None => format!("$effect:{}", e.name),
    }
}
fn ir_place_expr(p: &crate::check::Place, span: Span) -> Expr {
    let mut e = Expr {
        kind: ExprKind::Name(p.root.clone()),
        span: span.clone(),
    };
    for f in &p.path {
        e = Expr {
            kind: ExprKind::Field(Box::new(e), f.clone()),
            span: span.clone(),
        };
    }
    e
}

fn fnv1a64(s: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn type_is_recursive(world: &World, ty: &Type) -> bool {
    fn walk(world: &World, ty: &Type, visiting: &mut BTreeSet<String>) -> bool {
        let ty = crate::numeric::default_type(ty);
        if crate::fip::is_box_type(world, &ty)
            && let Some(inner) = crate::fip::box_inner(world, &ty)
        {
            return walk(world, inner, visiting);
        }
        match &ty {
            Type::Tuple(ts) => ts.iter().any(|t| walk(world, t, visiting)),
            Type::Named(n, args) => {
                if let Some(a) = world.adts.get(n) {
                    let key = ty.to_string();
                    if !visiting.insert(key.clone()) {
                        return true;
                    }
                    let vars: BTreeMap<String, Type> = a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    let rec = a.variants.iter().any(|v| {
                        v.fields
                            .iter()
                            .any(|f| walk(world, &substitute(&f.ty, &vars), visiting))
                    });
                    visiting.remove(&key);
                    rec
                } else {
                    false
                }
            }
            _ => false,
        }
    }
    walk(world, ty, &mut BTreeSet::new())
}

fn helper_load(
    b: &mut FunctionBuilder<'_>,
    world: &World,
    ptr: ir::Value,
    ty: &Type,
) -> Result<ir::Value, String> {
    if Layout::of(world, ty)?.aggregate() {
        Ok(ptr)
    } else {
        Ok(b.ins().load(scalar(ty, &[])?, ir::MemFlags::new(), ptr, 0))
    }
}

fn emit_equal<M: Module>(
    b: &mut FunctionBuilder<'_>,
    m: &mut M,
    world: &World,
    a: ir::Value,
    b_val: ir::Value,
    ty: &Type,
) -> Result<ir::Value, String> {
    let ty = crate::numeric::default_type(ty);
    if type_is_recursive(world, &ty) {
        let id = declare_or_define_eq_helper(m, world, &ty)?;
        let fun = m.declare_func_in_func(id, b.func);
        let call = b.ins().call(fun, &[a, b_val]);
        return Ok(b.inst_results(call)[0]);
    }
    emit_equal_inline(b, m, world, a, b_val, &ty)
}

fn emit_equal_inline<M: Module>(
    b: &mut FunctionBuilder<'_>,
    m: &mut M,
    world: &World,
    a: ir::Value,
    b_val: ir::Value,
    ty: &Type,
) -> Result<ir::Value, String> {
    if matches!(ty, Type::Function { .. }) {
        return Ok(b.ins().iconst(types::I8, 0));
    }
    // Strings and views are equal when their bytes are (§13 strings).
    if crate::strings::is_text(ty) {
        let order = strings_native::emit_str_order(b, a, b_val);
        return Ok(b.ins().icmp_imm(IntCC::Equal, order, 0));
    }
    if crate::fip::is_box_type(world, ty) {
        let inner =
            crate::fip::box_inner(world, ty).ok_or_else(|| format!("Box[T] expected, got {ty}"))?;
        let av = helper_load(b, world, a, inner)?;
        let bv = helper_load(b, world, b_val, inner)?;
        return emit_equal(b, m, world, av, bv, inner);
    }
    let layout = Layout::of(world, ty)?;
    match layout.kind {
        LayoutKind::Scalar => Ok(if crate::numeric::is_float_name(ty.name()) {
            b.ins().fcmp(FloatCC::Equal, a, b_val)
        } else {
            b.ins().icmp(IntCC::Equal, a, b_val)
        }),
        LayoutKind::Product(fs) => emit_equal_fields(b, m, world, a, b_val, &fs),
        LayoutKind::Enum(vs) => {
            if vs.is_empty() {
                return Ok(b.ins().iconst(types::I8, 0));
            }
            let at = b.ins().load(types::I64, ir::MemFlags::new(), a, 0);
            let bt = b.ins().load(types::I64, ir::MemFlags::new(), b_val, 0);
            let tags_eq = b.ins().icmp(IntCC::Equal, at, bt);
            let done = b.create_block();
            b.append_block_param(done, types::I8);
            let mismatch = b.create_block();
            let mut check_block = b.create_block();
            b.ins().brif(tags_eq, check_block, &[], mismatch, &[]);

            b.switch_to_block(mismatch);
            let zero = b.ins().iconst(types::I8, 0);
            b.ins().jump(done, &[zero.into()]);

            for (i, v) in vs.iter().enumerate() {
                b.switch_to_block(check_block);
                let is_var = b.ins().icmp_imm(IntCC::Equal, at, i as i64);
                let this_var = b.create_block();
                let next_var = if i + 1 < vs.len() {
                    b.create_block()
                } else {
                    mismatch
                };
                b.ins().brif(is_var, this_var, &[], next_var, &[]);

                b.switch_to_block(this_var);
                let eq = emit_equal_fields(b, m, world, a, b_val, &v.fields)?;
                b.ins().jump(done, &[eq.into()]);

                check_block = next_var;
            }

            b.switch_to_block(done);
            Ok(b.block_params(done)[0])
        }
    }
}

fn emit_equal_fields<M: Module>(
    b: &mut FunctionBuilder<'_>,
    m: &mut M,
    world: &World,
    a: ir::Value,
    b_val: ir::Value,
    fs: &[LayoutField],
) -> Result<ir::Value, String> {
    let mut result = b.ins().iconst(types::I8, 1);
    for f in fs {
        let ap = b.ins().iadd_imm(a, i64::from(f.offset));
        let bp = b.ins().iadd_imm(b_val, i64::from(f.offset));
        let av = helper_load(b, world, ap, &f.ty)?;
        let bv = helper_load(b, world, bp, &f.ty)?;
        let eq = emit_equal(b, m, world, av, bv, &f.ty)?;
        result = b.ins().band(result, eq);
    }
    Ok(result)
}

fn declare_or_define_eq_helper<M: Module>(
    m: &mut M,
    world: &World,
    ty: &Type,
) -> Result<FuncId, String> {
    let sym = format!("__vesper_eq_{:016x}", fnv1a64(&ty.to_string()));
    if let Some(id) = m.get_name(&sym) {
        let cranelift_module::FuncOrDataId::Func(func_id) = id else {
            return Err(format!("{sym} collides with data symbol"));
        };
        return Ok(func_id);
    }
    let layout = Layout::of(world, ty)?;
    let arg_ty = if layout.aggregate() {
        m.target_config().pointer_type()
    } else {
        scalar(ty, &[])?
    };
    let mut sig = m.make_signature();
    sig.params.push(AbiParam::new(arg_ty));
    sig.params.push(AbiParam::new(arg_ty));
    sig.returns.push(AbiParam::new(types::I8));

    let jit = std::any::type_name::<M>().contains("JITModule");
    let linkage = if jit {
        Linkage::Preemptible
    } else {
        Linkage::Local
    };
    let id = m
        .declare_function(&sym, linkage, &sig)
        .map_err(|e| e.to_string())?;

    let mut ctx = m.make_context();
    ctx.func.signature = sig;
    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let params = b.block_params(entry);
        let a = params[0];
        let b_val = params[1];
        let eq = emit_equal_inline(&mut b, m, world, a, b_val, ty)?;
        b.ins().return_(&[eq]);
        b.seal_all_blocks();
        b.finalize();
    }
    if jit {
        DefinitionLinkage::Jit.normalize_references(&mut ctx.func);
    }
    m.define_function(id, &mut ctx).map_err(|e| e.to_string())?;
    m.clear_context(&mut ctx);
    Ok(id)
}

fn emit_fip_box_free<M: Module>(
    b: &mut FunctionBuilder<'_>,
    m: &mut M,
    ptr: ir::Value,
) -> Result<(), String> {
    let jit = std::any::type_name::<M>().contains("JITModule");
    let id = if jit {
        let mut sig = m.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        m.declare_function("vesper_box_free", Linkage::Import, &sig)
            .map_err(|e| e.to_string())?
    } else {
        if m.isa().triple().operating_system == target_lexicon::OperatingSystem::None_ {
            return Err(
                "Box[T] in a freestanding image needs a box runtime; the generated one uses the C library"
                    .into(),
            );
        }
        crate::fip::aot_runtime::declare_or_define_free(m)?
    };
    let reference = m.declare_func_in_func(id, b.func);
    b.ins().call(reference, &[ptr]);
    Ok(())
}

fn emit_drop_box_value<M: Module>(
    b: &mut FunctionBuilder<'_>,
    m: &mut M,
    world: &World,
    val: ir::Value,
    ty: &Type,
) -> Result<(), String> {
    let ty = crate::numeric::default_type(ty);
    if crate::dyn_trait::is_boxed(&ty) {
        return dyn_native::emit_drop_boxed(b, m, val);
    }
    if !crate::fip::type_contains_box(world, &ty) {
        return Ok(());
    }
    if type_is_recursive(world, &ty) {
        let id = declare_or_define_drop_helper(m, world, &ty)?;
        let fun = m.declare_func_in_func(id, b.func);
        b.ins().call(fun, &[val]);
        return Ok(());
    }
    emit_drop_box_inline(b, m, world, val, &ty)
}

fn emit_drop_box_inline<M: Module>(
    b: &mut FunctionBuilder<'_>,
    m: &mut M,
    world: &World,
    val: ir::Value,
    ty: &Type,
) -> Result<(), String> {
    if !crate::fip::type_contains_box(world, ty) {
        return Ok(());
    }
    if crate::dyn_trait::is_boxed(ty) {
        return dyn_native::emit_drop_boxed(b, m, val);
    }
    if crate::fip::is_box_type(world, ty) {
        let inner =
            crate::fip::box_inner(world, ty).ok_or_else(|| format!("Box[T] expected, got {ty}"))?;
        if crate::fip::type_contains_box(world, inner) {
            let non_null = b.create_block();
            let done = b.create_block();
            let is_null = b.ins().icmp_imm(IntCC::Equal, val, 0);
            b.ins().brif(is_null, done, &[], non_null, &[]);

            b.switch_to_block(non_null);
            let inner_val = helper_load(b, world, val, inner)?;
            emit_drop_box_value(b, m, world, inner_val, inner)?;
            emit_fip_box_free(b, m, val)?;
            b.ins().jump(done, &[]);

            b.switch_to_block(done);
            return Ok(());
        }
        return emit_fip_box_free(b, m, val);
    }
    let layout = Layout::of(world, ty)?;
    match layout.kind {
        LayoutKind::Scalar => Ok(()),
        LayoutKind::Product(fs) => {
            let box_fields: Vec<LayoutField> = fs
                .into_iter()
                .filter(|f| crate::fip::type_contains_box(world, &f.ty))
                .collect();
            if box_fields.is_empty() {
                return Ok(());
            }
            let non_null = b.create_block();
            let done = b.create_block();
            let is_null = b.ins().icmp_imm(IntCC::Equal, val, 0);
            b.ins().brif(is_null, done, &[], non_null, &[]);

            b.switch_to_block(non_null);
            for f in box_fields {
                let ptr = b.ins().iadd_imm(val, i64::from(f.offset));
                let fv = helper_load(b, world, ptr, &f.ty)?;
                emit_drop_box_value(b, m, world, fv, &f.ty)?;
            }
            b.ins().jump(done, &[]);

            b.switch_to_block(done);
            Ok(())
        }
        LayoutKind::Enum(vs) => {
            let box_variants: Vec<(usize, Vec<LayoutField>)> = vs
                .into_iter()
                .enumerate()
                .filter_map(|(i, v)| {
                    let bfs: Vec<LayoutField> = v
                        .fields
                        .into_iter()
                        .filter(|f| crate::fip::type_contains_box(world, &f.ty))
                        .collect();
                    (!bfs.is_empty()).then_some((i, bfs))
                })
                .collect();
            if box_variants.is_empty() {
                return Ok(());
            }
            let non_null = b.create_block();
            let done = b.create_block();
            let is_null = b.ins().icmp_imm(IntCC::Equal, val, 0);
            b.ins().brif(is_null, done, &[], non_null, &[]);

            b.switch_to_block(non_null);
            let tag = b.ins().load(types::I64, ir::MemFlags::new(), val, 0);
            let mut check_block = b.create_block();
            b.ins().jump(check_block, &[]);
            for (idx, (tag_idx, bfs)) in box_variants.iter().enumerate() {
                b.switch_to_block(check_block);
                let is_var = b.ins().icmp_imm(IntCC::Equal, tag, *tag_idx as i64);
                let this_var = b.create_block();
                let next_var = if idx + 1 < box_variants.len() {
                    b.create_block()
                } else {
                    done
                };
                b.ins().brif(is_var, this_var, &[], next_var, &[]);

                b.switch_to_block(this_var);
                for f in bfs {
                    let ptr = b.ins().iadd_imm(val, i64::from(f.offset));
                    let fv = helper_load(b, world, ptr, &f.ty)?;
                    emit_drop_box_value(b, m, world, fv, &f.ty)?;
                }
                b.ins().jump(done, &[]);
                check_block = next_var;
            }
            b.switch_to_block(done);
            Ok(())
        }
    }
}

fn declare_or_define_drop_helper<M: Module>(
    m: &mut M,
    world: &World,
    ty: &Type,
) -> Result<FuncId, String> {
    let sym = format!("__vesper_drop_{:016x}", fnv1a64(&ty.to_string()));
    if let Some(id) = m.get_name(&sym) {
        let cranelift_module::FuncOrDataId::Func(func_id) = id else {
            return Err(format!("{sym} collides with data symbol"));
        };
        return Ok(func_id);
    }
    let layout = Layout::of(world, ty)?;
    let arg_ty = if layout.aggregate() {
        m.target_config().pointer_type()
    } else {
        scalar(ty, &[])?
    };
    let mut sig = m.make_signature();
    sig.params.push(AbiParam::new(arg_ty));

    let jit = std::any::type_name::<M>().contains("JITModule");
    let linkage = if jit {
        Linkage::Preemptible
    } else {
        Linkage::Local
    };
    let id = m
        .declare_function(&sym, linkage, &sig)
        .map_err(|e| e.to_string())?;

    let mut ctx = m.make_context();
    ctx.func.signature = sig;
    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let val = b.block_params(entry)[0];
        emit_drop_box_inline(&mut b, m, world, val, ty)?;
        b.ins().return_(&[]);
        b.seal_all_blocks();
        b.finalize();
    }
    if jit {
        DefinitionLinkage::Jit.normalize_references(&mut ctx.func);
    }
    m.define_function(id, &mut ctx).map_err(|e| e.to_string())?;
    m.clear_context(&mut ctx);
    Ok(id)
}

extern "C" fn native_float_remainder(a: f64, b: f64) -> f64 {
    a % b
}

#[path = "native_batch.rs"]
pub mod batch;

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod far_call_tests {
    use super::*;
    use cranelift_codegen::{Context, binemit::Reloc, control::ControlPlane, isa::OwnedTargetIsa};
    use cranelift_jit::{BranchProtection, JITMemoryProvider};
    use cranelift_module::{ModuleError, ModuleResult};
    use std::{io, panic::AssertUnwindSafe};

    const GAP: usize = 1 << 32;

    /// Own one virtual reservation, with only its first and last pages backed.
    /// The two functions cannot accidentally land within an x86 rel32 span.
    struct FarPages {
        base: *mut u8,
        page: usize,
        used: usize,
        reverse: bool,
        data: bool,
    }

    // SAFETY: this uniquely owned reservation moves with its JIT module. No
    // pointers escape the test, and the module is never accessed concurrently.
    unsafe impl Send for FarPages {}

    impl FarPages {
        fn new(reverse: bool) -> Self {
            // SAFETY: sysconf does not access Rust memory.
            let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
            assert!(page.is_power_of_two());
            // This is virtual address space, not four GiB of committed memory.
            // SAFETY: a fresh anonymous reservation; no existing mapping is replaced.
            let base = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    GAP + page,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                    -1,
                    0,
                )
            };
            assert_ne!(base, libc::MAP_FAILED, "{}", io::Error::last_os_error());
            Self {
                base: base.cast(),
                page,
                used: 0,
                reverse,
                data: false,
            }
        }

        fn address(&self, slot: usize) -> *mut u8 {
            assert!(slot < 2);
            // SAFETY: both pages lie in the single reservation this provider owns.
            unsafe { self.base.add((slot ^ usize::from(self.reverse)) * GAP) }
        }

        unsafe fn release(&mut self) {
            if !self.base.is_null() {
                // SAFETY: freeing exactly our reservation after every call returned.
                assert_eq!(
                    unsafe { libc::munmap(self.base.cast(), GAP + self.page) },
                    0
                );
                self.base = std::ptr::null_mut();
            }
        }
    }

    impl Drop for FarPages {
        fn drop(&mut self) {
            // SAFETY: test-owned code pointers never outlive the JIT module/provider.
            unsafe { self.release() };
        }
    }

    impl JITMemoryProvider for FarPages {
        fn allocate_readexec(&mut self, size: usize, align: u64) -> io::Result<*mut u8> {
            if self.used == (if self.data { 1 } else { 2 })
                || size > self.page
                || align > self.page as u64
            {
                return Err(io::Error::other(
                    "far-call fixture needs two tiny functions",
                ));
            }
            let at = self.address(self.used);
            // SAFETY: committing only the owned page selected for this function.
            if unsafe { libc::mprotect(at.cast(), self.page, libc::PROT_READ | libc::PROT_WRITE) }
                != 0
            {
                return Err(io::Error::last_os_error());
            }
            self.used += 1;
            Ok(at)
        }

        fn allocate_readwrite(&mut self, _: usize, _: u64) -> io::Result<*mut u8> {
            Err(io::Error::other("far-call fixture has no writable data"))
        }

        fn allocate_readonly(&mut self, size: usize, align: u64) -> io::Result<*mut u8> {
            assert_eq!(self.used, 0);
            assert!(!self.data && size <= self.page && align <= self.page as u64);
            let at = self.address(1);
            // SAFETY: committing the opposite owned page, before any code allocation.
            if unsafe { libc::mprotect(at.cast(), self.page, libc::PROT_READ | libc::PROT_WRITE) }
                != 0
            {
                return Err(io::Error::last_os_error());
            }
            self.data = true;
            Ok(at)
        }

        unsafe fn free_memory(&mut self) {
            // SAFETY: the caller guarantees that no retrieved code pointers remain live.
            unsafe { self.release() };
        }

        fn finalize(&mut self, protection: BranchProtection) -> ModuleResult<()> {
            assert_eq!(protection, BranchProtection::None);
            assert_eq!(self.used + usize::from(self.data), 2);
            for slot in 0..2 {
                // SAFETY: both pages are owned code allocations; x86 instruction
                // caches are coherent. No call occurs before this finalization.
                if unsafe {
                    libc::mprotect(
                        self.address(slot).cast(),
                        self.page,
                        if self.data && slot == 1 {
                            libc::PROT_READ
                        } else {
                            libc::PROT_READ | libc::PROT_EXEC
                        },
                    )
                } != 0
                {
                    return Err(ModuleError::Allocation {
                        message: "protect far-call fixture",
                        err: io::Error::last_os_error(),
                    });
                }
            }
            Ok(())
        }
    }

    fn world() -> World {
        let parsed = crate::parser::parse(
            "fn aaa_target(x: i64) -> i64 { wrapping_add(x, 1) } \
             fn main() -> i64 { aaa_target(41) }",
        );
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = crate::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        checked.world
    }

    fn module(reverse: bool) -> (JITModule, OwnedTargetIsa) {
        let mut flags = settings::builder();
        flags.set("is_pic", "false").unwrap();
        flags.set("use_colocated_libcalls", "false").unwrap();
        let isa = cranelift_native::builder()
            .unwrap()
            .finish(settings::Flags::new(flags))
            .unwrap();
        let mut builder =
            JITBuilder::with_isa(isa.clone(), cranelift_module::default_libcall_names());
        builder.memory_provider(Box::new(FarPages::new(reverse)));
        (JITModule::new(builder), isa)
    }

    fn relocations(isa: &OwnedTargetIsa, function: EmittedFunction, out: &mut Vec<Reloc>) {
        if function.source != "main" {
            return;
        }
        // Inspect machine relocations from the actual production-lowered function,
        // using the same ISA as the JIT module, rather than a handwritten CLIF call.
        let mut context = Context::for_function(function.function);
        context
            .compile(isa.as_ref(), &mut ControlPlane::default())
            .unwrap();
        out.extend(
            context
                .compiled_code()
                .unwrap()
                .buffer
                .relocs()
                .iter()
                .map(|relocation| relocation.kind),
        );
    }

    #[test]
    fn near_calls_fail_and_jit_far_calls_execute_across_four_gib() {
        let world = world();
        for reverse in [false, true] {
            let (mut old, isa) = module(reverse);
            let mut near = Vec::new();
            // The unchanged object policy is exactly the old JIT's final-linkage
            // behavior: Export/Local make direct calls assume colocation.
            let (ids, _) = compile(
                &mut old,
                &world,
                "main",
                false,
                Some(&mut |function| relocations(&isa, function, &mut near)),
                None,
                CodegenOptions::default(),
            )
            .unwrap();
            assert_eq!(
                old.declarations().get_function_decl(ids["main"]).linkage,
                Linkage::Export
            );
            assert_eq!(
                old.declarations()
                    .get_function_decl(ids["aaa_target"])
                    .linkage,
                Linkage::Local
            );
            assert!(near.contains(&Reloc::X86CallPCRel4), "{near:?}");
            let panic = std::panic::catch_unwind(AssertUnwindSafe(|| {
                old.finalize_definitions()
                    .map_err(|error| error.to_string())
            }))
            .expect_err("near relocation must fail across the forced four-GiB gap");
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("");
            assert!(message.contains("TryFromIntError"), "{message}");
            eprintln!("old near relocations {near:?}, reverse={reverse}: {message}");
            drop(old);

            let (mut fixed, isa) = module(reverse);
            let mut far = Vec::new();
            let (ids, _) = compile_jit(
                &mut fixed,
                &world,
                "main",
                false,
                Some(&mut |function| relocations(&isa, function, &mut far)),
                None,
                CodegenOptions::default(),
            )
            .unwrap();
            assert_eq!(
                fixed.declarations().get_function_decl(ids["main"]).linkage,
                Linkage::Preemptible
            );
            assert_eq!(
                fixed
                    .declarations()
                    .get_function_decl(ids["aaa_target"])
                    .linkage,
                Linkage::Preemptible
            );
            assert!(far.contains(&Reloc::Abs8), "{far:?}");
            assert!(!far.contains(&Reloc::X86CallPCRel4), "{far:?}");
            fixed.finalize_definitions().unwrap();
            let entry = fixed.get_finalized_function(ids["main"]);
            let target = fixed.get_finalized_function(ids["aaa_target"]);
            assert_eq!((entry as usize).abs_diff(target as usize), GAP);
            assert_eq!((target as usize) > (entry as usize), reverse);
            // SAFETY: actual production compilation checked fn() -> i64; the module
            // owns both finalized executable pages until after this call returns.
            let call: unsafe extern "C" fn() -> i64 = unsafe { std::mem::transmute(entry) };
            // SAFETY: the signature and code lifetime are established above.
            assert_eq!(unsafe { call() }, 42);
            eprintln!(
                "fixed far relocations {far:?}: target={target:p}, entry={entry:p}, result=42"
            );
            // SAFETY: the only code call returned, and no function pointer escapes.
            unsafe { fixed.free_memory() };
        }
    }

    /// Generated Local helpers and Export data keep their declarations; only
    /// the JIT IR references must stop assuming nearby allocations.
    #[test]
    fn generated_local_helpers_and_data_execute_across_four_gib() {
        for reverse in [false, true] {
            for data in [false, true] {
                for normalize in [false, true] {
                    let (mut m, isa) = module(reverse);
                    let mut sig = m.make_signature();
                    sig.returns.push(AbiParam::new(types::I64));
                    let helper = if data {
                        None
                    } else {
                        let id = m
                            .declare_function("foreign_leaf", Linkage::Local, &sig)
                            .unwrap();
                        let mut ctx = m.make_context();
                        ctx.func.signature = sig.clone();
                        let mut fb = FunctionBuilderContext::new();
                        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb);
                        let block = b.create_block();
                        b.switch_to_block(block);
                        let value = b.ins().iconst(types::I64, 42);
                        b.ins().return_(&[value]);
                        b.seal_all_blocks();
                        b.finalize();
                        m.define_function(id, &mut ctx).unwrap();
                        Some(id)
                    };
                    let datum = if data {
                        let id = m
                            .declare_data("mask_data", Linkage::Export, false, false)
                            .unwrap();
                        let mut description = cranelift_module::DataDescription::new();
                        description.define(42i64.to_le_bytes().to_vec().into_boxed_slice());
                        description.set_align(8);
                        m.define_data(id, &description).unwrap();
                        Some(id)
                    } else {
                        None
                    };
                    let entry = m.declare_function("caller", Linkage::Local, &sig).unwrap();
                    let mut ctx = m.make_context();
                    ctx.func.signature = sig;
                    let mut fb = FunctionBuilderContext::new();
                    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb);
                    let block = b.create_block();
                    b.switch_to_block(block);
                    let value = if let Some(id) = datum {
                        let symbol = m.declare_data_in_func(id, b.func);
                        let address = b.ins().symbol_value(types::I64, symbol);
                        b.ins().load(types::I64, ir::MemFlags::new(), address, 0)
                    } else {
                        let target = m.declare_func_in_func(helper.unwrap(), b.func);
                        let call = b.ins().call(target, &[]);
                        b.inst_results(call)[0]
                    };
                    b.ins().return_(&[value]);
                    b.seal_all_blocks();
                    b.finalize();
                    let before = ctx.func.to_string();
                    let mode = if normalize {
                        DefinitionLinkage::Jit
                    } else {
                        DefinitionLinkage::Object
                    };
                    mode.normalize_references(&mut ctx.func);
                    if !normalize {
                        assert_eq!(before, ctx.func.to_string());
                    }
                    let mut inspect = Context::for_function(ctx.func.clone());
                    inspect
                        .compile(isa.as_ref(), &mut ControlPlane::default())
                        .unwrap();
                    let relocs: Vec<_> = inspect
                        .compiled_code()
                        .unwrap()
                        .buffer
                        .relocs()
                        .iter()
                        .map(|r| r.kind)
                        .collect();
                    if normalize {
                        assert!(relocs.contains(&Reloc::Abs8), "{relocs:?}");
                        assert!(
                            !relocs.contains(&Reloc::X86PCRel4)
                                && !relocs.contains(&Reloc::X86CallPCRel4),
                            "{relocs:?}"
                        );
                    } else {
                        assert!(
                            relocs.contains(&Reloc::X86PCRel4)
                                || relocs.contains(&Reloc::X86CallPCRel4),
                            "{relocs:?}"
                        );
                    }
                    m.define_function(entry, &mut ctx).unwrap();
                    if normalize {
                        m.finalize_definitions().unwrap();
                        // SAFETY: the test constructed fn() -> i64 and owns all code/data until it returns.
                        let call: unsafe extern "C" fn() -> i64 =
                            unsafe { std::mem::transmute(m.get_finalized_function(entry)) };
                        // SAFETY: checked signature and finalized live mappings above.
                        assert_eq!(unsafe { call() }, 42);
                        eprintln!(
                            "normalized local/data reference reverse={reverse} data={data}: {relocs:?}, result=42"
                        );
                        // SAFETY: the call returned, and no pointer escapes.
                        unsafe { m.free_memory() };
                    } else {
                        let panic = std::panic::catch_unwind(AssertUnwindSafe(|| {
                            m.finalize_definitions().map_err(|e| e.to_string())
                        }));
                        assert!(
                            panic.is_err(),
                            "near reference must overflow reverse={reverse} data={data}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ordinary_instrumented_batch_and_session_jits_call_their_helpers() {
        let world = world();
        for optimize in [false, true] {
            assert_eq!(
                jit_with_options(&world, "main", optimize)
                    .unwrap()
                    .to_string(),
                "42"
            );
        }
        let mut unchanged = |_: &str, _: &mut ir::Function| Ok(());
        // SAFETY: the no-op hook inserts no memory accesses or addresses.
        let mut instrumented =
            unsafe { InstrumentedJit::compile(&world, "main", &mut unchanged).unwrap() };
        assert_eq!(instrumented.run(), 42);
        let mut batch =
            batch::NativeBatch::compile(&world, &["main"], batch::BatchLimits::default()).unwrap();
        assert_eq!(batch.run("main").unwrap().to_string(), "42");
        assert_eq!(batch.stats().defined_functions, 2);
        for optimize in [false, true] {
            let session =
                session::NativeSession::compile_with_options(&world, "main", &[], optimize)
                    .unwrap();
            let exit = session.fiber().unwrap().start().exited().unwrap();
            assert!(matches!(
                session.decode_exit(&world, &exit).unwrap(),
                session::SessionResult::Returned(crate::runtime::Value::Int(42))
            ));
        }
    }
}
