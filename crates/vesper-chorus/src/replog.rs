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
use crate::port::{self, binding};
use crate::raft::{
    APPEND_CHOREOGRAPHY, AppendAck, Entry, RaftAppendWireHeader, VOTE_CHOREOGRAPHY, Violation,
    VoteReq, decode_batch, encode_batch,
};
use crate::session::{
    Act, ActionName, AuditCounts, Branch, Case, ChorusError, End, LeakAuditor, Message, OfferN,
    Recv, SelectN, Send, Session,
};
use crate::warpnet::{WarpNet, WarpNetConfig, WarpNetError, WarpNetStats, WarpTls};
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

struct Run {
    n: u32,
    net: WarpNet,
    vote: Choreography,
    append: Choreography,
    auditor: LeakAuditor,
    core: binding::vesper__chorus__replog__ReplogCore,
}

/// A transport failure ends one RPC; anything else is a bug in the run.
fn transport_failure(e: &ChorusError) -> bool {
    matches!(e, ChorusError::Net(_))
}

impl Run {
    fn fault(&mut self, e: ChorusError) -> Result<(), ReplogError> {
        if transport_failure(&e) {
            binding::p2_replog_on_vote_fault(&mut self.core);
            Ok(())
        } else {
            Err(ReplogError::Session(e))
        }
    }

    /// One RequestVote exchange. Returns `(granted, term_answered, stepped_down)`.
    fn request_vote(
        &mut self,
        c: u32,
        v: u32,
        elect_term: u64,
    ) -> Result<(bool, bool), ReplogError> {
        let prep = binding::p2_replog_prep_vote(&mut self.core, u64::from(c));
        let ch = prep.channel;
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
        let req = VoteReq {
            term: prep.req.term,
            candidate: prep.req.candidate.get(),
            last_log_index: prep.req.last_log_index,
            last_log_term: prep.req.last_log_term,
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
                let dec = binding::p2_replog_on_vote_req(
                    &mut self.core,
                    u64::from(v),
                    &binding::repr::vesper__chorus__wire__WireVoteReq {
                        term: r.term,
                        candidate: vesper_port_abi::Word::new(r.candidate),
                        last_log_index: r.last_log_index,
                        last_log_term: r.last_log_term,
                    },
                );
                let sent = if dec.grant.get() {
                    vs.select(&Grant(dec.term)).map(Session::close)
                } else {
                    vs.select(&Deny(dec.term)).map(Session::close)
                };
                if let Err(f) = sent {
                    self.fault(f.into_error())?;
                }
            }
        }
        // Candidate.
        let ch_offer = match cs.offer() {
            Err(f) => {
                self.fault(f.into_error())?;
                return Ok((false, false));
            }
            Ok(ch_offer) => ch_offer,
        };
        match ch_offer.split() {
            Err(f) => Err(ReplogError::Session(f.into_error())),
            Ok(Case::This(Grant(t), s)) => {
                s.close();
                let stepped_down = binding::p2_replog_on_vote_resp(
                    &mut self.core,
                    u64::from(c),
                    elect_term,
                    true,
                    t,
                );
                Ok((true, stepped_down))
            }
            Ok(Case::Other(ch_offer)) => match ch_offer.split() {
                Err(f) => Err(ReplogError::Session(f.into_error())),
                Ok(Case::This(Deny(t), s)) => {
                    s.close();
                    let stepped_down = binding::p2_replog_on_vote_resp(
                        &mut self.core,
                        u64::from(c),
                        elect_term,
                        false,
                        t,
                    );
                    Ok((false, stepped_down))
                }
                Ok(Case::Other(ch_offer)) => {
                    Err(ReplogError::Session(ch_offer.exhausted().into_error()))
                }
            },
        }
    }

    fn elect(&mut self, c: u32, term: u64) -> Result<bool, ReplogError> {
        if !binding::p2_replog_start_elect(&mut self.core, u64::from(c), term) {
            return Ok(false);
        }
        let mut votes = 1u64;
        for v in 0..self.n {
            if v == c {
                continue;
            }
            let (granted, stepped_down) = self.request_vote(c, v, term)?;
            if stepped_down {
                return Ok(false);
            }
            if granted {
                votes += 1;
            }
        }
        Ok(binding::p2_replog_finish_elect(
            &mut self.core,
            self.net.now_ns(),
            u64::from(c),
            term,
            votes,
        ))
    }

    /// One AppendEntries exchange. Returns `false` if the leader must step
    /// down or the invariant checker flagged a violation.
    fn append(&mut self, l: u32, f: u32) -> Result<bool, ReplogError> {
        let prep = binding::p2_replog_prep_append(&mut self.core, u64::from(l), u64::from(f));
        let ch = prep.channel;
        let term = prep.header.term;
        let sent_last = prep.sent_last;
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
        let off = prep.entry_off as usize;
        let len = prep.entry_len as usize;
        let pool = self.core.as_ref().log_pool();
        let entries: Vec<Entry> = (off..off + len)
            .map(|idx| {
                let e = pool.get(idx).expect("log entry");
                Entry {
                    term: e.term,
                    cmd: e.cmd,
                }
            })
            .collect();
        let msg = AppendEntries {
            header: RaftAppendWireHeader {
                term,
                leader_id: prep.header.leader_id.get(),
                follower_slot: prep.header.follower_slot.get(),
                prev_log_index: prep.header.prev_log_index,
                prev_log_term: prep.header.prev_log_term,
                leader_commit: prep.header.leader_commit,
            },
            entries,
        };
        let ls = match ls.send(&msg) {
            Ok(s) => s,
            Err(e) => return Err(ReplogError::Session(e.into_error())),
        };
        // Follower.
        match fs.recv() {
            Err(e) => self.fault(e.into_error())?,
            Ok((AppendEntries { header: h, entries }, fs)) => {
                let raw_entries: Vec<binding::repr::vesper__chorus__wire__RaftEntry> = entries
                    .iter()
                    .map(|e| binding::repr::vesper__chorus__wire__RaftEntry {
                        term: e.term,
                        cmd: e.cmd,
                    })
                    .collect();
                let req_out = binding::p2_replog_on_append_req(
                    &mut self.core,
                    u64::from(f),
                    &binding::repr::vesper__chorus__wire__WireHeader {
                        term: h.term,
                        leader_id: vesper_port_abi::Word::new(h.leader_id),
                        follower_slot: vesper_port_abi::Word::new(h.follower_slot),
                        prev_log_index: h.prev_log_index,
                        prev_log_term: h.prev_log_term,
                        leader_commit: h.leader_commit,
                    },
                    &raw_entries,
                );
                let ack = AppendAck {
                    follower_id: req_out.ack.follower_id.get(),
                    term: req_out.ack.term,
                    match_index: req_out.ack.match_index,
                    accepted: req_out.ack.accepted.get(),
                };
                let fs = fs.act();
                let r = if ack.accepted {
                    fs.select(&Accept(ack)).map(Session::close)
                } else {
                    fs.select(&Reject(ack)).map(Session::close)
                };
                binding::p2_replog_check_follower(
                    &mut self.core,
                    self.net.now_ns(),
                    u64::from(f),
                    req_out.changed.get(),
                    req_out.committed.get(),
                );
                if let Err(e) = r {
                    self.fault(e.into_error())?;
                }
            }
        }
        // Leader.
        let ch_offer = match ls.offer() {
            Err(e) => {
                self.fault(e.into_error())?;
                return Ok(true);
            }
            Ok(ch_offer) => ch_offer,
        };
        let (ack, ok) = match ch_offer.split() {
            Err(e) => return Err(ReplogError::Session(e.into_error())),
            Ok(Case::This(Accept(a), s)) => {
                s.close();
                (a, true)
            }
            Ok(Case::Other(ch_offer)) => match ch_offer.split() {
                Err(e) => return Err(ReplogError::Session(e.into_error())),
                Ok(Case::This(Reject(a), s)) => {
                    s.close();
                    (a, false)
                }
                Ok(Case::Other(ch_offer)) => {
                    return Err(ReplogError::Session(ch_offer.exhausted().into_error()));
                }
            },
        };
        let resp_out = binding::p2_replog_on_append_resp(
            &mut self.core,
            self.net.now_ns(),
            u64::from(l),
            u64::from(f),
            term,
            sent_last,
            &binding::repr::vesper__chorus__wire__WireAck {
                follower_id: vesper_port_abi::Word::new(ack.follower_id),
                term: ack.term,
                match_index: ack.match_index,
                accepted: vesper_port_abi::Word::new(ack.accepted),
            },
            ok,
        );
        Ok(resp_out.keep_leading.get())
    }

    /// Lead one term for `rounds` rounds (or until stepping down). With
    /// `until_converged`, stop early once every log equals the leader's and
    /// all of it is committed everywhere.
    fn lead(&mut self, l: u32, rounds: u32, until_converged: bool) -> Result<bool, ReplogError> {
        binding::p2_replog_begin_lead(&mut self.core, u64::from(l));
        for round in 0..rounds {
            if !until_converged || round == 0 {
                binding::p2_replog_propose_round(&mut self.core, self.net.now_ns(), u64::from(l));
            }
            for f in 0..self.n {
                if f == l {
                    continue;
                }
                if !self.append(l, f)? {
                    return Ok(false);
                }
            }
            if until_converged && binding::p2_replog_converged(&self.core, u64::from(l)) {
                return Ok(true);
            }
        }
        Ok(!until_converged)
    }

    fn partition(&mut self, on: bool) {
        binding::p2_replog_partition(&mut self.core, on);
        let n = usize::from(self.n as u16);
        let blocked = self.core.as_ref().blocked().to_vec();
        for s in 0..self.n {
            for d in 0..self.n {
                if s == d {
                    continue;
                }
                self.net
                    .set_blocked(s, d, blocked[s as usize * n + d as usize]);
            }
        }
    }
}

/// Run one seeded replicated-log scenario (see the module docs).
/// Runs the scripted terms over a [`WarpNet`] whose links authenticate with
/// `tls` (mutual TLS 1.3, one credential per host).
pub fn run_replicated_log(cfg: &ReplogConfig, tls: &WarpTls) -> Result<ReplogReport, ReplogError> {
    let n = cfg.net.nodes;
    match binding::p2_replog_validate_cfg(u64::from(n), cfg.max_batch as u64) {
        0 => {}
        1 => return Err(ReplogError::Config("nodes must be in 3..=7")),
        _ => return Err(ReplogError::Config("max_batch must be in 1..=MAX_BATCH")),
    }
    let net = WarpNet::new(cfg.net, tls).map_err(ReplogError::Net)?;
    let mut run = Run {
        n,
        net,
        vote: parse_choreography(VOTE_CHOREOGRAPHY).expect("vote choreography parses"),
        append: parse_choreography(APPEND_CHOREOGRAPHY).expect("append choreography parses"),
        auditor: LeakAuditor::new(),
        core: binding::p2_replog_new(
            cfg.net.seed,
            u64::from(n),
            cfg.max_batch as u64,
            u64::from(cfg.proposals_per_round),
            u64::from(cfg.partition_per_mille),
        ),
    };
    let mut term = 0u64;
    for t in 1..=u64::from(cfg.terms) {
        term = t;
        run.partition(true);
        let c = ((t - 1) % u64::from(n)) as u32;
        if run.elect(c, t)? {
            run.lead(c, cfg.rounds_per_term, false)?;
        }
        if run.core.as_ref().checker().has_violation() {
            break;
        }
    }
    // Healing: no partitions; candidates in turn until one leads every log
    // to convergence.
    run.partition(false);
    let mut converged = run.core.as_ref().checker().has_violation();
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
    let net_stats = run.net.stats();
    let digest = binding::p2_replog_digest(&run.core, net_stats.datagrams, net_stats.dropped);
    let vs = binding::p2_replog_stats(&run.core);
    let stats = ReplogStats {
        elections: vs.elections,
        elections_won: vs.elections_won,
        votes_granted: vs.votes_granted,
        votes_denied: vs.votes_denied,
        appends: vs.appends,
        accepted: vs.accepted,
        rejected: vs.rejected,
        rpc_faults: vs.rpc_faults,
        proposals: vs.proposals,
        partitioned_links: vs.partitioned_links,
    };
    let r = run.core.as_ref();
    let pool = r.log_pool();
    let offs = r.nd_log_off().as_slice();
    let lens = r.nd_log_len().as_slice();
    let logs = (0..n as usize)
        .map(|i| {
            let s = offs[i] as usize;
            let e = s + lens[i] as usize;
            (s..e)
                .map(|idx| {
                    let x = pool.get(idx).expect("log entry");
                    Entry {
                        term: x.term,
                        cmd: x.cmd,
                    }
                })
                .collect()
        })
        .collect();
    let commit = r.nd_commit().as_slice().to_vec();
    let chk = r.checker();
    Ok(ReplogReport {
        seed: cfg.net.seed,
        leaders: port::checker_leaders(&chk),
        logs,
        commit,
        committed: chk.committed().len(),
        violation: port::checker_violation(&chk).map(|(_, v)| v),
        checks: chk.checks(),
        stats,
        net: net_stats,
        audit: run.auditor.counts(),
        open_sessions: run.auditor.open(),
        open_streams: run.net.open_streams(),
        settled,
        open_stream_details: run.net.describe_open_streams(),
        digest,
    })
}
