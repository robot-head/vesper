//! A Raft-style replicated log whose every RPC is a typed [`Session`] over
//! Warp QUIC streams on a lossy software link (wave six lane 17).
//!
//! Each RequestVote and AppendEntries exchange binds the two projections of
//! [`VOTE_CHOREOGRAPHY`] or [`APPEND_CHOREOGRAPHY`] as `Session<P>` endpoints
//! on a [`WarpNet`]: the candidate or leader on its host, the voter or
//! follower on its host. The choice of reply is an n-ary
//! [`SelectN`](crate::session::SelectN)/[`OfferN`](crate::session::OfferN),
//! the follower's `persist` is an [`Act`](crate::session::Act), and every
//! frame crosses the network as QUIC stream data that the stack retransmits
//! when the link loses it.
//!
//! The run is a single-threaded script, so it is deterministic for a seed:
//! terms follow one another, the candidate of term `t` is node
//! `(t - 1) mod n`, it wins only with a majority of grants under Raft's vote
//! rule (one vote per term, the candidate's log at least as up to date),
//! and a winner replicates for a number of rounds, appending proposals and
//! backing off `next_index` on rejection. Seeded partitions block some
//! directed links for a whole term; an RPC across a blocked link times out
//! on both ends and leaves through the explicit fault exit. A final healing
//! phase (no partitions) runs until every log equals the leader's and
//! everything is committed. The [`Checker`] verifies election safety, log
//! matching and committed-prefix stability after every change.
//!
//! What this is: the Raft election restriction and the AppendEntries
//! consistency rule executed over the Warp stack. What it is not: a
//! concurrent Raft with timers (see [`crate::raft`] for the SimPulse
//! cluster with crashes and clock skew), a real network, or durable
//! storage.

use crate::chor::Choreography;
use crate::parse::parse_choreography;
use crate::raft::{
    APPEND_CHOREOGRAPHY, AppendAck, Checker, Entry, MAX_BATCH, RaftAppendWireHeader,
    VOTE_CHOREOGRAPHY, Violation, VoteReq, decode_batch, encode_batch,
};
use crate::session::{
    Act, ActionName, AuditCounts, Branch, Case, ChorusError, End, LeakAuditor, Message, OfferN,
    Recv, SelectN, Send, Session,
};
use crate::simnet::SplitMix;
use crate::warpnet::{WarpNet, WarpNetConfig, WarpNetError, WarpNetStats};
use crate::{branches, role, u64_message};
use std::collections::BTreeMap;

role!(RCandidate = "Candidate");
role!(RVoter = "Voter");
role!(RLeader = "Leader");
role!(RFollower = "Follower");
u64_message!(Grant = "Grant", "VoteResp");
u64_message!(Deny = "Deny", "VoteResp");

/// `RequestVote(VoteReq)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestVote(pub VoteReq);
impl Message for RequestVote {
    const LABEL: &'static str = "RequestVote";
    const PAYLOAD: &'static str = "VoteReq";
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.encode());
    }
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        VoteReq::decode(bytes)
            .map(RequestVote)
            .map_err(|e| format!("{e:?}"))
    }
}

/// `AppendEntries(AppendBatch)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppendEntries {
    pub header: RaftAppendWireHeader,
    pub entries: Vec<Entry>,
}
impl Message for AppendEntries {
    const LABEL: &'static str = "AppendEntries";
    const PAYLOAD: &'static str = "AppendBatch";
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&encode_batch(&self.header, &self.entries));
    }
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        decode_batch(bytes)
            .map(|(header, entries)| AppendEntries { header, entries })
            .map_err(|e| format!("{e:?}"))
    }
}

macro_rules! ack_message {
    ($t:ident = $l:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $t(pub AppendAck);
        impl Message for $t {
            const LABEL: &'static str = $l;
            const PAYLOAD: &'static str = "AppendAck";
            fn encode(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.0.encode());
            }
            fn decode(bytes: &[u8]) -> Result<Self, String> {
                AppendAck::decode(bytes)
                    .map($t)
                    .map_err(|e| format!("{e:?}"))
            }
        }
    };
}
ack_message!(Accept = "Accept");
ack_message!(Reject = "Reject");

/// The follower's local `persist` action.
pub struct Persist;
impl ActionName for Persist {
    const NAME: &'static str = "persist";
}

/// Projections of [`VOTE_CHOREOGRAPHY`] and [`APPEND_CHOREOGRAPHY`] as
/// protocol types. `bind` checks each against the projection.
pub type CandidateProto =
    Send<RVoter, RequestVote, OfferN<RVoter, branches![Branch<Grant, End>, Branch<Deny, End>]>>;
pub type VoterProto = Recv<
    RCandidate,
    RequestVote,
    SelectN<RCandidate, branches![Branch<Grant, End>, Branch<Deny, End>]>,
>;
pub type LeaderProto = Send<
    RFollower,
    AppendEntries,
    OfferN<RFollower, branches![Branch<Accept, End>, Branch<Reject, End>]>,
>;
pub type FollowerProto = Recv<
    RLeader,
    AppendEntries,
    Act<Persist, SelectN<RLeader, branches![Branch<Accept, End>, Branch<Reject, End>]>>,
>;

#[derive(Clone, Copy, Debug)]
pub struct ReplogConfig {
    /// The network (its `nodes` is the cluster size, 3..=7).
    pub net: WarpNetConfig,
    /// Terms before the healing phase.
    pub terms: u32,
    /// Replication rounds per won term.
    pub rounds_per_term: u32,
    /// Entries the leader appends per round.
    pub proposals_per_round: u32,
    /// Chance (per mille) that a directed link is blocked for a term.
    pub partition_per_mille: u32,
    /// Largest AppendEntries batch.
    pub max_batch: u32,
    /// Healing-phase round budget.
    pub max_heal_rounds: u32,
}

impl Default for ReplogConfig {
    fn default() -> Self {
        Self {
            net: WarpNetConfig {
                nodes: 5,
                loss_ppm: 50_000,
                reorder_ppm: 50_000,
                duplicate_ppm: 10_000,
                max_wait_steps: 200,
                ..WarpNetConfig::default()
            },
            terms: 6,
            rounds_per_term: 4,
            proposals_per_round: 2,
            partition_per_mille: 150,
            max_batch: 32,
            max_heal_rounds: 200,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplogStats {
    pub elections: u64,
    pub elections_won: u64,
    pub votes_granted: u64,
    pub votes_denied: u64,
    pub appends: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub rpc_faults: u64,
    pub proposals: u64,
    pub partitioned_links: u64,
}

#[derive(Debug)]
pub struct ReplogReport {
    pub seed: u64,
    pub leaders: BTreeMap<u64, u32>,
    pub logs: Vec<Vec<Entry>>,
    pub commit: Vec<u64>,
    pub committed: usize,
    pub violation: Option<Violation>,
    pub checks: u64,
    pub stats: ReplogStats,
    pub net: WarpNetStats,
    pub audit: AuditCounts,
    pub open_sessions: u64,
    /// Session stream directions still open after the final settle.
    pub open_streams: usize,
    /// Whether the network drained after the run.
    pub settled: bool,
    pub open_stream_details: Vec<String>,
    /// FNV-1a digest of the final logs, commits and counters.
    pub digest: u64,
}

#[derive(Debug)]
pub enum ReplogError {
    Config(&'static str),
    Net(WarpNetError),
    /// A typed-session error other than a transport failure (a bug).
    Session(ChorusError),
    /// The healing phase did not converge.
    NoConvergence,
}

struct Node {
    term: u64,
    voted_for: Option<u32>,
    log: Vec<Entry>,
    commit: u64,
}

impl Node {
    fn last(&self) -> (u64, u64) {
        (self.log.len() as u64, self.log.last().map_or(0, |e| e.term))
    }
}

struct Run {
    cfg: ReplogConfig,
    n: u32,
    net: WarpNet,
    vote: Choreography,
    append: Choreography,
    auditor: LeakAuditor,
    nodes: Vec<Node>,
    checker: Checker,
    stats: ReplogStats,
    next_channel: u64,
    next_cmd: u64,
    rng: SplitMix,
}

/// A transport failure ends one RPC; anything else is a bug in the run.
fn transport_failure(e: &ChorusError) -> bool {
    matches!(e, ChorusError::Net(_))
}

impl Run {
    fn channel(&mut self) -> u64 {
        self.next_channel += 1;
        self.next_channel
    }

    fn check_logs(&mut self, a: u32) {
        let logs: Vec<&[Entry]> = self.nodes.iter().map(|n| n.log.as_slice()).collect();
        self.checker
            .logs_changed(self.net.now_ns(), a as usize, &logs);
    }

    fn check_commit(&mut self, a: u32) {
        let n = &self.nodes[a as usize];
        let (log, commit) = (n.log.clone(), n.commit);
        self.checker.committed(self.net.now_ns(), a, &log, commit);
    }

    fn fault(&mut self, e: ChorusError) -> Result<(), ReplogError> {
        if transport_failure(&e) {
            self.stats.rpc_faults += 1;
            Ok(())
        } else {
            Err(ReplogError::Session(e))
        }
    }

    /// One RequestVote exchange. Returns whether the vote was granted, or
    /// the higher term the voter answered with.
    fn request_vote(&mut self, c: u32, v: u32) -> Result<(bool, u64), ReplogError> {
        let ch = self.channel();
        let placement = [c, v];
        let cs = Session::<CandidateProto>::bind(
            &self.vote,
            "Candidate",
            ch,
            self.net
                .transport(c, &placement)
                .map_err(ReplogError::Net)?,
            &self.auditor,
        )
        .map_err(ReplogError::Session)?;
        let vs = Session::<VoterProto>::bind(
            &self.vote,
            "Voter",
            ch,
            self.net
                .transport(v, &placement)
                .map_err(ReplogError::Net)?,
            &self.auditor,
        )
        .map_err(ReplogError::Session)?;
        let (li, lt) = self.nodes[c as usize].last();
        let req = VoteReq {
            term: self.nodes[c as usize].term,
            candidate: c,
            last_log_index: li,
            last_log_term: lt,
        };
        let cs = match cs.send(&RequestVote(req)) {
            Ok(s) => s,
            // Only a dead network fails a first send; the unused voter end
            // is then counted as a leak.
            Err(f) => return Err(ReplogError::Session(f.into_error())),
        };
        // Voter.
        match vs.recv() {
            Err(f) => self.fault(f.into_error())?,
            Ok((RequestVote(r), vs)) => {
                let node = &mut self.nodes[v as usize];
                if r.term > node.term {
                    node.term = r.term;
                    node.voted_for = None;
                }
                let (i, t) = node.last();
                let up_to_date = (r.last_log_term, r.last_log_index) >= (t, i);
                let grant = r.term == node.term
                    && node.voted_for.is_none_or(|x| x == r.candidate)
                    && up_to_date;
                let term = node.term;
                let sent = if grant {
                    node.voted_for = Some(r.candidate);
                    vs.select(&Grant(term)).map(Session::close)
                } else {
                    vs.select(&Deny(term)).map(Session::close)
                };
                if let Err(f) = sent {
                    self.fault(f.into_error())?;
                }
            }
        }
        // Candidate.
        let c = match cs.offer() {
            Err(f) => {
                self.fault(f.into_error())?;
                return Ok((false, 0));
            }
            Ok(c) => c,
        };
        match c.split() {
            Err(f) => Err(ReplogError::Session(f.into_error())),
            Ok(Case::This(Grant(t), s)) => {
                s.close();
                self.stats.votes_granted += 1;
                Ok((true, t))
            }
            Ok(Case::Other(c)) => match c.split() {
                Err(f) => Err(ReplogError::Session(f.into_error())),
                Ok(Case::This(Deny(t), s)) => {
                    s.close();
                    self.stats.votes_denied += 1;
                    Ok((false, t))
                }
                Ok(Case::Other(c)) => Err(ReplogError::Session(c.exhausted().into_error())),
            },
        }
    }

    fn elect(&mut self, c: u32, term: u64) -> Result<bool, ReplogError> {
        self.stats.elections += 1;
        {
            let node = &mut self.nodes[c as usize];
            if node.term >= term {
                return Ok(false);
            }
            node.term = term;
            node.voted_for = Some(c);
        }
        let mut votes = 1;
        for v in 0..self.n {
            if v == c {
                continue;
            }
            let (granted, t) = self.request_vote(c, v)?;
            if t > term {
                let node = &mut self.nodes[c as usize];
                node.term = t;
                node.voted_for = None;
                return Ok(false);
            }
            if granted {
                votes += 1;
            }
        }
        if votes > self.n / 2 {
            self.stats.elections_won += 1;
            self.checker.leader_elected(self.net.now_ns(), term, c);
            return Ok(true);
        }
        Ok(false)
    }

    /// One AppendEntries exchange. Returns `false` if the leader must step
    /// down (a higher term answered).
    fn append(
        &mut self,
        l: u32,
        f: u32,
        next: &mut [u64],
        matched: &mut [u64],
    ) -> Result<bool, ReplogError> {
        let ch = self.channel();
        let placement = [l, f];
        let ls = Session::<LeaderProto>::bind(
            &self.append,
            "Leader",
            ch,
            self.net
                .transport(l, &placement)
                .map_err(ReplogError::Net)?,
            &self.auditor,
        )
        .map_err(ReplogError::Session)?;
        let fs = Session::<FollowerProto>::bind(
            &self.append,
            "Follower",
            ch,
            self.net
                .transport(f, &placement)
                .map_err(ReplogError::Net)?,
            &self.auditor,
        )
        .map_err(ReplogError::Session)?;
        let leader = &self.nodes[l as usize];
        let term = leader.term;
        let ni = next[f as usize].clamp(1, leader.log.len() as u64 + 1);
        let prev = ni - 1;
        let prev_term = if prev == 0 {
            0
        } else {
            leader.log[prev as usize - 1].term
        };
        let entries: Vec<Entry> = leader.log[prev as usize..]
            .iter()
            .take(self.cfg.max_batch as usize)
            .copied()
            .collect();
        let sent_last = prev + entries.len() as u64;
        let msg = AppendEntries {
            header: RaftAppendWireHeader {
                term,
                leader_id: l,
                follower_slot: f,
                prev_log_index: prev,
                prev_log_term: prev_term,
                leader_commit: leader.commit,
            },
            entries,
        };
        self.stats.appends += 1;
        let ls = match ls.send(&msg) {
            Ok(s) => s,
            Err(e) => return Err(ReplogError::Session(e.into_error())),
        };
        // Follower.
        match fs.recv() {
            Err(e) => self.fault(e.into_error())?,
            Ok((AppendEntries { header: h, entries }, fs)) => {
                let node = &mut self.nodes[f as usize];
                let mut accepted = false;
                let mut match_index = 0;
                let mut changed = false;
                let mut committed = false;
                if h.term >= node.term {
                    if h.term > node.term {
                        node.term = h.term;
                        node.voted_for = None;
                    }
                    let p = h.prev_log_index;
                    let ok = p == 0
                        || (p <= node.log.len() as u64
                            && node.log[p as usize - 1].term == h.prev_log_term);
                    if ok {
                        for (k, e) in entries.iter().enumerate() {
                            let idx = p as usize + k;
                            if idx < node.log.len() {
                                if node.log[idx].term != e.term {
                                    node.log.truncate(idx);
                                    node.log.push(*e);
                                    changed = true;
                                }
                            } else {
                                node.log.push(*e);
                                changed = true;
                            }
                        }
                        match_index = p + entries.len() as u64;
                        let c = h.leader_commit.min(match_index);
                        if c > node.commit {
                            node.commit = c;
                            committed = true;
                        }
                        accepted = true;
                    }
                }
                let ack = AppendAck {
                    follower_id: f,
                    term: node.term,
                    match_index,
                    accepted,
                };
                let fs = fs.act();
                let r = if accepted {
                    fs.select(&Accept(ack)).map(Session::close)
                } else {
                    fs.select(&Reject(ack)).map(Session::close)
                };
                if changed {
                    self.check_logs(f);
                }
                if committed {
                    self.check_commit(f);
                }
                if let Err(e) = r {
                    self.fault(e.into_error())?;
                }
            }
        }
        // Leader.
        let c = match ls.offer() {
            Err(e) => {
                self.fault(e.into_error())?;
                return Ok(true);
            }
            Ok(c) => c,
        };
        let (ack, ok) = match c.split() {
            Err(e) => return Err(ReplogError::Session(e.into_error())),
            Ok(Case::This(Accept(a), s)) => {
                s.close();
                (a, true)
            }
            Ok(Case::Other(c)) => match c.split() {
                Err(e) => return Err(ReplogError::Session(e.into_error())),
                Ok(Case::This(Reject(a), s)) => {
                    s.close();
                    (a, false)
                }
                Ok(Case::Other(c)) => {
                    return Err(ReplogError::Session(c.exhausted().into_error()));
                }
            },
        };
        if ack.term > term {
            let node = &mut self.nodes[l as usize];
            node.term = ack.term;
            node.voted_for = None;
            return Ok(false);
        }
        let fi = f as usize;
        if ok {
            self.stats.accepted += 1;
            if ack.match_index <= sent_last {
                matched[fi] = matched[fi].max(ack.match_index);
                next[fi] = next[fi].max(matched[fi] + 1);
            }
        } else {
            self.stats.rejected += 1;
            next[fi] = next[fi].saturating_sub(1).max(1);
        }
        Ok(true)
    }

    fn advance_commit(&mut self, l: u32, matched: &[u64]) {
        let node = &self.nodes[l as usize];
        for idx in (node.commit + 1..=node.log.len() as u64).rev() {
            if node.log[idx as usize - 1].term != node.term {
                continue;
            }
            let count = 1
                + (0..self.n)
                    .filter(|p| *p != l && matched[*p as usize] >= idx)
                    .count() as u32;
            if count > self.n / 2 {
                self.nodes[l as usize].commit = idx;
                self.check_commit(l);
                return;
            }
        }
    }

    /// Lead one term for `rounds` rounds (or until stepping down). With
    /// `until_converged`, stop early once every log equals the leader's and
    /// all of it is committed everywhere.
    fn lead(&mut self, l: u32, rounds: u32, until_converged: bool) -> Result<bool, ReplogError> {
        let len = self.nodes[l as usize].log.len() as u64;
        let mut next = vec![len + 1; self.n as usize];
        let mut matched = vec![0; self.n as usize];
        for round in 0..rounds {
            if !until_converged || round == 0 {
                let term = self.nodes[l as usize].term;
                for _ in 0..self.cfg.proposals_per_round {
                    self.next_cmd += 1;
                    let cmd = self.next_cmd;
                    self.nodes[l as usize].log.push(Entry { term, cmd });
                    self.stats.proposals += 1;
                }
                self.check_logs(l);
            }
            for f in 0..self.n {
                if f == l {
                    continue;
                }
                if !self.append(l, f, &mut next, &mut matched)? {
                    return Ok(false);
                }
                self.advance_commit(l, &matched);
                if self.checker.violation.is_some() {
                    return Ok(false);
                }
            }
            if until_converged && self.converged(l) {
                return Ok(true);
            }
        }
        Ok(!until_converged)
    }

    fn converged(&self, l: u32) -> bool {
        let leader = &self.nodes[l as usize];
        self.nodes
            .iter()
            .all(|n| n.log == leader.log && n.commit == leader.log.len() as u64)
    }

    fn partition(&mut self, on: bool) {
        for s in 0..self.n {
            for d in 0..self.n {
                if s == d {
                    continue;
                }
                let block = on && self.rng.chance(self.cfg.partition_per_mille);
                if block {
                    self.stats.partitioned_links += 1;
                }
                self.net.set_blocked(s, d, block);
            }
        }
    }
}

fn fnv(h: &mut u64, v: u64) {
    for b in v.to_le_bytes() {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

/// Run one seeded replicated-log scenario (see the module docs).
pub fn run_replicated_log(cfg: &ReplogConfig) -> Result<ReplogReport, ReplogError> {
    let n = cfg.net.nodes;
    if !(3..=7).contains(&n) {
        return Err(ReplogError::Config("nodes must be in 3..=7"));
    }
    if cfg.max_batch == 0 || cfg.max_batch > MAX_BATCH {
        return Err(ReplogError::Config("max_batch must be in 1..=MAX_BATCH"));
    }
    let net = WarpNet::new(cfg.net).map_err(ReplogError::Net)?;
    let mut run = Run {
        cfg: *cfg,
        n,
        net,
        vote: parse_choreography(VOTE_CHOREOGRAPHY).expect("vote choreography parses"),
        append: parse_choreography(APPEND_CHOREOGRAPHY).expect("append choreography parses"),
        auditor: LeakAuditor::new(),
        nodes: (0..n)
            .map(|_| Node {
                term: 0,
                voted_for: None,
                log: Vec::new(),
                commit: 0,
            })
            .collect(),
        checker: Checker::default(),
        stats: ReplogStats::default(),
        next_channel: 0,
        next_cmd: 0,
        rng: SplitMix(cfg.net.seed ^ 0x7265_706c_6f67),
    };
    let mut term = 0u64;
    for t in 1..=u64::from(cfg.terms) {
        term = t;
        run.partition(true);
        let c = ((t - 1) % u64::from(n)) as u32;
        if run.elect(c, t)? {
            run.lead(c, cfg.rounds_per_term, false)?;
        }
        if run.checker.violation.is_some() {
            break;
        }
    }
    // Healing: no partitions; candidates in turn until one leads every log
    // to convergence.
    run.partition(false);
    let mut converged = run.checker.violation.is_some();
    let mut attempts = 0;
    while !converged && attempts < 4 * n {
        attempts += 1;
        term += 1;
        let c = ((term - 1) % u64::from(n)) as u32;
        if run.elect(c, term)? {
            converged = run.lead(c, cfg.max_heal_rounds, true)?;
        }
    }
    if !converged {
        return Err(ReplogError::NoConvergence);
    }
    // Streams cut by a long partition finish only after QUIC's backed-off
    // probe timeouts fire, which can take thousands of steps.
    let settled = run.net.settle(200_000);
    let mut digest = 0xcbf2_9ce4_8422_2325u64;
    for node in &run.nodes {
        fnv(&mut digest, node.term);
        fnv(&mut digest, node.commit);
        for e in &node.log {
            fnv(&mut digest, e.term);
            fnv(&mut digest, e.cmd);
        }
    }
    let net_stats = run.net.stats();
    for v in [
        run.stats.rpc_faults,
        run.stats.rejected,
        net_stats.datagrams,
        net_stats.dropped,
    ] {
        fnv(&mut digest, v);
    }
    Ok(ReplogReport {
        seed: cfg.net.seed,
        leaders: run.checker.leaders().clone(),
        logs: run.nodes.iter().map(|n| n.log.clone()).collect(),
        commit: run.nodes.iter().map(|n| n.commit).collect(),
        committed: run.checker.committed_len(),
        violation: run.checker.violation.clone().map(|(_, v)| v),
        checks: run.checker.checks,
        stats: run.stats,
        net: net_stats,
        audit: run.auditor.counts(),
        open_sessions: run.auditor.open(),
        open_streams: run.net.open_streams(),
        settled,
        open_stream_details: run.net.describe_open_streams(),
        digest,
    })
}
