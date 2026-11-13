//! Model operations and model-internal timer tasks.
//!
//! Every step is computed on a copy of the model state. Its events are checked
//! against the event budget and, when replaying, the recorded model trace
//! before the copy is committed; only then does the step touch SimPulse
//! (completing parked waits or spawning timer tasks). A budget, misuse or
//! replay error therefore leaves the model and every owner unchanged.
use super::{LostTask, Session, SessionError, SourceEvent};
use crate::{
    model::{
        FailOp, Fault, LostState, ModelEvent, ModelPlan, ModelState, StaleSource,
        net::{Arrival, NetError, Sent},
        wal::WalError,
        warp::{WarpEffect, WarpError, WarpTimer},
    },
    scenario::{Action, BoundOperation, Outcome, canonical},
    value,
};
use serde_json::{Value as Json, json};
use vesper_simpulse::{CompletionId, IoResult, TaskState};

/// A model-internal SimPulse task. It sleeps until its time, fires once and
/// finishes. It never owns source state.
#[derive(Debug, Clone)]
pub(crate) enum Internal {
    Fault(usize),
    Deliver {
        network: String,
        message: u64,
    },
    Timeout {
        network: String,
        endpoint: u64,
        request: u64,
        epoch: u32,
    },
    Flush {
        log: String,
        request: u64,
        generation: u64,
        epoch: u32,
        fails: Option<u32>,
    },
    Warp(WarpTimer),
}
impl Internal {
    pub fn describe(&self) -> String {
        match self {
            Self::Fault(i) => format!("model:fault#{i}"),
            Self::Deliver { network, message } => format!("model:{network}:deliver#{message}"),
            Self::Timeout {
                network, request, ..
            } => format!("model:{network}:timeout#{request}"),
            Self::Flush { log, request, .. } => format!("model:{log}:flush#{request}"),
            Self::Warp(timer) => format!("model:{}", timer.describe()),
        }
    }
}

pub(crate) enum ModelStep {
    /// Resume at once with this response, optionally starting a timer task.
    Immediate {
        response: Json,
        internal: Vec<(Internal, u64)>,
    },
    /// Park on `request` until the timer task fires.
    Park {
        request: u64,
        internal: Internal,
        at_ns: u64,
    },
}

enum SimAction {
    Complete(CompletionId, IoResult),
    SpawnInternal(Internal, u64),
    SpawnTask {
        spec: usize,
        name: String,
        epoch: u32,
    },
}

fn payload(json: &Json) -> String {
    canonical(json)
}
fn parse(payload: &str) -> Json {
    serde_json::from_str(payload).expect("model payloads are canonical JSON")
}

/// Count this operation and return its injected failure code, if any.
fn injected(
    model: &mut ModelState,
    plan: &ModelPlan,
    target: &str,
    op: FailOp,
) -> (u64, Option<u32>) {
    let count = model.occurrences.entry((target.into(), op)).or_insert(0);
    *count += 1;
    let occurrence = *count;
    (
        occurrence,
        plan.failures.get(&(target.into(), op, occurrence)).copied(),
    )
}

fn warp_error(task: &str, error: WarpError) -> SessionError {
    match error {
        WarpError::Misuse(message) => SessionError::Model {
            task: task.into(),
            message,
        },
        WarpError::Budget(message) => SessionError::ModelBudget {
            task: task.into(),
            message,
        },
        WarpError::Conservation(message) => SessionError::Conservation {
            task: task.into(),
            message,
        },
    }
}

impl<'w> Session<'w> {
    /// One model operation. The Warp NICs are shared by every copy of the
    /// model state, so a step that fails is rolled back on them explicitly
    /// (the rest of the copy is simply dropped).
    pub(super) fn model_op(
        &mut self,
        index: usize,
        bound: &BoundOperation,
        arguments: &Json,
    ) -> Result<ModelStep, SessionError> {
        let result = self.model_op_step(index, bound, arguments);
        self.settle_warp(result.is_ok());
        result
    }

    /// A model-internal timer task fired; see [`Self::model_op`].
    pub(super) fn fire(&mut self, internal: &Internal) -> Result<(), SessionError> {
        let result = self.fire_step(internal);
        self.settle_warp(result.is_ok());
        result
    }

    fn settle_warp(&self, committed: bool) {
        if let Some(warp) = self.model.as_ref().and_then(|m| m.warp.as_ref()) {
            match committed {
                true => warp.commit(),
                false => warp.rollback(),
            }
        }
    }

    fn model_op_step(
        &mut self,
        index: usize,
        bound: &BoundOperation,
        arguments: &Json,
    ) -> Result<ModelStep, SessionError> {
        let now = self.sim.now_ns();
        let instance = &self.instances[index];
        let task = instance.name.clone();
        let epoch = instance.epoch;
        let process = instance.process.expect("model tasks belong to a process");
        let plan = self
            .plan
            .model
            .as_ref()
            .expect("model actions imply a model");
        let mut model = self.model.clone().expect("model actions imply a model");
        let args = arguments.as_array().expect("arguments are an array");
        let misuse = |target: &str, e: WalError| SessionError::Model {
            task: task.clone(),
            message: format!("{target}: {e:?}"),
        };
        let budget = |target: &str, e: String| SessionError::ModelBudget {
            task: task.clone(),
            message: format!("{target}: {e}"),
        };
        let request = self.next_request;
        let mut uses_request = false;
        let mut events = vec![];
        let step = match &bound.action {
            Action::WalRecover { log } => {
                let wal = &mut model.logs.get_mut(log).unwrap().wal;
                let recovery = wal.recover().map_err(|e| misuse(log, e))?;
                events.push(ModelEvent::WalRecovered {
                    log: log.clone(),
                    task: task.clone(),
                    epoch,
                    at_ns: now,
                    records: recovery.records.len() as u64,
                    valid_bytes: recovery.valid_bytes as u64,
                    discarded_bytes: recovery.discarded_bytes as u64,
                    damage: recovery.damage,
                });
                ModelStep::Immediate {
                    response: json!(recovery.records.len()),
                    internal: vec![],
                }
            }
            Action::WalAppend { log } => {
                let record = payload(&args[0]);
                let state = model.logs.get_mut(log).unwrap();
                state.wal.check_append(&record).map_err(|e| match e {
                    WalError::RecordTooLarge { .. } | WalError::LogFull { .. } => {
                        budget(log, format!("{e:?}"))
                    }
                    e => misuse(log, e),
                })?;
                let (occurrence, fails) = injected(&mut model, plan, log, FailOp::Append);
                let response = match fails {
                    Some(code) => {
                        events.push(ModelEvent::InjectedFailure {
                            target: log.clone(),
                            operation: FailOp::Append,
                            occurrence,
                            code,
                            task: task.clone(),
                            at_ns: now,
                        });
                        json!({ "Err": code })
                    }
                    None => {
                        let bytes = record.len() as u64;
                        let wal = &mut model.logs.get_mut(log).unwrap().wal;
                        let lsn = wal.append(record).map_err(|e| misuse(log, e))?;
                        events.push(ModelEvent::WalAppended {
                            log: log.clone(),
                            task: task.clone(),
                            lsn,
                            bytes,
                            at_ns: now,
                        });
                        json!({ "Ok": lsn })
                    }
                };
                ModelStep::Immediate {
                    response,
                    internal: vec![],
                }
            }
            Action::WalFlush { log } => {
                model.logs[log].wal.check_flush().map_err(|e| match e {
                    WalError::FlushInFlight => SessionError::Model {
                        task: task.clone(),
                        message: format!(
                            "{log}: a second concurrent flush is unsupported by wal-frame-v1"
                        ),
                    },
                    e => misuse(log, e),
                })?;
                let (occurrence, fails) = injected(&mut model, plan, log, FailOp::Flush);
                let state = model.logs.get_mut(log).unwrap();
                let complete_at_ns = now.checked_add(state.flush_delay_ns).ok_or_else(|| {
                    budget(log, "flush completion overflows the virtual clock".into())
                })?;
                let generation = state.wal.generation();
                let batch = state.wal.begin_flush(request).map_err(|e| misuse(log, e))?;
                let lsns = batch.records.iter().map(|r| r.lsn).collect();
                uses_request = true;
                if let Some(code) = fails {
                    events.push(ModelEvent::InjectedFailure {
                        target: log.clone(),
                        operation: FailOp::Flush,
                        occurrence,
                        code,
                        task: task.clone(),
                        at_ns: now,
                    });
                }
                events.push(ModelEvent::WalFlushSubmitted {
                    log: log.clone(),
                    task: task.clone(),
                    request,
                    lsns,
                    complete_at_ns,
                    at_ns: now,
                });
                ModelStep::Park {
                    request,
                    internal: Internal::Flush {
                        log: log.clone(),
                        request,
                        generation,
                        epoch,
                        fails,
                    },
                    at_ns: complete_at_ns,
                }
            }
            Action::WalRead { log } => {
                let wal = &model.logs[log].wal;
                if !wal.is_recovered() {
                    return Err(misuse(log, WalError::NotRecovered));
                }
                let index = args[0].as_u64().expect("u64 index");
                let record = usize::try_from(index).ok().and_then(|i| wal.view().get(i));
                events.push(ModelEvent::WalRead {
                    log: log.clone(),
                    task: task.clone(),
                    index,
                    lsn: record.map(|r| r.lsn),
                    at_ns: now,
                });
                ModelStep::Immediate {
                    response: match record {
                        Some(r) => json!({ "Some": parse(&r.payload) }),
                        None => json!("None"),
                    },
                    internal: vec![],
                }
            }
            Action::NetSend { network } => {
                let to = args[0].as_u64().expect("u64 destination");
                let message = payload(&args[1]);
                let from = plan.spec.processes[process].endpoints[network];
                let (occurrence, fails) = injected(&mut model, plan, network, FailOp::Send);
                let state = &model.networks[network];
                let mut internal = None;
                let response = if let Some(code) = fails {
                    events.push(ModelEvent::InjectedFailure {
                        target: network.clone(),
                        operation: FailOp::Send,
                        occurrence,
                        code,
                        task: task.clone(),
                        at_ns: now,
                    });
                    json!({ "Err": code })
                } else if !state.net.has_endpoint(to) {
                    events.push(ModelEvent::NetUnreachable {
                        network: network.clone(),
                        task: task.clone(),
                        to,
                        at_ns: now,
                    });
                    json!({ "Err": state.unreachable_code })
                } else {
                    let up = model.processes[state.owners[&to]].up;
                    let bytes = message.len() as u64;
                    let state = model.networks.get_mut(network).unwrap();
                    let sent = state
                        .net
                        .send(from, to, message, now, up)
                        .map_err(|e: NetError| budget(network, format!("{e:?}")))?;
                    let (m, dropped) = match sent {
                        Sent::InFlight(m) => (m, None),
                        Sent::Dropped(m, reason) => (m, Some(reason)),
                    };
                    events.push(ModelEvent::NetSent {
                        network: network.clone(),
                        task: task.clone(),
                        message: m.id,
                        from,
                        to,
                        bytes,
                        deliver_at_ns: m.deliver_at_ns,
                        at_ns: now,
                    });
                    match dropped {
                        Some(reason) => events.push(ModelEvent::NetDropped {
                            network: network.clone(),
                            message: m.id,
                            reason,
                            at_ns: now,
                        }),
                        None => {
                            internal = Some((
                                Internal::Deliver {
                                    network: network.clone(),
                                    message: m.id,
                                },
                                m.deliver_at_ns,
                            ))
                        }
                    }
                    json!({ "Ok": m.id })
                };
                ModelStep::Immediate {
                    response,
                    internal: internal.into_iter().collect(),
                }
            }
            Action::NetRecv { network } => {
                let timeout = args[0].as_u64().expect("u64 timeout");
                let endpoint = plan.spec.processes[process].endpoints[network];
                let state = model.networks.get_mut(network).unwrap();
                if let Some(m) = state.net.take_queued(endpoint) {
                    events.push(ModelEvent::NetTaken {
                        network: network.clone(),
                        task: task.clone(),
                        message: m.id,
                        at_ns: now,
                    });
                    ModelStep::Immediate {
                        response: json!({ "Ok": parse(&m.payload) }),
                        internal: vec![],
                    }
                } else if timeout == 0 {
                    uses_request = true;
                    events.push(ModelEvent::RecvTimedOut {
                        network: network.clone(),
                        request,
                        at_ns: now,
                    });
                    ModelStep::Immediate {
                        response: json!({ "Err": state.timeout_code }),
                        internal: vec![],
                    }
                } else {
                    let timeout_at_ns = now.checked_add(timeout).ok_or_else(|| {
                        budget(
                            network,
                            "receive timeout overflows the virtual clock".into(),
                        )
                    })?;
                    uses_request = true;
                    state.net.wait(endpoint, request);
                    events.push(ModelEvent::RecvWaiting {
                        network: network.clone(),
                        task: task.clone(),
                        request,
                        timeout_at_ns,
                        at_ns: now,
                    });
                    ModelStep::Park {
                        request,
                        internal: Internal::Timeout {
                            network: network.clone(),
                            endpoint,
                            request,
                            epoch,
                        },
                        at_ns: timeout_at_ns,
                    }
                }
            }
            action if action.warp().is_some() => {
                let (op, nic) = action.warp().expect("warp action");
                let warp = model.warp.as_mut().expect("xdp actions imply a warp model");
                let result = warp
                    .op(nic, op, &task, args, now, request)
                    .map_err(|e| warp_error(&task, e))?;
                events.extend(result.events.into_iter().map(ModelEvent::Warp));
                let spawns = result
                    .effects
                    .into_iter()
                    .map(|effect| match effect {
                        WarpEffect::Spawn(timer, at) => (Internal::Warp(timer), at),
                        WarpEffect::Wake { .. } => unreachable!("operations never wake waiters"),
                    })
                    .collect();
                match (result.response, result.park) {
                    (Some(response), _) => ModelStep::Immediate {
                        response,
                        internal: spawns,
                    },
                    (None, Some((timer, at_ns))) => {
                        uses_request = true;
                        ModelStep::Park {
                            request,
                            internal: Internal::Warp(timer),
                            at_ns,
                        }
                    }
                    (None, None) => unreachable!("an operation resumes or parks"),
                }
            }
            _ => unreachable!("not a model action"),
        };
        // An immediate step resumes the source right after committing, so its
        // Resume event is checked against the budget and the recorded trace
        // now, together with the model events, before anything is committed.
        if let ModelStep::Immediate { response, .. } = &step {
            let world = self.program.world();
            let invalid = |message| SessionError::InvalidResponse {
                task: task.clone(),
                message,
            };
            let value = value::decode(world, &bound.result, response).map_err(invalid)?;
            let response = value::encode(world, &bound.result, &value).map_err(invalid)?;
            self.check_source(
                &SourceEvent::Resume {
                    task: task.clone(),
                    at_ns: now,
                    response,
                    writes: vec![],
                    outcome: None,
                },
                events.len(),
            )?;
        }
        self.check_model(&events)?;
        self.model = Some(model);
        self.settle_warp(true);
        self.commit_model(events);
        if uses_request {
            self.next_request += 1;
        }
        Ok(step)
    }

    fn fire_step(&mut self, internal: &Internal) -> Result<(), SessionError> {
        let now = self.sim.now_ns();
        let plan = self
            .plan
            .model
            .as_ref()
            .expect("internal tasks imply a model");
        let mut model = self.model.clone().expect("internal tasks imply a model");
        let mut events = vec![];
        let mut actions = vec![];
        let mut deliveries = vec![];
        let mut lost = vec![];
        let mut crashed_process = None;
        // Find the live parked wait for a request.
        let completion_of = |request: u64| {
            self.waits
                .values()
                .find(|p| p.request == request)
                .map(|p| p.completion)
        };
        match internal {
            Internal::Deliver { network, message } => {
                let state = &model.networks[network];
                let to = state.net.in_flight()[message].to;
                let up = model.processes[state.owners[&to]].up;
                let state = model.networks.get_mut(network).unwrap();
                match state.net.arrive(*message, up).expect("in-flight message") {
                    Arrival::Delivered(m, request) => {
                        let completion =
                            completion_of(request).expect("waiting receivers are live requests");
                        events.push(ModelEvent::NetDelivered {
                            network: network.clone(),
                            message: m.id,
                            request,
                            at_ns: now,
                        });
                        let bytes = m.payload.len() as u64;
                        deliveries.push((
                            request,
                            Outcome::Success { bytes },
                            json!({ "Ok": parse(&m.payload) }),
                        ));
                        actions.push(SimAction::Complete(completion, IoResult::Success { bytes }));
                    }
                    Arrival::Queued(m) => events.push(ModelEvent::NetQueued {
                        network: network.clone(),
                        message: m.id,
                        at_ns: now,
                    }),
                    Arrival::Dropped(m, reason) => events.push(ModelEvent::NetDropped {
                        network: network.clone(),
                        message: m.id,
                        reason,
                        at_ns: now,
                    }),
                }
            }
            Internal::Timeout {
                network,
                endpoint,
                request,
                epoch,
            } => {
                let state = model.networks.get_mut(network).unwrap();
                if state.net.cancel_wait(*endpoint, *request) {
                    let completion =
                        completion_of(*request).expect("waiting receivers are live requests");
                    let code = state.timeout_code;
                    events.push(ModelEvent::RecvTimedOut {
                        network: network.clone(),
                        request: *request,
                        at_ns: now,
                    });
                    deliveries.push((
                        *request,
                        Outcome::Error { code: code as i32 },
                        json!({ "Err": code }),
                    ));
                    actions.push(SimAction::Complete(
                        completion,
                        IoResult::Error { code: code as i32 },
                    ));
                } else {
                    // Already answered by a delivery, or its epoch crashed.
                    events.push(ModelEvent::StaleRejected {
                        source: StaleSource::RecvTimeout,
                        request: *request,
                        epoch: *epoch,
                        at_ns: now,
                    });
                }
            }
            Internal::Flush {
                log,
                request,
                generation,
                epoch,
                fails,
            } => {
                let wal = &mut model.logs.get_mut(log).unwrap().wal;
                if wal.generation() != *generation {
                    events.push(ModelEvent::StaleRejected {
                        source: StaleSource::FlushCompletion,
                        request: *request,
                        epoch: *epoch,
                        at_ns: now,
                    });
                } else {
                    let completion =
                        completion_of(*request).expect("an in-flight flush has a live flusher");
                    match fails {
                        Some(code) => {
                            wal.fail_flush().expect("flush in flight");
                            events.push(ModelEvent::WalFlushFailed {
                                log: log.clone(),
                                request: *request,
                                code: *code,
                                at_ns: now,
                            });
                            let code = *code;
                            deliveries.push((
                                *request,
                                Outcome::Error { code: code as i32 },
                                json!({ "Err": code }),
                            ));
                            actions.push(SimAction::Complete(
                                completion,
                                IoResult::Error { code: code as i32 },
                            ));
                        }
                        None => {
                            let batch = wal.complete_flush().expect("flush in flight");
                            let bytes: u64 =
                                batch.records.iter().map(|r| r.frame_len() as u64).sum();
                            let durable = wal.durable_bytes();
                            events.push(ModelEvent::WalFlushed {
                                log: log.clone(),
                                request: *request,
                                durable_lsn: wal.durable_lsn(),
                                durable_bytes: durable.len() as u64,
                                durable_sha256: crate::program::sha256_hex(durable),
                                at_ns: now,
                            });
                            deliveries.push((
                                *request,
                                Outcome::Success { bytes },
                                json!({ "Ok": wal.durable_lsn() }),
                            ));
                            actions
                                .push(SimAction::Complete(completion, IoResult::Success { bytes }));
                        }
                    }
                }
            }
            Internal::Warp(timer) => {
                let spec = plan
                    .spec
                    .warp
                    .as_ref()
                    .expect("warp timers imply a warp model");
                let warp = model.warp.as_mut().expect("warp timers imply a warp model");
                let (warp_events, effects) = warp
                    .fire(spec, timer, now)
                    .map_err(|e| warp_error(&internal.describe(), e))?;
                events.extend(warp_events.into_iter().map(ModelEvent::Warp));
                for effect in effects {
                    match effect {
                        WarpEffect::Spawn(timer, at) => {
                            actions.push(SimAction::SpawnInternal(Internal::Warp(timer), at))
                        }
                        WarpEffect::Wake { request, rx_len } => {
                            let completion =
                                completion_of(request).expect("RX waiters are live requests");
                            let bytes = u64::from(rx_len);
                            deliveries.push((request, Outcome::Success { bytes }, json!(rx_len)));
                            actions
                                .push(SimAction::Complete(completion, IoResult::Success { bytes }));
                        }
                    }
                }
            }
            Internal::Fault(index) => match &plan.spec.faults[*index].fault {
                Fault::Crash { process, in_flight } => {
                    let p = plan.process_index[process];
                    if !model.processes[p].up {
                        return Err(SessionError::Model {
                            task: internal.describe(),
                            message: format!("process {process} is already down"),
                        });
                    }
                    if model.crashes >= plan.spec.limits.max_epochs {
                        return Err(SessionError::EpochLimit);
                    }
                    model.crashes += 1;
                    let epoch = model.processes[p].epoch;
                    model.processes[p].up = false;
                    crashed_process = Some(p);
                    events.push(ModelEvent::Crash {
                        process: process.clone(),
                        epoch,
                        at_ns: now,
                    });
                    for (name, state) in model.logs.iter_mut().filter(|(_, l)| l.process == p) {
                        let choice = in_flight.get(name).copied().unwrap_or_default();
                        let applied = state.wal.crash(choice.persisted_records, choice.torn_bytes);
                        let durable = state.wal.durable_bytes();
                        events.push(ModelEvent::WalCrash {
                            log: name.clone(),
                            at_ns: now,
                            choice,
                            applied,
                            durable_bytes: durable.len() as u64,
                            durable_sha256: crate::program::sha256_hex(durable),
                        });
                    }
                    for (name, endpoint) in &plan.spec.processes[p].endpoints {
                        let (inbox, _) = model.networks.get_mut(name).unwrap().net.crash(*endpoint);
                        if !inbox.is_empty() {
                            events.push(ModelEvent::InboxDiscarded {
                                network: name.clone(),
                                endpoint: *endpoint,
                                messages: inbox.iter().map(|m| m.id).collect(),
                                at_ns: now,
                            });
                        }
                    }
                    for (index, instance) in self.instances.iter().enumerate() {
                        if instance.process != Some(p) || instance.lost || instance.output.is_some()
                        {
                            continue;
                        }
                        let pending = self.waits.get(&instance.id);
                        let state = match (pending, instance.started) {
                            (Some(p), _) => LostState::Parked {
                                effect: p.effect.clone(),
                                operation: p.operation.clone(),
                                request: Some(p.request),
                            },
                            (None, true) => LostState::Ready,
                            (None, false) => LostState::NotStarted,
                        };
                        events.push(ModelEvent::TaskLost {
                            task: instance.name.clone(),
                            epoch,
                            at_ns: now,
                            state: state.clone(),
                        });
                        // Release only a wait SimPulse still holds unscheduled;
                        // one already completed is retired at its dispatch.
                        let waiting = |p: &&super::PendingOperation| {
                            p.unscheduled
                                && self.sim.task_state(instance.id)
                                    == Some(TaskState::WaitingIo {
                                        completion: p.completion,
                                    })
                        };
                        if let Some(p) = pending.filter(waiting) {
                            events.push(ModelEvent::WaitReleased {
                                task: instance.name.clone(),
                                request: p.request,
                                at_ns: now,
                            });
                            actions.push(SimAction::Complete(p.completion, IoResult::Cancelled));
                        }
                        lost.push((
                            index,
                            LostTask {
                                task: instance.name.clone(),
                                process: process.clone(),
                                epoch,
                                at_ns: now,
                                state,
                            },
                        ));
                    }
                }
                Fault::Restart { process } => {
                    let p = plan.process_index[process];
                    if model.processes[p].up {
                        return Err(SessionError::Model {
                            task: internal.describe(),
                            message: format!("process {process} is already up"),
                        });
                    }
                    let epoch = model.processes[p].epoch + 1;
                    model.processes[p] = crate::model::ProcessState {
                        name: process.clone(),
                        epoch,
                        up: true,
                    };
                    let specs = &self.plan.restarts[p];
                    if (self.instances.len() + specs.len()) as u64 > plan.spec.limits.max_instances
                    {
                        return Err(SessionError::InstanceLimit);
                    }
                    let mut tasks = vec![];
                    for spec in specs {
                        let name = format!("{}@{epoch}", self.plan.tasks[*spec].name);
                        tasks.push(name.clone());
                        actions.push(SimAction::SpawnTask {
                            spec: *spec,
                            name,
                            epoch,
                        });
                    }
                    events.push(ModelEvent::Restart {
                        process: process.clone(),
                        epoch,
                        at_ns: now,
                        tasks,
                    });
                }
                Fault::Partition { network, a, b } => {
                    let changed = model
                        .networks
                        .get_mut(network)
                        .unwrap()
                        .net
                        .partition(*a, *b);
                    events.push(ModelEvent::Partitioned {
                        network: network.clone(),
                        a: *a,
                        b: *b,
                        changed,
                        at_ns: now,
                    });
                }
                Fault::Heal { network, a, b } => {
                    let changed = model.networks.get_mut(network).unwrap().net.heal(*a, *b);
                    events.push(ModelEvent::Healed {
                        network: network.clone(),
                        a: *a,
                        b: *b,
                        changed,
                        at_ns: now,
                    });
                }
            },
        }
        self.check_model(&events)?;
        // Commit the model step, then apply its SimPulse side effects.
        self.model = Some(model);
        self.settle_warp(true);
        self.commit_model(events);
        for (request, outcome, json) in deliveries {
            self.deliveries.insert(request, (outcome, json));
        }
        if crashed_process.is_some() {
            for (index, record) in lost {
                let instance = &mut self.instances[index];
                instance.lost = true;
                if let Some(p) = self.waits.remove(&instance.id) {
                    self.stale.insert(p.request, p.epoch);
                    self.deliveries.remove(&p.request);
                    self.external.remove(&p.completion);
                }
                self.lost.push(record);
            }
        }
        for action in actions {
            match action {
                SimAction::Complete(completion, result) => self
                    .sim
                    .complete_io(completion, result)
                    .map_err(SessionError::Simulator)?,
                SimAction::SpawnTask { spec, name, epoch } => {
                    self.spawn_instance(spec, name, epoch)?
                }
                SimAction::SpawnInternal(internal, at_ns) => {
                    self.spawn_internal(internal, at_ns)?
                }
            }
        }
        Ok(())
    }
}
