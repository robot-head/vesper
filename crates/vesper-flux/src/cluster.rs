//! Deterministic multi-worker Flux execution under SimPulse.
//!
//! `run_cluster` runs one compiled circuit on N workers plus a checkpoint
//! coordinator, each a SimPulse task. The replayable input log holds one
//! `TickInput` per epoch. In every epoch each worker reads its source split
//! (rows at positions `i % N == worker`), hash-partitions every delta row by
//! the configured key column of its input and sends one data message per
//! destination (possibly empty: it doubles as that link's end-of-epoch
//! marker). A worker processes epoch `e` only after receiving all N epoch-`e`
//! messages, then advances its `FrontierTicket`; the cluster watermark is the
//! minimum live hold, i.e. the first epoch not yet processed by every worker.
//!
//! Correctness of the partitioned execution rests on a static check: every
//! equi-join must join on columns that carry its inputs' partition keys
//! (propagated through filters, column-preserving maps and key-agreeing
//! unions). Linear operators then distribute over the partition sum, and
//! co-partitioned joins only pair rows on the same worker, so the sum of
//! worker outputs equals the single-executor output at every epoch.
//!
//! Checkpoints use aligned barriers (Chandy-Lamport specialised to FIFO links
//! with global barrier epochs): after sending its data for a barrier epoch a
//! worker sends a barrier on every outgoing link. A worker snapshots, durably
//! and incrementally into the CAS, once barriers from all N peers arrived and
//! it has processed exactly the barrier epoch; it processes no later epoch
//! before that snapshot. The coordinator commits a global manifest when all N
//! acknowledgements arrived and the frontier shows every worker past the
//! epoch. A barrier always follows the last epoch, so completion is a commit.
//!
//! Outputs are staged per (epoch, worker) in a transactional sink and become
//! visible only when a checkpoint covering the epoch commits, so visible
//! output never rolls back. A crash at any scheduled step discards every
//! worker's memory, all in-flight messages, staged outputs and pending
//! acknowledgements; recovery restores all workers from the last committed
//! global manifest (rehash-verified) and replays the input log from the next
//! epoch. This is global rollback recovery of a simulated protocol on one
//! host: SimPulse orders tasks and a seeded generator picks link delays. No
//! real network, process isolation or concurrent execution is involved.
use super::checkpoint::durable::{DurableCheckpointer, blob_id, read_blob, storage};
use super::checkpoint::{CheckpointLimits, identity};
use super::{Circuit, Executor, Field, Op, TickInput};
use crate::{Budget, Error, Frontier, FrontierTicket, Limits, Row, ZSet};
use std::collections::BTreeMap;
use vesper_loom_cas::{Domain, ObjectId, Store};
use vesper_simpulse::{Journal, Simulator};

const GLOBAL_MAGIC: &[u8; 8] = b"VFLUXGM1";
const GLOBAL_VERSION: u16 = 1;
const MAX_WORKERS: usize = 64;

/// Cluster shape, checkpoint cadence and simulation bounds.
#[derive(Clone, Debug)]
pub struct ClusterConfig {
    /// 1..=64 workers.
    pub workers: usize,
    /// Partition key column of each circuit input, in declaration order.
    pub partition_columns: Vec<usize>,
    /// Epochs between barriers (>= 1). A barrier always follows the last epoch.
    pub checkpoint_interval: u64,
    /// SimPulse scheduling seed; also seeds link delays.
    pub seed: u64,
    /// Link delays are drawn uniformly from 1..=max_link_delay_ns (>= 1).
    pub max_link_delay_ns: u64,
    /// Scheduled task dispatches before the run fails with a limit error.
    pub max_steps: u64,
    pub checkpoint_limits: CheckpointLimits,
}
impl ClusterConfig {
    pub fn new(workers: usize, partition_columns: Vec<usize>) -> Self {
        Self {
            workers,
            partition_columns,
            checkpoint_interval: 2,
            seed: 0,
            max_link_delay_ns: 5_000,
            max_steps: 1_000_000,
            checkpoint_limits: CheckpointLimits::default(),
        }
    }
    /// Reject bad shapes and circuits whose joins are not co-partitioned.
    pub fn validate(&self, circuit: &Circuit) -> Result<(), Error> {
        if self.workers == 0 || self.workers > MAX_WORKERS {
            return Err(Error::Limit("workers"));
        }
        if self.checkpoint_interval == 0 || self.max_link_delay_ns == 0 || self.max_steps == 0 {
            return Err(Error::Cluster("configuration"));
        }
        partition_keys(circuit, &self.partition_columns).map(|_| ())
    }
}

/// One committed global checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommittedCheckpoint {
    /// Last epoch included in the checkpoint.
    pub after_epoch: u64,
    /// Content identity of the global manifest.
    pub manifest: ObjectId,
    /// Scheduler step at which the coordinator committed it.
    pub step: u64,
}

/// One crash and rollback recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recovery {
    pub step: u64,
    /// Restored checkpoint, or `None` to restart from the initial state.
    pub restored: Option<CommittedCheckpoint>,
}

#[derive(Clone, Debug)]
pub struct ClusterRun {
    /// Visible output delta of every epoch, summed over workers.
    pub outputs: Vec<ZSet>,
    /// Sum of all visible output deltas.
    pub materialized: ZSet,
    pub steps: u64,
    pub messages: u64,
    pub recoveries: Vec<Recovery>,
    pub committed: Vec<CommittedCheckpoint>,
    /// Arrangements written and reused by all durable worker checkpoints.
    pub arrangements_written: usize,
    pub arrangements_reused: usize,
    /// Final cluster watermark (first epoch not processed by every worker).
    pub watermark: i64,
    /// SimPulse scheduling journal of the run.
    pub journal: Journal,
}

/// Propagate partition keys through the DAG; joins must be co-partitioned.
fn partition_keys(c: &Circuit, cols: &[usize]) -> Result<Vec<Option<usize>>, Error> {
    if cols.len() != c.inputs.len() {
        return Err(Error::Partition("partition column count"));
    }
    for (col, width) in cols.iter().zip(&c.inputs) {
        if col >= width {
            return Err(Error::Partition("partition column out of range"));
        }
    }
    let mut keys: Vec<Option<usize>> = Vec::with_capacity(c.nodes.len());
    for n in &c.nodes {
        let key = match &n.op {
            Op::Input(slot) => Some(cols[*slot]),
            Op::Filter(a, _) => keys[*a],
            Op::Map(a, fields) => keys[*a].and_then(|k| {
                fields
                    .iter()
                    .position(|f| matches!(f, Field::Column(c) if *c == k))
            }),
            Op::Union(a, b) => keys[*a].filter(|k| keys[*b] == Some(*k)),
            Op::Join { a, b, ka, kb } => {
                if keys[*a] != Some(*ka) || keys[*b] != Some(*kb) {
                    return Err(Error::Partition("join key is not the partition key"));
                }
                Some(*ka)
            }
        };
        keys.push(key);
    }
    Ok(keys)
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}
fn route(key: i64, workers: usize) -> usize {
    // workers <= 64, so both conversions are exact.
    (mix(u64::from_le_bytes(key.to_le_bytes())) % workers as u64) as usize
}

#[derive(Debug)]
enum Msg {
    Data {
        epoch: u64,
        from: usize,
        deltas: Vec<ZSet>,
    },
    Barrier {
        after_epoch: u64,
        from: usize,
    },
    Ack {
        after_epoch: u64,
        worker: usize,
        manifest: ObjectId,
    },
}

/// FIFO links with seeded delays: a link's delivery times never decrease and
/// equal times keep send order, so per-link order is preserved.
struct Net {
    queue: BTreeMap<(usize, u64, u64), Msg>,
    link_clock: BTreeMap<(usize, usize), u64>,
    rng: u64,
    seq: u64,
    max_delay: u64,
    sent: u64,
}
impl Net {
    fn send(&mut self, now: u64, from: usize, to: usize, msg: Msg) -> Result<(), Error> {
        self.rng = self.rng.wrapping_add(1);
        let delay = 1 + mix(self.rng) % self.max_delay;
        let earliest = now.checked_add(delay).ok_or(Error::Overflow)?;
        let clock = self.link_clock.entry((from, to)).or_insert(0);
        let at = earliest.max(*clock);
        *clock = at;
        self.seq = self.seq.checked_add(1).ok_or(Error::Overflow)?;
        self.sent = self.sent.checked_add(1).ok_or(Error::Overflow)?;
        self.queue.insert((to, at, self.seq), msg);
        Ok(())
    }
    fn recv(&mut self, to: usize, now: u64) -> Vec<Msg> {
        let keys: Vec<_> = self
            .queue
            .range((to, 0, 0)..=(to, now, u64::MAX))
            .map(|(k, _)| *k)
            .collect();
        keys.into_iter()
            .filter_map(|k| self.queue.remove(&k))
            .collect()
    }
    fn next_delivery(&self, to: usize) -> Option<u64> {
        self.queue
            .range((to, 0, 0)..=(to, u64::MAX, u64::MAX))
            .next()
            .map(|((_, at, _), _)| *at)
    }
    fn clear(&mut self) {
        self.queue.clear();
        self.link_clock.clear();
    }
}

struct Pending {
    from: Vec<bool>,
    received: usize,
    deltas: Vec<ZSet>,
}
struct Worker {
    exec: Executor,
    ckpt: DurableCheckpointer,
    next_epoch: u64,
    next_source: u64,
    inbox: BTreeMap<u64, Pending>,
    barriers: BTreeMap<u64, Vec<bool>>,
    ticket: Option<FrontierTicket>,
}

#[derive(Clone, Copy, Debug)]
enum Role {
    Worker(usize),
    Coordinator,
}

struct State<'a> {
    circuit: &'a Circuit,
    inputs: &'a [TickInput],
    cfg: &'a ClusterConfig,
    store: &'a Store,
    fingerprint: [u8; 32],
    workers: Vec<Worker>,
    net: Net,
    frontier: Frontier,
    acks: BTreeMap<u64, Vec<Option<ObjectId>>>,
    committed: Option<CommittedCheckpoint>,
    staged: BTreeMap<(u64, usize), ZSet>,
    visible: Vec<ZSet>,
    done: bool,
    recoveries: Vec<Recovery>,
    history: Vec<CommittedCheckpoint>,
    written: usize,
    reused: usize,
}

fn epoch_index(epoch: u64) -> Result<usize, Error> {
    usize::try_from(epoch).map_err(|_| Error::Overflow)
}
fn timestamp(epoch: u64) -> Result<i64, Error> {
    i64::try_from(epoch).map_err(|_| Error::Overflow)
}

impl State<'_> {
    fn n(&self) -> usize {
        self.cfg.workers
    }
    fn limits(&self) -> Limits {
        self.circuit.limits
    }
    fn last_epoch(&self) -> u64 {
        // inputs is non-empty and bounded by usize.
        self.inputs.len() as u64 - 1
    }
    fn is_barrier(&self, epoch: u64) -> bool {
        (epoch + 1).is_multiple_of(self.cfg.checkpoint_interval) || epoch == self.last_epoch()
    }

    fn fresh_workers(&mut self, start: u64, manifests: Option<&[ObjectId]>) -> Result<(), Error> {
        let mut workers = Vec::with_capacity(self.n());
        for w in 0..self.n() {
            let mut exec = self.circuit.clone().executor();
            let mut ckpt = DurableCheckpointer::new();
            if let Some(m) = manifests {
                let restored =
                    ckpt.restore(&mut exec, self.store, &m[w], self.cfg.checkpoint_limits)?;
                if restored.ticks != start {
                    return Err(Error::Checkpoint("worker checkpoint epoch"));
                }
            }
            let ticket = self.frontier.acquire(timestamp(start)?)?;
            workers.push(Worker {
                exec,
                ckpt,
                next_epoch: start,
                next_source: start,
                inbox: BTreeMap::new(),
                barriers: BTreeMap::new(),
                ticket: Some(ticket),
            });
        }
        self.workers = workers;
        Ok(())
    }

    fn retire_all(&mut self) -> Result<(), Error> {
        for w in &mut self.workers {
            if let Some(t) = w.ticket.take() {
                self.frontier.retire(t).map_err(|e| e.error)?;
            }
        }
        Ok(())
    }

    /// Crash: every volatile component is lost. Only the store (CAS objects)
    /// and the coordinator's committed manifest handle survive.
    fn crash(&mut self, step: u64) -> Result<(), Error> {
        self.net.clear();
        // Recovery reclaims the failed incarnation's holds before replacing it.
        self.retire_all()?;
        self.acks.clear();
        let (start, manifests) = match self.committed {
            None => (0, None),
            Some(c) => (
                c.after_epoch.checked_add(1).ok_or(Error::Overflow)?,
                Some(self.read_global(&c.manifest)?),
            ),
        };
        self.staged.retain(|(epoch, _), _| *epoch < start);
        self.frontier = Frontier::new(timestamp(start)?, self.n())?;
        self.fresh_workers(start, manifests.as_deref())?;
        self.recoveries.push(Recovery {
            step,
            restored: self.committed,
        });
        Ok(())
    }

    fn write_global(&self, after_epoch: u64, manifests: &[ObjectId]) -> Result<ObjectId, Error> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(GLOBAL_MAGIC);
        bytes.extend_from_slice(&GLOBAL_VERSION.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&self.fingerprint);
        bytes.extend_from_slice(
            &u32::try_from(self.n())
                .map_err(|_| Error::Overflow)?
                .to_le_bytes(),
        );
        for col in &self.cfg.partition_columns {
            bytes.extend_from_slice(
                &u32::try_from(*col)
                    .map_err(|_| Error::Overflow)?
                    .to_le_bytes(),
            );
        }
        bytes.extend_from_slice(&self.cfg.checkpoint_interval.to_le_bytes());
        bytes.extend_from_slice(&after_epoch.to_le_bytes());
        for m in manifests {
            bytes.extend_from_slice(m.digest.as_bytes());
        }
        Ok(self
            .store
            .put_blob(Domain::Artifact, &bytes)
            .map_err(storage)?
            .id)
    }

    /// Parse and check a global manifest against this cluster's configuration.
    fn read_global(&self, id: &ObjectId) -> Result<Vec<ObjectId>, Error> {
        let bytes = read_blob(self.store, id, self.cfg.checkpoint_limits.max_bytes)?;
        let cols = self.cfg.partition_columns.len();
        let expected_len = 8 + 2 + 2 + 32 + 4 + 4 * cols + 8 + 8 + 32 * self.n();
        if bytes.len() != expected_len {
            return Err(Error::Checkpoint("global manifest length"));
        }
        let mut expected = Vec::with_capacity(expected_len);
        expected.extend_from_slice(GLOBAL_MAGIC);
        expected.extend_from_slice(&GLOBAL_VERSION.to_le_bytes());
        expected.extend_from_slice(&0u16.to_le_bytes());
        expected.extend_from_slice(&self.fingerprint);
        expected.extend_from_slice(
            &u32::try_from(self.n())
                .map_err(|_| Error::Overflow)?
                .to_le_bytes(),
        );
        for col in &self.cfg.partition_columns {
            expected.extend_from_slice(
                &u32::try_from(*col)
                    .map_err(|_| Error::Overflow)?
                    .to_le_bytes(),
            );
        }
        expected.extend_from_slice(&self.cfg.checkpoint_interval.to_le_bytes());
        let header = expected.len();
        if bytes[..header] != expected[..] {
            return Err(Error::Checkpoint("global manifest identity"));
        }
        let after = u64::from_le_bytes(
            bytes[header..header + 8]
                .try_into()
                .map_err(|_| Error::Checkpoint("global manifest"))?,
        );
        if self.committed.map(|c| c.after_epoch) != Some(after) {
            return Err(Error::Checkpoint("global manifest epoch"));
        }
        let mut out = Vec::with_capacity(self.n());
        for chunk in bytes[header + 8..].as_chunks::<32>().0 {
            out.push(blob_id(*chunk));
        }
        Ok(out)
    }

    fn worker_step(&mut self, w: usize, now: u64) -> Result<bool, Error> {
        let n = self.n();
        let limits = self.limits();
        for msg in self.net.recv(w, now) {
            let worker = &mut self.workers[w];
            match msg {
                Msg::Data {
                    epoch,
                    from,
                    deltas,
                } => {
                    if epoch < worker.next_epoch {
                        return Err(Error::Cluster("stale epoch data"));
                    }
                    let pending = match worker.inbox.entry(epoch) {
                        std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
                        std::collections::btree_map::Entry::Vacant(e) => {
                            let mut empty = Vec::with_capacity(self.circuit.inputs.len());
                            for width in &self.circuit.inputs {
                                empty.push(ZSet::empty(*width, limits)?);
                            }
                            e.insert(Pending {
                                from: vec![false; n],
                                received: 0,
                                deltas: empty,
                            })
                        }
                    };
                    if std::mem::replace(&mut pending.from[from], true) {
                        return Err(Error::Cluster("duplicate epoch data"));
                    }
                    pending.received += 1;
                    for (acc, d) in pending.deltas.iter_mut().zip(&deltas) {
                        *acc = acc.plus(d, limits)?;
                    }
                }
                Msg::Barrier { after_epoch, from } => {
                    let seen = worker
                        .barriers
                        .entry(after_epoch)
                        .or_insert_with(|| vec![false; n]);
                    if std::mem::replace(&mut seen[from], true) {
                        return Err(Error::Cluster("duplicate barrier"));
                    }
                }
                Msg::Ack { .. } => return Err(Error::Cluster("misrouted acknowledgement")),
            }
        }
        // 1. Aligned barrier: snapshot exactly at the barrier epoch.
        let worker = &mut self.workers[w];
        if let Some((&after, seen)) = worker.barriers.first_key_value()
            && seen.iter().all(|s| *s)
            && worker.next_epoch == after + 1
        {
            let report =
                worker
                    .ckpt
                    .checkpoint(&worker.exec, self.store, self.cfg.checkpoint_limits)?;
            worker.barriers.remove(&after);
            self.written += report.written;
            self.reused += report.reused;
            self.net.send(
                now,
                w,
                n,
                Msg::Ack {
                    after_epoch: after,
                    worker: w,
                    manifest: report.manifest,
                },
            )?;
            return Ok(true);
        }
        // 2. Process the next epoch once complete and not held by a barrier.
        let held = worker
            .barriers
            .first_key_value()
            .is_some_and(|(&after, _)| after < worker.next_epoch);
        if !held
            && worker
                .inbox
                .get(&worker.next_epoch)
                .is_some_and(|p| p.received == n)
        {
            let epoch = worker.next_epoch;
            let pending = worker.inbox.remove(&epoch).ok_or(Error::Cluster("inbox"))?;
            let out = worker.exec.tick(&TickInput {
                deltas: pending.deltas,
            })?;
            let next = epoch.checked_add(1).ok_or(Error::Overflow)?;
            let ticket = worker
                .ticket
                .take()
                .ok_or(Error::Cluster("missing ticket"))?;
            match self.frontier.advance(ticket, timestamp(next)?) {
                Ok(t) => worker.ticket = Some(t),
                Err(e) => {
                    worker.ticket = Some(e.ticket);
                    return Err(e.error);
                }
            }
            worker.next_epoch = next;
            self.staged.insert((epoch, w), out);
            return Ok(true);
        }
        // 3. Read and scatter the next source epoch, at most one epoch ahead.
        if worker.next_source < self.inputs.len() as u64
            && worker.next_source <= worker.next_epoch + 1
        {
            let epoch = worker.next_source;
            let input = &self.inputs[epoch_index(epoch)?];
            if input.deltas.len() != self.circuit.inputs.len() {
                return Err(Error::InputCount);
            }
            let mut parts: Vec<Vec<Vec<(Row, i64)>>> =
                vec![vec![Vec::new(); self.circuit.inputs.len()]; n];
            for (slot, delta) in input.deltas.iter().enumerate() {
                if delta.width != self.circuit.inputs[slot] {
                    return Err(Error::Schema);
                }
                let col = self.cfg.partition_columns[slot];
                for (i, (row, weight)) in delta.entries().enumerate() {
                    if i % n == w {
                        parts[route(row[col], n)][slot].push((row.clone(), weight));
                    }
                }
            }
            for (dest, slots) in parts.into_iter().enumerate() {
                let mut deltas = Vec::with_capacity(slots.len());
                for (slot, entries) in slots.iter().enumerate() {
                    deltas.push(ZSet::consolidate(
                        self.circuit.inputs[slot],
                        entries,
                        limits,
                    )?);
                }
                self.net.send(
                    now,
                    w,
                    dest,
                    Msg::Data {
                        epoch,
                        from: w,
                        deltas,
                    },
                )?;
            }
            if self.is_barrier(epoch) {
                for dest in 0..n {
                    self.net.send(
                        now,
                        w,
                        dest,
                        Msg::Barrier {
                            after_epoch: epoch,
                            from: w,
                        },
                    )?;
                }
            }
            self.workers[w].next_source = epoch + 1;
            return Ok(true);
        }
        Ok(false)
    }

    fn coordinator_step(&mut self, now: u64, step: u64) -> Result<bool, Error> {
        let n = self.n();
        for msg in self.net.recv(n, now) {
            match msg {
                Msg::Ack {
                    after_epoch,
                    worker,
                    manifest,
                } => {
                    let slot = &mut self
                        .acks
                        .entry(after_epoch)
                        .or_insert_with(|| vec![None; n])[worker];
                    if slot.replace(manifest).is_some() {
                        return Err(Error::Cluster("duplicate acknowledgement"));
                    }
                }
                _ => return Err(Error::Cluster("misrouted message")),
            }
        }
        let Some((&after, acks)) = self.acks.first_key_value() else {
            return Ok(false);
        };
        if acks.iter().any(Option::is_none) {
            return Ok(false);
        }
        if self.frontier.watermark() <= timestamp(after)? {
            return Err(Error::Cluster("checkpoint ahead of frontier"));
        }
        if self.committed.is_some_and(|c| c.after_epoch >= after) {
            return Err(Error::Cluster("checkpoint regression"));
        }
        let manifests: Vec<ObjectId> = acks.iter().flatten().copied().collect();
        let manifest = self.write_global(after, &manifests)?;
        // Publish staged outputs of every epoch the checkpoint covers.
        let mut published = Vec::new();
        for epoch in self.visible.len() as u64..=after {
            let mut sum =
                ZSet::empty(self.circuit.nodes[self.circuit.output].width, self.limits())?;
            for w in 0..n {
                let out = self
                    .staged
                    .get(&(epoch, w))
                    .ok_or(Error::Cluster("missing staged output"))?;
                sum = sum.plus(out, self.limits())?;
            }
            published.push(sum);
        }
        // Commit point: nothing fallible follows.
        self.staged.retain(|(epoch, _), _| *epoch > after);
        self.visible.extend(published);
        self.acks.remove(&after);
        let commit = CommittedCheckpoint {
            after_epoch: after,
            manifest,
            step,
        };
        self.committed = Some(commit);
        self.history.push(commit);
        if after == self.last_epoch() {
            self.done = true;
        }
        Ok(true)
    }
}

fn sim_error<E>(_: E) -> Error {
    Error::Cluster("simulator")
}

/// Run `inputs` (one tick per epoch) on a simulated cluster, crashing the
/// cluster at each scheduler step listed in `crash_steps` and recovering from
/// the last committed checkpoint. Deterministic for a fixed configuration,
/// seed, input and crash schedule.
pub fn run_cluster(
    circuit: &Circuit,
    inputs: &[TickInput],
    cfg: &ClusterConfig,
    crash_steps: &[u64],
    store: &Store,
) -> Result<ClusterRun, Error> {
    cfg.validate(circuit)?;
    let mut budget = Budget::new(Limits {
        max_work: cfg.checkpoint_limits.max_work.min(circuit.limits.max_work),
        ..circuit.limits
    });
    let fingerprint = identity(circuit, None, cfg.checkpoint_limits, &mut budget)?;
    let width = circuit.nodes[circuit.output].width;
    if inputs.is_empty() {
        return Ok(ClusterRun {
            outputs: Vec::new(),
            materialized: ZSet::empty(width, circuit.limits)?,
            steps: 0,
            messages: 0,
            recoveries: Vec::new(),
            committed: Vec::new(),
            arrangements_written: 0,
            arrangements_reused: 0,
            watermark: 0,
            journal: Simulator::<()>::new(cfg.seed).journal().clone(),
        });
    }
    u64::try_from(inputs.len()).map_err(|_| Error::Overflow)?;
    let mut state = State {
        circuit,
        inputs,
        cfg,
        store,
        fingerprint,
        workers: Vec::new(),
        net: Net {
            queue: BTreeMap::new(),
            link_clock: BTreeMap::new(),
            rng: mix(cfg.seed ^ 0x666c_7578_6e65_7400),
            seq: 0,
            max_delay: cfg.max_link_delay_ns,
            sent: 0,
        },
        frontier: Frontier::new(0, cfg.workers)?,
        acks: BTreeMap::new(),
        committed: None,
        staged: BTreeMap::new(),
        visible: Vec::new(),
        done: false,
        recoveries: Vec::new(),
        history: Vec::new(),
        written: 0,
        reused: 0,
    };
    state.fresh_workers(0, None)?;
    let mut sim = Simulator::new(cfg.seed);
    for w in 0..cfg.workers {
        sim.spawn(Role::Worker(w)).map_err(sim_error)?;
    }
    sim.spawn(Role::Coordinator).map_err(sim_error)?;
    let mut steps = 0u64;
    while let Some(token) = sim.next_task().map_err(sim_error)? {
        if state.done {
            sim.finish(token).map_err(sim_error)?;
            continue;
        }
        steps = steps.checked_add(1).ok_or(Error::Overflow)?;
        if steps > cfg.max_steps {
            return Err(Error::Limit("simulation steps"));
        }
        let now = sim.now_ns();
        let role = *token.payload();
        let acted = if crash_steps.contains(&steps) {
            state.crash(steps)?;
            true
        } else {
            match role {
                Role::Worker(w) => state.worker_step(w, now)?,
                Role::Coordinator => state.coordinator_step(now, steps)?,
            }
        };
        if acted {
            sim.yield_task(token).map_err(sim_error)?;
        } else {
            let endpoint = match role {
                Role::Worker(w) => w,
                Role::Coordinator => cfg.workers,
            };
            let wait = state
                .net
                .next_delivery(endpoint)
                .map_or(cfg.max_link_delay_ns, |at| at.saturating_sub(now).max(1));
            sim.sleep(token, wait).map_err(sim_error)?;
        }
    }
    if !state.done || !sim.is_complete() {
        return Err(Error::Cluster("quiescent before completion"));
    }
    let watermark = state.frontier.watermark();
    // Cross-check: visible outputs integrate to the workers' summed views.
    let mut materialized = ZSet::empty(width, circuit.limits)?;
    for out in &state.visible {
        materialized = materialized.plus(out, circuit.limits)?;
    }
    let mut views = ZSet::empty(width, circuit.limits)?;
    for w in &state.workers {
        views = views.plus(w.exec.materialized(), circuit.limits)?;
    }
    if views != materialized {
        return Err(Error::Cluster("materialized view mismatch"));
    }
    state.retire_all()?;
    if state.frontier.live_holds() != 0 {
        return Err(Error::Cluster("leaked frontier hold"));
    }
    Ok(ClusterRun {
        outputs: state.visible,
        materialized,
        steps,
        messages: state.net.sent,
        recoveries: state.recoveries,
        committed: state.history,
        arrangements_written: state.written,
        arrangements_reused: state.reused,
        watermark,
        journal: sim.journal().clone(),
    })
}
