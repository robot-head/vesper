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
    fn validate(self) -> Result<(), String> {
        if self.max_entries == 0
            || self.max_entries > 128
            || self.max_functions == 0
            || self.max_functions > 512
            || self.max_ast_nodes == 0
            || self.max_ast_nodes > 50_000
            || self.max_ast_depth == 0
            || self.max_ast_depth > 64
            || self.max_ir_instructions == 0
            || self.max_ir_instructions > 100_000
            || self.max_ir_blocks == 0
            || self.max_ir_blocks > 4096
            || self.max_call_depth == 0
            || self.max_call_depth > 64
            || self.max_execution_work == 0
            || self.max_execution_work > 1_000_000
            || self.max_runs == 0
            || self.max_runs > 100_000_000
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
    Signed,
    Unsigned(u32),
    Float,
    Bool,
    Unit,
}
fn result_kind(t: &Type) -> Result<ResultKind, String> {
    if *t == Type::unit() || t.name() == "Unit" {
        return Ok(ResultKind::Unit);
    }
    match t {
        Type::Named(n, args) if args.is_empty() => match n.as_str() {
            "i64" => Ok(ResultKind::Signed),
            "u8" => Ok(ResultKind::Unsigned(8)),
            "u32" => Ok(ResultKind::Unsigned(32)),
            "u64" => Ok(ResultKind::Unsigned(64)),
            "f64" => Ok(ResultKind::Float),
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
        limits.validate()?;
        if entries.is_empty() || entries.len() > limits.max_entries {
            return Err("native batch entry limit".into());
        }
        let unique: BTreeSet<_> = entries.iter().copied().collect();
        if unique.len() != entries.len() {
            return Err("native batch duplicate entry".into());
        }
        preflight_ast(world, limits)?;
        let program = crate::ir::lower_world(world);
        let (reachable, max_work) = preflight_ir(world, &program, entries, limits)?;
        let instances: InstanceMap = reachable
            .iter()
            .map(|name| (name.clone(), (name.clone(), BTreeMap::new())))
            .collect();
        let mut flags = settings::builder();
        flags
            .set("use_colocated_libcalls", "false")
            .map_err(|e| e.to_string())?;
        flags.set("is_pic", "false").map_err(|e| e.to_string())?;
        let isa = cranelift_native::builder()
            .map_err(str::to_string)?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbol("fmod", native_float_remainder as *const u8);
        let mut owned = Self {
            module: Some(JITModule::new(builder)),
            entries: BTreeMap::new(),
            stats: BatchStats {
                module_count: 1,
                compilation_count: 1,
                defined_functions: instances.len(),
                entry_count: entries.len(),
                runs: 0,
                max_entry_work: max_work,
            },
            max_runs: limits.max_runs,
            _single_thread: std::marker::PhantomData,
        };
        let module = owned.module.as_mut().expect("owned JIT module");
        // Union declared and defined ONCE; shared helpers occur only once.
        let ids = declare_instances(module, world, entries[0], &instances)?;
        define_instances(module, world, &instances, &ids, &program)?;
        module.finalize_definitions().map_err(|e| e.to_string())?;
        let mut compiled = BTreeMap::new();
        for name in entries {
            let f = &world.functions[*name];
            let sig = signature(module, world, f)?;
            let result = result_kind(&f.result)?;
            let expected = if matches!(result, ResultKind::Float) {
                types::F64
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
            compiled.insert(
                (*name).to_string(),
                Entry {
                    code: module.get_finalized_function(ids[*name]),
                    result,
                },
            );
        }
        owned.entries = compiled;
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
            } else {
                let call: unsafe extern "C" fn() -> i64 = std::mem::transmute(entry.code);
                let n = call();
                match entry.result {
                    ResultKind::Signed => crate::runtime::Value::Int(n),
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
                    ResultKind::Float => unreachable!(),
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

/// Iterative source-tree guard before the existing recursive lowering helpers.
fn preflight_ast(world: &World, limits: BatchLimits) -> Result<(), String> {
    if world.functions.len() > limits.max_functions
        || world.adts.len() > 256
        || world.effects.len() > 128
        || world.types.len() > limits.max_ast_nodes
        || world.calls.len() > limits.max_ast_nodes
    {
        return Err("native batch world limit".into());
    }
    let mut todo = Vec::new();
    for f in world.functions.values() {
        if f.params.len() > 64 || f.generics.len() > 32 {
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
        if !f.generics.is_empty() || !f.effects.instances.is_empty() || f.effects.tail.is_some() {
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
                    | Op::Try(_) => {
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
