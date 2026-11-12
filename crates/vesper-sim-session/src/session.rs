//! Source tasks executed by the reference evaluator, with every external linear
//! operation routed through a SimPulse-owned task token.
//!
//! Each task's payload owns either its unstarted `Runtime` or the unique
//! `Suspension` holding the evaluator continuation. The checked `World` is
//! borrowed from a `CheckedProgram` that outlives the session, so no payload is
//! self-referential. Adapter errors detected before a consuming runtime call
//! leave the owner in the session; failures reported by `begin_external` or
//! `Suspension::resume` are terminal because those calls consume the machine.
//!
//! Scenario version 2 adds model-internal SimPulse tasks (fault timers,
//! message arrivals, receive timeouts and flush completions) and process
//! epochs. A crashed task's owner is never resumed: when SimPulse next hands
//! it out, the session retires the token and keeps the owner as explicit
//! [`LostOwner`] state. See [`crate::model`].
mod model_ops;

use crate::{
    identity::{ExecutionIdentity, IdentityMismatch},
    model::{
        DurableState, LostState, ModelEvent, ModelSnapshot, ModelState, umem::HostBug,
        warp::WarpState,
    },
    program::CheckedProgram,
    scenario::{Action, ArgumentWrite, Outcome, Plan, Scenario, ScenarioError, validate},
    value::{self, unit},
};
use model_ops::{Internal, ModelStep};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::BTreeMap;
use vesper::runtime::{Execution, Runtime, Suspension, Value};
use vesper_simpulse::{
    CompletionId, Journal, RunToken, Simulator, TaskId, TransitionError, WakeReason,
};

pub const RECORDING_FORMAT: &str = "vesper-sim-recording";
/// Version 2 adds the model trace, final durable state and lost tasks.
pub const RECORDING_VERSION: u32 = 2;

/// Source-level events, kept separate from the SimPulse journal schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceEvent {
    Start {
        task: String,
        at_ns: u64,
    },
    Operation {
        task: String,
        at_ns: u64,
        effect: String,
        operation: String,
        arguments: Json,
    },
    Resume {
        task: String,
        at_ns: u64,
        response: Json,
        writes: Vec<(usize, Json)>,
        outcome: Option<Outcome>,
    },
    CancelRequested {
        task: String,
        at_ns: u64,
    },
    Output {
        task: String,
        at_ns: u64,
        value: Json,
    },
}

/// A task instance whose process crashed before it finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LostTask {
    pub task: String,
    pub process: String,
    pub epoch: u32,
    pub at_ns: u64,
    pub state: LostState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recording {
    pub format: String,
    pub version: u32,
    pub identity: ExecutionIdentity,
    /// The unmodified SimPulse v1 journal, including model-internal tasks.
    pub journal: Journal,
    pub trace: Vec<SourceEvent>,
    pub outputs: BTreeMap<String, Json>,
    pub model_trace: Vec<ModelEvent>,
    /// Exact durable bytes of every log at completion.
    pub durable: DurableState,
    pub lost: Vec<LostTask>,
}
impl Recording {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("recording serialization is infallible")
    }
    pub fn from_json(text: &str) -> Result<Self, SessionError> {
        let recording: Self =
            serde_json::from_str(text).map_err(|e| SessionError::Recording(e.to_string()))?;
        if recording.format != RECORDING_FORMAT || recording.version != RECORDING_VERSION {
            return Err(SessionError::Recording(format!(
                "unsupported recording {} version {}",
                recording.format, recording.version
            )));
        }
        Ok(recording)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionError {
    Scenario(ScenarioError),
    Fingerprint(String),
    Recording(String),
    Identity(IdentityMismatch),
    Simulator(vesper_simpulse::Error),
    TraceMismatch {
        index: usize,
        expected: Option<Box<SourceEvent>>,
        actual: Box<SourceEvent>,
    },
    TraceUnconsumed {
        remaining: usize,
    },
    OutputMismatch {
        task: String,
    },
    UnboundOperation {
        task: String,
        effect: String,
        operation: String,
    },
    InvalidArguments {
        task: String,
        message: String,
    },
    MissingScript {
        task: String,
        effect: String,
        operation: String,
    },
    ScriptMismatch {
        task: String,
        index: usize,
        expected: String,
        actual: String,
    },
    UnconsumedScript {
        task: String,
        remaining: usize,
    },
    CancelledWithoutRequest {
        task: String,
    },
    UnexpectedWake {
        task: String,
    },
    ArgumentWrite {
        task: String,
        message: String,
    },
    InvalidResponse {
        task: String,
        message: String,
    },
    UnknownTask(String),
    NoExternalOperation {
        task: String,
    },
    DuplicateCompletion {
        task: String,
    },
    OutputUnencodable {
        task: String,
        message: String,
    },
    OperationLimit {
        task: String,
    },
    VirtualTimeLimit {
        at_ns: u64,
    },
    DispatchLimit,
    /// The session was already interrupted; inspect or recover its owners.
    Interrupted,
    /// A model operation the model does not support in this state, such as
    /// appending before recovery or a second concurrent flush.
    Model {
        task: String,
        message: String,
    },
    /// A payload, log, queue or in-flight bound would be exceeded.
    ModelBudget {
        task: String,
        message: String,
    },
    /// Source plus model events would exceed `model.limits.max_events`.
    EventLimit,
    InstanceLimit,
    EpochLimit,
    /// The task's process crashed; its owner is lost process state.
    TaskLost {
        task: String,
    },
    /// A completion addressed to a request of an earlier process epoch.
    StaleCompletion {
        request: u64,
        epoch: u32,
    },
    ModelTraceMismatch {
        index: usize,
        expected: Option<Box<ModelEvent>>,
        actual: Box<ModelEvent>,
    },
    ModelTraceUnconsumed {
        remaining: usize,
    },
    DurableMismatch {
        log: String,
    },
    LostMismatch,
    /// UMEM conservation failed: a frame is missing, duplicated or still
    /// held by a finished task. This is an explicit model error, never a
    /// silent recycle.
    Conservation {
        task: String,
        message: String,
    },
}
impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SessionError {}

/// A host completion the session refused. The submitted response is handed
/// back unchanged, so the host still owns it.
#[derive(Debug, Clone, PartialEq)]
pub struct RejectedCompletion {
    pub error: SessionError,
    pub outcome: Outcome,
    pub response: Json,
    pub writes: Vec<ArgumentWrite>,
}

enum Wait {
    Yield,
    Timer,
    Script(usize),
    External,
    /// A parked model operation; its response is in `deliveries`.
    Model(u64),
}
enum Owner<'w> {
    Start(Runtime<'w>),
    /// Parked on a SimPulse transition with the wake it expects.
    Parked(Suspension<'w>, Wait),
    /// Received from the runtime but not yet parked (only seen when stalled).
    Routing(Suspension<'w>),
    /// A model-internal timer task that fires at the given virtual time.
    Internal(Internal, u64),
    Empty,
}
/// SimPulse payload. It is opaque outside this crate.
pub struct TaskSlot<'w> {
    owner: Owner<'w>,
}

/// An owner recovered from an interrupted session.
pub enum RecoveredOwner<'w> {
    NotStarted(Runtime<'w>),
    Suspended(Suspension<'w>),
    /// The task's runtime had already completed or been consumed.
    None,
    /// A model-internal timer task (fault, arrival, timeout or flush); it
    /// owns no source state.
    Internal(String),
}
pub struct StalledOwner<'w> {
    pub task: String,
    pub owner: RecoveredOwner<'w>,
}
/// The owner of a task whose process crashed. It is never resumed; holding
/// it here makes the loss explicit instead of a silent drop.
pub struct LostOwner<'w> {
    pub task: String,
    pub epoch: u32,
    pub owner: RecoveredOwner<'w>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingOperation {
    pub task: String,
    pub effect: String,
    pub operation: String,
    pub arguments: Json,
    pub external: bool,
    /// Unique per session; host completions may address it.
    pub request: u64,
    /// The task's process epoch when the operation was submitted.
    pub epoch: u32,
    completion: CompletionId,
    /// Whether SimPulse holds no scheduled event for this wait.
    unscheduled: bool,
}

#[derive(Default)]
struct Progress {
    script: usize,
    operations: u64,
    submissions: u64,
}

/// One spawned source task: an initial task or a restarted one.
struct Instance {
    name: String,
    spec: usize,
    process: Option<usize>,
    epoch: u32,
    id: TaskId,
    started: bool,
    lost: bool,
    progress: Progress,
    output: Option<Json>,
}

struct ExternalResponse {
    outcome: Outcome,
    response: Value,
    response_json: Json,
    writes: Vec<(usize, Value, Json)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Results of completed task instances in spawn order.
    pub outputs: Vec<(String, Json)>,
    pub virtual_ns: u64,
    pub recording: Recording,
    pub lost: Vec<LostTask>,
    /// Final model state (version 2 scenarios).
    pub model: Option<ModelSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalFailure {
    pub task: String,
    pub at_ns: u64,
    /// The runtime's error. The machine that produced it was consumed by the
    /// baseline `begin_external`/`resume` API and cannot be recovered.
    pub message: String,
    pub trace: Vec<SourceEvent>,
    pub model_trace: Vec<ModelEvent>,
}

pub enum RunResult<'w> {
    Complete(Report),
    /// The per-call step budget ran out; every owner is retained. Call `run` again.
    Budget(Session<'w>),
    /// Only externally completed operations are pending; complete one, then run.
    Quiescent(Session<'w>),
    /// A pre-transition error. Owners remain in the session; see `take_stalled`.
    Interrupted {
        session: Session<'w>,
        error: SessionError,
    },
    /// A consuming runtime transition failed; that task's owner no longer exists.
    Terminal(TerminalFailure),
}
impl std::fmt::Debug for RunResult<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Complete(r) => f.debug_tuple("Complete").field(r).finish(),
            Self::Budget(_) => f.write_str("Budget(..)"),
            Self::Quiescent(_) => f.write_str("Quiescent(..)"),
            Self::Interrupted { error, .. } => f.debug_tuple("Interrupted").field(error).finish(),
            Self::Terminal(t) => f.debug_tuple("Terminal").field(t).finish(),
        }
    }
}
impl<'w> RunResult<'w> {
    /// Drive budget exits to completion; any other result is returned. A zero
    /// budget makes no progress, so the result is returned unchanged.
    pub fn finish(self, steps_per_call: u64) -> Self {
        if steps_per_call == 0 {
            return self;
        }
        let mut result = self;
        while let Self::Budget(session) = result {
            result = session.run(steps_per_call);
        }
        result
    }
}

enum Fault<'w> {
    Stall(Box<RunToken<TaskSlot<'w>>>, SessionError),
    Interrupt(SessionError),
    Terminal(TerminalFailure),
}
impl<'w> Fault<'w> {
    fn stall(token: RunToken<TaskSlot<'w>>, error: SessionError) -> Self {
        Self::Stall(Box::new(token), error)
    }
}
type Step<'w> = Result<(), Fault<'w>>;
fn stall<'w>(error: TransitionError<TaskSlot<'w>>) -> Fault<'w> {
    Fault::stall(error.token, SessionError::Simulator(error.error))
}

struct Expected {
    recording: Recording,
    source: usize,
    model: usize,
}

pub struct Session<'w> {
    program: &'w CheckedProgram,
    plan: Plan,
    identity: ExecutionIdentity,
    sim: Simulator<TaskSlot<'w>>,
    /// Source task instances by SimPulse id; internal tasks are absent.
    tasks: BTreeMap<TaskId, usize>,
    instances: Vec<Instance>,
    waits: BTreeMap<TaskId, PendingOperation>,
    external: BTreeMap<CompletionId, ExternalResponse>,
    trace: Vec<SourceEvent>,
    expected: Option<Expected>,
    dispatches: u64,
    stalled: Option<RunToken<TaskSlot<'w>>>,
    interrupted: bool,
    model: Option<ModelState>,
    model_trace: Vec<ModelEvent>,
    next_request: u64,
    /// Requests of crashed epochs, with that epoch.
    stale: BTreeMap<u64, u32>,
    /// Responses for parked model operations, by request.
    deliveries: BTreeMap<u64, (Outcome, Json)>,
    lost: Vec<LostTask>,
    lost_owners: Vec<LostOwner<'w>>,
}

impl<'w> Session<'w> {
    /// Validate `scenario` against the checked program and spawn its tasks.
    pub fn record(program: &'w CheckedProgram, scenario: Scenario) -> Result<Self, SessionError> {
        let plan = validate(program.world(), scenario).map_err(SessionError::Scenario)?;
        let identity =
            ExecutionIdentity::compute(program, &plan).map_err(SessionError::Fingerprint)?;
        let sim = Simulator::new(plan.scenario().seed);
        Self::start(program, plan, identity, sim, None)
    }

    /// Re-run a recording. The executable, source, scenario, bindings, options
    /// and semantics must all match before any task is spawned.
    pub fn replay(
        program: &'w CheckedProgram,
        scenario: Scenario,
        recording: Recording,
    ) -> Result<Self, SessionError> {
        let plan = validate(program.world(), scenario).map_err(SessionError::Scenario)?;
        let identity =
            ExecutionIdentity::compute(program, &plan).map_err(SessionError::Fingerprint)?;
        recording
            .identity
            .compare(&identity)
            .map_err(SessionError::Identity)?;
        if recording.journal.seed != plan.scenario().seed {
            return Err(SessionError::Recording("journal seed differs".into()));
        }
        let sim = Simulator::replay(recording.journal.clone()).map_err(SessionError::Simulator)?;
        let expected = Expected {
            recording,
            source: 0,
            model: 0,
        };
        Self::start(program, plan, identity, sim, Some(expected))
    }

    fn start(
        program: &'w CheckedProgram,
        plan: Plan,
        identity: ExecutionIdentity,
        sim: Simulator<TaskSlot<'w>>,
        expected: Option<Expected>,
    ) -> Result<Self, SessionError> {
        let seed = plan.scenario().seed;
        let model = plan.model.as_ref().map(|m| ModelState::new(m, seed));
        let mut session = Self {
            program,
            plan,
            identity,
            sim,
            tasks: BTreeMap::new(),
            instances: vec![],
            waits: BTreeMap::new(),
            external: BTreeMap::new(),
            trace: vec![],
            expected,
            dispatches: 0,
            stalled: None,
            interrupted: false,
            model,
            model_trace: vec![],
            next_request: 1,
            stale: BTreeMap::new(),
            deliveries: BTreeMap::new(),
            lost: vec![],
            lost_owners: vec![],
        };
        for spec in 0..session.plan.tasks.len() {
            if !session.plan.tasks[spec].restart {
                let name = session.plan.tasks[spec].name.clone();
                session.spawn_instance(spec, name, 0)?;
            }
        }
        let faults: Vec<u64> = session
            .plan
            .model
            .iter()
            .flat_map(|m| m.spec.faults.iter().map(|f| f.at_ns))
            .collect();
        for (index, at_ns) in faults.into_iter().enumerate() {
            session.spawn_internal(Internal::Fault(index), at_ns)?;
        }
        let warp_timers = session
            .plan
            .model
            .as_ref()
            .and_then(|m| m.spec.warp.as_ref())
            .map(WarpState::initial_timers)
            .unwrap_or_default();
        for (timer, at_ns) in warp_timers {
            session.spawn_internal(Internal::Warp(timer), at_ns)?;
        }
        Ok(session)
    }

    fn spawn_instance(
        &mut self,
        spec: usize,
        name: String,
        epoch: u32,
    ) -> Result<(), SessionError> {
        let options = &self.plan.scenario().runtime;
        let mut runtime = Runtime::new(self.program.world());
        runtime.fuel = options.fuel;
        runtime.optimize = options.optimize;
        let id = self
            .sim
            .spawn(TaskSlot {
                owner: Owner::Start(runtime),
            })
            .map_err(|e| SessionError::Simulator(e.error))?;
        self.tasks.insert(id, self.instances.len());
        self.instances.push(Instance {
            name,
            spec,
            process: self.plan.tasks[spec].process,
            epoch,
            id,
            started: false,
            lost: false,
            progress: Progress::default(),
            output: None,
        });
        Ok(())
    }

    fn spawn_internal(&mut self, internal: Internal, at_ns: u64) -> Result<(), SessionError> {
        self.sim
            .spawn(TaskSlot {
                owner: Owner::Internal(internal, at_ns),
            })
            .map(|_| ())
            .map_err(|e| SessionError::Simulator(e.error))
    }

    pub fn identity(&self) -> &ExecutionIdentity {
        &self.identity
    }
    pub fn now_ns(&self) -> u64 {
        self.sim.now_ns()
    }
    pub fn trace(&self) -> &[SourceEvent] {
        &self.trace
    }
    pub fn model_trace(&self) -> &[ModelEvent] {
        &self.model_trace
    }
    pub fn journal(&self) -> &Journal {
        self.sim.journal()
    }
    /// The current process, log and network state (version 2 scenarios).
    pub fn model_snapshot(&self) -> Option<ModelSnapshot> {
        self.model.as_ref().map(|m| m.snapshot(self.sim.now_ns()))
    }
    /// Install a deliberately broken host binding (tests of the UMEM
    /// conservation check only).
    #[doc(hidden)]
    pub fn inject_host_bug(&mut self, bug: HostBug) {
        if let Some(warp) = self.model.as_mut().and_then(|m| m.warp.as_mut()) {
            warp.set_bug(Some(bug));
        }
    }
    /// Tasks lost to process crashes so far.
    pub fn lost(&self) -> &[LostTask] {
        &self.lost
    }
    /// Take the owners of crashed tasks that SimPulse has already retired.
    pub fn take_lost_owners(&mut self) -> Vec<LostOwner<'w>> {
        std::mem::take(&mut self.lost_owners)
    }
    /// Parked operations still owned by the simulator, in task order.
    pub fn pending(&self) -> Vec<PendingOperation> {
        self.waits.values().cloned().collect()
    }
    fn token_name(&self, token: &RunToken<TaskSlot<'w>>) -> String {
        match (&token.payload().owner, self.tasks.get(&token.task())) {
            (Owner::Internal(internal, _), _) => internal.describe(),
            (_, Some(index)) => self.instances[*index].name.clone(),
            (_, None) => "unknown".into(),
        }
    }
    /// The task and operation of the owner held after a pre-transition error.
    pub fn stalled_task(&self) -> Option<String> {
        let token = self.stalled.as_ref()?;
        Some(self.token_name(token))
    }

    /// Take the owner that was checked out when the session was interrupted.
    /// Owners parked inside the simulator stay there and are dropped with it.
    pub fn take_stalled(&mut self) -> Option<StalledOwner<'w>> {
        let mut token = self.stalled.take()?;
        let task = self.token_name(&token);
        let owner = recovered(std::mem::replace(
            &mut token.payload_mut().owner,
            Owner::Empty,
        ));
        Some(StalledOwner { task, owner })
    }

    /// Deliver an externally driven completion. The response, inout writes and
    /// outcome are validated against the checked signature and policy before
    /// SimPulse accepts the completion; on error nothing changes.
    pub fn complete_external(
        &mut self,
        task: &str,
        outcome: Outcome,
        response: Json,
        writes: Vec<ArgumentWrite>,
    ) -> Result<(), SessionError> {
        let index = self
            .instances
            .iter()
            .position(|t| t.name == task)
            .ok_or_else(|| SessionError::UnknownTask(task.into()))?;
        if self.instances[index].lost {
            return Err(SessionError::TaskLost { task: task.into() });
        }
        let id = self.instances[index].id;
        let pending = self
            .waits
            .get(&id)
            .filter(|p| p.external)
            .ok_or_else(|| SessionError::NoExternalOperation { task: task.into() })?;
        if self.external.contains_key(&pending.completion) {
            return Err(SessionError::DuplicateCompletion { task: task.into() });
        }
        let bound = &self.plan.bindings[&(pending.effect.clone(), pending.operation.clone())];
        let world = self.program.world();
        let invalid = |message: String| SessionError::InvalidResponse {
            task: task.into(),
            message,
        };
        let value = value::decode(world, &bound.result, &response).map_err(invalid)?;
        let response_json = value::encode(world, &bound.result, &value).map_err(invalid)?;
        let mut checked = vec![];
        for write in writes {
            let (pass, ty) = bound
                .params
                .get(write.argument)
                .ok_or_else(|| invalid(format!("no argument {}", write.argument)))?;
            if *pass != vesper::ast::Pass::Inout
                || checked.iter().any(|(i, _, _)| *i == write.argument)
            {
                return Err(invalid(format!(
                    "argument {} is not a distinct inout parameter",
                    write.argument
                )));
            }
            let v = value::decode(world, ty, &write.value).map_err(invalid)?;
            let json = value::encode(world, ty, &v).map_err(invalid)?;
            checked.push((write.argument, v, json));
        }
        if outcome == Outcome::Cancelled && self.sim.cancel_requested(id) != Some(true) {
            return Err(SessionError::CancelledWithoutRequest { task: task.into() });
        }
        let completion = pending.completion;
        self.sim
            .complete_io(completion, outcome.io_result())
            .map_err(SessionError::Simulator)?;
        self.external.insert(
            completion,
            ExternalResponse {
                outcome,
                response: value,
                response_json,
                writes: checked,
            },
        );
        Ok(())
    }

    /// Deliver a host completion addressed by request id. A request of a
    /// crashed epoch is recorded as a stale completion and rejected; every
    /// rejection hands the submitted response back.
    pub fn complete_request(
        &mut self,
        request: u64,
        outcome: Outcome,
        response: Json,
        writes: Vec<ArgumentWrite>,
    ) -> Result<(), Box<RejectedCompletion>> {
        let reject = |error| {
            Box::new(RejectedCompletion {
                error,
                outcome,
                response: response.clone(),
                writes: writes.clone(),
            })
        };
        if let Some(&epoch) = self.stale.get(&request) {
            let event = ModelEvent::StaleRejected {
                source: crate::model::StaleSource::HostCompletion,
                request,
                epoch,
                at_ns: self.sim.now_ns(),
            };
            if let Err(error) = self.check_model(std::slice::from_ref(&event)) {
                return Err(reject(error));
            }
            self.commit_model(vec![event]);
            return Err(reject(SessionError::StaleCompletion { request, epoch }));
        }
        let Some(task) = self
            .waits
            .values()
            .find(|p| p.request == request && p.external)
            .map(|p| p.task.clone())
        else {
            return Err(reject(SessionError::NoExternalOperation {
                task: format!("request {request}"),
            }));
        };
        self.complete_external(&task, outcome, response.clone(), writes.clone())
            .map_err(reject)
    }

    /// Run at most `max_steps` scheduler dispatches.
    pub fn run(mut self, max_steps: u64) -> RunResult<'w> {
        if self.interrupted {
            return RunResult::Interrupted {
                session: self,
                error: SessionError::Interrupted,
            };
        }
        let mut steps = 0;
        loop {
            if self.sim.is_complete() {
                return self.complete();
            }
            if steps >= max_steps {
                return RunResult::Budget(self);
            }
            if self.dispatches >= self.plan.scenario().limits.max_dispatches {
                return self.interrupt(SessionError::DispatchLimit);
            }
            let token = match self.sim.next_task() {
                Ok(Some(token)) => token,
                Ok(None) => return RunResult::Quiescent(self),
                Err(error) => return self.interrupt(SessionError::Simulator(error)),
            };
            steps += 1;
            self.dispatches += 1;
            let outcome = if self.sim.now_ns() > self.plan.scenario().limits.max_virtual_ns {
                Err(Fault::stall(
                    token,
                    SessionError::VirtualTimeLimit {
                        at_ns: self.sim.now_ns(),
                    },
                ))
            } else {
                self.dispatch(token)
            };
            match outcome {
                Ok(()) => {}
                Err(Fault::Stall(token, error)) => {
                    self.stalled = Some(*token);
                    return self.interrupt(error);
                }
                Err(Fault::Interrupt(error)) => return self.interrupt(error),
                Err(Fault::Terminal(failure)) => return RunResult::Terminal(failure),
            }
        }
    }

    fn interrupt(mut self, error: SessionError) -> RunResult<'w> {
        self.interrupted = true;
        RunResult::Interrupted {
            session: self,
            error,
        }
    }

    fn complete(self) -> RunResult<'w> {
        if let Err(error) = self.sim.finish_replay() {
            return self.interrupt(SessionError::Simulator(error));
        }
        let outputs: Vec<(String, Json)> = self
            .instances
            .iter()
            .filter_map(|i| i.output.clone().map(|o| (i.name.clone(), o)))
            .collect();
        let output_map: BTreeMap<String, Json> = outputs.iter().cloned().collect();
        let durable = self
            .model
            .as_ref()
            .map(ModelState::durable)
            .unwrap_or_default();
        if let Some(expected) = &self.expected {
            let recording = &expected.recording;
            if expected.source != recording.trace.len() {
                let remaining = recording.trace.len() - expected.source;
                return self.interrupt(SessionError::TraceUnconsumed { remaining });
            }
            if expected.model != recording.model_trace.len() {
                let remaining = recording.model_trace.len() - expected.model;
                return self.interrupt(SessionError::ModelTraceUnconsumed { remaining });
            }
            if let Some(task) = output_map
                .iter()
                .find(|(k, v)| recording.outputs.get(*k) != Some(v))
                .map(|(k, _)| k.clone())
                .or_else(|| {
                    recording
                        .outputs
                        .keys()
                        .find(|k| !output_map.contains_key(*k))
                        .cloned()
                })
            {
                return self.interrupt(SessionError::OutputMismatch { task });
            }
            if let Some(log) = durable
                .iter()
                .find(|(k, v)| recording.durable.get(*k) != Some(v))
                .map(|(k, _)| k.clone())
                .or_else(|| {
                    recording
                        .durable
                        .keys()
                        .find(|k| !durable.contains_key(*k))
                        .cloned()
                })
            {
                return self.interrupt(SessionError::DurableMismatch { log });
            }
            if recording.lost != self.lost {
                return self.interrupt(SessionError::LostMismatch);
            }
        }
        let recording = Recording {
            format: RECORDING_FORMAT.into(),
            version: RECORDING_VERSION,
            identity: self.identity.clone(),
            journal: self.sim.journal().clone(),
            trace: self.trace.clone(),
            outputs: output_map,
            model_trace: self.model_trace.clone(),
            durable,
            lost: self.lost.clone(),
        };
        RunResult::Complete(Report {
            outputs,
            virtual_ns: self.sim.now_ns(),
            recording,
            lost: self.lost.clone(),
            model: self.model_snapshot(),
        })
    }

    fn event_budget(&self, additional: usize) -> Result<(), SessionError> {
        if let Some(model) = &self.plan.model {
            let used = (self.trace.len() + self.model_trace.len() + additional) as u64;
            if used > model.spec.limits.max_events {
                return Err(SessionError::EventLimit);
            }
        }
        Ok(())
    }

    /// Record (or, when replaying, check) one source event before the
    /// transition it describes.
    fn event(&mut self, event: SourceEvent) -> Result<(), SessionError> {
        self.check_source(&event, 0)?;
        if let Some(expected) = &mut self.expected {
            expected.source += 1;
        }
        self.trace.push(event);
        Ok(())
    }

    /// Check the next source event against the event budget (with
    /// `alongside` further events reserved) and the recorded trace.
    fn check_source(&self, event: &SourceEvent, alongside: usize) -> Result<(), SessionError> {
        self.event_budget(1 + alongside)?;
        if let Some(expected) = &self.expected {
            let recorded = expected.recording.trace.get(expected.source);
            if recorded != Some(event) {
                return Err(SessionError::TraceMismatch {
                    index: expected.source,
                    expected: recorded.cloned().map(Box::new),
                    actual: Box::new(event.clone()),
                });
            }
        }
        Ok(())
    }

    /// Check model events against the event budget and, when replaying, the
    /// recorded model trace, without committing them.
    fn check_model(&self, events: &[ModelEvent]) -> Result<(), SessionError> {
        self.event_budget(events.len())?;
        if let Some(expected) = &self.expected {
            for (offset, event) in events.iter().enumerate() {
                let index = expected.model + offset;
                let recorded = expected.recording.model_trace.get(index);
                if recorded != Some(event) {
                    return Err(SessionError::ModelTraceMismatch {
                        index,
                        expected: recorded.cloned().map(Box::new),
                        actual: Box::new(event.clone()),
                    });
                }
            }
        }
        Ok(())
    }
    fn commit_model(&mut self, events: Vec<ModelEvent>) {
        if let Some(expected) = &mut self.expected {
            expected.model += events.len();
        }
        self.model_trace.extend(events);
    }

    fn dispatch(&mut self, mut token: RunToken<TaskSlot<'w>>) -> Step<'w> {
        let id = token.task();
        let at_ns = self.sim.now_ns();
        if let Owner::Internal(..) = token.payload().owner {
            return self.dispatch_internal(token);
        }
        let Some(&index) = self.tasks.get(&id) else {
            return Err(Fault::stall(
                token,
                SessionError::UnexpectedWake {
                    task: "unknown".into(),
                },
            ));
        };
        let name = self.instances[index].name.clone();
        if self.instances[index].lost {
            // Retire the crashed task's SimPulse token without resuming it.
            let owner = recovered(std::mem::replace(
                &mut token.payload_mut().owner,
                Owner::Empty,
            ));
            self.lost_owners.push(LostOwner {
                task: name,
                epoch: self.instances[index].epoch,
                owner,
            });
            self.sim.finish(token).map_err(stall)?;
            return Ok(());
        }
        let spec = self.instances[index].spec;
        let owner = std::mem::replace(&mut token.payload_mut().owner, Owner::Empty);
        let execution = match (owner, token.reason()) {
            (Owner::Start(runtime), WakeReason::Start) => {
                if let Err(error) = self.event(SourceEvent::Start {
                    task: name.clone(),
                    at_ns,
                }) {
                    token.payload_mut().owner = Owner::Start(runtime);
                    return Err(Fault::stall(token, error));
                }
                self.instances[index].started = true;
                let task = &self.plan.tasks[spec];
                runtime.begin_external(&task.entry, task.arguments.clone())
            }
            (Owner::Parked(mut continuation, wait), reason) => {
                let pending = self.waits.get(&id).map(|p| p.completion);
                let delivered = match (&wait, reason) {
                    (Wait::Yield, WakeReason::Yield) => Some((unit(), Json::Null, vec![], None)),
                    (Wait::Timer, WakeReason::Timer(c)) if pending == Some(c) => {
                        Some((unit(), Json::Null, vec![], None))
                    }
                    (Wait::Script(step), WakeReason::Io { completion, result })
                        if pending == Some(completion)
                            && self.plan.tasks[spec].script[*step].outcome.io_result()
                                == result =>
                    {
                        let step = &self.plan.tasks[spec].script[*step];
                        Some((
                            step.response.clone(),
                            step.response_json.clone(),
                            step.writes.clone(),
                            Some(step.outcome),
                        ))
                    }
                    (Wait::External, WakeReason::Io { completion, result })
                        if pending == Some(completion)
                            && self
                                .external
                                .get(&completion)
                                .is_some_and(|r| r.outcome.io_result() == result) =>
                    {
                        let r = self.external.remove(&completion).unwrap();
                        Some((r.response, r.response_json, r.writes, Some(r.outcome)))
                    }
                    (Wait::Model(request), WakeReason::Io { completion, result })
                        if pending == Some(completion)
                            && self
                                .deliveries
                                .get(request)
                                .is_some_and(|(o, _)| o.io_result() == result) =>
                    {
                        let p = &self.waits[&id];
                        let bound = &self.plan.bindings[&(p.effect.clone(), p.operation.clone())];
                        let (outcome, json) = &self.deliveries[request];
                        match value::decode(self.program.world(), &bound.result, json) {
                            Ok(value) => Some((value, json.clone(), vec![], Some(*outcome))),
                            Err(_) => None,
                        }
                    }
                    _ => None,
                };
                let Some((response, response_json, writes, outcome)) = delivered else {
                    token.payload_mut().owner = Owner::Parked(continuation, wait);
                    return Err(Fault::stall(
                        token,
                        SessionError::UnexpectedWake { task: name },
                    ));
                };
                if outcome == Some(Outcome::Cancelled) && !token.cancel_requested() {
                    token.payload_mut().owner = Owner::Parked(continuation, wait);
                    return Err(Fault::stall(
                        token,
                        SessionError::CancelledWithoutRequest { task: name },
                    ));
                }
                let event = SourceEvent::Resume {
                    task: name.clone(),
                    at_ns,
                    response: response_json,
                    writes: writes.iter().map(|(i, _, j)| (*i, j.clone())).collect(),
                    outcome,
                };
                if let Err(error) = self.event(event) {
                    token.payload_mut().owner = Owner::Parked(continuation, wait);
                    return Err(Fault::stall(token, error));
                }
                for (argument, value, _) in writes {
                    if let Err(message) = continuation.write_argument(argument, value) {
                        token.payload_mut().owner = Owner::Parked(continuation, wait);
                        return Err(Fault::stall(
                            token,
                            SessionError::ArgumentWrite {
                                task: name,
                                message,
                            },
                        ));
                    }
                }
                if let Wait::Model(request) = wait {
                    self.deliveries.remove(&request);
                }
                self.waits.remove(&id);
                continuation.resume(response)
            }
            (owner, _) => {
                token.payload_mut().owner = owner;
                return Err(Fault::stall(
                    token,
                    SessionError::UnexpectedWake { task: name },
                ));
            }
        };
        self.drive(token, index, execution)
    }

    fn dispatch_internal(&mut self, mut token: RunToken<TaskSlot<'w>>) -> Step<'w> {
        let Owner::Internal(internal, at_ns) =
            std::mem::replace(&mut token.payload_mut().owner, Owner::Empty)
        else {
            unreachable!("checked by dispatch")
        };
        match token.reason() {
            WakeReason::Start => {
                let delay = at_ns.saturating_sub(self.sim.now_ns());
                token.payload_mut().owner = Owner::Internal(internal, at_ns);
                self.sim.sleep(token, delay).map_err(stall)?;
                Ok(())
            }
            WakeReason::Timer(_) => {
                if let Err(error) = self.fire(&internal) {
                    token.payload_mut().owner = Owner::Internal(internal, at_ns);
                    return Err(Fault::stall(token, error));
                }
                self.sim.finish(token).map_err(stall)?;
                Ok(())
            }
            _ => {
                let task = internal.describe();
                token.payload_mut().owner = Owner::Internal(internal, at_ns);
                Err(Fault::stall(token, SessionError::UnexpectedWake { task }))
            }
        }
    }

    fn drive(
        &mut self,
        mut token: RunToken<TaskSlot<'w>>,
        index: usize,
        mut execution: Result<Execution<'w>, String>,
    ) -> Step<'w> {
        let id = token.task();
        let name = self.instances[index].name.clone();
        let spec = self.instances[index].spec;
        let epoch = self.instances[index].epoch;
        let world = self.program.world();
        loop {
            let at_ns = self.sim.now_ns();
            let continuation = match execution {
                Err(message) => {
                    return Err(Fault::Terminal(TerminalFailure {
                        task: name,
                        at_ns,
                        message,
                        trace: std::mem::take(&mut self.trace),
                        model_trace: std::mem::take(&mut self.model_trace),
                    }));
                }
                Ok(Execution::Complete(value)) => {
                    let held = self
                        .model
                        .as_ref()
                        .and_then(|m| m.warp.as_ref())
                        .map(|w| w.held_by(&name))
                        .unwrap_or_default();
                    if !held.is_empty() {
                        return Err(Fault::stall(
                            token,
                            SessionError::Conservation {
                                task: name,
                                message: format!(
                                    "the task finished while still holding UMEM frames {held:?}"
                                ),
                            },
                        ));
                    }
                    let value = match value::encode(world, &self.plan.tasks[spec].result, &value) {
                        Ok(value) => value,
                        Err(message) => {
                            return Err(Fault::stall(
                                token,
                                SessionError::OutputUnencodable {
                                    task: name,
                                    message,
                                },
                            ));
                        }
                    };
                    if let Err(error) = self.event(SourceEvent::Output {
                        task: name.clone(),
                        at_ns,
                        value: value.clone(),
                    }) {
                        return Err(Fault::stall(token, error));
                    }
                    self.instances[index].output = Some(value);
                    self.sim.finish(token).map_err(stall)?;
                    let remaining =
                        self.plan.tasks[spec].script.len() - self.instances[index].progress.script;
                    if remaining != 0 {
                        return Err(Fault::Interrupt(SessionError::UnconsumedScript {
                            task: name,
                            remaining,
                        }));
                    }
                    return Ok(());
                }
                Ok(Execution::Suspended(continuation)) => continuation,
            };
            let request = continuation.request();
            let key = (request.effect.name.clone(), request.operation.clone());
            let Some(bound) = self.plan.bindings.get(&key).cloned() else {
                token.payload_mut().owner = Owner::Routing(continuation);
                return Err(Fault::stall(
                    token,
                    SessionError::UnboundOperation {
                        task: name,
                        effect: key.0,
                        operation: key.1,
                    },
                ));
            };
            let arguments = if request.arguments.len() == bound.params.len() {
                bound
                    .params
                    .iter()
                    .zip(&request.arguments)
                    .map(|((_, ty), v)| value::encode(world, ty, v))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Json::Array)
            } else {
                Err("argument count differs from the signature".into())
            };
            let duration = match &bound.action {
                Action::Sleep { duration_argument } => {
                    match request.arguments[*duration_argument] {
                        Value::UInt(n, 64) => Some(n),
                        _ => None,
                    }
                }
                _ => None,
            };
            let progress = &mut self.instances[index].progress;
            progress.operations += 1;
            if progress.operations > self.plan.scenario().limits.max_operations_per_task {
                token.payload_mut().owner = Owner::Routing(continuation);
                return Err(Fault::stall(
                    token,
                    SessionError::OperationLimit { task: name },
                ));
            }
            let arguments = match arguments {
                Ok(arguments) => arguments,
                Err(message) => {
                    token.payload_mut().owner = Owner::Routing(continuation);
                    return Err(Fault::stall(
                        token,
                        SessionError::InvalidArguments {
                            task: name,
                            message: format!("{}: {message}", bound.signature),
                        },
                    ));
                }
            };
            if let Err(error) = self.event(SourceEvent::Operation {
                task: name.clone(),
                at_ns,
                effect: key.0.clone(),
                operation: key.1.clone(),
                arguments: arguments.clone(),
            }) {
                token.payload_mut().owner = Owner::Routing(continuation);
                return Err(Fault::stall(token, error));
            }
            let action = bound.action.clone();
            let mut spawn = vec![];
            let immediate = match &action {
                Action::Clock => Some(Value::UInt(at_ns, 64)),
                Action::CancelRequested => {
                    Some(Value::Bool(self.sim.cancel_requested(id).unwrap_or(false)))
                }
                action if action.is_model() => match self.model_op(index, &bound, &arguments) {
                    Ok(ModelStep::Immediate { response, internal }) => {
                        spawn = internal;
                        match value::decode(world, &bound.result, &response) {
                            Ok(value) => Some(value),
                            Err(message) => {
                                token.payload_mut().owner = Owner::Routing(continuation);
                                return Err(Fault::stall(
                                    token,
                                    SessionError::InvalidResponse {
                                        task: name,
                                        message,
                                    },
                                ));
                            }
                        }
                    }
                    Ok(ModelStep::Park {
                        request,
                        internal,
                        at_ns: fires_at,
                    }) => {
                        token.payload_mut().owner =
                            Owner::Parked(continuation, Wait::Model(request));
                        let completion = self.sim.park_io(token).map_err(stall)?;
                        self.waits.insert(
                            id,
                            PendingOperation {
                                task: name.clone(),
                                effect: key.0.clone(),
                                operation: key.1.clone(),
                                arguments: arguments.clone(),
                                external: false,
                                request,
                                epoch,
                                completion,
                                unscheduled: true,
                            },
                        );
                        self.spawn_internal(internal, fires_at)
                            .map_err(Fault::Interrupt)?;
                        return Ok(());
                    }
                    Err(error) => {
                        token.payload_mut().owner = Owner::Routing(continuation);
                        return Err(Fault::stall(token, error));
                    }
                },
                _ => None,
            };
            if let Some(response) = immediate {
                let response_json = value::encode(world, &bound.result, &response)
                    .expect("immediate responses match their validated signatures");
                for (internal, fires_at) in spawn {
                    if let Err(error) = self.spawn_internal(internal, fires_at) {
                        token.payload_mut().owner = Owner::Routing(continuation);
                        return Err(Fault::stall(token, error));
                    }
                }
                if let Err(error) = self.event(SourceEvent::Resume {
                    task: name.clone(),
                    at_ns,
                    response: response_json,
                    writes: vec![],
                    outcome: None,
                }) {
                    token.payload_mut().owner = Owner::Routing(continuation);
                    return Err(Fault::stall(token, error));
                }
                execution = continuation.resume(response);
                continue;
            }
            let request = self.next_request;
            let pending = |completion, external, unscheduled| PendingOperation {
                task: name.clone(),
                effect: key.0.clone(),
                operation: key.1.clone(),
                arguments: arguments.clone(),
                external,
                request,
                epoch,
                completion,
                unscheduled,
            };
            match action {
                Action::Yield => {
                    token.payload_mut().owner = Owner::Parked(continuation, Wait::Yield);
                    self.sim.yield_task(token).map_err(stall)?;
                }
                Action::Sleep { .. } => {
                    let Some(duration) = duration else {
                        token.payload_mut().owner = Owner::Routing(continuation);
                        return Err(Fault::stall(
                            token,
                            SessionError::InvalidArguments {
                                task: name,
                                message: "sleep duration is not a u64".into(),
                            },
                        ));
                    };
                    token.payload_mut().owner = Owner::Parked(continuation, Wait::Timer);
                    let completion = self.sim.sleep(token, duration).map_err(stall)?;
                    self.next_request += 1;
                    self.waits.insert(id, pending(completion, false, false));
                }
                Action::Script => {
                    let cursor = self.instances[index].progress.script;
                    let Some(step) = self.plan.tasks[spec].script.get(cursor) else {
                        token.payload_mut().owner = Owner::Routing(continuation);
                        return Err(Fault::stall(
                            token,
                            SessionError::MissingScript {
                                task: name,
                                effect: key.0,
                                operation: key.1,
                            },
                        ));
                    };
                    if step.effect != key.0
                        || step.operation != key.1
                        || step.arguments != arguments
                    {
                        let expected =
                            format!("{}::{}{}", step.effect, step.operation, step.arguments);
                        token.payload_mut().owner = Owner::Routing(continuation);
                        return Err(Fault::stall(
                            token,
                            SessionError::ScriptMismatch {
                                task: name,
                                index: cursor,
                                expected,
                                actual: format!("{}::{}{}", key.0, key.1, arguments),
                            },
                        ));
                    }
                    let (delay, result) = (step.delay_ns, step.outcome.io_result());
                    token.payload_mut().owner = Owner::Parked(continuation, Wait::Script(cursor));
                    let completion = self.sim.schedule_io(token, delay, result).map_err(stall)?;
                    self.next_request += 1;
                    self.instances[index].progress.script += 1;
                    self.waits.insert(id, pending(completion, false, false));
                    self.submitted(id, index)?;
                }
                Action::External => {
                    token.payload_mut().owner = Owner::Parked(continuation, Wait::External);
                    let completion = self.sim.park_io(token).map_err(stall)?;
                    self.next_request += 1;
                    self.waits.insert(id, pending(completion, true, true));
                    self.submitted(id, index)?;
                }
                _ => unreachable!("immediate actions resumed above"),
            }
            return Ok(());
        }
    }

    /// Apply the scenario's cancellation policy after a parked submission.
    fn submitted(&mut self, id: TaskId, index: usize) -> Step<'w> {
        let instance = &mut self.instances[index];
        instance.progress.submissions += 1;
        let task = &self.plan.tasks[instance.spec];
        if task.cancel_after == Some(instance.progress.submissions) {
            let event = SourceEvent::CancelRequested {
                task: instance.name.clone(),
                at_ns: self.sim.now_ns(),
            };
            self.event(event).map_err(Fault::Interrupt)?;
            self.sim
                .request_cancel(id)
                .map_err(|e| Fault::Interrupt(SessionError::Simulator(e)))?;
        }
        Ok(())
    }
}

fn recovered(owner: Owner<'_>) -> RecoveredOwner<'_> {
    match owner {
        Owner::Start(runtime) => RecoveredOwner::NotStarted(runtime),
        Owner::Parked(continuation, _) | Owner::Routing(continuation) => {
            RecoveredOwner::Suspended(continuation)
        }
        Owner::Internal(internal, _) => RecoveredOwner::Internal(internal.describe()),
        Owner::Empty => RecoveredOwner::None,
    }
}
