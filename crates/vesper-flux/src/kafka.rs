//! Kafka source, sink, producer and consumer group for the Flux cluster,
//! over a small synchronous wire client of its own (`kafka_wire.rs`,
//! `kafka_client.rs`): TLS through rustls, SASL PLAIN and SCRAM through
//! `rsasl`, v2 record batches with gzip/snappy/lz4/zstd, read-committed
//! fetches, and the idempotent-producer, transaction and group APIs.
//!
//! # Source: offsets are part of the checkpoint
//!
//! A [`KafkaSource`] reads CDC records (the 32-byte [`CdcHeader`] followed by
//! `width` little-endian i64 cells, as in `Executor::ingest_cdc`) from one or
//! more Kafka partitions, each bound to a [`CdcPolicy`] binding. Epochs are
//! cut by [`KafkaSource::poll`]: it fetches the records between the end of
//! the last epoch and the partitions' current last stable offsets (at most
//! `max_records_per_epoch` per partition), decodes and validates them, and
//! then appends the new end offsets and next LSNs to a durable *epoch index*
//! file (one checksummed, fsynced frame per epoch) before the epoch is
//! visible. Epoch `e` therefore always means the same Kafka offset ranges, so
//! the cluster can replay it after any fault, and the sink's commit record
//! (`after_epoch`) names a fixed position in every partition: the Kafka
//! offsets are committed atomically with the outputs and the checkpoint
//! handle, in the same Quarry transaction, as one epoch number.
//! [`KafkaSource::committed`] maps a commit back to offsets and
//! [`KafkaSource::release`] deletes the records a commit no longer needs.
//! Fetches are read-committed by default: records of aborted transactions
//! and transaction markers are never seen.
//!
//! LSNs must be consecutive per binding. A record whose LSN is below the
//! next expected one is a producer retry and is dropped (counted in
//! [`KafkaSourceStats::duplicates`]); a gap is a typed error and the epoch
//! is not cut. A *malformed* record (wrong length, foreign partition id or
//! schema, reserved flags, bad weight, no value) is a typed error by default;
//! with [`KafkaSourceLimits::dead_letter`] set it is copied to the
//! dead-letter topic instead (key = source topic, partition and offset) and
//! skipped, so it never blocks the partition.
//!
//! # Sink: commit, then publish each epoch in one Kafka transaction
//!
//! A [`KafkaSink`] wraps a [`CommitSink`] (normally a `QuarrySink`).
//! Outputs are committed there first (with the offsets and the checkpoint);
//! only committed epochs are then produced to the output topic, so nothing a
//! reader saw is ever rolled back. Each epoch is published in its own Kafka
//! transaction under the sink's transactional id: its records, plus a
//! *progress* offset commit (group `<transactional id>.progress`, one offset
//! per output partition: the next output LSN, with the epoch and the
//! circuit tag as metadata) and, when a [`GroupOffsetSource`] is attached,
//! the consumer group's input offsets for that epoch. The transaction
//! commits all of them or none. A new sink incarnation calls
//! InitProducerId, which fences every earlier producer with the same id (a
//! request it still has in flight is rejected with a fenced-epoch error)
//! and aborts its open transaction; it then reads the progress offsets with
//! `require_stable` and publishes exactly the epochs after them. Exactly-once
//! publication therefore rests on broker-side fencing, not on a time window.
//! Readers must read committed.
use super::cdc::CdcPolicy;
use super::client::{Client, ProducerState};
use super::sink::{CommitRecord, CommitSink};
use super::wire::{ConnSettings, Fail, KResult};
use super::{Circuit, EpochSource, TickInput};
use crate::zset::Accumulator;
use crate::{Budget, CdcHeader, Error, Limits, Row, ZSet};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

pub use super::client::GroupOffsets;
pub use super::wire::{
    KafkaCompression, KafkaRecord, KafkaSasl, KafkaSecurity, KafkaTls, SaslMechanism,
};

/// How to reach the brokers.
#[derive(Clone, Debug)]
pub struct KafkaConfig {
    /// `host:port` bootstrap brokers.
    pub bootstrap: Vec<String>,
    pub client_id: String,
    /// TLS and SASL for every connection.
    pub security: KafkaSecurity,
    /// How long one operation keeps retrying a broker that is unreachable,
    /// not yet leader, or a coordinator that is loading or moved.
    pub retry_for: Duration,
    /// Socket read/write timeout of one request.
    pub request_timeout: Duration,
    /// Upper bound of one fetch response.
    pub fetch_bytes: i32,
    /// Codec of produced batches.
    pub compression: KafkaCompression,
    /// Read only committed transactional records (the default).
    pub read_committed: bool,
    /// Transaction timeout requested at InitProducerId.
    pub transaction_timeout: Duration,
}
impl KafkaConfig {
    pub fn new(bootstrap: impl Into<String>) -> Self {
        Self {
            bootstrap: vec![bootstrap.into()],
            client_id: "vesper-flux".into(),
            security: KafkaSecurity::default(),
            retry_for: Duration::from_secs(20),
            request_timeout: Duration::from_secs(30),
            fetch_bytes: 1 << 20,
            compression: KafkaCompression::None,
            read_committed: true,
            transaction_timeout: Duration::from_secs(60),
        }
    }
}

/// One client of a Kafka cluster. Every call blocks until the broker
/// answered (or the retry deadline passed).
pub struct KafkaConnection {
    client: RefCell<Client>,
    cfg: KafkaConfig,
}
impl std::fmt::Debug for KafkaConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KafkaConnection")
            .field("bootstrap", &self.cfg.bootstrap)
            .finish_non_exhaustive()
    }
}

impl KafkaConnection {
    /// Connect (and authenticate) to a bootstrap broker.
    pub fn connect(cfg: &KafkaConfig) -> Result<Self, Error> {
        if cfg.bootstrap.is_empty() || cfg.fetch_bytes < 1024 || cfg.request_timeout.is_zero() {
            return Err(Error::Kafka("configuration"));
        }
        let max = usize::try_from(cfg.fetch_bytes)
            .map_err(|_| Error::Kafka("configuration"))?
            .saturating_mul(4)
            .max(16 << 20);
        let settings = ConnSettings::new(&cfg.client_id, cfg.request_timeout, max, &cfg.security)?;
        let conn = Self {
            client: RefCell::new(Client::new(settings, cfg.bootstrap.clone(), cfg.retry_for)),
            cfg: cfg.clone(),
        };
        conn.run(|c| c.reachable())?;
        Ok(conn)
    }

    pub fn config(&self) -> &KafkaConfig {
        &self.cfg
    }

    /// The client's message for the last failed operation.
    pub fn last_error(&self) -> Option<String> {
        self.client.borrow().last_error.clone()
    }

    pub(crate) fn run<T>(&self, f: impl FnOnce(&mut Client) -> KResult<T>) -> Result<T, Error> {
        let mut c = self.client.borrow_mut();
        f(&mut c).map_err(|e| {
            c.note(&e);
            e.error()
        })
    }

    pub(crate) fn raw<T>(&self, f: impl FnOnce(&mut Client) -> KResult<T>) -> KResult<T> {
        let mut c = self.client.borrow_mut();
        let r = f(&mut c);
        if let Err(e) = &r {
            c.note(e);
        }
        r
    }

    /// Create a topic with replication factor 1 and wait for its leaders.
    pub fn create_topic(&self, topic: &str, partitions: i32) -> Result<(), Error> {
        if !(1..=10_000).contains(&partitions) {
            return Err(Error::Kafka("partitions"));
        }
        self.run(|c| c.create_topic(topic, partitions))
    }

    /// Number of partitions of a topic.
    pub fn partitions(&self, topic: &str) -> Result<i32, Error> {
        self.run(|c| c.partitions(topic))
    }

    /// Earliest retained and end offset of a partition: the last stable
    /// offset when reading committed, else the high watermark.
    pub fn offsets(&self, topic: &str, partition: i32) -> Result<(i64, i64), Error> {
        let rc = self.cfg.read_committed;
        let earliest = self.run(|c| c.list_offset(topic, partition, -2, rc))?;
        let latest = self.run(|c| c.list_offset(topic, partition, -1, rc))?;
        if earliest < 0 || latest < earliest {
            return Err(Error::Kafka("offsets"));
        }
        Ok((earliest, latest))
    }

    /// Append records as one batch without a producer id (acks = all, with
    /// client retries): at-least-once, a lost response can duplicate the
    /// batch. Returns the base offset. Use [`KafkaProducer`] for
    /// idempotent or transactional appends.
    pub fn produce(
        &self,
        topic: &str,
        partition: i32,
        records: Vec<KafkaRecord>,
    ) -> Result<i64, Error> {
        let batch = self.run(|_| {
            super::wire::encode_batch(
                &records,
                self.cfg.compression,
                super::wire::ProducerStamp::NONE,
            )
        })?;
        self.run(|c| c.retry(|c| c.produce_once(topic, partition, None, &batch)))
    }

    /// Every record with offset in `from..to` that the configured isolation
    /// level shows, in offset order.
    pub fn fetch(
        &self,
        topic: &str,
        partition: i32,
        from: i64,
        to: i64,
    ) -> Result<Vec<(i64, KafkaRecord)>, Error> {
        let mut out = Vec::new();
        let mut at = from;
        let rc = self.cfg.read_committed;
        while at < to {
            let f = self.run(|c| c.fetch(topic, partition, at, self.cfg.fetch_bytes, rc))?;
            if f.log_start > at {
                return Err(Error::Kafka("offset out of range"));
            }
            for (o, r) in f.records {
                if o < at {
                    continue;
                }
                if o >= to {
                    break;
                }
                out.push((o, r));
            }
            match f.end {
                Some(end) if end > at => at = end,
                _ => {
                    let visible = if rc { f.last_stable } else { f.high_watermark };
                    if visible < to {
                        return Err(Error::Kafka("records missing below the end offset"));
                    }
                    // Offsets in `at..to` hold no batch we can see.
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Delete every record below `offset`.
    pub fn delete_before(&self, topic: &str, partition: i32, offset: i64) -> Result<(), Error> {
        self.run(|c| c.delete_records(topic, partition, offset))
    }

    /// Committed offsets of a consumer group (stable: waits for pending
    /// transactional offset commits): partition to (offset, metadata).
    pub fn group_offsets(
        &self,
        group: &str,
        topic: &str,
        partitions: &[i32],
    ) -> Result<BTreeMap<i32, (i64, String)>, Error> {
        let got = self.run(|c| c.offset_fetch(group, topic, partitions))?;
        Ok(got
            .into_iter()
            .map(|(p, c)| (p, (c.offset, c.metadata)))
            .collect())
    }
}

// ----------------------------------------------------------------------
// Producer
// ----------------------------------------------------------------------

/// An idempotent or transactional producer.
///
/// Every batch carries the producer id, epoch and per-partition sequence
/// number, so the broker appends it at most once however often it is
/// resent. A transactional producer additionally groups batches and offset
/// commits into transactions; creating one with a transactional id fences
/// every earlier producer with that id.
pub struct KafkaProducer {
    conn: KafkaConnection,
    state: ProducerState,
}
impl std::fmt::Debug for KafkaProducer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KafkaProducer")
            .field("transactional_id", &self.state.txn)
            .field("producer", &self.state.pid)
            .finish_non_exhaustive()
    }
}

/// A produce request encoded (with its sequence number) but not yet sent:
/// fault injection for delayed or duplicated in-flight requests.
#[derive(Clone, Debug)]
pub struct HeldProduce {
    topic: String,
    partition: i32,
    txn: Option<String>,
    batch: Vec<u8>,
}
impl HeldProduce {
    /// Send the request once, now, over `conn`: the base offset, `-1` if
    /// the broker recognised it as a duplicate, or its error.
    pub fn deliver(&self, conn: &KafkaConnection) -> Result<i64, Error> {
        conn.run(|c| c.produce_once(&self.topic, self.partition, self.txn.as_deref(), &self.batch))
    }
}

impl KafkaProducer {
    /// An idempotent producer (no transactions).
    pub fn idempotent(conn: KafkaConnection) -> Result<Self, Error> {
        let (timeout, codec) = (conn.cfg.transaction_timeout, conn.cfg.compression);
        let state = conn.run(|c| ProducerState::init(c, None, timeout, codec))?;
        Ok(Self { conn, state })
    }
    /// A transactional producer. Fences every earlier producer with
    /// `transactional_id` and aborts the transaction it left open.
    pub fn transactional(conn: KafkaConnection, transactional_id: &str) -> Result<Self, Error> {
        if transactional_id.is_empty() {
            return Err(Error::Kafka("transactional id"));
        }
        let (timeout, codec) = (conn.cfg.transaction_timeout, conn.cfg.compression);
        let state =
            conn.run(|c| ProducerState::init(c, Some(transactional_id.into()), timeout, codec))?;
        Ok(Self { conn, state })
    }
    /// Producer id and epoch.
    pub fn id(&self) -> (i64, i16) {
        self.state.pid
    }
    pub fn connection(&self) -> &KafkaConnection {
        &self.conn
    }
    /// Whether the broker reported this producer fenced (it can do nothing
    /// more).
    pub fn fenced(&self) -> bool {
        self.state.fenced
    }
    pub fn begin(&mut self) -> Result<(), Error> {
        self.state.begin().map_err(|e| e.error())
    }
    /// Append `records` as one batch; returns the base offset (`-1` when
    /// the broker already held it).
    pub fn send(
        &mut self,
        topic: &str,
        partition: i32,
        records: &[KafkaRecord],
    ) -> Result<i64, Error> {
        let state = &mut self.state;
        self.conn.run(|c| {
            let batch = state.prepare(c, topic, partition, records)?;
            state.deliver(c, topic, partition, &batch)
        })
    }
    /// Encode the next batch (registering the partition with the open
    /// transaction) without sending it.
    pub fn hold(
        &mut self,
        topic: &str,
        partition: i32,
        records: &[KafkaRecord],
    ) -> Result<HeldProduce, Error> {
        let state = &mut self.state;
        let batch = self
            .conn
            .run(|c| state.prepare(c, topic, partition, records))?;
        Ok(HeldProduce {
            topic: topic.into(),
            partition,
            txn: self.state.txn.clone(),
            batch,
        })
    }
    /// Commit consumer-group offsets as part of the open transaction.
    pub fn send_offsets(&mut self, offsets: &GroupOffsets) -> Result<(), Error> {
        let state = &mut self.state;
        self.conn.run(|c| state.send_offsets(c, offsets))
    }
    pub fn commit(&mut self) -> Result<(), Error> {
        let state = &mut self.state;
        self.conn.run(|c| state.end(c, true))
    }
    pub fn abort(&mut self) -> Result<(), Error> {
        let state = &mut self.state;
        self.conn.run(|c| state.end(c, false))
    }
}

/// A Kafka record value in the Flux CDC format.
pub fn cdc_value(header: CdcHeader, row: &[i64]) -> Vec<u8> {
    let mut v = Vec::with_capacity(CdcHeader::BYTES + 8 * row.len());
    v.extend_from_slice(&header.encode());
    for c in row {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v
}
/// A Kafka record holding `value`, no key or headers.
pub fn kafka_record(value: Vec<u8>) -> KafkaRecord {
    KafkaRecord {
        value: Some(value),
        ..KafkaRecord::default()
    }
}
fn decode_value(value: Option<&Vec<u8>>, width: usize) -> Result<(CdcHeader, Row), Error> {
    let bytes = value.ok_or(Error::Cdc("record without value"))?;
    let len = width
        .checked_mul(8)
        .and_then(|n| n.checked_add(CdcHeader::BYTES))
        .ok_or(Error::Overflow)?;
    if bytes.len() != len {
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

// ----------------------------------------------------------------------
// Durable epoch index
// ----------------------------------------------------------------------

const INDEX_MAGIC: &[u8; 8] = b"VFLUXKX1";
const INDEX_VERSION: u16 = 1;
const SUM: usize = 32;

/// Position of one binding at an epoch boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KafkaPosition {
    pub topic: String,
    pub partition: i32,
    /// Next Kafka offset to read.
    pub offset: i64,
    /// Next CDC LSN expected.
    pub next_lsn: u64,
}

/// Fixed-size frames: epoch, then (end offset, next LSN) per binding, then
/// a BLAKE3 sum. Single writer; every append is synced.
struct EpochIndex {
    file: File,
    count: usize,
    header_len: u64,
    initial: Vec<(i64, u64)>,
    frames: Vec<Vec<(i64, u64)>>,
}
fn io(_: std::io::Error) -> Error {
    Error::Storage("io")
}
fn index_sum(domain: &[u8], bytes: &[u8]) -> [u8; SUM] {
    let mut h = blake3::Hasher::new();
    h.update(domain);
    h.update(bytes);
    *h.finalize().as_bytes()
}
impl EpochIndex {
    fn frame_len(&self) -> u64 {
        (8 + 16 * self.count + SUM) as u64
    }
    fn header(fingerprint: &[u8; 32], initial: &[(i64, u64)]) -> Result<Vec<u8>, Error> {
        let mut h = Vec::new();
        h.extend_from_slice(INDEX_MAGIC);
        h.extend_from_slice(&INDEX_VERSION.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(
            &u32::try_from(initial.len())
                .map_err(|_| Error::Overflow)?
                .to_le_bytes(),
        );
        h.extend_from_slice(fingerprint);
        for (o, l) in initial {
            h.extend_from_slice(&o.to_le_bytes());
            h.extend_from_slice(&l.to_le_bytes());
        }
        let sum = index_sum(b"vesper.flux.kafka-index.header/1", &h);
        h.extend_from_slice(&sum);
        Ok(h)
    }
    fn create(
        path: &Path,
        fingerprint: &[u8; 32],
        initial: Vec<(i64, u64)>,
    ) -> Result<Self, Error> {
        let header = Self::header(fingerprint, &initial)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(io)?;
        file.write_all(&header).map_err(io)?;
        file.sync_all().map_err(io)?;
        sync_dir(path)?;
        Ok(Self {
            file,
            count: initial.len(),
            header_len: header.len() as u64,
            initial,
            frames: Vec::new(),
        })
    }
    /// Returns the index and the bytes of a torn final append it cut off.
    fn open(
        path: &Path,
        fingerprint: &[u8; 32],
        count: usize,
        max_epochs: u64,
    ) -> Result<(Self, u64), Error> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(io)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(io)?;
        let fixed = 8 + 2 + 2 + 4 + 32;
        let header_len = fixed + 16 * count + SUM;
        if bytes.len() < header_len {
            return Err(Error::Storage("kafka index header"));
        }
        let mut initial = Vec::with_capacity(count);
        for i in 0..count {
            let at = fixed + 16 * i;
            initial.push((
                i64::from_le_bytes(bytes[at..at + 8].try_into().map_err(|_| Error::Overflow)?),
                u64::from_le_bytes(
                    bytes[at + 8..at + 16]
                        .try_into()
                        .map_err(|_| Error::Overflow)?,
                ),
            ));
        }
        if Self::header(fingerprint, &initial)? != bytes[..header_len] {
            return Err(Error::Storage("kafka index header"));
        }
        let mut index = Self {
            file,
            count,
            header_len: header_len as u64,
            initial,
            frames: Vec::new(),
        };
        let frame_len = index.frame_len() as usize;
        let body = &bytes[header_len..];
        let full = body.len() / frame_len;
        let mut keep = 0;
        for i in 0..full {
            let frame = &body[i * frame_len..(i + 1) * frame_len];
            match index.parse_frame(frame, i as u64) {
                Ok(f) => {
                    if index.frames.len() as u64 >= max_epochs {
                        return Err(Error::Limit("kafka epochs"));
                    }
                    index.frames.push(f);
                    keep = i + 1;
                }
                // A final frame failing its checksum is a torn append; a bad
                // frame before the end, or a checksummed frame with invalid
                // contents, is corruption.
                Err(Error::Storage("kafka index checksum"))
                    if i + 1 == full && body.len() == full * frame_len =>
                {
                    break;
                }
                Err(Error::Storage("kafka index checksum")) => {
                    return Err(Error::Storage("corrupt kafka index"));
                }
                Err(e) => return Err(e),
            }
        }
        let end = header_len + keep * frame_len;
        let torn = (bytes.len() - end) as u64;
        if torn > 0 {
            index.file.set_len(end as u64).map_err(io)?;
            index.file.sync_all().map_err(io)?;
        }
        Ok((index, torn))
    }
    fn parse_frame(&self, frame: &[u8], epoch: u64) -> Result<Vec<(i64, u64)>, Error> {
        let (body, sum) = frame.split_at(frame.len() - SUM);
        if index_sum(b"vesper.flux.kafka-index.frame/1", body) != sum {
            return Err(Error::Storage("kafka index checksum"));
        }
        if u64::from_le_bytes(body[..8].try_into().map_err(|_| Error::Overflow)?) != epoch {
            return Err(Error::Storage("corrupt kafka index"));
        }
        let previous = match epoch.checked_sub(1) {
            None => &self.initial,
            Some(p) => self
                .frames
                .get(usize::try_from(p).map_err(|_| Error::Overflow)?)
                .ok_or(Error::Storage("corrupt kafka index"))?,
        };
        let mut out = Vec::with_capacity(self.count);
        for (i, &(po, pl)) in previous.iter().enumerate().take(self.count) {
            let at = 8 + 16 * i;
            let o = i64::from_le_bytes(body[at..at + 8].try_into().map_err(|_| Error::Overflow)?);
            let l = u64::from_le_bytes(
                body[at + 8..at + 16]
                    .try_into()
                    .map_err(|_| Error::Overflow)?,
            );
            if o < po || l < pl {
                return Err(Error::Storage("corrupt kafka index"));
            }
            out.push((o, l));
        }
        Ok(out)
    }
    fn append(&mut self, ends: &[(i64, u64)]) -> Result<u64, Error> {
        let epoch = self.frames.len() as u64;
        let mut body = Vec::with_capacity(self.frame_len() as usize);
        body.extend_from_slice(&epoch.to_le_bytes());
        for (o, l) in ends {
            body.extend_from_slice(&o.to_le_bytes());
            body.extend_from_slice(&l.to_le_bytes());
        }
        let sum = index_sum(b"vesper.flux.kafka-index.frame/1", &body);
        body.extend_from_slice(&sum);
        let at = self.header_len + epoch * self.frame_len();
        self.file.seek(SeekFrom::Start(at)).map_err(io)?;
        self.file.write_all(&body).map_err(io)?;
        self.file.sync_all().map_err(io)?;
        self.frames.push(ends.to_vec());
        Ok(epoch)
    }
    /// Positions before `epoch` (the end of `epoch - 1`).
    fn start_of(&self, epoch: u64) -> Result<&[(i64, u64)], Error> {
        match epoch.checked_sub(1) {
            None => Ok(&self.initial),
            Some(p) => self
                .frames
                .get(usize::try_from(p).map_err(|_| Error::Overflow)?)
                .map(Vec::as_slice)
                .ok_or(Error::InputCount),
        }
    }
}
#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        let dir = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        File::open(dir).and_then(|d| d.sync_all()).map_err(io)?;
    }
    Ok(())
}
#[cfg(not(unix))]
fn sync_dir(_: &Path) -> Result<(), Error> {
    Ok(())
}

// ----------------------------------------------------------------------
// Source
// ----------------------------------------------------------------------

/// The Kafka partition carrying one [`CdcPolicy`] binding (same order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KafkaBinding {
    pub topic: String,
    pub partition: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KafkaSourceLimits {
    /// Records cut into one epoch from one partition (1..=65536).
    pub max_records_per_epoch: usize,
    pub max_epochs: u64,
    /// Decoded epochs kept in memory for workers reading the same epoch.
    pub cache_epochs: usize,
    /// Topic (partition 0) that receives malformed records instead of
    /// failing the epoch. `None`: a malformed record is a typed error.
    pub dead_letter: Option<String>,
}
impl Default for KafkaSourceLimits {
    fn default() -> Self {
        Self {
            max_records_per_epoch: 512,
            max_epochs: 1 << 20,
            cache_epochs: 16,
            dead_letter: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KafkaSourceStats {
    /// Records fetched and decoded (including replays).
    pub fetched: u64,
    /// Records dropped as producer retries (LSN below the expected one).
    pub duplicates: u64,
    /// Epochs decoded from the broker (not served from the cache).
    pub decoded: u64,
    /// Malformed records copied to the dead-letter topic (at the first cut;
    /// replays skip them again without copying).
    pub dead_lettered: u64,
}

/// A replayable [`EpochSource`] over Kafka partitions (see the module docs).
pub struct KafkaSource {
    conn: KafkaConnection,
    policy: CdcPolicy,
    topics: Vec<KafkaBinding>,
    widths: Vec<usize>,
    limits: Limits,
    source_limits: KafkaSourceLimits,
    index: EpochIndex,
    cache: RefCell<BTreeMap<u64, TickInput>>,
    stats: Cell<KafkaSourceStats>,
}
impl std::fmt::Debug for KafkaSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KafkaSource")
            .field("topics", &self.topics)
            .field("epochs", &self.index.frames.len())
            .finish_non_exhaustive()
    }
}

fn binding_fingerprint(policy: &CdcPolicy, topics: &[KafkaBinding], widths: &[usize]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"vesper.flux.kafka-source/1");
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
        h.update(&[u8::from(b.weights == super::cdc::WeightPolicy::Unit)]);
    }
    *h.finalize().as_bytes()
}

impl KafkaSource {
    /// Open the source. A missing index is created, starting every binding
    /// at its partition's earliest retained offset and `first_lsn`; an
    /// existing one is verified (a torn final append is cut off) and must
    /// match the bindings. Returns the source and the torn bytes removed.
    pub fn open(
        conn: KafkaConnection,
        circuit: &Circuit,
        policy: CdcPolicy,
        topics: Vec<KafkaBinding>,
        index: impl AsRef<Path>,
        source_limits: KafkaSourceLimits,
    ) -> Result<(Self, u64), Error> {
        policy.validate(circuit)?;
        if topics.len() != policy.bindings.len()
            || source_limits.dead_letter.as_deref() == Some("")
            || source_limits.max_records_per_epoch == 0
            || source_limits.max_records_per_epoch > 65_536
            || source_limits.max_records_per_epoch > circuit.limits.max_input_entries
            || source_limits.max_epochs == 0
        {
            return Err(Error::Limit("kafka source"));
        }
        for (i, t) in topics.iter().enumerate() {
            if t.partition < 0 || topics[..i].contains(t) {
                return Err(Error::Kafka("bindings"));
            }
        }
        let widths = circuit.inputs.clone();
        let fp = binding_fingerprint(&policy, &topics, &widths);
        let path = index.as_ref();
        let (index, torn) = if path.exists() {
            EpochIndex::open(path, &fp, topics.len(), source_limits.max_epochs)?
        } else {
            let mut initial = Vec::with_capacity(topics.len());
            for (t, b) in topics.iter().zip(&policy.bindings) {
                let (earliest, _) = conn.offsets(&t.topic, t.partition)?;
                initial.push((earliest, b.first_lsn));
            }
            (EpochIndex::create(path, &fp, initial)?, 0)
        };
        Ok((
            Self {
                conn,
                policy,
                topics,
                widths,
                limits: circuit.limits,
                source_limits,
                index,
                cache: RefCell::new(BTreeMap::new()),
                stats: Cell::new(KafkaSourceStats::default()),
            },
            torn,
        ))
    }

    pub fn connection(&self) -> &KafkaConnection {
        &self.conn
    }
    pub fn stats(&self) -> KafkaSourceStats {
        self.stats.get()
    }

    /// Cut the next epoch from whatever the partitions hold now: `None` if
    /// no partition has new records. The epoch's records are decoded and
    /// validated, and its boundary is durable, before this returns.
    pub fn poll(&mut self) -> Result<Option<u64>, Error> {
        let epoch = self.index.frames.len() as u64;
        if epoch >= self.source_limits.max_epochs {
            return Err(Error::Limit("kafka epochs"));
        }
        let start = self.index.start_of(epoch)?.to_vec();
        let mut ends = Vec::with_capacity(start.len());
        let mut any = false;
        let cap = self.source_limits.max_records_per_epoch as i64;
        for (t, (from, _)) in self.topics.iter().zip(&start) {
            let (earliest, latest) = self.conn.offsets(&t.topic, t.partition)?;
            if earliest > *from {
                return Err(Error::Kafka("offset out of range"));
            }
            let end = latest.min(from.saturating_add(cap));
            any |= end > *from;
            ends.push(end);
        }
        if !any {
            return Ok(None);
        }
        let (input, lsns, dead) = self.decode(&start, &ends)?;
        // Malformed records reach the dead-letter topic before the epoch
        // is durable: a crash in between copies them again (the key names
        // the source position, so copies are recognisable).
        if let Some(topic) = &self.source_limits.dead_letter
            && !dead.is_empty()
        {
            let n = dead.len() as u64;
            self.conn.produce(topic, 0, dead)?;
            let mut st = self.stats.get();
            st.dead_lettered += n;
            self.stats.set(st);
        }
        let frame: Vec<(i64, u64)> = ends.into_iter().zip(lsns).collect();
        let e = self.index.append(&frame)?;
        self.remember(e, input);
        Ok(Some(e))
    }

    /// Positions after the commit (or the initial ones without a commit):
    /// what a restarted job resumes from.
    pub fn committed(&self, record: Option<&CommitRecord>) -> Result<Vec<KafkaPosition>, Error> {
        let epoch = match record {
            None => 0,
            Some(r) => r.after_epoch.checked_add(1).ok_or(Error::Overflow)?,
        };
        let at = self.index.start_of(epoch)?;
        Ok(self
            .topics
            .iter()
            .zip(at)
            .map(|(t, (o, l))| KafkaPosition {
                topic: t.topic.clone(),
                partition: t.partition,
                offset: *o,
                next_lsn: *l,
            })
            .collect())
    }

    /// Delete the records below the committed positions: a restart never
    /// reads them again. Uncommitted epochs keep their records.
    pub fn release(&self, record: &CommitRecord) -> Result<(), Error> {
        for p in self.committed(Some(record))? {
            self.conn.delete_before(&p.topic, p.partition, p.offset)?;
        }
        Ok(())
    }

    fn remember(&self, epoch: u64, input: TickInput) {
        let mut cache = self.cache.borrow_mut();
        cache.insert(epoch, input);
        while cache.len() > self.source_limits.cache_epochs.max(1) {
            cache.pop_first();
        }
    }

    /// Fetch and decode the records in `start..ends` of every binding; also
    /// the dead-letter copies of malformed records (when configured).
    fn decode(
        &self,
        start: &[(i64, u64)],
        ends: &[i64],
    ) -> Result<(TickInput, Vec<u64>, Vec<KafkaRecord>), Error> {
        let mut acc: Vec<Accumulator> = self
            .widths
            .iter()
            .map(|w| Accumulator::new(*w, self.limits))
            .collect::<Result<_, _>>()?;
        let mut budget = Budget::new(self.limits);
        let mut stats = self.stats.get();
        let mut lsns = Vec::with_capacity(start.len());
        let mut dead = Vec::new();
        for (i, (t, (from, first))) in self.topics.iter().zip(start).enumerate() {
            let binding = &self.policy.bindings[i];
            let width = self.widths[binding.input];
            let mut next = *first;
            for (offset, record) in self.conn.fetch(&t.topic, t.partition, *from, ends[i])? {
                stats.fetched += 1;
                budget.charge(1)?;
                let decoded = decode_value(record.value.as_ref(), width).and_then(|(h, row)| {
                    match self.policy.binding(h)? {
                        b if b == i => Ok((h, row)),
                        _ => Err(Error::Cdc("partition")),
                    }
                });
                let (header, row) = match decoded {
                    Ok(v) => v,
                    Err(e) => match &self.source_limits.dead_letter {
                        None => return Err(e),
                        Some(_) => {
                            dead.push(dead_letter(t, offset, &record, &e));
                            continue;
                        }
                    },
                };
                if header.source_lsn < next {
                    stats.duplicates += 1;
                    continue;
                }
                if header.source_lsn != next {
                    return Err(Error::Cdc("source offset"));
                }
                next = next.checked_add(1).ok_or(Error::Overflow)?;
                acc[binding.input].add(&row, i128::from(header.z_weight), &mut budget)?;
            }
            lsns.push(next);
        }
        stats.decoded += 1;
        self.stats.set(stats);
        let deltas = acc
            .into_iter()
            .map(Accumulator::finish)
            .collect::<Result<Vec<_>, _>>()?;
        if deltas
            .iter()
            .any(|d| d.len() > self.limits.max_input_entries)
        {
            return Err(Error::Limit("input entries"));
        }
        Ok((TickInput { deltas }, lsns, dead))
    }
}

/// The dead-letter copy of a malformed record.
fn dead_letter(t: &KafkaBinding, offset: i64, record: &KafkaRecord, e: &Error) -> KafkaRecord {
    let position = format!("{}/{}/{}", t.topic, t.partition, offset);
    let mut headers = record.headers.clone();
    headers.insert("vesper-flux-error".into(), format!("{e}").into_bytes());
    headers.insert("vesper-flux-source".into(), position.clone().into_bytes());
    KafkaRecord {
        key: Some(position.into_bytes()),
        value: record.value.clone(),
        headers,
        timestamp: record.timestamp,
    }
}

impl EpochSource for KafkaSource {
    fn epochs(&self) -> u64 {
        self.index.frames.len() as u64
    }
    fn read(&self, epoch: u64) -> Result<TickInput, Error> {
        if let Some(t) = self.cache.borrow().get(&epoch) {
            return Ok(t.clone());
        }
        let start = self.index.start_of(epoch)?.to_vec();
        let end = self
            .index
            .start_of(epoch.checked_add(1).ok_or(Error::Overflow)?)?;
        let ends: Vec<i64> = end.iter().map(|(o, _)| *o).collect();
        let lsns: Vec<u64> = end.iter().map(|(_, l)| *l).collect();
        let (input, got, _) = self.decode(&start, &ends)?;
        if got != lsns {
            // The partition no longer holds what the epoch was cut from.
            return Err(Error::Kafka("replay differs from the indexed epoch"));
        }
        self.remember(epoch, input.clone());
        Ok(input)
    }
}

// ----------------------------------------------------------------------
// ----------------------------------------------------------------------
// Sink
// ----------------------------------------------------------------------

const H_EPOCH: &str = "vesper-flux-epoch";
const H_SEQ: &str = "vesper-flux-seq";
const H_CIRCUIT: &str = "vesper-flux-circuit";
const PROGRESS_PREFIX: &str = "vflux/1";

/// Output topic layout and the producer's transactional id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KafkaSinkConfig {
    pub topic: String,
    /// Output partitions (1..=1024); a row goes to the partition of its
    /// `key_column` value.
    pub partitions: i32,
    pub key_column: usize,
    /// Records per produce request.
    pub max_batch_records: usize,
    /// Kafka transactional id: one per output topic and job. A new sink with
    /// the same id fences the previous one.
    pub transactional_id: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KafkaSinkStats {
    pub published: u64,
    /// Committed Kafka transactions (one per published epoch).
    pub transactions: u64,
    /// Transactions this incarnation aborted after a failure.
    pub aborted: u64,
    /// Epochs skipped because the progress offsets show them published.
    pub already_present: u64,
    /// Producer (re)initialisations: each fences earlier incarnations.
    pub inits: u64,
    /// Progress reads.
    pub progress_reads: u64,
    /// Publications that failed and were left for the next commit or flush.
    pub failures: u64,
}

/// Fault injection: where the next publication stops, leaving its Kafka
/// transaction open as if the process died there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishFault {
    /// After the epoch's records were produced.
    AfterRecords,
    /// After the progress (and input) offsets were added too.
    AfterOffsets,
}

/// Input offsets of a consumer-group source, committed inside the output
/// transaction of the epoch they end.
pub trait GroupOffsetSource {
    /// The group offsets at the end of `epoch`, or `None` if the source has
    /// nothing to commit for it.
    fn offsets_after(&self, epoch: u64) -> Result<Option<GroupOffsets>, Error>;
    /// `epoch` and everything before it is published.
    fn published(&self, epoch: u64);
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Progress {
    /// Last published epoch.
    epoch: Option<u64>,
    /// Next output LSN per partition.
    next_lsn: Vec<u64>,
}

/// Commits into `inner`, then publishes committed epochs to Kafka, one
/// transaction per epoch (see the module docs). A publication failure never
/// fails the commit: it stays pending until the next commit or
/// [`KafkaSink::flush`].
pub struct KafkaSink<S: CommitSink> {
    inner: S,
    conn: KafkaConnection,
    cfg: KafkaSinkConfig,
    producer: Option<ProducerState>,
    progress: Option<Progress>,
    offsets: Option<Rc<dyn GroupOffsetSource>>,
    pending: Option<Error>,
    fault: Option<PublishFault>,
    stats: KafkaSinkStats,
}
impl<S: CommitSink> std::fmt::Debug for KafkaSink<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KafkaSink")
            .field("cfg", &self.cfg)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}
fn output_partition(key: i64, partitions: i32) -> i32 {
    // partitions is 1..=1024, so the remainder fits.
    (mix(u64::from_le_bytes(key.to_le_bytes())) % partitions as u64) as i32
}
fn header_u64(r: &KafkaRecord, name: &str) -> Result<u64, Error> {
    let v = r
        .headers
        .get(name)
        .ok_or(Error::Kafka("output record headers"))?;
    Ok(u64::from_be_bytes(
        v.as_slice()
            .try_into()
            .map_err(|_| Error::Kafka("output record headers"))?,
    ))
}
/// The execution identity a cluster run writes into its commit records.
fn circuit_identity(circuit: &Circuit) -> Result<[u8; 32], Error> {
    let limits = super::CheckpointLimits::default();
    super::checkpoint::identity(circuit, None, limits, &mut limits.budget(circuit))
}
fn circuit_tag(fp: &[u8; 32]) -> Vec<u8> {
    fp[..16].to_vec()
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
/// One decoded output record: epoch, seq, LSN, circuit tag, row, weight.
type OutputRecord = (u64, u32, u64, Vec<u8>, Row, i64);
fn decode_output(r: &KafkaRecord, width: usize, partition: i32) -> Result<OutputRecord, Error> {
    let epoch = header_u64(r, H_EPOCH)?;
    let seq =
        u32::try_from(header_u64(r, H_SEQ)?).map_err(|_| Error::Kafka("output record headers"))?;
    let tag = r
        .headers
        .get(H_CIRCUIT)
        .cloned()
        .ok_or(Error::Kafka("output record headers"))?;
    let (h, row) = decode_value(r.value.as_ref(), width)?;
    if h.partition_id != partition as u32 || h.schema_id != 0 || h.flags != 0 || h.z_weight == 0 {
        return Err(Error::Kafka("output record"));
    }
    Ok((epoch, seq, h.source_lsn, tag, row, h.z_weight))
}
/// Progress metadata: `vflux/1 e=<epoch> t=<circuit tag hex>`.
fn progress_metadata(epoch: u64, tag: &[u8]) -> String {
    format!("{PROGRESS_PREFIX} e={epoch} t={}", hex(tag))
}
fn parse_progress(meta: &str) -> Result<(u64, String), Error> {
    let bad = Error::Kafka("output progress");
    let mut it = meta.split(' ');
    if it.next() != Some(PROGRESS_PREFIX) {
        return Err(bad);
    }
    let e = it
        .next()
        .and_then(|s| s.strip_prefix("e="))
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(bad.clone())?;
    let t = it
        .next()
        .and_then(|s| s.strip_prefix("t="))
        .ok_or(bad.clone())?
        .to_string();
    if it.next().is_some() || t.len() != 32 {
        return Err(bad);
    }
    Ok((e, t))
}

impl<S: CommitSink> KafkaSink<S> {
    pub fn new(
        inner: S,
        conn: KafkaConnection,
        circuit: &Circuit,
        cfg: KafkaSinkConfig,
    ) -> Result<Self, Error> {
        let width = circuit.nodes[circuit.output].width;
        if cfg.partitions < 1
            || cfg.partitions > 1024
            || cfg.key_column >= width
            || cfg.max_batch_records == 0
            || cfg.max_batch_records > 65_536
            || cfg.transactional_id.is_empty()
        {
            return Err(Error::Kafka("sink configuration"));
        }
        let mut inner = inner;
        // A reopened sink may hold committed epochs a dead incarnation never
        // published: the progress offsets decide, at the first publication.
        let pending = inner
            .last_commit()?
            .map(|_| Error::Kafka("opened with committed epochs"));
        Ok(Self {
            inner,
            conn,
            cfg,
            producer: None,
            progress: None,
            offsets: None,
            pending,
            fault: None,
            stats: KafkaSinkStats::default(),
        })
    }
    /// Commit a consumer group's input offsets inside every epoch's output
    /// transaction.
    pub fn with_group_offsets(mut self, source: Rc<dyn GroupOffsetSource>) -> Self {
        self.offsets = Some(source);
        self
    }
    pub fn inner(&self) -> &S {
        &self.inner
    }
    pub fn into_inner(self) -> S {
        self.inner
    }
    pub fn stats(&self) -> KafkaSinkStats {
        self.stats
    }
    /// The error that left committed epochs unpublished, if any.
    pub fn pending(&self) -> Option<&Error> {
        self.pending.as_ref()
    }
    pub fn connection(&self) -> &KafkaConnection {
        &self.conn
    }
    /// The last epoch the output topic holds (read at the first publication).
    pub fn published_through(&self) -> Option<u64> {
        self.progress.as_ref().and_then(|p| p.epoch)
    }
    /// Fault injection: stop the next publication at `fault`, leave its
    /// transaction open and forget the producer, as a dying process would.
    pub fn inject_publish_fault(&mut self, fault: PublishFault) {
        self.fault = Some(fault);
    }
    fn progress_group(&self) -> String {
        format!("{}.progress", self.cfg.transactional_id)
    }

    /// Publish every committed epoch the topic does not hold yet.
    pub fn flush(&mut self) -> Result<(), Error> {
        let Some(record) = self.inner.last_commit()? else {
            return Ok(());
        };
        let outputs = self.inner.outputs()?;
        match self.publish(&record.fingerprint, &outputs) {
            Ok(()) => {
                self.pending = None;
                Ok(())
            }
            Err(e) => {
                self.stats.failures += 1;
                self.pending = Some(e.clone());
                Err(e)
            }
        }
    }

    /// InitProducerId (fencing every earlier incarnation), then the stable
    /// progress offsets.
    fn ensure_producer(&mut self, tag: &[u8]) -> Result<(), Error> {
        if self.producer.is_some() && self.progress.is_some() {
            return Ok(());
        }
        self.progress = None;
        let (timeout, codec) = (
            self.conn.cfg.transaction_timeout,
            self.conn.cfg.compression,
        );
        let txn = self.cfg.transactional_id.clone();
        let state = self
            .conn
            .run(|c| ProducerState::init(c, Some(txn), timeout, codec))?;
        self.producer = Some(state);
        self.stats.inits += 1;
        self.stats.progress_reads += 1;
        let parts: Vec<i32> = (0..self.cfg.partitions).collect();
        let got = self
            .conn
            .group_offsets(&self.progress_group(), &self.cfg.topic, &parts)?;
        let want = hex(tag);
        let mut epoch: Option<u64> = None;
        let mut next_lsn = vec![0u64; parts.len()];
        if !got.is_empty() {
            if got.len() != parts.len() {
                return Err(Error::Kafka("output progress"));
            }
            for (p, (offset, meta)) in &got {
                let (e, t) = parse_progress(meta)?;
                if t != want {
                    return Err(Error::Kafka("output topic belongs to another circuit"));
                }
                if epoch.is_some_and(|x| x != e) {
                    return Err(Error::Kafka("output progress"));
                }
                epoch = Some(e);
                let slot = usize::try_from(*p).map_err(|_| Error::Kafka("output progress"))?;
                *next_lsn
                    .get_mut(slot)
                    .ok_or(Error::Kafka("output progress"))? =
                    u64::try_from(*offset).map_err(|_| Error::Kafka("output progress"))?;
            }
        }
        self.progress = Some(Progress { epoch, next_lsn });
        Ok(())
    }

    fn publish(&mut self, fingerprint: &[u8; 32], outputs: &[(u64, ZSet)]) -> Result<(), Error> {
        let tag = circuit_tag(fingerprint);
        self.ensure_producer(&tag)?;
        for (epoch, set) in outputs {
            let done = self.progress.as_ref().and_then(|p| p.epoch);
            if done.is_some_and(|d| *epoch <= d) {
                self.stats.already_present += 1;
                if let Some(src) = &self.offsets {
                    src.published(*epoch);
                }
                continue;
            }
            if let Err(e) = self.publish_epoch(*epoch, set, &tag) {
                // Abort what can be aborted; a fenced, broken or faulted
                // producer is simply dropped (the next InitProducerId aborts
                // its transaction).
                if let Some(mut p) = self.producer.take()
                    && p.in_txn
                    && !p.fenced
                    && e != Error::Kafka("injected fault")
                {
                    let ok = self.conn.run(|c| p.end(c, false)).is_ok();
                    if ok {
                        self.stats.aborted += 1;
                        self.producer = Some(p);
                    }
                }
                if self.producer.is_none() {
                    self.progress = None;
                }
                return Err(e);
            }
            if let Some(src) = &self.offsets {
                src.published(*epoch);
            }
        }
        Ok(())
    }

    fn publish_epoch(&mut self, epoch: u64, set: &ZSet, tag: &[u8]) -> Result<(), Error> {
        let fault = self.fault.take();
        let mut next = self
            .progress
            .as_ref()
            .ok_or(Error::Kafka("output progress"))?
            .next_lsn
            .clone();
        let progress_group = self.progress_group();
        let group_offsets = match &self.offsets {
            Some(src) => src.offsets_after(epoch)?,
            None => None,
        };
        let producer = self.producer.as_mut().ok_or(Error::Kafka("producer"))?;
        let (key, parts) = (self.cfg.key_column, self.cfg.partitions);
        let mut published = 0u64;
        self.conn.run(|c| -> KResult<()> {
            producer.begin()?;
            for p in 0..parts {
                let slot = p as usize;
                let rows = set
                    .entries()
                    .filter(|(row, _)| output_partition(row[key], parts) == p);
                let mut batch: Vec<KafkaRecord> = Vec::new();
                for (seq, (row, weight)) in rows.enumerate() {
                    let seq = u64::try_from(seq).map_err(|_| Fail::Local(Error::Overflow))?;
                    let header = CdcHeader {
                        source_lsn: next[slot],
                        event_ts_ms: 0,
                        z_weight: weight,
                        partition_id: p as u32,
                        schema_id: 0,
                        flags: 0,
                    };
                    let mut record = kafka_record(cdc_value(header, row));
                    record
                        .headers
                        .insert(H_EPOCH.into(), epoch.to_be_bytes().to_vec());
                    record
                        .headers
                        .insert(H_SEQ.into(), seq.to_be_bytes().to_vec());
                    record.headers.insert(H_CIRCUIT.into(), tag.to_vec());
                    batch.push(record);
                    next[slot] = next[slot]
                        .checked_add(1)
                        .ok_or(Fail::Local(Error::Overflow))?;
                    if batch.len() == self.cfg.max_batch_records {
                        let b = producer.prepare(c, &self.cfg.topic, p, &batch)?;
                        producer.deliver(c, &self.cfg.topic, p, &b)?;
                        published += batch.len() as u64;
                        batch.clear();
                    }
                }
                if !batch.is_empty() {
                    let b = producer.prepare(c, &self.cfg.topic, p, &batch)?;
                    producer.deliver(c, &self.cfg.topic, p, &b)?;
                    published += batch.len() as u64;
                }
            }
            if fault == Some(PublishFault::AfterRecords) {
                return Err(Fail::Local(Error::Kafka("injected fault")));
            }
            let meta = progress_metadata(epoch, tag);
            let progress = GroupOffsets {
                group: progress_group.clone(),
                generation: -1,
                member_id: String::new(),
                instance_id: None,
                offsets: (0..parts)
                    .map(|p| -> KResult<_> {
                        let lsn = i64::try_from(next[p as usize])
                            .map_err(|_| Fail::Local(Error::Overflow))?;
                        Ok((self.cfg.topic.clone(), p, lsn, meta.clone()))
                    })
                    .collect::<KResult<Vec<_>>>()?,
            };
            producer.send_offsets(c, &progress)?;
            if let Some(g) = &group_offsets {
                producer.send_offsets(c, g)?;
            }
            if fault == Some(PublishFault::AfterOffsets) {
                return Err(Fail::Local(Error::Kafka("injected fault")));
            }
            producer.end(c, true)
        })?;
        self.stats.published += published;
        self.stats.transactions += 1;
        self.progress = Some(Progress {
            epoch: Some(epoch),
            next_lsn: next,
        });
        Ok(())
    }
}

impl<S: CommitSink> CommitSink for KafkaSink<S> {
    fn last_commit(&mut self) -> Result<Option<CommitRecord>, Error> {
        self.inner.last_commit()
    }
    fn commit(&mut self, record: &CommitRecord, outputs: &[(u64, ZSet)]) -> Result<(), Error> {
        self.inner.commit(record, outputs)?;
        // Committed: publication can only lag from here on.
        let result = if self.pending.is_none() {
            self.publish(&record.fingerprint, outputs)
        } else {
            match self.inner.outputs() {
                Ok(all) => self.publish(&record.fingerprint, &all),
                Err(e) => Err(e),
            }
        };
        match result {
            Ok(()) => self.pending = None,
            Err(e) => {
                self.stats.failures += 1;
                self.pending = Some(e);
            }
        }
        Ok(())
    }
    fn crash(&mut self) -> Result<(), Error> {
        self.inner.crash()?;
        // A new incarnation knows nothing about the topic: it fences the
        // old producer and reads the progress again.
        self.producer = None;
        self.progress = None;
        self.pending = Some(Error::Kafka("restarted"));
        Ok(())
    }
    fn outputs(&mut self) -> Result<Vec<(u64, ZSet)>, Error> {
        self.inner.outputs()
    }
}

/// Read an output topic back (read-committed) as per-epoch deltas, checking
/// that every partition is a gap- and duplicate-free sequence (LSN
/// `0, 1, ...` and increasing `(epoch, seq)`) written for one circuit.
pub fn read_kafka_output(
    conn: &KafkaConnection,
    circuit: &Circuit,
    topic: &str,
    partitions: i32,
) -> Result<BTreeMap<u64, ZSet>, Error> {
    if !conn.cfg.read_committed {
        return Err(Error::Kafka("output must be read committed"));
    }
    let width = circuit.nodes[circuit.output].width;
    let limits = circuit.limits;
    let tag = circuit_tag(&circuit_identity(circuit)?);
    let mut epochs: BTreeMap<u64, Vec<(Row, i64)>> = BTreeMap::new();
    for p in 0..partitions {
        let (earliest, latest) = conn.offsets(topic, p)?;
        if earliest != 0 {
            return Err(Error::Kafka("output topic truncated"));
        }
        let mut previous: Option<(u64, u32)> = None;
        for (lsn, (_, r)) in conn.fetch(topic, p, 0, latest)?.iter().enumerate() {
            let (epoch, seq, got_lsn, got_tag, row, weight) = decode_output(r, width, p)?;
            if got_lsn != lsn as u64 {
                return Err(Error::Kafka("output LSN gap or duplicate"));
            }
            if got_tag != tag {
                return Err(Error::Kafka("output topic belongs to another circuit"));
            }
            let ok = match previous {
                None => seq == 0,
                Some((e, s)) => (epoch == e && seq == s + 1) || (epoch > e && seq == 0),
            };
            if !ok {
                return Err(Error::Kafka("output sequence"));
            }
            previous = Some((epoch, seq));
            epochs.entry(epoch).or_default().push((row, weight));
        }
    }
    let mut out = BTreeMap::new();
    for (epoch, entries) in epochs {
        let big = Limits {
            max_input_entries: entries.len().max(limits.max_input_entries),
            ..limits
        };
        out.insert(epoch, ZSet::consolidate(width, &entries, big)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp() -> [u8; 32] {
        [7; 32]
    }

    #[test]
    fn index_round_trip_torn_tail_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx");
        let mut idx = EpochIndex::create(&path, &fp(), vec![(5, 100), (0, 0)]).unwrap();
        assert_eq!(idx.append(&[(9, 104), (2, 2)]).unwrap(), 0);
        assert_eq!(idx.append(&[(9, 104), (7, 7)]).unwrap(), 1);
        drop(idx);
        let (idx, torn) = EpochIndex::open(&path, &fp(), 2, 10).unwrap();
        assert_eq!(torn, 0);
        assert_eq!(idx.start_of(0).unwrap(), &[(5, 100), (0, 0)]);
        assert_eq!(idx.start_of(2).unwrap(), &[(9, 104), (7, 7)]);
        assert_eq!(idx.start_of(3).unwrap_err(), Error::InputCount);
        let frame = idx.frame_len();
        drop(idx);
        // Wrong fingerprint or binding count.
        assert!(EpochIndex::open(&path, &[8; 32], 2, 10).is_err());
        assert!(EpochIndex::open(&path, &fp(), 3, 10).is_err());
        // Every partial final append is cut off.
        let good = std::fs::read(&path).unwrap();
        for cut in 1..frame as usize {
            let mut bytes = good.clone();
            bytes.truncate(good.len() - cut);
            std::fs::write(&path, &bytes).unwrap();
            let (idx, torn) = EpochIndex::open(&path, &fp(), 2, 10).unwrap();
            assert_eq!(idx.frames.len(), 1, "cut {cut}");
            assert_eq!(torn, frame - cut as u64);
        }
        // A damaged last frame is torn; a damaged middle frame is corruption.
        let mut bytes = good.clone();
        let n = bytes.len();
        bytes[n - 40] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let (idx, torn) = EpochIndex::open(&path, &fp(), 2, 10).unwrap();
        assert_eq!((idx.frames.len(), torn), (1, frame));
        let mut bytes = good.clone();
        bytes[n - frame as usize - 40] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            EpochIndex::open(&path, &fp(), 2, 10).err(),
            Some(Error::Storage("corrupt kafka index"))
        );
        // Offsets may never move backwards.
        std::fs::remove_file(&path).unwrap();
        let mut idx = EpochIndex::create(&path, &fp(), vec![(5, 100), (0, 0)]).unwrap();
        idx.append(&[(4, 100), (0, 0)]).unwrap();
        drop(idx);
        assert!(EpochIndex::open(&path, &fp(), 2, 10).is_err());
        // The epoch limit.
        std::fs::remove_file(&path).unwrap();
        let mut idx = EpochIndex::create(&path, &fp(), vec![(0, 0)]).unwrap();
        idx.append(&[(1, 1)]).unwrap();
        idx.append(&[(2, 2)]).unwrap();
        drop(idx);
        assert_eq!(
            EpochIndex::open(&path, &fp(), 1, 1).err(),
            Some(Error::Limit("kafka epochs"))
        );
    }

    #[test]
    fn record_values_decode_exactly() {
        let h = CdcHeader {
            source_lsn: 3,
            event_ts_ms: -1,
            z_weight: -2,
            partition_id: 4,
            schema_id: 5,
            flags: 0,
        };
        let v = cdc_value(h, &[1, -2, i64::MAX]);
        assert_eq!(
            decode_value(Some(&v), 3).unwrap(),
            (h, vec![1, -2, i64::MAX])
        );
        assert_eq!(
            decode_value(Some(&v), 2).unwrap_err(),
            Error::Cdc("record length")
        );
        assert_eq!(
            decode_value(None, 3).unwrap_err(),
            Error::Cdc("record without value")
        );
        let short = v[..v.len() - 1].to_vec();
        assert!(decode_value(Some(&short), 3).is_err());
        // Output partitioning is stable and in range.
        for key in [-5i64, 0, 1, 77, i64::MIN, i64::MAX] {
            for parts in [1, 2, 7, 1024] {
                let p = output_partition(key, parts);
                assert!((0..parts).contains(&p));
                assert_eq!(p, output_partition(key, parts));
            }
        }
    }

    #[test]
    fn unreachable_brokers_are_typed_errors() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let cfg = KafkaConfig {
            retry_for: Duration::from_millis(200),
            request_timeout: Duration::from_secs(5),
            ..KafkaConfig::new(format!("127.0.0.1:{port}"))
        };
        assert!(matches!(
            KafkaConnection::connect(&cfg).unwrap_err(),
            Error::Kafka(_)
        ));
        let empty = KafkaConfig {
            bootstrap: Vec::new(),
            ..cfg
        };
        assert_eq!(
            KafkaConnection::connect(&empty).unwrap_err(),
            Error::Kafka("configuration")
        );
    }
}
