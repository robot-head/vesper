//! Bounded deterministic process, storage and network models for scenario
//! version 2.
//!
//! The models are plain data driven by typed source operations through the
//! session adapter. Model semantics are versioned by [`MODEL_SEMANTICS`] and
//! bound into every recording's identity:
//! - `process-epochs-v1`: a process crash ends the current epoch of one named
//!   process. Every unfinished task of that process becomes explicitly lost
//!   process state (never resumed and never reported as source cancellation),
//!   its volatile storage and inbox vanish, and completions addressed to the
//!   old epoch are rejected as stale. A restart begins the next epoch and
//!   spawns the process's restart tasks.
//! - `wal-frame-v1`: see [`wal`].
//! - `net-v1`: see [`net`].
//! - `umem-v1` and `datagram-v1` (scenario version 3 only): see [`warp`],
//!   [`umem`] and [`link`].
//!
//! Every fault, failure, crash choice, recovery and model transition is a
//! [`ModelEvent`] in a model trace kept separate from the SimPulse v1 journal.
pub mod link;
pub mod net;
pub mod umem;
pub mod wal;
pub mod warp;

use crate::scenario::TaskSpec;
use net::{DropReason, Message, NetLimits, Network};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use wal::{CrashApplied, TailDamage, Wal, WalLimits, WalRecord};

pub const MODEL_SEMANTICS: &str = "vesper-sim-models-v1(process-epochs-v1,wal-frame-v1,net-v1)";
/// Model semantics of scenario version 3 (with a `warp` section).
pub const WARP_MODEL_SEMANTICS: &str =
    "vesper-sim-models-v2(process-epochs-v1,wal-frame-v1,net-v1,umem-v1,datagram-v1)";

/// Hard upper bounds applied regardless of model limits.
pub const MAX_PROCESSES: usize = 16;
pub const MAX_LOGS: usize = 16;
pub const MAX_NETWORKS: usize = 8;
pub const MAX_FAULTS: usize = 256;
pub const MAX_FAILURES: usize = 256;
pub const MAX_EVENTS: u64 = 1_000_000;
pub const MAX_EPOCHS: u32 = 64;
pub const MAX_INSTANCES: u64 = 256;
pub const MAX_LOG_BYTES: u64 = 1 << 20;
pub const MAX_LOG_RECORDS: u64 = 1 << 16;
pub const MAX_QUEUE: u64 = 4096;
pub const MAX_MESSAGE_BYTES: u64 = 1 << 16;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    pub limits: ModelLimits,
    #[serde(default)]
    pub processes: Vec<ProcessSpec>,
    #[serde(default)]
    pub logs: Vec<LogSpec>,
    #[serde(default)]
    pub networks: Vec<NetworkSpec>,
    #[serde(default)]
    pub faults: Vec<TimedFault>,
    #[serde(default)]
    pub failures: Vec<FailureSpec>,
    /// Required by scenario version 3 and rejected by version 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<warp::WarpSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLimits {
    /// Source plus model trace events for the whole session.
    pub max_events: u64,
    /// Process crashes for the whole session.
    pub max_epochs: u32,
    /// Source task instances, including every restarted task.
    pub max_instances: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSpec {
    pub name: String,
    /// The process's address on each named network.
    #[serde(default)]
    pub endpoints: BTreeMap<String, u64>,
    /// Tasks spawned at every restart, named `<name>@<epoch>`.
    #[serde(default)]
    pub restart: Vec<TaskSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogSpec {
    pub name: String,
    /// The only process whose tasks may use this log.
    pub process: String,
    pub flush_delay_ns: u64,
    pub limits: WalLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    pub name: String,
    pub delay_ns: u64,
    /// Directional per-link delays overriding `delay_ns`.
    #[serde(default)]
    pub links: Vec<LinkSpec>,
    pub max_in_flight: u64,
    pub max_inbox: u64,
    pub max_message_bytes: u64,
    /// `Err` code a receive returns when its timeout elapses.
    pub timeout_code: u32,
    /// `Err` code a send returns for an unknown destination.
    pub unreachable_code: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkSpec {
    pub from: u64,
    pub to: u64,
    pub delay_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimedFault {
    /// Absolute virtual time at which the fault fires.
    pub at_ns: u64,
    pub fault: Fault,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Fault {
    Crash {
        process: String,
        /// Per-log choice of what an in-flight flush leaves on the medium.
        /// Logs not listed persist nothing of their in-flight batch.
        #[serde(default)]
        in_flight: BTreeMap<String, CrashChoice>,
    },
    Restart {
        process: String,
    },
    Partition {
        network: String,
        a: u64,
        b: u64,
    },
    Heal {
        network: String,
        a: u64,
        b: u64,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrashChoice {
    /// Whole in-flight records that reach the medium, oldest first.
    pub persisted_records: u64,
    /// Bytes of the next record's frame that also reach the medium.
    pub torn_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailOp {
    Append,
    Flush,
    Send,
}

/// Fail the `occurrence`-th (1-based, whole session) `operation` on `target`
/// with `Err(code)`. Flush failures are delivered after the flush delay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureSpec {
    pub target: String,
    pub operation: FailOp,
    pub occurrence: u64,
    pub code: u32,
}

/// What a lost task was doing when its process crashed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LostState {
    NotStarted,
    Ready,
    Parked {
        effect: String,
        operation: String,
        request: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleSource {
    FlushCompletion,
    RecvTimeout,
    HostCompletion,
}

/// One model transition or choice, recorded before it takes effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelEvent {
    Crash {
        process: String,
        epoch: u32,
        at_ns: u64,
    },
    TaskLost {
        task: String,
        epoch: u32,
        at_ns: u64,
        state: LostState,
    },
    /// A crashed task's unscheduled SimPulse wait was released so the kernel
    /// can retire it; its continuation is never resumed.
    WaitReleased {
        task: String,
        request: u64,
        at_ns: u64,
    },
    InboxDiscarded {
        network: String,
        endpoint: u64,
        messages: Vec<u64>,
        at_ns: u64,
    },
    WalCrash {
        log: String,
        at_ns: u64,
        choice: CrashChoice,
        applied: CrashApplied,
        durable_bytes: u64,
        durable_sha256: String,
    },
    Restart {
        process: String,
        epoch: u32,
        at_ns: u64,
        tasks: Vec<String>,
    },
    WalRecovered {
        log: String,
        task: String,
        epoch: u32,
        at_ns: u64,
        records: u64,
        valid_bytes: u64,
        discarded_bytes: u64,
        damage: Option<TailDamage>,
    },
    WalAppended {
        log: String,
        task: String,
        lsn: u64,
        bytes: u64,
        at_ns: u64,
    },
    WalFlushSubmitted {
        log: String,
        task: String,
        request: u64,
        lsns: Vec<u64>,
        complete_at_ns: u64,
        at_ns: u64,
    },
    WalFlushed {
        log: String,
        request: u64,
        durable_lsn: u64,
        durable_bytes: u64,
        durable_sha256: String,
        at_ns: u64,
    },
    WalFlushFailed {
        log: String,
        request: u64,
        code: u32,
        at_ns: u64,
    },
    WalRead {
        log: String,
        task: String,
        index: u64,
        lsn: Option<u64>,
        at_ns: u64,
    },
    InjectedFailure {
        target: String,
        operation: FailOp,
        occurrence: u64,
        code: u32,
        task: String,
        at_ns: u64,
    },
    NetSent {
        network: String,
        task: String,
        message: u64,
        from: u64,
        to: u64,
        bytes: u64,
        deliver_at_ns: u64,
        at_ns: u64,
    },
    NetUnreachable {
        network: String,
        task: String,
        to: u64,
        at_ns: u64,
    },
    NetDropped {
        network: String,
        message: u64,
        reason: DropReason,
        at_ns: u64,
    },
    NetDelivered {
        network: String,
        message: u64,
        request: u64,
        at_ns: u64,
    },
    NetQueued {
        network: String,
        message: u64,
        at_ns: u64,
    },
    NetTaken {
        network: String,
        task: String,
        message: u64,
        at_ns: u64,
    },
    RecvWaiting {
        network: String,
        task: String,
        request: u64,
        timeout_at_ns: u64,
        at_ns: u64,
    },
    RecvTimedOut {
        network: String,
        request: u64,
        at_ns: u64,
    },
    Partitioned {
        network: String,
        a: u64,
        b: u64,
        changed: bool,
        at_ns: u64,
    },
    Healed {
        network: String,
        a: u64,
        b: u64,
        changed: bool,
        at_ns: u64,
    },
    StaleRejected {
        source: StaleSource,
        request: u64,
        epoch: u32,
        at_ns: u64,
    },
    /// A Warp (UMEM, datagram link or flow) transition.
    Warp(warp::WarpEvent),
}

/// Exact durable bytes of each log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableLog {
    pub bytes_hex: String,
    pub sha256: String,
}
pub type DurableState = BTreeMap<String, DurableLog>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub name: String,
    pub epoch: u32,
    pub up: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSnapshot {
    pub process: String,
    pub recovered: bool,
    pub durable_bytes: u64,
    pub durable_sha256: String,
    /// What recovery would accept from the medium right now.
    pub recoverable: Vec<WalRecord>,
    pub damage: Option<TailDamage>,
    pub in_flight_lsns: Vec<u64>,
    pub volatile_lsns: Vec<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSnapshot {
    pub partitions: Vec<(u64, u64)>,
    pub in_flight: Vec<Message>,
    pub inboxes: BTreeMap<u64, Vec<Message>>,
    pub waiting: BTreeMap<u64, Vec<u64>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSnapshot {
    pub at_ns: u64,
    pub processes: Vec<ProcessSnapshot>,
    pub logs: BTreeMap<String, LogSnapshot>,
    pub networks: BTreeMap<String, NetworkSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<warp::WarpSnapshot>,
}

/// A model spec checked for internal consistency (bindings and tasks are
/// checked against the source world by the scenario validator).
#[derive(Debug, Clone)]
pub(crate) struct ModelPlan {
    pub spec: ModelSpec,
    pub process_index: BTreeMap<String, usize>,
    pub failures: BTreeMap<(String, FailOp, u64), u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProcessState {
    pub name: String,
    pub epoch: u32,
    pub up: bool,
}
#[derive(Debug, Clone)]
pub(crate) struct LogState {
    pub wal: Wal,
    pub process: usize,
    pub flush_delay_ns: u64,
}
#[derive(Debug, Clone)]
pub(crate) struct NetState {
    pub net: Network,
    pub timeout_code: u32,
    pub unreachable_code: u32,
    /// Endpoint to owning process.
    pub owners: BTreeMap<u64, usize>,
}

/// Mutable model state. It holds no runtime owners, so a step can be computed
/// on a copy, checked against budgets and the expected trace, then committed.
#[derive(Debug, Clone)]
pub(crate) struct ModelState {
    pub processes: Vec<ProcessState>,
    pub logs: BTreeMap<String, LogState>,
    pub networks: BTreeMap<String, NetState>,
    pub occurrences: BTreeMap<(String, FailOp), u64>,
    pub crashes: u32,
    pub warp: Option<warp::WarpState>,
}

impl ModelState {
    pub fn new(plan: &ModelPlan, seed: u64) -> Self {
        let spec = &plan.spec;
        let processes = spec
            .processes
            .iter()
            .map(|p| ProcessState {
                name: p.name.clone(),
                epoch: 0,
                up: true,
            })
            .collect();
        let logs = spec
            .logs
            .iter()
            .map(|l| {
                (
                    l.name.clone(),
                    LogState {
                        wal: Wal::new(l.limits.clone()),
                        process: plan.process_index[&l.process],
                        flush_delay_ns: l.flush_delay_ns,
                    },
                )
            })
            .collect();
        let networks = spec
            .networks
            .iter()
            .map(|n| {
                let owners: BTreeMap<u64, usize> = spec
                    .processes
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| p.endpoints.get(&n.name).map(|e| (*e, i)))
                    .collect();
                let delays = n
                    .links
                    .iter()
                    .map(|l| ((l.from, l.to), l.delay_ns))
                    .collect();
                let net = Network::new(
                    NetLimits {
                        max_in_flight: n.max_in_flight,
                        max_inbox: n.max_inbox,
                        max_message_bytes: n.max_message_bytes,
                    },
                    n.delay_ns,
                    delays,
                    owners.keys().copied(),
                );
                (
                    n.name.clone(),
                    NetState {
                        net,
                        timeout_code: n.timeout_code,
                        unreachable_code: n.unreachable_code,
                        owners,
                    },
                )
            })
            .collect();
        Self {
            processes,
            logs,
            networks,
            occurrences: BTreeMap::new(),
            crashes: 0,
            warp: spec
                .warp
                .as_ref()
                .map(|w| warp::WarpState::new(w, &plan.process_index, seed)),
        }
    }

    pub fn durable(&self) -> DurableState {
        self.logs
            .iter()
            .map(|(name, log)| {
                let bytes = log.wal.durable_bytes();
                (
                    name.clone(),
                    DurableLog {
                        bytes_hex: wal::hex(bytes),
                        sha256: crate::program::sha256_hex(bytes),
                    },
                )
            })
            .collect()
    }

    pub fn snapshot(&self, at_ns: u64) -> ModelSnapshot {
        ModelSnapshot {
            at_ns,
            processes: self
                .processes
                .iter()
                .map(|p| ProcessSnapshot {
                    name: p.name.clone(),
                    epoch: p.epoch,
                    up: p.up,
                })
                .collect(),
            logs: self
                .logs
                .iter()
                .map(|(name, log)| {
                    let bytes = log.wal.durable_bytes();
                    let scan = wal::scan(bytes);
                    (
                        name.clone(),
                        LogSnapshot {
                            process: self.processes[log.process].name.clone(),
                            recovered: log.wal.is_recovered(),
                            durable_bytes: bytes.len() as u64,
                            durable_sha256: crate::program::sha256_hex(bytes),
                            recoverable: scan.records,
                            damage: scan.damage,
                            in_flight_lsns: log
                                .wal
                                .in_flight()
                                .map(|b| b.records.iter().map(|r| r.lsn).collect())
                                .unwrap_or_default(),
                            volatile_lsns: log.wal.volatile().iter().map(|r| r.lsn).collect(),
                        },
                    )
                })
                .collect(),
            networks: self
                .networks
                .iter()
                .map(|(name, n)| {
                    (
                        name.clone(),
                        NetworkSnapshot {
                            partitions: n.net.partitions().iter().copied().collect(),
                            in_flight: n.net.in_flight().values().cloned().collect(),
                            inboxes: n
                                .net
                                .endpoints()
                                .iter()
                                .map(|(e, s)| (*e, s.inbox.iter().cloned().collect()))
                                .collect(),
                            waiting: n
                                .net
                                .endpoints()
                                .iter()
                                .map(|(e, s)| (*e, s.waiters.iter().copied().collect()))
                                .collect(),
                        },
                    )
                })
                .collect(),
            warp: self.warp.as_ref().map(|w| {
                let names: Vec<String> = self.processes.iter().map(|p| p.name.clone()).collect();
                w.snapshot(&names)
            }),
        }
    }
}

fn fail<T>(message: impl Into<String>) -> Result<T, String> {
    Err(message.into())
}

/// Check the model spec on its own. `initial_tasks` counts scenario tasks and
/// `max_virtual_ns` bounds fault times.
pub(crate) fn validate(
    spec: ModelSpec,
    initial_tasks: u64,
    max_virtual_ns: u64,
) -> Result<ModelPlan, String> {
    let limits = &spec.limits;
    if limits.max_events == 0 || limits.max_events > MAX_EVENTS {
        return fail(format!("model max_events must be in 1..={MAX_EVENTS}"));
    }
    if limits.max_epochs > MAX_EPOCHS {
        return fail(format!("model max_epochs must be at most {MAX_EPOCHS}"));
    }
    if limits.max_instances == 0 || limits.max_instances > MAX_INSTANCES {
        return fail(format!(
            "model max_instances must be in 1..={MAX_INSTANCES}"
        ));
    }
    if spec.processes.len() > MAX_PROCESSES
        || spec.logs.len() > MAX_LOGS
        || spec.networks.len() > MAX_NETWORKS
        || spec.faults.len() > MAX_FAULTS
        || spec.failures.len() > MAX_FAILURES
    {
        return fail("model exceeds its process/log/network/fault/failure count bounds");
    }
    let networks: BTreeSet<&str> = spec.networks.iter().map(|n| n.name.as_str()).collect();
    if networks.len() != spec.networks.len() {
        return fail("network names must be unique");
    }
    let mut process_index = BTreeMap::new();
    let mut addresses: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
    for (i, process) in spec.processes.iter().enumerate() {
        if process.name.is_empty()
            || process.name.contains('@')
            || process_index.insert(process.name.clone(), i).is_some()
        {
            return fail(format!(
                "process names must be unique, non-empty and free of '@': {:?}",
                process.name
            ));
        }
        for (network, endpoint) in &process.endpoints {
            if !networks.contains(network.as_str()) {
                return fail(format!(
                    "process {} names unknown network {network}",
                    process.name
                ));
            }
            if !addresses.entry(network).or_default().insert(*endpoint) {
                return fail(format!("endpoint {endpoint} is reused on {network}"));
            }
        }
        for task in &process.restart {
            if task.process.as_ref().is_some_and(|p| *p != process.name) {
                return fail(format!("restart task {} names another process", task.name));
            }
        }
    }
    let mut logs = BTreeSet::new();
    for log in &spec.logs {
        if log.name.is_empty() || !logs.insert(log.name.as_str()) {
            return fail(format!("log names must be unique: {:?}", log.name));
        }
        if !process_index.contains_key(&log.process) {
            return fail(format!(
                "log {} names unknown process {}",
                log.name, log.process
            ));
        }
        let l = &log.limits;
        if l.max_records == 0
            || l.max_records > MAX_LOG_RECORDS
            || l.max_bytes == 0
            || l.max_bytes > MAX_LOG_BYTES
            || l.max_record_bytes == 0
            || l.max_record_bytes > l.max_bytes
        {
            return fail(format!(
                "log {} limits must be positive, records <= {MAX_LOG_RECORDS}, bytes <= {MAX_LOG_BYTES}, record bytes <= bytes",
                log.name
            ));
        }
    }
    for network in &spec.networks {
        if network.max_in_flight == 0
            || network.max_in_flight > MAX_QUEUE
            || network.max_inbox == 0
            || network.max_inbox > MAX_QUEUE
            || network.max_message_bytes == 0
            || network.max_message_bytes > MAX_MESSAGE_BYTES
        {
            return fail(format!(
                "network {} bounds must be positive, queues <= {MAX_QUEUE}, messages <= {MAX_MESSAGE_BYTES} bytes",
                network.name
            ));
        }
        if network.timeout_code > i32::MAX as u32 || network.unreachable_code > i32::MAX as u32 {
            return fail(format!("network {} error codes exceed i32", network.name));
        }
        let known = addresses.get(network.name.as_str());
        for link in &network.links {
            if !known.is_some_and(|k| k.contains(&link.from) && k.contains(&link.to)) {
                return fail(format!(
                    "network {} link {}->{} names an unknown endpoint",
                    network.name, link.from, link.to
                ));
            }
        }
    }
    // Faults: known targets, bounded times, per-process crash/restart order.
    let mut timeline: BTreeMap<&str, Vec<(u64, bool)>> = BTreeMap::new();
    let mut crashes = 0u32;
    for timed in &spec.faults {
        if timed.at_ns > max_virtual_ns {
            return fail(format!(
                "fault at {} ns exceeds max_virtual_ns",
                timed.at_ns
            ));
        }
        match &timed.fault {
            Fault::Crash { process, in_flight } => {
                let index = *process_index
                    .get(process)
                    .ok_or_else(|| format!("crash names unknown process {process}"))?;
                for log in in_flight.keys() {
                    if !spec
                        .logs
                        .iter()
                        .any(|l| l.name == *log && process_index[&l.process] == index)
                    {
                        return fail(format!(
                            "crash choice names log {log}, which {process} does not own"
                        ));
                    }
                }
                crashes += 1;
                timeline
                    .entry(process)
                    .or_default()
                    .push((timed.at_ns, true));
            }
            Fault::Restart { process } => {
                if !process_index.contains_key(process) {
                    return fail(format!("restart names unknown process {process}"));
                }
                timeline
                    .entry(process)
                    .or_default()
                    .push((timed.at_ns, false));
            }
            Fault::Partition { network, a, b } | Fault::Heal { network, a, b } => {
                let known = addresses.get(network.as_str());
                if !known.is_some_and(|k| k.contains(a) && k.contains(b)) || a == b {
                    return fail(format!(
                        "partition {a}<->{b} names an unknown network or endpoint"
                    ));
                }
            }
        }
    }
    if crashes > limits.max_epochs {
        return fail(format!(
            "{crashes} crashes exceed model max_epochs {}",
            limits.max_epochs
        ));
    }
    let mut instances = initial_tasks;
    for (process, mut events) in timeline {
        events.sort();
        let mut expect_crash = true;
        let mut last = None;
        for (at, crash) in events {
            if last == Some(at) || crash != expect_crash {
                return fail(format!(
                    "process {process} faults must alternate crash, restart at distinct times"
                ));
            }
            if !crash {
                instances += spec.processes[process_index[process]].restart.len() as u64;
            }
            expect_crash = !expect_crash;
            last = Some(at);
        }
    }
    if instances > limits.max_instances {
        return fail(format!(
            "{instances} task instances exceed model max_instances {}",
            limits.max_instances
        ));
    }
    if let Some(w) = &spec.warp {
        let crashing: BTreeSet<String> = spec
            .faults
            .iter()
            .filter_map(|f| match &f.fault {
                Fault::Crash { process, .. } => Some(process.clone()),
                _ => None,
            })
            .collect();
        warp::validate(w, &process_index, &crashing, max_virtual_ns)?;
    }
    let mut failures = BTreeMap::new();
    for f in &spec.failures {
        let ok = match f.operation {
            FailOp::Append | FailOp::Flush => logs.contains(f.target.as_str()),
            FailOp::Send => networks.contains(f.target.as_str()),
        };
        if !ok {
            return fail(format!(
                "failure names unknown {:?} target {}",
                f.operation, f.target
            ));
        }
        if f.occurrence == 0 || f.code > i32::MAX as u32 {
            return fail("failure occurrences start at 1 and codes fit i32");
        }
        if failures
            .insert((f.target.clone(), f.operation, f.occurrence), f.code)
            .is_some()
        {
            return fail(format!("duplicate failure for {}", f.target));
        }
    }
    Ok(ModelPlan {
        spec,
        process_index,
        failures,
    })
}
