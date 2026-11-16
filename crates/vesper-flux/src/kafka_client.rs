//! Cluster-level Kafka client over [`super::wire`]: broker metadata and
//! leaders, coordinator lookup, retries, the data and admin APIs, an
//! idempotent / transactional producer and classic consumer-group
//! membership.
use super::wire::{
    Batch, Conn, ConnSettings, Fail, KResult, KafkaCompression, KafkaRecord, ProducerStamp, R, W,
    api, committed_records, decode_batches, encode_batch,
};
use crate::Error;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// Coordinator kinds (FindCoordinator key_type).
pub(crate) const GROUP: i8 = 0;
pub(crate) const TXN: i8 = 1;

/// One partition's fetch result.
pub(crate) struct Fetched {
    pub(crate) high_watermark: i64,
    pub(crate) last_stable: i64,
    pub(crate) log_start: i64,
    pub(crate) records: Vec<(i64, KafkaRecord)>,
    /// Offset after the last returned batch, if any.
    pub(crate) end: Option<i64>,
}

/// Offsets committed for a group (`TxnOffsetCommit` / `OffsetCommit`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupOffsets {
    pub group: String,
    /// Generation and member of a joined group, or `-1` / empty for a
    /// group nobody joins (the sink's progress group).
    pub generation: i32,
    pub member_id: String,
    pub instance_id: Option<String>,
    /// (topic, partition, next offset, metadata).
    pub offsets: Vec<(String, i32, i64, String)>,
}

/// A committed group offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Committed {
    pub(crate) offset: i64,
    pub(crate) metadata: String,
}

pub(crate) struct Client {
    settings: ConnSettings,
    bootstrap: Vec<String>,
    pub(crate) retry_for: Duration,
    conns: BTreeMap<i32, Conn>,
    brokers: BTreeMap<i32, String>,
    leaders: BTreeMap<(String, i32), i32>,
    coords: BTreeMap<(i8, String), i32>,
    pub(crate) last_error: Option<String>,
}

/// Bootstrap connections use negative ids below this.
const BOOT: i32 = -1000;

impl Client {
    pub(crate) fn new(settings: ConnSettings, bootstrap: Vec<String>, retry_for: Duration) -> Self {
        Self {
            settings,
            bootstrap,
            retry_for,
            conns: BTreeMap::new(),
            brokers: BTreeMap::new(),
            leaders: BTreeMap::new(),
            coords: BTreeMap::new(),
            last_error: None,
        }
    }

    pub(crate) fn reachable(&mut self) -> KResult<()> {
        self.retry(|c| c.any().map(|_| ()))
    }

    pub(crate) fn note(&mut self, f: &Fail) {
        self.last_error = Some(f.message());
    }

    /// Forget every connection (after a transport failure the stream state
    /// is unknown).
    pub(crate) fn reset(&mut self) {
        self.conns.clear();
        self.leaders.clear();
        self.coords.clear();
    }

    fn conn(&mut self, node: i32) -> KResult<&mut Conn> {
        if !self.conns.contains_key(&node) {
            let addr = if node <= BOOT {
                let i = usize::try_from(BOOT - node).map_err(|_| Fail::Proto("node"))?;
                self.bootstrap.get(i).cloned().ok_or(Fail::Proto("node"))?
            } else {
                self.brokers
                    .get(&node)
                    .cloned()
                    .ok_or(Fail::Io(format!("unknown broker {node}")))?
            };
            let c = Conn::open(&addr, &self.settings)?;
            self.conns.insert(node, c);
        }
        self.conns.get_mut(&node).ok_or(Fail::Proto("node"))
    }

    /// Any reachable broker (a known one first, then the bootstrap list).
    fn any(&mut self) -> KResult<i32> {
        if let Some(&n) = self.conns.keys().next() {
            return Ok(n);
        }
        let mut last = Fail::Io("no bootstrap broker".into());
        let known: Vec<i32> = self.brokers.keys().copied().collect();
        let boot: Vec<i32> = (0..self.bootstrap.len() as i32).map(|i| BOOT - i).collect();
        for n in known.into_iter().chain(boot) {
            match self.conn(n) {
                Ok(_) => return Ok(n),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// One request on `node`; a transport failure drops the connection.
    pub(crate) fn call(
        &mut self,
        node: i32,
        key: i16,
        body: impl FnOnce(&mut W) -> KResult<()>,
    ) -> KResult<Vec<u8>> {
        let out = self.conn(node)?.call(key, body);
        if let Err(Fail::Io(_) | Fail::Proto(_)) = &out {
            self.conns.remove(&node);
        }
        out
    }

    /// Run `f` until it succeeds, fails permanently, or `retry_for` passes.
    pub(crate) fn retry<T>(&mut self, mut f: impl FnMut(&mut Self) -> KResult<T>) -> KResult<T> {
        let deadline = Instant::now() + self.retry_for;
        let mut backoff = Duration::from_millis(20);
        loop {
            match f(self) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    self.note(&e);
                    if !e.retriable() || Instant::now() >= deadline {
                        return Err(e);
                    }
                    if matches!(e, Fail::Io(_)) {
                        self.reset();
                    }
                    if matches!(e.code(), Some(3 | 5 | 6 | 74 | 75)) {
                        self.leaders.clear();
                    }
                    if e.coordinator_moved() {
                        self.coords.clear();
                    }
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(500));
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Metadata
    // ------------------------------------------------------------------

    fn refresh(&mut self, topic: &str) -> KResult<()> {
        let node = self.any()?;
        let body = self.call(node, api::METADATA, |w| {
            w.array(1)?.str(topic)?;
            w.bool(false).bool(false).bool(false);
            Ok(())
        })?;
        let mut r = R::new(&body, false);
        r.i32()?;
        let nb = r.array()?;
        let mut brokers = BTreeMap::new();
        for _ in 0..nb {
            let id = r.i32()?;
            let host = r.str()?;
            let port = r.i32()?;
            r.nstr()?;
            let addr = if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            };
            brokers.insert(id, addr);
        }
        r.nstr()?;
        r.i32()?;
        let nt = r.array()?;
        for _ in 0..nt {
            let code = r.i16()?;
            let name = r.str()?;
            r.bool()?;
            let np = r.array()?;
            let mut parts = Vec::new();
            for _ in 0..np {
                let pcode = r.i16()?;
                let index = r.i32()?;
                let leader = r.i32()?;
                r.i32()?;
                for _ in 0..3 {
                    let n = r.array()?;
                    r.take(n.checked_mul(4).ok_or(Fail::Proto("metadata"))?)?;
                }
                parts.push((pcode, index, leader));
            }
            r.i32()?;
            if name == topic {
                check_code(code, "metadata")?;
                for (pcode, index, leader) in parts {
                    if pcode == 0 && leader >= 0 {
                        self.leaders.insert((name.clone(), index), leader);
                    }
                }
            }
        }
        for (id, addr) in brokers {
            if self.brokers.get(&id) != Some(&addr) {
                self.conns.remove(&id);
            }
            self.brokers.insert(id, addr);
        }
        Ok(())
    }

    fn leader(&mut self, topic: &str, partition: i32) -> KResult<i32> {
        let key = (topic.to_string(), partition);
        if let Some(&l) = self.leaders.get(&key) {
            return Ok(l);
        }
        self.refresh(topic)?;
        self.leaders
            .get(&key)
            .copied()
            .ok_or(Fail::Code(3, format!("leader of {topic}/{partition}")))
    }

    pub(crate) fn partitions(&mut self, topic: &str) -> KResult<i32> {
        self.retry(|c| {
            c.refresh(topic)?;
            let n = c.leaders.keys().filter(|(t, _)| t == topic).count();
            if n == 0 {
                return Err(Fail::Code(3, format!("partitions of {topic}")));
            }
            i32::try_from(n).map_err(|_| Fail::Local(Error::Overflow))
        })
    }

    // ------------------------------------------------------------------
    // Admin
    // ------------------------------------------------------------------

    pub(crate) fn create_topic(&mut self, topic: &str, partitions: i32) -> KResult<()> {
        let r = self.retry(|c| {
            let node = c.any()?;
            let body = c.call(node, api::CREATE_TOPICS, |w| {
                w.array(1)?.str(topic)?;
                w.i32(partitions).i16(1);
                w.array(0)?;
                w.array(0)?;
                w.i32(30_000).bool(false);
                Ok(())
            })?;
            let mut r = R::new(&body, false);
            r.i32()?;
            let n = r.array()?;
            for _ in 0..n {
                r.str()?;
                let code = r.i16()?;
                let msg = r.nstr()?;
                if code != 0 {
                    return Err(Fail::Code(
                        code,
                        format!("create topic {topic}: {}", msg.unwrap_or_default()),
                    ));
                }
            }
            Ok(())
        });
        r?;
        // Wait until every partition has a leader.
        self.retry(|c| {
            c.refresh(topic)?;
            let n = c.leaders.keys().filter(|(t, _)| t == topic).count();
            if n < partitions as usize {
                return Err(Fail::Code(5, format!("leaders of {topic}")));
            }
            Ok(())
        })
    }

    pub(crate) fn delete_records(
        &mut self,
        topic: &str,
        partition: i32,
        offset: i64,
    ) -> KResult<()> {
        self.retry(|c| {
            let node = c.leader(topic, partition)?;
            let body = c.call(node, api::DELETE_RECORDS, |w| {
                w.array(1)?.str(topic)?;
                w.array(1)?.i32(partition).i64(offset);
                w.i32(30_000);
                Ok(())
            })?;
            let mut r = R::new(&body, false);
            r.i32()?;
            let n = r.array()?;
            for _ in 0..n {
                r.str()?;
                let np = r.array()?;
                for _ in 0..np {
                    r.i32()?;
                    r.i64()?;
                    check_code(r.i16()?, "delete records")?;
                }
            }
            Ok(())
        })
    }

    // ------------------------------------------------------------------
    // Offsets and fetch
    // ------------------------------------------------------------------

    /// Offset for timestamp `ts` (-2 earliest, -1 latest; latest is the last
    /// stable offset when `committed`).
    pub(crate) fn list_offset(
        &mut self,
        topic: &str,
        partition: i32,
        ts: i64,
        committed: bool,
    ) -> KResult<i64> {
        self.retry(|c| {
            let node = c.leader(topic, partition)?;
            let body = c.call(node, api::LIST_OFFSETS, |w| {
                w.i32(-1).i8(i8::from(committed));
                w.array(1)?.str(topic)?;
                w.array(1)?.i32(partition).i32(-1).i64(ts);
                Ok(())
            })?;
            let mut r = R::new(&body, false);
            r.i32()?;
            let mut out = None;
            for _ in 0..r.array()? {
                r.str()?;
                for _ in 0..r.array()? {
                    let p = r.i32()?;
                    let code = r.i16()?;
                    r.i64()?;
                    let offset = r.i64()?;
                    r.i32()?;
                    check_code(code, "list offsets")?;
                    if p == partition {
                        out = Some(offset);
                    }
                }
            }
            out.ok_or(Fail::Proto("list offsets"))
        })
    }

    pub(crate) fn fetch(
        &mut self,
        topic: &str,
        partition: i32,
        offset: i64,
        max_bytes: i32,
        committed: bool,
    ) -> KResult<Fetched> {
        let max_decoded = self.settings.max_response;
        self.retry(|c| {
            let node = c.leader(topic, partition)?;
            let body = c.call(node, api::FETCH, |w| {
                w.i32(-1)
                    .i32(50)
                    .i32(1)
                    .i32(max_bytes)
                    .i8(i8::from(committed));
                w.i32(0).i32(-1);
                w.array(1)?.str(topic)?;
                w.array(1)?
                    .i32(partition)
                    .i32(-1)
                    .i64(offset)
                    .i64(-1)
                    .i32(max_bytes);
                w.array(0)?;
                w.str("")?;
                Ok(())
            })?;
            let mut r = R::new(&body, false);
            r.i32()?;
            check_code(r.i16()?, "fetch")?;
            r.i32()?;
            let mut out = None;
            for _ in 0..r.array()? {
                r.str()?;
                for _ in 0..r.array()? {
                    let p = r.i32()?;
                    let code = r.i16()?;
                    let high_watermark = r.i64()?;
                    let last_stable = r.i64()?;
                    let log_start = r.i64()?;
                    let na = r.array()?;
                    let mut aborted = Vec::with_capacity(na.min(1024));
                    for _ in 0..na {
                        aborted.push((r.i64()?, r.i64()?));
                    }
                    r.i32()?;
                    let records = r.nbytes()?.unwrap_or(&[]);
                    check_code(code, "fetch")?;
                    if p != partition {
                        continue;
                    }
                    let batches: Vec<Batch> = decode_batches(records, max_decoded)?;
                    let (records, end) = committed_records(batches, &aborted, committed);
                    out = Some(Fetched {
                        high_watermark,
                        last_stable,
                        log_start,
                        records,
                        end,
                    });
                }
            }
            out.ok_or(Fail::Proto("fetch response"))
        })
    }

    /// One produce request: the base offset, or the broker's error. A
    /// duplicate sequence number (an idempotent retry the broker already
    /// appended) counts as success with offset -1.
    pub(crate) fn produce_once(
        &mut self,
        topic: &str,
        partition: i32,
        txn: Option<&str>,
        batch: &[u8],
    ) -> KResult<i64> {
        let node = self.leader(topic, partition)?;
        let body = self.call(node, api::PRODUCE, |w| {
            w.nstr(txn)?;
            w.i16(-1).i32(30_000);
            w.array(1)?.str(topic)?;
            w.array(1)?.i32(partition).bytes(batch)?;
            Ok(())
        })?;
        let mut r = R::new(&body, false);
        let mut out = None;
        for _ in 0..r.array()? {
            r.str()?;
            for _ in 0..r.array()? {
                let p = r.i32()?;
                let code = r.i16()?;
                let base = r.i64()?;
                r.i64()?;
                r.i64()?;
                for _ in 0..r.array()? {
                    r.i32()?;
                    r.nstr()?;
                }
                let msg = r.nstr()?;
                if p != partition {
                    continue;
                }
                out = Some(match code {
                    0 => Ok(base),
                    46 => Ok(-1),
                    c => Err(Fail::Code(
                        c,
                        format!("produce {topic}/{p}: {}", msg.unwrap_or_default()),
                    )),
                });
            }
        }
        r.i32()?;
        out.ok_or(Fail::Proto("produce response"))?
    }

    // ------------------------------------------------------------------
    // Coordinators
    // ------------------------------------------------------------------

    pub(crate) fn coordinator(&mut self, kind: i8, key: &str) -> KResult<i32> {
        if let Some(&n) = self.coords.get(&(kind, key.to_string())) {
            return Ok(n);
        }
        let node = self.any()?;
        let body = self.call(node, api::FIND_COORDINATOR, |w| {
            w.str(key)?.i8(kind);
            Ok(())
        })?;
        let mut r = R::new(&body, false);
        r.i32()?;
        let code = r.i16()?;
        let msg = r.nstr()?;
        let id = r.i32()?;
        let host = r.str()?;
        let port = r.i32()?;
        if code != 0 {
            return Err(Fail::Code(
                code,
                format!("find coordinator: {}", msg.unwrap_or_default()),
            ));
        }
        let addr = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        if self.brokers.get(&id) != Some(&addr) {
            self.conns.remove(&id);
            self.brokers.insert(id, addr);
        }
        self.coords.insert((kind, key.to_string()), id);
        Ok(id)
    }

    /// A request to the coordinator of `key`, looking it up again when it
    /// moved. Not retried here.
    pub(crate) fn coord_call(
        &mut self,
        kind: i8,
        key: &str,
        api_key: i16,
        body: impl FnOnce(&mut W) -> KResult<()>,
    ) -> KResult<Vec<u8>> {
        let node = self.coordinator(kind, key)?;
        let out = self.call(node, api_key, body);
        if out.is_err() {
            self.coords.remove(&(kind, key.to_string()));
        }
        out
    }

    pub(crate) fn set_read_timeout(&mut self, kind: i8, key: &str, t: Duration) -> KResult<()> {
        let node = self.coordinator(kind, key)?;
        self.conn(node)?.set_read_timeout(t)
    }

    // ------------------------------------------------------------------
    // Group offsets
    // ------------------------------------------------------------------

    /// Committed offsets of `group` for `partitions` (stable: waits out
    /// pending transactional commits).
    pub(crate) fn offset_fetch(
        &mut self,
        group: &str,
        topic: &str,
        partitions: &[i32],
    ) -> KResult<BTreeMap<i32, Committed>> {
        self.retry(|c| {
            let body = c.coord_call(GROUP, group, api::OFFSET_FETCH, |w| {
                w.str(group)?;
                w.array(1)?.str(topic)?;
                w.array(partitions.len())?;
                for p in partitions {
                    w.i32(*p);
                }
                w.tags();
                w.bool(true).tags();
                Ok(())
            })?;
            let mut r = R::new(&body, true);
            r.i32()?;
            let mut out = BTreeMap::new();
            let mut first_err = 0;
            for _ in 0..r.array()? {
                r.str()?;
                for _ in 0..r.array()? {
                    let p = r.i32()?;
                    let offset = r.i64()?;
                    r.i32()?;
                    let metadata = r.nstr()?.unwrap_or_default();
                    let code = r.i16()?;
                    r.tags()?;
                    if code != 0 && first_err == 0 {
                        first_err = code;
                    }
                    if offset >= 0 {
                        out.insert(p, Committed { offset, metadata });
                    }
                }
                r.tags()?;
            }
            let code = r.i16()?;
            check_code(code, "offset fetch")?;
            check_code(first_err, "offset fetch")?;
            Ok(out)
        })
    }

    // ------------------------------------------------------------------
    // Group membership (classic protocol)
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn join_group(
        &mut self,
        group: &str,
        member_id: &str,
        instance_id: Option<&str>,
        session_timeout: Duration,
        rebalance_timeout: Duration,
        protocol: &str,
        metadata: &[u8],
    ) -> KResult<Joined> {
        let ms =
            |d: Duration| i32::try_from(d.as_millis()).map_err(|_| Fail::Local(Error::Overflow));
        let (st, rt) = (ms(session_timeout)?, ms(rebalance_timeout)?);
        self.set_read_timeout(GROUP, group, rebalance_timeout + Duration::from_secs(5))?;
        let body = self.coord_call(GROUP, group, api::JOIN_GROUP, |w| {
            w.str(group)?.i32(st).i32(rt).str(member_id)?;
            w.nstr(instance_id)?;
            w.str("consumer")?;
            w.array(1)?.str(protocol)?.bytes(metadata)?;
            Ok(())
        })?;
        let mut r = R::new(&body, false);
        r.i32()?;
        let code = r.i16()?;
        let generation = r.i32()?;
        let protocol_name = r.str()?;
        let leader = r.str()?;
        let me = r.str()?;
        let mut members = Vec::new();
        for _ in 0..r.array()? {
            let id = r.str()?;
            r.nstr()?;
            members.push((id, r.bytes()?.to_vec()));
        }
        if code == 79 {
            return Ok(Joined {
                generation: -1,
                member_id: me,
                leader: String::new(),
                protocol: protocol_name,
                members,
                retry: true,
            });
        }
        check_code(code, "join group")?;
        Ok(Joined {
            generation,
            member_id: me,
            leader,
            protocol: protocol_name,
            members,
            retry: false,
        })
    }

    pub(crate) fn sync_group(
        &mut self,
        group: &str,
        generation: i32,
        member_id: &str,
        instance_id: Option<&str>,
        assignments: &[(String, Vec<u8>)],
    ) -> KResult<Vec<u8>> {
        let body = self.coord_call(GROUP, group, api::SYNC_GROUP, |w| {
            w.str(group)?.i32(generation).str(member_id)?;
            w.nstr(instance_id)?;
            w.array(assignments.len())?;
            for (m, a) in assignments {
                w.str(m)?.bytes(a)?;
            }
            Ok(())
        })?;
        let mut r = R::new(&body, false);
        r.i32()?;
        check_code(r.i16()?, "sync group")?;
        Ok(r.bytes()?.to_vec())
    }

    /// `Ok(())`, or the broker's code (27 rebalance in progress, 22, 25).
    pub(crate) fn heartbeat(
        &mut self,
        group: &str,
        generation: i32,
        member_id: &str,
        instance_id: Option<&str>,
    ) -> KResult<()> {
        let body = self.coord_call(GROUP, group, api::HEARTBEAT, |w| {
            w.str(group)?.i32(generation).str(member_id)?;
            w.nstr(instance_id)?;
            Ok(())
        })?;
        let mut r = R::new(&body, false);
        r.i32()?;
        check_code(r.i16()?, "heartbeat")
    }

    pub(crate) fn leave_group(
        &mut self,
        group: &str,
        member_id: &str,
        instance_id: Option<&str>,
    ) -> KResult<()> {
        let body = self.coord_call(GROUP, group, api::LEAVE_GROUP, |w| {
            w.str(group)?;
            w.array(1)?.str(member_id)?;
            w.nstr(instance_id)?;
            Ok(())
        })?;
        let mut r = R::new(&body, false);
        r.i32()?;
        check_code(r.i16()?, "leave group")
    }

    // ------------------------------------------------------------------
    // Transactions
    // ------------------------------------------------------------------

    /// InitProducerId: (producer id, epoch). With a transactional id this
    /// fences every earlier incarnation and completes (aborts) its open
    /// transaction first.
    pub(crate) fn init_producer(
        &mut self,
        txn: Option<&str>,
        timeout: Duration,
    ) -> KResult<(i64, i16)> {
        let ms = i32::try_from(timeout.as_millis()).map_err(|_| Fail::Local(Error::Overflow))?;
        self.retry(|c| {
            let body = match txn {
                Some(t) => c.coord_call(TXN, t, api::INIT_PRODUCER_ID, |w| {
                    w.nstr(Some(t))?.i32(ms);
                    Ok(())
                })?,
                None => {
                    let node = c.any()?;
                    c.call(node, api::INIT_PRODUCER_ID, |w| {
                        w.nstr(None)?.i32(ms);
                        Ok(())
                    })?
                }
            };
            let mut r = R::new(&body, false);
            r.i32()?;
            let code = r.i16()?;
            let id = r.i64()?;
            let epoch = r.i16()?;
            check_code(code, "init producer id")?;
            Ok((id, epoch))
        })
    }

    pub(crate) fn add_partitions(
        &mut self,
        txn: &str,
        pid: (i64, i16),
        topic: &str,
        partition: i32,
    ) -> KResult<()> {
        self.retry(|c| {
            let body = c.coord_call(TXN, txn, api::ADD_PARTITIONS_TO_TXN, |w| {
                w.str(txn)?.i64(pid.0).i16(pid.1);
                w.array(1)?.str(topic)?;
                w.array(1)?.i32(partition);
                Ok(())
            })?;
            let mut r = R::new(&body, false);
            r.i32()?;
            for _ in 0..r.array()? {
                r.str()?;
                for _ in 0..r.array()? {
                    r.i32()?;
                    check_code(r.i16()?, "add partitions to txn")?;
                }
            }
            Ok(())
        })
    }

    pub(crate) fn add_offsets(&mut self, txn: &str, pid: (i64, i16), group: &str) -> KResult<()> {
        self.retry(|c| {
            let body = c.coord_call(TXN, txn, api::ADD_OFFSETS_TO_TXN, |w| {
                w.str(txn)?.i64(pid.0).i16(pid.1).str(group)?;
                Ok(())
            })?;
            let mut r = R::new(&body, false);
            r.i32()?;
            check_code(r.i16()?, "add offsets to txn")
        })
    }

    pub(crate) fn txn_offset_commit(
        &mut self,
        txn: &str,
        pid: (i64, i16),
        o: &GroupOffsets,
    ) -> KResult<()> {
        self.retry(|c| {
            let body = c.coord_call(GROUP, &o.group, api::TXN_OFFSET_COMMIT, |w| {
                w.str(txn)?.str(&o.group)?.i64(pid.0).i16(pid.1);
                w.i32(o.generation).str(&o.member_id)?;
                w.nstr(o.instance_id.as_deref())?;
                write_offsets(w, &o.offsets, true)?;
                w.tags();
                Ok(())
            })?;
            read_offset_errors(&body, true, "txn offset commit")
        })
    }

    pub(crate) fn end_txn(&mut self, txn: &str, pid: (i64, i16), commit: bool) -> KResult<()> {
        self.retry(|c| {
            let body = c.coord_call(TXN, txn, api::END_TXN, |w| {
                w.str(txn)?.i64(pid.0).i16(pid.1).bool(commit);
                Ok(())
            })?;
            let mut r = R::new(&body, false);
            r.i32()?;
            check_code(r.i16()?, "end txn")
        })
    }
}

/// A JoinGroup result.
pub(crate) struct Joined {
    pub(crate) generation: i32,
    pub(crate) member_id: String,
    pub(crate) leader: String,
    pub(crate) protocol: String,
    pub(crate) members: Vec<(String, Vec<u8>)>,
    /// MEMBER_ID_REQUIRED: join again with `member_id`.
    pub(crate) retry: bool,
}

fn check_code(code: i16, what: &str) -> KResult<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Fail::Code(code, what.to_string()))
    }
}

fn write_offsets(w: &mut W, offsets: &[(String, i32, i64, String)], flex: bool) -> KResult<()> {
    let mut by_topic: BTreeMap<&str, Vec<(i32, i64, &str)>> = BTreeMap::new();
    for (t, p, o, m) in offsets {
        by_topic.entry(t).or_default().push((*p, *o, m));
    }
    w.array(by_topic.len())?;
    for (t, parts) in by_topic {
        w.str(t)?;
        w.array(parts.len())?;
        for (p, o, m) in parts {
            w.i32(p).i64(o).i32(-1).nstr(Some(m))?;
            if flex {
                w.tags();
            }
        }
        if flex {
            w.tags();
        }
    }
    Ok(())
}

fn read_offset_errors(body: &[u8], flex: bool, what: &str) -> KResult<()> {
    let mut r = R::new(body, flex);
    r.i32()?;
    for _ in 0..r.array()? {
        r.str()?;
        for _ in 0..r.array()? {
            r.i32()?;
            check_code(r.i16()?, what)?;
            r.tags()?;
        }
        r.tags()?;
    }
    r.tags()
}

// ----------------------------------------------------------------------
// Producer
// ----------------------------------------------------------------------

/// Producer state: identity, per-partition sequence numbers and the open
/// transaction.
pub(crate) struct ProducerState {
    pub(crate) txn: Option<String>,
    pub(crate) pid: (i64, i16),
    pub(crate) seqs: BTreeMap<(String, i32), i32>,
    pub(crate) in_txn: bool,
    pub(crate) partitions: BTreeSet<(String, i32)>,
    pub(crate) groups: BTreeSet<String>,
    pub(crate) compression: KafkaCompression,
    pub(crate) fenced: bool,
}
impl ProducerState {
    pub(crate) fn init(
        client: &mut Client,
        txn: Option<String>,
        timeout: Duration,
        compression: KafkaCompression,
    ) -> KResult<Self> {
        let pid = client.init_producer(txn.as_deref(), timeout)?;
        Ok(Self {
            txn,
            pid,
            seqs: BTreeMap::new(),
            in_txn: false,
            partitions: BTreeSet::new(),
            groups: BTreeSet::new(),
            compression,
            fenced: false,
        })
    }

    fn guard(&self) -> KResult<()> {
        if self.fenced {
            return Err(Fail::Code(90, "producer".into()));
        }
        Ok(())
    }

    pub(crate) fn begin(&mut self) -> KResult<()> {
        self.guard()?;
        if self.txn.is_none() {
            return Err(Fail::Local(Error::Kafka("not a transactional producer")));
        }
        if self.in_txn {
            return Err(Fail::Local(Error::Kafka("transaction already open")));
        }
        self.in_txn = true;
        self.partitions.clear();
        self.groups.clear();
        Ok(())
    }

    /// Encode the next batch for a partition, registering the partition
    /// with the transaction first. The sequence advances: the batch must
    /// then be delivered (and may be retried byte-for-byte).
    pub(crate) fn prepare(
        &mut self,
        client: &mut Client,
        topic: &str,
        partition: i32,
        records: &[KafkaRecord],
    ) -> KResult<Vec<u8>> {
        self.guard()?;
        if self.txn.is_some() && !self.in_txn {
            return Err(Fail::Local(Error::Kafka("no open transaction")));
        }
        let key = (topic.to_string(), partition);
        if let Some(txn) = self.txn.clone()
            && !self.partitions.contains(&key)
        {
            self.fence_on(client.add_partitions(&txn, self.pid, topic, partition))?;
            self.partitions.insert(key.clone());
        }
        let seq = self.seqs.get(&key).copied().unwrap_or(0);
        let n = i32::try_from(records.len()).map_err(|_| Fail::Local(Error::Overflow))?;
        let batch = encode_batch(
            records,
            self.compression,
            ProducerStamp {
                id: self.pid.0,
                epoch: self.pid.1,
                sequence: seq,
                transactional: self.txn.is_some(),
            },
        )?;
        // Sequences wrap at i32::MAX (KIP-98).
        let next = if seq > i32::MAX - n {
            n - (i32::MAX - seq) - 1
        } else {
            seq + n
        };
        self.seqs.insert(key, next);
        Ok(batch)
    }

    /// Deliver a prepared batch, retrying the identical bytes: the broker
    /// deduplicates by (producer id, epoch, sequence), so a retry after a
    /// lost response never appends twice.
    pub(crate) fn deliver(
        &mut self,
        client: &mut Client,
        topic: &str,
        partition: i32,
        batch: &[u8],
    ) -> KResult<i64> {
        let txn = self.txn.clone();
        let r = client.retry(|c| c.produce_once(topic, partition, txn.as_deref(), batch));
        self.fence_on(r)
    }

    pub(crate) fn send_offsets(&mut self, client: &mut Client, o: &GroupOffsets) -> KResult<()> {
        self.guard()?;
        let txn = self
            .txn
            .clone()
            .ok_or(Fail::Local(Error::Kafka("not a transactional producer")))?;
        if !self.in_txn {
            return Err(Fail::Local(Error::Kafka("no open transaction")));
        }
        if !self.groups.contains(&o.group) {
            self.fence_on(client.add_offsets(&txn, self.pid, &o.group))?;
            self.groups.insert(o.group.clone());
        }
        let r = client.txn_offset_commit(&txn, self.pid, o);
        self.fence_on(r)
    }

    pub(crate) fn end(&mut self, client: &mut Client, commit: bool) -> KResult<()> {
        self.guard()?;
        let txn = self
            .txn
            .clone()
            .ok_or(Fail::Local(Error::Kafka("not a transactional producer")))?;
        if !self.in_txn {
            return Err(Fail::Local(Error::Kafka("no open transaction")));
        }
        if self.partitions.is_empty() && self.groups.is_empty() {
            // Nothing was registered: the coordinator has no transaction.
            self.in_txn = false;
            return Ok(());
        }
        let r = client.end_txn(&txn, self.pid, commit);
        let r = self.fence_on(r);
        if r.is_ok() {
            self.in_txn = false;
        }
        r
    }

    fn fence_on<T>(&mut self, r: KResult<T>) -> KResult<T> {
        if let Err(e) = &r
            && e.fenced()
        {
            self.fenced = true;
        }
        r
    }
}
