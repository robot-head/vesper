//! Explicit, stack-safe CEK evaluator. Effect continuations own their suspended
//! frame segment and can be resumed once. No polling or Rust stack capture.
use crate::{ast::*, check::World};
use std::{cell::RefCell, collections::BTreeMap, fmt, rc::Rc};

pub mod resource_trace;
#[cfg(test)]
mod resource_trace_acceptance;
use resource_trace::{
    ResourceConsume, ResourceIdentity, ResourceTraceCompletion, ResourceTraceConfig,
    ResourceTraceEngine, ResourceTraceError, ResourceTraceFailure, ResourceTraceObserver,
};

type Env = BTreeMap<String, Slot>;
#[derive(Clone)]
struct Slot {
    cell: Rc<RefCell<Value>>,
    path: Vec<String>,
}
impl Slot {
    fn new(v: Value) -> Self {
        Self {
            cell: Rc::new(RefCell::new(v)),
            path: vec![],
        }
    }
    fn get(&self) -> Result<Value, String> {
        let cell = self
            .cell
            .try_borrow()
            .map_err(|_| "runtime value is mutably borrowed")?;
        let mut v = &*cell;
        for f in &self.path {
            v = match v {
                Value::Data { fields, .. } => fields
                    .get(f)
                    .ok_or_else(|| format!("unknown field '{f}'"))?,
                Value::Tuple(v) => v
                    .get(f.parse::<usize>().map_err(|_| "invalid tuple index")?)
                    .ok_or("tuple index out of bounds")?,
                _ => return Err("field access requires aggregate".into()),
            };
        }
        Ok(v.clone())
    }
    fn set(&self, value: Value) -> Result<(), String> {
        let mut cell = self
            .cell
            .try_borrow_mut()
            .map_err(|_| "runtime value is already borrowed")?;
        let mut v = &mut *cell;
        for f in &self.path {
            v = match v {
                Value::Data { fields, .. } => fields.get_mut(f).ok_or("unknown field")?,
                Value::Tuple(v) => v
                    .get_mut(f.parse::<usize>().map_err(|_| "invalid index")?)
                    .ok_or("index out of bounds")?,
                _ => return Err("assignment requires aggregate".into()),
            };
        }
        *v = value;
        Ok(())
    }
}
#[derive(Clone)]
pub enum Value {
    Int(i64),
    UInt(u64, u32),
    Float(f64),
    Bool(bool),
    String(String),
    Tuple(Vec<Value>),
    Data {
        ty: String,
        constructor: String,
        fields: BTreeMap<String, Value>,
        resource_identity: Option<ResourceIdentity>,
    },
    Function(String),
    Constructor(String),
    Capability(u64),
    EffectOp {
        target: Option<u64>,
        effect: String,
        label: Option<String>,
        op: String,
    },
    Closure(Rc<Closure>),
    Bound(Rc<BoundCallable>),
    Continuation(Rc<RefCell<Option<Continuation>>>),
}
#[derive(Clone)]
pub struct Closure {
    params: Vec<Param>,
    body: Expr,
    ir_body: Option<String>,
    env: Env,
}
#[derive(Clone)]
pub struct BoundCallable {
    function: Value,
    evidence: Env,
}
#[derive(Clone)]
pub struct Continuation {
    frames: Vec<Frame>,
    handler: Handler,
}
impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(v) => write!(f, "{v}"),
            Self::UInt(v, _) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::String(v) => write!(f, "{v:?}"),
            Self::Tuple(v) => write!(
                f,
                "({})",
                v.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Data {
                constructor,
                fields,
                ..
            } => {
                if fields.is_empty() {
                    write!(f, "{constructor}")
                } else if fields.keys().all(|k| k.parse::<usize>().is_ok()) {
                    let mut pairs = fields.iter().collect::<Vec<_>>();
                    pairs.sort_by_key(|(k, _)| k.parse::<usize>().unwrap());
                    write!(
                        f,
                        "{constructor}({})",
                        pairs
                            .iter()
                            .map(|(_, v)| v.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    write!(
                        f,
                        "{constructor} {{ {} }}",
                        fields
                            .iter()
                            .map(|(k, v)| format!("{k}: {v}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Self::Bound(b) => write!(f, "{}", b.function),
            Self::Capability(_) => write!(f, "<lexical capability>"),
            Self::Function(n) => write!(f, "<fn {n}>"),
            Self::Constructor(n) => write!(f, "<constructor {n}>"),
            Self::EffectOp { effect, op, .. } => write!(f, "<{effect}::{op}>"),
            Self::Closure(_) => write!(f, "<closure>"),
            Self::Continuation(_) => write!(f, "<one-shot continuation>"),
        }
    }
}
#[derive(Clone)]
struct Handler {
    id: u64,
    effect: Effect,
    state: Option<Value>,
    ret: Clause,
    ops: Vec<Clause>,
    env: Env,
}
#[derive(Clone)]
enum Frame {
    IrResume {
        state: Box<IrState>,
        dest: crate::ir::ValueId,
        ty: Type,
    },
    Block {
        rest: Vec<Statement>,
        tail: Option<Box<Expr>>,
        env: Env,
        bind: Option<Pattern>,
    },
    Aggregate {
        name: Option<String>,
        done: Vec<(String, Value)>,
        rest: Vec<(String, Expr)>,
        field: String,
        env: Env,
        site: Span,
    },
    Coerce(Type, Env),
    /// The operator, whether it flushes subnormals (ConstDenorm code), and
    /// the evidence environment.
    Unary(String, bool, Env),
    BinaryLeft {
        op: String,
        flush: bool,
        right: Expr,
        env: Env,
    },
    BinaryRight {
        op: String,
        flush: bool,
        left: Value,
        evidence: Env,
    },
    Assign {
        slot: Slot,
        op: String,
        flush: bool,
        evidence: Env,
    },
    Field(String),
    IndexBase {
        index: Expr,
        env: Env,
    },
    IndexValue(Value),
    If {
        then: Expr,
        otherwise: Option<Expr>,
        env: Env,
    },
    Match {
        arms: Vec<(Pattern, Expr)>,
        env: Env,
    },
    Callee {
        args: Vec<Arg>,
        env: Env,
    },
    Arguments {
        callee: Value,
        done: Vec<Value>,
        refs: Vec<Option<Slot>>,
        rest: Vec<Arg>,
        env: Env,
        current: Option<Slot>,
    },
    Handler(Handler),
    HandleState {
        static_label: String,
        effect: Effect,
        body: Expr,
        ret: Clause,
        ops: Vec<Clause>,
        env: Env,
    },
    Try(Option<u64>),
}
#[derive(Clone)]
struct IrState {
    function: String,
    block: usize,
    ip: usize,
    values: BTreeMap<crate::ir::ValueId, Value>,
    env: Env,
}
enum Control {
    Ir(Box<IrState>),
    Eval(Expr, Env),
    Return(Value),
    External(ExternalRequest),
}
pub struct ExternalRequest {
    pub effect: Effect,
    pub operation: String,
    pub arguments: Vec<Value>,
    references: Vec<Option<Slot>>,
}
enum Outcome {
    Complete(Value),
    Suspended(ExternalRequest),
}
pub enum Execution<'a> {
    Complete(Value),
    Suspended(Suspension<'a>),
}
/// The whole machine moves into this non-Clone token. Resuming consumes it.
pub struct Suspension<'a> {
    machine: Runtime<'a>,
    request: ExternalRequest,
}
impl<'a> Suspension<'a> {
    pub fn request(&self) -> &ExternalRequest {
        &self.request
    }
    pub fn resume(self, value: Value) -> Result<Execution<'a>, String> {
        let declaration = &self.machine.world.effects[&self.request.effect.name];
        let operation = declaration
            .ops
            .iter()
            .find(|f| f.name == self.request.operation)
            .ok_or("unknown external operation")?;
        let substitutions = declaration
            .generics
            .iter()
            .zip(&self.request.effect.args)
            .map(|(g, t)| (g.name.clone(), t.clone()))
            .collect();
        let value = coerce_value(value, &substitute(&operation.result, &substitutions));
        self.machine.continue_external(Control::Return(value))
    }
    /// The host implementation may update an inout argument before resuming.
    /// Host implementations must uphold the operation's declared value types.
    pub fn write_argument(&mut self, index: usize, value: Value) -> Result<(), String> {
        let operation = self.machine.world.effects[&self.request.effect.name]
            .ops
            .iter()
            .find(|f| f.name == self.request.operation)
            .ok_or("unknown external operation")?;
        if operation
            .params
            .get(index)
            .is_none_or(|p| p.pass != Pass::Inout)
        {
            return Err("external argument is not mutable".into());
        }
        self.request
            .references
            .get(index)
            .and_then(Option::as_ref)
            .ok_or("external mutable argument lacks a place")?
            .set(value)
    }
}
pub struct Runtime<'a> {
    world: &'a World,
    stack: Vec<Frame>,
    program: crate::ir::Program,
    next_capability: u64,
    pub fuel: u64,
    pub optimize: bool,
    optimized: bool,
    external: BTreeMap<u64, Effect>,
    /// The checked call site of the function being invoked, used to
    /// instantiate generic effect arguments for typed instance selection.
    call_site: Option<(usize, usize)>,
    resource_trace: Option<Box<ResourceTraceEngine>>,
    resource_fuel_exhausted: bool,
    /// Spans of `eff [ConstDenorm]` code, whose float operations flush
    /// subnormal operands and results to zero.
    denorm: Vec<Span>,
}
impl<'a> Runtime<'a> {
    pub fn new(world: &'a World) -> Self {
        Self {
            world,
            stack: vec![],
            program: crate::ir::lower_world(world),
            next_capability: 1,
            fuel: 10_000_000,
            optimize: true,
            optimized: false,
            external: BTreeMap::new(),
            call_site: None,
            resource_trace: None,
            resource_fuel_exhausted: false,
            denorm: crate::capability_effects::const_denorm_spans(world),
        }
    }
    /// Observe selected empty linear data carriers without changing evaluation
    /// or optimizer selection. Every public run starts a new observation epoch.
    pub fn observe_resources(
        &mut self,
        config: ResourceTraceConfig,
    ) -> Result<ResourceTraceObserver, ResourceTraceError> {
        if self.resource_trace.is_some() {
            return Err(ResourceTraceError::new(
                ResourceTraceFailure::Reconfigured,
                None,
            ));
        }
        config.validate_world(self.world)?;
        let engine = ResourceTraceEngine::new(config)?;
        let observer = engine.observer();
        self.resource_trace = Some(Box::new(engine));
        Ok(observer)
    }
    fn construct_data(
        &mut self,
        ty: String,
        constructor: String,
        fields: BTreeMap<String, Value>,
        site: Option<Span>,
    ) -> Result<Value, String> {
        let resource_identity = if let Some(trace) = &mut self.resource_trace {
            if trace.is_selected(&ty) {
                let site = site.ok_or_else(|| {
                    trace
                        .fail(ResourceTraceFailure::MissingSite, None)
                        .to_string()
                })?;
                trace
                    .mint(&ty, &constructor, fields.len(), site)
                    .map_err(|e| e.to_string())?
            } else {
                None
            }
        } else {
            None
        };
        Ok(Value::Data {
            ty,
            constructor,
            fields,
            resource_identity,
        })
    }
    fn bind_owned_pattern(
        &mut self,
        pattern: &Pattern,
        value: &Value,
        env: &mut Env,
    ) -> Result<bool, String> {
        // Existing matching may tentatively bind a prefix. Stage those bindings
        // locally: no terminal event or caller binding commits on a failed match.
        let mut bindings = Env::new();
        if !bind_pattern(pattern, value, &mut bindings) {
            return Ok(false);
        }
        self.consume_owned_patterns(&[(pattern, value)])?;
        env.extend(bindings);
        Ok(true)
    }
    fn consume_owned_patterns(&mut self, roots: &[(&Pattern, &Value)]) -> Result<(), String> {
        if let Some(trace) = &mut self.resource_trace {
            let limits = trace.limits();
            let mut pending = Vec::new();
            if !roots.is_empty() && limits.max_value_depth == 0 {
                return Err(trace
                    .fail(ResourceTraceFailure::ValueDepthLimit, None)
                    .to_string());
            }
            for &(pattern, value) in roots.iter().rev() {
                trace_push(
                    &mut pending,
                    (pattern, value, 1usize),
                    limits.max_value_nodes,
                )
                .map_err(|reason| trace.fail(reason, Some(pattern.span.clone())).to_string())?;
            }
            let mut consumes = Vec::new();
            let mut nodes = 0usize;
            while let Some((pattern, value, depth)) = pending.pop() {
                nodes = nodes.checked_add(1).ok_or_else(|| {
                    trace
                        .fail(
                            ResourceTraceFailure::ValueNodeLimit,
                            Some(pattern.span.clone()),
                        )
                        .to_string()
                })?;
                if nodes > limits.max_value_nodes {
                    return Err(trace
                        .fail(
                            ResourceTraceFailure::ValueNodeLimit,
                            Some(pattern.span.clone()),
                        )
                        .to_string());
                }
                if depth > limits.max_value_depth {
                    return Err(trace
                        .fail(
                            ResourceTraceFailure::ValueDepthLimit,
                            Some(pattern.span.clone()),
                        )
                        .to_string());
                }
                match (&pattern.kind, value) {
                    (PatternKind::Tuple(patterns), Value::Tuple(values)) => {
                        let next_depth = trace_child_depth(
                            trace,
                            nodes,
                            pending.len(),
                            patterns.len(),
                            depth,
                            Some(&pattern.span),
                        )?;
                        for (p, v) in patterns.iter().zip(values).rev() {
                            trace_push(&mut pending, (p, v, next_depth), limits.max_value_nodes)
                                .map_err(|reason| {
                                    trace.fail(reason, Some(p.span.clone())).to_string()
                                })?;
                        }
                    }
                    (
                        PatternKind::Constructor {
                            name,
                            fields: patterns,
                            ..
                        },
                        Value::Data {
                            ty,
                            constructor,
                            fields,
                            resource_identity,
                        },
                    ) => {
                        let expected = self
                            .world
                            .constructor(name)
                            .map(|(adt, _)| adt.name.as_str());
                        if (trace.is_selected(ty)
                            || expected.is_some_and(|ty| trace.is_selected(ty)))
                            && expected != Some(ty.as_str())
                        {
                            return Err(trace
                                .fail(ResourceTraceFailure::WrongKind, Some(pattern.span.clone()))
                                .to_string());
                        }
                        if trace.is_selected(ty) {
                            if consumes.len() >= limits.max_events {
                                return Err(trace
                                    .fail(
                                        ResourceTraceFailure::EventLimit,
                                        Some(pattern.span.clone()),
                                    )
                                    .to_string());
                            }
                            trace_push(
                                &mut consumes,
                                ResourceConsume {
                                    ty,
                                    constructor,
                                    field_count: fields.len(),
                                    identity: *resource_identity,
                                    site: pattern.span.clone(),
                                },
                                limits.max_events,
                            )
                            .map_err(|reason| {
                                trace.fail(reason, Some(pattern.span.clone())).to_string()
                            })?;
                        } else {
                            let next_depth = trace_child_depth(
                                trace,
                                nodes,
                                pending.len(),
                                patterns.len(),
                                depth,
                                Some(&pattern.span),
                            )?;
                            for (name, p) in patterns.iter().rev() {
                                let v = fields.get(name).ok_or_else(|| {
                                    trace
                                        .fail(
                                            ResourceTraceFailure::MalformedCarrier,
                                            Some(p.span.clone()),
                                        )
                                        .to_string()
                                })?;
                                trace_push(
                                    &mut pending,
                                    (p, v, next_depth),
                                    limits.max_value_nodes,
                                )
                                .map_err(|reason| {
                                    trace.fail(reason, Some(p.span.clone())).to_string()
                                })?;
                            }
                        }
                    }
                    _ => {}
                }
            }
            trace.consume_batch(&consumes).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    // A deliberately closed public boundary profile. Borrow actual checked
    // field types and actual values; no substitution, type cloning, or carrier
    // coercion can repair malformed host inputs or outputs.
    fn validate_resource_boundary(
        &mut self,
        entry: &str,
        values: &[Value],
        result: bool,
    ) -> Result<(), String> {
        let Some(trace) = &mut self.resource_trace else {
            return Ok(());
        };
        let function = self.world.functions.get(entry).ok_or_else(|| {
            trace
                .fail(ResourceTraceFailure::UnsupportedValue, None)
                .to_string()
        })?;
        if !function.generics.is_empty()
            || values.len() != if result { 1 } else { function.params.len() }
        {
            return Err(trace
                .fail(ResourceTraceFailure::UnsupportedValue, None)
                .to_string());
        }
        let limits = trace.limits();
        let mut pending = Vec::new();
        if !values.is_empty() && limits.max_value_depth == 0 {
            return Err(trace
                .fail(ResourceTraceFailure::ValueDepthLimit, None)
                .to_string());
        }
        if result {
            let ty = self
                .world
                .function_results
                .get(entry)
                .unwrap_or(&function.result);
            trace_push(
                &mut pending,
                (ty, &values[0], 1usize),
                limits.max_value_nodes,
            )
            .map_err(|reason| trace.fail(reason, None).to_string())?;
        } else {
            for (param, value) in function.params.iter().zip(values).rev() {
                trace_push(
                    &mut pending,
                    (&param.ty, value, 1usize),
                    limits.max_value_nodes,
                )
                .map_err(|reason| trace.fail(reason, None).to_string())?;
            }
        }
        let mut nodes = 0usize;
        while let Some((ty, value, depth)) = pending.pop() {
            nodes = nodes.checked_add(1).ok_or_else(|| {
                trace
                    .fail(ResourceTraceFailure::ValueNodeLimit, None)
                    .to_string()
            })?;
            if nodes > limits.max_value_nodes {
                return Err(trace
                    .fail(ResourceTraceFailure::ValueNodeLimit, None)
                    .to_string());
            }
            if depth > limits.max_value_depth {
                return Err(trace
                    .fail(ResourceTraceFailure::ValueDepthLimit, None)
                    .to_string());
            }
            match ty {
                Type::Tuple(types) => {
                    let Value::Tuple(values) = value else {
                        return Err(trace
                            .fail(ResourceTraceFailure::BoundaryTypeMismatch, None)
                            .to_string());
                    };
                    if types.len() != values.len() {
                        return Err(trace
                            .fail(ResourceTraceFailure::BoundaryTypeMismatch, None)
                            .to_string());
                    }
                    let next_depth =
                        trace_child_depth(trace, nodes, pending.len(), types.len(), depth, None)?;
                    for (ty, value) in types.iter().zip(values).rev() {
                        trace_push(
                            &mut pending,
                            (ty, value, next_depth),
                            limits.max_value_nodes,
                        )
                        .map_err(|reason| trace.fail(reason, None).to_string())?;
                    }
                }
                Type::Named(name, args) if args.is_empty() => {
                    let scalar = match name.as_str() {
                        "i64" => Some(matches!(value, Value::Int(_))),
                        "u8" => Some(matches!(value, Value::UInt(n, 8) if *n <= u8::MAX as u64)),
                        "u32" => Some(matches!(value, Value::UInt(n, 32) if *n <= u32::MAX as u64)),
                        "u64" => Some(matches!(value, Value::UInt(_, 64))),
                        "f64" => Some(matches!(value, Value::Float(_))),
                        "bool" => Some(matches!(value, Value::Bool(_))),
                        "String" => Some(matches!(value, Value::String(_))),
                        "Unit" => Some(matches!(value, Value::Tuple(values) if values.is_empty())),
                        _ => None,
                    };
                    if let Some(valid) = scalar {
                        if !valid {
                            return Err(trace
                                .fail(ResourceTraceFailure::BoundaryTypeMismatch, None)
                                .to_string());
                        }
                        continue;
                    }
                    let adt = self
                        .world
                        .adts
                        .get(name)
                        .filter(|adt| adt.generics.is_empty())
                        .ok_or_else(|| {
                            trace
                                .fail(ResourceTraceFailure::UnsupportedValue, None)
                                .to_string()
                        })?;
                    let Value::Data {
                        ty: actual,
                        constructor,
                        fields,
                        ..
                    } = value
                    else {
                        return Err(trace
                            .fail(ResourceTraceFailure::BoundaryTypeMismatch, None)
                            .to_string());
                    };
                    if actual != name {
                        return Err(trace
                            .fail(ResourceTraceFailure::BoundaryTypeMismatch, None)
                            .to_string());
                    }
                    let variant = adt
                        .variants
                        .iter()
                        .find(|variant| variant.name == *constructor)
                        .ok_or_else(|| {
                            trace
                                .fail(ResourceTraceFailure::BoundaryTypeMismatch, None)
                                .to_string()
                        })?;
                    if fields.len() != variant.fields.len() {
                        return Err(trace
                            .fail(ResourceTraceFailure::BoundaryTypeMismatch, None)
                            .to_string());
                    }
                    let next_depth = trace_child_depth(
                        trace,
                        nodes,
                        pending.len(),
                        variant.fields.len(),
                        depth,
                        None,
                    )?;
                    for field in variant.fields.iter().rev() {
                        let value = fields.get(&field.name).ok_or_else(|| {
                            trace
                                .fail(ResourceTraceFailure::BoundaryTypeMismatch, None)
                                .to_string()
                        })?;
                        trace_push(
                            &mut pending,
                            (&field.ty, value, next_depth),
                            limits.max_value_nodes,
                        )
                        .map_err(|reason| trace.fail(reason, None).to_string())?;
                    }
                }
                _ => {
                    return Err(trace
                        .fail(ResourceTraceFailure::UnsupportedValue, None)
                        .to_string());
                }
            }
        }
        Ok(())
    }
    pub fn run(&mut self, entry: &str, args: Vec<Value>) -> Result<Value, String> {
        self.resource_fuel_exhausted = false;
        if let Some(trace) = &mut self.resource_trace {
            trace.begin_run().map_err(|e| e.to_string())?;
            trace.validate_values(&args).map_err(|e| e.to_string())?;
        }
        let mut result = match self.validate_resource_boundary(entry, &args, false) {
            Ok(()) => self.run_execution(entry, args),
            Err(error) => Err(error),
        };
        if let Ok(value) = &result
            && let Some(trace) = &mut self.resource_trace
            && let Err(e) = trace.validate_values(std::slice::from_ref(value))
        {
            result = Err(e.to_string());
        }
        if let Ok(value) = &result
            && let Err(error) =
                self.validate_resource_boundary(entry, std::slice::from_ref(value), true)
        {
            result = Err(error);
        }
        if let Some(trace) = &mut self.resource_trace {
            let completion = if self.resource_fuel_exhausted {
                ResourceTraceCompletion::FuelExhausted
            } else if result.is_ok() {
                ResourceTraceCompletion::Completed
            } else {
                ResourceTraceCompletion::RuntimeError
            };
            if let Err(error) = trace.finish(completion) {
                let mut message = format!("resource trace finalization failed: {error}");
                if let Err(original) = &result {
                    message.push_str("; original runtime error: ");
                    let mut end = original.len().min(4096usize.saturating_sub(message.len()));
                    while !original.is_char_boundary(end) {
                        end -= 1;
                    }
                    message.push_str(&original[..end]);
                }
                return Err(message);
            }
        }
        result
    }
    fn run_execution(&mut self, entry: &str, args: Vec<Value>) -> Result<Value, String> {
        self.stack.clear();
        self.external.clear();
        if self.optimize && !self.optimized {
            crate::ir_opt::optimize(self.world, &mut self.program)?;
            self.optimized = true;
        }
        let control = self.invoke(Value::Function(entry.into()), args, vec![])?;
        match self.drive(control)? {
            Outcome::Complete(value) => Ok(value),
            Outcome::Suspended(request) => Err(format!(
                "unhandled external effect {}::{}",
                request.effect.name, request.operation
            )),
        }
    }
    /// Install the entry function's linear effects at the host boundary. The
    /// host must resume each operation, including I/O errors and cancellation.
    pub fn begin_external(
        mut self,
        entry: &str,
        args: Vec<Value>,
    ) -> Result<Execution<'a>, String> {
        if let Some(trace) = &mut self.resource_trace {
            trace.begin_run().map_err(|e| e.to_string())?;
            return Err(trace
                .fail(ResourceTraceFailure::UnsupportedExternal, None)
                .to_string());
        }
        self.stack.clear();
        self.external.clear();
        if self.optimize && !self.optimized {
            crate::ir_opt::optimize(self.world, &mut self.program)?;
            self.optimized = true;
        }
        let function = self
            .world
            .functions
            .get(entry)
            .ok_or("missing entry function")?;
        if function.effects.tail.is_some() {
            return Err("external entry requires a concrete effect row".into());
        }
        let mut caller = Env::new();
        for effect in &function.effects.instances {
            if !self.world.effects[&effect.name].linear {
                return Err("external effects must guarantee one-shot resumption".into());
            }
            let id = self.next_capability;
            self.next_capability += 1;
            self.external.insert(id, effect.clone());
            caller.insert(
                format!("$effect:{}", effect.name),
                Slot::new(Value::Capability(id)),
            );
            if let Some(key) = crate::numeric::instance_key(&effect.name, &effect.args) {
                caller.insert(key, Slot::new(Value::Capability(id)));
            }
            if let Some(label) = &effect.label {
                caller.insert(format!("$label:{label}"), Slot::new(Value::Capability(id)));
            }
        }
        let control = self.invoke_scoped(Value::Function(entry.into()), args, vec![], &caller)?;
        self.continue_external(control)
    }
    fn continue_external(mut self, control: Control) -> Result<Execution<'a>, String> {
        match self.drive(control)? {
            Outcome::Complete(value) => Ok(Execution::Complete(value)),
            Outcome::Suspended(request) => Ok(Execution::Suspended(Suspension {
                machine: self,
                request,
            })),
        }
    }
    fn lowered_control(&self, kind: &str, body: Expr, env: Env) -> Control {
        let function = crate::ir::nested_name(kind, &body.span);
        if self.program.functions.contains_key(&function) {
            Control::Ir(Box::new(IrState {
                function,
                block: 0,
                ip: 0,
                values: BTreeMap::new(),
                env,
            }))
        } else {
            Control::Eval(body, env)
        }
    }
    fn ir_step(&mut self, mut state: IrState) -> Result<Control, String> {
        use crate::ir::{Op, Terminator};
        let block = self
            .program
            .functions
            .get(&state.function)
            .and_then(|f| f.blocks.get(state.block))
            .ok_or("invalid IR block")?;
        if state.ip >= block.instructions.len() {
            let term = block.terminator.clone();
            match term {
                Terminator::Return(v) => {
                    return Ok(Control::Return(
                        state
                            .values
                            .get(&v)
                            .cloned()
                            .ok_or("missing IR return value")?,
                    ));
                }
                Terminator::Jump { block, args } => {
                    let values = args
                        .iter()
                        .map(|v| {
                            state
                                .values
                                .get(v)
                                .cloned()
                                .ok_or("missing IR edge argument")
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let params = &self.program.functions[&state.function].blocks[block].params;
                    for (id, value) in params.iter().zip(values) {
                        state.values.insert(*id, value);
                    }
                    state.block = block;
                }
                Terminator::Branch { condition, yes, no } => {
                    let value = state.values.get(&condition).ok_or("missing IR condition")?;
                    state.block = match value {
                        Value::Bool(true) => yes,
                        Value::Bool(false) => no,
                        _ => return Err("IR branch condition must be bool".into()),
                    };
                }
                Terminator::Match { value, arms } => {
                    let value = state.values.get(&value).ok_or("missing IR match value")?;
                    let selected = arms
                        .iter()
                        .find(|(p, _)| bind_pattern(p, value, &mut Env::new()))
                        .map(|(_, b)| *b)
                        .ok_or("non-exhaustive IR match")?;
                    state.block = selected;
                }
                Terminator::Unreachable => return Err("reached an unreachable IR block".into()),
            }
            state.ip = 0;
            return Ok(Control::Ir(Box::new(state)));
        }
        let instruction = block.instructions[state.ip].clone();
        state.ip += 1;
        let get = |id| {
            state
                .values
                .get(&id)
                .cloned()
                .ok_or_else(|| format!("undefined IR value %{id}"))
        };
        let handler_state = if let Op::Handle { state_value, .. } = &instruction.op {
            *state_value
        } else {
            None
        };
        let flush = crate::capability_effects::flushes_subnormals(&self.denorm, &instruction.span);
        let result = match instruction.op {
            Op::Constant(l) => match l {
                Literal::Int(n) => {
                    if n > i64::MAX as i128 {
                        Value::UInt(n as u64, 64)
                    } else {
                        Value::Int(n as i64)
                    }
                }
                Literal::Float(n) => Value::Float(n),
                Literal::Bool(b) => Value::Bool(b),
                Literal::String(s) => Value::String(s),
            },
            Op::Unit => Value::Tuple(vec![]),
            Op::Read { root, path, .. } => {
                if path.is_empty()
                    && self.world.functions.contains_key(&root)
                    && !state.env.contains_key(&root)
                {
                    Value::Function(root)
                } else {
                    let mut slot = state
                        .env
                        .get(&root)
                        .cloned()
                        .ok_or_else(|| format!("undefined IR place '{root}'"))?;
                    slot.path.extend(path);
                    slot.get()?
                }
            }
            Op::Bind { pattern, value } => {
                let value = get(value)?;
                if !self.bind_owned_pattern(&pattern, &value, &mut state.env)? {
                    return Err("IR binding pattern mismatch".into());
                }
                Value::Tuple(vec![])
            }
            Op::Write { place, op, value } => {
                let value = get(value)?;
                let mut path = place.split('.');
                let root = path.next().ok_or("empty IR place")?;
                let mut slot = state
                    .env
                    .get(root)
                    .cloned()
                    .ok_or("undefined IR assignment place")?;
                slot.path.extend(path.map(str::to_owned));
                // A failing compound assignment raises before its write.
                let value = if op == "=" {
                    value
                } else {
                    match binary_at(flush, &op[..1], slot.get()?, value) {
                        Err(Fault::Raise(failure)) => {
                            return self.primitive_failure(
                                state,
                                instruction.dest,
                                instruction.ty,
                                failure,
                            );
                        }
                        result => result?,
                    }
                };
                slot.set(value)?;
                Value::Tuple(vec![])
            }
            Op::Unary(op, v) => match unary_at(flush, &op, get(v)?) {
                Err(Fault::Raise(failure)) => {
                    return self.primitive_failure(
                        state,
                        instruction.dest,
                        instruction.ty,
                        failure,
                    );
                }
                result => result?,
            },
            Op::Binary(op, a, b) => match binary_at(flush, &op, get(a)?, get(b)?) {
                Err(Fault::Raise(failure)) => {
                    return self.primitive_failure(
                        state,
                        instruction.dest,
                        instruction.ty,
                        failure,
                    );
                }
                result => result?,
            },
            Op::Tuple(ids) => {
                Value::Tuple(ids.into_iter().map(get).collect::<Result<Vec<_>, _>>()?)
            }
            Op::Data(name, fields) => {
                let (a, v) = self
                    .world
                    .constructor(&name)
                    .ok_or("unknown IR constructor")?;
                let ty = a.name.clone();
                let constructor = v.name.clone();
                let fields = fields
                    .into_iter()
                    .map(|(n, v)| Ok((n, get(v)?)))
                    .collect::<Result<_, String>>()?;
                self.construct_data(ty, constructor, fields, Some(instruction.span.clone()))?
            }
            Op::Project(base, field) => match get(base)? {
                Value::Data { mut fields, .. } => {
                    fields.remove(&field).ok_or("unknown IR field")?
                }
                Value::Tuple(values) => values
                    .get(
                        field
                            .parse::<usize>()
                            .map_err(|_| "invalid IR tuple field")?,
                    )
                    .cloned()
                    .ok_or("IR tuple field out of bounds")?,
                _ => return Err("IR projection requires data".into()),
            },
            Op::Index(base, index) => {
                let i = match get(index)? {
                    Value::Int(n) => usize::try_from(n).ok(),
                    Value::UInt(n, _) => usize::try_from(n).ok(),
                    _ => return Err("IR index must be integer".into()),
                };
                match get(base)? {
                    Value::Tuple(values) => match i.and_then(|i| values.get(i)).cloned() {
                        Some(value) => value,
                        None => {
                            return self.primitive_failure(
                                state,
                                instruction.dest,
                                instruction.ty,
                                Failure::IndexOutOfBounds,
                            );
                        }
                    },
                    _ => return Err("IR index requires tuple".into()),
                }
            }
            Op::Call {
                callee,
                args,
                capability,
                types,
            } => {
                let values = args
                    .iter()
                    .map(|a| get(a.value))
                    .collect::<Result<Vec<_>, _>>()?;
                let refs = args
                    .iter()
                    .map(|a| {
                        a.place.as_ref().and_then(|p| {
                            state.env.get(&p.root).cloned().map(|mut s| {
                                s.path.extend(p.path.clone());
                                s
                            })
                        })
                    })
                    .collect();
                let callee = if self.world.functions.contains_key(&callee) {
                    self.call_site = Some((instruction.span.start, instruction.span.end));
                    Value::Function(callee)
                } else if let Some((effect, op)) = callee.split_once("::") {
                    Value::EffectOp {
                        target: capability
                            .as_ref()
                            .and_then(|l| {
                                crate::runtime::capability(&state.env, &format!("$label:{l}"))
                            })
                            .or_else(|| instance_capability(&state.env, effect, &types)),
                        effect: effect.into(),
                        label: capability,
                        op: op.into(),
                    }
                } else {
                    return Err("unknown IR callee".into());
                };
                let caller = evidence_environment(&state.env);
                self.stack.push(Frame::IrResume {
                    state: Box::new(state),
                    dest: instruction.dest,
                    ty: instruction.ty,
                });
                return self.invoke_scoped(callee, values, refs, &caller);
            }
            Op::Indirect { callee, args } => {
                let callee = get(callee)?;
                let values = args
                    .iter()
                    .map(|a| get(a.value))
                    .collect::<Result<Vec<_>, _>>()?;
                let refs = args
                    .iter()
                    .map(|a| {
                        a.place.as_ref().and_then(|p| {
                            state.env.get(&p.root).cloned().map(|mut s| {
                                s.path.extend(p.path.clone());
                                s
                            })
                        })
                    })
                    .collect();
                let caller = evidence_environment(&state.env);
                self.stack.push(Frame::IrResume {
                    state: Box::new(state),
                    dest: instruction.dest,
                    ty: instruction.ty,
                });
                return self.invoke_scoped(callee, values, refs, &caller);
            }
            Op::Try(v) => {
                let value = get(v)?;
                let error = self.program.functions[&state.function]
                    .values
                    .get(&v)
                    .and_then(result_error);
                let target =
                    instance_capability(&state.env, "Fail", &error.into_iter().collect::<Vec<_>>());
                self.stack.push(Frame::IrResume {
                    state: Box::new(state),
                    dest: instruction.dest,
                    ty: instruction.ty,
                });
                self.stack.push(Frame::Try(target));
                return Ok(Control::Return(value));
            }
            Op::Closure {
                mut expr, aliases, ..
            }
            | Op::Handle {
                mut expr, aliases, ..
            } => {
                let mut env: Env = aliases
                    .into_iter()
                    .filter_map(|(name, slot)| state.env.get(&slot).cloned().map(|s| (name, s)))
                    .collect();
                env.extend(evidence_environment(&state.env));
                if let Some(id) = handler_state {
                    let name = format!("$handler_init{}", instruction.dest);
                    env.insert(name.clone(), Slot::new(get(id)?));
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
                self.stack.push(Frame::IrResume {
                    state: Box::new(state),
                    dest: instruction.dest,
                    ty: instruction.ty,
                });
                return Ok(Control::Eval(expr, env));
            }
        };
        state.values.insert(
            instruction.dest,
            coerce_value(
                self.qualify_value(result, &instruction.ty, &state.env, false),
                &instruction.ty,
            ),
        );
        Ok(Control::Ir(Box::new(state)))
    }
    fn drive(&mut self, mut control: Control) -> Result<Outcome, String> {
        loop {
            if self.fuel == 0 {
                self.resource_fuel_exhausted = true;
                return Err("execution fuel exhausted".into());
            }
            self.fuel -= 1;
            control = match control {
                Control::External(request) => return Ok(Outcome::Suspended(request)),
                Control::Ir(state) => self.ir_step(*state)?,
                Control::Eval(e, env) => {
                    let operation_site = e.span.clone();
                    if let Some(ty) = self.world.types.get(&(e.span.start, e.span.end)) {
                        self.stack
                            .push(Frame::Coerce(ty.clone(), evidence_environment(&env)));
                    }
                    match e.kind {
                        ExprKind::Literal(l) => Control::Return(match l {
                            Literal::Int(n) => {
                                if n > i64::MAX as i128 {
                                    Value::UInt(n as u64, 64)
                                } else {
                                    Value::Int(n as i64)
                                }
                            }
                            Literal::Float(n) => Value::Float(n),
                            Literal::Bool(b) => Value::Bool(b),
                            Literal::String(s) => Value::String(s),
                        }),
                        ExprKind::Name(n) => {
                            let value = if let Some(slot) = env.get(&n) {
                                slot.get()?
                            } else if self.world.functions.contains_key(&n) {
                                Value::Function(n)
                            } else if let Some((a, v)) = self.world.constructor(&n) {
                                if v.fields.is_empty() {
                                    let ty = a.name.clone();
                                    let constructor = v.name.clone();
                                    self.construct_data(
                                        ty,
                                        constructor,
                                        BTreeMap::new(),
                                        Some(operation_site),
                                    )?
                                } else {
                                    Value::Constructor(n)
                                }
                            } else if let Some((eff, op)) = n.split_once("::") {
                                Value::EffectOp {
                                    target: capability(&env, &format!("$effect:{eff}")),
                                    effect: eff.into(),
                                    label: None,
                                    op: op.into(),
                                }
                            } else {
                                return Err(format!("unknown runtime name '{n}'"));
                            };
                            Control::Return(value)
                        }
                        ExprKind::Tuple(es) => self.aggregate(
                            None,
                            es.into_iter()
                                .enumerate()
                                .map(|(i, e)| (i.to_string(), e))
                                .collect(),
                            env,
                            operation_site,
                        )?,
                        ExprKind::Struct { name, fields } => {
                            self.aggregate(Some(name), fields, env, operation_site)?
                        }
                        ExprKind::Block { statements, tail } => {
                            self.block(statements, tail, env)?
                        }
                        ExprKind::Unary(op, v) => {
                            let flush = crate::capability_effects::flushes_subnormals(
                                &self.denorm,
                                &operation_site,
                            );
                            self.stack
                                .push(Frame::Unary(op, flush, evidence_environment(&env)));
                            Control::Eval(*v, env)
                        }
                        ExprKind::Binary(op, left, right) => {
                            self.stack.push(Frame::BinaryLeft {
                                op,
                                flush: crate::capability_effects::flushes_subnormals(
                                    &self.denorm,
                                    &operation_site,
                                ),
                                right: *right,
                                env: env.clone(),
                            });
                            Control::Eval(*left, env)
                        }
                        ExprKind::Assign(op, left, right) => {
                            let slot = place_slot(&left, &env)?;
                            self.stack.push(Frame::Assign {
                                slot,
                                op,
                                flush: crate::capability_effects::flushes_subnormals(
                                    &self.denorm,
                                    &operation_site,
                                ),
                                evidence: evidence_environment(&env),
                            });
                            Control::Eval(*right, env)
                        }
                        ExprKind::Field(base, field) => {
                            if let ExprKind::Name(label) = &base.kind
                                && !env.contains_key(label)
                                && let Some(id) = capability(&env, &format!("$label:{label}"))
                            {
                                let effect = self
                                    .stack
                                    .iter()
                                    .rev()
                                    .find_map(|frame| {
                                        if let Frame::Handler(h) = frame {
                                            if h.id == id {
                                                Some(h.effect.name.clone())
                                            } else {
                                                None
                                            }
                                        } else {
                                            None
                                        }
                                    })
                                    .ok_or("lexical capability is no longer active")?;
                                control = Control::Return(Value::EffectOp {
                                    target: Some(id),
                                    effect,
                                    label: Some(label.clone()),
                                    op: field,
                                });
                                continue;
                            }
                            let v = place_slot(&base, &env).and_then(|s| s.get());
                            if let Ok(Value::Data { ty, .. }) = v {
                                let name = format!("{ty}::{field}");
                                if self.world.functions.contains_key(&name) {
                                    return Err(
                                        "method values are second-class; call the method directly"
                                            .into(),
                                    );
                                }
                            }
                            self.stack.push(Frame::Field(field));
                            Control::Eval(*base, env)
                        }
                        ExprKind::Index(base, index) => {
                            self.stack.push(Frame::IndexBase {
                                index: *index,
                                env: env.clone(),
                            });
                            Control::Eval(*base, env)
                        }
                        ExprKind::If {
                            condition,
                            then,
                            otherwise,
                        } => {
                            self.stack.push(Frame::If {
                                then: *then,
                                otherwise: otherwise.map(|b| *b),
                                env: env.clone(),
                            });
                            Control::Eval(*condition, env)
                        }
                        ExprKind::Match { value, arms } => {
                            self.stack.push(Frame::Match {
                                arms,
                                env: env.clone(),
                            });
                            Control::Eval(*value, env)
                        }
                        ExprKind::Try(v) => {
                            let error = self
                                .world
                                .types
                                .get(&(v.span.start, v.span.end))
                                .and_then(result_error);
                            self.stack.push(Frame::Try(instance_capability(
                                &env,
                                "Fail",
                                &error.into_iter().collect::<Vec<_>>(),
                            )));
                            Control::Eval(*v, env)
                        }
                        ExprKind::Closure {
                            mut params, body, ..
                        } => {
                            if let Some(Type::Function { params: typed, .. }) =
                                self.world.types.get(&(e.span.start, e.span.end))
                            {
                                for (p, (pass, t)) in params.iter_mut().zip(typed) {
                                    p.pass = *pass;
                                    p.ty = t.clone();
                                }
                            }
                            let names = crate::check::free_names(&body);
                            let mut captured = env
                                .iter()
                                .filter(|(n, _)| {
                                    (names.contains(*n)
                                        || n.starts_with("$effect:")
                                        || n.starts_with("$label:")
                                        || n.starts_with("$static:"))
                                        && !params.iter().any(|p| &p.name == *n)
                                })
                                .map(|(n, s)| Ok((n.clone(), Slot::new(s.get()?))))
                                .collect::<Result<Env, String>>()?;
                            if let Some(Type::Function { effects, .. }) =
                                self.world.types.get(&(e.span.start, e.span.end))
                            {
                                for effect in &effects.instances {
                                    if let Some(label) = &effect.label
                                        && let Some(id) =
                                            capability(&env, &format!("$static:{label}"))
                                    {
                                        captured.insert(
                                            format!("$effect:{}", effect.name),
                                            Slot::new(Value::Capability(id)),
                                        );
                                    }
                                }
                            }
                            let key = crate::ir::nested_name("closure", &body.span);
                            let ir_body = self.program.functions.contains_key(&key).then_some(key);
                            Control::Return(Value::Closure(Rc::new(Closure {
                                params,
                                body: *body,
                                ir_body,
                                env: captured,
                            })))
                        }
                        ExprKind::Call { callee, args, .. } => {
                            if let ExprKind::Field(base, method) = &callee.kind
                                && let Ok(slot) = place_slot(base, &env)
                                && let Value::Data { ty, .. } = slot.get()?
                            {
                                let name = format!("{ty}::{method}");
                                if self.world.functions.contains_key(&name) {
                                    let mut actuals = vec![Arg {
                                        pass: Pass::In,
                                        explicit: false,
                                        expr: *base.clone(),
                                    }];
                                    actuals.extend(args);
                                    control = self.arguments(
                                        Value::Function(name),
                                        vec![],
                                        vec![],
                                        actuals,
                                        env,
                                    )?;
                                    continue;
                                }
                            }
                            self.stack.push(Frame::Callee {
                                args,
                                env: env.clone(),
                            });
                            Control::Eval(*callee, env)
                        }
                        ExprKind::Handle {
                            effect,
                            state,
                            body,
                            ret,
                            ops,
                        } => {
                            if let Some((_, init)) = state {
                                self.stack.push(Frame::HandleState {
                                    static_label: format!("$cap{}", e.span.start),
                                    effect,
                                    body: *body,
                                    ret: *ret,
                                    ops,
                                    env: env.clone(),
                                });
                                Control::Eval(*init, env)
                            } else {
                                let captured = handler_environment(&env, &ret, &ops)?;
                                let id = self.next_capability;
                                self.next_capability = self
                                    .next_capability
                                    .checked_add(1)
                                    .ok_or("capability identity space exhausted")?;
                                let env = handler_body_environment(
                                    env,
                                    &effect,
                                    id,
                                    &format!("$cap{}", e.span.start),
                                );
                                self.stack.push(Frame::Handler(Handler {
                                    id,
                                    effect,
                                    state: None,
                                    ret: *ret,
                                    ops,
                                    env: captured,
                                }));
                                self.lowered_control("handler_body", *body, env)
                            }
                        }
                    }
                }
                Control::Return(value) => {
                    let Some(frame) = self.stack.pop() else {
                        return Ok(Outcome::Complete(value));
                    };
                    match frame {
                        Frame::IrResume {
                            mut state,
                            dest,
                            ty,
                        } => {
                            state.values.insert(
                                dest,
                                coerce_value(
                                    self.qualify_value(value, &ty, &state.env, false),
                                    &ty,
                                ),
                            );
                            Control::Ir(state)
                        }
                        Frame::Block {
                            rest,
                            tail,
                            mut env,
                            bind,
                        } => {
                            if let Some(p) = bind
                                && !self.bind_owned_pattern(&p, &value, &mut env)?
                            {
                                return Err("let pattern did not match".into());
                            }
                            self.block(rest, tail, env)?
                        }
                        Frame::Aggregate {
                            name,
                            mut done,
                            rest,
                            field,
                            env,
                            site,
                        } => {
                            done.push((field, value));
                            self.aggregate_next(name, done, rest, env, site)?
                        }
                        Frame::Coerce(ty, env) => Control::Return(coerce_value(
                            self.qualify_value(value, &ty, &env, false),
                            &ty,
                        )),
                        Frame::Unary(op, flush, evidence) => match unary_at(flush, &op, value) {
                            Err(Fault::Raise(failure)) => {
                                self.raise_primitive(&evidence, failure)?
                            }
                            result => Control::Return(result?),
                        },
                        Frame::BinaryLeft {
                            op,
                            flush,
                            right,
                            env,
                        } => {
                            if (op == "&&" && matches!(value, Value::Bool(false)))
                                || (op == "||" && matches!(value, Value::Bool(true)))
                            {
                                Control::Return(value)
                            } else {
                                self.stack.push(Frame::BinaryRight {
                                    op,
                                    flush,
                                    left: value,
                                    evidence: evidence_environment(&env),
                                });
                                Control::Eval(right, env)
                            }
                        }
                        Frame::BinaryRight {
                            op,
                            flush,
                            left,
                            evidence,
                        } => match binary_at(flush, &op, left, value) {
                            Err(Fault::Raise(failure)) => {
                                self.raise_primitive(&evidence, failure)?
                            }
                            result => Control::Return(result?),
                        },
                        Frame::Assign {
                            slot,
                            op,
                            flush,
                            evidence,
                        } => {
                            let value = if op == "=" {
                                Ok(value)
                            } else {
                                binary_at(flush, &op[..1], slot.get()?, value)
                            };
                            match value {
                                Err(Fault::Raise(failure)) => {
                                    self.raise_primitive(&evidence, failure)?
                                }
                                value => {
                                    slot.set(value?)?;
                                    Control::Return(Value::Tuple(vec![]))
                                }
                            }
                        }
                        Frame::Field(field) => Control::Return(match value {
                            Value::Data { mut fields, .. } => {
                                fields.remove(&field).ok_or("unknown field")?
                            }
                            _ => return Err("field access requires data value".into()),
                        }),
                        Frame::IndexBase { index, env } => {
                            self.stack.push(Frame::IndexValue(value));
                            Control::Eval(index, env)
                        }
                        Frame::IndexValue(base) => {
                            let Value::Int(i) = value else {
                                return Err("index requires integer".into());
                            };
                            Control::Return(match base {
                                Value::Tuple(v) => v
                                    .get(usize::try_from(i).map_err(|_| "negative index")?)
                                    .cloned()
                                    .ok_or("index out of bounds")?,
                                _ => return Err("index requires tuple".into()),
                            })
                        }
                        Frame::If {
                            then,
                            otherwise,
                            env,
                        } => match value {
                            Value::Bool(true) => Control::Eval(then, env),
                            Value::Bool(false) => {
                                if let Some(e) = otherwise {
                                    Control::Eval(e, env)
                                } else {
                                    Control::Return(Value::Tuple(vec![]))
                                }
                            }
                            _ => return Err("condition requires bool".into()),
                        },
                        Frame::Match { arms, env } => {
                            let mut selected = None;
                            for (p, body) in arms {
                                let mut local = env.clone();
                                if self.bind_owned_pattern(&p, &value, &mut local)? {
                                    selected = Some(Control::Eval(body, local));
                                    break;
                                }
                            }
                            selected.ok_or("non-exhaustive match at runtime")?
                        }
                        Frame::Callee { args, env } => {
                            self.arguments(value, vec![], vec![], args, env)?
                        }
                        Frame::Arguments {
                            callee,
                            mut done,
                            mut refs,
                            rest,
                            env,
                            current,
                        } => {
                            done.push(value);
                            refs.push(current);
                            self.arguments(callee, done, refs, rest, env)?
                        }
                        Frame::HandleState {
                            static_label,
                            effect,
                            body,
                            ret,
                            ops,
                            env,
                        } => {
                            let captured = handler_environment(&env, &ret, &ops)?;
                            let id = self.next_capability;
                            self.next_capability = self
                                .next_capability
                                .checked_add(1)
                                .ok_or("capability identity space exhausted")?;
                            let env = handler_body_environment(env, &effect, id, &static_label);
                            self.stack.push(Frame::Handler(Handler {
                                id,
                                effect,
                                state: Some(value),
                                ret,
                                ops,
                                env: captured,
                            }));
                            self.lowered_control("handler_body", body, env)
                        }
                        Frame::Handler(h) => {
                            let mut env = h.env.clone();
                            let v = if let Some(state) = h.state {
                                Value::Tuple(vec![value, state])
                            } else {
                                value
                            };
                            if !self.bind_owned_pattern(&h.ret.pats[0], &v, &mut env)? {
                                return Err("handler return pattern mismatch".into());
                            }
                            self.lowered_control("handler_clause", h.ret.body, env)
                        }
                        Frame::Try(target) => match value {
                            Value::Data {
                                constructor,
                                mut fields,
                                ..
                            } if constructor == "Ok" => {
                                Control::Return(fields.remove("0").ok_or("missing Ok payload")?)
                            }
                            Value::Data {
                                constructor,
                                mut fields,
                                ..
                            } if constructor == "Err" => self.perform(
                                "Fail",
                                None,
                                "raise",
                                vec![fields.remove("0").ok_or("missing Err payload")?],
                                vec![],
                                target,
                            )?,
                            _ => return Err("'?' requires Result".into()),
                        },
                    }
                }
            };
        }
    }
    fn block(
        &mut self,
        mut rest: Vec<Statement>,
        tail: Option<Box<Expr>>,
        env: Env,
    ) -> Result<Control, String> {
        if rest.is_empty() {
            return Ok(if let Some(tail) = tail {
                Control::Eval(*tail, env)
            } else {
                Control::Return(Value::Tuple(vec![]))
            });
        }
        let first = rest.remove(0);
        let (bind, value) = match first {
            Statement::Let { pat, value, .. } => (Some(pat), value),
            Statement::Expr(e) => (None, e),
        };
        self.stack.push(Frame::Block {
            rest,
            tail,
            env: env.clone(),
            bind,
        });
        Ok(Control::Eval(value, env))
    }
    fn aggregate(
        &mut self,
        name: Option<String>,
        fields: Vec<(String, Expr)>,
        env: Env,
        site: Span,
    ) -> Result<Control, String> {
        self.aggregate_next(name, vec![], fields, env, site)
    }
    fn aggregate_next(
        &mut self,
        name: Option<String>,
        done: Vec<(String, Value)>,
        mut rest: Vec<(String, Expr)>,
        env: Env,
        site: Span,
    ) -> Result<Control, String> {
        if rest.is_empty() {
            let v = if let Some(name) = name {
                let (a, v) = self.world.constructor(&name).ok_or("unknown constructor")?;
                let ty = a.name.clone();
                let constructor = v.name.clone();
                self.construct_data(ty, constructor, done.into_iter().collect(), Some(site))?
            } else {
                Value::Tuple(done.into_iter().map(|(_, v)| v).collect())
            };
            return Ok(Control::Return(v));
        }
        let (field, expr) = rest.remove(0);
        self.stack.push(Frame::Aggregate {
            name,
            done,
            rest,
            field,
            env: env.clone(),
            site,
        });
        Ok(Control::Eval(expr, env))
    }
    fn arguments(
        &mut self,
        callee: Value,
        done: Vec<Value>,
        refs: Vec<Option<Slot>>,
        mut rest: Vec<Arg>,
        env: Env,
    ) -> Result<Control, String> {
        if rest.is_empty() {
            return self.invoke_scoped(callee, done, refs, &env);
        }
        let arg = rest.remove(0);
        let current = place_slot(&arg.expr, &env).ok();
        self.stack.push(Frame::Arguments {
            callee,
            done,
            refs,
            rest,
            env: env.clone(),
            current,
        });
        Ok(Control::Eval(arg.expr, env))
    }
    fn invoke(
        &mut self,
        callee: Value,
        args: Vec<Value>,
        refs: Vec<Option<Slot>>,
    ) -> Result<Control, String> {
        self.invoke_scoped(callee, args, refs, &Env::new())
    }
    fn invoke_scoped(
        &mut self,
        callee: Value,
        args: Vec<Value>,
        refs: Vec<Option<Slot>>,
        caller: &Env,
    ) -> Result<Control, String> {
        match callee {
            Value::Function(name) => {
                let f = self
                    .world
                    .functions
                    .get(&name)
                    .ok_or_else(|| format!("unknown function '{name}'"))?;
                if args.len() != f.params.len() {
                    return Err("argument arity mismatch".into());
                }
                let mut env = if f.effects.tail.is_some() {
                    evidence_environment(caller)
                } else {
                    Env::new()
                };
                let vars = self
                    .call_site
                    .take()
                    .and_then(|site| self.world.calls.get(&site))
                    .cloned()
                    .unwrap_or_default();
                for (index, effect) in f.effects.instances.iter().enumerate() {
                    let target = if let Some(label) = &effect.label {
                        capability(caller, &format!("$label:{label}"))
                    } else {
                        let args = effect
                            .args
                            .iter()
                            .map(|t| substitute(t, &vars))
                            .collect::<Vec<_>>();
                        instance_capability(caller, &effect.name, &args)
                    };
                    if let Some(id) = target {
                        env.insert(
                            format!("$static:$fncap:{name}:{index}"),
                            Slot::new(Value::Capability(id)),
                        );
                        env.insert(
                            format!("$effect:{}", effect.name),
                            Slot::new(Value::Capability(id)),
                        );
                        if let Some(key) = crate::numeric::instance_key(&effect.name, &effect.args)
                        {
                            env.insert(key, Slot::new(Value::Capability(id)));
                        }
                        if let Some(label) = &effect.label {
                            env.insert(format!("$label:{label}"), Slot::new(Value::Capability(id)));
                        }
                    }
                }
                for (i, (p, v)) in f.params.iter().zip(args).enumerate() {
                    let v = self.qualify_value(v, &p.ty, &env, true);
                    let slot = if p.pass == Pass::Inout {
                        refs.get(i)
                            .cloned()
                            .flatten()
                            .ok_or("inout argument requires place")?
                    } else {
                        Slot::new(v)
                    };
                    env.insert(p.name.clone(), slot);
                }
                if !self.program.functions.contains_key(&name) {
                    return Err("function has no lowered body".into());
                }
                Ok(Control::Ir(Box::new(IrState {
                    function: name,
                    block: 0,
                    ip: 0,
                    values: BTreeMap::new(),
                    env,
                })))
            }
            Value::Constructor(name) => {
                let (a, v) = self.world.constructor(&name).ok_or("unknown constructor")?;
                let ty = a.name.clone();
                let constructor = v.name.clone();
                let fields = args
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| (i.to_string(), v))
                    .collect();
                Ok(Control::Return(self.construct_data(
                    ty,
                    constructor,
                    fields,
                    None,
                )?))
            }
            Value::Closure(c) => {
                if args.len() != c.params.len() {
                    return Err("closure argument arity mismatch".into());
                }
                let mut env = evidence_environment(caller);
                env.extend(snapshot_environment(&c.env)?);
                for (i, (p, v)) in c.params.iter().zip(args).enumerate() {
                    let v = self.qualify_value(v, &p.ty, &env, true);
                    let slot = if p.pass == Pass::Inout {
                        refs.get(i)
                            .cloned()
                            .flatten()
                            .ok_or("inout argument requires place")?
                    } else {
                        Slot::new(v)
                    };
                    env.insert(p.name.clone(), slot);
                }
                Ok(if let Some(function) = &c.ir_body {
                    Control::Ir(Box::new(IrState {
                        function: function.clone(),
                        block: 0,
                        ip: 0,
                        values: BTreeMap::new(),
                        env,
                    }))
                } else {
                    Control::Eval(c.body.clone(), env)
                })
            }
            Value::Bound(bound) => {
                let mut evidence = evidence_environment(caller);
                evidence.extend(bound.evidence.clone());
                self.invoke_scoped(bound.function.clone(), args, refs, &evidence)
            }
            Value::EffectOp {
                effect,
                label,
                op,
                target,
            } => self.perform(&effect, label.as_deref(), &op, args, refs, target),
            Value::Continuation(k) => {
                let mut k = k
                    .try_borrow_mut()
                    .map_err(|_| "continuation is already borrowed")?
                    .take()
                    .ok_or("one-shot continuation was resumed twice")?;
                let expected = if k.handler.state.is_some() { 2 } else { 1 };
                if args.len() != expected {
                    return Err(format!("continuation expects {expected} arguments"));
                }
                if expected == 2 {
                    k.handler.state = Some(args[1].clone());
                }
                self.stack.push(Frame::Handler(k.handler));
                self.stack.extend(k.frames);
                Ok(Control::Return(args[0].clone()))
            }
            _ => Err("value is not callable".into()),
        }
    }
    fn qualify_value(&self, value: Value, ty: &Type, env: &Env, ambient: bool) -> Value {
        match (value, ty) {
            (Value::Tuple(values), Type::Tuple(types)) => Value::Tuple(
                values
                    .into_iter()
                    .zip(types)
                    .map(|(v, t)| self.qualify_value(v, t, env, ambient))
                    .collect(),
            ),
            (
                Value::Data {
                    ty: name,
                    constructor,
                    mut fields,
                    resource_identity,
                },
                Type::Named(n, args),
            ) => {
                if let Some(a) = self.world.adts.get(n) {
                    let vars = a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    if let Some(variant) = a.variants.iter().find(|v| v.name == constructor) {
                        for f in &variant.fields {
                            if let Some(v) = fields.remove(&f.name) {
                                fields.insert(
                                    f.name.clone(),
                                    self.qualify_value(v, &substitute(&f.ty, &vars), env, ambient),
                                );
                            }
                        }
                    }
                }
                Value::Data {
                    ty: name,
                    constructor,
                    fields,
                    resource_identity,
                }
            }
            (value, Type::Function { effects, .. }) => {
                self.bind_callable_evidence(value, effects, env, ambient)
            }
            (value, _) => value,
        }
    }
    fn bind_callable_evidence(
        &self,
        value: Value,
        effects: &Effects,
        env: &Env,
        ambient: bool,
    ) -> Value {
        let mut evidence = if ambient && effects.tail.is_some() {
            evidence_environment(env)
        } else {
            Env::new()
        };
        for e in &effects.instances {
            let bound = e
                .label
                .as_ref()
                .is_some_and(|l| l.starts_with("$cap") || l.starts_with("$fncap:"));
            if !ambient && !bound {
                continue;
            }
            let key = if bound {
                format!("$static:{}", e.label.as_ref().unwrap())
            } else if let Some(label) = e.label.as_ref().filter(|l| !l.starts_with("$latent:")) {
                format!("$label:{label}")
            } else {
                format!("$effect:{}", e.name)
            };
            if let Some(id) = capability(env, &key) {
                evidence.insert(
                    format!("$effect:{}", e.name),
                    Slot::new(Value::Capability(id)),
                );
                if bound {
                    evidence.insert(key, Slot::new(Value::Capability(id)));
                } else if let Some(label) = &e.label {
                    evidence.insert(format!("$label:{label}"), Slot::new(Value::Capability(id)));
                }
                if let Value::Function(name) = &value
                    && let Some(f) = self.world.functions.get(name)
                {
                    for source in &f.effects.instances {
                        if source.name == e.name
                            && let Some(label) = &source.label
                        {
                            evidence.insert(
                                format!("$label:{label}"),
                                Slot::new(Value::Capability(id)),
                            );
                        }
                    }
                }
            }
        }
        if evidence.is_empty() {
            return value;
        }
        match value {
            Value::Closure(c) => {
                let mut c = (*c).clone();
                for (key, value) in evidence {
                    c.env.entry(key).or_insert(value);
                }
                Value::Closure(Rc::new(c))
            }
            Value::Function(_) => Value::Bound(Rc::new(BoundCallable {
                function: value,
                evidence,
            })),
            Value::Bound(bound) => {
                evidence.extend(bound.evidence.clone());
                Value::Bound(Rc::new(BoundCallable {
                    function: bound.function.clone(),
                    evidence,
                }))
            }
            _ => value,
        }
    }
    fn primitive_failure(
        &mut self,
        state: IrState,
        dest: u32,
        ty: Type,
        failure: Failure,
    ) -> Result<Control, String> {
        let env = evidence_environment(&state.env);
        self.stack.push(Frame::IrResume {
            state: Box::new(state),
            dest,
            ty,
        });
        self.raise_primitive(&env, failure)
    }
    /// Raise a primitive failure through the typed Fail instance in scope.
    fn raise_primitive(&mut self, env: &Env, failure: Failure) -> Result<Control, String> {
        let (ty, constructor) = match failure {
            Failure::Arith(crate::numeric::ArithFailure::Overflow) => {
                (crate::numeric::OVERFLOW_TYPE, crate::numeric::OVERFLOW_TYPE)
            }
            Failure::Arith(crate::numeric::ArithFailure::DivisionByZero) => {
                ("RuntimeError", "DivisionByZero")
            }
            Failure::IndexOutOfBounds => ("RuntimeError", "IndexOutOfBounds"),
        };
        let target = instance_capability(env, "Fail", &[Type::named(ty)]);
        self.perform(
            "Fail",
            None,
            "raise",
            vec![Value::Data {
                ty: ty.into(),
                constructor: constructor.into(),
                fields: BTreeMap::new(),
                resource_identity: None,
            }],
            vec![None],
            target,
        )
    }
    fn perform(
        &mut self,
        effect: &str,
        label: Option<&str>,
        op: &str,
        args: Vec<Value>,
        refs: Vec<Option<Slot>>,
        target: Option<u64>,
    ) -> Result<Control, String> {
        let index = self.stack.iter().rposition(|f| matches!(f, Frame::Handler(h) if h.effect.name == effect && target.map(|id|h.id == id).unwrap_or_else(|| label.is_none() || h.effect.label.as_deref() == label)));
        let Some(index) = index else {
            if let Some(effect) = target.and_then(|id| self.external.get(&id)).cloned() {
                return Ok(Control::External(ExternalRequest {
                    effect,
                    operation: op.into(),
                    arguments: args,
                    references: refs,
                }));
            }
            return Err(format!("unhandled effect {effect}::{op}"));
        };
        let frames = self.stack.split_off(index + 1);
        let Frame::Handler(h) = self.stack.pop().unwrap() else {
            unreachable!()
        };
        let clause = h
            .ops
            .iter()
            .find(|c| c.name == op)
            .cloned()
            .ok_or("handler lacks operation")?;
        let mut env = snapshot_environment(&h.env)?;
        let argument_count = args.len();
        let passes = self.world.effects[effect]
            .ops
            .iter()
            .find(|f| f.name == op)
            .ok_or("unknown effect operation")?
            .params
            .iter()
            .map(|p| p.pass)
            .collect::<Vec<_>>();
        let mut values = args;
        if let Some(state) = &h.state {
            values.push(state.clone());
        }
        values.push(Value::Continuation(Rc::new(RefCell::new(Some(
            Continuation { frames, handler: h },
        )))));
        if values.len() != clause.pats.len() {
            return Err("operation handler pattern arity mismatch".into());
        }
        let mut bindings = Env::new();
        let mut owned = Vec::new();
        for (i, (p, v)) in clause.pats.iter().zip(&values).enumerate() {
            let borrowed = i < argument_count && passes[i] != Pass::Sink;
            let bound = if borrowed {
                let slot = refs
                    .get(i)
                    .cloned()
                    .flatten()
                    .unwrap_or_else(|| Slot::new(v.clone()));
                bind_pattern_slot(p, &slot, &mut bindings)?
            } else {
                if let Some(trace) = &mut self.resource_trace {
                    trace_push(&mut owned, (p, v), trace.limits().max_value_nodes)
                        .map_err(|reason| trace.fail(reason, Some(p.span.clone())).to_string())?;
                }
                bind_pattern(p, v, &mut bindings)
            };
            if !bound {
                return Err("operation handler pattern mismatch".into());
            }
        }
        self.consume_owned_patterns(&owned)?;
        env.extend(bindings);
        Ok(self.lowered_control("handler_clause", clause.body, env))
    }
}
// Prepay every scheduled visit and depth before growing a traversal stack.
fn trace_child_depth(
    trace: &mut ResourceTraceEngine,
    visited: usize,
    pending: usize,
    children: usize,
    depth: usize,
    site: Option<&Span>,
) -> Result<usize, String> {
    let limits = trace.limits();
    if visited
        .checked_add(pending)
        .and_then(|n| n.checked_add(children))
        .is_none_or(|n| n > limits.max_value_nodes)
    {
        return Err(trace
            .fail(ResourceTraceFailure::ValueNodeLimit, site.cloned())
            .to_string());
    }
    if children == 0 {
        return Ok(depth);
    }
    let next_depth = depth
        .checked_add(1)
        .filter(|d| *d <= limits.max_value_depth)
        .ok_or_else(|| {
            trace
                .fail(ResourceTraceFailure::ValueDepthLimit, site.cloned())
                .to_string()
        })?;
    Ok(next_depth)
}
fn trace_push<T>(values: &mut Vec<T>, value: T, max: usize) -> Result<(), ResourceTraceFailure> {
    if values.len() >= max {
        return Err(ResourceTraceFailure::ValueNodeLimit);
    }
    if values.len() == values.capacity() {
        let capacity = values
            .capacity()
            .checked_mul(2)
            .unwrap_or(max)
            .max(1)
            .min(max);
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(|_| ResourceTraceFailure::Allocation)?;
    }
    values.push(value);
    Ok(())
}
fn place_slot(e: &Expr, env: &Env) -> Result<Slot, String> {
    match &e.kind {
        ExprKind::Name(n) => env
            .get(n)
            .cloned()
            .ok_or_else(|| format!("unknown place '{n}'")),
        ExprKind::Field(base, field) => {
            let mut s = place_slot(base, env)?;
            s.path.push(field.clone());
            Ok(s)
        }
        _ => Err("expression is not a place".into()),
    }
}
fn bind_pattern(p: &Pattern, value: &Value, env: &mut Env) -> bool {
    match &p.kind {
        PatternKind::Wild => true,
        PatternKind::Bind(n) => {
            env.insert(n.clone(), Slot::new(value.clone()));
            true
        }
        PatternKind::Literal(l) => match (l, value) {
            (Literal::Int(a), Value::Int(b)) => *a == *b as i128,
            (Literal::Int(a), Value::UInt(b, _)) => *a == *b as i128,
            (Literal::Float(a), Value::Float(b)) => a == b,
            (Literal::Bool(a), Value::Bool(b)) => a == b,
            (Literal::String(a), Value::String(b)) => a == b,
            _ => false,
        },
        PatternKind::Tuple(ps) => {
            if let Value::Tuple(vs) = value {
                ps.len() == vs.len() && ps.iter().zip(vs).all(|(p, v)| bind_pattern(p, v, env))
            } else {
                false
            }
        }
        PatternKind::Constructor { name, fields, .. } => {
            if let Value::Data {
                constructor,
                fields: values,
                ..
            } = value
            {
                name.rsplit("::").next() == Some(constructor.as_str())
                    && fields
                        .iter()
                        .all(|(name, p)| values.get(name).is_some_and(|v| bind_pattern(p, v, env)))
            } else {
                false
            }
        }
    }
}
fn bind_pattern_slot(p: &Pattern, slot: &Slot, env: &mut Env) -> Result<bool, String> {
    let value = slot.get()?;
    if !bind_pattern(p, &value, &mut Env::new()) {
        return Ok(false);
    }
    match &p.kind {
        PatternKind::Bind(n) => {
            env.insert(n.clone(), slot.clone());
        }
        PatternKind::Tuple(ps) => {
            for (i, p) in ps.iter().enumerate() {
                let mut field = slot.clone();
                field.path.push(i.to_string());
                if !bind_pattern_slot(p, &field, env)? {
                    return Ok(false);
                }
            }
        }
        PatternKind::Constructor { fields, .. } => {
            for (n, p) in fields {
                let mut field = slot.clone();
                field.path.push(n.clone());
                if !bind_pattern_slot(p, &field, env)? {
                    return Ok(false);
                }
            }
        }
        _ => {}
    }
    Ok(true)
}
/// A primitive operation either fails with a raisable source failure or with
/// an internal evaluator error.
enum Fault {
    Raise(Failure),
    Error(String),
}
#[derive(Clone, Copy)]
enum Failure {
    Arith(crate::numeric::ArithFailure),
    IndexOutOfBounds,
}
impl From<String> for Fault {
    fn from(e: String) -> Self {
        Self::Error(e)
    }
}
impl From<&str> for Fault {
    fn from(e: &str) -> Self {
        Self::Error(e.into())
    }
}
impl From<Fault> for String {
    fn from(f: Fault) -> Self {
        match f {
            Fault::Error(e) => e,
            Fault::Raise(_) => "unraised primitive failure".into(),
        }
    }
}
/// The integer kind carried by a coerced runtime value.
fn value_kind(v: &Value) -> Option<(crate::numeric::IntegerKind, i128)> {
    use crate::numeric::IntegerKind as K;
    Some(match v {
        Value::Int(n) => (K::I64, *n as i128),
        Value::UInt(n, 8) => (K::U8, *n as i128),
        Value::UInt(n, 32) => (K::U32, *n as i128),
        Value::UInt(n, _) => (K::U64, *n as i128),
        _ => return None,
    })
}
fn integer_value(kind: crate::numeric::IntegerKind, n: i128) -> Value {
    if kind.unsigned() {
        Value::UInt(n as u64, kind.bits())
    } else {
        Value::Int(n as i64)
    }
}
/// Software flush-to-zero for `eff [ConstDenorm]` code: a subnormal float
/// becomes a zero of the same sign.
fn flush_value(flush: bool, v: Value) -> Value {
    match v {
        Value::Float(x) if flush => Value::Float(crate::capability_effects::flush_subnormal(x)),
        v => v,
    }
}
/// [`unary`], flushing subnormal operands and results when `flush` holds.
fn unary_at(flush: bool, op: &str, v: Value) -> Result<Value, Fault> {
    unary(op, flush_value(flush, v)).map(|v| flush_value(flush, v))
}
/// [`binary`], flushing subnormal operands and results when `flush` holds.
fn binary_at(flush: bool, op: &str, a: Value, b: Value) -> Result<Value, Fault> {
    binary(op, flush_value(flush, a), flush_value(flush, b)).map(|v| flush_value(flush, v))
}
fn unary(op: &str, v: Value) -> Result<Value, Fault> {
    Ok(match (op, v) {
        ("-", Value::Float(n)) => Value::Float(-n),
        ("!", Value::Bool(b)) => Value::Bool(!b),
        (crate::capability_effects::WIDEN_INTRINSIC, v) => {
            let (_, n) = value_kind(&v).ok_or("invalid unary operands")?;
            Value::UInt(n as u64, 64)
        }
        ("-", v) => {
            let (kind, n) = value_kind(&v).ok_or("invalid unary operands")?;
            integer_value(
                kind,
                kind.checked_neg(n).ok_or(Fault::Raise(Failure::Arith(
                    crate::numeric::ArithFailure::Overflow,
                )))?,
            )
        }
        _ => return Err("invalid unary operands".into()),
    })
}
/// Binary operators under `vesper.arith.checked/1`. Integer operands share a
/// kind; an untyped `Int` operand adopts its unsigned partner's kind.
fn binary(op: &str, a: Value, b: Value) -> Result<Value, Fault> {
    if let (Some((ak, x)), Some((bk, y))) = (value_kind(&a), value_kind(&b)) {
        let kind = if ak.unsigned() { ak } else { bk };
        let (x, y) = (kind.wrap(x), kind.wrap(y));
        return Ok(match op {
            "+" | "-" | "*" | "/" | "%" => integer_value(
                kind,
                kind.checked(op, x, y)
                    .map_err(|f| Fault::Raise(Failure::Arith(f)))?,
            ),
            _ if crate::numeric::wrapping_intrinsic(op).is_some() => integer_value(
                kind,
                kind.wrapping(crate::numeric::wrapping_intrinsic(op).unwrap(), x, y),
            ),
            _ if crate::capability_effects::bit_intrinsic(op).is_some() => integer_value(
                kind,
                crate::capability_effects::bitwise(op, kind, x, y).unwrap_or_default(),
            ),
            "==" => Value::Bool(x == y),
            "!=" => Value::Bool(x != y),
            "<" => Value::Bool(x < y),
            "<=" => Value::Bool(x <= y),
            ">" => Value::Bool(x > y),
            ">=" => Value::Bool(x >= y),
            _ => return Err("invalid integer operator".into()),
        });
    }
    Ok(match (a, b) {
        (Value::Float(a), Value::Float(b)) => match op {
            "+" => Value::Float(a + b),
            "-" => Value::Float(a - b),
            "*" => Value::Float(a * b),
            "/" => Value::Float(a / b),
            "%" => Value::Float(a % b),
            "==" => Value::Bool(a == b),
            "!=" => Value::Bool(a != b),
            "<" => Value::Bool(a < b),
            "<=" => Value::Bool(a <= b),
            ">" => Value::Bool(a > b),
            ">=" => Value::Bool(a >= b),
            _ => return Err("invalid float operator".into()),
        },
        (Value::Bool(a), Value::Bool(b)) => match op {
            "&&" => Value::Bool(a && b),
            "||" => Value::Bool(a || b),
            "==" => Value::Bool(a == b),
            "!=" => Value::Bool(a != b),
            _ => return Err("invalid boolean operator".into()),
        },
        (Value::String(a), Value::String(b)) => match op {
            "==" => Value::Bool(a == b),
            "!=" => Value::Bool(a != b),
            _ => return Err("invalid string operator".into()),
        },
        (a, b) if op == "==" || op == "!=" => {
            let eq = equal(&a, &b);
            Value::Bool(if op == "==" { eq } else { !eq })
        }
        _ => return Err("incompatible binary operands".into()),
    })
}
fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::UInt(a, ab), Value::UInt(b, bb)) => a == b && ab == bb,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Tuple(a), Value::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        (
            Value::Data {
                constructor: a,
                fields: af,
                ..
            },
            Value::Data {
                constructor: b,
                fields: bf,
                ..
            },
        ) => {
            a == b
                && af.len() == bf.len()
                && af
                    .iter()
                    .all(|(n, a)| bf.get(n).is_some_and(|b| equal(a, b)))
        }
        _ => false,
    }
}

fn coerce_value(v: Value, t: &Type) -> Value {
    let Some(kind) = crate::numeric::IntegerKind::from_type(t) else {
        return v;
    };
    match v {
        Value::Int(n) => {
            if kind.unsigned() {
                Value::UInt(n as u64 & kind.mask(), kind.bits())
            } else {
                Value::Int(n)
            }
        }
        Value::UInt(n, _) => {
            if kind.unsigned() {
                Value::UInt(n & kind.mask(), kind.bits())
            } else {
                Value::Int(n as i64)
            }
        }
        _ => v,
    }
}

fn snapshot_environment(env: &Env) -> Result<Env, String> {
    env.iter()
        .map(|(n, s)| Ok((n.clone(), Slot::new(s.get()?))))
        .collect()
}
fn handler_environment(env: &Env, ret: &Clause, ops: &[Clause]) -> Result<Env, String> {
    let mut free = crate::check::clause_free_names(ret);
    for op in ops {
        free.extend(crate::check::clause_free_names(op));
    }
    env.iter()
        .filter(|(n, _)| {
            free.contains(*n)
                || n.starts_with("$effect:")
                || n.starts_with("$label:")
                || n.starts_with("$static:")
        })
        .map(|(n, s)| Ok((n.clone(), Slot::new(s.get()?))))
        .collect()
}

fn capability(env: &Env, key: &str) -> Option<u64> {
    env.get(key).and_then(|s| s.get().ok()).and_then(|v| {
        if let Value::Capability(id) = v {
            Some(id)
        } else {
            None
        }
    })
}
fn evidence_environment(env: &Env) -> Env {
    env.iter()
        .filter(|(n, _)| {
            n.starts_with("$effect:") || n.starts_with("$label:") || n.starts_with("$static:")
        })
        .map(|(n, s)| (n.clone(), s.clone()))
        .collect()
}
fn handler_body_environment(mut env: Env, effect: &Effect, id: u64, static_label: &str) -> Env {
    env.insert(
        format!("$static:{static_label}"),
        Slot::new(Value::Capability(id)),
    );
    env.insert(
        format!("$effect:{}", effect.name),
        Slot::new(Value::Capability(id)),
    );
    if let Some(key) = crate::numeric::instance_key(&effect.name, &effect.args) {
        env.insert(key, Slot::new(Value::Capability(id)));
    }
    if let Some(label) = &effect.label {
        env.insert(format!("$label:{label}"), Slot::new(Value::Capability(id)));
    }
    env
}
/// Typed instance selection (`vesper.arith.checked/1`): prefer the innermost
/// capability with exactly these concrete arguments, else the innermost by name.
fn instance_capability(env: &Env, effect: &str, args: &[Type]) -> Option<u64> {
    crate::numeric::instance_key(effect, args)
        .and_then(|key| capability(env, &key))
        .or_else(|| capability(env, &format!("$effect:{effect}")))
}
fn result_error(t: &Type) -> Option<Type> {
    match t {
        Type::Named(n, args) if n == "Result" && args.len() == 2 => Some(args[1].clone()),
        _ => None,
    }
}
