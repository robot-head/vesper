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

/// Checked source entries with declared linear external effects compiled to
/// ABI v2 fiber entries that park through `vesper-fiber-abi`.
#[path = "native_session.rs"]
pub mod session;
/// Windows x64 COFF objects with unwind tables, and the MSVC link driver.
#[path = "native_windows.rs"]
pub mod windows;

fn scalar(t: &Type, generics: &[Generic]) -> Result<ir::Type, String> {
    match t {
        Type::IntLiteral(_) => Ok(types::I64),
        Type::Named(n, _) if n == "f64" => Ok(types::F64),
        Type::Named(n, _)
            if matches!(
                n.as_str(),
                "u8" | "u32" | "u64" | "i64" | "bool" | "Unit" | "Never"
            ) || generics.iter().any(|g| g.name == *n) =>
        {
            Ok(types::I64)
        }
        Type::Tuple(ts) if ts.is_empty() => Ok(types::I64),
        Type::Infer | Type::Function { .. } => Ok(types::I64),
        _ => Err(format!(
            "native backend does not yet lower aggregate type {t}"
        )),
    }
}
fn signature<M: Module>(m: &M, world: &World, f: &Function) -> Result<ir::Signature, String> {
    let mut sig = m.make_signature();
    let result = Layout::of(world, &f.result)?;
    if result.aggregate() {
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
    if fail_only(world, f) {
        sig.params
            .push(AbiParam::new(m.target_config().pointer_type()));
    }
    sig.returns.push(AbiParam::new(if result.aggregate() {
        types::I64
    } else {
        scalar(&f.result, &f.generics)?
    }));
    Ok(sig)
}
fn instance_key(name: &str, vars: &BTreeMap<String, Type>) -> String {
    if vars.is_empty() {
        name.into()
    } else {
        format!("{name}${vars:?}")
    }
}
fn concrete(world: &World, name: &str, vars: &BTreeMap<String, Type>) -> Result<Function, String> {
    let mut f = world
        .functions
        .get(name)
        .ok_or_else(|| format!("unknown function '{name}'"))?
        .clone();
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
        _ => {}
    }
}
fn contains_function(world: &World, t: &Type) -> bool {
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
fn capability_only(f: &Function) -> bool {
    f.effects
        .instances
        .iter()
        .all(|e| crate::capability_effects::is_capability_effect(&e.name))
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
fn fail_only(world: &World, f: &Function) -> bool {
    f.effects.tail.is_none()
        && !f.effects.instances.is_empty()
        && f.effects
            .instances
            .iter()
            .all(|e| e.name == "Fail" && e.label.is_none() && e.args.len() == 1)
        && !contains_function(world, &f.result)
        && !f.params.iter().any(|p| contains_function(world, &p.ty))
}
/// Bytes of a failure record: an instance tag followed by the error payload.
fn fail_record_size(world: &World, f: &Function) -> Result<u32, String> {
    let mut payload = 8;
    for e in &f.effects.instances {
        payload = payload.max(Layout::of(world, &e.args[0])?.size);
    }
    Ok(8 + payload.div_ceil(8) * 8)
}
type InstanceMap = BTreeMap<String, (String, BTreeMap<String, Type>)>;
fn instances(world: &World, entry: &str) -> Result<InstanceMap, String> {
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
        if !specialized_function(world, &f) {
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
                    if let Some(name) = target {
                        todo.push((name, call_vars(world, e, &vars)));
                    }
                }
            });
        }
    }
    Ok(result)
}
fn compile<M: Module>(
    m: &mut M,
    world: &World,
    entry: &str,
    optimize: bool,
    observer: Option<&mut dyn FnMut(EmittedFunction)>,
    instrument: Option<Instrument<'_>>,
) -> Result<(BTreeMap<String, FuncId>, String), String> {
    let entry_fn = world.functions.get(entry).ok_or("unknown entry")?;
    if !entry_fn.effects.instances.is_empty() || entry_fn.effects.tail.is_some() {
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
    let instances = instances(world, entry)?;
    let mut program = crate::ir::lower_world(world);
    if optimize {
        crate::ir_opt::optimize(world, &mut program)?;
    }
    let ids = declare_instances(m, world, entry, &instances)?;
    let clif =
        define_instances_observed(m, world, &instances, &ids, &program, observer, instrument)?;
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
}
/// Declare every monomorphized ordinary function. `entry` is exported under
/// its source name when it is itself an ordinary instance.
fn declare_instances<M: Module>(
    m: &mut M,
    world: &World,
    entry: &str,
    instances: &InstanceMap,
) -> Result<BTreeMap<String, FuncId>, String> {
    let mut ids = BTreeMap::new();
    for (i, (key, (name, vars))) in instances.iter().enumerate() {
        let f = concrete(world, name, vars)?;
        let symbol = if key == entry {
            entry.to_owned()
        } else {
            format!("vesper_function_{i}")
        };
        let id = m
            .declare_function(
                &symbol,
                if key == entry {
                    Linkage::Export
                } else {
                    Linkage::Local
                },
                &signature(m, world, &f)?,
            )
            .map_err(|e| e.to_string())?;
        ids.insert(key.clone(), id);
    }
    Ok(ids)
}
/// Define the bodies of the instances declared by [`declare_instances`].
fn define_instances<M: Module>(
    m: &mut M,
    world: &World,
    instances: &InstanceMap,
    ids: &BTreeMap<String, FuncId>,
    program: &crate::ir::Program,
) -> Result<String, String> {
    define_instances_observed(m, world, instances, ids, program, None, None)
}
fn define_instances_observed<M: Module>(
    m: &mut M,
    world: &World,
    instances: &InstanceMap,
    ids: &BTreeMap<String, FuncId>,
    program: &crate::ir::Program,
    mut observer: Option<&mut dyn FnMut(EmittedFunction)>,
    mut instrument: Option<Instrument<'_>>,
) -> Result<String, String> {
    let mut clif = String::new();
    for (key, (name, generic_vars)) in instances {
        let f = concrete(world, name, generic_vars)?;
        let mut ctx = m.make_context();
        ctx.func.signature = signature(m, world, &f)?;
        let mut fbctx = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let mut values = b.block_params(entry).to_vec();
            let fail_record = if fail_only(world, &f) {
                values.pop()
            } else {
                None
            };
            let result_layout = Layout::of(world, &f.result)?;
            let sret = if result_layout.aggregate() {
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
                inline_depth: 0,
                fail_record: fail_record.map(|record| {
                    (
                        record,
                        f.effects
                            .instances
                            .iter()
                            .map(|e| e.args[0].clone())
                            .collect(),
                    )
                }),
                session: None,
            };
            if fail_record.is_some() {
                for (index, effect) in f.effects.instances.iter().enumerate() {
                    let identity = format!("native-fail-exit:{index}");
                    c.capabilities
                        .insert(format!("$effect:{}", effect.name), identity.clone());
                    c.capabilities
                        .insert(format!("$static:$fncap:{name}:{index}"), identity.clone());
                    if let Some(key) = crate::numeric::instance_key(&effect.name, &effect.args) {
                        c.capabilities.insert(key, identity);
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
        clif.push_str(&format!("; {key}\n{}\n", ctx.func.display()));
        if let Some(observe) = observer.as_mut() {
            let id = ids[key];
            let mut function = ctx.func.clone();
            function.name = ir::UserFuncName::user(0, id.as_u32());
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
                    .get_function_decl(id)
                    .linkage_name(id)
                    .into_owned(),
                func_id: id.as_u32(),
                function,
                callees,
            });
        }
        m.define_function(ids[key], &mut ctx)
            .map_err(|e| format!("native codegen '{name}': {e}"))?;
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
    let f = world.functions.get(entry).ok_or("missing entry function")?;
    if !f.params.is_empty() {
        return Err("JIT entry must take no arguments".into());
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
    builder.symbol("fmod", native_float_remainder as *const u8);
    let mut m = JITModule::new(builder);
    let (ids, _) = compile(&mut m, world, entry, optimize, None, None)?;
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
            crate::layout::decode(
                world,
                &f.result,
                std::slice::from_raw_parts(result.as_ptr().cast::<u8>(), layout.size as usize),
            )?
        } else if f.result.name() == "f64" {
            let call: unsafe extern "C" fn() -> f64 = std::mem::transmute(code);
            crate::runtime::Value::Float(call())
        } else {
            let call: unsafe extern "C" fn() -> i64 = std::mem::transmute(code);
            let n = call();
            if f.result.name() == "bool" {
                crate::runtime::Value::Bool(n != 0)
            } else if f.result == Type::unit() || f.result.name() == "Unit" {
                crate::runtime::Value::Tuple(vec![])
            } else {
                if let Some(k) = crate::numeric::IntegerKind::from_type(&f.result) {
                    if k.unsigned() {
                        crate::runtime::Value::UInt(n as u64 & k.mask(), k.bits())
                    } else {
                        crate::runtime::Value::Int(n)
                    }
                } else {
                    crate::runtime::Value::Int(n)
                }
            }
        }
    };
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
    isa_with_flags(target, false)
}
/// [`object_isa`] for `world`: NaN canonicalisation is enabled when the
/// program has a DetFloat function ([`needs_nan_canonicalization`]).
pub fn object_isa_for(
    world: &World,
    target: Option<&str>,
) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>, String> {
    isa_with_flags(target, needs_nan_canonicalization(world))
}
fn isa_with_flags(
    target: Option<&str>,
    canonicalize_nans: bool,
) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>, String> {
    let mut flags = settings::builder();
    flags.set("is_pic", "true").map_err(|e| e.to_string())?;
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
    let isa = object_isa_for(world, target)?;
    if windows::is_windows(isa.triple()) {
        return windows::object(world, entry, isa, optimize, observer, instrument);
    }
    let builder = ObjectBuilder::new(isa, "vesper", cranelift_module::default_libcall_names())
        .map_err(|e| e.to_string())?;
    let mut m = ObjectModule::new(builder);
    let (_, clif) = compile(&mut m, world, entry, optimize, observer, instrument)?;
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
        builder.symbol("fmod", native_float_remainder as *const u8);
        let mut owned = Self {
            module: Some(JITModule::new(builder)),
            code: std::ptr::null(),
            _single_thread: std::marker::PhantomData,
        };
        let m = owned.module.as_mut().expect("owned JIT module");
        let (ids, _) = compile(m, world, entry, false, None, Some(instrument))?;
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
    inline_depth: usize,
    /// The failure-record pointer of a `fail_only` function being compiled,
    /// with the error type of each of its `Fail` instances.
    fail_record: Option<(ir::Value, Vec<Type>)>,
    /// Present only while compiling a suspending session entry: external
    /// operations park instead of resolving to a lexical handler.
    session: Option<session::SessionCodegen>,
}
impl<M: Module> Codegen<'_, '_, M> {
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
        self.ir_graph(&graph, None, Some(join))?;
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
    fn bind_ir(&mut self, p: &Pattern, value: ir::Value, ty: &Type) -> Result<(), String> {
        match &p.kind {
            PatternKind::Bind(n) => {
                let var = *self.vars.get(n).ok_or("IR binding lacks a declared slot")?;
                self.def_var(var, value);
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
                    self.bind_ir(p, v, &f.ty)?;
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
                    self.bind_ir(p, v, &f.ty)?;
                }
                Ok(())
            }
            _ => Ok(()),
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
        for (n, t) in &f.locals {
            let ty = crate::numeric::default_type(&substitute(t, &self.generic_vars));
            let layout = Layout::of(self.world, &ty)?;
            let var = if let Some(var) = self.vars.get(n) {
                *var
            } else {
                let native_ty = if layout.aggregate() {
                    self.m.target_config().pointer_type()
                } else {
                    scalar(&ty, &[])?
                };
                let var = self.b.declare_var(native_ty);
                let zero = if native_ty == types::F64 {
                    self.b.ins().f64const(0.0)
                } else {
                    self.b.ins().iconst(native_ty, 0)
                };
                self.def_var(var, zero);
                self.vars.insert(n.clone(), var);
                self.local_types.insert(n.clone(), ty.clone());
                var
            };
            if !layout.aggregate() && !self.pointers.contains_key(&var.as_u32()) {
                let value = self.use_var(var);
                let ptr = self.alloc(8);
                self.store(ptr, value, &ty)?;
                self.set_pointer(var, ptr);
            }
        }
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
            let zero = if native_ty == types::F64 {
                self.b.ins().f64const(0.0)
            } else {
                self.b.ins().iconst(native_ty, 0)
            };
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
        let order = petgraph::algo::toposort(&graph, None).map_err(
            |_| "native closure specialization requires structured acyclic function CFGs",
        )?;
        for i in order {
            let block = &f.blocks[i];
            if self.b.current_block() != Some(blocks[i]) {
                self.b.switch_to_block(blocks[i]);
            }
            for id in &block.params {
                self.def_var(registers[id], parameters[id]);
            }
            for inst in &block.instructions {
                for (id, register) in &registers {
                    values.insert(*id, self.use_var(*register));
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
                    Op::Constant(l) => self.expr(&make(ExprKind::Literal(l.clone())))?,
                    Op::Unit => self.zero(),
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
                            self.expr(&e)?
                        }
                    }
                    Op::Bind { pattern, value } => {
                        let t = crate::numeric::default_type(&substitute(
                            &f.values[value],
                            &self.generic_vars,
                        ));
                        self.bind_ir(pattern, values[value], &t)?;
                        self.zero()
                    }
                    Op::Write { place, op, value } => {
                        let mut path = place.split('.');
                        let p = crate::check::Place {
                            root: path.next().ok_or("empty IR place")?.into(),
                            path: path.map(str::to_owned).collect(),
                        };
                        let left = ir_place_expr(&p, inst.span.clone());
                        let right = self.ir_temp(*value, &values, &f.values)?;
                        self.expr(&make(ExprKind::Assign(
                            op.clone(),
                            Box::new(left),
                            Box::new(right),
                        )))?
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
                    Op::Tuple(ids) => {
                        let es = ids
                            .iter()
                            .map(|id| self.ir_temp(*id, &values, &f.values))
                            .collect::<Result<Vec<_>, _>>()?;
                        self.data(
                            None,
                            es.iter()
                                .enumerate()
                                .map(|(i, e)| (i.to_string(), e))
                                .collect(),
                            &ty,
                        )?
                    }
                    Op::Data(name, fields) => {
                        let es = fields
                            .iter()
                            .map(|(n, id)| Ok((n.clone(), self.ir_temp(*id, &values, &f.values)?)))
                            .collect::<Result<Vec<_>, String>>()?;
                        self.data(
                            Some(name),
                            es.iter().map(|(n, e)| (n.clone(), e)).collect(),
                            &ty,
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
                                let expr = if a.pass == Pass::Inout
                                    || (a.pass == Pass::In
                                        && !self.world.functions.contains_key(callee)
                                        && a.place.is_some())
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
                        if self.world.functions.contains_key(callee) {
                            self.call_function(callee, &es, &site)?
                        } else if let Some((effect, op)) = callee.split_once("::") {
                            self.perform(effect, capability.as_deref(), instance, op, &es)?
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
            }
            for (id, register) in &registers {
                values.insert(*id, self.use_var(*register));
            }
            match &block.terminator {
                Terminator::Return(id) => {
                    let ty =
                        crate::numeric::default_type(&substitute(&f.result, &self.generic_vars));
                    let value = if f.values[id].name() == "Never" {
                        self.default_value(&ty)?
                    } else {
                        values[id]
                    };
                    let value = if let Some(output) = sret {
                        let layout = Layout::of(self.world, &ty)?;
                        self.copy_bytes(value, output, layout.size);
                        self.zero()
                    } else {
                        value
                    };
                    if let Some(join) = return_to {
                        self.jump(join, &[value.into()]);
                    } else {
                        self.b.ins().return_(&[value]);
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
                    self.jump(
                        blocks[*block],
                        &vs.iter().map(|v| (*v).into()).collect::<Vec<_>>(),
                    );
                }
                Terminator::Branch { condition, yes, no } => {
                    self.branch(values[condition], blocks[*yes], &[], blocks[*no], &[]);
                }
                Terminator::Match { value, arms } => {
                    let ty = crate::numeric::default_type(&substitute(
                        &f.values[value],
                        &self.generic_vars,
                    ));
                    for (p, target) in arms {
                        let next = self.b.create_block();
                        let condition = self.pattern_condition(p, values[value], &ty)?;
                        self.branch(condition, blocks[*target], &[], next, &[]);
                        self.b.switch_to_block(next);
                    }
                    self.b.ins().trap(ir::TrapCode::unwrap_user(1));
                }
                Terminator::Unreachable => {
                    self.b.ins().trap(ir::TrapCode::unwrap_user(1));
                }
            }
        }
        Ok(())
    }
    fn default_value(&mut self, ty: &Type) -> Result<ir::Value, String> {
        let l = Layout::of(self.world, ty)?;
        Ok(if l.aggregate() {
            self.alloc(l.size)
        } else if ty.name() == "f64" {
            self.b.ins().f64const(0.0)
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
        match &p.kind {
            PatternKind::Bind(n) => {
                let var = self.b.declare_var(self.b.func.dfg.value_type(value));
                self.def_var(var, value);
                self.vars.insert(n.clone(), var);
                self.local_types.insert(n.clone(), ty.clone());
                Ok(())
            }
            PatternKind::Wild | PatternKind::Literal(_) => Ok(()),
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
                    self.bind(p, v, &f.ty)?;
                }
                Ok(())
            }
            PatternKind::Constructor { name, fields, .. } => {
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
                    self.bind(p, v, &f.ty)?;
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
                    let n = self.b.ins().f64const(*n);
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
                let layout = Layout::of(self.world, ty)?;
                let (tag, fs) = layout
                    .variant(name)
                    .ok_or("native pattern has unknown variant")?;
                let mut result = if matches!(layout.kind, LayoutKind::Enum(_)) {
                    let actual = self.b.ins().load(types::I64, ir::MemFlags::new(), value, 0);
                    self.b.ins().icmp_imm(IntCC::Equal, actual, tag as i64)
                } else {
                    self.b.ins().iconst(types::I8, 1)
                };
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
        })
    }
    fn string(&mut self, text: &str) -> Result<ir::Value, String> {
        let layout = Layout::of(self.world, &Type::named("String"))?;
        if text.len() > self.world.string_capacity {
            return Err("string exceeds the checked module's literal capacity".into());
        }
        let pointer = self.alloc(layout.size);
        let length = self.b.ins().iconst(types::I64, text.len() as i64);
        self.b.ins().store(ir::MemFlags::new(), length, pointer, 0);
        for (index, bytes) in text.as_bytes().chunks(8).enumerate() {
            let mut word = [0; 8];
            word[..bytes.len()].copy_from_slice(bytes);
            let value = self.b.ins().iconst(types::I64, i64::from_le_bytes(word));
            self.b.ins().store(
                ir::MemFlags::new(),
                value,
                pointer,
                ((index + 1) * 8) as i32,
            );
        }
        Ok(pointer)
    }
    fn data(
        &mut self,
        name: Option<&str>,
        fields: Vec<(String, &Expr)>,
        ty: &Type,
    ) -> Result<ir::Value, String> {
        let layout = Layout::of(self.world, ty)?;
        if !layout.aggregate() {
            return Ok(self.zero());
        }
        let ptr = self.alloc(layout.size);
        let fs = if let Some(name) = name {
            let (tag, fs) = layout.variant(name).ok_or("unknown native constructor")?;
            if matches!(layout.kind, LayoutKind::Enum(_)) {
                let tag = self.b.ins().iconst(types::I64, tag as i64);
                self.b.ins().store(ir::MemFlags::new(), tag, ptr, 0);
            }
            fs
        } else {
            let LayoutKind::Product(fs) = &layout.kind else {
                return Err("tuple requires product layout".into());
            };
            fs.as_slice()
        };
        for (n, e) in fields {
            let f = fs
                .iter()
                .find(|f| f.name == n)
                .ok_or("unknown native constructor field")?;
            let value = self.expr(e)?;
            let dest = self.offset(ptr, f.offset);
            self.store(dest, value, &f.ty)?;
        }
        Ok(ptr)
    }
    fn equal(&mut self, a: ir::Value, b: ir::Value, ty: &Type) -> Result<ir::Value, String> {
        if matches!(ty, Type::Function { .. }) {
            return Ok(self.b.ins().iconst(types::I8, 0));
        }
        let layout = Layout::of(self.world, ty)?;
        match layout.kind {
            LayoutKind::Scalar => Ok(if ty.name() == "f64" {
                self.b.ins().fcmp(FloatCC::Equal, a, b)
            } else {
                self.b.ins().icmp(IntCC::Equal, a, b)
            }),
            LayoutKind::Product(fs) => self.equal_fields(a, b, &fs),
            LayoutKind::Enum(vs) => {
                let at = self.b.ins().load(types::I64, ir::MemFlags::new(), a, 0);
                let bt = self.b.ins().load(types::I64, ir::MemFlags::new(), b, 0);
                let tags = self.b.ins().icmp(IntCC::Equal, at, bt);
                let mut result = self.b.ins().iconst(types::I8, 0);
                for (i, v) in vs.iter().enumerate() {
                    let fields = self.equal_fields(a, b, &v.fields)?;
                    let active = self.b.ins().icmp_imm(IntCC::Equal, at, i as i64);
                    let active = self.b.ins().band(active, tags);
                    let eq = self.b.ins().band(active, fields);
                    result = self.b.ins().bor(result, eq);
                }
                Ok(result)
            }
        }
    }
    fn equal_fields(
        &mut self,
        a: ir::Value,
        b: ir::Value,
        fs: &[LayoutField],
    ) -> Result<ir::Value, String> {
        let mut result = self.b.ins().iconst(types::I8, 1);
        for f in fs {
            let ap = self.offset(a, f.offset);
            let bp = self.offset(b, f.offset);
            let av = self.load(ap, &f.ty)?;
            let bv = self.load(bp, &f.ty)?;
            let eq = self.equal(av, bv, &f.ty)?;
            result = self.b.ins().band(result, eq);
        }
        Ok(result)
    }
    fn expr(&mut self, e: &Expr) -> Result<ir::Value, String> {
        let value = match &e.kind {
            ExprKind::Literal(l) => match l {
                Literal::Int(n) => self.b.ins().iconst(types::I64, *n as i64),
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
                    let ptr = self.alloc(layout.size);
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
            ExprKind::Index(base, index) => {
                let layout = Layout::of(self.world, &self.ty(base))?;
                let LayoutKind::Product(fs) = layout.kind else {
                    return Err("native indexing requires tuple".into());
                };
                let ptr = self.expr(base)?;
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
                self.load(address, &f.ty)?
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
            ExprKind::Unary(op, v) => {
                let kind = crate::numeric::IntegerKind::from_type(&self.ty(v));
                let v = self.expr(v)?;
                let float = self.b.func.dfg.value_type(v) == types::F64;
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
                        self.b.ins().icmp_imm(IntCC::Equal, v, i64::MIN)
                    };
                    self.check_overflow(overflow)?;
                }
                match (op.as_str(), float) {
                    ("-", false) => self.b.ins().ineg(v),
                    ("-", true) => self.b.ins().fneg(v),
                    ("!", false) => {
                        let b = self.b.ins().icmp_imm(IntCC::Equal, v, 0);
                        self.b.ins().uextend(types::I64, b)
                    }
                    _ => return Err("invalid native unary op".into()),
                }
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
            ExprKind::Binary(op, a, b) => {
                let ty = self.ty(a);
                let av = self.expr(a)?;
                let bv = self.expr(b)?;
                if matches!(op.as_str(), "==" | "!=")
                    && (Layout::of(self.world, &ty)?.aggregate()
                        || matches!(ty, Type::Function { .. }))
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
                    } else {
                        self.binary(op, av, bv, unsigned)
                    }?
                }
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
                            self.binary(&op[..1], old, value, unsigned)?
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
                            self.binary(&op[..1], old, value, unsigned)?
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
                if let ExprKind::Name(n) = &callee.kind {
                    if let Some(var) = self.vars.get(n) {
                        let tag = self.use_var(*var);
                        return self.invoke_tag(tag, &self.ty(callee), args, e);
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
                self.handlers.push(Handler {
                    identity,
                    capabilities: outer_caps.clone(),
                    output_type: output_type.clone(),
                    effect,
                    state,
                    state_type: state_type.clone(),
                    ops: ops.clone(),
                    captured: capture_variables.clone(),
                    captured_types: capture_types.clone(),
                    generic_vars: self.generic_vars.clone(),
                    control: control.clone(),
                });
                let body_value = self.shared_control("handler_body", body)?;
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
    fn coerce(&mut self, v: ir::Value, t: Option<&Type>) -> ir::Value {
        if let Some(kind) = t.and_then(crate::numeric::IntegerKind::from_type)
            && kind.bits() < 64
            && self.b.func.dfg.value_type(v) == types::I64
        {
            return self.b.ins().band_imm(v, kind.mask() as i64);
        }
        v
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
            ("+" | "*", K::U8 | K::U32) => {
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
            ("-", K::U8 | K::U32 | K::U64) => {
                Some(self.b.ins().icmp(IntCC::UnsignedLessThan, a, b))
            }
            ("+", K::U64) => {
                let r = self.b.ins().iadd(a, b);
                Some(self.b.ins().icmp(IntCC::UnsignedLessThan, r, a))
            }
            ("*", K::U64) => {
                let high = self.b.ins().umulhi(a, b);
                Some(self.b.ins().icmp_imm(IntCC::NotEqual, high, 0))
            }
            ("+" | "-", K::I64) => {
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
            ("*", K::I64) => {
                let r = self.b.ins().imul(a, b);
                let high = self.b.ins().smulhi(a, b);
                let sign = self.b.ins().sshr_imm(r, 63);
                Some(self.b.ins().icmp(IntCC::NotEqual, high, sign))
            }
            ("/", K::I64) => {
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
    fn reraise(&mut self, f: &Function, record: ir::Value) -> Result<(), String> {
        let tag = self
            .b
            .ins()
            .load(types::I64, ir::MemFlags::new(), record, 0);
        for (index, effect) in f.effects.instances.iter().enumerate() {
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
        let float = self.b.func.dfg.value_type(a) == types::F64;
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
                    let mut signature = self.m.make_signature();
                    signature
                        .params
                        .extend([AbiParam::new(types::F64), AbiParam::new(types::F64)]);
                    signature.returns.push(AbiParam::new(types::F64));
                    let id = self
                        .m
                        .declare_function("fmod", Linkage::Import, &signature)
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
        let vars = call_vars(self.world, site, &self.generic_vars);
        let f = concrete(self.world, name, &vars)?;
        let result_layout = Layout::of(self.world, &f.result)?;
        let mut values = vec![];
        let mut restores = vec![];
        for (arg, param) in args.iter().zip(&f.params) {
            let layout = Layout::of(self.world, &param.ty)?;
            if param.pass == Pass::Inout {
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
        let result = if specialized_function(self.world, &f) {
            if self.inline_depth >= 64 {
                return Err("recursive effect specialization exceeds 64 inline frames".into());
            }
            let scope = std::mem::take(&mut self.vars);
            let type_scope = std::mem::take(&mut self.local_types);
            let generic_scope = std::mem::replace(&mut self.generic_vars, vars);
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
            self.capabilities = capability_scope;
            result?
        } else {
            let key = instance_key(name, &vars);
            let id = *self
                .ids
                .get(&key)
                .ok_or_else(|| format!("missing monomorphized function '{key}'"))?;
            let output = if result_layout.aggregate() {
                Some(self.alloc(result_layout.size))
            } else {
                None
            };
            if let Some(out) = output {
                values.insert(0, out);
            }
            let record = if fail_only(self.world, &f) {
                let record = self.alloc(fail_record_size(self.world, &f)?);
                values.push(record);
                Some(record)
            } else {
                None
            };
            let fun = self.m.declare_func_in_func(id, self.b.func);
            let sig = self.b.func.dfg.ext_funcs[fun].signature;
            let params = &self.b.func.dfg.signatures[sig].params;
            if params.len() != values.len()
                || params
                    .iter()
                    .zip(&values)
                    .any(|(p, v)| p.value_type != self.b.func.dfg.value_type(*v))
            {
                return Err(format!("native call ABI mismatch in '{name}'"));
            }
            let call = self.b.ins().call(fun, &values);
            let result = output.unwrap_or_else(|| self.b.inst_results(call)[0]);
            if let Some(record) = record {
                self.reraise(&f, record)?;
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
        self.b.ins().store(ir::MemFlags::new(), tag, record, 0);
        let payload = self.offset(record, 8);
        self.store(payload, values[0], &ty)?;
        let returns = self.b.func.signature.returns[0].value_type;
        let placeholder = if returns == types::F64 {
            self.b.ins().f64const(0.0)
        } else {
            self.b.ins().iconst(returns, 0)
        };
        self.b.ins().return_(&[placeholder]);
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

extern "C" fn native_float_remainder(a: f64, b: f64) -> f64 {
    a % b
}

#[path = "native_batch.rs"]
pub mod batch;
