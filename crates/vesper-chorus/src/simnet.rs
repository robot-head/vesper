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
use crate::port::binding;
use crate::session::{Frame, NetFail, Transport};
use std::sync::{Arc, Mutex};
use vesper_simpulse::{CompletionId, IoResult, Journal, Simulator};

/// SplitMix64, used for network randomness (not cryptographic).
#[derive(Clone, Debug)]
pub struct SplitMix(pub u64);

impl SplitMix {
    pub fn next_u64(&mut self) -> u64 {
        binding::p2_sm_next(&mut self.0)
    }
    /// Uniform in `lo..=hi` (modulo bias is irrelevant for simulation).
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        binding::p2_sm_range(&mut self.0, lo, hi)
    }
    pub fn chance(&mut self, per_mille: u32) -> bool {
        binding::p2_sm_chance(&mut self.0, u64::from(per_mille))
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

#[derive(Clone)]
pub struct SimNet {
    inner: Arc<Mutex<binding::vesper__chorus__simnet__SimNetCore>>,
}

impl SimNet {
    pub fn new(seed: u64, cfg: SimNetConfig) -> Self {
        let core = binding::p2_simnet_new(
            seed,
            cfg.min_latency_ns,
            cfg.max_latency_ns,
            u64::from(cfg.duplicate_per_mille),
        );
        Self {
            inner: Arc::new(Mutex::new(core)),
        }
    }
    pub fn handle(&self) -> Box<dyn Transport> {
        Box::new(self.clone())
    }
    pub fn stats(&self) -> NetStats {
        let st = binding::p2_simnet_stats(&self.lock());
        NetStats {
            sent: st.sent,
            delivered: st.delivered,
            duplicated: st.duplicated,
        }
    }
    /// Frames delivered to a mailbox but never received, plus frames in
    /// flight.
    pub fn undelivered(&self) -> usize {
        binding::p2_simnet_stats(&self.lock()).undelivered as usize
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, binding::vesper__chorus__simnet__SimNetCore> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Transport for SimNet {
    fn send(&mut self, frame: Frame) -> Result<(), NetFail> {
        let mut g = self.lock();
        let rc = binding::p2_simnet_send(
            &mut g,
            frame.channel,
            u64::from(frame.from.0),
            u64::from(frame.to.0),
            frame.seq,
            &frame.label,
            &frame.payload_ty,
            &frame.body,
        );
        if rc == 1 {
            Err(NetFail::PeerGone)
        } else {
            Ok(())
        }
    }
    fn try_recv(
        &mut self,
        channel: u64,
        from: RoleId,
        to: RoleId,
    ) -> Result<Option<Frame>, NetFail> {
        let mut g = self.lock();
        match binding::p2_simnet_try_recv(&mut g, channel, u64::from(from.0), u64::from(to.0)) {
            1 => {
                let r = g.as_ref();
                Ok(Some(Frame {
                    channel,
                    from,
                    to,
                    seq: r.rx_seq(),
                    label: String::from_utf8_lossy(r.rx_label().as_slice()).into_owned(),
                    payload_ty: String::from_utf8_lossy(r.rx_pty().as_slice()).into_owned(),
                    body: r.rx_body().as_slice().to_vec(),
                }))
            }
            2 => Err(NetFail::PeerGone),
            _ => Ok(None),
        }
    }
    fn abandon(&mut self, channel: u64, me: RoleId) {
        let mut g = self.lock();
        binding::p2_simnet_abandon(&mut g, channel, u64::from(me.0));
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
                let st = binding::p2_simnet_stats(&net.lock());
                return Ok(SimRunReport {
                    journal: sim.journal().clone(),
                    end_ns: sim.now_ns(),
                    stats: NetStats {
                        sent: st.sent,
                        delivered: st.delivered,
                        duplicated: st.duplicated,
                    },
                    undelivered: st.undelivered as usize,
                });
            }
            return Err(SimRunError::Deadlock {
                blocked: (0..roles.len()).filter(|i| !done[*i]).collect(),
            });
        };
        let now = sim.now_ns();
        binding::p2_simnet_set_now(&mut net.lock(), now);
        match *token.payload() {
            Task::Network => {
                net_wait = None;
                let out = binding::p2_simnet_deliver_due(&mut net.lock(), now);
                let delivered = out.delivered.get();
                let next = out.has_next.get().then_some(out.next_at);
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
                if binding::p2_simnet_take_abandon(&mut net.lock()) {
                    for w in waits.iter_mut() {
                        if let Some(c) = w.take() {
                            sim.complete_io(c, IoResult::Success { bytes: 0 })
                                .map_err(SimRunError::Sim)?;
                        }
                    }
                }
                // Wake the network early if a new frame is due before its
                // current deadline, or if everyone is done.
                let next = binding::p2_simnet_next_delivery(&net.lock());
                let earliest = next.has_next.get().then_some(next.next_at);
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
