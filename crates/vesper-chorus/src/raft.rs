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

use crate::chor::{Choreography, RoleId};
use crate::parse::parse_choreography;
use crate::session::{AuditCounts, FaultReason, LeakAuditor, NetFail, RawSession};
use crate::simnet::SplitMix;
use std::collections::{BTreeMap, BTreeSet};
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
    log.iter()
        .rev()
        .find(|e| e.cmd & CONFIG_FLAG != 0)
        .map_or(initial, |e| e.cmd & !CONFIG_FLAG)
}

fn config_index(log: &[Entry]) -> u64 {
    log.iter()
        .rposition(|e| e.cmd & CONFIG_FLAG != 0)
        .map_or(0, |i| i as u64 + 1)
}

fn member(mask: u64, id: u32) -> bool {
    id < 64 && mask >> id & 1 == 1
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

fn be_u64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}
fn be_u32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

impl RaftAppendWireHeader {
    pub fn encode(&self) -> [u8; APPEND_HEADER_LEN] {
        let mut o = [0u8; APPEND_HEADER_LEN];
        o[0..8].copy_from_slice(&self.term.to_be_bytes());
        o[8..12].copy_from_slice(&self.leader_id.to_be_bytes());
        o[12..16].copy_from_slice(&self.follower_slot.to_be_bytes());
        o[16..24].copy_from_slice(&self.prev_log_index.to_be_bytes());
        o[24..32].copy_from_slice(&self.prev_log_term.to_be_bytes());
        o[32..40].copy_from_slice(&self.leader_commit.to_be_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        if b.len() != APPEND_HEADER_LEN {
            return Err(WireError::Length {
                expected: APPEND_HEADER_LEN,
                found: b.len(),
            });
        }
        Ok(Self {
            term: be_u64(b, 0),
            leader_id: be_u32(b, 8),
            follower_slot: be_u32(b, 12),
            prev_log_index: be_u64(b, 16),
            prev_log_term: be_u64(b, 24),
            leader_commit: be_u64(b, 32),
        })
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
    let mut o = Vec::with_capacity(APPEND_HEADER_LEN + 4 + 16 * entries.len());
    o.extend_from_slice(&h.encode());
    o.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for e in entries {
        o.extend_from_slice(&e.term.to_be_bytes());
        o.extend_from_slice(&e.cmd.to_be_bytes());
    }
    o
}

pub fn decode_batch(b: &[u8]) -> Result<(RaftAppendWireHeader, Vec<Entry>), WireError> {
    if b.len() < APPEND_HEADER_LEN + 4 {
        return Err(WireError::Length {
            expected: APPEND_HEADER_LEN + 4,
            found: b.len(),
        });
    }
    let h = RaftAppendWireHeader::decode(&b[..APPEND_HEADER_LEN])?;
    let count = be_u32(b, APPEND_HEADER_LEN);
    if count > MAX_BATCH {
        return Err(WireError::TooManyEntries(count));
    }
    let want = APPEND_HEADER_LEN + 4 + 16 * count as usize;
    if b.len() != want {
        return Err(WireError::Length {
            expected: want,
            found: b.len(),
        });
    }
    let entries = (0..count as usize)
        .map(|i| {
            let at = APPEND_HEADER_LEN + 4 + 16 * i;
            Entry {
                term: be_u64(b, at),
                cmd: be_u64(b, at + 8),
            }
        })
        .collect();
    Ok((h, entries))
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
        let mut o = Vec::with_capacity(Self::LEN);
        o.extend_from_slice(&self.follower_id.to_be_bytes());
        o.extend_from_slice(&self.term.to_be_bytes());
        o.extend_from_slice(&self.match_index.to_be_bytes());
        o.push(u8::from(self.accepted));
        o
    }
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        if b.len() != Self::LEN {
            return Err(WireError::Length {
                expected: Self::LEN,
                found: b.len(),
            });
        }
        let accepted = match b[20] {
            0 => false,
            1 => true,
            f => return Err(WireError::BadFlag(f)),
        };
        Ok(Self {
            follower_id: be_u32(b, 0),
            term: be_u64(b, 4),
            match_index: be_u64(b, 12),
            accepted,
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
        let mut o = Vec::with_capacity(Self::LEN);
        o.extend_from_slice(&self.term.to_be_bytes());
        o.extend_from_slice(&self.candidate.to_be_bytes());
        o.extend_from_slice(&self.last_log_index.to_be_bytes());
        o.extend_from_slice(&self.last_log_term.to_be_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        if b.len() != Self::LEN {
            return Err(WireError::Length {
                expected: Self::LEN,
                found: b.len(),
            });
        }
        Ok(Self {
            term: be_u64(b, 0),
            candidate: be_u32(b, 8),
            last_log_index: be_u64(b, 12),
            last_log_term: be_u64(b, 20),
        })
    }
}

/// `VoteResp`: term (8 bytes). Granted or denied is the label.
fn encode_term(t: u64) -> Vec<u8> {
    t.to_be_bytes().to_vec()
}
fn decode_term(b: &[u8]) -> Result<u64, WireError> {
    if b.len() != 8 {
        return Err(WireError::Length {
            expected: 8,
            found: b.len(),
        });
    }
    Ok(be_u64(b, 0))
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
        match self {
            QuorumRule::Majority => n / 2 + 1,
            QuorumRule::HalfFloor => (n / 2).max(1),
        }
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
}

/// Invariant checker over the whole cluster.
#[derive(Default)]
pub struct Checker {
    leaders: BTreeMap<u64, u32>,
    committed: Vec<Entry>,
    pub violation: Option<(u64, Violation)>,
    pub checks: u64,
}

impl Checker {
    fn record(&mut self, now: u64, v: Violation) {
        if self.violation.is_none() {
            self.violation = Some((now, v));
        }
    }

    pub fn leader_elected(&mut self, now: u64, term: u64, node: u32) {
        self.checks += 1;
        match self.leaders.get(&term) {
            Some(&other) if other != node => self.record(
                now,
                Violation::ElectionSafety {
                    term,
                    first: other,
                    second: node,
                },
            ),
            _ => {
                self.leaders.insert(term, node);
            }
        }
    }

    /// Log matching between `a` and every other log.
    pub fn logs_changed(&mut self, now: u64, a: usize, logs: &[&[Entry]]) {
        for b in 0..logs.len() {
            if b == a {
                continue;
            }
            self.checks += 1;
            if let Some(index) = log_matching_violation(logs[a], logs[b]) {
                self.record(
                    now,
                    Violation::LogMatching {
                        a: a as u32,
                        b: b as u32,
                        index,
                    },
                );
                return;
            }
        }
    }

    pub fn committed(&mut self, now: u64, node: u32, log: &[Entry], commit: u64) {
        self.checks += 1;
        for (i, e) in log.iter().take(commit as usize).enumerate() {
            match self.committed.get(i) {
                Some(c) if c != e => {
                    self.record(now, Violation::CommittedDivergence { node, index: i });
                    return;
                }
                Some(_) => {}
                None => self.committed.push(*e),
            }
        }
    }

    pub fn compacted_changed(&mut self, now: u64, node: u32, index: usize) {
        self.checks += 1;
        self.record(now, Violation::CompactedEntryChanged { node, index });
    }

    pub fn leaders(&self) -> &BTreeMap<u64, u32> {
        &self.leaders
    }

    pub fn committed_len(&self) -> usize {
        self.committed.len()
    }
}

/// First index where log matching fails between two logs, if any.
pub fn log_matching_violation(x: &[Entry], y: &[Entry]) -> Option<usize> {
    let mut prefix_equal = true;
    for (i, (a, b)) in x.iter().zip(y).enumerate() {
        if a.term == b.term && (!prefix_equal || a.cmd != b.cmd) {
            return Some(i);
        }
        prefix_equal &= a == b;
    }
    None
}

// ---------------------------------------------------------------- nodes

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Follower,
    Candidate,
    Leader,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RpcKind {
    Vote {
        term: u64,
    },
    Append {
        term: u64,
        peer: u32,
        sent_last: u64,
    },
    Snapshot {
        term: u64,
        peer: u32,
        last: u64,
    },
}

struct Outstanding {
    session: RawSession,
    deadline: u64,
    kind: RpcKind,
}

/// A frame in flight between two nodes.
#[derive(Clone, Debug)]
struct Packet {
    src: u32,
    dst: u32,
    channel: u64,
    /// Sender's role in the choreography.
    from_role: RoleId,
    label: &'static str,
    payload_ty: &'static str,
    body: Vec<u8>,
    /// True for the first frame of an exchange (opens a responder session).
    request: bool,
}

struct Node {
    id: u32,
    n: u32,
    up: bool,
    incarnation: u64,
    // persistent
    term: u64,
    voted_for: Option<u32>,
    log: Vec<Entry>,
    /// Entries `1..=snap_index` are compacted into the snapshot.
    snap_index: u64,
    snap_term: u64,
    initial_members: u64,
    // volatile
    role: Role,
    commit: u64,
    votes: BTreeSet<u32>,
    next_index: Vec<u64>,
    match_index: Vec<u64>,
    election_deadline: u64,
    heartbeat_deadline: u64,
    skew_ppm: i64,
    outstanding: BTreeMap<u64, Outstanding>,
    next_channel: u64,
    inbox: Vec<Packet>,
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
}

struct Protocols {
    vote: Choreography,
    append: Choreography,
    snapshot: Choreography,
}

const CANDIDATE: RoleId = RoleId(0);
const VOTER: RoleId = RoleId(1);
const LEADER: RoleId = RoleId(0);
const FOLLOWER: RoleId = RoleId(1);

struct Ctx<'a> {
    now: u64,
    cfg: &'a RaftConfig,
    protocols: &'a Protocols,
    auditor: &'a LeakAuditor,
    rng: &'a mut SplitMix,
    out: &'a mut Vec<Packet>,
    stats: &'a mut RaftStats,
    next_cmd: &'a mut u64,
    events: &'a mut Vec<NodeEvent>,
}

/// What a node step asks the cluster to check.
enum NodeEvent {
    Leader { term: u64 },
    LogChanged,
    Committed,
    CompactedChanged { index: usize },
}

impl Node {
    fn local_to_virtual(&self, d: u64) -> u64 {
        // A fast clock (positive ppm) fires local timeouts early.
        let denom = (1_000_000i64 + self.skew_ppm).max(1) as u128;
        ((u128::from(d) * 1_000_000) / denom).min(u128::from(u64::MAX)) as u64
    }

    fn reset_election(&mut self, cx: &mut Ctx<'_>) {
        let local = cx.rng.range(cx.cfg.election_min_ns, cx.cfg.election_max_ns);
        self.election_deadline = cx.now.saturating_add(self.local_to_virtual(local));
    }

    fn last(&self) -> (u64, u64) {
        (self.log.len() as u64, self.log.last().map_or(0, |e| e.term))
    }

    fn new_channel(&mut self) -> u64 {
        self.next_channel += 1;
        (u64::from(self.id + 1) << 48)
            | ((self.incarnation & 0xffff) << 32)
            | (self.next_channel & 0xffff_ffff)
    }

    fn become_follower(&mut self, term: u64) {
        if term > self.term {
            self.term = term;
            self.voted_for = None;
        }
        self.role = Role::Follower;
        self.votes.clear();
    }

    /// Voting members of this node's newest configuration.
    fn members(&self) -> u64 {
        config_of(&self.log, self.initial_members)
    }

    fn quorum(&self, cx: &Ctx<'_>) -> usize {
        cx.cfg.quorum.size(self.members().count_ones()) as usize
    }

    fn crash(&mut self) {
        self.up = false;
        self.role = Role::Follower;
        // The snapshot is committed state and survives the crash.
        self.commit = self.snap_index;
        self.votes.clear();
        self.inbox.clear();
        for (_, o) in std::mem::take(&mut self.outstanding) {
            o.session.fault(FaultReason::Crash);
        }
    }

    fn restart(&mut self, cx: &mut Ctx<'_>) {
        self.up = true;
        self.incarnation += 1;
        self.next_channel = 0;
        self.reset_election(cx);
    }

    fn shutdown(&mut self) {
        for (_, o) in std::mem::take(&mut self.outstanding) {
            o.session.fault(FaultReason::Net(NetFail::Shutdown));
        }
    }

    fn start_election(&mut self, cx: &mut Ctx<'_>) {
        cx.stats.elections += 1;
        self.term += 1;
        cx.stats.max_term = cx.stats.max_term.max(self.term);
        self.role = Role::Candidate;
        self.voted_for = Some(self.id);
        self.votes.clear();
        self.votes.insert(self.id);
        self.reset_election(cx);
        let (li, lt) = self.last();
        let members = self.members();
        for p in 0..self.n {
            if p == self.id || !member(members, p) {
                continue;
            }
            let ch = self.new_channel();
            let mut s = RawSession::open(&cx.protocols.vote, "Candidate", ch, cx.auditor)
                .expect("vote choreography projects");
            s.on_send(VOTER, "RequestVote", "VoteReq")
                .expect("projection starts with RequestVote");
            let req = VoteReq {
                term: self.term,
                candidate: self.id,
                last_log_index: li,
                last_log_term: lt,
            };
            cx.out.push(Packet {
                src: self.id,
                dst: p,
                channel: ch,
                from_role: CANDIDATE,
                label: "RequestVote",
                payload_ty: "VoteReq",
                body: req.encode(),
                request: true,
            });
            self.outstanding.insert(
                ch,
                Outstanding {
                    session: s,
                    deadline: cx.now.saturating_add(cx.cfg.rpc_timeout_ns),
                    kind: RpcKind::Vote { term: self.term },
                },
            );
        }
        self.maybe_win(cx);
    }

    fn maybe_win(&mut self, cx: &mut Ctx<'_>) {
        let members = self.members();
        let votes = self.votes.iter().filter(|v| member(members, **v)).count();
        if self.role == Role::Candidate && votes >= self.quorum(cx) {
            self.role = Role::Leader;
            cx.stats.leaders_elected += 1;
            let next = self.log.len() as u64 + 1;
            self.next_index = vec![next; self.n as usize];
            self.match_index = vec![0; self.n as usize];
            self.heartbeat_deadline = cx.now;
            cx.events.push(NodeEvent::Leader { term: self.term });
        }
    }

    fn send_appends(&mut self, cx: &mut Ctx<'_>) {
        if self.log.len() < cx.cfg.max_log && cx.rng.chance(cx.cfg.propose_per_mille) {
            *cx.next_cmd += 1;
            self.log.push(Entry {
                term: self.term,
                cmd: *cx.next_cmd,
            });
            cx.stats.proposals += 1;
            cx.events.push(NodeEvent::LogChanged);
            // A quorum the leader alone satisfies (one node) commits now.
            self.advance_commit(cx);
        }
        self.maybe_change_membership(cx);
        if self.role != Role::Leader {
            return;
        }
        let members = self.members();
        for p in 0..self.n {
            if p == self.id || !member(members, p) {
                continue;
            }
            let next = self.next_index[p as usize].clamp(1, self.log.len() as u64 + 1);
            let prev = next - 1;
            if prev < self.snap_index {
                self.send_snapshot(cx, p);
                continue;
            }
            let prev_term = if prev == 0 {
                0
            } else {
                self.log[prev as usize - 1].term
            };
            let entries: Vec<Entry> = self.log[prev as usize..]
                .iter()
                .take(cx.cfg.max_batch)
                .copied()
                .collect();
            let h = RaftAppendWireHeader {
                term: self.term,
                leader_id: self.id,
                follower_slot: p,
                prev_log_index: prev,
                prev_log_term: prev_term,
                leader_commit: self.commit,
            };
            let ch = self.new_channel();
            let mut s = RawSession::open(&cx.protocols.append, "Leader", ch, cx.auditor)
                .expect("append choreography projects");
            s.on_send(FOLLOWER, "AppendEntries", "AppendBatch")
                .expect("projection starts with AppendEntries");
            let sent_last = prev + entries.len() as u64;
            cx.out.push(Packet {
                src: self.id,
                dst: p,
                channel: ch,
                from_role: LEADER,
                label: "AppendEntries",
                payload_ty: "AppendBatch",
                body: encode_batch(&h, &entries),
                request: true,
            });
            self.outstanding.insert(
                ch,
                Outstanding {
                    session: s,
                    deadline: cx.now.saturating_add(cx.cfg.rpc_timeout_ns),
                    kind: RpcKind::Append {
                        term: self.term,
                        peer: p,
                        sent_last,
                    },
                },
            );
        }
        self.heartbeat_deadline = cx
            .now
            .saturating_add(self.local_to_virtual(cx.cfg.heartbeat_ns));
    }

    /// Single-server membership change: only once an entry of this term is
    /// committed and no configuration entry is pending.
    fn maybe_change_membership(&mut self, cx: &mut Ctx<'_>) {
        if cx.cfg.membership_per_mille == 0
            || self.log.len() >= cx.cfg.max_log
            || self.commit == 0
            || self.log[self.commit as usize - 1].term != self.term
            || config_index(&self.log) > self.commit
            || !cx.rng.chance(cx.cfg.membership_per_mille)
        {
            return;
        }
        let members = self.members();
        let r = cx.rng.range(0, u64::from(self.n) - 1) as u32;
        let next = if member(members, r) {
            if members.count_ones() <= cx.cfg.min_members {
                return;
            }
            members & !(1 << r)
        } else {
            members | (1 << r)
        };
        self.log.push(Entry {
            term: self.term,
            cmd: CONFIG_FLAG | next,
        });
        cx.stats.config_changes += 1;
        cx.events.push(NodeEvent::LogChanged);
        if member(next, r) {
            // A new member starts from the beginning of the log.
            self.next_index[r as usize] = 1;
            self.match_index[r as usize] = 0;
        }
        self.advance_commit(cx);
    }

    fn send_snapshot(&mut self, cx: &mut Ctx<'_>, p: u32) {
        let h = RaftAppendWireHeader {
            term: self.term,
            leader_id: self.id,
            follower_slot: p,
            prev_log_index: self.snap_index,
            prev_log_term: self.snap_term,
            leader_commit: self.commit,
        };
        let body = encode_batch(&h, &self.log[..self.snap_index as usize]);
        let ch = self.new_channel();
        let mut s = RawSession::open(&cx.protocols.snapshot, "Leader", ch, cx.auditor)
            .expect("snapshot choreography projects");
        s.on_send(FOLLOWER, "InstallSnapshot", "SnapshotImage")
            .expect("projection starts with InstallSnapshot");
        cx.stats.snapshots_sent += 1;
        cx.out.push(Packet {
            src: self.id,
            dst: p,
            channel: ch,
            from_role: LEADER,
            label: "InstallSnapshot",
            payload_ty: "SnapshotImage",
            body,
            request: true,
        });
        self.outstanding.insert(
            ch,
            Outstanding {
                session: s,
                deadline: cx.now.saturating_add(cx.cfg.rpc_timeout_ns),
                kind: RpcKind::Snapshot {
                    term: self.term,
                    peer: p,
                    last: self.snap_index,
                },
            },
        );
    }

    fn advance_commit(&mut self, cx: &mut Ctx<'_>) {
        let q = self.quorum(cx);
        let members = self.members();
        for idx in (self.commit + 1..=self.log.len() as u64).rev() {
            if self.log[idx as usize - 1].term != self.term {
                continue;
            }
            let count = usize::from(member(members, self.id))
                + (0..self.n)
                    .filter(|p| {
                        *p != self.id && member(members, *p) && self.match_index[*p as usize] >= idx
                    })
                    .count();
            if count >= q {
                self.commit = idx;
                cx.events.push(NodeEvent::Committed);
                break;
            }
        }
        // A leader that a committed configuration removed steps down.
        if self.role == Role::Leader
            && !member(members, self.id)
            && config_index(&self.log) <= self.commit
        {
            cx.stats.leader_step_downs += 1;
            self.role = Role::Follower;
        }
    }

    /// Compact the committed prefix once it is long enough.
    fn maybe_compact(&mut self, cx: &mut Ctx<'_>) {
        let t = cx.cfg.snapshot_threshold;
        if t > 0 && self.commit >= self.snap_index.saturating_add(t) {
            self.snap_index = self.commit;
            self.snap_term = self.log[self.commit as usize - 1].term;
            cx.stats.compactions += 1;
        }
    }

    /// Record an attempted change at `idx` (0-based) inside the snapshot.
    fn guard_compacted(&self, cx: &mut Ctx<'_>, idx: usize) {
        if (idx as u64) < self.snap_index {
            cx.events.push(NodeEvent::CompactedChanged { index: idx });
        }
    }

    fn handle_request(&mut self, cx: &mut Ctx<'_>, pk: Packet) {
        match pk.label {
            "RequestVote" => {
                let mut s =
                    match RawSession::open(&cx.protocols.vote, "Voter", pk.channel, cx.auditor) {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                let req = match s
                    .on_recv(pk.from_role, pk.label, pk.payload_ty)
                    .ok()
                    .and_then(|()| VoteReq::decode(&pk.body).ok())
                {
                    Some(r) => r,
                    None => {
                        cx.stats.rejected_frames += 1;
                        s.fault(FaultReason::Violation);
                        return;
                    }
                };
                if req.term > self.term {
                    self.become_follower(req.term);
                }
                let up_to_date = (req.last_log_term, req.last_log_index) >= {
                    let (i, t) = self.last();
                    (t, i)
                };
                let grant = req.term == self.term
                    && self.voted_for.is_none_or(|v| v == req.candidate)
                    && up_to_date;
                if grant {
                    self.voted_for = Some(req.candidate);
                    self.reset_election(cx);
                }
                let label = if grant { "Grant" } else { "Deny" };
                s.on_send(CANDIDATE, label, "VoteResp")
                    .expect("Voter projection offers Grant and Deny");
                cx.out.push(Packet {
                    src: self.id,
                    dst: pk.src,
                    channel: pk.channel,
                    from_role: VOTER,
                    label,
                    payload_ty: "VoteResp",
                    body: encode_term(self.term),
                    request: false,
                });
                if let Err((_, s)) = s.close() {
                    s.fault(FaultReason::Violation);
                }
            }
            "AppendEntries" => {
                let mut s = match RawSession::open(
                    &cx.protocols.append,
                    "Follower",
                    pk.channel,
                    cx.auditor,
                ) {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let (h, entries) = match s
                    .on_recv(pk.from_role, pk.label, pk.payload_ty)
                    .ok()
                    .and_then(|()| decode_batch(&pk.body).ok())
                {
                    Some(r) => r,
                    None => {
                        cx.stats.rejected_frames += 1;
                        s.fault(FaultReason::Violation);
                        return;
                    }
                };
                let mut accepted = false;
                let mut match_index = 0;
                if h.term >= self.term {
                    if h.term > self.term || self.role != Role::Follower {
                        self.become_follower(h.term);
                    }
                    self.reset_election(cx);
                    let prev = h.prev_log_index;
                    let prev_ok = prev == 0
                        || (prev <= self.log.len() as u64
                            && self.log[prev as usize - 1].term == h.prev_log_term);
                    if prev_ok {
                        let mut changed = false;
                        for (k, e) in entries.iter().enumerate() {
                            let idx = prev as usize + k;
                            if idx < self.log.len() {
                                if self.log[idx].term != e.term {
                                    self.guard_compacted(cx, idx);
                                    self.log.truncate(idx);
                                    self.log.push(*e);
                                    changed = true;
                                }
                            } else {
                                self.log.push(*e);
                                changed = true;
                            }
                        }
                        if changed {
                            cx.events.push(NodeEvent::LogChanged);
                        }
                        match_index = prev + entries.len() as u64;
                        let new_commit = h.leader_commit.min(match_index);
                        if new_commit > self.commit {
                            self.commit = new_commit;
                            cx.events.push(NodeEvent::Committed);
                        }
                        accepted = true;
                    }
                }
                s.on_action("persist")
                    .expect("Follower persists before replying");
                let label = if accepted { "Accept" } else { "Reject" };
                s.on_send(LEADER, label, "AppendAck")
                    .expect("Follower projection offers Accept and Reject");
                let ack = AppendAck {
                    follower_id: self.id,
                    term: self.term,
                    match_index,
                    accepted,
                };
                cx.out.push(Packet {
                    src: self.id,
                    dst: pk.src,
                    channel: pk.channel,
                    from_role: FOLLOWER,
                    label,
                    payload_ty: "AppendAck",
                    body: ack.encode(),
                    request: false,
                });
                if let Err((_, s)) = s.close() {
                    s.fault(FaultReason::Violation);
                }
            }
            "InstallSnapshot" => {
                let mut s = match RawSession::open(
                    &cx.protocols.snapshot,
                    "Follower",
                    pk.channel,
                    cx.auditor,
                ) {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let decoded = s
                    .on_recv(pk.from_role, pk.label, pk.payload_ty)
                    .ok()
                    .and_then(|()| decode_batch(&pk.body).ok())
                    .filter(|(h, image)| {
                        // The image is exactly the prefix up to the
                        // snapshot point, ending in its term.
                        image.len() as u64 == h.prev_log_index
                            && image.last().map_or(0, |e| e.term) == h.prev_log_term
                    });
                let Some((h, image)) = decoded else {
                    cx.stats.rejected_frames += 1;
                    s.fault(FaultReason::Violation);
                    return;
                };
                let mut accepted = false;
                let last = h.prev_log_index;
                if h.term >= self.term {
                    if h.term > self.term || self.role != Role::Follower {
                        self.become_follower(h.term);
                    }
                    self.reset_election(cx);
                    accepted = true;
                    if last > self.commit {
                        let have = last as usize <= self.log.len()
                            && (last == 0 || self.log[last as usize - 1].term == h.prev_log_term);
                        if !have {
                            // Discard the log and take the image.
                            let first_diff = self
                                .log
                                .iter()
                                .zip(&image)
                                .position(|(a, b)| a != b)
                                .unwrap_or(self.log.len().min(image.len()));
                            if first_diff < self.log.len() {
                                self.guard_compacted(cx, first_diff);
                            }
                            self.log = image;
                            cx.events.push(NodeEvent::LogChanged);
                        }
                        self.commit = last;
                        cx.events.push(NodeEvent::Committed);
                        if last > self.snap_index {
                            self.snap_index = last;
                            self.snap_term = h.prev_log_term;
                        }
                        cx.stats.snapshots_installed += 1;
                    }
                }
                s.on_action("persist")
                    .expect("Follower persists before replying");
                s.on_send(LEADER, "Installed", "AppendAck")
                    .expect("Follower projection answers Installed");
                let ack = AppendAck {
                    follower_id: self.id,
                    term: self.term,
                    match_index: if accepted { last } else { 0 },
                    accepted,
                };
                cx.out.push(Packet {
                    src: self.id,
                    dst: pk.src,
                    channel: pk.channel,
                    from_role: FOLLOWER,
                    label: "Installed",
                    payload_ty: "AppendAck",
                    body: ack.encode(),
                    request: false,
                });
                if let Err((_, s)) = s.close() {
                    s.fault(FaultReason::Violation);
                }
            }
            _ => cx.stats.rejected_frames += 1,
        }
    }

    fn handle_response(&mut self, cx: &mut Ctx<'_>, pk: Packet) {
        let Some(mut o) = self.outstanding.remove(&pk.channel) else {
            cx.stats.stale_responses += 1;
            return;
        };
        if o.session
            .on_recv(pk.from_role, pk.label, pk.payload_ty)
            .is_err()
        {
            cx.stats.rejected_frames += 1;
            o.session.fault(FaultReason::Violation);
            return;
        }
        match o.session.close() {
            Ok(()) => {}
            Err((_, s)) => {
                cx.stats.rejected_frames += 1;
                s.fault(FaultReason::Violation);
                return;
            }
        }
        match o.kind {
            RpcKind::Vote { term } => {
                let Ok(rterm) = decode_term(&pk.body) else {
                    cx.stats.rejected_frames += 1;
                    return;
                };
                if rterm > self.term {
                    self.become_follower(rterm);
                    self.reset_election(cx);
                    return;
                }
                if pk.label == "Grant" && term == self.term && self.role == Role::Candidate {
                    self.votes.insert(pk.src);
                    self.maybe_win(cx);
                }
            }
            RpcKind::Append {
                term,
                peer,
                sent_last,
            } => {
                let Ok(ack) = AppendAck::decode(&pk.body) else {
                    cx.stats.rejected_frames += 1;
                    return;
                };
                if ack.term > self.term {
                    self.become_follower(ack.term);
                    self.reset_election(cx);
                    return;
                }
                if self.role != Role::Leader || term != self.term || ack.follower_id != peer {
                    return;
                }
                let p = peer as usize;
                if ack.accepted && ack.match_index <= sent_last {
                    if ack.match_index > self.match_index[p] {
                        self.match_index[p] = ack.match_index;
                    }
                    self.next_index[p] = self.next_index[p].max(self.match_index[p] + 1);
                    self.advance_commit(cx);
                } else if !ack.accepted {
                    self.next_index[p] = self.next_index[p].saturating_sub(1).max(1);
                }
            }
            RpcKind::Snapshot { term, peer, last } => {
                let Ok(ack) = AppendAck::decode(&pk.body) else {
                    cx.stats.rejected_frames += 1;
                    return;
                };
                if ack.term > self.term {
                    self.become_follower(ack.term);
                    self.reset_election(cx);
                    return;
                }
                if self.role != Role::Leader || term != self.term || ack.follower_id != peer {
                    return;
                }
                if ack.accepted && ack.match_index == last {
                    let p = peer as usize;
                    self.match_index[p] = self.match_index[p].max(last);
                    self.next_index[p] = self.next_index[p].max(last + 1);
                    self.advance_commit(cx);
                }
            }
        }
    }

    fn step(&mut self, cx: &mut Ctx<'_>, seen: &mut BTreeSet<(u32, u64)>) {
        if !self.up {
            self.inbox.clear();
            return;
        }
        for pk in std::mem::take(&mut self.inbox) {
            if pk.request {
                if !seen.insert((pk.src, pk.channel)) {
                    cx.stats.duplicate_requests += 1;
                    continue;
                }
                self.handle_request(cx, pk);
            } else {
                self.handle_response(cx, pk);
            }
        }
        let expired: Vec<u64> = self
            .outstanding
            .iter()
            .filter(|(_, o)| o.deadline <= cx.now)
            .map(|(c, _)| *c)
            .collect();
        for c in expired {
            let o = self.outstanding.remove(&c).expect("expired");
            cx.stats.timeouts += 1;
            o.session.fault(FaultReason::Net(NetFail::Timeout));
        }
        if self.role != Role::Leader && cx.now >= self.election_deadline {
            if member(self.members(), self.id) {
                self.start_election(cx);
            } else {
                // A non-member does not campaign.
                self.reset_election(cx);
            }
        }
        if self.role == Role::Leader && cx.now >= self.heartbeat_deadline {
            self.send_appends(cx);
        }
        self.maybe_compact(cx);
    }

    fn next_wake(&self) -> u64 {
        let mut t = if self.role == Role::Leader {
            self.heartbeat_deadline
        } else {
            self.election_deadline
        };
        if let Some(d) = self.outstanding.values().map(|o| o.deadline).min() {
            t = t.min(d);
        }
        t
    }
}

// ---------------------------------------------------------------- chaos

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChaosKind {
    Crash(u32),
    Restart(u32),
}

/// A window during which some directed links drop every frame.
#[derive(Clone, Debug)]
pub struct PartitionWindow {
    pub start_ns: u64,
    pub end_ns: u64,
    /// `blocked[src * n + dst]`.
    pub blocked: Vec<bool>,
}

#[derive(Clone, Debug)]
pub struct ChaosPlan {
    events: Vec<(u64, ChaosKind)>,
    pub partitions: Vec<PartitionWindow>,
    pub skew_ppm: Vec<i64>,
}

impl ChaosPlan {
    fn generate(cfg: &RaftConfig, rng: &mut SplitMix) -> Self {
        let n = cfg.nodes;
        let mut events = Vec::new();
        if cfg.crashes {
            for _ in 0..rng.range(1, 3) {
                let node = rng.range(0, u64::from(n) - 1) as u32;
                let at = rng.range(0, cfg.duration_ns * 4 / 5);
                let down = rng.range(50_000_000, 600_000_000);
                events.push((at, ChaosKind::Crash(node)));
                events.push((at.saturating_add(down), ChaosKind::Restart(node)));
            }
        }
        events.sort_by_key(|(t, k)| (*t, matches!(k, ChaosKind::Restart(_))));
        let mut partitions = Vec::new();
        if cfg.partitions {
            for _ in 0..rng.range(1, 4) {
                let start = rng.range(0, cfg.duration_ns * 4 / 5);
                let len = rng.range(100_000_000, 800_000_000);
                let p = rng.range(15, 60) as u32 * 10;
                let blocked = (0..n * n)
                    .map(|i| i / n != i % n && rng.chance(p))
                    .collect();
                partitions.push(PartitionWindow {
                    start_ns: start,
                    end_ns: start.saturating_add(len),
                    blocked,
                });
            }
        }
        let skew = cfg.max_skew_ppm as i64;
        let skew_ppm = (0..n)
            .map(|_| rng.range(0, 2 * skew as u64) as i64 - skew)
            .collect();
        Self {
            events,
            partitions,
            skew_ppm,
        }
    }

    fn blocked(&self, n: u32, src: u32, dst: u32, now: u64) -> bool {
        self.partitions
            .iter()
            .any(|w| w.start_ns <= now && now < w.end_ns && w.blocked[(src * n + dst) as usize])
    }
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

fn fnv(h: &mut u64, v: u64) {
    for b in v.to_le_bytes() {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

/// Run one seeded cluster to `cfg.duration_ns`. `replay` re-runs a recorded
/// SimPulse journal and checks every scheduling event against it.
pub fn run_cluster(
    seed: u64,
    cfg: &RaftConfig,
    replay: Option<Journal>,
) -> Result<RaftRunReport, RaftRunError> {
    if !(1..=64).contains(&cfg.nodes) {
        return Err(RaftRunError::Config("nodes must be in 1..=64"));
    }
    if cfg.election_min_ns > cfg.election_max_ns || cfg.min_latency_ns > cfg.max_latency_ns {
        return Err(RaftRunError::Config("inverted range"));
    }
    if cfg.max_batch as u64 > u64::from(MAX_BATCH) {
        return Err(RaftRunError::Config("max_batch exceeds the wire limit"));
    }
    if cfg.snapshot_threshold > 0 && cfg.max_log as u64 > u64::from(MAX_BATCH) {
        return Err(RaftRunError::Config(
            "snapshots need max_log within the wire limit (the image is one batch)",
        ));
    }
    if cfg.initial_members == 0 || cfg.initial_members > cfg.nodes {
        return Err(RaftRunError::Config("initial_members must be in 1..=nodes"));
    }
    if (cfg.membership_per_mille > 0 || cfg.initial_members < cfg.nodes) && cfg.nodes > 63 {
        return Err(RaftRunError::Config(
            "membership change supports at most 63 nodes",
        ));
    }
    let protocols = Protocols {
        vote: parse_choreography(VOTE_CHOREOGRAPHY).expect("vote choreography parses"),
        append: parse_choreography(APPEND_CHOREOGRAPHY).expect("append choreography parses"),
        snapshot: parse_choreography(SNAPSHOT_CHOREOGRAPHY).expect("snapshot choreography parses"),
    };
    let n = cfg.nodes;
    let initial_members = if cfg.initial_members >= 64 {
        u64::MAX
    } else {
        (1u64 << cfg.initial_members) - 1
    };
    let mut rng = SplitMix(seed ^ 0x7261_6674_2d73_696d);
    let plan = ChaosPlan::generate(cfg, &mut rng);
    let auditor = LeakAuditor::new();
    let mut stats = RaftStats::default();
    let mut checker = Checker::default();
    let mut next_cmd = 0u64;
    let mut seen_requests: Vec<BTreeSet<(u32, u64)>> = vec![BTreeSet::new(); n as usize];
    let mut nodes: Vec<Node> = (0..n)
        .map(|id| Node {
            id,
            n,
            up: true,
            incarnation: 0,
            term: 0,
            voted_for: None,
            log: Vec::new(),
            snap_index: 0,
            snap_term: 0,
            initial_members,
            role: Role::Follower,
            commit: 0,
            votes: BTreeSet::new(),
            next_index: Vec::new(),
            match_index: Vec::new(),
            election_deadline: 0,
            heartbeat_deadline: 0,
            skew_ppm: plan.skew_ppm[id as usize],
            outstanding: BTreeMap::new(),
            next_channel: 0,
            inbox: Vec::new(),
        })
        .collect();
    // Initial election timers.
    for node in nodes.iter_mut() {
        let local = rng.range(cfg.election_min_ns, cfg.election_max_ns);
        node.election_deadline = node.local_to_virtual(local);
    }
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
    let mut in_flight: BTreeMap<(u64, u64), Packet> = BTreeMap::new();
    let mut packet_id = 0u64;
    let mut node_wait: Vec<Option<CompletionId>> = vec![None; n as usize];
    let mut node_done = vec![false; n as usize];
    let mut net_wait: Option<(CompletionId, Option<u64>)> = None;
    let mut chaos_next = 0usize;
    let end = cfg.duration_ns;
    for _ in 0..cfg.max_steps {
        let Some(token) = sim.next_task().map_err(RaftRunError::Sim)? else {
            break;
        };
        let now = sim.now_ns();
        let mut out = Vec::new();
        let mut events = Vec::new();
        match *token.payload() {
            Task::Network => {
                net_wait = None;
                let due: Vec<(u64, u64)> = in_flight
                    .range(..=(now, u64::MAX))
                    .map(|(k, _)| *k)
                    .collect();
                let mut woken = BTreeSet::new();
                for k in due {
                    let pk = in_flight.remove(&k).expect("due");
                    let dst = pk.dst as usize;
                    if nodes[dst].up && !node_done[dst] {
                        nodes[dst].inbox.push(pk);
                        woken.insert(dst);
                    }
                }
                for d in woken {
                    if let Some(c) = node_wait[d].take() {
                        sim.complete_io(c, IoResult::Success { bytes: 0 })
                            .map_err(RaftRunError::Sim)?;
                    }
                }
                if node_done.iter().all(|d| *d) {
                    sim.finish(token).map_err(tr)?;
                } else if let Some(&(t, _)) = in_flight.keys().next() {
                    let c = sim
                        .schedule_io(token, t - now, IoResult::Success { bytes: 0 })
                        .map_err(tr)?;
                    net_wait = Some((c, Some(t)));
                } else {
                    let c = sim.park_io(token).map_err(tr)?;
                    net_wait = Some((c, None));
                }
            }
            Task::Chaos => {
                while chaos_next < plan.events.len() && plan.events[chaos_next].0 <= now {
                    let (_, kind) = plan.events[chaos_next];
                    chaos_next += 1;
                    match kind {
                        ChaosKind::Crash(i) if nodes[i as usize].up => {
                            stats.crashes += 1;
                            nodes[i as usize].crash();
                        }
                        ChaosKind::Restart(i) if !nodes[i as usize].up => {
                            let mut cx = Ctx {
                                now,
                                cfg,
                                protocols: &protocols,
                                auditor: &auditor,
                                rng: &mut rng,
                                out: &mut out,
                                stats: &mut stats,
                                next_cmd: &mut next_cmd,
                                events: &mut events,
                            };
                            nodes[i as usize].restart(&mut cx);
                            if let Some(c) = node_wait[i as usize].take() {
                                sim.complete_io(c, IoResult::Success { bytes: 0 })
                                    .map_err(RaftRunError::Sim)?;
                            }
                        }
                        _ => {}
                    }
                }
                match plan.events.get(chaos_next) {
                    Some(&(t, _)) if t < end => {
                        sim.sleep(token, t - now).map_err(tr)?;
                    }
                    _ => {
                        sim.finish(token).map_err(tr)?;
                    }
                }
            }
            Task::Node(i) => {
                let iu = i as usize;
                if now >= end {
                    node_done[iu] = true;
                    sim.finish(token).map_err(tr)?;
                } else {
                    {
                        let mut cx = Ctx {
                            now,
                            cfg,
                            protocols: &protocols,
                            auditor: &auditor,
                            rng: &mut rng,
                            out: &mut out,
                            stats: &mut stats,
                            next_cmd: &mut next_cmd,
                            events: &mut events,
                        };
                        nodes[iu].step(&mut cx, &mut seen_requests[iu]);
                    }
                    if nodes[iu].up {
                        let wake = nodes[iu].next_wake().clamp(now, end);
                        let c = sim
                            .schedule_io(token, wake - now, IoResult::Success { bytes: 0 })
                            .map_err(tr)?;
                        node_wait[iu] = Some(c);
                    } else {
                        // Down: wait for restart (or the end of the run).
                        let c = sim
                            .schedule_io(token, end - now, IoResult::Success { bytes: 0 })
                            .map_err(tr)?;
                        node_wait[iu] = Some(c);
                    }
                    for ev in events.drain(..) {
                        match ev {
                            NodeEvent::Leader { term } => checker.leader_elected(now, term, i),
                            NodeEvent::LogChanged => {
                                let logs: Vec<&[Entry]> =
                                    nodes.iter().map(|n| n.log.as_slice()).collect();
                                checker.logs_changed(now, iu, &logs);
                            }
                            NodeEvent::Committed => {
                                checker.committed(now, i, &nodes[iu].log, nodes[iu].commit)
                            }
                            NodeEvent::CompactedChanged { index } => {
                                checker.compacted_changed(now, i, index)
                            }
                        }
                    }
                }
            }
        }
        // Route packets produced this step.
        let mut earliest: Option<u64> = None;
        for pk in out {
            stats.sent += 1;
            if plan.blocked(n, pk.src, pk.dst, now) {
                stats.partitioned += 1;
                continue;
            }
            if rng.chance(cfg.drop_per_mille) {
                stats.dropped += 1;
                continue;
            }
            let copies = if rng.chance(cfg.duplicate_per_mille) {
                stats.duplicated += 1;
                2
            } else {
                1
            };
            for _ in 0..copies {
                let at = now.saturating_add(rng.range(cfg.min_latency_ns, cfg.max_latency_ns));
                earliest = Some(earliest.map_or(at, |e: u64| e.min(at)));
                in_flight.insert((at, packet_id), pk.clone());
                packet_id += 1;
            }
        }
        if let Some((c, deadline)) = net_wait {
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
        if checker.violation.is_some() {
            break;
        }
    }
    if checker.violation.is_none() && !sim.is_complete() {
        return Err(RaftRunError::StepBudget);
    }
    // No-op for a recording simulator; for a replay, the whole journal must
    // have been consumed.
    sim.finish_replay().map_err(RaftRunError::Sim)?;
    for node in nodes.iter_mut() {
        node.shutdown();
    }
    let mut digest = 0xcbf2_9ce4_8422_2325u64;
    for node in &nodes {
        fnv(&mut digest, node.term);
        fnv(&mut digest, node.voted_for.map_or(u64::MAX, u64::from));
        fnv(&mut digest, node.commit);
        fnv(&mut digest, node.snap_index);
        for e in &node.log {
            fnv(&mut digest, e.term);
            fnv(&mut digest, e.cmd);
        }
    }
    for v in [
        stats.sent,
        stats.dropped,
        stats.duplicated,
        stats.elections,
        stats.timeouts,
        stats.stale_responses,
    ] {
        fnv(&mut digest, v);
    }
    let audit = auditor.counts();
    Ok(RaftRunReport {
        seed,
        journal: sim.journal().clone(),
        digest,
        violation: checker.violation.clone(),
        stats,
        open_sessions: auditor.open(),
        audit,
        leaders: checker.leaders().clone(),
        committed: checker.committed_len(),
        checks: checker.checks,
        end_ns: sim.now_ns(),
        members: nodes.iter().map(Node::members).collect(),
    })
}
