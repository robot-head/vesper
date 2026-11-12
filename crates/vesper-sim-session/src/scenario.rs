//! Versioned, explicitly bounded simulation scenarios.
//!
//! A scenario maps each checked external operation signature to one simulated
//! action. It is validated against the checked program before any task starts:
//! unknown effects/operations, mismatched signatures, unsupported value types,
//! malformed scripted values and missing bindings are all load errors.
use crate::value::{self, type_name};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet};
use vesper::{
    ast::{Pass, Type},
    check::World,
    runtime::Value,
};
use vesper_simpulse::IoResult;

pub const SCENARIO_FORMAT: &str = "vesper-sim-scenario";
pub const SCENARIO_VERSION: u32 = 1;
/// Hard upper bounds applied regardless of scenario limits.
pub const MAX_SCENARIO_BYTES: usize = 1 << 20;
pub const MAX_TASKS: usize = 64;
pub const MAX_BINDINGS: usize = 256;
pub const MAX_SCRIPT_ENTRIES: usize = 4096;
pub const MAX_FUEL: u64 = 1_000_000_000;
pub const MAX_OPERATIONS: u64 = 1_000_000;
pub const MAX_DISPATCHES: u64 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub format: String,
    pub version: u32,
    pub seed: u64,
    pub limits: Limits,
    pub runtime: RuntimeOptions,
    pub bindings: Vec<Binding>,
    pub tasks: Vec<TaskSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// External operations each task may perform, including immediate ones.
    pub max_operations_per_task: u64,
    /// Upper bound on the virtual clock; exceeding it interrupts the session.
    pub max_virtual_ns: u64,
    /// Total scheduler dispatches for the whole session.
    pub max_dispatches: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeOptions {
    /// Reference-evaluator fuel given to each task's `Runtime`.
    pub fuel: u64,
    pub optimize: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub effect: String,
    pub operation: String,
    /// Declared parameters, e.g. `"u64"` or `"inout u64"`.
    pub params: Vec<String>,
    /// Declared result, e.g. `"Result[u64, u32]"` or `"()"`.
    pub result: String,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// `fn op() -> u64`: resume immediately with the virtual clock.
    Clock,
    /// `fn op()`: return the task to the ready set.
    Yield,
    /// `fn op(.., ns: u64, ..)`: park on a virtual timer.
    Sleep { duration_argument: usize },
    /// `fn op() -> bool`: resume immediately with the cancellation flag.
    CancelRequested,
    /// Complete from the task's ordered script with a scheduled delay.
    Script,
    /// Park until the embedding host supplies a checked completion.
    External,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub name: String,
    pub entry: String,
    pub arguments: Vec<Json>,
    #[serde(default)]
    pub script: Vec<ScriptEntry>,
    #[serde(default)]
    pub cancel: Option<CancelSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelSpec {
    /// Request cancellation right after this many scripted/external
    /// submissions by the task have been parked (at least 1).
    pub after_submissions: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptEntry {
    pub effect: String,
    pub operation: String,
    /// The exact arguments the source must pass to this operation.
    pub arguments: Vec<Json>,
    pub delay_ns: u64,
    pub outcome: Outcome,
    /// Value delivered to the continuation; checked against the result type.
    pub response: Json,
    #[serde(default)]
    pub writes: Vec<ArgumentWrite>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Success {
        bytes: u64,
    },
    Error {
        code: i32,
    },
    /// Only valid after the task's cancellation was requested.
    Cancelled,
}
impl Outcome {
    pub fn io_result(self) -> IoResult {
        match self {
            Self::Success { bytes } => IoResult::Success { bytes },
            Self::Error { code } => IoResult::Error { code },
            Self::Cancelled => IoResult::Cancelled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgumentWrite {
    pub argument: usize,
    pub value: Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioError(pub String);
impl std::fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid scenario: {}", self.0)
    }
}
impl std::error::Error for ScenarioError {}
fn fail<T>(message: impl Into<String>) -> Result<T, ScenarioError> {
    Err(ScenarioError(message.into()))
}

impl Scenario {
    pub fn from_json(text: &str) -> Result<Self, ScenarioError> {
        if text.len() > MAX_SCENARIO_BYTES {
            return fail("scenario exceeds the 1 MiB bound");
        }
        serde_json::from_str(text).map_err(|e| ScenarioError(e.to_string()))
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("scenario serialization is infallible")
    }
}

/// A checked operation signature after effect-generic substitution.
#[derive(Debug, Clone)]
pub(crate) struct BoundOperation {
    pub action: Action,
    pub params: Vec<(Pass, Type)>,
    pub result: Type,
    pub signature: String,
}

pub(crate) struct ScriptStep {
    pub effect: String,
    pub operation: String,
    pub arguments: Json,
    pub delay_ns: u64,
    pub outcome: Outcome,
    pub response: Value,
    pub response_json: Json,
    pub writes: Vec<(usize, Value, Json)>,
}

pub(crate) struct TaskPlan {
    pub name: String,
    pub entry: String,
    pub arguments: Vec<Value>,
    pub result: Type,
    pub script: Vec<ScriptStep>,
    pub cancel_after: Option<u64>,
}

/// A scenario validated against one checked world.
pub struct Plan {
    pub(crate) scenario: Scenario,
    pub(crate) bindings: BTreeMap<(String, String), BoundOperation>,
    pub(crate) tasks: Vec<TaskPlan>,
}
impl Plan {
    pub fn scenario(&self) -> &Scenario {
        &self.scenario
    }
}

fn pass_name(pass: Pass) -> &'static str {
    match pass {
        Pass::In => "",
        Pass::Inout => "inout ",
        Pass::Sink => "sink ",
    }
}
pub(crate) fn param_name(pass: Pass, ty: &Type) -> String {
    format!("{}{}", pass_name(pass), type_name(ty))
}

pub(crate) fn validate(world: &World, scenario: Scenario) -> Result<Plan, ScenarioError> {
    if scenario.format != SCENARIO_FORMAT {
        return fail(format!("unknown format {:?}", scenario.format));
    }
    if scenario.version != SCENARIO_VERSION {
        return fail(format!("unsupported scenario version {}", scenario.version));
    }
    let limits = &scenario.limits;
    if scenario.runtime.fuel == 0 || scenario.runtime.fuel > MAX_FUEL {
        return fail(format!("runtime fuel must be in 1..={MAX_FUEL}"));
    }
    if limits.max_operations_per_task == 0 || limits.max_operations_per_task > MAX_OPERATIONS {
        return fail(format!(
            "max_operations_per_task must be in 1..={MAX_OPERATIONS}"
        ));
    }
    if limits.max_dispatches == 0 || limits.max_dispatches > MAX_DISPATCHES {
        return fail(format!("max_dispatches must be in 1..={MAX_DISPATCHES}"));
    }
    if scenario.tasks.is_empty() || scenario.tasks.len() > MAX_TASKS {
        return fail(format!("a scenario needs 1..={MAX_TASKS} tasks"));
    }
    if scenario.bindings.len() > MAX_BINDINGS {
        return fail(format!("at most {MAX_BINDINGS} bindings are allowed"));
    }
    let entries: usize = scenario.tasks.iter().map(|t| t.script.len()).sum();
    if entries > MAX_SCRIPT_ENTRIES {
        return fail(format!(
            "at most {MAX_SCRIPT_ENTRIES} script entries are allowed"
        ));
    }

    let mut bindings = BTreeMap::new();
    for binding in &scenario.bindings {
        let key = (binding.effect.clone(), binding.operation.clone());
        let effect = world
            .effects
            .get(&binding.effect)
            .ok_or_else(|| ScenarioError(format!("unknown effect {}", binding.effect)))?;
        let operation = effect
            .ops
            .iter()
            .find(|o| o.name == binding.operation)
            .ok_or_else(|| {
                ScenarioError(format!(
                    "unknown operation {}::{}",
                    binding.effect, binding.operation
                ))
            })?;
        if !effect.generics.is_empty() {
            return fail(format!(
                "generic effect {} is unsupported by scenario version 1",
                effect.name
            ));
        }
        if !effect.linear {
            return fail(format!("effect {} is not linear", effect.name));
        }
        let params: Vec<(Pass, Type)> = operation
            .params
            .iter()
            .map(|p| (p.pass, p.ty.clone()))
            .collect();
        let declared: Vec<String> = params.iter().map(|(p, t)| param_name(*p, t)).collect();
        let result = operation.result.clone();
        let signature = format!(
            "{}::{}({}) -> {}",
            binding.effect,
            binding.operation,
            declared.join(", "),
            type_name(&result)
        );
        if declared != binding.params || type_name(&result) != binding.result {
            return fail(format!(
                "binding for {}::{} declares ({}) -> {} but the checked signature is {signature}",
                binding.effect,
                binding.operation,
                binding.params.join(", "),
                binding.result
            ));
        }
        for (_, ty) in &params {
            supported(world, ty).map_err(|e| ScenarioError(format!("{signature}: {e}")))?;
        }
        supported(world, &result).map_err(|e| ScenarioError(format!("{signature}: {e}")))?;
        let action_ok = match &binding.action {
            Action::Clock => params.is_empty() && type_name(&result) == "u64",
            Action::CancelRequested => params.is_empty() && type_name(&result) == "bool",
            Action::Yield => value::is_unit(&result),
            Action::Sleep { duration_argument } => {
                value::is_unit(&result)
                    && params
                        .get(*duration_argument)
                        .is_some_and(|(p, t)| *p != Pass::Inout && type_name(t) == "u64")
            }
            Action::Script | Action::External => true,
        };
        if !action_ok {
            return fail(format!(
                "action {:?} is incompatible with {signature}",
                binding.action
            ));
        }
        let bound = BoundOperation {
            action: binding.action.clone(),
            params,
            result,
            signature,
        };
        if bindings.insert(key, bound).is_some() {
            return fail(format!(
                "duplicate binding for {}::{}",
                binding.effect, binding.operation
            ));
        }
    }

    let mut names = BTreeSet::new();
    let mut tasks = vec![];
    for task in &scenario.tasks {
        if task.name.is_empty() || !names.insert(task.name.clone()) {
            return fail(format!(
                "task names must be unique and non-empty: {:?}",
                task.name
            ));
        }
        let context = |e: String| ScenarioError(format!("task {}: {e}", task.name));
        let function = world
            .functions
            .get(&task.entry)
            .filter(|f| f.body.is_some())
            .ok_or_else(|| context(format!("unknown entry function {}", task.entry)))?;
        if !function.generics.is_empty() {
            return Err(context("generic entry functions are unsupported".into()));
        }
        if function.effects.tail.is_some() {
            return Err(context("entry needs a concrete effect row".into()));
        }
        for effect in &function.effects.instances {
            let declaration = &world.effects[&effect.name];
            for op in &declaration.ops {
                if !bindings.contains_key(&(effect.name.clone(), op.name.clone())) {
                    return Err(context(format!(
                        "no binding for {}::{} installed by {}",
                        effect.name, op.name, task.entry
                    )));
                }
            }
        }
        if function.params.len() != task.arguments.len() {
            return Err(context(format!(
                "{} takes {} arguments",
                task.entry,
                function.params.len()
            )));
        }
        let mut arguments = vec![];
        for (param, json) in function.params.iter().zip(&task.arguments) {
            if param.pass == Pass::Inout {
                return Err(context("entry parameters cannot be inout".into()));
            }
            arguments.push(value::decode(world, &param.ty, json).map_err(context)?);
        }
        let result = world
            .function_results
            .get(&task.entry)
            .cloned()
            .unwrap_or_else(|| function.result.clone());
        supported(world, &result).map_err(context)?;
        let installed: BTreeSet<&str> = function
            .effects
            .instances
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        let mut script = vec![];
        for (index, entry) in task.script.iter().enumerate() {
            let context =
                |e: String| ScenarioError(format!("task {} script entry {index}: {e}", task.name));
            let bound = bindings
                .get(&(entry.effect.clone(), entry.operation.clone()))
                .filter(|b| b.action == Action::Script && installed.contains(entry.effect.as_str()))
                .ok_or_else(|| {
                    context(format!(
                        "{}::{} is not a scripted operation of this task",
                        entry.effect, entry.operation
                    ))
                })?;
            if entry.arguments.len() != bound.params.len() {
                return Err(context("argument count differs from the signature".into()));
            }
            let mut arguments = vec![];
            for ((_, ty), json) in bound.params.iter().zip(&entry.arguments) {
                let v = value::decode(world, ty, json).map_err(context)?;
                arguments.push(value::encode(world, ty, &v).map_err(context)?);
            }
            let response = value::decode(world, &bound.result, &entry.response).map_err(context)?;
            let response_json = value::encode(world, &bound.result, &response).map_err(context)?;
            let mut writes = vec![];
            let mut written = BTreeSet::new();
            for write in &entry.writes {
                let (pass, ty) = bound
                    .params
                    .get(write.argument)
                    .ok_or_else(|| context(format!("no argument {}", write.argument)))?;
                if *pass != Pass::Inout || !written.insert(write.argument) {
                    return Err(context(format!(
                        "argument {} is not a distinct inout parameter",
                        write.argument
                    )));
                }
                let v = value::decode(world, ty, &write.value).map_err(context)?;
                let json = value::encode(world, ty, &v).map_err(context)?;
                writes.push((write.argument, v, json));
            }
            script.push(ScriptStep {
                effect: entry.effect.clone(),
                operation: entry.operation.clone(),
                arguments: Json::Array(arguments),
                delay_ns: entry.delay_ns,
                outcome: entry.outcome,
                response,
                response_json,
                writes,
            });
        }
        let cancel_after = match &task.cancel {
            Some(CancelSpec {
                after_submissions: 0,
            }) => return Err(context("after_submissions must be at least 1".into())),
            Some(spec) => Some(spec.after_submissions),
            None => None,
        };
        tasks.push(TaskPlan {
            name: task.name.clone(),
            entry: task.entry.clone(),
            arguments,
            result,
            script,
            cancel_after,
        });
    }
    Ok(Plan {
        scenario,
        bindings,
        tasks,
    })
}

/// Whether host values of `ty` can be decoded and encoded.
fn supported(world: &World, ty: &Type) -> Result<(), String> {
    fn walk(world: &World, ty: &Type, seen: &mut BTreeSet<String>) -> Result<(), String> {
        if value::is_unit(ty) {
            return Ok(());
        }
        match ty {
            Type::Tuple(items) => items.iter().try_for_each(|t| walk(world, t, seen)),
            Type::Named(name, args) => match name.as_str() {
                "u8" | "u32" | "u64" | "i64" | "f64" | "bool" | "String" | "Never" => Ok(()),
                _ => {
                    let adt = world
                        .adts
                        .get(name)
                        .ok_or_else(|| format!("unsupported host type {}", type_name(ty)))?;
                    if adt.generics.len() != args.len() {
                        return Err(format!("unresolved generic type {}", type_name(ty)));
                    }
                    args.iter().try_for_each(|t| walk(world, t, seen))?;
                    if !seen.insert(name.clone()) {
                        return Ok(());
                    }
                    let vars = adt
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    for variant in &adt.variants {
                        for field in &variant.fields {
                            walk(world, &vesper::ast::substitute(&field.ty, &vars), seen)?;
                        }
                    }
                    Ok(())
                }
            },
            _ => Err(format!("unsupported host type {}", type_name(ty))),
        }
    }
    walk(world, ty, &mut BTreeSet::new())
}

/// Deterministic JSON with recursively sorted object keys.
pub(crate) fn canonical(json: &Json) -> String {
    fn write(json: &Json, out: &mut String) {
        match json {
            Json::Object(map) => {
                out.push('{');
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort();
                for (i, key) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Json::String(key.clone()).to_string());
                    out.push(':');
                    write(&map[key], out);
                }
                out.push('}');
            }
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(json, &mut out);
    out
}
