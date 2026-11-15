//! Deterministic multi-worker Flux execution under SimPulse, with durable
//! sources and sinks, partial recovery, coordinator failover, aligned and
//! unaligned checkpoints, rescaling and checkpoint garbage collection.
//!
//! # Execution
//!
//! [`run_cluster_with`] runs a circuit on N workers plus a checkpoint
//! coordinator, each a SimPulse task, over an [`EpochSource`] (a replayable
//! input log: an [`super::InputLog`] on disk, or a slice). In every epoch each
//! worker reads its split of the epoch (rows at positions `i % N == worker`),
//! routes every row by the partition column of its input and sends one data
//! message per destination (possibly empty: it doubles as that link's
//! end-of-epoch marker).
//!
//! The circuit is *planned* first (`Circuit::plan`): partition keys are
//! propagated through filters, column-preserving maps and key-agreeing
//! unions, and every join input whose key is not its join key gets an
//! exchange that re-shuffles its delta by the join key. A worker evaluates an
//! epoch's differentiated program in order; at an exchange it scatters its
//! local delta to the workers that own each key and waits for all N parts.
//! Linear operators distribute over the partition sum and every join pairs
//! rows on the worker that owns their key, so the summed worker outputs equal
//! the single-executor outputs at every epoch.
//!
//! # Checkpoints and the sink
//!
//! Barrier epochs are fixed by the log position (every `checkpoint_interval`
//! epochs and after the last one). After sending its data for a barrier
//! epoch, a worker sends a barrier on every link.
//!
//! * Aligned: a worker snapshots once barriers from all N peers arrived and
//!   it has processed exactly the barrier epoch, and processes no later epoch
//!   before that.
//! * Unaligned: a worker snapshots its executor as soon as the first barrier
//!   arrives, whatever epoch it reached, and keeps processing. Every message
//!   for an epoch between its snapshot and the barrier epoch (received before
//!   or after the snapshot) is recorded as channel state; once the worker has
//!   processed the barrier epoch it writes the channel state as a blob next to
//!   the snapshot. Restore replays the channel state locally to reach the
//!   barrier epoch.
//!
//! Snapshots are durable, incremental LSM spines in the CAS
//! ([`super::DurableCheckpointer`]). A worker's acknowledgement carries its
//! manifest, its channel blob and the outputs of the epochs it computed since
//! the last commit it knows of. The coordinator then, in three separate
//! steps, (1) writes the global manifest to the CAS, (2) commits the covered
//! outputs together with the source offset and the global manifest handle in
//! one sink transaction ([`super::CommitSink`], e.g. a Quarry `WriteTxn`) and
//! (3) tells every worker, and collects the objects only the superseded
//! checkpoint referenced. Visible output therefore never rolls back
//! (exactly-once output), and the sink's commit record is the only durable
//! "how far" state.
//!
//! # Faults
//!
//! * Whole cluster ([`FaultKind::Cluster`]): every worker, the coordinator,
//!   all in-flight messages and staged outputs are lost. All workers restore
//!   from the sink's last commit and replay the log from the next epoch.
//! * One worker ([`FaultKind::Worker`]): only that worker's memory and the
//!   messages addressed to it are lost. It restores from the last commit;
//!   every peer resends the messages it buffered for it (upstream backup:
//!   each worker keeps what it sent since the last commit it knows of), and
//!   its regenerated sends are deduplicated by the receivers, which check
//!   that a duplicate equals the original (replay must be deterministic).
//!   Peers keep running; nothing else rolls back.
//! * Coordinator ([`FaultKind::Coordinator`]): pending acknowledgements,
//!   staged outputs and an uncommitted global manifest are lost. The new
//!   coordinator reopens the sink, reads the last commit and asks every
//!   worker to resend its acknowledgements.
//!
//! # Rescaling
//!
//! If the last commit was made by a different worker count or different
//! partition columns, the run first rescales: it restores every old worker,
//! sums their input integrals, routes them to the new workers, recomputes
//! every derived integral (gathering exchange inputs globally), checkpoints
//! the new workers, and commits the rescaled global manifest with no outputs.
//!
//! # Scope
//!
//! This is a protocol model on one host. SimPulse orders task steps
//! deterministically and a seeded generator picks link delays; no real
//! network, process isolation or concurrency is involved. The frontier
//! tracker is modelled as a service that survives a coordinator failover (a
//! whole-cluster crash rebuilds it). Garbage collection deletes object files
//! directly; it assumes one cluster per circuit fingerprint writes a store.
use super::checkpoint::durable::{
    BATCH_MAGIC, DurableCheckpointer, MANIFEST_MAGIC, blob_id, parse_manifest, read_blob, storage,
};
use super::checkpoint::{CheckpointLimits, identity};
use super::sink::{CommitRecord, CommitSink, MemorySink};
use super::{Circuit, Executor, Op, Staged, TickInput, apply};
use crate::{Error, Frontier, FrontierTicket, Limits, ZSet};
use std::collections::{BTreeMap, BTreeSet};
use vesper_loom_cas::{Domain, ObjectId, Store};
use vesper_simpulse::{Journal, Simulator};

const GLOBAL_MAGIC: &[u8; 8] = b"VFLUXGM2";
const CHANNEL_MAGIC: &[u8; 8] = b"VFLUXCH1";
const BLOB_VERSION: u16 = 1;
const MAX_WORKERS: usize = 64;

/// How workers take their part of a checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointMode {
    /// Barrier alignment: snapshot exactly at the barrier epoch, blocking.
    Aligned,
    /// Snapshot at the first barrier and persist the channel state.
    Unaligned,
}

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
    pub mode: CheckpointMode,
    /// Delete objects only superseded checkpoints reference.
    pub collect_garbage: bool,
    /// Parallelism changes made while the job runs, in increasing epoch
    /// order. `workers` is the parallelism before the first of them.
    pub live_rescales: Vec<LiveRescale>,
}

/// Change the number of workers after a barrier epoch without stopping the
/// job: from `after_epoch + 1` on, `workers` workers run it. The keyed state
/// moves between running workers as messages; nothing is restored from
/// storage unless a fault hits the reconfiguration (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveRescale {
    /// A barrier epoch: `(after_epoch + 1) % checkpoint_interval == 0`.
    pub after_epoch: u64,
    /// 1..=64 workers.
    pub workers: usize,
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
            mode: CheckpointMode::Aligned,
            collect_garbage: true,
            live_rescales: Vec::new(),
        }
    }
    /// Reject bad shapes and circuits that cannot be planned.
    pub fn validate(&self, circuit: &Circuit) -> Result<(), Error> {
        if self.workers == 0 || self.workers > MAX_WORKERS {
            return Err(Error::Limit("workers"));
        }
        if self.checkpoint_interval == 0 || self.max_link_delay_ns == 0 || self.max_steps == 0 {
            return Err(Error::Cluster("configuration"));
        }
        let mut previous: Option<u64> = None;
        let mut layout = self.workers;
        for r in &self.live_rescales {
            if r.workers == layout {
                return Err(Error::Cluster("live rescale schedule"));
            }
            layout = r.workers;
            if r.workers == 0 || r.workers > MAX_WORKERS {
                return Err(Error::Limit("workers"));
            }
            let barrier = r
                .after_epoch
                .checked_add(1)
                .ok_or(Error::Overflow)?
                .is_multiple_of(self.checkpoint_interval);
            if !barrier || previous.is_some_and(|p| r.after_epoch <= p) {
                return Err(Error::Cluster("live rescale schedule"));
            }
            previous = Some(r.after_epoch);
        }
        circuit.plan(&self.partition_columns).map(|_| ())
    }
    /// Workers that run `epoch` under the live rescale schedule.
    pub fn workers_at(&self, epoch: u64) -> usize {
        self.live_rescales
            .iter()
            .rev()
            .find(|r| r.after_epoch < epoch)
            .map_or(self.workers, |r| r.workers)
    }
    /// Live rescales applied before `epoch` (the layout generation).
    fn generation_at(&self, epoch: u64) -> usize {
        self.live_rescales
            .iter()
            .filter(|r| r.after_epoch < epoch)
            .count()
    }
    /// The largest worker count of the schedule.
    fn max_workers(&self) -> usize {
        self.live_rescales
            .iter()
            .map(|r| r.workers)
            .fold(self.workers, usize::max)
    }
    /// The first epoch at or after `from` that worker `w` runs, if any.
    fn activation(&self, w: usize, from: u64) -> Option<u64> {
        if w < self.workers_at(from) {
            return Some(from);
        }
        self.live_rescales
            .iter()
            .filter(|r| r.after_epoch >= from && w < r.workers)
            .map(|r| r.after_epoch + 1)
            .next()
    }
    /// Exchanges the planner inserts for this configuration.
    pub fn exchanges(&self, circuit: &Circuit) -> Result<usize, Error> {
        Ok(circuit.plan(&self.partition_columns)?.0.exchange_count())
    }
}

/// A replayable, epoch-indexed input source.
pub trait EpochSource {
    fn epochs(&self) -> u64;
    fn read(&self, epoch: u64) -> Result<TickInput, Error>;
}
impl EpochSource for Vec<TickInput> {
    fn epochs(&self) -> u64 {
        self.as_slice().epochs()
    }
    fn read(&self, epoch: u64) -> Result<TickInput, Error> {
        self.as_slice().read(epoch)
    }
}
struct Slice<'a>(&'a [TickInput]);
impl EpochSource for Slice<'_> {
    fn epochs(&self) -> u64 {
        self.0.epochs()
    }
    fn read(&self, epoch: u64) -> Result<TickInput, Error> {
        self.0.read(epoch)
    }
}
impl EpochSource for [TickInput] {
    fn epochs(&self) -> u64 {
        self.len() as u64
    }
    fn read(&self, epoch: u64) -> Result<TickInput, Error> {
        let i = usize::try_from(epoch).map_err(|_| Error::Overflow)?;
        self.get(i).cloned().ok_or(Error::InputCount)
    }
}
impl EpochSource for super::InputLog {
    fn epochs(&self) -> u64 {
        self.len()
    }
    fn read(&self, epoch: u64) -> Result<TickInput, Error> {
        super::InputLog::read(self, epoch)
    }
}

/// What fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FaultKind {
    Cluster,
    Worker(usize),
    Coordinator,
}
/// A fault injected at a scheduler step (which it consumes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub step: u64,
    pub kind: FaultKind,
}
impl Fault {
    pub fn cluster(step: u64) -> Self {
        Self {
            step,
            kind: FaultKind::Cluster,
        }
    }
    pub fn worker(step: u64, worker: usize) -> Self {
        Self {
            step,
            kind: FaultKind::Worker(worker),
        }
    }
    pub fn coordinator(step: u64) -> Self {
        Self {
            step,
            kind: FaultKind::Coordinator,
        }
    }
}

/// Progress of the oldest uncommitted checkpoint when a fault hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckpointPhase {
    /// No barrier past the last commit is anywhere in the cluster.
    Idle,
    /// Barriers are in flight or partly received; nobody has snapshotted.
    Injected,
    /// Some workers snapshotted (or acknowledged), not all acknowledged.
    Snapshotting,
    /// The coordinator holds every acknowledgement.
    Acknowledged,
    /// The global manifest is in the CAS; the sink has not committed it.
    ManifestWritten,
    /// The sink committed; workers have not been told.
    Committed,
}

/// One committed global checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommittedCheckpoint {
    /// Last epoch included in the checkpoint.
    pub after_epoch: u64,
    /// Content identity of the global manifest.
    pub manifest: ObjectId,
    /// Scheduler step at which the sink committed it (0: before this run).
    pub step: u64,
}

/// One fault and its recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recovery {
    pub step: u64,
    pub kind: FaultKind,
    /// Checkpoint restored, or `None` for the initial state. The coordinator
    /// restores no worker state; it reports the commit it read.
    pub restored: Option<CommittedCheckpoint>,
    pub phase: CheckpointPhase,
    /// A worker or coordinator fault during a live reconfiguration (between
    /// the commit at its barrier and the commit of the new layout) is
    /// handled as a whole-cluster recovery.
    pub escalated: bool,
}

/// A live rescale performed during the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveMigration {
    pub after_epoch: u64,
    pub from_workers: usize,
    pub to_workers: usize,
    /// Keyed rows (input and exchange integrals) that changed owner.
    pub rows_moved: usize,
    /// Keyed rows in total at the barrier.
    pub rows_total: usize,
    /// Global manifest of the new layout, committed with no outputs.
    pub manifest: ObjectId,
    /// Scheduler steps from the commit at the barrier to the commit of the
    /// new layout. Workers that finished migrating kept processing.
    pub steps: u64,
}

/// A rescale performed before the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rescale {
    pub from_workers: usize,
    pub to_workers: usize,
    pub from_columns: Vec<usize>,
    pub after_epoch: u64,
    pub manifest: ObjectId,
}

/// Objects deleted by checkpoint garbage collection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Collected when a commit superseded a checkpoint.
    pub superseded: usize,
    /// Collected by the final sweep (orphans of failed incarnations and
    /// uncommitted checkpoints).
    pub swept: usize,
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub struct ClusterRun {
    /// First epoch this run processed.
    pub start_epoch: u64,
    /// Output delta of every epoch from `start_epoch`, summed over workers.
    pub outputs: Vec<ZSet>,
    /// The full materialized view (summed worker views), checked against the
    /// sum of every output the sink holds.
    pub materialized: ZSet,
    pub steps: u64,
    pub messages: u64,
    pub recoveries: Vec<Recovery>,
    pub committed: Vec<CommittedCheckpoint>,
    pub rescale: Option<Rescale>,
    /// Live rescales completed in this run (not counting ones redone after
    /// an escalated fault: those appear as offline `recovery_rescales`).
    pub migrations: Vec<LiveMigration>,
    /// Offline rescales done by whole-cluster recoveries that restored a
    /// commit made under a layout the schedule had already left.
    pub recovery_rescales: usize,
    /// Exchange nodes in the plan (re-shuffled join inputs).
    pub exchanges: usize,
    /// Spine batches written and reused, and compaction merges, of all
    /// durable worker checkpoints.
    pub arrangements_written: usize,
    pub arrangements_reused: usize,
    pub batches_merged: usize,
    /// Rows written into spine batches by worker checkpoints.
    pub rows_written: usize,
    /// Channel-state blobs written by unaligned checkpoints.
    pub channel_blobs: usize,
    /// Messages resent from upstream buffers to recovering workers.
    pub resent: u64,
    /// Duplicate messages and acknowledgements dropped.
    pub duplicates: u64,
    pub gc: GcReport,
    /// Final cluster watermark (first epoch not processed by every worker).
    pub watermark: i64,
    /// SimPulse scheduling journal of the run.
    pub journal: Journal,
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
/// Split a canonical set by the owner of `row[col]`.
fn split(set: &ZSet, col: usize, workers: usize) -> Vec<ZSet> {
    let mut parts: Vec<ZSet> = (0..workers)
        .map(|_| ZSet {
            width: set.width,
            rows: BTreeMap::new(),
        })
        .collect();
    for (row, w) in &set.rows {
        parts[route(row[col], workers)].rows.insert(row.clone(), *w);
    }
    parts
}

#[derive(Clone, Debug, PartialEq)]
enum Part {
    Data(Vec<ZSet>),
    Shuffle(usize, ZSet),
}

#[derive(Clone, Debug)]
enum Msg {
    Data {
        epoch: u64,
        from: usize,
        deltas: Vec<ZSet>,
    },
    Shuffle {
        epoch: u64,
        exchange: usize,
        from: usize,
        delta: ZSet,
    },
    Barrier {
        after_epoch: u64,
        from: usize,
    },
    Ack {
        after_epoch: u64,
        worker: usize,
        manifest: ObjectId,
        channel: Option<ObjectId>,
        outputs: Vec<(u64, ZSet)>,
    },
    Committed {
        after_epoch: u64,
    },
    Resync {
        committed: Option<u64>,
    },
    /// Coordinator to every worker: the barrier `after_epoch` of a live
    /// rescale is committed; move keyed state to the new layout.
    Reconfigure {
        after_epoch: u64,
    },
    /// Old worker `from` to new worker: its keyed integrals (inputs and
    /// exchanges, in node order) whose keys the new layout gives the receiver.
    Migrate {
        after_epoch: u64,
        from: usize,
        parts: Vec<ZSet>,
    },
    /// New worker to coordinator: its migrated state is checkpointed.
    Migrated {
        after_epoch: u64,
        worker: usize,
        manifest: ObjectId,
    },
}
impl Msg {
    /// Epoch an upstream-buffered message belongs to.
    fn epoch(&self) -> u64 {
        match self {
            Msg::Data { epoch, .. } | Msg::Shuffle { epoch, .. } => *epoch,
            Msg::Barrier { after_epoch, .. } => *after_epoch,
            _ => u64::MAX,
        }
    }
}

/// FIFO links with seeded delays: a link's delivery times never decrease and
/// equal times keep send order, so per-link order is preserved.
struct Net {
    queue: BTreeMap<(usize, u64, u64), (usize, Msg)>,
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
        self.queue.insert((to, at, self.seq), (from, msg));
        Ok(())
    }
    fn recv(&mut self, to: usize, now: u64) -> Vec<Msg> {
        let keys: Vec<_> = self
            .queue
            .range((to, 0, 0)..=(to, now, u64::MAX))
            .map(|(k, _)| *k)
            .collect();
        keys.into_iter()
            .filter_map(|k| self.queue.remove(&k).map(|(_, m)| m))
            .collect()
    }
    fn next_delivery(&self, to: usize) -> Option<u64> {
        self.queue
            .range((to, 0, 0)..=(to, u64::MAX, u64::MAX))
            .next()
            .map(|((_, at, _), _)| *at)
    }
    /// Lose every message addressed to `to`.
    fn drop_to(&mut self, to: usize) {
        self.queue.retain(|(dest, _, _), _| *dest != to);
    }
    fn clear(&mut self) {
        self.queue.clear();
        self.link_clock.clear();
    }
    fn barriers_in_flight(&self, after: Option<u64>) -> bool {
        self.queue.values().any(|(_, m)| {
            matches!(m, Msg::Barrier { after_epoch, .. } if after.is_none_or(|c| *after_epoch > c))
        })
    }
}

#[derive(Default)]
struct Inbox {
    data: BTreeMap<usize, Vec<ZSet>>,
    shuffles: BTreeMap<(usize, usize), ZSet>,
}
struct Capture {
    /// First epoch not in the snapshot (its tick count).
    start: u64,
    manifest: ObjectId,
    parts: Vec<(u64, usize, Part)>,
}
struct Worker {
    exec: Executor,
    ckpt: DurableCheckpointer,
    next_epoch: u64,
    next_source: u64,
    inbox: BTreeMap<u64, Inbox>,
    /// The tick of `next_epoch`, paused at an exchange already scattered.
    staged: Option<Staged>,
    barriers: BTreeMap<u64, Vec<bool>>,
    last_snapshot: Option<u64>,
    captures: BTreeMap<u64, Capture>,
    acked: BTreeMap<u64, (ObjectId, Option<ObjectId>)>,
    outputs: BTreeMap<u64, ZSet>,
    /// Upstream backup: messages sent since the last commit this worker knows of.
    sent: Vec<(usize, Msg)>,
    known: Option<u64>,
    resend: Vec<usize>,
    ticket: Option<FrontierTicket>,
    /// Live rescales applied to `exec`'s layout (see `ClusterConfig`).
    generation: usize,
    /// Live rescale barriers whose reconfiguration notice arrived.
    reconfigure: BTreeSet<u64>,
    /// Barrier whose keyed state this worker already sent out.
    migrated_out: Option<u64>,
    /// Migrated parts received, per barrier and old worker.
    migration_in: BTreeMap<u64, BTreeMap<usize, Vec<ZSet>>>,
}

/// A worker's part of a global checkpoint: its manifest and channel blob.
type WorkerCheckpoint = (ObjectId, Option<ObjectId>);

enum Pending {
    Idle,
    Written {
        after: u64,
        global: ObjectId,
        /// Workers of the layout the record names.
        workers: usize,
    },
    Committed {
        after: u64,
        previous: Option<ObjectId>,
    },
}
struct Coordinator {
    acks: BTreeMap<u64, Vec<Option<WorkerCheckpoint>>>,
    staged: BTreeMap<(u64, usize), ZSet>,
    pending: Pending,
    committed: Option<CommitRecord>,
    resync: bool,
    /// Checkpoints of the new layout during a live rescale.
    migrated: BTreeMap<usize, ObjectId>,
    /// Reconfiguration notices sent for this barrier (and at which step).
    reconfiguring: Option<(u64, u64)>,
}
impl Coordinator {
    fn new(committed: Option<CommitRecord>, resync: bool) -> Self {
        Self {
            acks: BTreeMap::new(),
            staged: BTreeMap::new(),
            pending: Pending::Idle,
            committed,
            resync,
            migrated: BTreeMap::new(),
            reconfiguring: None,
        }
    }
    fn epoch(&self) -> Option<u64> {
        self.committed.map(|c| c.after_epoch)
    }
}

#[derive(Clone, Copy, Debug)]
enum Role {
    Worker(usize),
    Coordinator,
}

/// A parsed global manifest.
struct Global {
    plan_fp: [u8; 32],
    workers: usize,
    columns: Vec<usize>,
    after_epoch: u64,
    entries: Vec<(ObjectId, Option<ObjectId>)>,
}

/// One planned layout: the planned circuit, its fingerprint and node keys.
struct Plan {
    circuit: Circuit,
    fingerprint: [u8; 32],
    keys: Vec<Option<usize>>,
    columns: Vec<usize>,
}
impl Plan {
    fn new(base: &Circuit, columns: &[usize], limits: CheckpointLimits) -> Result<Self, Error> {
        let (circuit, keys) = base.plan(columns)?;
        let mut budget = limits.budget(&circuit);
        let fingerprint = identity(&circuit, None, limits, &mut budget)?;
        Ok(Self {
            circuit,
            fingerprint,
            keys,
            columns: columns.to_vec(),
        })
    }
}

struct State<'a> {
    base: &'a Circuit,
    base_fp: [u8; 32],
    plan: Plan,
    source: &'a dyn EpochSource,
    sink: &'a mut dyn CommitSink,
    cfg: &'a ClusterConfig,
    store: &'a Store,
    epochs: u64,
    workers: Vec<Worker>,
    net: Net,
    frontier: Frontier,
    coord: Coordinator,
    done: bool,
    visible: Vec<ZSet>,
    recoveries: Vec<Recovery>,
    history: Vec<CommittedCheckpoint>,
    /// Plan fingerprints whose objects the final sweep may collect.
    sweep_fps: Vec<[u8; 32]>,
    written: usize,
    reused: usize,
    merged: usize,
    rows_written: usize,
    channels: usize,
    resent: u64,
    duplicates: u64,
    gc: GcReport,
    /// Endpoint of the coordinator: one past the largest worker index.
    coord_id: usize,
    migrations: Vec<LiveMigration>,
    recovery_rescales: usize,
    /// Keyed rows moved / in total during the current live rescale.
    moving: (usize, usize),
}

fn timestamp(epoch: u64) -> Result<i64, Error> {
    i64::try_from(epoch).map_err(|_| Error::Overflow)
}
fn next(epoch: Option<u64>) -> Result<u64, Error> {
    epoch.map_or(Ok(0), |e| e.checked_add(1).ok_or(Error::Overflow))
}

struct Bytes<'a> {
    b: &'a [u8],
    at: usize,
}
impl<'a> Bytes<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).ok_or(Error::Overflow)?;
        let s = self
            .b
            .get(self.at..end)
            .ok_or(Error::Checkpoint("truncated"))?;
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| Error::Overflow)?,
        ))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| Error::Overflow)?,
        ))
    }
    fn usize32(&mut self) -> Result<usize, Error> {
        usize::try_from(self.u32()?).map_err(|_| Error::Overflow)
    }
    fn digest(&mut self) -> Result<[u8; 32], Error> {
        self.take(32)?.try_into().map_err(|_| Error::Overflow)
    }
}
fn put_u32(out: &mut Vec<u8>, n: usize) -> Result<(), Error> {
    out.extend_from_slice(&u32::try_from(n).map_err(|_| Error::Overflow)?.to_le_bytes());
    Ok(())
}
fn put_set(out: &mut Vec<u8>, set: &ZSet) -> Result<(), Error> {
    put_u32(out, set.width)?;
    put_u32(out, set.len())?;
    for (row, w) in set.entries() {
        for c in row {
            out.extend_from_slice(&c.to_le_bytes());
        }
        out.extend_from_slice(&w.to_le_bytes());
    }
    Ok(())
}
fn read_set(r: &mut Bytes<'_>, width: usize, rows_left: &mut usize) -> Result<ZSet, Error> {
    if r.usize32()? != width {
        return Err(Error::Checkpoint("channel schema"));
    }
    let count = r.usize32()?;
    *rows_left = rows_left
        .checked_sub(count)
        .ok_or(Error::Limit("checkpoint state"))?;
    let mut rows = BTreeMap::new();
    for _ in 0..count {
        let bytes = r.take((width + 1) * 8)?;
        let cells = bytes.as_chunks::<8>().0;
        let row: Vec<i64> = cells[..width]
            .iter()
            .map(|c| i64::from_le_bytes(*c))
            .collect();
        let w = i64::from_le_bytes(cells[width]);
        if w == 0 || rows.last_key_value().is_some_and(|(l, _)| l >= &row) {
            return Err(Error::Checkpoint("noncanonical rows"));
        }
        rows.insert(row, w);
    }
    Ok(ZSet { width, rows })
}

fn remove_object(store: &Store, id: &ObjectId) -> Result<u64, Error> {
    let path = store.object_path(id);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(_) => return Err(Error::Storage("io")),
    };
    if !meta.is_file() {
        return Err(Error::Storage("wrong object kind"));
    }
    // Objects are read-only; Windows refuses to delete read-only files.
    let mut perms = meta.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    let _ = std::fs::set_permissions(&path, perms);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(meta.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(_) => Err(Error::Storage("io")),
    }
}

impl State<'_> {
    /// Workers that run `epoch`.
    fn n_at(&self, epoch: u64) -> usize {
        self.cfg.workers_at(epoch)
    }
    fn gen_at(&self, epoch: u64) -> usize {
        self.cfg.generation_at(epoch)
    }
    /// Whether barrier `b` starts a live rescale inside this run.
    fn rescales_after(&self, b: u64) -> bool {
        b < self.last_epoch() && self.gen_at(b + 1) > self.gen_at(b)
    }
    /// Inside a live reconfiguration: the last commit is a rescale barrier
    /// still made under the old layout.
    fn in_reconfiguration(&self, record: Option<CommitRecord>) -> Option<u64> {
        let r = record?;
        (self.rescales_after(r.after_epoch) && r.workers as usize == self.n_at(r.after_epoch))
            .then_some(r.after_epoch)
    }
    /// Whether worker `w` holds valid state to run `epoch`.
    fn runs(&self, w: usize, epoch: u64) -> bool {
        w < self.n_at(epoch) && self.workers[w].generation == self.gen_at(epoch)
    }
    fn limits(&self) -> Limits {
        self.plan.circuit.limits
    }
    fn last_epoch(&self) -> u64 {
        self.epochs - 1
    }
    fn is_barrier(&self, epoch: u64) -> bool {
        (epoch + 1).is_multiple_of(self.cfg.checkpoint_interval) || epoch == self.last_epoch()
    }

    // ------------------------------------------------------------------
    // Global manifests and channel blobs
    // ------------------------------------------------------------------

    fn write_global(
        &self,
        plan: &Plan,
        workers: usize,
        after_epoch: u64,
        entries: &[(ObjectId, Option<ObjectId>)],
    ) -> Result<ObjectId, Error> {
        let mut b = Vec::new();
        b.extend_from_slice(GLOBAL_MAGIC);
        b.extend_from_slice(&BLOB_VERSION.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&self.base_fp);
        b.extend_from_slice(&plan.fingerprint);
        put_u32(&mut b, workers)?;
        put_u32(&mut b, plan.columns.len())?;
        for c in &plan.columns {
            put_u32(&mut b, *c)?;
        }
        b.extend_from_slice(&after_epoch.to_le_bytes());
        for (m, ch) in entries {
            b.extend_from_slice(m.digest.as_bytes());
            match ch {
                Some(ch) => {
                    b.push(1);
                    b.extend_from_slice(ch.digest.as_bytes());
                }
                None => b.push(0),
            }
        }
        Ok(self
            .store
            .put_blob(Domain::Artifact, &b)
            .map_err(storage)?
            .id)
    }

    fn read_global(&self, id: &ObjectId) -> Result<Global, Error> {
        let bytes = read_blob(self.store, id, self.cfg.checkpoint_limits.max_bytes)?;
        let mut r = Bytes { b: &bytes, at: 0 };
        if r.take(8)? != GLOBAL_MAGIC
            || r.take(2)? != BLOB_VERSION.to_le_bytes()
            || r.take(2)? != [0, 0]
        {
            return Err(Error::Checkpoint("global manifest version"));
        }
        if r.digest()? != self.base_fp {
            return Err(Error::Checkpoint("global manifest identity"));
        }
        let plan_fp = r.digest()?;
        let workers = r.usize32()?;
        if workers == 0 || workers > MAX_WORKERS {
            return Err(Error::Checkpoint("global manifest workers"));
        }
        let cols = r.usize32()?;
        if cols != self.plan.circuit.inputs.len() {
            return Err(Error::Checkpoint("global manifest columns"));
        }
        let mut columns = Vec::with_capacity(cols);
        for _ in 0..cols {
            columns.push(r.usize32()?);
        }
        let after_epoch = r.u64()?;
        let mut entries = Vec::with_capacity(workers);
        for _ in 0..workers {
            let m = blob_id(r.digest()?);
            let ch = match r.u8()? {
                0 => None,
                1 => Some(blob_id(r.digest()?)),
                _ => return Err(Error::Checkpoint("global manifest channel tag")),
            };
            entries.push((m, ch));
        }
        if r.at != bytes.len() {
            return Err(Error::Checkpoint("trailing bytes"));
        }
        Ok(Global {
            plan_fp,
            workers,
            columns,
            after_epoch,
            entries,
        })
    }

    fn write_channel(&self, w: usize, b: u64, cap: &Capture) -> Result<ObjectId, Error> {
        let mut out = Vec::new();
        out.extend_from_slice(CHANNEL_MAGIC);
        out.extend_from_slice(&BLOB_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.plan.fingerprint);
        put_u32(&mut out, w)?;
        put_u32(&mut out, self.n_at(b))?;
        out.extend_from_slice(&cap.start.to_le_bytes());
        out.extend_from_slice(&b.to_le_bytes());
        put_u32(&mut out, cap.parts.len())?;
        for (epoch, from, part) in &cap.parts {
            match part {
                Part::Data(deltas) => {
                    out.push(0);
                    out.extend_from_slice(&epoch.to_le_bytes());
                    put_u32(&mut out, *from)?;
                    put_u32(&mut out, 0)?;
                    put_u32(&mut out, deltas.len())?;
                    for d in deltas {
                        put_set(&mut out, d)?;
                    }
                }
                Part::Shuffle(x, delta) => {
                    out.push(1);
                    out.extend_from_slice(&epoch.to_le_bytes());
                    put_u32(&mut out, *from)?;
                    put_u32(&mut out, *x)?;
                    put_u32(&mut out, 1)?;
                    put_set(&mut out, delta)?;
                }
            }
        }
        if out.len() > self.cfg.checkpoint_limits.max_bytes {
            return Err(Error::Limit("checkpoint bytes"));
        }
        Ok(self
            .store
            .put_blob(Domain::Artifact, &out)
            .map_err(storage)?
            .id)
    }

    /// Parse a channel blob into per-epoch inboxes, checking completeness.
    fn read_channel(
        &self,
        plan: &Plan,
        workers: usize,
        id: &ObjectId,
        w: usize,
        start: u64,
        b: u64,
    ) -> Result<BTreeMap<u64, Inbox>, Error> {
        let limits = self.cfg.checkpoint_limits;
        let bytes = read_blob(self.store, id, limits.max_bytes)?;
        let mut r = Bytes { b: &bytes, at: 0 };
        if r.take(8)? != CHANNEL_MAGIC
            || r.take(2)? != BLOB_VERSION.to_le_bytes()
            || r.take(2)? != [0, 0]
        {
            return Err(Error::Checkpoint("channel version"));
        }
        if r.digest()? != plan.fingerprint {
            return Err(Error::Checkpoint("channel identity"));
        }
        if r.usize32()? != w || r.usize32()? != workers || r.u64()? != start || r.u64()? != b {
            return Err(Error::Checkpoint("channel header"));
        }
        if start > b {
            return Err(Error::Checkpoint("channel range"));
        }
        let count = r.usize32()?;
        let mut rows_left = limits.max_total_rows;
        let c = &plan.circuit;
        let exchanges = c.exchange_widths();
        let mut inboxes: BTreeMap<u64, Inbox> = BTreeMap::new();
        for _ in 0..count {
            let kind = r.u8()?;
            let epoch = r.u64()?;
            let from = r.usize32()?;
            let x = r.usize32()?;
            let sets = r.usize32()?;
            if epoch < start || epoch > b || from >= workers {
                return Err(Error::Checkpoint("channel entry"));
            }
            let inbox = inboxes.entry(epoch).or_default();
            match kind {
                0 => {
                    if x != 0 || sets != c.inputs.len() {
                        return Err(Error::Checkpoint("channel entry"));
                    }
                    let mut deltas = Vec::with_capacity(sets);
                    for width in &c.inputs {
                        deltas.push(read_set(&mut r, *width, &mut rows_left)?);
                    }
                    if inbox.data.insert(from, deltas).is_some() {
                        return Err(Error::Checkpoint("channel duplicate"));
                    }
                }
                1 => {
                    let width = *exchanges
                        .get(&x)
                        .ok_or(Error::Checkpoint("channel exchange"))?;
                    if sets != 1 {
                        return Err(Error::Checkpoint("channel entry"));
                    }
                    let delta = read_set(&mut r, width, &mut rows_left)?;
                    if inbox.shuffles.insert((x, from), delta).is_some() {
                        return Err(Error::Checkpoint("channel duplicate"));
                    }
                }
                _ => return Err(Error::Checkpoint("channel entry kind")),
            }
        }
        if r.at != bytes.len() {
            return Err(Error::Checkpoint("trailing bytes"));
        }
        for epoch in start..=b {
            let inbox = inboxes.get(&epoch);
            let complete = inbox.is_some_and(|i| {
                i.data.len() == workers
                    && exchanges
                        .keys()
                        .all(|x| (0..workers).all(|f| i.shuffles.contains_key(&(*x, f))))
                    && i.shuffles.len() == exchanges.len() * workers
            });
            if !complete {
                return Err(Error::Checkpoint("channel state incomplete"));
            }
        }
        Ok(inboxes)
    }

    // ------------------------------------------------------------------
    // Restore, rescale and garbage collection
    // ------------------------------------------------------------------

    /// Restore worker `w` of `global` (made by `workers` workers under
    /// `plan`) at the checkpoint epoch, replaying channel state if any.
    fn restore_executor(
        &self,
        plan: &Plan,
        global: &Global,
        w: usize,
    ) -> Result<(Executor, DurableCheckpointer), Error> {
        let limits = self.cfg.checkpoint_limits;
        let (manifest, channel) = global.entries[w];
        let mut exec = plan.circuit.clone().executor();
        let mut ckpt = DurableCheckpointer::new();
        let restored = ckpt.restore(&mut exec, self.store, &manifest, limits)?;
        let target = global.after_epoch.checked_add(1).ok_or(Error::Overflow)?;
        match channel {
            None if restored.ticks == target => {}
            None => return Err(Error::Checkpoint("worker checkpoint epoch")),
            Some(ch) => {
                if restored.ticks >= target {
                    return Err(Error::Checkpoint("worker checkpoint epoch"));
                }
                let inboxes = self.read_channel(
                    plan,
                    global.workers,
                    &ch,
                    w,
                    restored.ticks,
                    global.after_epoch,
                )?;
                for (_, inbox) in inboxes {
                    let mut deltas = Vec::with_capacity(plan.circuit.inputs.len());
                    for (slot, width) in plan.circuit.inputs.iter().enumerate() {
                        let mut sum = ZSet::empty(*width, plan.circuit.limits)?;
                        for parts in inbox.data.values() {
                            sum = sum.plus(&parts[slot], plan.circuit.limits)?;
                        }
                        deltas.push(sum);
                    }
                    let mut st = exec.stage(&TickInput { deltas })?;
                    while exec.run_stage(&mut st, true)?.is_some() {
                        let x = st.position();
                        let mut sum: Option<ZSet> = None;
                        for ((ex, _), part) in &inbox.shuffles {
                            if *ex == x {
                                sum = Some(match sum {
                                    None => part.clone(),
                                    Some(s) => s.plus(part, plan.circuit.limits)?,
                                });
                            }
                        }
                        exec.provide(&mut st, sum.ok_or(Error::Checkpoint("channel exchange"))?)?;
                    }
                    exec.finish_stage(st)?;
                }
            }
        }
        if exec.ticks() != target {
            return Err(Error::Checkpoint("worker checkpoint epoch"));
        }
        // Received integrals must hold exactly the rows this worker owns.
        for (node, set) in exec.integrals().iter().enumerate() {
            let key = match plan.circuit.nodes[node].op {
                Op::Input(_) | Op::Exchange { .. } => plan.keys[node],
                _ => None,
            };
            if let Some(k) = key
                && set
                    .rows
                    .keys()
                    .any(|row| route(row[k], global.workers) != w)
            {
                return Err(Error::Checkpoint("rows on the wrong worker"));
            }
        }
        Ok((exec, ckpt))
    }

    fn fresh_worker(
        &mut self,
        exec: Executor,
        ckpt: DurableCheckpointer,
        known: Option<u64>,
    ) -> Result<Worker, Error> {
        let start = next(known)?;
        Ok(Worker {
            exec,
            ckpt,
            next_epoch: start,
            next_source: start,
            inbox: BTreeMap::new(),
            staged: None,
            barriers: BTreeMap::new(),
            last_snapshot: known,
            captures: BTreeMap::new(),
            acked: BTreeMap::new(),
            outputs: BTreeMap::new(),
            sent: Vec::new(),
            known,
            resend: Vec::new(),
            ticket: None,
            generation: self.gen_at(start),
            reconfigure: BTreeSet::new(),
            migrated_out: None,
            migration_in: BTreeMap::new(),
        })
    }

    /// A worker slot that runs no epoch from `next(known)` on: it waits for
    /// the live rescale that activates it, if any, and holds no state.
    fn idle_worker(&mut self, w: usize, known: Option<u64>) -> Result<Worker, Error> {
        let exec = self.plan.circuit.clone().executor();
        let mut worker = self.fresh_worker(exec, DurableCheckpointer::new(), known)?;
        match self.cfg.activation(w, next(known)?) {
            Some(at) => {
                worker.next_epoch = at;
                worker.next_source = at;
                // State arrives by migration at the barrier before `at`.
                worker.generation = self.gen_at(at).saturating_sub(1);
            }
            None => {
                worker.next_epoch = u64::MAX;
                worker.next_source = u64::MAX;
            }
        }
        Ok(worker)
    }

    /// A worker restored from the sink's last commit (or initial).
    fn restored_worker(&mut self, record: Option<CommitRecord>, w: usize) -> Result<Worker, Error> {
        let start = next(record.map(|r| r.after_epoch))?;
        // Inside a live reconfiguration the commit still has the old layout:
        // restore it, and the worker then takes part in the migration.
        let window = self.in_reconfiguration(record);
        let layout = match window {
            Some(b) => self.n_at(b),
            None => self.n_at(start),
        };
        if w >= layout {
            return self.idle_worker(w, record.map(|r| r.after_epoch));
        }
        match record {
            None => {
                let exec = self.plan.circuit.clone().executor();
                self.fresh_worker(exec, DurableCheckpointer::new(), None)
            }
            Some(r) => {
                let global = self.read_global(&r.global)?;
                if global.plan_fp != self.plan.fingerprint
                    || global.workers != layout
                    || global.after_epoch != r.after_epoch
                {
                    return Err(Error::Checkpoint("global manifest layout"));
                }
                let (exec, ckpt) = self.restore_executor(&self.plan, &global, w)?;
                let mut worker = self.fresh_worker(exec, ckpt, Some(r.after_epoch))?;
                if let Some(b) = window {
                    worker.generation = self.gen_at(b);
                }
                Ok(worker)
            }
        }
    }

    fn restored_checkpoint(&self, record: Option<CommitRecord>) -> Option<CommittedCheckpoint> {
        record.map(|r| {
            self.history
                .iter()
                .rev()
                .find(|c| c.manifest == r.global)
                .copied()
                .unwrap_or(CommittedCheckpoint {
                    after_epoch: r.after_epoch,
                    manifest: r.global,
                    step: 0,
                })
        })
    }

    /// Every object a global manifest references, including itself.
    fn refs(&self, id: &ObjectId) -> Result<BTreeSet<ObjectId>, Error> {
        let global = self.read_global(id)?;
        if global.plan_fp == self.plan.fingerprint {
            return refs_with(self, &self.plan, id);
        }
        let plan = Plan::new(self.base, &global.columns, self.cfg.checkpoint_limits)?;
        refs_with(self, &plan, id)
    }

    /// Delete what `previous` references and `current` does not.
    fn collect(&mut self, previous: &ObjectId, current: &ObjectId) -> Result<(), Error> {
        if !self.cfg.collect_garbage || previous == current {
            return Ok(());
        }
        let keep = self.refs(current)?;
        let old = self.refs(previous)?;
        for id in old.difference(&keep) {
            let bytes = remove_object(self.store, id)?;
            if bytes > 0 {
                self.gc.superseded += 1;
                self.gc.bytes += bytes;
            }
        }
        Ok(())
    }

    /// Quiescent sweep: delete every Flux object of this circuit's plans that
    /// the current commit does not reference.
    fn sweep(&mut self, current: Option<ObjectId>) -> Result<(), Error> {
        if !self.cfg.collect_garbage {
            return Ok(());
        }
        let keep = match current {
            Some(c) => self.refs(&c)?,
            None => BTreeSet::new(),
        };
        let probe = self.store.object_path(&blob_id([0; 32]));
        let Some(base) = probe.parent().and_then(|p| p.parent()) else {
            return Ok(());
        };
        let Ok(dirs) = std::fs::read_dir(base) else {
            return Ok(());
        };
        let mut victims = Vec::new();
        for dir in dirs.flatten() {
            let Ok(files) = std::fs::read_dir(dir.path()) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name();
                let Some(digest) = name.to_str().and_then(vesper_loom_cas::Digest::from_hex) else {
                    continue;
                };
                let id = blob_id(digest.0);
                if keep.contains(&id) {
                    continue;
                }
                let mut head = [0u8; 44];
                let ok = std::fs::File::open(file.path())
                    .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head));
                if ok.is_err() {
                    continue;
                }
                let magic = &head[..8];
                let fp: [u8; 32] = head[12..44].try_into().map_err(|_| Error::Overflow)?;
                let ours = (magic == GLOBAL_MAGIC && fp == self.base_fp)
                    || ([&MANIFEST_MAGIC[..], &BATCH_MAGIC[..], &CHANNEL_MAGIC[..]]
                        .contains(&magic)
                        && self.sweep_fps.contains(&fp));
                if ours {
                    victims.push(id);
                }
            }
        }
        for id in victims {
            let bytes = remove_object(self.store, &id)?;
            if bytes > 0 {
                self.gc.swept += 1;
                self.gc.bytes += bytes;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Faults
    // ------------------------------------------------------------------

    fn phase(&self) -> CheckpointPhase {
        match self.coord.pending {
            Pending::Written { .. } => return CheckpointPhase::ManifestWritten,
            Pending::Committed { .. } => return CheckpointPhase::Committed,
            Pending::Idle => {}
        }
        let committed = self.coord.epoch();
        if self
            .coord
            .acks
            .values()
            .any(|a| a.iter().all(Option::is_some))
        {
            return CheckpointPhase::Acknowledged;
        }
        let snapshotting = !self.coord.acks.is_empty()
            || self.workers.iter().any(|w| {
                !w.captures.is_empty() || w.acked.keys().any(|b| committed.is_none_or(|c| *b > c))
            })
            || self
                .net
                .queue
                .values()
                .any(|(_, m)| matches!(m, Msg::Ack { .. }));
        if snapshotting {
            return CheckpointPhase::Snapshotting;
        }
        if self.workers.iter().any(|w| !w.barriers.is_empty())
            || self.net.barriers_in_flight(committed)
        {
            return CheckpointPhase::Injected;
        }
        CheckpointPhase::Idle
    }

    fn fault(&mut self, kind: FaultKind, step: u64) -> Result<(), Error> {
        let phase = self.phase();
        if let FaultKind::Worker(w) = kind
            && w >= self.coord_id
        {
            return Err(Error::Cluster("fault worker"));
        }
        let durable = self.sink.last_commit()?;
        let escalated = kind != FaultKind::Cluster && self.in_reconfiguration(durable).is_some();
        let restored = if escalated {
            self.recover_cluster()?
        } else {
            match kind {
                FaultKind::Cluster => self.recover_cluster()?,
                FaultKind::Worker(w) => {
                    self.net.drop_to(w);
                    let record = self.sink.last_commit()?;
                    let mut worker = self.restored_worker(record, w)?;
                    // The failed incarnation's hold stays until the replacement
                    // passes it again, so the watermark never regresses.
                    worker.ticket = self.workers[w].ticket.take();
                    self.workers[w] = worker;
                    for (p, peer) in self.workers.iter_mut().enumerate() {
                        if p != w {
                            peer.resend.push(w);
                        }
                    }
                    record
                }
                FaultKind::Coordinator => {
                    self.net.drop_to(self.coord_id);
                    self.sink.crash()?;
                    let record = self.sink.last_commit()?;
                    self.coord = Coordinator::new(record, true);
                    record
                }
            }
        };
        self.recoveries.push(Recovery {
            step,
            kind,
            restored: self.restored_checkpoint(restored),
            phase,
            escalated,
        });
        Ok(())
    }

    /// Whole-cluster recovery from the sink's last commit. A commit made
    /// under a layout the schedule has already left (the barrier of an
    /// interrupted live rescale) is rescaled offline first.
    fn recover_cluster(&mut self) -> Result<Option<CommitRecord>, Error> {
        self.net.clear();
        // Recovery reclaims the failed incarnation's holds before replacing it.
        self.retire_all()?;
        self.sink.crash()?;
        let mut record = self.sink.last_commit()?;
        let start = next(record.map(|r| r.after_epoch))?;
        let mut moved = None;
        if let Some(r) = record {
            let global = self.read_global(&r.global)?;
            if global.workers != self.n_at(start) {
                let (m, _) = rescale_from(self, self.base, &r, &global, self.n_at(start))?;
                self.recovery_rescales += 1;
                record = self.sink.last_commit()?;
                moved = Some(m);
            }
        }
        self.coord = Coordinator::new(record, false);
        self.frontier = Frontier::new(timestamp(start)?, self.coord_id)?;
        let workers = self.start_workers(record, moved)?;
        self.workers = workers;
        Ok(record)
    }

    /// Every worker slot for a run from `record`: restored (or moved by an
    /// offline rescale) when it runs the next epoch, idle otherwise. Running
    /// workers get a frontier hold.
    fn start_workers(
        &mut self,
        record: Option<CommitRecord>,
        moved: Option<Vec<(Executor, DurableCheckpointer)>>,
    ) -> Result<Vec<Worker>, Error> {
        let known = record.map(|r| r.after_epoch);
        let start = next(known)?;
        let mut moved = moved.map(|m| m.into_iter());
        let mut workers = Vec::with_capacity(self.coord_id);
        for w in 0..self.coord_id {
            let mut worker = match moved.as_mut() {
                Some(m) if w < self.n_at(start) => {
                    let (exec, ckpt) = m.next().ok_or(Error::Cluster("rescale"))?;
                    self.fresh_worker(exec, ckpt, known)?
                }
                _ => self.restored_worker(record, w)?,
            };
            if w < self.n_at(start) {
                worker.ticket = Some(self.frontier.acquire(timestamp(start)?)?);
            }
            workers.push(worker);
        }
        Ok(workers)
    }

    fn retire_all(&mut self) -> Result<(), Error> {
        for w in &mut self.workers {
            if let Some(t) = w.ticket.take() {
                self.frontier.retire(t).map_err(|e| e.error)?;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Workers
    // ------------------------------------------------------------------

    /// Send from worker `w`, keeping data, shuffles and barriers for replay.
    fn send(&mut self, now: u64, w: usize, to: usize, msg: Msg) -> Result<(), Error> {
        if matches!(
            msg,
            Msg::Data { .. } | Msg::Shuffle { .. } | Msg::Barrier { .. }
        ) {
            self.workers[w].sent.push((to, msg.clone()));
        }
        self.net.send(now, w, to, msg)
    }

    fn ack(&mut self, now: u64, w: usize, after: u64) -> Result<(), Error> {
        let worker = &self.workers[w];
        let (manifest, channel) = *worker
            .acked
            .get(&after)
            .ok_or(Error::Cluster("missing acknowledgement"))?;
        let outputs: Vec<(u64, ZSet)> = worker
            .outputs
            .range(next(worker.known)?..=after)
            .map(|(e, z)| (*e, z.clone()))
            .collect();
        let coordinator = self.coord_id;
        self.net.send(
            now,
            w,
            coordinator,
            Msg::Ack {
                after_epoch: after,
                worker: w,
                manifest,
                channel,
                outputs,
            },
        )
    }

    fn trim(worker: &mut Worker, committed: u64) {
        if worker.known.is_some_and(|k| k >= committed) {
            return;
        }
        worker.known = Some(committed);
        worker.sent.retain(|(_, m)| m.epoch() > committed);
        worker.outputs.retain(|e, _| *e > committed);
        worker.acked.retain(|b, _| *b > committed);
        worker.captures.retain(|b, _| *b > committed);
        worker.barriers.retain(|b, _| *b > committed);
    }

    /// Record a message for every open unaligned capture it belongs to.
    fn capture(worker: &mut Worker, epoch: u64, from: usize, part: &Part) {
        for (b, cap) in &mut worker.captures {
            if cap.start <= epoch && epoch <= *b {
                cap.parts.push((epoch, from, part.clone()));
            }
        }
    }

    fn deliver(&mut self, w: usize, now: u64, msg: Msg) -> Result<bool, Error> {
        let cfg = self.cfg;
        let n_at = |e: u64| cfg.workers_at(e);
        let worker = &mut self.workers[w];
        match msg {
            Msg::Data {
                epoch,
                from,
                deltas,
            } => {
                if from >= n_at(epoch)
                    || w >= n_at(epoch)
                    || deltas.len() != self.plan.circuit.inputs.len()
                {
                    return Err(Error::Cluster("malformed data"));
                }
                if epoch < worker.next_epoch {
                    self.duplicates += 1;
                    return Ok(false);
                }
                let inbox = worker.inbox.entry(epoch).or_default();
                if let Some(old) = inbox.data.get(&from) {
                    if *old != deltas {
                        return Err(Error::Cluster("nondeterministic replay"));
                    }
                    self.duplicates += 1;
                    return Ok(false);
                }
                let part = Part::Data(deltas.clone());
                inbox.data.insert(from, deltas);
                Self::capture(worker, epoch, from, &part);
                Ok(false)
            }
            Msg::Shuffle {
                epoch,
                exchange,
                from,
                delta,
            } => {
                if from >= n_at(epoch) || w >= n_at(epoch) {
                    return Err(Error::Cluster("malformed shuffle"));
                }
                if epoch < worker.next_epoch {
                    self.duplicates += 1;
                    return Ok(false);
                }
                let inbox = worker.inbox.entry(epoch).or_default();
                if let Some(old) = inbox.shuffles.get(&(exchange, from)) {
                    if *old != delta {
                        return Err(Error::Cluster("nondeterministic replay"));
                    }
                    self.duplicates += 1;
                    return Ok(false);
                }
                let part = Part::Shuffle(exchange, delta.clone());
                inbox.shuffles.insert((exchange, from), delta);
                Self::capture(worker, epoch, from, &part);
                Ok(false)
            }
            Msg::Barrier { after_epoch, from } => {
                let n = n_at(after_epoch);
                if from >= n || w >= n {
                    return Err(Error::Cluster("malformed barrier"));
                }
                if worker.last_snapshot.is_some_and(|s| s >= after_epoch) {
                    self.duplicates += 1;
                    return Ok(false);
                }
                let seen = worker
                    .barriers
                    .entry(after_epoch)
                    .or_insert_with(|| vec![false; n]);
                if std::mem::replace(&mut seen[from], true) {
                    self.duplicates += 1;
                }
                Ok(false)
            }
            Msg::Committed { after_epoch } => {
                Self::trim(worker, after_epoch);
                self.fast_forward(w, after_epoch)
            }
            Msg::Resync { committed } => {
                if let Some(c) = committed {
                    Self::trim(worker, c);
                    if self.fast_forward(w, c)? {
                        return Ok(true);
                    }
                }
                let pending: Vec<u64> = self.workers[w].acked.keys().copied().collect();
                for b in pending {
                    self.ack(now, w, b)?;
                }
                Ok(true)
            }
            Msg::Reconfigure { after_epoch } => {
                worker.reconfigure.insert(after_epoch);
                Ok(false)
            }
            Msg::Migrate {
                after_epoch,
                from,
                parts,
            } => {
                let next = after_epoch.checked_add(1).ok_or(Error::Overflow)?;
                if from >= n_at(after_epoch) || w >= n_at(next) {
                    return Err(Error::Cluster("malformed migration"));
                }
                if worker.generation >= cfg.generation_at(next) {
                    self.duplicates += 1;
                    return Ok(false);
                }
                let got = worker.migration_in.entry(after_epoch).or_default();
                if got.insert(from, parts).is_some() {
                    return Err(Error::Cluster("duplicate migration"));
                }
                Ok(false)
            }
            Msg::Ack { .. } | Msg::Migrated { .. } => {
                Err(Error::Cluster("misrouted acknowledgement"))
            }
        }
    }

    /// A worker still replaying epochs that a newer commit covers restores
    /// from that commit instead: peers no longer buffer those epochs.
    fn fast_forward(&mut self, w: usize, committed: u64) -> Result<bool, Error> {
        if self.workers[w].next_epoch > committed {
            return Ok(false);
        }
        let record = self.sink.last_commit()?;
        if record.is_none_or(|r| r.after_epoch < committed) {
            return Err(Error::Cluster("commit notice ahead of the sink"));
        }
        let mut worker = self.restored_worker(record, w)?;
        worker.ticket = self.workers[w].ticket.take();
        self.workers[w] = worker;
        for (p, peer) in self.workers.iter_mut().enumerate() {
            if p != w && !peer.resend.contains(&w) {
                peer.resend.push(w);
            }
        }
        Ok(true)
    }

    fn worker_step(&mut self, w: usize, now: u64) -> Result<bool, Error> {
        // 0. Resend buffered messages to a recovering peer.
        if let Some(peer) = self.workers[w].resend.pop() {
            let msgs: Vec<Msg> = self.workers[w]
                .sent
                .iter()
                .filter(|(to, _)| *to == peer)
                .map(|(_, m)| m.clone())
                .collect();
            for msg in msgs {
                self.resent += 1;
                self.net.send(now, w, peer, msg)?;
            }
            return Ok(true);
        }
        let mut acted = false;
        for msg in self.net.recv(w, now) {
            acted |= self.deliver(w, now, msg)?;
        }
        if acted {
            return Ok(true);
        }
        if self.try_migrate(w, now)? {
            return Ok(true);
        }
        if self.try_snapshot(w, now)? {
            return Ok(true);
        }
        if self.try_capture_ack(w, now)? {
            return Ok(true);
        }
        if self.try_process(w, now)? {
            return Ok(true);
        }
        // Read and scatter the next source epoch, at most one epoch ahead.
        let worker = &self.workers[w];
        if worker.next_source < self.epochs
            && worker.next_source <= worker.next_epoch.saturating_add(1)
            && w < self.n_at(worker.next_source)
        {
            let epoch = worker.next_source;
            let n = self.n_at(epoch);
            let input = self.source.read(epoch)?;
            let c = &self.plan.circuit;
            if input.deltas.len() != c.inputs.len() {
                return Err(Error::InputCount);
            }
            let mut parts: Vec<Vec<ZSet>> = Vec::with_capacity(n);
            for _ in 0..n {
                let mut v = Vec::with_capacity(c.inputs.len());
                for width in &c.inputs {
                    v.push(ZSet::empty(*width, c.limits)?);
                }
                parts.push(v);
            }
            for (slot, delta) in input.deltas.iter().enumerate() {
                if delta.width != c.inputs[slot] || delta.len() > c.limits.max_input_entries {
                    return Err(Error::Schema);
                }
                let col = self.plan.columns[slot];
                for (i, (row, weight)) in delta.rows.iter().enumerate() {
                    if i % n == w {
                        parts[route(row[col], n)][slot]
                            .rows
                            .insert(row.clone(), *weight);
                    }
                }
            }
            for (dest, deltas) in parts.into_iter().enumerate() {
                self.send(
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
                    self.send(
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
            // A worker the next layout does not include stops reading
            // until a later rescale activates it again.
            self.workers[w].next_source = self.cfg.activation(w, epoch + 1).unwrap_or(u64::MAX);
            return Ok(true);
        }
        Ok(false)
    }

    /// Live rescale at barrier `b`, once its commit is announced: an old
    /// worker that processed `b` sends each new worker the rows of its keyed
    /// integrals (inputs and exchanges) the new layout routes there; a new
    /// worker that has every old worker's part rebuilds its derived
    /// integrals locally, checkpoints and reports to the coordinator, then
    /// runs `b + 1`. Old workers the new layout drops retire.
    fn try_migrate(&mut self, w: usize, now: u64) -> Result<bool, Error> {
        let Some(&b) = self.workers[w].reconfigure.first() else {
            return Ok(false);
        };
        let after = b.checked_add(1).ok_or(Error::Overflow)?;
        if self.workers[w].generation >= self.gen_at(after) {
            self.workers[w].reconfigure.remove(&b);
            return Ok(true);
        }
        let (old_n, new_n) = (self.n_at(b), self.n_at(after));
        let c = &self.plan.circuit;
        let keyed: Vec<(usize, usize)> = c
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, node)| match node.op {
                Op::Input(slot) => Some((i, self.plan.columns[slot])),
                Op::Exchange { key, .. } => Some((i, key)),
                _ => None,
            })
            .collect();
        let worker = &self.workers[w];
        if w < old_n && worker.migrated_out != Some(b) {
            if worker.next_epoch != after
                || worker.staged.is_some()
                || worker.generation != self.gen_at(b)
            {
                return Ok(false);
            }
            let integrals = worker.exec.integrals();
            let mut parts: Vec<Vec<ZSet>> = vec![Vec::with_capacity(keyed.len()); new_n];
            for &(node, key) in &keyed {
                let set = &integrals[node];
                self.moving.1 += set.len();
                for (j, part) in split(set, key, new_n).into_iter().enumerate() {
                    if j != w {
                        self.moving.0 += part.len();
                    }
                    parts[j].push(part);
                }
            }
            for (dest, parts) in parts.into_iter().enumerate() {
                self.net.send(
                    now,
                    w,
                    dest,
                    Msg::Migrate {
                        after_epoch: b,
                        from: w,
                        parts,
                    },
                )?;
            }
            self.workers[w].migrated_out = Some(b);
            if w >= new_n {
                // Retire: everything this worker computed is in the commit at `b`.
                // Keep what peers already sent for a later activation.
                let mut idle = self.idle_worker(w, Some(b))?;
                let old = &mut self.workers[w];
                idle.reconfigure = std::mem::take(&mut old.reconfigure);
                idle.reconfigure.remove(&b);
                idle.inbox = std::mem::take(&mut old.inbox);
                idle.inbox.retain(|e, _| *e > b);
                idle.barriers = std::mem::take(&mut old.barriers);
                idle.barriers.retain(|e, _| *e > b);
                idle.migration_in = std::mem::take(&mut old.migration_in);
                idle.migration_in.retain(|e, _| *e > b);
                if let Some(t) = self.workers[w].ticket.take() {
                    self.frontier.retire(t).map_err(|e| e.error)?;
                }
                self.workers[w] = idle;
            }
            return Ok(true);
        }
        if w >= new_n {
            return Ok(false);
        }
        if worker
            .migration_in
            .get(&b)
            .is_none_or(|got| got.len() != old_n)
            || worker.staged.is_some()
        {
            return Ok(false);
        }
        let got = self.workers[w]
            .migration_in
            .remove(&b)
            .ok_or(Error::Cluster("migration"))?;
        let lim = c.limits;
        let limits = self.cfg.checkpoint_limits;
        let mut budget = limits.budget(c);
        let mut sums: Vec<ZSet> = Vec::with_capacity(keyed.len());
        for (k, &(node, _)) in keyed.iter().enumerate() {
            let mut sum = ZSet::empty(c.nodes[node].width, lim)?;
            for parts in got.values() {
                let part = parts.get(k).ok_or(Error::Cluster("malformed migration"))?;
                if part.width != sum.width {
                    return Err(Error::Cluster("malformed migration"));
                }
                sum = sum.plus(part, lim)?;
            }
            sums.push(sum);
        }
        if got.values().any(|p| p.len() != keyed.len()) {
            return Err(Error::Cluster("malformed migration"));
        }
        let mut sums = sums.into_iter();
        let mut prior: Vec<ZSet> = Vec::with_capacity(c.nodes.len());
        for node in &c.nodes {
            match &node.op {
                Op::Input(_) | Op::Exchange { .. } => {
                    prior.push(sums.next().ok_or(Error::Cluster("migration"))?);
                }
                op => {
                    let value = apply(op, &prior, lim, &mut budget)?;
                    prior.push(value);
                }
            }
        }
        let exec = Executor::from_state(c.clone(), prior, after, limits)?;
        let mut ckpt = DurableCheckpointer::new();
        let report = ckpt.checkpoint(&exec, self.store, limits)?;
        self.tally(&report);
        let ticket = match self.workers[w].ticket.take() {
            Some(t) => t,
            None => self.frontier.acquire(timestamp(after)?)?,
        };
        let generation = self.gen_at(after);
        let worker = &mut self.workers[w];
        worker.exec = exec;
        worker.ckpt = ckpt;
        worker.generation = generation;
        worker.ticket = Some(ticket);
        worker.reconfigure.remove(&b);
        worker.last_snapshot = Some(b);
        Self::trim(worker, b);
        let coordinator = self.coord_id;
        self.net.send(
            now,
            w,
            coordinator,
            Msg::Migrated {
                after_epoch: b,
                worker: w,
                manifest: report.manifest,
            },
        )?;
        Ok(true)
    }

    fn try_snapshot(&mut self, w: usize, now: u64) -> Result<bool, Error> {
        let limits = self.cfg.checkpoint_limits;
        let generation = match self.workers[w].barriers.first_key_value() {
            Some((&b, _)) => self.gen_at(b),
            None => return Ok(false),
        };
        let worker = &mut self.workers[w];
        let Some((&b, seen)) = worker.barriers.first_key_value() else {
            return Ok(false);
        };
        // A worker waiting for migrated state has nothing to snapshot yet.
        if worker.generation != generation {
            return Ok(false);
        }
        match self.cfg.mode {
            CheckpointMode::Aligned => {
                if !(seen.iter().all(|s| *s)
                    && worker.next_epoch == b + 1
                    && worker.staged.is_none())
                {
                    return Ok(false);
                }
                let report = worker.ckpt.checkpoint(&worker.exec, self.store, limits)?;
                worker.barriers.remove(&b);
                worker.last_snapshot = Some(b);
                worker.acked.insert(b, (report.manifest, None));
                self.tally(&report);
                self.ack(now, w, b)?;
                Ok(true)
            }
            CheckpointMode::Unaligned => {
                if worker.next_epoch > b + 1 {
                    return Err(Error::Cluster("barrier overtaken"));
                }
                let report = worker.ckpt.checkpoint(&worker.exec, self.store, limits)?;
                let start = worker.next_epoch;
                let mut parts = Vec::new();
                for (epoch, inbox) in worker.inbox.range(start..=b) {
                    for (from, deltas) in &inbox.data {
                        parts.push((*epoch, *from, Part::Data(deltas.clone())));
                    }
                    for ((x, from), delta) in &inbox.shuffles {
                        parts.push((*epoch, *from, Part::Shuffle(*x, delta.clone())));
                    }
                }
                worker.captures.insert(
                    b,
                    Capture {
                        start,
                        manifest: report.manifest,
                        parts,
                    },
                );
                worker.barriers.remove(&b);
                worker.last_snapshot = Some(b);
                self.tally(&report);
                Ok(true)
            }
        }
    }

    fn tally(&mut self, report: &super::DurableReport) {
        self.written += report.written;
        self.reused += report.reused;
        self.merged += report.merged;
        self.rows_written += report.rows_written;
    }

    fn try_capture_ack(&mut self, w: usize, now: u64) -> Result<bool, Error> {
        let worker = &self.workers[w];
        let Some((&b, cap)) = worker.captures.first_key_value() else {
            return Ok(false);
        };
        if worker.next_epoch <= b {
            return Ok(false);
        }
        let channel = if cap.start <= b {
            self.channels += 1;
            Some(self.write_channel(w, b, cap)?)
        } else {
            None
        };
        let worker = &mut self.workers[w];
        let cap = worker
            .captures
            .remove(&b)
            .ok_or(Error::Cluster("capture"))?;
        worker.acked.insert(b, (cap.manifest, channel));
        self.ack(now, w, b)?;
        Ok(true)
    }

    fn try_process(&mut self, w: usize, now: u64) -> Result<bool, Error> {
        let epoch = self.workers[w].next_epoch;
        if !self.runs(w, epoch) {
            return Ok(false);
        }
        let n = self.n_at(epoch);
        let limits = self.limits();
        let aligned = self.cfg.mode == CheckpointMode::Aligned;
        let worker = &mut self.workers[w];
        if aligned
            && worker
                .barriers
                .first_key_value()
                .is_some_and(|(&b, _)| b < worker.next_epoch)
        {
            return Ok(false);
        }
        let Some(inbox) = worker.inbox.get(&epoch) else {
            return Ok(false);
        };
        let mut st = match worker.staged.take() {
            None => {
                if inbox.data.len() != n {
                    return Ok(false);
                }
                let mut deltas = Vec::with_capacity(self.plan.circuit.inputs.len());
                for (slot, width) in self.plan.circuit.inputs.iter().enumerate() {
                    let mut sum = ZSet::empty(*width, limits)?;
                    for parts in inbox.data.values() {
                        sum = sum.plus(&parts[slot], limits)?;
                    }
                    deltas.push(sum);
                }
                worker.exec.stage(&TickInput { deltas })?
            }
            Some(mut st) => {
                let x = st.position();
                let parts: Vec<&ZSet> = inbox
                    .shuffles
                    .range((x, 0)..(x, n))
                    .map(|(_, z)| z)
                    .collect();
                if parts.len() != n {
                    worker.staged = Some(st);
                    return Ok(false);
                }
                let mut sum = parts[0].clone();
                for p in &parts[1..] {
                    sum = sum.plus(p, limits)?;
                }
                worker.exec.provide(&mut st, sum)?;
                st
            }
        };
        match worker.exec.run_stage(&mut st, true)? {
            Some((a, key)) => {
                let x = st.position();
                let pieces = split(st.local(a)?, key, n);
                worker.staged = Some(st);
                for (dest, delta) in pieces.into_iter().enumerate() {
                    self.send(
                        now,
                        w,
                        dest,
                        Msg::Shuffle {
                            epoch,
                            exchange: x,
                            from: w,
                            delta,
                        },
                    )?;
                }
            }
            None => {
                let out = worker.exec.finish_stage(st)?;
                let next_epoch = epoch.checked_add(1).ok_or(Error::Overflow)?;
                if worker.known.is_none_or(|k| epoch > k) {
                    worker.outputs.insert(epoch, out);
                }
                worker.inbox.remove(&epoch);
                worker.next_epoch = next_epoch;
                let ticket = worker
                    .ticket
                    .take()
                    .ok_or(Error::Cluster("missing ticket"))?;
                if timestamp(next_epoch)? > ticket.timestamp() {
                    match self.frontier.advance(ticket, timestamp(next_epoch)?) {
                        Ok(t) => self.workers[w].ticket = Some(t),
                        Err(e) => {
                            self.workers[w].ticket = Some(e.ticket);
                            return Err(e.error);
                        }
                    }
                } else {
                    worker.ticket = Some(ticket);
                }
            }
        }
        Ok(true)
    }

    // ------------------------------------------------------------------
    // Coordinator
    // ------------------------------------------------------------------

    fn coordinator_step(&mut self, now: u64, step: u64) -> Result<bool, Error> {
        let me = self.coord_id;
        let received = self.net.recv(me, now);
        let got = !received.is_empty();
        for msg in received {
            let (after_epoch, worker, manifest, channel, outputs) = match msg {
                Msg::Ack {
                    after_epoch,
                    worker,
                    manifest,
                    channel,
                    outputs,
                } => (after_epoch, worker, manifest, channel, outputs),
                Msg::Migrated {
                    after_epoch,
                    worker,
                    manifest,
                } => {
                    if self.in_reconfiguration(self.coord.committed) != Some(after_epoch) {
                        self.duplicates += 1;
                        continue;
                    }
                    if worker >= self.n_at(after_epoch + 1) {
                        return Err(Error::Cluster("malformed acknowledgement"));
                    }
                    match self.coord.migrated.get(&worker) {
                        None => {
                            self.coord.migrated.insert(worker, manifest);
                        }
                        Some(old) if *old == manifest => self.duplicates += 1,
                        Some(_) => return Err(Error::Cluster("nondeterministic migration")),
                    }
                    continue;
                }
                _ => return Err(Error::Cluster("misrouted message")),
            };
            let n = self.n_at(after_epoch);
            if worker >= n {
                return Err(Error::Cluster("malformed acknowledgement"));
            }
            let committed = self.coord.epoch();
            for (epoch, out) in outputs {
                if committed.is_some_and(|c| epoch <= c) {
                    continue;
                }
                match self.coord.staged.get(&(epoch, worker)) {
                    Some(old) if *old != out => {
                        return Err(Error::Cluster("nondeterministic output"));
                    }
                    Some(_) => {}
                    None => {
                        self.coord.staged.insert((epoch, worker), out);
                    }
                }
            }
            if committed.is_some_and(|c| after_epoch <= c) {
                self.duplicates += 1;
                continue;
            }
            let slot = &mut self
                .coord
                .acks
                .entry(after_epoch)
                .or_insert_with(|| vec![None; n])[worker];
            match slot {
                None => *slot = Some((manifest, channel)),
                Some(old) if *old == (manifest, channel) => self.duplicates += 1,
                // An unaligned snapshot point depends on timing, so a
                // replaced worker may acknowledge a different, equally valid
                // checkpoint. The first one wins.
                Some(_) if self.cfg.mode == CheckpointMode::Unaligned => self.duplicates += 1,
                Some(_) => return Err(Error::Cluster("nondeterministic checkpoint")),
            }
        }
        if got {
            return Ok(true);
        }
        if self.coord.resync {
            self.coord.resync = false;
            let committed = self.coord.epoch();
            for dest in 0..me {
                self.net.send(now, me, dest, Msg::Resync { committed })?;
            }
            if committed == Some(self.last_epoch()) {
                self.done = true;
            }
            return Ok(true);
        }
        match std::mem::replace(&mut self.coord.pending, Pending::Idle) {
            Pending::Committed { after, previous } => {
                for dest in 0..me {
                    self.net
                        .send(now, me, dest, Msg::Committed { after_epoch: after })?;
                }
                let current = self.coord.committed.map(|c| c.global);
                if let (Some(p), Some(c)) = (previous, current) {
                    self.collect(&p, &c)?;
                }
                if self.in_reconfiguration(self.coord.committed) == Some(after) {
                    self.reconfigure(now, after, step)?;
                }
                if after == self.last_epoch() {
                    self.done = true;
                }
                Ok(true)
            }
            Pending::Written {
                after,
                global,
                workers,
            } => {
                if self.frontier.watermark() <= timestamp(after)? {
                    return Err(Error::Cluster("checkpoint ahead of frontier"));
                }
                let width = self.plan.circuit.nodes[self.plan.circuit.output].width;
                let mut outputs = Vec::new();
                for epoch in next(self.coord.epoch())?..=after {
                    let mut sum = ZSet::empty(width, self.limits())?;
                    for w in 0..self.n_at(epoch) {
                        let out = self
                            .coord
                            .staged
                            .get(&(epoch, w))
                            .ok_or(Error::Cluster("missing staged output"))?;
                        sum = sum.plus(out, self.limits())?;
                    }
                    outputs.push((epoch, sum));
                }
                let record = CommitRecord {
                    fingerprint: self.base_fp,
                    after_epoch: after,
                    workers: u32::try_from(workers).map_err(|_| Error::Overflow)?,
                    global,
                };
                let migration = self.in_reconfiguration(self.coord.committed) == Some(after);
                self.sink.commit(&record, &outputs)?;
                // Commit point: nothing fallible follows until the next step.
                let previous = self.coord.committed.map(|c| c.global);
                self.coord.committed = Some(record);
                self.coord.staged.retain(|(e, _), _| *e > after);
                self.coord.acks.retain(|b, _| *b > after);
                self.visible.extend(outputs.into_iter().map(|(_, z)| z));
                self.history.push(CommittedCheckpoint {
                    after_epoch: after,
                    manifest: global,
                    step,
                });
                if migration {
                    let started = self.coord.reconfiguring.map_or(step, |(_, s)| s);
                    self.migrations.push(LiveMigration {
                        after_epoch: after,
                        from_workers: self.n_at(after),
                        to_workers: workers,
                        rows_moved: self.moving.0,
                        rows_total: self.moving.1,
                        manifest: global,
                        steps: step.saturating_sub(started),
                    });
                    self.moving = (0, 0);
                    self.coord.migrated.clear();
                    self.coord.reconfiguring = None;
                }
                self.coord.pending = Pending::Committed { after, previous };
                Ok(true)
            }
            Pending::Idle => {
                if self.coord.epoch() == Some(self.last_epoch()) {
                    // A recovered coordinator finds everything committed.
                    self.done = true;
                    return Ok(true);
                }
                if let Some(b) = self.in_reconfiguration(self.coord.committed) {
                    // The new layout commits before any later checkpoint.
                    if self.coord.reconfiguring.is_none() {
                        self.reconfigure(now, b, step)?;
                        return Ok(true);
                    }
                    let m = self.n_at(b + 1);
                    if self.coord.migrated.len() != m {
                        return Ok(false);
                    }
                    let entries: Vec<_> =
                        self.coord.migrated.values().map(|m| (*m, None)).collect();
                    let global = self.write_global(&self.plan, m, b, &entries)?;
                    self.coord.pending = Pending::Written {
                        after: b,
                        global,
                        workers: m,
                    };
                    return Ok(true);
                }
                let Some((&after, acks)) = self.coord.acks.first_key_value() else {
                    return Ok(false);
                };
                if acks.iter().any(Option::is_none) {
                    return Ok(false);
                }
                let n = acks.len();
                let entries: Vec<_> = acks.iter().flatten().copied().collect();
                let global = self.write_global(&self.plan, n, after, &entries)?;
                self.coord.pending = Pending::Written {
                    after,
                    global,
                    workers: n,
                };
                Ok(true)
            }
        }
    }

    /// Announce the live rescale at the committed barrier `b`.
    fn reconfigure(&mut self, now: u64, b: u64, step: u64) -> Result<(), Error> {
        let me = self.coord_id;
        for dest in 0..me {
            self.net
                .send(now, me, dest, Msg::Reconfigure { after_epoch: b })?;
        }
        self.coord.reconfiguring = Some((b, step));
        Ok(())
    }
}

fn sim_error<E>(_: E) -> Error {
    Error::Cluster("simulator")
}

/// Run `inputs` (one tick per epoch) on a simulated cluster with an in-memory
/// sink, crashing the whole cluster at each scheduler step in `crash_steps`
/// and recovering from the last committed checkpoint.
pub fn run_cluster(
    circuit: &Circuit,
    inputs: &[TickInput],
    cfg: &ClusterConfig,
    crash_steps: &[u64],
    store: &Store,
) -> Result<ClusterRun, Error> {
    let faults: Vec<Fault> = crash_steps.iter().map(|s| Fault::cluster(*s)).collect();
    let mut sink = MemorySink::new();
    run_cluster_with(circuit, &Slice(inputs), &mut sink, store, cfg, &faults)
}

/// Run every epoch of `source` that `sink` has not committed yet, on a
/// simulated cluster with the given faults (see the module docs).
/// Deterministic for a fixed configuration, seed, input, sink state and
/// fault schedule.
pub fn run_cluster_with(
    circuit: &Circuit,
    source: &dyn EpochSource,
    sink: &mut dyn CommitSink,
    store: &Store,
    cfg: &ClusterConfig,
    faults: &[Fault],
) -> Result<ClusterRun, Error> {
    cfg.validate(circuit)?;
    let limits = cfg.checkpoint_limits;
    limits.validate()?;
    let mut budget = limits.budget(circuit);
    let base_fp = identity(circuit, None, limits, &mut budget)?;
    let plan = Plan::new(circuit, &cfg.partition_columns, limits)?;
    let width = circuit.nodes[circuit.output].width;
    let epochs = source.epochs();
    let record = sink.last_commit()?;
    if let Some(r) = record
        && r.fingerprint != base_fp
    {
        return Err(Error::Checkpoint("sink belongs to another circuit"));
    }
    let exchanges = plan.circuit.exchange_count();
    let sweep_fps = vec![plan.fingerprint];
    let mut state = State {
        base: circuit,
        base_fp,
        plan,
        source,
        sink,
        cfg,
        store,
        epochs,
        workers: Vec::new(),
        net: Net {
            queue: BTreeMap::new(),
            link_clock: BTreeMap::new(),
            rng: mix(cfg.seed ^ 0x666c_7578_6e65_7400),
            seq: 0,
            max_delay: cfg.max_link_delay_ns,
            sent: 0,
        },
        frontier: Frontier::new(0, cfg.max_workers())?,
        coord: Coordinator::new(record, false),
        done: false,
        visible: Vec::new(),
        recoveries: Vec::new(),
        history: Vec::new(),
        sweep_fps,
        written: 0,
        reused: 0,
        merged: 0,
        rows_written: 0,
        channels: 0,
        resent: 0,
        duplicates: 0,
        gc: GcReport::default(),
        coord_id: cfg.max_workers(),
        migrations: Vec::new(),
        recovery_rescales: 0,
        moving: (0, 0),
    };
    let start = next(record.map(|r| r.after_epoch))?;
    if record.is_some_and(|r| r.after_epoch >= epochs) {
        return Err(Error::Checkpoint("sink is ahead of the source"));
    }
    // Rescale when the committed layout differs from the configured one.
    let mut rescale = None;
    let mut prepared: Option<Vec<(Executor, DurableCheckpointer)>> = None;
    if let Some(r) = record {
        let global = state.read_global(&r.global)?;
        let to = cfg.workers_at(start);
        if global.workers != to || global.columns != cfg.partition_columns {
            let (moved, manifest) = rescale_from(&mut state, circuit, &r, &global, to)?;
            rescale = Some(Rescale {
                from_workers: global.workers,
                to_workers: to,
                from_columns: global.columns.clone(),
                after_epoch: r.after_epoch,
                manifest,
            });
            prepared = Some(moved);
        }
    }
    let empty_run = |state: State<'_>, rescale| -> Result<ClusterRun, Error> {
        // Nothing left to run: the view is what the sink already committed.
        let mut materialized = ZSet::empty(width, circuit.limits)?;
        for (_, out) in state.sink.outputs()? {
            materialized = materialized.plus(&out, circuit.limits)?;
        }
        Ok(ClusterRun {
            start_epoch: start,
            outputs: Vec::new(),
            materialized,
            steps: 0,
            messages: 0,
            recoveries: Vec::new(),
            committed: Vec::new(),
            rescale,
            migrations: Vec::new(),
            recovery_rescales: 0,
            exchanges,
            arrangements_written: state.written,
            arrangements_reused: state.reused,
            batches_merged: state.merged,
            rows_written: state.rows_written,
            channel_blobs: 0,
            resent: 0,
            duplicates: 0,
            gc: state.gc,
            watermark: timestamp(start)?,
            journal: Simulator::<()>::new(cfg.seed).journal().clone(),
        })
    };
    if start >= epochs {
        return empty_run(state, rescale);
    }
    state.frontier = Frontier::new(timestamp(start)?, state.coord_id)?;
    let record = state.sink.last_commit()?;
    state.coord = Coordinator::new(record, false);
    state.workers = state.start_workers(record, prepared)?;
    let mut sim = Simulator::new(cfg.seed);
    for w in 0..state.coord_id {
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
        let mut faulted = false;
        for f in faults.iter().filter(|f| f.step == steps) {
            state.fault(f.kind, steps)?;
            faulted = true;
        }
        let acted = faulted
            || match role {
                Role::Worker(w) => state.worker_step(w, now)?,
                Role::Coordinator => state.coordinator_step(now, steps)?,
            };
        if acted {
            sim.yield_task(token).map_err(sim_error)?;
        } else {
            let endpoint = match role {
                Role::Worker(w) => w,
                Role::Coordinator => state.coord_id,
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
    // A worker replaced after acknowledging the final checkpoint may still be
    // replaying: deliver the final commit notice, which fast-forwards it.
    let last = state.last_epoch();
    for w in 0..state.coord_id {
        state.fast_forward(w, last)?;
    }
    let watermark = state.frontier.watermark();
    // Cross-check: the sink's outputs integrate to the workers' summed views.
    let mut materialized = ZSet::empty(width, circuit.limits)?;
    for w in &state.workers[..state.n_at(last)] {
        materialized = materialized.plus(w.exec.materialized(), circuit.limits)?;
    }
    let mut committed_total = ZSet::empty(width, circuit.limits)?;
    for (_, out) in state.sink.outputs()? {
        committed_total = committed_total.plus(&out, circuit.limits)?;
    }
    if committed_total != materialized {
        return Err(Error::Cluster("materialized view mismatch"));
    }
    state.retire_all()?;
    if state.frontier.live_holds() != 0 {
        return Err(Error::Cluster("leaked frontier hold"));
    }
    let current = state.coord.committed.map(|c| c.global);
    state.sweep(current)?;
    Ok(ClusterRun {
        start_epoch: start,
        outputs: state.visible,
        materialized,
        steps,
        messages: state.net.sent,
        recoveries: state.recoveries,
        committed: state.history,
        rescale,
        migrations: state.migrations,
        recovery_rescales: state.recovery_rescales,
        exchanges,
        arrangements_written: state.written,
        arrangements_reused: state.reused,
        batches_merged: state.merged,
        rows_written: state.rows_written,
        channel_blobs: state.channels,
        resent: state.resent,
        duplicates: state.duplicates,
        gc: state.gc,
        watermark,
        journal: sim.journal().clone(),
    })
}

/// Restore every worker of `global`, redistribute their state over
/// `cfg.workers` workers under the configured columns, checkpoint the new
/// workers and commit the rescaled manifest (no outputs).
fn rescale_from(
    state: &mut State<'_>,
    circuit: &Circuit,
    record: &CommitRecord,
    global: &Global,
    to: usize,
) -> Result<(Vec<(Executor, DurableCheckpointer)>, ObjectId), Error> {
    let limits = state.cfg.checkpoint_limits;
    let old_plan = Plan::new(circuit, &global.columns, limits)?;
    if old_plan.fingerprint != global.plan_fp || global.after_epoch != record.after_epoch {
        return Err(Error::Checkpoint("global manifest layout"));
    }
    if !state.sweep_fps.contains(&old_plan.fingerprint) {
        state.sweep_fps.push(old_plan.fingerprint);
    }
    let lim = circuit.limits;
    // Gathered (global) sets are bounded by the whole checkpoint's row
    // budget, not one worker's; `Executor::from_state` enforces the
    // per-worker limits again after redistribution.
    let eval = Limits {
        max_rows: lim.max_rows.max(limits.max_total_rows),
        max_input_entries: lim.max_input_entries.max(limits.max_total_rows),
        ..lim
    };
    // Global input integrals: the disjoint union of every old worker's.
    let mut inputs: Vec<ZSet> = circuit
        .inputs
        .iter()
        .map(|w| ZSet::empty(*w, eval))
        .collect::<Result<_, _>>()?;
    for w in 0..global.workers {
        let (exec, _) = state.restore_executor(&old_plan, global, w)?;
        for (node, set) in exec.integrals().iter().enumerate() {
            if let Op::Input(slot) = old_plan.circuit.nodes[node].op {
                inputs[slot] = inputs[slot].plus(set, eval)?;
            }
        }
    }
    let n = to;
    let plan = &state.plan;
    let mut budget = limits.budget(&plan.circuit);
    let mut locals: Vec<Vec<ZSet>> = vec![Vec::new(); n];
    for node in &plan.circuit.nodes {
        match &node.op {
            Op::Input(slot) => {
                for (j, part) in split(&inputs[*slot], plan.columns[*slot], n)
                    .into_iter()
                    .enumerate()
                {
                    locals[j].push(part);
                }
            }
            Op::Exchange { a, key } => {
                let mut whole = ZSet::empty(node.width, eval)?;
                for l in &locals {
                    whole = whole.plus(&l[*a], eval)?;
                }
                for (j, part) in split(&whole, *key, n).into_iter().enumerate() {
                    locals[j].push(part);
                }
            }
            op => {
                for l in &mut locals {
                    let value = apply(op, l, eval, &mut budget)?;
                    l.push(value);
                }
            }
        }
    }
    let ticks = record.after_epoch.checked_add(1).ok_or(Error::Overflow)?;
    let planned = plan.circuit.clone();
    let mut moved = Vec::with_capacity(n);
    let mut entries = Vec::with_capacity(n);
    for prior in locals {
        let exec = Executor::from_state(planned.clone(), prior, ticks, limits)?;
        let mut ckpt = DurableCheckpointer::new();
        let report = ckpt.checkpoint(&exec, state.store, limits)?;
        state.tally(&report);
        entries.push((report.manifest, None));
        moved.push((exec, ckpt));
    }
    let manifest = state.write_global(&state.plan, n, record.after_epoch, &entries)?;
    let new_record = CommitRecord {
        fingerprint: state.base_fp,
        after_epoch: record.after_epoch,
        workers: u32::try_from(n).map_err(|_| Error::Overflow)?,
        global: manifest,
    };
    state.sink.commit(&new_record, &[])?;
    if state.cfg.collect_garbage {
        // The old layout's objects are referenced by nothing now.
        let keep = state.refs(&manifest)?;
        let old = refs_with(state, &old_plan, &record.global)?;
        for id in old.difference(&keep) {
            let bytes = remove_object(state.store, id)?;
            if bytes > 0 {
                state.gc.superseded += 1;
                state.gc.bytes += bytes;
            }
        }
    }
    Ok((moved, manifest))
}

/// Objects of a global manifest made under `plan`.
fn refs_with(state: &State<'_>, plan: &Plan, id: &ObjectId) -> Result<BTreeSet<ObjectId>, Error> {
    let global = state.read_global(id)?;
    let mut out = BTreeSet::from([*id]);
    let mut budget = state.cfg.checkpoint_limits.budget(&plan.circuit);
    for (m, ch) in &global.entries {
        out.insert(*m);
        if let Some(ch) = ch {
            out.insert(*ch);
        }
        let parsed = parse_manifest(
            state.store,
            m,
            &plan.circuit,
            &plan.fingerprint,
            state.cfg.checkpoint_limits,
            &mut budget,
        )?;
        out.extend(parsed.batches());
    }
    Ok(out)
}
