//! Session frames delivered by SimPulse-scheduled virtual time.
//!
//! [`SimNet`] is a [`Transport`] whose frames are held in flight for a
//! seeded latency (so frames on different channels, and frames of the same
//! channel, can be reordered) and optionally duplicated. [`run_roles`] runs a
//! set of role state machines as SimPulse tasks next to one network task:
//! a role that cannot receive yet parks on an I/O completion, and the network
//! task completes it when a frame is delivered. Every scheduling choice comes
//! from the SimPulse seed and every network choice from the same seed, so a
//! run is reproducible and its SimPulse journal can be replayed.

use crate::chor::RoleId;
use crate::session::{Frame, NetFail, Transport};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use vesper_simpulse::{CompletionId, IoResult, Journal, Simulator};

/// SplitMix64, used for network randomness (not cryptographic).
#[derive(Clone, Debug)]
pub struct SplitMix(pub u64);

impl SplitMix {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// Uniform in `lo..=hi` (modulo bias is irrelevant for simulation).
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            return lo;
        }
        lo + self.next_u64() % (hi - lo + 1)
    }
    pub fn chance(&mut self, per_mille: u32) -> bool {
        self.next_u64() % 1000 < u64::from(per_mille)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SimNetConfig {
    pub min_latency_ns: u64,
    pub max_latency_ns: u64,
    /// Probability (per mille) that a frame is delivered twice.
    pub duplicate_per_mille: u32,
}

impl Default for SimNetConfig {
    fn default() -> Self {
        Self {
            min_latency_ns: 1_000,
            max_latency_ns: 50_000,
            duplicate_per_mille: 100,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetStats {
    pub sent: u64,
    pub delivered: u64,
    pub duplicated: u64,
}

struct NetState {
    now: u64,
    rng: SplitMix,
    cfg: SimNetConfig,
    next_id: u64,
    in_flight: BTreeMap<(u64, u64), Frame>,
    mailboxes: HashMap<(u64, RoleId, RoleId), VecDeque<Frame>>,
    abandoned: Vec<(u64, RoleId)>,
    stats: NetStats,
    /// Earliest delivery time added since the network task last ran.
    earliest_new: Option<u64>,
    /// A channel end was abandoned since the driver last looked: blocked
    /// peers must be woken to observe `PeerGone`.
    abandon_event: bool,
}

#[derive(Clone)]
pub struct SimNet {
    inner: Arc<Mutex<NetState>>,
}

impl SimNet {
    pub fn new(seed: u64, cfg: SimNetConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(NetState {
                now: 0,
                rng: SplitMix(seed ^ 0x6e65_7477_6f72_6b21),
                cfg,
                next_id: 0,
                in_flight: BTreeMap::new(),
                mailboxes: HashMap::new(),
                abandoned: Vec::new(),
                stats: NetStats::default(),
                earliest_new: None,
                abandon_event: false,
            })),
        }
    }
    pub fn handle(&self) -> Box<dyn Transport> {
        Box::new(self.clone())
    }
    pub fn stats(&self) -> NetStats {
        self.lock().stats.clone()
    }
    /// Frames delivered to a mailbox but never received, plus frames in
    /// flight.
    pub fn undelivered(&self) -> usize {
        let g = self.lock();
        g.in_flight.len() + g.mailboxes.values().map(VecDeque::len).sum::<usize>()
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, NetState> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Transport for SimNet {
    fn send(&mut self, frame: Frame) -> Result<(), NetFail> {
        let mut g = self.lock();
        if g.abandoned.contains(&(frame.channel, frame.to)) {
            return Err(NetFail::PeerGone);
        }
        let (lo, hi) = (g.cfg.min_latency_ns, g.cfg.max_latency_ns);
        let dup = g.cfg.duplicate_per_mille;
        let copies = if g.rng.chance(dup) { 2 } else { 1 };
        g.stats.sent += 1;
        for _ in 0..copies {
            let at = g.now.saturating_add(g.rng.range(lo, hi));
            let id = g.next_id;
            g.next_id += 1;
            g.in_flight.insert((at, id), frame.clone());
            g.earliest_new = Some(g.earliest_new.map_or(at, |e| e.min(at)));
        }
        if copies == 2 {
            g.stats.duplicated += 1;
        }
        Ok(())
    }
    fn try_recv(
        &mut self,
        channel: u64,
        from: RoleId,
        to: RoleId,
    ) -> Result<Option<Frame>, NetFail> {
        let mut g = self.lock();
        if let Some(f) = g
            .mailboxes
            .get_mut(&(channel, from, to))
            .and_then(VecDeque::pop_front)
        {
            return Ok(Some(f));
        }
        if g.abandoned.contains(&(channel, from)) {
            return Err(NetFail::PeerGone);
        }
        Ok(None)
    }
    fn abandon(&mut self, channel: u64, me: RoleId) {
        let mut g = self.lock();
        g.abandoned.push((channel, me));
        g.abandon_event = true;
    }
}

/// Outcome of one role step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleStep {
    /// Made progress; run again later.
    Progress,
    /// Waiting for a frame.
    Blocked,
    /// Finished its protocol.
    Done,
}

/// A role program driven by the simulator.
pub trait SimRole {
    fn step(&mut self) -> Result<RoleStep, crate::session::ChorusError>;
}

#[derive(Debug)]
pub enum SimRunError {
    Sim(vesper_simpulse::Error),
    Role {
        role: usize,
        error: crate::session::ChorusError,
    },
    /// Every role is blocked and nothing is in flight.
    Deadlock {
        blocked: Vec<usize>,
    },
    StepBudget,
}

#[derive(Debug)]
pub struct SimRunReport {
    pub journal: Journal,
    pub end_ns: u64,
    pub stats: NetStats,
    pub undelivered: usize,
}

enum Task {
    Network,
    Role(usize),
}

/// How the simulator is constructed.
pub enum SimMode {
    Record(u64),
    Replay(Journal),
}

/// Run `roles` to completion over `net` under SimPulse.
pub fn run_roles(
    mode: SimMode,
    net: &SimNet,
    roles: &mut [Box<dyn SimRole + '_>],
    max_steps: usize,
) -> Result<SimRunReport, SimRunError> {
    let mut sim: Simulator<Task> = match mode {
        SimMode::Record(seed) => Simulator::new(seed),
        SimMode::Replay(j) => Simulator::replay(j).map_err(SimRunError::Sim)?,
    };
    let spawn = |sim: &mut Simulator<Task>, t| sim.spawn(t).map_err(|e| SimRunError::Sim(e.error));
    spawn(&mut sim, Task::Network)?;
    for i in 0..roles.len() {
        spawn(&mut sim, Task::Role(i))?;
    }
    let mut done = vec![false; roles.len()];
    let mut waits: Vec<Option<CompletionId>> = vec![None; roles.len()];
    let mut net_wait: Option<(CompletionId, Option<u64>)> = None;
    let tr = |e: vesper_simpulse::TransitionError<Task>| SimRunError::Sim(e.error);
    for _ in 0..max_steps {
        let Some(token) = sim.next_task().map_err(SimRunError::Sim)? else {
            if sim.is_complete() {
                // A replayed journal must be consumed completely.
                sim.finish_replay().map_err(SimRunError::Sim)?;
                let g = net.lock();
                let undelivered =
                    g.in_flight.len() + g.mailboxes.values().map(VecDeque::len).sum::<usize>();
                return Ok(SimRunReport {
                    journal: sim.journal().clone(),
                    end_ns: sim.now_ns(),
                    stats: g.stats.clone(),
                    undelivered,
                });
            }
            return Err(SimRunError::Deadlock {
                blocked: (0..roles.len()).filter(|i| !done[*i]).collect(),
            });
        };
        let now = sim.now_ns();
        net.lock().now = now;
        match *token.payload() {
            Task::Network => {
                net_wait = None;
                let mut g = net.lock();
                g.earliest_new = None;
                let due: Vec<(u64, u64)> = g
                    .in_flight
                    .range(..=(now, u64::MAX))
                    .map(|(k, _)| *k)
                    .collect();
                let delivered = !due.is_empty();
                for k in due {
                    let f = g.in_flight.remove(&k).expect("due frame");
                    g.stats.delivered += 1;
                    g.mailboxes
                        .entry((f.channel, f.from, f.to))
                        .or_default()
                        .push_back(f);
                }
                let next = g.in_flight.keys().next().map(|(t, _)| *t);
                drop(g);
                if delivered {
                    for w in waits.iter_mut() {
                        if let Some(c) = w.take() {
                            sim.complete_io(c, IoResult::Success { bytes: 0 })
                                .map_err(SimRunError::Sim)?;
                        }
                    }
                }
                if done.iter().all(|d| *d) {
                    sim.finish(token).map_err(tr)?;
                } else if let Some(t) = next {
                    let c = sim
                        .schedule_io(token, t - now, IoResult::Success { bytes: 0 })
                        .map_err(tr)?;
                    net_wait = Some((c, Some(t)));
                } else {
                    let c = sim.park_io(token).map_err(tr)?;
                    net_wait = Some((c, None));
                }
            }
            Task::Role(i) => {
                let step = roles[i]
                    .step()
                    .map_err(|error| SimRunError::Role { role: i, error })?;
                match step {
                    RoleStep::Progress => sim.yield_task(token).map_err(tr)?,
                    RoleStep::Blocked => waits[i] = Some(sim.park_io(token).map_err(tr)?),
                    RoleStep::Done => {
                        done[i] = true;
                        sim.finish(token).map_err(tr)?;
                    }
                }
                // An abandoned channel end is a readiness event for every
                // parked role (it may be the peer that now sees `PeerGone`).
                if std::mem::take(&mut net.lock().abandon_event) {
                    for w in waits.iter_mut() {
                        if let Some(c) = w.take() {
                            sim.complete_io(c, IoResult::Success { bytes: 0 })
                                .map_err(SimRunError::Sim)?;
                        }
                    }
                }
                // Wake the network early if a new frame is due before its
                // current deadline, or if everyone is done.
                let earliest = net.lock().earliest_new;
                if let Some((c, deadline)) = net_wait {
                    let wake = match (earliest, deadline) {
                        (Some(e), Some(d)) => e < d,
                        (Some(_), None) => true,
                        _ => false,
                    } || done.iter().all(|d| *d);
                    if wake {
                        sim.complete_io(c, IoResult::Success { bytes: 0 })
                            .map_err(SimRunError::Sim)?;
                        net_wait = None;
                    }
                }
            }
        }
    }
    Err(SimRunError::StepBudget)
}
