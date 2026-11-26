//! Cluster-level Kafka client over [`super::wire`]: broker metadata and
//! leaders, coordinator lookup, retries, the data and admin APIs, an
//! idempotent / transactional producer and classic consumer-group
//! membership.
use super::codec::{self, Head};
use super::wire::{
    Conn, ConnSettings, Fail, KResult, KafkaCompression, KafkaRecord, ProducerStamp, api,
    encode_batch,
};
use crate::Error;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// Coordinator kinds (FindCoordinator key_type).
pub(crate) const GROUP: i8 = 0;
pub(crate) const TXN: i8 = 1;

/// One partition's fetch result.
#[derive(Debug)]
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
        encode: impl FnOnce(&Head) -> KResult<Vec<u8>>,
    ) -> KResult<Vec<u8>> {
        let out = self.conn(node)?.call(key, encode);
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
        let body = self.call(node, api::METADATA, |h| codec::metadata_request(h, topic))?;
        let m = codec::metadata(&body, topic);
        // Leaders of the topic count as they are read, as before an error.
        for (index, leader) in m.leaders {
            self.leaders.insert((topic.to_string(), index), leader);
        }
        m.status?;
        let brokers: BTreeMap<i32, String> = m.brokers.into_iter().collect();
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
            let body = c.call(node, api::CREATE_TOPICS, |h| {
                codec::create_topic_request(h, topic, partitions)
            })?;
            codec::create_topic(&body, topic)
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
            let body = c.call(node, api::DELETE_RECORDS, |h| {
                codec::delete_records_request(h, topic, partition, offset)
            })?;
            codec::delete_records(&body)
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
            let body = c.call(node, api::LIST_OFFSETS, |h| {
                codec::list_offsets_request(h, topic, partition, ts, committed)
            })?;
            codec::list_offsets(&body, partition)
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
            let body = c.call(node, api::FETCH, |h| {
                codec::fetch_request(h, topic, partition, offset, max_bytes, committed)
            })?;
            codec::fetch(&body, partition, max_decoded, committed)
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
        let body = self.call(node, api::PRODUCE, |h| {
            codec::produce_request(h, txn, topic, partition, batch)
        })?;
        codec::produce(&body, topic, partition)
    }

    // ------------------------------------------------------------------
    // Coordinators
    // ------------------------------------------------------------------

    pub(crate) fn coordinator(&mut self, kind: i8, key: &str) -> KResult<i32> {
        if let Some(&n) = self.coords.get(&(kind, key.to_string())) {
            return Ok(n);
        }
        let node = self.any()?;
        let body = self.call(node, api::FIND_COORDINATOR, |h| {
            codec::find_coordinator_request(h, key, kind)
        })?;
        let (id, addr) = codec::find_coordinator(&body)?;
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
        encode: impl FnOnce(&Head) -> KResult<Vec<u8>>,
    ) -> KResult<Vec<u8>> {
        let node = self.coordinator(kind, key)?;
        let out = self.call(node, api_key, encode);
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
            let body = c.coord_call(GROUP, group, api::OFFSET_FETCH, |h| {
                codec::offset_fetch_request(h, group, topic, partitions)
            })?;
            codec::offset_fetch(&body)
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
        let body = self.coord_call(GROUP, group, api::JOIN_GROUP, |h| {
            codec::join_group_request(h, group, st, rt, member_id, instance_id, protocol, metadata)
        })?;
        codec::join_group(&body)
    }

    pub(crate) fn sync_group(
        &mut self,
        group: &str,
        generation: i32,
        member_id: &str,
        instance_id: Option<&str>,
        assignments: &[(String, Vec<u8>)],
    ) -> KResult<Vec<u8>> {
        let body = self.coord_call(GROUP, group, api::SYNC_GROUP, |h| {
            codec::sync_group_request(h, group, generation, member_id, instance_id, assignments)
        })?;
        codec::sync_group(&body)
    }

    /// `Ok(())`, or the broker's code (27 rebalance in progress, 22, 25).
    pub(crate) fn heartbeat(
        &mut self,
        group: &str,
        generation: i32,
        member_id: &str,
        instance_id: Option<&str>,
    ) -> KResult<()> {
        let body = self.coord_call(GROUP, group, api::HEARTBEAT, |h| {
            codec::heartbeat_request(h, group, generation, member_id, instance_id)
        })?;
        codec::code(&body, "heartbeat")
    }

    pub(crate) fn leave_group(
        &mut self,
        group: &str,
        member_id: &str,
        instance_id: Option<&str>,
    ) -> KResult<()> {
        let body = self.coord_call(GROUP, group, api::LEAVE_GROUP, |h| {
            codec::leave_group_request(h, group, member_id, instance_id)
        })?;
        codec::code(&body, "leave group")
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
                Some(t) => c.coord_call(TXN, t, api::INIT_PRODUCER_ID, |h| {
                    codec::init_producer_request(h, Some(t), ms)
                })?,
                None => {
                    let node = c.any()?;
                    c.call(node, api::INIT_PRODUCER_ID, |h| {
                        codec::init_producer_request(h, None, ms)
                    })?
                }
            };
            codec::init_producer(&body)
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
            let body = c.coord_call(TXN, txn, api::ADD_PARTITIONS_TO_TXN, |h| {
                codec::add_partitions_request(h, txn, pid, topic, partition)
            })?;
            codec::add_partitions(&body)
        })
    }

    pub(crate) fn add_offsets(&mut self, txn: &str, pid: (i64, i16), group: &str) -> KResult<()> {
        self.retry(|c| {
            let body = c.coord_call(TXN, txn, api::ADD_OFFSETS_TO_TXN, |h| {
                codec::add_offsets_request(h, txn, pid, group)
            })?;
            codec::code(&body, "add offsets to txn")
        })
    }

    pub(crate) fn txn_offset_commit(
        &mut self,
        txn: &str,
        pid: (i64, i16),
        o: &GroupOffsets,
    ) -> KResult<()> {
        self.retry(|c| {
            let body = c.coord_call(GROUP, &o.group, api::TXN_OFFSET_COMMIT, |h| {
                codec::txn_offset_commit_request(
                    h,
                    txn,
                    &o.group,
                    pid,
                    o.generation,
                    &o.member_id,
                    o.instance_id.as_deref(),
                    &o.offsets,
                )
            })?;
            codec::offset_errors(&body, true, "txn offset commit")
        })
    }

    pub(crate) fn end_txn(&mut self, txn: &str, pid: (i64, i16), commit: bool) -> KResult<()> {
        self.retry(|c| {
            let body = c.coord_call(TXN, txn, api::END_TXN, |h| {
                codec::end_txn_request(h, txn, pid, commit)
            })?;
            codec::code(&body, "end txn")
        })
    }
}

/// A JoinGroup result.
#[derive(Debug)]
pub(crate) struct Joined {
    pub(crate) generation: i32,
    pub(crate) member_id: String,
    pub(crate) leader: String,
    pub(crate) protocol: String,
    pub(crate) members: Vec<(String, Vec<u8>)>,
    /// MEMBER_ID_REQUIRED: join again with `member_id`.
    pub(crate) retry: bool,
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
