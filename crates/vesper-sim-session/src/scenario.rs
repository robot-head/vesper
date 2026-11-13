//! Versioned, explicitly bounded simulation scenarios.
//!
//! A scenario maps each checked external operation signature to one simulated
//! action. It is validated against the checked program before any task starts:
//! unknown effects/operations, mismatched signatures, unsupported value types,
//! malformed scripted values and missing bindings are all load errors.
//!
//! Version 1 has no model. Version 2 adds a required `model` section
//! (processes, virtual WAL logs, networks, timed faults and injected
//! failures), per-task `process` membership and the model actions.
//!
//! Version 3 additionally requires `model.warp` (UMEM NICs, datagram links,
//! flows and NAT events) and adds the `xdp_*` actions that bind the §35.2
//! `XdpNic` operations. Their signatures are checked structurally against the
//! checked source declarations at load (see [`warp_signature`]).
use crate::{
    model::{self, ModelPlan, ModelSpec, warp::WarpOp},
    value::{self, type_name},
};
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
/// Scenario version with a crash/storage/network model section.
pub const MODEL_SCENARIO_VERSION: u32 = 2;
/// Scenario version whose model has a `warp` section.
pub const WARP_SCENARIO_VERSION: u32 = 3;
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
    /// Required by version 2 and rejected by version 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSpec>,
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
    /// `fn op() -> u64`: open the log for this epoch; returns valid records.
    WalRecover { log: String },
    /// `fn op(record: T) -> Result[u64, u32]`: volatile append; `Ok(lsn)`.
    WalAppend { log: String },
    /// `fn op() -> Result[u64, u32]`: park until the flush delay elapses;
    /// `Ok(durable_lsn)` once the batch is durable.
    WalFlush { log: String },
    /// `fn op(index: u64) -> Option[T]`: the index-th durable record.
    WalRead { log: String },
    /// `fn op(to: u64, message: T) -> Result[u64, u32]`: `Ok(message_id)`.
    NetSend { network: String },
    /// `fn op(timeout_ns: u64) -> Result[T, u32]`: oldest message, or
    /// `Err(timeout_code)` once the timeout elapses.
    NetRecv { network: String },
    /// `harvest_rx_single() -> Option[UmemFrame[Rx]]`.
    XdpHarvest { nic: String },
    /// `view_frame_bytes(in frame: UmemFrame[Rx]) -> ByteSlice`.
    XdpView { nic: String },
    /// `recycle_to_fill_ring(sink frame: UmemFrame[Rx])`.
    XdpRecycle { nic: String },
    /// `turn_around_tx_in_place(sink frame: UmemFrame[Rx], tx_len: u32) -> UmemFrame[Tx]`.
    XdpTurnAround { nic: String },
    /// `submit_paced_tx(sink frame: UmemFrame[Tx], pkt_num_full: u64, edt_launch_ns: u64) -> InFlightQuicPkt`.
    XdpSubmit { nic: String },
    /// `park_for_ack_or_pto(sink inflight: InFlightQuicPkt)`.
    XdpPark { nic: String },
    /// `split_short_header(bytes: ByteSlice) -> Result[QuicShortHdr, u32]`.
    XdpSplitShortHeader { nic: String },
    /// `wait_rx(timeout_ns: u64) -> u64`: RX ring occupancy, 0 on timeout.
    XdpWaitRx { nic: String },
    /// `on_ack(bytes: ByteSlice) -> Result[AckSummary, u32]`: retire the
    /// parked leases a harvested ACK datagram covers.
    XdpOnAck { nic: String },
}
impl Action {
    pub fn is_model(&self) -> bool {
        matches!(
            self,
            Self::WalRecover { .. }
                | Self::WalAppend { .. }
                | Self::WalFlush { .. }
                | Self::WalRead { .. }
                | Self::NetSend { .. }
                | Self::NetRecv { .. }
        ) || self.warp().is_some()
    }
    /// The Warp operation and NIC of an `xdp_*` action.
    pub fn warp(&self) -> Option<(WarpOp, &str)> {
        Some(match self {
            Self::XdpHarvest { nic } => (WarpOp::Harvest, nic),
            Self::XdpView { nic } => (WarpOp::View, nic),
            Self::XdpRecycle { nic } => (WarpOp::Recycle, nic),
            Self::XdpTurnAround { nic } => (WarpOp::TurnAround, nic),
            Self::XdpSubmit { nic } => (WarpOp::Submit, nic),
            Self::XdpPark { nic } => (WarpOp::Park, nic),
            Self::XdpSplitShortHeader { nic } => (WarpOp::SplitShortHeader, nic),
            Self::XdpWaitRx { nic } => (WarpOp::WaitRx, nic),
            Self::XdpOnAck { nic } => (WarpOp::OnAck, nic),
            _ => return None,
        })
    }
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
    /// Version 2: the model process this task belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
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
    pub process: Option<usize>,
    /// Restart tasks are spawned by restarts, never at session start.
    pub restart: bool,
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
    /// Initial tasks first (scenario order), then every process's restart
    /// tasks.
    pub(crate) tasks: Vec<TaskPlan>,
    pub(crate) model: Option<ModelPlan>,
    /// Plan indices of each process's restart tasks.
    pub(crate) restarts: Vec<Vec<usize>>,
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
    let warp = scenario.model.as_ref().map(|m| m.warp.is_some());
    match (scenario.version, warp) {
        (SCENARIO_VERSION, None)
        | (MODEL_SCENARIO_VERSION, Some(false))
        | (WARP_SCENARIO_VERSION, Some(true)) => {}
        (SCENARIO_VERSION, Some(_)) => {
            return fail("scenario version 1 has no model section; use version 2");
        }
        (MODEL_SCENARIO_VERSION, None) => {
            return fail("scenario version 2 requires a model section");
        }
        (MODEL_SCENARIO_VERSION, Some(true)) => {
            return fail("a warp section needs scenario version 3");
        }
        (WARP_SCENARIO_VERSION, _) => {
            return fail("scenario version 3 requires a model with a warp section");
        }
        (version, _) => return fail(format!("unsupported scenario version {version}")),
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
    let restart_specs: Vec<&TaskSpec> = scenario
        .model
        .iter()
        .flat_map(|m| m.processes.iter().flat_map(|p| &p.restart))
        .collect();
    let entries: usize = scenario
        .tasks
        .iter()
        .chain(restart_specs.iter().copied())
        .map(|t| t.script.len())
        .sum();
    if entries > MAX_SCRIPT_ENTRIES {
        return fail(format!(
            "at most {MAX_SCRIPT_ENTRIES} script entries are allowed"
        ));
    }
    if scenario.tasks.len() + restart_specs.len() > MAX_TASKS {
        return fail(format!(
            "at most {MAX_TASKS} task specifications (initial plus restart) are allowed"
        ));
    }
    let model = match &scenario.model {
        Some(spec) => Some(
            model::validate(
                spec.clone(),
                scenario.tasks.len() as u64,
                scenario.limits.max_virtual_ns,
            )
            .map_err(ScenarioError)?,
        ),
        None => None,
    };

    let mut bindings = BTreeMap::new();
    // Payload type of each log and network, fixed by the first binding.
    let mut payloads: BTreeMap<(bool, String), String> = BTreeMap::new();
    // Source types of each NIC's frames, lease, view and header.
    let mut warp_roles: BTreeMap<(String, &'static str), String> = BTreeMap::new();
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
            action if action.warp().is_some() => {
                let (op, nic) = action.warp().expect("warp action");
                let known = model
                    .as_ref()
                    .and_then(|m| m.spec.warp.as_ref())
                    .is_some_and(|w| w.nics.iter().any(|n| n.name == nic));
                if !known {
                    return fail(format!(
                        "{signature}: unknown NIC {nic} (warp needs version 3)"
                    ));
                }
                for (role, ty) in warp_signature(world, op, &params, &result)
                    .map_err(|e| ScenarioError(format!("{signature}: {e}")))?
                {
                    let fixed = warp_roles
                        .entry((nic.to_string(), role))
                        .or_insert(ty.clone());
                    if *fixed != ty {
                        return fail(format!(
                            "{signature}: NIC {nic} uses {fixed} as its {role}, not {ty}"
                        ));
                    }
                }
                true
            }
            action => {
                let payload = model_payload(action, &params, &result).ok_or_else(|| {
                    ScenarioError(format!(
                        "action {action:?} is incompatible with {signature}"
                    ))
                })?;
                let Some(model) = &model else {
                    return fail(format!(
                        "{signature}: model actions need scenario version 2"
                    ));
                };
                let target = model_target(action);
                let known = if target.0 {
                    model.spec.networks.iter().any(|n| n.name == target.1)
                } else {
                    model.spec.logs.iter().any(|l| l.name == target.1)
                };
                if !known {
                    return fail(format!("{signature}: unknown model target {}", target.1));
                }
                if let Some(payload) = payload {
                    let fixed = payloads.entry(target.clone()).or_insert(payload.clone());
                    if *fixed != payload {
                        return fail(format!(
                            "{signature}: {} carries {fixed}, not {payload}",
                            target.1
                        ));
                    }
                }
                true
            }
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

    for nic in model
        .iter()
        .filter_map(|m| m.spec.warp.as_ref())
        .flat_map(|w| &w.nics)
    {
        if let (Some(rx), Some(tx)) = (
            warp_roles.get(&(nic.name.clone(), "rx_frame")),
            warp_roles.get(&(nic.name.clone(), "tx_frame")),
        ) && rx == tx
        {
            return fail(format!(
                "NIC {}: RX and TX frames need distinct typestates, both are {rx}",
                nic.name
            ));
        }
    }
    // A NIC's frames, leases and views identify chunks only within that
    // NIC, so no source type may serve two NICs: a value of one NIC can then
    // never be passed to another's operations.
    let mut role_owner: BTreeMap<&String, &String> = BTreeMap::new();
    for ((nic, role), ty) in &warp_roles {
        if *role == "header" || *role == "ack" {
            continue;
        }
        if let Some(other) = role_owner.insert(ty, nic)
            && other != nic
        {
            return fail(format!(
                "NICs {other} and {nic} both use {ty}; each NIC needs its own frame, lease and view types"
            ));
        }
    }
    let mut names = BTreeSet::new();
    let mut tasks = vec![];
    for task in &scenario.tasks {
        let process = match (&task.process, &model) {
            (None, _) => None,
            (Some(name), Some(model)) => Some(*model.process_index.get(name).ok_or_else(|| {
                ScenarioError(format!("task {} names unknown process {name}", task.name))
            })?),
            (Some(_), None) => {
                return fail(format!(
                    "task {}: processes need scenario version 2",
                    task.name
                ));
            }
        };
        tasks.push(task_plan(
            world, &bindings, &model, task, process, false, &mut names,
        )?);
    }
    let mut restarts = vec![];
    if let Some(model) = &model {
        for (index, process) in model.spec.processes.iter().enumerate() {
            let mut indices = vec![];
            for task in &process.restart {
                indices.push(tasks.len());
                tasks.push(task_plan(
                    world,
                    &bindings,
                    &Some(model.clone()),
                    task,
                    Some(index),
                    true,
                    &mut names,
                )?);
            }
            restarts.push(indices);
        }
    }
    Ok(Plan {
        scenario,
        bindings,
        tasks,
        model,
        restarts,
    })
}

/// `(is_network, name)` of a model action.
pub(crate) fn model_target(action: &Action) -> (bool, String) {
    match action {
        Action::WalRecover { log }
        | Action::WalAppend { log }
        | Action::WalFlush { log }
        | Action::WalRead { log } => (false, log.clone()),
        Action::NetSend { network } | Action::NetRecv { network } => (true, network.clone()),
        _ => unreachable!("not a model action"),
    }
}

/// Check a model action's signature. `Some(Some(t))` fixes payload type `t`;
/// `Some(None)` carries no payload; `None` is incompatible.
fn model_payload(
    action: &Action,
    params: &[(Pass, Type)],
    result: &Type,
) -> Option<Option<String>> {
    let by_value = |p: &(Pass, Type)| p.0 != Pass::Inout;
    let u64_arg = |p: &(Pass, Type)| by_value(p) && type_name(&p.1) == "u64";
    let status = type_name(result) == "Result[u64, u32]";
    let generic_arg = |name: &str, index: usize| match result {
        Type::Named(n, args) if n == name && args.len() == index + 1 => Some(&args[0]),
        _ => None,
    };
    match action {
        Action::WalRecover { .. } => {
            (params.is_empty() && type_name(result) == "u64").then_some(None)
        }
        Action::WalFlush { .. } => (params.is_empty() && status).then_some(None),
        Action::WalAppend { .. } => match params {
            [record] if by_value(record) && status => Some(Some(type_name(&record.1))),
            _ => None,
        },
        Action::WalRead { .. } => match params {
            [index] if u64_arg(index) => generic_arg("Option", 0).map(|t| Some(type_name(t))),
            _ => None,
        },
        Action::NetSend { .. } => match params {
            [to, message] if u64_arg(to) && by_value(message) && status => {
                Some(Some(type_name(&message.1)))
            }
            _ => None,
        },
        Action::NetRecv { .. } => match (params, result) {
            ([timeout], Type::Named(n, args))
                if u64_arg(timeout)
                    && n == "Result"
                    && args.len() == 2
                    && type_name(&args[1]) == "u32" =>
            {
                Some(Some(type_name(&args[0])))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Named fields of a non-generic-or-instantiated struct `ty` with `mode`,
/// sorted by name, as `(name, type)` strings.
fn struct_fields(world: &World, ty: &Type, linear: bool) -> Option<Vec<(String, String)>> {
    let Type::Named(name, args) = ty else {
        return None;
    };
    let adt = world.adts.get(name)?;
    let mode_ok = if linear {
        adt.mode == vesper::ast::Mode::Linear
    } else {
        adt.mode == vesper::ast::Mode::Copy
    };
    if adt.is_enum || !mode_ok || adt.generics.len() != args.len() || adt.variants.len() != 1 {
        return None;
    }
    let vars = adt
        .generics
        .iter()
        .zip(args)
        .map(|(g, t)| (g.name.clone(), t.clone()))
        .collect();
    let mut fields: Vec<(String, String)> = adt.variants[0]
        .fields
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                type_name(&vesper::ast::substitute(&f.ty, &vars)),
            )
        })
        .collect();
    fields.sort();
    Some(fields)
}
fn has_fields(actual: Option<Vec<(String, String)>>, expected: &[(&str, &str)]) -> bool {
    let mut expected: Vec<(String, String)> = expected
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect();
    expected.sort();
    actual == Some(expected)
}
/// `UmemFrame[State]`: a linear struct with one typestate parameter, a copy
/// struct with no fields, and exactly the descriptor fields.
fn frame_type(world: &World, ty: &Type) -> Result<String, String> {
    let ok = match ty {
        Type::Named(_, args) if args.len() == 1 => {
            has_fields(struct_fields(world, &args[0], false), &[])
                && has_fields(
                    struct_fields(world, ty, true),
                    &[
                        ("chunk_idx", "u32"),
                        ("data_offset", "u32"),
                        ("pkt_len", "u32"),
                    ],
                )
        }
        _ => false,
    };
    if ok {
        Ok(type_name(ty))
    } else {
        Err(format!(
            "{} is not a linear frame struct[State] {{ chunk_idx: u32, data_offset: u32, pkt_len: u32 }} with a field-less copy typestate",
            type_name(ty)
        ))
    }
}
fn view_type(world: &World, ty: &Type) -> Result<String, String> {
    let fields = [
        ("chunk_idx", "u32"),
        ("generation", "u64"),
        ("offset", "u32"),
        ("len", "u32"),
    ];
    if has_fields(struct_fields(world, ty, false), &fields) {
        Ok(type_name(ty))
    } else {
        Err(format!(
            "{} is not a copy view struct {{ chunk_idx: u32, generation: u64, offset: u32, len: u32 }}",
            type_name(ty)
        ))
    }
}
fn lease_type(world: &World, ty: &Type) -> Result<(String, String), String> {
    let err = || {
        format!(
            "{} is not a linear lease struct {{ pkt_num_full: u64, edt_departure_ns: u64, held_frame: <TX frame> }}",
            type_name(ty)
        )
    };
    let fields = struct_fields(world, ty, true).ok_or_else(err)?;
    let held = fields
        .iter()
        .find(|(n, _)| n == "held_frame")
        .map(|(_, t)| t.clone())
        .ok_or_else(err)?;
    if !has_fields(
        Some(fields),
        &[
            ("edt_departure_ns", "u64"),
            ("held_frame", held.as_str()),
            ("pkt_num_full", "u64"),
        ],
    ) {
        return Err(err());
    }
    let Type::Named(name, _) = ty else {
        return Err(err());
    };
    let Type::Named(_, args) = ty else {
        return Err(err());
    };
    let adt = &world.adts[name];
    let vars = adt
        .generics
        .iter()
        .zip(args)
        .map(|(g, t)| (g.name.clone(), t.clone()))
        .collect();
    let field_ty = adt.variants[0]
        .fields
        .iter()
        .find(|f| f.name == "held_frame")
        .map(|f| vesper::ast::substitute(&f.ty, &vars))
        .ok_or_else(err)?;
    Ok((type_name(ty), frame_type(world, &field_ty)?))
}
fn header_type(world: &World, ty: &Type) -> Result<String, String> {
    let fields = [
        ("first_byte", "u8"),
        ("dcid", "u64"),
        ("pkt_num_trunc", "u64"),
        ("payload_len", "u32"),
        ("peer_addr", "u32"),
        ("peer_port", "u32"),
    ];
    if has_fields(struct_fields(world, ty, false), &fields) {
        Ok(type_name(ty))
    } else {
        Err(format!(
            "{} is not a copy header struct {{ first_byte: u8, dcid: u64, pkt_num_trunc: u64, payload_len: u32, peer_addr: u32, peer_port: u32 }}",
            type_name(ty)
        ))
    }
}

fn ack_summary_type(world: &World, ty: &Type) -> Result<String, String> {
    let fields = [("largest_acked", "u64"), ("retired", "u64")];
    if has_fields(struct_fields(world, ty, false), &fields) {
        Ok(type_name(ty))
    } else {
        Err(format!(
            "{} is not a copy ACK summary struct {{ largest_acked: u64, retired: u64 }}",
            type_name(ty)
        ))
    }
}

/// Check an `xdp_*` binding against the §35.2 shape and return the source
/// type it fixes for each NIC role (`rx_frame`, `tx_frame`, `lease`, `view`,
/// `header`).
pub(crate) fn warp_signature(
    world: &World,
    op: WarpOp,
    params: &[(Pass, Type)],
    result: &Type,
) -> Result<Vec<(&'static str, String)>, String> {
    let unit = value::is_unit(result);
    let scalar = |p: &(Pass, Type), name: &str| p.0 == Pass::In && type_name(&p.1) == name;
    let shape = |ok: bool| {
        if ok {
            Ok(())
        } else {
            Err(format!(
                "{op:?} has the wrong parameter passing or result shape"
            ))
        }
    };
    Ok(match (op, params) {
        (WarpOp::Harvest, []) => match result {
            Type::Named(n, args) if n == "Option" && args.len() == 1 => {
                vec![("rx_frame", frame_type(world, &args[0])?)]
            }
            _ => return Err("harvest must return Option[<RX frame>]".into()),
        },
        (WarpOp::View, [frame]) => {
            shape(frame.0 == Pass::In)?;
            vec![
                ("rx_frame", frame_type(world, &frame.1)?),
                ("view", view_type(world, result)?),
            ]
        }
        (WarpOp::Recycle, [frame]) => {
            shape(frame.0 == Pass::Sink && unit)?;
            vec![("rx_frame", frame_type(world, &frame.1)?)]
        }
        (WarpOp::TurnAround, [frame, len]) => {
            shape(frame.0 == Pass::Sink && scalar(len, "u32"))?;
            vec![
                ("rx_frame", frame_type(world, &frame.1)?),
                ("tx_frame", frame_type(world, result)?),
            ]
        }
        (WarpOp::Submit, [frame, pn, edt]) => {
            shape(frame.0 == Pass::Sink && scalar(pn, "u64") && scalar(edt, "u64"))?;
            let tx = frame_type(world, &frame.1)?;
            let (lease, held) = lease_type(world, result)?;
            if held != tx {
                return Err(format!("the lease holds {held}, not the submitted {tx}"));
            }
            vec![("tx_frame", tx), ("lease", lease)]
        }
        (WarpOp::Park, [lease]) => {
            shape(lease.0 == Pass::Sink && unit)?;
            let (lease, held) = lease_type(world, &lease.1)?;
            vec![("lease", lease), ("tx_frame", held)]
        }
        (WarpOp::SplitShortHeader, [view]) => {
            shape(view.0 == Pass::In)?;
            let header = match result {
                Type::Named(n, args)
                    if n == "Result" && args.len() == 2 && type_name(&args[1]) == "u32" =>
                {
                    header_type(world, &args[0])?
                }
                _ => return Err("split_short_header must return Result[<header>, u32]".into()),
            };
            vec![("view", view_type(world, &view.1)?), ("header", header)]
        }
        (WarpOp::OnAck, [view]) => {
            shape(view.0 == Pass::In)?;
            let summary = match result {
                Type::Named(n, args)
                    if n == "Result" && args.len() == 2 && type_name(&args[1]) == "u32" =>
                {
                    ack_summary_type(world, &args[0])?
                }
                _ => return Err("on_ack must return Result[<ACK summary>, u32]".into()),
            };
            vec![("view", view_type(world, &view.1)?), ("ack", summary)]
        }
        (WarpOp::WaitRx, [timeout]) => {
            shape(scalar(timeout, "u64") && type_name(result) == "u64")?;
            vec![]
        }
        _ => return Err(format!("{op:?} has the wrong number of parameters")),
    })
}

fn task_plan(
    world: &World,
    bindings: &BTreeMap<(String, String), BoundOperation>,
    model: &Option<ModelPlan>,
    task: &TaskSpec,
    process: Option<usize>,
    restart: bool,
    names: &mut BTreeSet<String>,
) -> Result<TaskPlan, ScenarioError> {
    if task.name.is_empty() || task.name.contains('@') || !names.insert(task.name.clone()) {
        return fail(format!(
            "task names must be unique, non-empty and free of '@': {:?}",
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
            let Some(bound) = bindings.get(&(effect.name.clone(), op.name.clone())) else {
                return Err(context(format!(
                    "no binding for {}::{} installed by {}",
                    effect.name, op.name, task.entry
                )));
            };
            if bound.action.is_model() {
                let model = model.as_ref().expect("model actions imply a model");
                let Some(process) = process else {
                    return Err(context(format!(
                        "{} uses a model action but the task has no process",
                        bound.signature
                    )));
                };
                let spec = &model.spec;
                if let Some((_, nic)) = bound.action.warp() {
                    let owner = spec
                        .warp
                        .iter()
                        .flat_map(|w| &w.nics)
                        .find(|n| n.name == nic)
                        .map(|n| model.process_index[&n.process]);
                    if owner != Some(process) {
                        return Err(context(format!(
                            "{} uses NIC {nic}, which process {} does not own",
                            bound.signature, spec.processes[process].name
                        )));
                    }
                    continue;
                }
                let (network, target) = model_target(&bound.action);
                let owned = if network {
                    spec.processes[process].endpoints.contains_key(&target)
                } else {
                    spec.logs
                        .iter()
                        .any(|l| l.name == target && model.process_index[&l.process] == process)
                };
                if !owned {
                    return Err(context(format!(
                        "{} uses {target}, which process {} does not own",
                        bound.signature, spec.processes[process].name
                    )));
                }
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
    Ok(TaskPlan {
        name: task.name.clone(),
        process,
        restart,
        entry: task.entry.clone(),
        arguments,
        result,
        script,
        cancel_after,
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
