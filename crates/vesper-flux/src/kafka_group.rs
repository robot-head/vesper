//! A Kafka consumer-group source whose rebalances are tied to Flux
//! checkpoints.
//!
//! A [`KafkaGroupSource`] is one member of a classic consumer group (the
//! `consumer` protocol type with a round-robin assignor of its own, so
//! standard tools can describe the group). It cuts epochs, like
//! [`super::KafkaSource`], only from the partitions assigned to it, into a
//! durable local epoch index that also records which bindings each epoch
//! owned, so every cut epoch replays identically after a fault.
//!
//! Input offsets reach the group only at checkpoints: a [`super::KafkaSink`]
//! built with `with_group_offsets` commits the offsets at the end of each
//! published epoch *inside that epoch's output transaction*
//! (`TxnOffsetCommit` carrying the member's generation, so a member the
//! group has already replaced is rejected). Input offsets and output
//! records therefore commit together.
//!
//! Rebalances are cooperative at checkpoint boundaries: when a heartbeat
//! reports a rebalance, [`KafkaGroupSource::poll`] stops cutting epochs and
//! returns [`GroupPoll::Rebalancing`] until every epoch it cut is
//! published, and only then rejoins. A partition therefore changes owner
//! exactly at a published checkpoint, and its new owner starts from the
//! offset (and CDC LSN) committed with that checkpoint's output.
//!
//! Scope: the Flux state of a member is not moved between members, so the
//! end-to-end guarantee across a rebalance holds for circuits whose output
//! for a binding depends only on that binding's records (per-partition
//! pipelines); with shared state, use static membership (`instance_id`).
//! A member restarted after a crash between its sink's local commit and the
//! Kafka publication refuses (typed error) to publish epochs whose
//! partitions the group gave away meanwhile.
use super::cdc::CdcPolicy;
use super::client::Client;
use super::kafka::{
    GroupOffsetSource, GroupOffsets, KafkaBinding, KafkaConnection, KafkaSourceLimits,
};
use super::wire::{Fail, KResult, R, W};
use super::{Circuit, EpochSource, TickInput};
use crate::zset::Accumulator;
use crate::{Budget, CdcHeader, Error, Limits, Row};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

const MAGIC: &[u8; 8] = b"VFLUXKG1";
const SUM: usize = 32;
const ASSIGNOR: &str = "vesper-flux-roundrobin";

/// Group membership settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KafkaGroupConfig {
    pub group_id: String,
    /// Static membership (KIP-345): the same member across restarts.
    pub instance_id: Option<String>,
    pub session_timeout: Duration,
    pub rebalance_timeout: Duration,
    pub heartbeat_interval: Duration,
}
impl KafkaGroupConfig {
    pub fn new(group_id: impl Into<String>) -> Self {
        Self {
            group_id: group_id.into(),
            instance_id: None,
            session_timeout: Duration::from_secs(10),
            rebalance_timeout: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(1),
        }
    }
}

/// What [`KafkaGroupSource::poll`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupPoll {
    /// Cut this epoch.
    Epoch(u64),
    /// Nothing new in the assigned partitions.
    Idle,
    /// A rebalance waits for the cut epochs to be published.
    Rebalancing,
}

/// One binding's range in one epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    owned: bool,
    start: (i64, u64),
    end: (i64, u64),
}
const SPAN: usize = 1 + 32;

struct Index {
    file: File,
    header_len: u64,
    count: usize,
    frames: Vec<(i32, Vec<Span>)>,
}
fn io(_: std::io::Error) -> Error {
    Error::Storage("io")
}
fn sum(domain: &[u8], bytes: &[u8]) -> [u8; SUM] {
    let mut h = blake3::Hasher::new();
    h.update(domain);
    h.update(bytes);
    *h.finalize().as_bytes()
}
impl Index {
    fn frame_len(&self) -> usize {
        8 + 4 + SPAN * self.count + SUM
    }
    fn header(fp: &[u8; 32], count: usize) -> Result<Vec<u8>, Error> {
        let mut h = Vec::new();
        h.extend_from_slice(MAGIC);
        h.extend_from_slice(
            &u32::try_from(count)
                .map_err(|_| Error::Overflow)?
                .to_le_bytes(),
        );
        h.extend_from_slice(fp);
        let s = sum(b"vesper.flux.kafka-group-index.header/1", &h);
        h.extend_from_slice(&s);
        Ok(h)
    }
    fn open(path: &Path, fp: &[u8; 32], count: usize, max: u64) -> Result<Self, Error> {
        let header = Self::header(fp, count)?;
        if !path.exists() {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(io)?;
            file.write_all(&header).map_err(io)?;
            file.sync_all().map_err(io)?;
            // The new directory entry must survive a power loss too.
            super::kafka::sync_dir(path)?;
            return Ok(Self {
                file,
                header_len: header.len() as u64,
                count,
                frames: Vec::new(),
            });
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(io)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(io)?;
        if bytes.len() < header.len() || bytes[..header.len()] != header[..] {
            return Err(Error::Storage("kafka group index header"));
        }
        let mut index = Self {
            file,
            header_len: header.len() as u64,
            count,
            frames: Vec::new(),
        };
        let fl = index.frame_len();
        let body = &bytes[header.len()..];
        let full = body.len() / fl;
        let mut keep = 0;
        for i in 0..full {
            let frame = &body[i * fl..(i + 1) * fl];
            let (b, s) = frame.split_at(fl - SUM);
            if sum(b"vesper.flux.kafka-group-index.frame/1", b) != s {
                if i + 1 == full {
                    break; // torn final append
                }
                return Err(Error::Storage("corrupt kafka group index"));
            }
            if index.frames.len() as u64 >= max {
                return Err(Error::Limit("kafka epochs"));
            }
            let mut r = Bytes(b, 0);
            if r.u64()? != i as u64 {
                return Err(Error::Storage("corrupt kafka group index"));
            }
            let generation = r.i32()?;
            let mut spans = Vec::with_capacity(count);
            for k in 0..count {
                let owned = match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(Error::Storage("corrupt kafka group index")),
                };
                let span = Span {
                    owned,
                    start: (r.i64()?, r.u64()?),
                    end: (r.i64()?, r.u64()?),
                };
                let prev_end = index.frames.last().map(|(_, s)| s[k].end);
                let monotone = span.start.0 <= span.end.0
                    && span.start.1 <= span.end.1
                    && prev_end.is_none_or(|p| p.0 <= span.start.0 && p.1 <= span.start.1);
                if !monotone || (!owned && span.start != span.end) {
                    return Err(Error::Storage("corrupt kafka group index"));
                }
                spans.push(span);
            }
            index.frames.push((generation, spans));
            keep = i + 1;
        }
        let end = header.len() + keep * fl;
        if bytes.len() > end {
            index.file.set_len(end as u64).map_err(io)?;
            index.file.sync_all().map_err(io)?;
        }
        Ok(index)
    }
    fn append(&mut self, generation: i32, spans: &[Span]) -> Result<u64, Error> {
        let epoch = self.frames.len() as u64;
        let mut b = Vec::with_capacity(self.frame_len());
        b.extend_from_slice(&epoch.to_le_bytes());
        b.extend_from_slice(&generation.to_le_bytes());
        for s in spans {
            b.push(u8::from(s.owned));
            b.extend_from_slice(&s.start.0.to_le_bytes());
            b.extend_from_slice(&s.start.1.to_le_bytes());
            b.extend_from_slice(&s.end.0.to_le_bytes());
            b.extend_from_slice(&s.end.1.to_le_bytes());
        }
        let s = sum(b"vesper.flux.kafka-group-index.frame/1", &b);
        b.extend_from_slice(&s);
        let at = self.header_len + epoch * self.frame_len() as u64;
        self.file.seek(SeekFrom::Start(at)).map_err(io)?;
        self.file.write_all(&b).map_err(io)?;
        self.file.sync_all().map_err(io)?;
        self.frames.push((generation, spans.to_vec()));
        Ok(epoch)
    }
}
struct Bytes<'a>(&'a [u8], usize);
impl Bytes<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let s = self
            .0
            .get(self.1..self.1 + N)
            .ok_or(Error::Storage("corrupt kafka group index"))?;
        self.1 += N;
        s.try_into().map_err(|_| Error::Overflow)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take::<1>()?[0])
    }
    fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(self.take()?))
    }
    fn i64(&mut self) -> Result<i64, Error> {
        Ok(i64::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take()?))
    }
}

/// Mutable membership state.
struct Member {
    member_id: String,
    generation: i32,
    owned: BTreeSet<usize>,
    /// Current position of each owned binding.
    position: BTreeMap<usize, (i64, u64)>,
    rebalance: bool,
    /// The session expired: the group no longer knows this member.
    lost: bool,
    last_heartbeat: Instant,
    published: Option<u64>,
    joins: u64,
}

/// A consumer-group member cutting replayable epochs (see the module docs).
pub struct KafkaGroupSource {
    conn: KafkaConnection,
    policy: CdcPolicy,
    topics: Vec<KafkaBinding>,
    widths: Vec<usize>,
    limits: Limits,
    source_limits: KafkaSourceLimits,
    group: KafkaGroupConfig,
    index: RefCell<Index>,
    member: RefCell<Member>,
    cache: RefCell<BTreeMap<u64, TickInput>>,
    decoded: Cell<u64>,
}
impl std::fmt::Debug for KafkaGroupSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KafkaGroupSource")
            .field("group", &self.group.group_id)
            .field("owned", &self.member.borrow().owned)
            .finish_non_exhaustive()
    }
}

fn fingerprint(
    policy: &CdcPolicy,
    topics: &[KafkaBinding],
    widths: &[usize],
    group: &str,
) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"vesper.flux.kafka-group-source/1");
    h.update(&(group.len() as u64).to_le_bytes());
    h.update(group.as_bytes());
    for w in widths {
        h.update(&(*w as u64).to_le_bytes());
    }
    for (b, t) in policy.bindings.iter().zip(topics) {
        h.update(&(t.topic.len() as u64).to_le_bytes());
        h.update(t.topic.as_bytes());
        h.update(&t.partition.to_le_bytes());
        h.update(&b.partition_id.to_le_bytes());
        h.update(&b.schema_id.to_le_bytes());
        h.update(&(b.input as u64).to_le_bytes());
        h.update(&b.first_lsn.to_le_bytes());
    }
    *h.finalize().as_bytes()
}

/// Consumer protocol subscription (version 0): the topics, with the exact
/// bindings (topic, partition) as user data, so the leader can check that
/// every member subscribes to the same bindings.
fn subscription(topics: &[KafkaBinding]) -> KResult<Vec<u8>> {
    let names: BTreeSet<&str> = topics.iter().map(|t| t.topic.as_str()).collect();
    let bindings: BTreeSet<(&str, i32)> = topics
        .iter()
        .map(|t| (t.topic.as_str(), t.partition))
        .collect();
    let mut user = W::new(false);
    user.array(bindings.len())?;
    for (t, p) in bindings {
        user.str(t)?;
        user.i32(p);
    }
    let mut w = W::new(false);
    w.i16(0);
    w.array(names.len())?;
    for n in names {
        w.str(n)?;
    }
    w.bytes(&user.buf)?;
    Ok(w.buf)
}
/// Consumer protocol assignment (version 0).
fn assignment(parts: &BTreeMap<String, Vec<i32>>) -> KResult<Vec<u8>> {
    let mut w = W::new(false);
    w.i16(0);
    w.array(parts.len())?;
    for (t, ps) in parts {
        w.str(t)?;
        w.array(ps.len())?;
        for p in ps {
            w.i32(*p);
        }
    }
    w.bytes(&[])?;
    Ok(w.buf)
}
fn parse_assignment(b: &[u8]) -> KResult<Vec<(String, i32)>> {
    if b.is_empty() {
        return Ok(Vec::new());
    }
    let mut r = R::new(b, false);
    r.i16()?;
    let mut out = Vec::new();
    for _ in 0..r.array()? {
        let t = r.str()?;
        for _ in 0..r.array()? {
            out.push((t.clone(), r.i32()?));
        }
    }
    Ok(out)
}
fn input_metadata(lsn: u64) -> String {
    format!("vflux/1 l={lsn}")
}
fn parse_input_metadata(m: &str) -> Result<u64, Error> {
    m.strip_prefix("vflux/1 l=")
        .and_then(|s| s.parse().ok())
        .ok_or(Error::Kafka("group offset metadata"))
}

impl KafkaGroupSource {
    /// Open (or create) the local index and join the group (blocking until
    /// the group has an assignment).
    pub fn join(
        conn: KafkaConnection,
        circuit: &Circuit,
        policy: CdcPolicy,
        topics: Vec<KafkaBinding>,
        index: impl AsRef<Path>,
        source_limits: KafkaSourceLimits,
        group: KafkaGroupConfig,
    ) -> Result<Self, Error> {
        policy.validate(circuit)?;
        if topics.len() != policy.bindings.len()
            || source_limits.max_records_per_epoch == 0
            || source_limits.max_records_per_epoch > 65_536
            || source_limits.max_records_per_epoch > circuit.limits.max_input_entries
            || source_limits.max_epochs == 0
            || source_limits.dead_letter.is_some()
            || group.group_id.is_empty()
            || group.heartbeat_interval >= group.session_timeout
        {
            return Err(Error::Limit("kafka group source"));
        }
        for (i, t) in topics.iter().enumerate() {
            if t.partition < 0 || topics[..i].contains(t) {
                return Err(Error::Kafka("bindings"));
            }
        }
        let widths = circuit.inputs.clone();
        let fp = fingerprint(&policy, &topics, &widths, &group.group_id);
        let index = Index::open(index.as_ref(), &fp, topics.len(), source_limits.max_epochs)?;
        let source = Self {
            conn,
            policy,
            topics,
            widths,
            limits: circuit.limits,
            source_limits,
            group,
            index: RefCell::new(index),
            member: RefCell::new(Member {
                member_id: String::new(),
                generation: -1,
                owned: BTreeSet::new(),
                position: BTreeMap::new(),
                rebalance: false,
                lost: false,
                last_heartbeat: Instant::now(),
                published: None,
                joins: 0,
            }),
            cache: RefCell::new(BTreeMap::new()),
            decoded: Cell::new(0),
        };
        source.rejoin()?;
        Ok(source)
    }

    pub fn connection(&self) -> &KafkaConnection {
        &self.conn
    }
    pub fn generation(&self) -> i32 {
        self.member.borrow().generation
    }
    /// Completed joins (the first one included).
    pub fn joins(&self) -> u64 {
        self.member.borrow().joins
    }
    /// The bindings currently assigned to this member.
    pub fn assignment(&self) -> Vec<KafkaBinding> {
        let m = self.member.borrow();
        m.owned.iter().map(|i| self.topics[*i].clone()).collect()
    }
    /// The last epoch the sink reported published.
    pub fn published(&self) -> Option<u64> {
        self.member.borrow().published
    }

    /// Join (or rejoin) the group and take up the new assignment.
    fn rejoin(&self) -> Result<(), Error> {
        let meta = self.conn.run(|_| subscription(&self.topics))?;
        let g = &self.group;
        let mut member_id = self.member.borrow().member_id.clone();
        let joined = loop {
            let r = self.conn.run(|c| {
                c.retry(|c| {
                    c.join_group(
                        &g.group_id,
                        &member_id,
                        g.instance_id.as_deref(),
                        g.session_timeout,
                        g.rebalance_timeout,
                        ASSIGNOR,
                        &meta,
                    )
                })
            });
            match r {
                Ok(j) if j.retry => member_id = j.member_id,
                Ok(j) => break j,
                Err(Error::Kafka("unknown member")) if !member_id.is_empty() => {
                    member_id.clear();
                }
                Err(e) => return Err(e),
            }
        };
        if joined.protocol != ASSIGNOR {
            return Err(Error::Kafka("group assignor"));
        }
        let assignments = if joined.leader == joined.member_id {
            self.assign(&joined.members)?
        } else {
            Vec::new()
        };
        let mine = self.conn.run(|c| {
            c.sync_group(
                &g.group_id,
                joined.generation,
                &joined.member_id,
                g.instance_id.as_deref(),
                &assignments,
            )
        })?;
        let parts = self.conn.run(|_| parse_assignment(&mine))?;
        let mut owned = BTreeSet::new();
        for (t, p) in parts {
            let i = self
                .topics
                .iter()
                .position(|b| b.topic == t && b.partition == p)
                .ok_or(Error::Kafka("assignment outside the bindings"))?;
            owned.insert(i);
        }
        // Positions: our own end where we cut beyond the group's offset (the
        // sink publishes those epochs with their offsets), else the group's
        // committed offset, else the start of the partition.
        let frames: Vec<Vec<Span>> = self
            .index
            .borrow()
            .frames
            .iter()
            .map(|(_, s)| s.clone())
            .collect();
        let mut by_topic: BTreeMap<&str, Vec<i32>> = BTreeMap::new();
        for i in &owned {
            by_topic
                .entry(self.topics[*i].topic.as_str())
                .or_default()
                .push(self.topics[*i].partition);
        }
        let mut committed: BTreeMap<(String, i32), (i64, String)> = BTreeMap::new();
        for (t, ps) in &by_topic {
            for (p, v) in self.conn.group_offsets(&g.group_id, t, ps)? {
                committed.insert((t.to_string(), p), v);
            }
        }
        let mut position = BTreeMap::new();
        for i in &owned {
            let b = &self.topics[*i];
            let own_end = frames.iter().rev().find(|s| s[*i].owned).map(|s| s[*i].end);
            let group = match committed.get(&(b.topic.clone(), b.partition)) {
                Some((offset, meta)) => Some((*offset, parse_input_metadata(meta)?)),
                None => None,
            };
            let pos = match (group, own_end) {
                (Some(c), Some(e)) if e.0 >= c.0 => e,
                (Some(c), _) => c,
                (None, Some(e)) => e,
                (None, None) => {
                    let (earliest, _) = self.conn.offsets(&b.topic, b.partition)?;
                    (earliest, self.policy.bindings[*i].first_lsn)
                }
            };
            position.insert(*i, pos);
        }
        let mut m = self.member.borrow_mut();
        m.member_id = joined.member_id;
        m.generation = joined.generation;
        m.owned = owned;
        m.position = position;
        m.rebalance = false;
        m.lost = false;
        m.last_heartbeat = Instant::now();
        m.joins += 1;
        Ok(())
    }

    /// Leader: round-robin the bindings over the sorted member ids.
    fn assign(&self, members: &[(String, Vec<u8>)]) -> Result<Vec<(String, Vec<u8>)>, Error> {
        // The round-robin spreads the leader's bindings over every member, so
        // every member must subscribe to exactly the same bindings.
        let own = self.conn.run(|_| subscription(&self.topics))?;
        if members.iter().any(|(_, meta)| *meta != own) {
            return Err(Error::Kafka(
                "group members subscribe to different bindings",
            ));
        }
        let mut ids: Vec<&str> = members.iter().map(|(m, _)| m.as_str()).collect();
        ids.sort_unstable();
        let mut per: BTreeMap<&str, BTreeMap<String, Vec<i32>>> =
            ids.iter().map(|m| (*m, BTreeMap::new())).collect();
        for (i, b) in self.topics.iter().enumerate() {
            let m = ids[i % ids.len().max(1)];
            per.entry(m)
                .or_default()
                .entry(b.topic.clone())
                .or_default()
                .push(b.partition);
        }
        per.into_iter()
            .map(|(m, parts)| Ok((m.to_string(), self.conn.run(|_| assignment(&parts))?)))
            .collect()
    }

    fn heartbeat(&self) -> Result<(), Error> {
        let (gen_, id) = {
            let m = self.member.borrow();
            if m.last_heartbeat.elapsed() < self.group.heartbeat_interval {
                return Ok(());
            }
            (m.generation, m.member_id.clone())
        };
        let g = &self.group;
        let r = self.conn.raw(|c: &mut Client| {
            c.retry(
                |c| match c.heartbeat(&g.group_id, gen_, &id, g.instance_id.as_deref()) {
                    Err(Fail::Code(27, _)) => Ok((true, false)),
                    // ILLEGAL_GENERATION, UNKNOWN_MEMBER_ID: the session
                    // expired and the group moved on without this member.
                    Err(Fail::Code(22 | 25, _)) => Ok((true, true)),
                    Err(e) => Err(e),
                    Ok(()) => Ok((false, false)),
                },
            )
        });
        let mut m = self.member.borrow_mut();
        m.last_heartbeat = Instant::now();
        match r {
            Ok((rebalance, lost)) => {
                m.rebalance |= rebalance;
                m.lost |= lost;
                Ok(())
            }
            Err(e) => Err(e.error()),
        }
    }

    /// Heartbeat now if the interval has passed. Call this from long
    /// processing between polls: a member whose session expires while it
    /// holds unpublished epochs cannot commit their input offsets any more.
    /// The sink also calls it before each transaction.
    pub fn keep_alive(&self) -> Result<(), Error> {
        self.heartbeat()
    }

    /// Heartbeat; rejoin when a rebalance is pending and everything cut is
    /// published; otherwise cut the next epoch from the owned partitions.
    pub fn poll(&self) -> Result<GroupPoll, Error> {
        self.heartbeat()?;
        let cut = self.epochs();
        let (rebalance, published, lost) = {
            let m = self.member.borrow();
            (m.rebalance, m.published, m.lost)
        };
        if rebalance {
            let all_published = cut == 0 || published.is_some_and(|p| p + 1 >= cut);
            if !all_published && lost {
                // Waiting would never end: the group already gave the
                // partitions away, so these epochs cannot be published here.
                return Err(Error::Kafka(
                    "group membership lost with unpublished epochs",
                ));
            }
            if !all_published {
                return Ok(GroupPoll::Rebalancing);
            }
            self.rejoin()?;
        }
        if cut >= self.source_limits.max_epochs {
            return Err(Error::Limit("kafka epochs"));
        }
        let m = self.member.borrow();
        let generation = m.generation;
        let cap = self.source_limits.max_records_per_epoch as i64;
        let last = self.index.borrow().frames.last().map(|(_, s)| s.clone());
        let mut spans = Vec::with_capacity(self.topics.len());
        let mut any = false;
        for (i, t) in self.topics.iter().enumerate() {
            if let Some(&start) = m.position.get(&i) {
                let (earliest, latest) = self.conn.offsets(&t.topic, t.partition)?;
                if earliest > start.0 {
                    return Err(Error::Kafka("offset out of range"));
                }
                let end = latest.min(start.0.saturating_add(cap));
                any |= end > start.0;
                spans.push(Span {
                    owned: true,
                    start,
                    end: (end, start.1),
                });
            } else {
                let at = last.as_ref().map_or((0, 0), |s| s[i].end);
                spans.push(Span {
                    owned: false,
                    start: at,
                    end: at,
                });
            }
        }
        drop(m);
        if !any {
            return Ok(GroupPoll::Idle);
        }
        let (input, lsns) = self.decode(&spans)?;
        for (s, l) in spans.iter_mut().zip(&lsns) {
            s.end.1 = *l;
        }
        let e = self.index.borrow_mut().append(generation, &spans)?;
        let mut m = self.member.borrow_mut();
        for (i, s) in spans.iter().enumerate() {
            if s.owned {
                m.position.insert(i, s.end);
            }
        }
        drop(m);
        self.remember(e, input);
        Ok(GroupPoll::Epoch(e))
    }

    /// Leave the group (other members take the partitions at once).
    pub fn leave(&self) -> Result<(), Error> {
        let m = self.member.borrow();
        let g = &self.group;
        self.conn
            .run(|c| c.leave_group(&g.group_id, &m.member_id, g.instance_id.as_deref()))
    }

    fn remember(&self, epoch: u64, input: TickInput) {
        let mut cache = self.cache.borrow_mut();
        cache.insert(epoch, input);
        while cache.len() > self.source_limits.cache_epochs.max(1) {
            cache.pop_first();
        }
    }

    fn decode(&self, spans: &[Span]) -> Result<(TickInput, Vec<u64>), Error> {
        let mut acc = Accumulator::new(&self.widths, self.limits)?;
        let mut budget = Budget::new(self.limits);
        let mut lsns = Vec::with_capacity(spans.len());
        for (i, (t, s)) in self.topics.iter().zip(spans).enumerate() {
            let binding = &self.policy.bindings[i];
            let width = self.widths[binding.input];
            let mut next = s.start.1;
            if s.owned {
                for (_, record) in self.conn.fetch(&t.topic, t.partition, s.start.0, s.end.0)? {
                    budget.charge(1)?;
                    let (header, row) = decode(record.value.as_ref(), width)?;
                    if self.policy.binding(header)? != i {
                        return Err(Error::Cdc("partition"));
                    }
                    if header.source_lsn < next {
                        continue;
                    }
                    if header.source_lsn != next {
                        return Err(Error::Cdc("source offset"));
                    }
                    next = next.checked_add(1).ok_or(Error::Overflow)?;
                    acc.add(
                        binding.input,
                        &row,
                        i128::from(header.z_weight),
                        &mut budget,
                    )?;
                }
            }
            lsns.push(next);
        }
        self.decoded.set(self.decoded.get() + 1);
        let deltas = acc.finish()?;
        Ok((TickInput { deltas }, lsns))
    }
}

fn decode(value: Option<&Vec<u8>>, width: usize) -> Result<(CdcHeader, Row), Error> {
    let bytes = value.ok_or(Error::Cdc("record without value"))?;
    if bytes.len() != CdcHeader::BYTES + 8 * width {
        return Err(Error::Cdc("record length"));
    }
    let header = CdcHeader::decode(&bytes[..CdcHeader::BYTES])?;
    let row = bytes[CdcHeader::BYTES..]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| i64::from_le_bytes(*c))
        .collect();
    Ok((header, row))
}

impl EpochSource for KafkaGroupSource {
    fn epochs(&self) -> u64 {
        self.index.borrow().frames.len() as u64
    }
    fn read(&self, epoch: u64) -> Result<TickInput, Error> {
        if let Some(t) = self.cache.borrow().get(&epoch) {
            return Ok(t.clone());
        }
        let spans = {
            let index = self.index.borrow();
            index
                .frames
                .get(usize::try_from(epoch).map_err(|_| Error::Overflow)?)
                .map(|(_, s)| s.clone())
                .ok_or(Error::InputCount)?
        };
        let (input, lsns) = self.decode(&spans)?;
        if lsns.iter().zip(&spans).any(|(l, s)| *l != s.end.1) {
            return Err(Error::Kafka("replay differs from the indexed epoch"));
        }
        self.remember(epoch, input.clone());
        Ok(input)
    }
}

impl GroupOffsetSource for KafkaGroupSource {
    fn offsets_after(&self, epoch: u64) -> Result<Option<GroupOffsets>, Error> {
        // Publication can take long: keep the session alive.
        self.heartbeat()?;
        let spans = {
            let index = self.index.borrow();
            index
                .frames
                .get(usize::try_from(epoch).map_err(|_| Error::Overflow)?)
                .map(|(_, s)| s.clone())
                .ok_or(Error::InputCount)?
        };
        let (generation, member_id, owned) = {
            let m = self.member.borrow();
            (m.generation, m.member_id.clone(), m.owned.clone())
        };
        let mut offsets = Vec::new();
        for (i, s) in spans.iter().enumerate() {
            if !s.owned {
                continue;
            }
            let t = &self.topics[i];
            // The epoch must continue exactly where the group's offset is: a
            // partition another member read meanwhile, or one the group gave
            // away, would deliver its records twice.
            let committed =
                self.conn
                    .group_offsets(&self.group.group_id, &t.topic, &[t.partition])?;
            let continues = match committed.get(&t.partition) {
                Some((offset, _)) => *offset == s.start.0,
                None => true,
            };
            if !owned.contains(&i) || !continues {
                return Err(Error::Kafka("assignment lost unpublished epochs"));
            }
            offsets.push((
                t.topic.clone(),
                t.partition,
                s.end.0,
                input_metadata(s.end.1),
            ));
        }
        if offsets.is_empty() {
            return Ok(None);
        }
        Ok(Some(GroupOffsets {
            group: self.group.group_id.clone(),
            generation,
            member_id,
            instance_id: self.group.instance_id.clone(),
            offsets,
        }))
    }
    fn published(&self, epoch: u64) {
        let mut m = self.member.borrow_mut();
        m.published = Some(m.published.map_or(epoch, |p| p.max(epoch)));
    }
}
