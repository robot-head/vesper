//! Raft-style replication over projected choreographies under SimPulse
//! (design §25.1 item 4, §25.2, §25.3).
//!
//! Every RPC is one instance of a two-role choreography
//! ([`VOTE_CHOREOGRAPHY`], [`APPEND_CHOREOGRAPHY`]). Both sides of each
//! exchange hold a [`RawSession`] whose monitor is the projection for their
//! role, so every frame a node sends or accepts is checked against the
//! projected local type; a frame outside it is rejected and the session is
//! faulted. Unanswered requests leave through the explicit recovery exit
//! (timeout, crash, shutdown). The [`LeakAuditor`] confirms at the end of a
//! run that no session was silently dropped.
//!
//! The cluster runs on 5 or 7 simulated nodes inside one SimPulse simulator
//! (one task per node, one network task, one chaos task). From a single
//! seed the run draws seeded asymmetric partitions, message loss, reordering
//! (random latency), duplication, per-node clock skew and crash/restart
//! events. After every log change the [`Checker`] verifies election safety
//! (at most one leader per term), log matching, and that committed prefixes
//! never diverge. [`QuorumRule::HalfFloor`] is a deliberately wrong
//! non-majority quorum the checker must catch.
//!
//! Depth (wave six lane 17): with [`RaftConfig::snapshot_threshold`] set,
//! nodes compact committed prefixes into a snapshot and a leader whose
//! follower lags behind its snapshot sends it with an InstallSnapshot
//! exchange ([`SNAPSHOT_CHOREOGRAPHY`]); with
//! [`RaftConfig::membership_per_mille`] set, the leader changes the voting
//! membership one server at a time through configuration entries in the
//! log (Ongaro's single-server rule: a node uses the latest configuration in
//! its log, committed or not; a leader proposes a change only after it has
//! committed an entry of its own term and while no change is pending; a
//! leader removed by a committed change steps down; non-members do not
//! campaign). The checker additionally reports any change to a compacted
//! entry.
//!
//! What this is: a deterministic model of the Raft election and log
//! replication rules, executed and checked. What it is not: a production
//! consensus implementation, Paxos, a real network, or the source
//! `@choreography` macro.

use crate::durable::{DurableLog, PersistentState, SimDisk};
use crate::port::{self, binding};
use crate::session::AuditCounts;
use crate::simnet::SplitMix;
use std::collections::BTreeMap;
use vesper_simpulse::{CompletionId, IoResult, Journal, Simulator, TransitionError};

/// RequestVote exchange between a candidate and one voter.
pub const VOTE_CHOREOGRAPHY: &str = "\
roles Candidate, Voter;
Candidate -> Voter: RequestVote(VoteReq);
Voter -> Candidate {
    Grant(VoteResp) { }
    Deny(VoteResp) { }
}
";

/// AppendEntries exchange between a leader and one follower. The follower
/// persists before it answers, in both branches.
pub const APPEND_CHOREOGRAPHY: &str = "\
roles Leader, Follower;
Leader -> Follower: AppendEntries(AppendBatch);
Follower: persist;
Follower -> Leader {
    Accept(AppendAck) { }
    Reject(AppendAck) { }
}
";

/// InstallSnapshot exchange between a leader and one lagging follower. The
/// image is the compacted prefix of the leader's log (in this model the
/// state machine is the command history, so the prefix is its image).
pub const SNAPSHOT_CHOREOGRAPHY: &str = "\
roles Leader, Follower;
Leader -> Follower: InstallSnapshot(SnapshotImage);
Follower: persist;
Follower -> Leader: Installed(AppendAck);
";

/// The `cmd` of a configuration entry: this flag ORed with the bit mask of
/// voting members (so membership change supports at most 63 nodes).
pub const CONFIG_FLAG: u64 = 1 << 63;

/// Voting members named by the newest configuration entry of `log`, or
/// `initial` if it has none.
pub fn config_of(log: &[Entry], initial: u64) -> u64 {
    let raw: Vec<binding::repr::vesper__chorus__wire__RaftEntry> = log
        .iter()
        .map(|e| binding::repr::vesper__chorus__wire__RaftEntry {
            term: e.term,
            cmd: e.cmd,
        })
        .collect();
    binding::p2_raft_config_of(&raw, initial)
}

// ---------------------------------------------------------------- wire

/// The 40-byte AppendEntries wire header of §25.2. All fields big-endian,
/// no padding: 8 + 4 + 4 + 8 + 8 + 8 = 40.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaftAppendWireHeader {
    pub term: u64,
    pub leader_id: u32,
    pub follower_slot: u32,
    pub prev_log_index: u64,
    pub prev_log_term: u64,
    pub leader_commit: u64,
}

pub const APPEND_HEADER_LEN: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    Length { expected: usize, found: usize },
    TooManyEntries(u32),
    BadFlag(u8),
}

impl RaftAppendWireHeader {
    fn to_wire(self) -> binding::repr::vesper__chorus__wire__WireHeader {
        binding::repr::vesper__chorus__wire__WireHeader {
            term: self.term,
            leader_id: vesper_port_abi::Word::new(self.leader_id),
            follower_slot: vesper_port_abi::Word::new(self.follower_slot),
            prev_log_index: self.prev_log_index,
            prev_log_term: self.prev_log_term,
            leader_commit: self.leader_commit,
        }
    }

    fn from_wire(w: binding::repr::vesper__chorus__wire__WireHeader) -> Self {
        Self {
            term: w.term,
            leader_id: w.leader_id.get(),
            follower_slot: w.follower_slot.get(),
            prev_log_index: w.prev_log_index,
            prev_log_term: w.prev_log_term,
            leader_commit: w.leader_commit,
        }
    }

    pub fn encode(&self) -> [u8; APPEND_HEADER_LEN] {
        let mut o = [0u8; APPEND_HEADER_LEN];
        binding::p2_header_encode(&self.to_wire(), &mut o);
        o
    }
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        binding::p2_header_decode(b)
            .map(Self::from_wire)
            .map_err(port::wire_err_to_raft)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    pub term: u64,
    pub cmd: u64,
}

/// Upper bound on entries per AppendEntries batch accepted from the wire.
pub const MAX_BATCH: u32 = 256;

/// `AppendBatch`: header, `u32be` count, then `count` × (term, cmd) `u64be`.
pub fn encode_batch(h: &RaftAppendWireHeader, entries: &[Entry]) -> Vec<u8> {
    let mut o = vec![0u8; APPEND_HEADER_LEN + 4 + 16 * entries.len()];
    let raw: Vec<binding::repr::vesper__chorus__wire__RaftEntry> = entries
        .iter()
        .map(|e| binding::repr::vesper__chorus__wire__RaftEntry {
            term: e.term,
            cmd: e.cmd,
        })
        .collect();
    binding::p2_batch_encode(&h.to_wire(), &raw, &mut o);
    o
}

pub fn decode_batch(b: &[u8]) -> Result<(RaftAppendWireHeader, Vec<Entry>), WireError> {
    let mut raw = Vec::new();
    let h = binding::p2_batch_decode(b, &mut raw).map_err(port::wire_err_to_raft)?;
    let entries = raw
        .into_iter()
        .map(|e| Entry {
            term: e.term,
            cmd: e.cmd,
        })
        .collect();
    Ok((RaftAppendWireHeader::from_wire(h), entries))
}

/// `AppendAck` (§25.3): follower id, term, match index, accepted flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppendAck {
    pub follower_id: u32,
    pub term: u64,
    pub match_index: u64,
    pub accepted: bool,
}

impl AppendAck {
    pub const LEN: usize = 21;
    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![0u8; Self::LEN];
        binding::p2_ack_encode(
            &binding::repr::vesper__chorus__wire__WireAck {
                follower_id: vesper_port_abi::Word::new(self.follower_id),
                term: self.term,
                match_index: self.match_index,
                accepted: vesper_port_abi::Word::new(self.accepted),
            },
            &mut o,
        );
        o
    }
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        let a = binding::p2_ack_decode(b).map_err(port::wire_err_to_raft)?;
        Ok(Self {
            follower_id: a.follower_id.get(),
            term: a.term,
            match_index: a.match_index,
            accepted: a.accepted.get(),
        })
    }
}

/// `VoteReq`: term, candidate, last log index, last log term (28 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoteReq {
    pub term: u64,
    pub candidate: u32,
    pub last_log_index: u64,
    pub last_log_term: u64,
}

impl VoteReq {
    pub const LEN: usize = 28;
    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![0u8; Self::LEN];
        binding::p2_votereq_encode(
            &binding::repr::vesper__chorus__wire__WireVoteReq {
                term: self.term,
                candidate: vesper_port_abi::Word::new(self.candidate),
                last_log_index: self.last_log_index,
                last_log_term: self.last_log_term,
            },
            &mut o,
        );
        o
    }
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        let v = binding::p2_votereq_decode(b).map_err(port::wire_err_to_raft)?;
        Ok(Self {
            term: v.term,
            candidate: v.candidate.get(),
            last_log_index: v.last_log_index,
            last_log_term: v.last_log_term,
        })
    }
}

// ---------------------------------------------------------------- configuration

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuorumRule {
    /// `n / 2 + 1`: any two quorums intersect.
    Majority,
    /// `n / 2`: deliberately wrong; two disjoint quorums exist.
    HalfFloor,
}

impl QuorumRule {
    pub fn size(self, n: u32) -> u32 {
        let rule = match self {
            QuorumRule::Majority => 0,
            QuorumRule::HalfFloor => 1,
        };
        binding::p2_quorum_size(rule, u64::from(n)) as u32
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RaftConfig {
    pub nodes: u32,
    pub quorum: QuorumRule,
    pub duration_ns: u64,
    pub heartbeat_ns: u64,
    pub election_min_ns: u64,
    pub election_max_ns: u64,
    pub rpc_timeout_ns: u64,
    pub min_latency_ns: u64,
    pub max_latency_ns: u64,
    pub drop_per_mille: u32,
    pub duplicate_per_mille: u32,
    /// Clock rate error bound, parts per million, per node.
    pub max_skew_ppm: u64,
    pub crashes: bool,
    pub partitions: bool,
    /// Chance per heartbeat that the leader accepts a client command.
    pub propose_per_mille: u32,
    pub max_log: usize,
    pub max_batch: usize,
    pub max_steps: usize,
    /// Compact once `commit - snapshot_index` reaches this (0: never).
    pub snapshot_threshold: u64,
    /// Voting members at start: nodes `0..initial_members` (the others are
    /// spare servers that membership changes can add).
    pub initial_members: u32,
    /// Chance per heartbeat that an eligible leader proposes adding or
    /// removing one server (0: fixed membership).
    pub membership_per_mille: u32,
    /// Membership never shrinks below this.
    pub min_members: u32,
    /// Keep each server's term, vote, snapshot and log in a
    /// [`DurableLog`] on a [`SimDisk`], persisted (and synced) at the end
    /// of every step before its messages leave; a crash loses unsynced
    /// data and a restart recovers from the disk.
    pub durable: bool,
    /// With `durable`: chance per persisting step (per mille) that power
    /// fails during the persist, before the log sync. The step's messages
    /// are lost, the server recovers from what reached the disk and
    /// restarts at once.
    pub torn_crash_per_mille: u32,
    /// Deliberately unsafe (for negative tests): a torn persist still
    /// sends the step's messages, i.e. the server answers before its log
    /// is synced.
    pub reply_before_sync: bool,
}

impl RaftConfig {
    pub fn new(nodes: u32) -> Self {
        Self {
            nodes,
            quorum: QuorumRule::Majority,
            duration_ns: 2_000_000_000,
            heartbeat_ns: 50_000_000,
            election_min_ns: 150_000_000,
            election_max_ns: 300_000_000,
            rpc_timeout_ns: 120_000_000,
            min_latency_ns: 500_000,
            max_latency_ns: 15_000_000,
            drop_per_mille: 20,
            duplicate_per_mille: 50,
            max_skew_ppm: 150_000,
            crashes: true,
            partitions: true,
            propose_per_mille: 600,
            max_log: 48,
            max_batch: 8,
            max_steps: 2_000_000,
            snapshot_threshold: 0,
            initial_members: nodes,
            membership_per_mille: 0,
            min_members: 3,
            durable: false,
            torn_crash_per_mille: 0,
            reply_before_sync: false,
        }
    }
}

// ---------------------------------------------------------------- checker

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    /// Two different nodes were leader in the same term.
    ElectionSafety { term: u64, first: u32, second: u32 },
    /// Two logs hold an entry with the same index and term but differ at or
    /// before it.
    LogMatching { a: u32, b: u32, index: usize },
    /// A node committed an entry that differs from the cluster's committed
    /// entry at that index.
    CommittedDivergence { node: u32, index: usize },
    /// A node changed an entry inside its compacted snapshot prefix.
    CompactedEntryChanged { node: u32, index: usize },
    /// A restarted node recovered a state from its disk that differs from
    /// the state it last synced (or the disk did not recover at all).
    RecoveryLostState { node: u32 },
    /// After a power failure during a persist, the recovered state is
    /// neither the last synced state nor a state between it and the one
    /// being written.
    RecoveryInconsistent { node: u32 },
}

/// Invariant checker over the whole cluster.
pub struct Checker {
    core: binding::vesper__chorus__wire__CheckerCore,
    leaders: BTreeMap<u64, u32>,
    pub violation: Option<(u64, Violation)>,
    pub checks: u64,
}

impl Default for Checker {
    fn default() -> Self {
        Self {
            core: binding::p2_checker_new(),
            leaders: BTreeMap::new(),
            violation: None,
            checks: 0,
        }
    }
}

impl Checker {
    fn sync(&mut self) {
        let r = self.core.as_ref();
        self.checks = r.checks();
        if self.violation.is_none() {
            self.violation = port::checker_violation(&r);
        }
    }

    pub fn leader_elected(&mut self, now: u64, term: u64, node: u32) {
        if binding::p2_checker_leader_elected(&mut self.core, now, term, u64::from(node)) {
            self.leaders.insert(term, node);
        }
        self.sync();
    }

    /// Log matching between `a` and every other log.
    pub fn logs_changed(&mut self, now: u64, a: usize, logs: &[&[Entry]]) {
        let mut arena = Vec::new();
        let mut offs = Vec::with_capacity(logs.len());
        let mut lens = Vec::with_capacity(logs.len());
        for log in logs {
            offs.push(arena.len() as u64);
            lens.push(log.len() as u64);
            arena.extend(
                log.iter()
                    .map(|e| binding::repr::vesper__chorus__wire__RaftEntry {
                        term: e.term,
                        cmd: e.cmd,
                    }),
            );
        }
        binding::p2_checker_logs_changed(&mut self.core, now, a as u64, &arena, &offs, &lens);
        self.sync();
    }

    pub fn committed(&mut self, now: u64, node: u32, log: &[Entry], commit: u64) {
        let raw: Vec<binding::repr::vesper__chorus__wire__RaftEntry> = log
            .iter()
            .map(|e| binding::repr::vesper__chorus__wire__RaftEntry {
                term: e.term,
                cmd: e.cmd,
            })
            .collect();
        binding::p2_checker_committed(&mut self.core, now, u64::from(node), &raw, commit);
        self.sync();
    }

    pub fn compacted_changed(&mut self, now: u64, node: u32, index: usize) {
        binding::p2_checker_compacted_changed(&mut self.core, now, u64::from(node), index as u64);
        self.sync();
    }

    pub fn leaders(&self) -> &BTreeMap<u64, u32> {
        &self.leaders
    }

    pub fn committed_len(&self) -> usize {
        self.core.as_ref().committed().len()
    }
}

/// First index where log matching fails between two logs, if any.
pub fn log_matching_violation(x: &[Entry], y: &[Entry]) -> Option<usize> {
    let rx: Vec<binding::repr::vesper__chorus__wire__RaftEntry> = x
        .iter()
        .map(|e| binding::repr::vesper__chorus__wire__RaftEntry {
            term: e.term,
            cmd: e.cmd,
        })
        .collect();
    let ry: Vec<binding::repr::vesper__chorus__wire__RaftEntry> = y
        .iter()
        .map(|e| binding::repr::vesper__chorus__wire__RaftEntry {
            term: e.term,
            cmd: e.cmd,
        })
        .collect();
    let idx = binding::p2_log_matching(&rx, &ry);
    (idx != u64::MAX).then_some(idx as usize)
}

// ---------------------------------------------------------------- nodes

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Follower,
    Candidate,
    Leader,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RaftStats {
    pub elections: u64,
    pub leaders_elected: u64,
    pub max_term: u64,
    pub sent: u64,
    pub dropped: u64,
    pub partitioned: u64,
    pub duplicated: u64,
    pub crashes: u64,
    pub stale_responses: u64,
    pub duplicate_requests: u64,
    pub rejected_frames: u64,
    pub timeouts: u64,
    pub proposals: u64,
    pub compactions: u64,
    pub snapshots_sent: u64,
    pub snapshots_installed: u64,
    pub config_changes: u64,
    pub leader_step_downs: u64,
    /// Vote requests ignored under leader stickiness.
    pub sticky_vote_denials: u64,
    /// Durable mode: persists that wrote something, and log syncs.
    pub persists: u64,
    pub disk_syncs: u64,
    /// Restarts recovered from disk.
    pub recoveries: u64,
    /// Power failures during a persist.
    pub torn_crashes: u64,
    /// Log entries written in a torn persist that did not survive it.
    pub torn_entries_lost: u64,
    /// Bytes of torn log tails dropped by recovery.
    pub torn_bytes_dropped: u64,
    /// Log truncations and snapshot installs/compactions written to disk.
    pub disk_truncations: u64,
    pub disk_snapshots: u64,
}

// ---------------------------------------------------------------- cluster run

#[derive(Debug)]
pub struct RaftRunReport {
    pub seed: u64,
    pub journal: Journal,
    /// FNV-1a digest of every node's final persistent state and the stats.
    pub digest: u64,
    pub violation: Option<(u64, Violation)>,
    pub stats: RaftStats,
    pub audit: AuditCounts,
    /// Sessions still open after shutdown (must be 0).
    pub open_sessions: u64,
    pub leaders: BTreeMap<u64, u32>,
    pub committed: usize,
    pub checks: u64,
    pub end_ns: u64,
    /// Each node's newest configuration (voting-member bit mask).
    pub members: Vec<u64>,
}

#[derive(Debug)]
pub enum RaftRunError {
    Sim(vesper_simpulse::Error),
    Config(&'static str),
    StepBudget,
}

enum Task {
    Network,
    Node(u32),
    Chaos,
}

/// Crash a server's disk (power loss) and recover its state.
fn recover(dl: &mut Option<DurableLog<SimDisk>>, stats: &mut RaftStats) -> Option<PersistentState> {
    let mut disk = dl.take()?.into_disk();
    disk.crash();
    let (log, rec) = DurableLog::open(disk).ok()?;
    stats.recoveries += 1;
    stats.torn_bytes_dropped += rec.torn_bytes;
    let state = log.state().clone();
    *dl = Some(log);
    Some(state)
}

fn persist(
    dl: &mut Option<DurableLog<SimDisk>>,
    synced: &mut PersistentState,
    target: PersistentState,
    torn: bool,
    stats: &mut RaftStats,
) -> Result<Option<PersistentState>, u64> {
    let log = dl.as_mut().ok_or(1u64)?;
    let before = log.stats;
    let r = if torn {
        log.persist_unsynced(&target)
    } else {
        log.persist(&target)
    };
    r.map_err(|_| 1u64)?;
    stats.persists += 1;
    stats.disk_syncs += log.stats.syncs - before.syncs;
    stats.disk_truncations += log.stats.truncations - before.truncations;
    stats.disk_snapshots += log.stats.snapshots - before.snapshots;
    if !torn {
        *synced = target;
        return Ok(None);
    }
    stats.torn_crashes += 1;
    let state = recover(dl, stats).ok_or(1u64)?;
    // Meta and snapshot replacements are synced as they are written; only
    // the log's unsynced appends and truncation can be lost. The log must
    // be the old one or a prefix of the new one that keeps every entry
    // the two share.
    let old = synced.full_log();
    let new = target.full_log();
    let got = state.full_log();
    let shared = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let log_ok = got == old || (new.starts_with(&got) && got.len() >= shared);
    let meta_ok = (state.term, state.voted_for) == (target.term, target.voted_for)
        && state.snapshot_index == target.snapshot_index
        && state.snapshot_term == target.snapshot_term;
    if !(log_ok && meta_ok) {
        return Err(2u64);
    }
    stats.torn_entries_lost += new.len().saturating_sub(got.len()) as u64;
    *synced = state.clone();
    Ok(Some(state))
}

fn wire_log_from_state(s: &PersistentState) -> Vec<binding::repr::vesper__chorus__wire__RaftEntry> {
    s.full_log()
        .into_iter()
        .map(|e| binding::repr::vesper__chorus__wire__RaftEntry {
            term: e.term,
            cmd: e.cmd,
        })
        .collect()
}

fn read_persistent(rc: &binding::vesper__chorus__raft__RaftCluster, i: u32) -> PersistentState {
    let p = binding::p2_raft_node_persistent(rc, u64::from(i));
    let pool = rc.as_ref().log_pool();
    let start = p.log_off as usize;
    let end = start + p.log_len as usize;
    let snap = (p.snap_index as usize).min(p.log_len as usize);
    let full: Vec<Entry> = (start..end)
        .map(|idx| {
            let e = pool.get(idx).expect("log entry");
            Entry {
                term: e.term,
                cmd: e.cmd,
            }
        })
        .collect();
    PersistentState {
        term: p.term,
        voted_for: (p.voted_for != u64::MAX).then_some(p.voted_for as u32),
        snapshot_index: p.snap_index,
        snapshot_term: p.snap_term,
        snapshot_image: full[..snap].to_vec(),
        log: full[snap..].to_vec(),
    }
}

/// Run one seeded cluster to `cfg.duration_ns`. `replay` re-runs a recorded
/// SimPulse journal and checks every scheduling event against it.
pub fn run_cluster(
    seed: u64,
    cfg: &RaftConfig,
    replay: Option<Journal>,
) -> Result<RaftRunReport, RaftRunError> {
    let vcfg = binding::repr::vesper__chorus__raft__RaftCfg {
        nodes: u64::from(cfg.nodes),
        quorum: match cfg.quorum {
            QuorumRule::Majority => 0,
            QuorumRule::HalfFloor => 1,
        },
        duration_ns: cfg.duration_ns,
        heartbeat_ns: cfg.heartbeat_ns,
        election_min_ns: cfg.election_min_ns,
        election_max_ns: cfg.election_max_ns,
        rpc_timeout_ns: cfg.rpc_timeout_ns,
        min_latency_ns: cfg.min_latency_ns,
        max_latency_ns: cfg.max_latency_ns,
        drop_per_mille: u64::from(cfg.drop_per_mille),
        duplicate_per_mille: u64::from(cfg.duplicate_per_mille),
        max_skew_ppm: cfg.max_skew_ppm,
        crashes: vesper_port_abi::Word::new(cfg.crashes),
        partitions: vesper_port_abi::Word::new(cfg.partitions),
        propose_per_mille: u64::from(cfg.propose_per_mille),
        max_log: cfg.max_log as u64,
        max_batch: cfg.max_batch as u64,
        max_steps: cfg.max_steps as u64,
        snapshot_threshold: cfg.snapshot_threshold,
        initial_members: u64::from(cfg.initial_members),
        membership_per_mille: u64::from(cfg.membership_per_mille),
        min_members: u64::from(cfg.min_members),
        durable: vesper_port_abi::Word::new(cfg.durable),
        torn_crash_per_mille: u64::from(cfg.torn_crash_per_mille),
        reply_before_sync: vesper_port_abi::Word::new(cfg.reply_before_sync),
    };
    match binding::p2_raft_validate_cfg(&vcfg) {
        0 => {}
        1 => return Err(RaftRunError::Config("nodes must be in 1..=64")),
        2 => return Err(RaftRunError::Config("inverted range")),
        3 => return Err(RaftRunError::Config("max_batch exceeds the wire limit")),
        4 => {
            return Err(RaftRunError::Config(
                "snapshots need max_log within the wire limit (the image is one batch)",
            ));
        }
        5 => return Err(RaftRunError::Config("initial_members must be in 1..=nodes")),
        6 => {
            return Err(RaftRunError::Config(
                "membership change supports at most 63 nodes",
            ));
        }
        _ => return Err(RaftRunError::Config("torn crashes need durable storage")),
    }
    let n = cfg.nodes;
    let mut disk_rng = SplitMix(seed ^ 0x6475_7261_626c_6521);
    let mut disks: Vec<Option<DurableLog<SimDisk>>> = Vec::new();
    let mut synced: Vec<PersistentState> = Vec::new();
    if cfg.durable {
        for i in 0..n {
            let disk = SimDisk::new(seed ^ (u64::from(i) << 40));
            let (dl, _) = DurableLog::open(disk)
                .map_err(|_| RaftRunError::Config("cannot format the simulated disk"))?;
            disks.push(Some(dl));
            synced.push(PersistentState::default());
        }
    }
    let mut rc = binding::p2_raft_new(seed, &vcfg);
    let mut disk_stats = RaftStats::default();
    let mut sim: Simulator<Task> = match replay {
        None => Simulator::new(seed),
        Some(j) => Simulator::replay(j).map_err(RaftRunError::Sim)?,
    };
    let spawn = |sim: &mut Simulator<Task>, t| sim.spawn(t).map_err(|e| RaftRunError::Sim(e.error));
    spawn(&mut sim, Task::Network)?;
    for i in 0..n {
        spawn(&mut sim, Task::Node(i))?;
    }
    spawn(&mut sim, Task::Chaos)?;
    let tr = |e: TransitionError<Task>| RaftRunError::Sim(e.error);
    let mut node_wait: Vec<Option<CompletionId>> = vec![None; n as usize];
    let mut node_done = vec![false; n as usize];
    let mut net_wait: Option<(CompletionId, Option<u64>)> = None;

    for _ in 0..cfg.max_steps {
        let Some(token) = sim.next_task().map_err(RaftRunError::Sim)? else {
            break;
        };
        let now = sim.now_ns();
        let step_out = match *token.payload() {
            Task::Network => {
                net_wait = None;
                let out = binding::p2_raft_step_network(&mut rc, now);
                for &d in rc.as_ref().woken().as_slice() {
                    if let Some(c) = node_wait[d as usize].take() {
                        sim.complete_io(c, IoResult::Success { bytes: 0 })
                            .map_err(RaftRunError::Sim)?;
                    }
                }
                if out.code == 1 {
                    sim.finish(token).map_err(tr)?;
                } else if out.has_wake.get() {
                    let c = sim
                        .schedule_io(token, out.wake_delay, IoResult::Success { bytes: 0 })
                        .map_err(tr)?;
                    net_wait = Some((c, Some(out.net_wake_earliest)));
                } else {
                    let c = sim.park_io(token).map_err(tr)?;
                    net_wait = Some((c, None));
                }
                out
            }
            Task::Chaos => {
                let mut out = binding::p2_raft_step_chaos(&mut rc, now);
                while out.code == 3 {
                    let iu = out.arg as usize;
                    let rec = recover(&mut disks[iu], &mut disk_stats);
                    let (ok, s) = match rec {
                        Some(st) if st == synced[iu] => (true, st),
                        _ => (false, PersistentState::default()),
                    };
                    let log = if ok {
                        wire_log_from_state(&s)
                    } else {
                        Vec::new()
                    };
                    out = binding::p2_raft_chaos_finish_restart(
                        &mut rc,
                        out.arg,
                        now,
                        ok,
                        s.term,
                        s.voted_for.map_or(u64::MAX, u64::from),
                        s.snapshot_index,
                        s.snapshot_term,
                        &log,
                    );
                }
                for &d in rc.as_ref().woken().as_slice() {
                    if let Some(c) = node_wait[d as usize].take() {
                        sim.complete_io(c, IoResult::Success { bytes: 0 })
                            .map_err(RaftRunError::Sim)?;
                    }
                }
                if out.code == 1 {
                    sim.finish(token).map_err(tr)?;
                } else {
                    sim.sleep(token, out.wake_delay).map_err(tr)?;
                }
                out
            }
            Task::Node(i) => {
                let iu = i as usize;
                let mut out = binding::p2_raft_step_node(&mut rc, u64::from(i), now);
                if out.code == 2 {
                    let target = read_persistent(&rc, i);
                    let torn = disk_rng.chance(cfg.torn_crash_per_mille);
                    out = match persist(
                        &mut disks[iu],
                        &mut synced[iu],
                        target,
                        torn,
                        &mut disk_stats,
                    ) {
                        Ok(None) => binding::p2_raft_finish_node_persist(
                            &mut rc,
                            u64::from(i),
                            now,
                            0,
                            false,
                            0,
                            u64::MAX,
                            0,
                            0,
                            &[],
                        ),
                        Ok(Some(state)) => {
                            let rec_log = wire_log_from_state(&state);
                            binding::p2_raft_finish_node_persist(
                                &mut rc,
                                u64::from(i),
                                now,
                                0,
                                true,
                                state.term,
                                state.voted_for.map_or(u64::MAX, u64::from),
                                state.snapshot_index,
                                state.snapshot_term,
                                &rec_log,
                            )
                        }
                        Err(err_kind) => binding::p2_raft_finish_node_persist(
                            &mut rc,
                            u64::from(i),
                            now,
                            err_kind,
                            false,
                            0,
                            u64::MAX,
                            0,
                            0,
                            &[],
                        ),
                    };
                }
                if out.code == 1 {
                    node_done[iu] = true;
                    sim.finish(token).map_err(tr)?;
                } else {
                    let c = sim
                        .schedule_io(token, out.wake_delay, IoResult::Success { bytes: 0 })
                        .map_err(tr)?;
                    node_wait[iu] = Some(c);
                }
                out
            }
        };
        if let Some((c, deadline)) = net_wait {
            let earliest = step_out
                .net_wake_has_due
                .get()
                .then_some(step_out.net_wake_earliest);
            let wake = match (earliest, deadline) {
                (Some(e), Some(d)) => e < d,
                (Some(_), None) => true,
                _ => false,
            } || node_done.iter().all(|d| *d);
            if wake {
                sim.complete_io(c, IoResult::Success { bytes: 0 })
                    .map_err(RaftRunError::Sim)?;
                net_wait = None;
            }
        }
        if step_out.has_violation.get() {
            break;
        }
    }
    if !binding::p2_raft_has_violation(&rc) && !sim.is_complete() {
        return Err(RaftRunError::StepBudget);
    }
    // No-op for a recording simulator; for a replay, the whole journal must
    // have been consumed.
    sim.finish_replay().map_err(RaftRunError::Sim)?;
    let digest = binding::p2_raft_shutdown_and_digest(&mut rc);
    let vs = binding::p2_raft_stats(&rc);
    let stats = RaftStats {
        elections: vs.elections,
        leaders_elected: vs.leaders_elected,
        max_term: vs.max_term,
        sent: vs.sent,
        dropped: vs.dropped,
        partitioned: vs.partitioned,
        duplicated: vs.duplicated,
        crashes: vs.crashes,
        stale_responses: vs.stale_responses,
        duplicate_requests: vs.duplicate_requests,
        rejected_frames: vs.rejected_frames,
        timeouts: vs.timeouts,
        proposals: vs.proposals,
        compactions: vs.compactions,
        snapshots_sent: vs.snapshots_sent,
        snapshots_installed: vs.snapshots_installed,
        config_changes: vs.config_changes,
        leader_step_downs: vs.leader_step_downs,
        sticky_vote_denials: vs.sticky_vote_denials,
        persists: disk_stats.persists,
        disk_syncs: disk_stats.disk_syncs,
        recoveries: disk_stats.recoveries,
        torn_crashes: disk_stats.torn_crashes,
        torn_entries_lost: disk_stats.torn_entries_lost,
        torn_bytes_dropped: disk_stats.torn_bytes_dropped,
        disk_truncations: disk_stats.disk_truncations,
        disk_snapshots: disk_stats.disk_snapshots,
    };
    let r = rc.as_ref();
    let chk = r.checker();
    let audit = AuditCounts {
        opened: r.audit_opened(),
        closed: r.audit_closed(),
        faulted: r.audit_faulted(),
        leaked: 0,
        duplicates_discarded: 0,
    };
    Ok(RaftRunReport {
        seed,
        journal: sim.journal().clone(),
        digest,
        violation: port::checker_violation(&chk),
        stats,
        open_sessions: binding::p2_raft_open_sessions(&rc),
        audit,
        leaders: port::checker_leaders(&chk),
        committed: chk.committed().len(),
        checks: chk.checks(),
        end_ns: sim.now_ns(),
        members: (0..n)
            .map(|i| binding::p2_raft_node_members(&rc, u64::from(i)))
            .collect(),
    })
}
