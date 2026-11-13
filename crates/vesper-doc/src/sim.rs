//! Hermetic SimPulse doctest environment (§40.2).
//!
//! A doctest fence `vesper,sim(seed=N,clock,net,disk)` declares a fixed seed
//! and the simulated devices its entry may use. Each device is a source
//! `linear effect` whose operations must match [`OPERATIONS`] exactly (all
//! parameters and results `u64`). The compiler facade runs the entry on its
//! reference evaluator and routes every effect operation to a [`SimHost`].
//!
//! The host owns one `vesper_simpulse::Simulator` task. Every operation with
//! latency parks that task on a scheduled completion, so virtual time only
//! advances through the SimPulse kernel. Network latency jitter comes from a
//! seed-derived generator, so the same seed reproduces the same trace and a
//! different seed may not. [`SimHost::replay`] re-runs against a recorded
//! SimPulse journal and rejects the first divergent transition.
//!
//! This is a deterministic model with scripted semantics, not real network,
//! disk or clock I/O, not an OS sandbox and not schedule exploration.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
};
use vesper_simpulse::{IoResult, Journal, RunToken, Simulator, WakeReason};

pub const SIM_SEMANTICS: &str = "vesper.codex.sim/1";

/// A simulated device, declared by name in the fence attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Device {
    Clock,
    Net,
    Disk,
}
impl Device {
    /// The source effect name that provides this device.
    pub fn effect(self) -> &'static str {
        match self {
            Self::Clock => "Clock",
            Self::Net => "Net",
            Self::Disk => "Disk",
        }
    }
    pub fn from_effect(name: &str) -> Option<Self> {
        match name {
            "Clock" => Some(Self::Clock),
            "Net" => Some(Self::Net),
            "Disk" => Some(Self::Disk),
            _ => None,
        }
    }
    fn from_attribute(name: &str) -> Option<Self> {
        match name {
            "clock" => Some(Self::Clock),
            "net" => Some(Self::Net),
            "disk" => Some(Self::Disk),
            _ => None,
        }
    }
}

/// The complete simulated operation contract: (device, operation, u64 arity).
/// Every result is `u64`.
///
/// - `Clock::now() -> u64`: current virtual time in nanoseconds.
/// - `Clock::sleep(ns) -> u64`: park for `ns`; returns the new time.
/// - `Net::send(port, value) -> u64`: enqueue `value` for `port` after a
///   seeded latency of 50..=100 ns; returns 0, or 113 when the port inbox is full.
/// - `Net::recv(port, timeout_ns) -> u64`: wait up to `timeout_ns` for the
///   earliest message to `port`; returns it, or 0 on timeout.
/// - `Disk::write(block, value) -> u64`: volatile write (20 ns); returns 0.
/// - `Disk::read(block) -> u64`: latest written value, or 0 (10 ns).
/// - `Disk::flush() -> u64`: make all writes durable (100 ns); returns the
///   number of durable blocks.
pub const OPERATIONS: &[(Device, &str, usize)] = &[
    (Device::Clock, "now", 0),
    (Device::Clock, "sleep", 1),
    (Device::Net, "send", 2),
    (Device::Net, "recv", 2),
    (Device::Disk, "write", 2),
    (Device::Disk, "read", 1),
    (Device::Disk, "flush", 0),
];
pub fn operation_arity(device: Device, name: &str) -> Option<usize> {
    OPERATIONS
        .iter()
        .find(|(d, n, _)| *d == device && *n == name)
        .map(|(_, _, a)| *a)
}

const NET_BASE_NS: u64 = 50;
const NET_JITTER_NS: u64 = 50;
const DISK_WRITE_NS: u64 = 20;
const DISK_READ_NS: u64 = 10;
const DISK_FLUSH_NS: u64 = 100;
const NET_UNREACHABLE: u64 = 113;

/// A parsed `sim(...)` fence attribute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimSpec {
    pub seed: u64,
    pub devices: BTreeSet<Device>,
}
impl SimSpec {
    /// Parse the inside of `sim(...)`: `seed=N` (decimal or `0x` hex,
    /// `_` separators allowed) and one or more of `clock`, `net`, `disk`.
    pub fn parse(inner: &str) -> Result<Self, SimError> {
        let mut seed = None;
        let mut devices = BTreeSet::new();
        for part in inner.split(',').map(str::trim) {
            if let Some(value) = part.strip_prefix("seed=") {
                if seed.is_some() {
                    return Err(SimError::Attribute("duplicate seed".into()));
                }
                let digits: String = value.chars().filter(|c| *c != '_').collect();
                let parsed = if let Some(hex) = digits.strip_prefix("0x") {
                    u64::from_str_radix(hex, 16)
                } else {
                    digits.parse::<u64>()
                };
                seed = Some(
                    parsed.map_err(|_| SimError::Attribute(format!("invalid seed '{value}'")))?,
                );
            } else if let Some(device) = Device::from_attribute(part) {
                if !devices.insert(device) {
                    return Err(SimError::Attribute(format!("duplicate device '{part}'")));
                }
            } else {
                return Err(SimError::Attribute(format!(
                    "unknown sim attribute '{part}' (expected seed=N, clock, net or disk)"
                )));
            }
        }
        let seed = seed.ok_or_else(|| SimError::Attribute("sim requires a fixed seed=N".into()))?;
        if devices.is_empty() {
            return Err(SimError::Attribute(
                "sim requires at least one device: clock, net or disk".into(),
            ));
        }
        Ok(Self { seed, devices })
    }
}

/// Work bounds for one simulated run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimLimits {
    pub max_operations: usize,
    pub max_virtual_ns: u64,
    pub max_inbox: usize,
    pub max_blocks: usize,
}
impl Default for SimLimits {
    fn default() -> Self {
        Self {
            max_operations: 4096,
            max_virtual_ns: 10_000_000_000,
            max_inbox: 64,
            max_blocks: 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SimError {
    Attribute(String),
    UndeclaredDevice(String),
    UnknownOperation {
        effect: String,
        operation: String,
    },
    Arity {
        operation: String,
        expected: usize,
        actual: usize,
    },
    OperationLimit(usize),
    VirtualTimeLimit(u64),
    BlockLimit(usize),
    Kernel(String),
}
impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attribute(m) => write!(f, "invalid sim attribute: {m}"),
            Self::UndeclaredDevice(e) => {
                write!(
                    f,
                    "effect {e} is not a simulated device declared by this doctest"
                )
            }
            Self::UnknownOperation { effect, operation } => {
                write!(f, "{effect}::{operation} is not a simulated operation")
            }
            Self::Arity {
                operation,
                expected,
                actual,
            } => write!(
                f,
                "{operation} expects {expected} u64 argument(s), got {actual}"
            ),
            Self::OperationLimit(n) => write!(f, "simulated operation limit {n} exceeded"),
            Self::VirtualTimeLimit(n) => write!(f, "virtual time limit {n} ns exceeded"),
            Self::BlockLimit(n) => write!(f, "simulated disk block limit {n} exceeded"),
            Self::Kernel(m) => write!(f, "SimPulse kernel: {m}"),
        }
    }
}
impl std::error::Error for SimError {}

/// One device operation as observed by the doctest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimEvent {
    pub at_ns: u64,
    pub device: Device,
    pub operation: String,
    pub arguments: Vec<u64>,
    pub result: u64,
    pub completed_ns: u64,
}

/// Everything a finished run reproduces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimOutcome {
    pub semantics: String,
    pub seed: u64,
    pub final_ns: u64,
    pub events: Vec<SimEvent>,
    pub durable_blocks: BTreeMap<u64, u64>,
    pub undelivered_messages: usize,
    pub journal: Journal,
}

#[derive(Clone, Copy, Debug)]
struct Message {
    arrival_ns: u64,
    sequence: u64,
    value: u64,
}

/// The simulated environment for one doctest run.
pub struct SimHost {
    spec: SimSpec,
    limits: SimLimits,
    kernel: Simulator<()>,
    token: Option<RunToken<()>>,
    rng: u64,
    sequence: u64,
    inboxes: BTreeMap<u64, VecDeque<Message>>,
    volatile: BTreeMap<u64, u64>,
    durable: BTreeMap<u64, u64>,
    events: Vec<SimEvent>,
}
impl SimHost {
    /// Record a new run under `spec.seed`.
    pub fn new(spec: SimSpec, limits: SimLimits) -> Result<Self, SimError> {
        let kernel = Simulator::new(spec.seed);
        Self::start(spec, limits, kernel)
    }
    /// Re-run against a recorded journal; any divergent SimPulse transition
    /// is an error. The journal's seed must equal `spec.seed`.
    pub fn replay(spec: SimSpec, limits: SimLimits, journal: Journal) -> Result<Self, SimError> {
        if journal.seed != spec.seed {
            return Err(SimError::Kernel(
                "journal seed differs from the doctest seed".into(),
            ));
        }
        let kernel = Simulator::replay(journal).map_err(|e| SimError::Kernel(e.to_string()))?;
        Self::start(spec, limits, kernel)
    }
    fn start(
        spec: SimSpec,
        limits: SimLimits,
        mut kernel: Simulator<()>,
    ) -> Result<Self, SimError> {
        kernel
            .spawn(())
            .map_err(|e| SimError::Kernel(e.error.to_string()))?;
        let token = kernel
            .next_task()
            .map_err(|e| SimError::Kernel(e.to_string()))?
            .ok_or_else(|| SimError::Kernel("spawned doctest task did not start".into()))?;
        Ok(Self {
            // splitmix64 state, separate from the kernel's own scheduling stream.
            rng: spec.seed ^ 0x9E37_79B9_7F4A_7C15,
            spec,
            limits,
            kernel,
            token: Some(token),
            sequence: 0,
            inboxes: BTreeMap::new(),
            volatile: BTreeMap::new(),
            durable: BTreeMap::new(),
            events: Vec::new(),
        })
    }
    pub fn spec(&self) -> &SimSpec {
        &self.spec
    }
    pub fn now_ns(&self) -> u64 {
        self.kernel.now_ns()
    }
    fn random(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Park the doctest task for `delay_ns` through the kernel and resume it.
    fn wait(&mut self, delay_ns: u64, sleep: bool) -> Result<(), SimError> {
        let deadline = self
            .kernel
            .now_ns()
            .checked_add(delay_ns)
            .ok_or(SimError::VirtualTimeLimit(self.limits.max_virtual_ns))?;
        if deadline > self.limits.max_virtual_ns {
            return Err(SimError::VirtualTimeLimit(self.limits.max_virtual_ns));
        }
        let token = self
            .token
            .take()
            .ok_or_else(|| SimError::Kernel("doctest task is not running".into()))?;
        let parked = if sleep {
            self.kernel.sleep(token, delay_ns)
        } else {
            self.kernel
                .schedule_io(token, delay_ns, IoResult::Success { bytes: 8 })
        };
        let completion = parked.map_err(|e| SimError::Kernel(e.error.to_string()))?;
        let token = self
            .kernel
            .next_task()
            .map_err(|e| SimError::Kernel(e.to_string()))?
            .ok_or_else(|| SimError::Kernel("parked doctest task never resumed".into()))?;
        let expected = match token.reason() {
            WakeReason::Timer(id) => sleep && id == completion,
            WakeReason::Io { completion: id, .. } => !sleep && id == completion,
            _ => false,
        };
        if !expected {
            return Err(SimError::Kernel(
                "doctest task resumed for the wrong completion".into(),
            ));
        }
        self.token = Some(token);
        Ok(())
    }
    /// Execute one device operation. `effect` is the source effect name.
    pub fn call(
        &mut self,
        effect: &str,
        operation: &str,
        arguments: &[u64],
    ) -> Result<u64, SimError> {
        let device = Device::from_effect(effect)
            .filter(|d| self.spec.devices.contains(d))
            .ok_or_else(|| SimError::UndeclaredDevice(effect.into()))?;
        let arity =
            operation_arity(device, operation).ok_or_else(|| SimError::UnknownOperation {
                effect: effect.into(),
                operation: operation.into(),
            })?;
        if arguments.len() != arity {
            return Err(SimError::Arity {
                operation: format!("{effect}::{operation}"),
                expected: arity,
                actual: arguments.len(),
            });
        }
        if self.events.len() >= self.limits.max_operations {
            return Err(SimError::OperationLimit(self.limits.max_operations));
        }
        let at_ns = self.now_ns();
        let result = match (device, operation) {
            (Device::Clock, "now") => at_ns,
            (Device::Clock, "sleep") => {
                self.wait(arguments[0], true)?;
                self.now_ns()
            }
            (Device::Net, "send") => {
                let latency = NET_BASE_NS + self.random() % (NET_JITTER_NS + 1);
                let inbox = self.inboxes.entry(arguments[0]).or_default();
                if inbox.len() >= self.limits.max_inbox {
                    NET_UNREACHABLE
                } else {
                    self.sequence += 1;
                    let message = Message {
                        arrival_ns: at_ns.saturating_add(latency),
                        sequence: self.sequence,
                        value: arguments[1],
                    };
                    let inbox = self.inboxes.entry(arguments[0]).or_default();
                    let position = inbox
                        .iter()
                        .position(|m| {
                            (m.arrival_ns, m.sequence) > (message.arrival_ns, message.sequence)
                        })
                        .unwrap_or(inbox.len());
                    inbox.insert(position, message);
                    0
                }
            }
            (Device::Net, "recv") => {
                let deadline = at_ns.saturating_add(arguments[1]);
                let next = self
                    .inboxes
                    .get(&arguments[0])
                    .and_then(|q| q.front())
                    .filter(|m| m.arrival_ns <= deadline)
                    .copied();
                match next {
                    Some(message) => {
                        self.wait(message.arrival_ns.saturating_sub(at_ns), false)?;
                        self.inboxes
                            .get_mut(&arguments[0])
                            .and_then(VecDeque::pop_front);
                        message.value
                    }
                    None => {
                        self.wait(arguments[1], true)?;
                        0
                    }
                }
            }
            (Device::Disk, "write") => {
                if !self.volatile.contains_key(&arguments[0])
                    && self.volatile.len() >= self.limits.max_blocks
                {
                    return Err(SimError::BlockLimit(self.limits.max_blocks));
                }
                self.wait(DISK_WRITE_NS, false)?;
                self.volatile.insert(arguments[0], arguments[1]);
                0
            }
            (Device::Disk, "read") => {
                self.wait(DISK_READ_NS, false)?;
                self.volatile.get(&arguments[0]).copied().unwrap_or(0)
            }
            (Device::Disk, "flush") => {
                self.wait(DISK_FLUSH_NS, false)?;
                self.durable = self.volatile.clone();
                self.durable.len() as u64
            }
            _ => unreachable!("operation table and dispatch agree"),
        };
        self.events.push(SimEvent {
            at_ns,
            device,
            operation: operation.into(),
            arguments: arguments.to_vec(),
            result,
            completed_ns: self.now_ns(),
        });
        Ok(result)
    }
    /// Finish the doctest task. In replay, the whole journal must be consumed.
    pub fn finish(mut self) -> Result<SimOutcome, SimError> {
        let token = self
            .token
            .take()
            .ok_or_else(|| SimError::Kernel("doctest task is not running".into()))?;
        self.kernel
            .finish(token)
            .map_err(|e| SimError::Kernel(e.error.to_string()))?;
        self.kernel
            .finish_replay()
            .map_err(|e| SimError::Kernel(e.to_string()))?;
        if !self.kernel.is_complete() {
            return Err(SimError::Kernel(
                "simulation finished with live tasks".into(),
            ));
        }
        Ok(SimOutcome {
            semantics: SIM_SEMANTICS.into(),
            seed: self.spec.seed,
            final_ns: self.kernel.now_ns(),
            events: self.events,
            durable_blocks: self.durable,
            undelivered_messages: self.inboxes.values().map(VecDeque::len).sum(),
            journal: self.kernel.journal().clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(seed: u64) -> SimSpec {
        SimSpec::parse(&format!("seed={seed},clock,net,disk")).unwrap()
    }
    fn script(host: &mut SimHost) -> Vec<u64> {
        vec![
            host.call("Net", "send", &[7, 41]).unwrap(),
            host.call("Net", "send", &[7, 42]).unwrap(),
            host.call("Net", "recv", &[7, 1_000]).unwrap(),
            host.call("Net", "recv", &[7, 1_000]).unwrap(),
            host.call("Net", "recv", &[7, 5]).unwrap(),
            host.call("Disk", "write", &[0, 9]).unwrap(),
            host.call("Disk", "flush", &[]).unwrap(),
            host.call("Disk", "read", &[0]).unwrap(),
            host.call("Clock", "sleep", &[30]).unwrap(),
            host.call("Clock", "now", &[]).unwrap(),
        ]
    }

    #[test]
    fn same_seed_reproduces_and_replays_the_kernel_journal() {
        let mut first = SimHost::new(spec(0x5EED), SimLimits::default()).unwrap();
        let a = script(&mut first);
        let first = first.finish().unwrap();
        let mut second =
            SimHost::replay(spec(0x5EED), SimLimits::default(), first.journal.clone()).unwrap();
        let b = script(&mut second);
        let second = second.finish().unwrap();
        assert_eq!(a, b);
        assert_eq!(first, second);
        assert_eq!(&a[2..4], &[41, 42]);
        assert_eq!(a[4], 0, "empty inbox times out");
        assert_eq!(a[6], 1);
        assert_eq!(a[7], 9);
        assert_eq!(first.durable_blocks, BTreeMap::from([(0, 9)]));
        assert_eq!(a[8], a[9]);
    }

    #[test]
    fn seeds_change_network_latency_and_divergence_is_rejected() {
        let finals: BTreeSet<u64> = (0..16)
            .map(|seed| {
                let mut host = SimHost::new(spec(seed), SimLimits::default()).unwrap();
                script(&mut host);
                host.finish().unwrap().final_ns
            })
            .collect();
        assert!(finals.len() > 1, "seeded jitter must matter");
        let mut host = SimHost::new(spec(3), SimLimits::default()).unwrap();
        host.call("Clock", "sleep", &[10]).unwrap();
        let journal = host.finish().unwrap().journal;
        let mut replay = SimHost::replay(spec(3), SimLimits::default(), journal.clone()).unwrap();
        assert!(matches!(
            replay.call("Clock", "sleep", &[11]),
            Err(SimError::Kernel(_))
        ));
        let replay = SimHost::replay(spec(3), SimLimits::default(), journal.clone()).unwrap();
        assert!(replay.finish().is_err(), "unconsumed journal is rejected");
        assert!(SimHost::replay(spec(4), SimLimits::default(), journal).is_err());
    }

    #[test]
    fn attributes_devices_arity_and_limits_are_typed_errors() {
        assert_eq!(
            SimSpec::parse("seed=0x5EED_CAFE, clock").unwrap().seed,
            0x5EED_CAFE
        );
        for bad in [
            "clock",
            "seed=1",
            "seed=1,seed=2,clock",
            "seed=x,clock",
            "seed=1,gpu",
            "seed=1,net,net",
        ] {
            assert!(
                matches!(SimSpec::parse(bad), Err(SimError::Attribute(_))),
                "{bad}"
            );
        }
        let only_clock = SimSpec::parse("seed=1,clock").unwrap();
        let mut host = SimHost::new(only_clock, SimLimits::default()).unwrap();
        assert_eq!(
            host.call("Net", "send", &[1, 2]),
            Err(SimError::UndeclaredDevice("Net".into()))
        );
        assert!(matches!(
            host.call("Clock", "tick", &[]),
            Err(SimError::UnknownOperation { .. })
        ));
        assert!(matches!(
            host.call("Clock", "sleep", &[]),
            Err(SimError::Arity { .. })
        ));
        assert_eq!(
            host.call("Clock", "sleep", &[u64::MAX]),
            Err(SimError::VirtualTimeLimit(
                SimLimits::default().max_virtual_ns
            ))
        );
        let limits = SimLimits {
            max_operations: 1,
            ..SimLimits::default()
        };
        let mut host = SimHost::new(spec(1), limits).unwrap();
        host.call("Clock", "now", &[]).unwrap();
        assert_eq!(
            host.call("Clock", "now", &[]),
            Err(SimError::OperationLimit(1))
        );
    }
}
