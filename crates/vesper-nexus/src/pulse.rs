//! SimPulse run-queue adapter for fuel preemption (§30.1 item 4).
//!
//! [`SimPulseScheduler`] implements [`QuantumScheduler`] on a
//! `vesper_simpulse::Simulator`: each admitted plugin call becomes a SimPulse
//! task whose payload is the job index, [`NexusEngine::run_scheduled`]
//! asks SimPulse's run queue which task to dispatch for every fuel quantum,
//! and a preempted guest is handed back with `yield_task` (it stays ready)
//! while a returned or trapped one leaves with `finish`. SimPulse's seeded,
//! round-based selection therefore decides the interleaving: every runnable
//! tenant gets one quantum per round, in a seed-determined order, and the
//! whole schedule is recorded in SimPulse's versioned journal.
//!
//! [`SimPulseScheduler::replay`] drives the same run from a recorded journal;
//! SimPulse validates every spawn, dispatch, yield and finish against it, so
//! a replay that diverges is reported as a [`SimPulseFault`] instead of
//! silently picking another order.
//!
//! This is SimPulse, the deterministic kernel, not a `PulseWorkerShard`:
//! nothing here runs on an OS thread pool or harvests io_uring completions.
//!
//! [`NexusEngine::run_scheduled`]: crate::host::NexusEngine::run_scheduled

use crate::fuel::QuantumScheduler;
use crate::table::TenantId;
use vesper_simpulse::{Error, Journal, RunToken, Simulator, TaskId};

/// A SimPulse transition the adapter could not make. The engine treats the
/// affected slice as a scheduler violation and drains the remaining jobs
/// round robin, so no instance is stranded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SimPulseFault {
    Spawn { job: usize, error: Error },
    Dispatch(Error),
    Yield { task: TaskId, error: Error },
    Finish { task: TaskId, error: Error },
}

/// [`QuantumScheduler`] backed by a SimPulse simulator.
pub struct SimPulseScheduler {
    sim: Simulator<usize>,
    /// Job index to SimPulse task.
    tasks: Vec<Option<TaskId>>,
    tenants: Vec<Option<TenantId>>,
    /// Dispatch order: (task, tenant) per slice.
    dispatches: Vec<(TaskId, TenantId)>,
    faults: Vec<SimPulseFault>,
}

impl SimPulseScheduler {
    /// A fresh run queue whose selection order is determined by `seed`.
    pub fn new(seed: u64) -> Self {
        Self::from_simulator(Simulator::new(seed))
    }

    /// A run queue that must reproduce `journal` exactly.
    pub fn replay(journal: Journal) -> Result<Self, Error> {
        Ok(Self::from_simulator(Simulator::replay(journal)?))
    }

    fn from_simulator(sim: Simulator<usize>) -> Self {
        Self {
            sim,
            tasks: Vec::new(),
            tenants: Vec::new(),
            dispatches: Vec::new(),
            faults: Vec::new(),
        }
    }

    pub fn simulator(&self) -> &Simulator<usize> {
        &self.sim
    }
    /// SimPulse's recorded (or replayed) event journal.
    pub fn journal(&self) -> &Journal {
        self.sim.journal()
    }
    /// The SimPulse task carrying job `job`.
    pub fn task_of(&self, job: usize) -> Option<TaskId> {
        self.tasks.get(job).copied().flatten()
    }
    /// Every dispatched slice, in order, with the tenant it ran.
    pub fn dispatches(&self) -> &[(TaskId, TenantId)] {
        &self.dispatches
    }
    /// Transitions SimPulse rejected (empty on a well-formed run or replay).
    pub fn faults(&self) -> &[SimPulseFault] {
        &self.faults
    }
    /// Whether every admitted job finished.
    pub fn is_complete(&self) -> bool {
        self.sim.is_complete()
    }
    /// For a replay: whether the whole journal was consumed.
    pub fn finish_replay(&self) -> Result<(), Error> {
        self.sim.finish_replay()
    }

    fn tenant_of(&self, job: usize) -> TenantId {
        self.tenants.get(job).copied().flatten().unwrap_or_default()
    }
}

impl QuantumScheduler for SimPulseScheduler {
    type Token = RunToken<usize>;

    fn admit(&mut self, job: usize, tenant: TenantId) {
        if self.tasks.len() <= job {
            self.tasks.resize(job + 1, None);
            self.tenants.resize(job + 1, None);
        }
        self.tenants[job] = Some(tenant);
        match self.sim.spawn(job) {
            Ok(task) => self.tasks[job] = Some(task),
            Err(e) => self.faults.push(SimPulseFault::Spawn {
                job,
                error: e.error,
            }),
        }
    }

    fn next_job(&mut self) -> Option<(Self::Token, usize)> {
        match self.sim.next_task() {
            Ok(Some(token)) => {
                let job = *token.payload();
                self.dispatches.push((token.task(), self.tenant_of(job)));
                Some((token, job))
            }
            Ok(None) => None,
            Err(e) => {
                self.faults.push(SimPulseFault::Dispatch(e));
                None
            }
        }
    }

    fn preempted(&mut self, token: Self::Token) {
        let task = token.task();
        if let Err(e) = self.sim.yield_task(token) {
            self.faults.push(SimPulseFault::Yield {
                task,
                error: e.error,
            });
        }
    }

    fn finished(&mut self, token: Self::Token) {
        let task = token.task();
        if let Err(e) = self.sim.finish(token) {
            self.faults.push(SimPulseFault::Finish {
                task,
                error: e.error,
            });
        }
    }
}
