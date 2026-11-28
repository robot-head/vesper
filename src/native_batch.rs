//! One owned JIT module for a conservative finite scalar/direct-call subset.
//! Bounds are structural, not hard timeouts, SimPulse isolation or a sandbox.
use super::*;

#[derive(Clone, Copy, Debug)]
pub struct BatchLimits {
    pub max_entries: usize,
    pub max_functions: usize,
    pub max_ast_nodes: usize,
    pub max_ast_depth: usize,
    pub max_ir_instructions: usize,
    pub max_ir_blocks: usize,
    pub max_call_depth: usize,
    pub max_execution_work: usize,
    pub max_runs: u64,
}
impl Default for BatchLimits {
    fn default() -> Self {
        Self {
            max_entries: 64,
            max_functions: 256,
            max_ast_nodes: 20_000,
            max_ast_depth: 64,
            max_ir_instructions: 50_000,
            max_ir_blocks: 4096,
            max_call_depth: 32,
            max_execution_work: 100_000,
            max_runs: 1_000_000,
        }
    }
}
impl BatchLimits {
    /// The largest limits a caller may configure; `validate` refuses more.
    const CEILING: Self = Self {
        max_entries: 128,
        max_functions: 512,
        max_ast_nodes: 50_000,
        max_ast_depth: 64,
        max_ir_instructions: 100_000,
        max_ir_blocks: 4096,
        max_call_depth: 64,
        max_execution_work: 1_000_000,
        max_runs: 100_000_000,
    };

    fn validate(self) -> Result<(), String> {
        let ceiling = Self::CEILING;
        if self.max_entries == 0
            || self.max_entries > ceiling.max_entries
            || self.max_functions == 0
            || self.max_functions > ceiling.max_functions
            || self.max_ast_nodes == 0
            || self.max_ast_nodes > ceiling.max_ast_nodes
            || self.max_ast_depth == 0
            || self.max_ast_depth > ceiling.max_ast_depth
            || self.max_ir_instructions == 0
            || self.max_ir_instructions > ceiling.max_ir_instructions
            || self.max_ir_blocks == 0
            || self.max_ir_blocks > ceiling.max_ir_blocks
            || self.max_call_depth == 0
            || self.max_call_depth > ceiling.max_call_depth
            || self.max_execution_work == 0
            || self.max_execution_work > ceiling.max_execution_work
            || self.max_runs == 0
            || self.max_runs > ceiling.max_runs
        {
            return Err("native batch limit configuration".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchStats {
    pub module_count: usize,
    pub compilation_count: usize,
    pub defined_functions: usize,
    pub entry_count: usize,
    pub runs: u64,
    pub max_entry_work: usize,
}
#[derive(Clone, Copy)]
enum ResultKind {
    /// A signed integer of the given width, sign-extended in its lane.
    Signed(u32),
    Unsigned(u32),
    Float,
    Float32,
    Bool,
    Unit,
}
fn result_kind(t: &Type) -> Result<ResultKind, String> {
    if *t == Type::unit() || t.name() == "Unit" {
        return Ok(ResultKind::Unit);
    }
    match t {
        Type::Named(n, args) if args.is_empty() => match n.as_str() {
            n if crate::numeric::is_integer_name(n) => {
                let k = crate::numeric::IntegerKind::from_name(n).unwrap();
                Ok(if k.unsigned() {
                    ResultKind::Unsigned(k.bits())
                } else {
                    ResultKind::Signed(k.bits())
                })
            }
            "f64" => Ok(ResultKind::Float),
            "f32" => Ok(ResultKind::Float32),
            "bool" => Ok(ResultKind::Bool),
            _ => Err(format!("native batch unsupported scalar type {t}")),
        },
        _ => Err(format!("native batch unsupported scalar type {t}")),
    }
}
fn scalar_value(t: &Type) -> bool {
    matches!(t, Type::IntLiteral(_)) || result_kind(t).is_ok()
}
struct Entry {
    code: *const u8,
    result: ResultKind,
}
/// Code pointers never escape this owner. Calls borrow the owner exclusively;
/// dropping it frees its single module only after all calls have returned.
/// Optimization is deliberately OFF; existing backend lowering is reused.
pub struct NativeBatch {
    module: Option<JITModule>,
    entries: BTreeMap<String, Entry>,
    stats: BatchStats,
    max_runs: u64,
    _single_thread: std::marker::PhantomData<Rc<()>>,
}
impl NativeBatch {
    pub fn compile(world: &World, entries: &[&str], limits: BatchLimits) -> Result<Self, String> {
        let plan = BatchPlan::new(world, entries, limits)?;
        let mut builder =
            JITBuilder::with_isa(plan.isa.clone(), cranelift_module::default_libcall_names());
        super::register_jit_runtime_symbols(&mut builder, false);
        let mut owned = Self {
            module: Some(JITModule::new(builder)),
            entries: BTreeMap::new(),
            stats: plan.stats(entries),
            max_runs: limits.max_runs,
            _single_thread: std::marker::PhantomData,
        };
        let module = owned.module.as_mut().expect("owned JIT module");
        let ids = plan.define(module, world, entries)?;
        module.finalize_definitions().map_err(|e| e.to_string())?;
        owned.entries = ids
            .into_iter()
            .map(|(name, (id, result))| {
                let code = module.get_finalized_function(id);
                (name, Entry { code, result })
            })
            .collect();
        Ok(owned)
    }
    pub fn stats(&self) -> BatchStats {
        self.stats
    }
    pub fn run(&mut self, entry: &str) -> Result<crate::runtime::Value, String> {
        let entry = self
            .entries
            .get(entry)
            .ok_or("unknown native batch entry")?;
        if self.stats.runs >= self.max_runs {
            return Err("native batch run limit".into());
        }
        self.stats.runs = self
            .stats
            .runs
            .checked_add(1)
            .ok_or("native batch run overflow")?;
        // SAFETY: compile verifies the complete direct-call scalar closure,
        // absence of imports/pointers/hidden effect/sret arguments, host C ABI,
        // no argument entry and exact return kind. The exclusively borrowed
        // owner keeps executable memory alive; pointers cannot escape this API.
        let value = unsafe {
            if matches!(entry.result, ResultKind::Float) {
                let call: unsafe extern "C" fn() -> f64 = std::mem::transmute(entry.code);
                crate::runtime::Value::Float(call())
            } else if matches!(entry.result, ResultKind::Float32) {
                let call: unsafe extern "C" fn() -> f32 = std::mem::transmute(entry.code);
                crate::runtime::Value::Float32(call())
            } else {
                let call: unsafe extern "C" fn() -> i64 = std::mem::transmute(entry.code);
                let n = call();
                match entry.result {
                    ResultKind::Signed(bits) => {
                        if bits < 64 && (n << (64 - bits)) >> (64 - bits) != n {
                            return Err("noncanonical native signed result".into());
                        }
                        crate::runtime::integer_from_word(
                            if bits == 64 {
                                crate::numeric::IntegerKind::I64
                            } else {
                                crate::numeric::IntegerKind::from_name(&format!("i{bits}")).unwrap()
                            },
                            n as u64,
                        )
                    }
                    ResultKind::Unsigned(bits) => {
                        let value = n as u64;
                        if bits < 64 && value >> bits != 0 {
                            return Err("noncanonical native unsigned result".into());
                        }
                        crate::runtime::Value::UInt(value, bits)
                    }
                    ResultKind::Bool => match n {
                        0 => crate::runtime::Value::Bool(false),
                        1 => crate::runtime::Value::Bool(true),
                        _ => return Err("noncanonical native bool result".into()),
                    },
                    ResultKind::Unit => {
                        if n != 0 {
                            return Err("noncanonical native Unit result".into());
                        }
                        crate::runtime::Value::Tuple(vec![])
                    }
                    ResultKind::Float | ResultKind::Float32 => unreachable!(),
                }
            }
        };
        Ok(value)
    }
}
impl Drop for NativeBatch {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            // SAFETY: entry pointers are private, calls require &mut self and
            // return only owned scalar values, and this owner is !Send/!Sync.
            unsafe {
                module.free_memory();
            }
        }
    }
}

/// The checked, preflighted program of a batch, before any code exists:
/// what [`NativeBatch::compile`] and [`compile_code`] share.
struct BatchPlan {
    program: crate::ir::Program,
    instances: InstanceMap,
    max_work: usize,
    isa: std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>,
}
impl BatchPlan {
    fn new(world: &World, entries: &[&str], limits: BatchLimits) -> Result<Self, String> {
        limits.validate()?;
        if entries.is_empty() || entries.len() > limits.max_entries {
            return Err("native batch entry limit".into());
        }
        let unique: BTreeSet<_> = entries.iter().copied().collect();
        if unique.len() != entries.len() {
            return Err("native batch duplicate entry".into());
        }
        preflight_ast(world, limits)?;
        let mut program = crate::ir::lower_world(world);
        let (reachable, max_work) = preflight_ir(world, &program, entries, limits)?;
        crate::ir_opt::fip_reuse_pass(world, &mut program)?;
        crate::fip::elaborate_non_fip_drops(world, &mut program);
        let instances: InstanceMap = reachable
            .iter()
            .map(|name| (name.clone(), (name.clone(), BTreeMap::new())))
            .collect();
        Ok(Self {
            program,
            instances,
            max_work,
            isa: batch_isa(world)?,
        })
    }
    fn stats(&self, entries: &[&str]) -> BatchStats {
        BatchStats {
            module_count: 1,
            compilation_count: 1,
            defined_functions: self.instances.len(),
            entry_count: entries.len(),
            runs: 0,
            max_entry_work: self.max_work,
        }
    }
    /// Declare and define every instance in `module`, then check each
    /// entry's ABI. Returns each entry's function and result kind.
    fn define<M: Module>(
        &self,
        module: &mut M,
        world: &World,
        entries: &[&str],
    ) -> Result<BTreeMap<String, (FuncId, ResultKind)>, String> {
        // Union declared and defined ONCE; shared helpers occur only once.
        let ids = declare_instances(
            module,
            world,
            entries[0],
            &self.instances,
            DefinitionLinkage::Jit,
        )?;
        define_instances(
            module,
            world,
            &self.instances,
            &ids,
            &self.program,
            DefinitionLinkage::Jit,
            false,
        )?;
        let mut compiled = BTreeMap::new();
        for name in entries {
            let f = &world.functions[*name];
            let sig = signature(module, world, f)?;
            let result = result_kind(&f.result)?;
            let expected = if matches!(result, ResultKind::Float) {
                types::F64
            } else if matches!(result, ResultKind::Float32) {
                types::F32
            } else {
                types::I64
            };
            if !sig.params.is_empty()
                || sig.returns.len() != 1
                || sig.returns[0].value_type != expected
                || sig.call_conv != module.target_config().default_call_conv
            {
                return Err("native batch ABI mismatch".into());
            }
            compiled.insert((*name).to_string(), (ids[*name], result));
        }
        Ok(compiled)
    }
}

/// The batch JIT's ISA: non-PIC, no colocated libcalls, NaN
/// canonicalization for DetFloat programs, host target. `vesper bench
/// --asm` lists machine code with the same flags.
pub fn batch_isa(
    world: &World,
) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>, String> {
    let mut flags = settings::builder();
    flags
        .set("use_colocated_libcalls", "false")
        .map_err(|e| e.to_string())?;
    flags.set("is_pic", "false").map_err(|e| e.to_string())?;
    // DetFloat programs promise bit-identical floats across x86-64 and
    // AArch64, as in object emission and the JIT (lane 23, #346).
    if super::needs_nan_canonicalization(world) {
        flags
            .set("enable_nan_canonicalization", "true")
            .map_err(|e| e.to_string())?;
    }
    cranelift_native::builder()
        .map_err(str::to_string)?
        .finish(settings::Flags::new(flags))
        .map_err(|e| e.to_string())
}

/// One function of a batch, compiled to host machine code but not loaded:
/// its bytes, its relocations and how it is declared.
pub struct BatchFunction {
    /// The module's function id (`u0:<index>` in its relocations).
    pub func_id: u32,
    pub alignment: u64,
    pub code: Vec<u8>,
    pub relocs: Vec<cranelift_module::ModuleReloc>,
}

/// A batch compiled to machine code for a loader in another process
/// (`vesper bench`, #2670): the module's declarations in id order, every
/// defined function and the entries. Loading the declarations in the same
/// order into a fresh module gives every function the same id, so the
/// relocations apply unchanged.
pub struct BatchCode {
    /// `(symbol, linkage)` of every declared function, by id.
    pub declarations: Vec<(String, Linkage)>,
    pub functions: Vec<BatchFunction>,
    /// Each entry's function id and result type.
    pub entries: Vec<(String, u32, BatchResult)>,
    pub stats: BatchStats,
}

/// The result an entry returns, as its loader reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchResult {
    /// A signed integer of the given width, sign-extended in its 64-bit lane.
    Signed(u32),
    /// An unsigned integer of the given width, zero-extended.
    Unsigned(u32),
    F64,
    F32,
    Bool,
    Unit,
}
impl From<ResultKind> for BatchResult {
    fn from(kind: ResultKind) -> Self {
        match kind {
            ResultKind::Signed(bits) => Self::Signed(bits),
            ResultKind::Unsigned(bits) => Self::Unsigned(bits),
            ResultKind::Float => Self::F64,
            ResultKind::Float32 => Self::F32,
            ResultKind::Bool => Self::Bool,
            ResultKind::Unit => Self::Unit,
        }
    }
}

/// [`NativeBatch::compile`]'s program, compiled with the same ISA and the
/// same lowering, but handed back as code and relocations instead of being
/// loaded into this process.
pub fn compile_code(
    world: &World,
    entries: &[&str],
    limits: BatchLimits,
) -> Result<BatchCode, String> {
    let plan = BatchPlan::new(world, entries, limits)?;
    let builder = JITBuilder::with_isa(plan.isa.clone(), cranelift_module::default_libcall_names());
    let mut module = CaptureModule {
        inner: JITModule::new(builder),
        functions: Vec::new(),
    };
    let ids = plan.define(&mut module, world, entries)?;
    let declarations = module
        .inner
        .declarations()
        .get_functions()
        .map(|(id, decl)| (decl.linkage_name(id).into_owned(), decl.linkage))
        .collect();
    let entries_code = entries
        .iter()
        .map(|name| {
            let (id, result) = ids[*name];
            ((*name).to_string(), id.as_u32(), BatchResult::from(result))
        })
        .collect();
    let stats = plan.stats(entries);
    // The JIT module only held declarations: nothing was defined in it.
    // SAFETY: no code of this module was ever made executable or handed out.
    unsafe { module.inner.free_memory() };
    Ok(BatchCode {
        declarations,
        functions: module.functions,
        entries: entries_code,
        stats,
    })
}

/// A module that declares through a JIT module but compiles each defined
/// function to bytes and relocations, as the JIT would, without loading it.
struct CaptureModule {
    inner: JITModule,
    functions: Vec<BatchFunction>,
}
impl Module for CaptureModule {
    fn isa(&self) -> &dyn cranelift_codegen::isa::TargetIsa {
        self.inner.isa()
    }
    fn declarations(&self) -> &cranelift_module::ModuleDeclarations {
        self.inner.declarations()
    }
    fn declare_function(
        &mut self,
        name: &str,
        linkage: Linkage,
        signature: &ir::Signature,
    ) -> cranelift_module::ModuleResult<FuncId> {
        self.inner.declare_function(name, linkage, signature)
    }
    fn declare_anonymous_function(
        &mut self,
        signature: &ir::Signature,
    ) -> cranelift_module::ModuleResult<FuncId> {
        self.inner.declare_anonymous_function(signature)
    }
    fn declare_data(
        &mut self,
        name: &str,
        linkage: Linkage,
        writable: bool,
        tls: bool,
    ) -> cranelift_module::ModuleResult<cranelift_module::DataId> {
        self.inner.declare_data(name, linkage, writable, tls)
    }
    fn declare_anonymous_data(
        &mut self,
        writable: bool,
        tls: bool,
    ) -> cranelift_module::ModuleResult<cranelift_module::DataId> {
        self.inner.declare_anonymous_data(writable, tls)
    }
    fn define_function_with_control_plane(
        &mut self,
        func: FuncId,
        ctx: &mut cranelift_codegen::Context,
        ctrl_plane: &mut cranelift_codegen::control::ControlPlane,
    ) -> cranelift_module::ModuleResult<()> {
        // What `JITModule::define_function_with_control_plane` compiles.
        ctx.compile(self.inner.isa(), ctrl_plane)
            .map_err(|e| cranelift_module::ModuleError::Compilation(e.inner))?;
        let compiled = ctx.compiled_code().expect("compiled just now");
        let relocs = compiled
            .buffer
            .relocs()
            .iter()
            .map(|reloc| cranelift_module::ModuleReloc::from_mach_reloc(reloc, &ctx.func, func))
            .collect();
        self.functions.push(BatchFunction {
            func_id: func.as_u32(),
            alignment: u64::from(compiled.buffer.alignment),
            code: compiled.code_buffer().to_vec(),
            relocs,
        });
        Ok(())
    }
    fn define_function_bytes(
        &mut self,
        func_id: FuncId,
        alignment: u64,
        bytes: &[u8],
        relocs: &[cranelift_module::ModuleReloc],
    ) -> cranelift_module::ModuleResult<()> {
        self.inner
            .define_function_bytes(func_id, alignment, bytes, relocs)
    }
    fn define_data(
        &mut self,
        data_id: cranelift_module::DataId,
        data: &cranelift_module::DataDescription,
    ) -> cranelift_module::ModuleResult<()> {
        self.inner.define_data(data_id, data)
    }
}

/// The most ADTs a batch world may declare.
const MAX_WORLD_ADTS: usize = 256;
/// The most effects a batch world may declare.
const MAX_WORLD_EFFECTS: usize = 128;
/// The most parameters one batch function may take.
const MAX_FUNCTION_PARAMS: usize = 64;
/// The most generic parameters one batch function may take.
const MAX_FUNCTION_GENERICS: usize = 32;

/// Iterative source-tree guard before the existing recursive lowering helpers.
fn preflight_ast(world: &World, limits: BatchLimits) -> Result<(), String> {
    if world.functions.len() > limits.max_functions
        || world.adts.len() > MAX_WORLD_ADTS
        || world.effects.len() > MAX_WORLD_EFFECTS
        || world.types.len() > limits.max_ast_nodes
        || world.calls.len() > limits.max_ast_nodes
    {
        return Err("native batch world limit".into());
    }
    let mut todo = Vec::new();
    for f in world.functions.values() {
        if f.params.len() > MAX_FUNCTION_PARAMS || f.generics.len() > MAX_FUNCTION_GENERICS {
            return Err("native batch signature limit".into());
        }
        if let Some(body) = &f.body {
            todo.push((body, 1usize));
        }
    }
    let mut count = 0usize;
    while let Some((expr, depth)) = todo.pop() {
        count = count.checked_add(1).ok_or("native batch AST overflow")?;
        if count > limits.max_ast_nodes || depth > limits.max_ast_depth {
            return Err("native batch AST limit".into());
        }
        let child_depth = depth
            .checked_add(1)
            .ok_or("native batch AST depth overflow")?;
        let mut add = |e| todo.push((e, child_depth));
        match &expr.kind {
            ExprKind::Tuple(es) => es.iter().for_each(&mut add),
            ExprKind::Struct { fields, .. } => fields.iter().for_each(|(_, e)| add(e)),
            ExprKind::Block { statements, tail } => {
                for s in statements {
                    add(match s {
                        Statement::Let { value, .. } => value,
                        Statement::Expr(e) => e,
                    });
                }
                if let Some(e) = tail {
                    add(e);
                }
            }
            ExprKind::Call { callee, args, .. } => {
                add(callee);
                for a in args {
                    add(&a.expr);
                }
            }
            ExprKind::Unary(_, e) | ExprKind::Field(e, _) | ExprKind::Try(e) => add(e),
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Assign(_, a, b) => {
                add(a);
                add(b);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                add(condition);
                add(then);
                if let Some(e) = otherwise {
                    add(e);
                }
            }
            ExprKind::Match { value, arms } => {
                add(value);
                for (_, e) in arms {
                    add(e);
                }
            }
            ExprKind::Closure { body, .. } => add(body),
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                if let Some((_, e)) = state {
                    add(e);
                }
                add(body);
                add(&ret.body);
                for c in ops {
                    add(&c.body);
                }
            }
            k @ (ExprKind::Loop(_)
            | ExprKind::For { .. }
            | ExprKind::Range { .. }
            | ExprKind::Break(_)
            | ExprKind::Continue
            | ExprKind::Return(_)) => {
                for c in k.control_children().into_iter().flatten() {
                    add(c);
                }
            }
            _ => {}
        }
    }
    Ok(())
}
fn preflight_ir(
    world: &World,
    program: &crate::ir::Program,
    entries: &[&str],
    limits: BatchLimits,
) -> Result<(BTreeSet<String>, usize), String> {
    use crate::ir::{Op, Terminator};
    let mut total_instructions = 0usize;
    let mut total_blocks = 0usize;
    for f in program.functions.values() {
        total_blocks = total_blocks
            .checked_add(f.blocks.len())
            .ok_or("IR block overflow")?;
        for b in &f.blocks {
            total_instructions = total_instructions
                .checked_add(b.instructions.len())
                .ok_or("IR instruction overflow")?;
        }
    }
    if total_instructions > limits.max_ir_instructions || total_blocks > limits.max_ir_blocks {
        return Err("native batch IR volume limit".into());
    }
    let mut todo: Vec<String> = entries.iter().map(|e| (*e).to_string()).collect();
    let mut reachable = BTreeSet::new();
    let mut calls = BTreeMap::new();
    let mut own_cost = BTreeMap::new();
    while let Some(name) = todo.pop() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        let f = world
            .functions
            .get(&name)
            .ok_or_else(|| format!("unknown native batch function {name}"))?;
        if entries.contains(&name.as_str()) && !f.params.is_empty() {
            return Err("native batch entry must take no arguments".into());
        }
        if f.body.is_none() {
            return Err(format!(
                "native batch rejects ambient/foreign import {name}"
            ));
        }
        // Capability effects (ConstTime, Realtime, DetFloat) have no
        // operations; such functions compile like pure ones (lane 23).
        if !f.generics.is_empty()
            || !f.effects.instances.iter().all(|e| {
                crate::capability_effects::is_capability_effect(&e.name) && e.args.is_empty()
            })
            || f.effects.tail.is_some()
        {
            return Err(format!(
                "native batch requires pure ordinary monomorphic function {name}"
            ));
        }
        result_kind(&f.result)?;
        for p in &f.params {
            result_kind(&p.ty)?;
            if p.pass == Pass::Inout {
                return Err("native batch inout parameters unsupported".into());
            }
        }
        let ir = program
            .functions
            .get(&name)
            .ok_or("missing native batch IR")?;
        if ir
            .values
            .values()
            .chain(ir.locals.values())
            .any(|t| !scalar_value(t))
        {
            return Err("native batch aggregate/function values unsupported".into());
        }
        let mut edges = vec![vec![]; ir.blocks.len()];
        let mut local_calls = Vec::new();
        let mut cost = ir.blocks.len();
        for (index, block) in ir.blocks.iter().enumerate() {
            cost = cost
                .checked_add(block.instructions.len())
                .ok_or("native batch work overflow")?;
            for inst in &block.instructions {
                if inst.fallible && !matches!(inst.op, Op::Call { .. }) {
                    return Err("native batch fallible primitive unsupported".into());
                }
                match &inst.op {
                    Op::Call {
                        callee,
                        args,
                        capability,
                        types,
                    } => {
                        if capability.is_some()
                            || !types.is_empty()
                            || args.iter().any(|a| a.pass == Pass::Inout)
                        {
                            return Err("native batch indirect/effect call unsupported".into());
                        }
                        if !world.functions.contains_key(callee) {
                            return Err(format!("native batch unresolved/foreign call {callee}"));
                        }
                        local_calls.push(callee.clone());
                        todo.push(callee.clone());
                    }
                    Op::Constant(Literal::String(_))
                    | Op::Indirect { .. }
                    | Op::Closure { .. }
                    | Op::Handle { .. }
                    | Op::Data(_, _)
                    | Op::Project(_, _)
                    | Op::Index(_, _)
                    | Op::Try(_)
                    | Op::Vector { .. }
                    | Op::Tiled { .. }
                    | Op::Seq { .. }
                    | Op::Fip { .. } => {
                        return Err(
                            "native batch unsupported aggregate/indirect/handler operation".into(),
                        );
                    }
                    Op::Read {
                        path,
                        projections,
                        root,
                        ..
                    } if !path.is_empty()
                        || !projections.is_empty()
                        || world.functions.contains_key(root) =>
                    {
                        return Err("native batch projected/function read unsupported".into());
                    }
                    Op::Tuple(es) if !es.is_empty() => {
                        return Err("native batch aggregate tuple unsupported".into());
                    }
                    _ => {}
                }
            }
            match &block.terminator {
                Terminator::Jump { block, .. } => edges[index].push(*block),
                Terminator::Branch { yes, no, .. } => {
                    edges[index].push(*yes);
                    edges[index].push(*no);
                }
                Terminator::Match { arms, .. } => edges[index].extend(arms.iter().map(|(_, i)| *i)),
                _ => {}
            }
        }
        acyclic(&edges)?;
        own_cost.insert(name.clone(), cost);
        calls.insert(name, local_calls);
    }
    let mut costs = BTreeMap::<String, (usize, usize)>::new();
    while costs.len() < reachable.len() {
        let before = costs.len();
        for name in &reachable {
            if costs.contains_key(name) || calls[name].iter().any(|c| !costs.contains_key(c)) {
                continue;
            }
            let (mut cost, mut depth) = (own_cost[name], 1usize);
            for call in &calls[name] {
                cost = cost
                    .checked_add(costs[call].0)
                    .ok_or("native batch expanded-work overflow")?;
                depth = depth.max(
                    costs[call]
                        .1
                        .checked_add(1)
                        .ok_or("native batch call-depth overflow")?,
                );
            }
            if cost > limits.max_execution_work || depth > limits.max_call_depth {
                return Err("native batch expanded-work/call-depth limit".into());
            }
            costs.insert(name.clone(), (cost, depth));
        }
        if costs.len() == before {
            return Err("native batch recursion/cyclic call graph unsupported".into());
        }
    }
    let maximum = entries.iter().map(|e| costs[*e].0).max().unwrap_or(0);
    Ok((reachable, maximum))
}
fn acyclic(edges: &[Vec<usize>]) -> Result<(), String> {
    let mut indegree = vec![0usize; edges.len()];
    for successors in edges {
        for &target in successors {
            let degree = indegree
                .get_mut(target)
                .ok_or("native batch invalid CFG target")?;
            *degree = degree.checked_add(1).ok_or("CFG degree overflow")?;
        }
    }
    let mut todo: Vec<_> = indegree
        .iter()
        .enumerate()
        .filter_map(|(i, &d)| (d == 0).then_some(i))
        .collect();
    let mut visited = 0;
    while let Some(i) = todo.pop() {
        visited += 1;
        for &target in &edges[i] {
            indegree[target] -= 1;
            if indegree[target] == 0 {
                todo.push(target);
            }
        }
    }
    if visited != edges.len() {
        return Err("native batch cyclic CFG unsupported".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn cyclic_and_invalid_cfgs_are_rejected() {
        assert!(super::acyclic(&[vec![0]]).is_err());
        assert!(super::acyclic(&[vec![1], vec![0]]).is_err());
        assert!(super::acyclic(&[vec![2], vec![]]).is_err());
        assert!(super::acyclic(&[vec![1, 2], vec![2], vec![]]).is_ok());
    }
}
