//! Kafka source and sink for the Flux cluster, over the pure-Rust `rskafka`
//! wire client (no librdkafka).
//!
//! # Source: offsets are part of the checkpoint
//!
//! A [`KafkaSource`] reads CDC records (the 32-byte [`CdcHeader`] followed by
//! `width` little-endian i64 cells, as in `Executor::ingest_cdc`) from one or
//! more Kafka partitions, each bound to a [`CdcPolicy`] binding. Epochs are
//! cut by [`KafkaSource::poll`]: it fetches the records between the end of
//! the last epoch and the partitions' current high watermarks (at most
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
//! Offsets are not committed to a Kafka consumer group; the Flux checkpoint
//! is the only authority.
//!
//! LSNs must be consecutive per binding. A record whose LSN is below the
//! next expected one is a producer retry and is dropped (counted in
//! [`KafkaSourceStats::duplicates`]); a gap, a foreign partition id or
//! schema, reserved flags, a bad weight or a value of the wrong length is a
//! typed error, and the epoch is not cut.
//!
//! # Sink: commit, then publish, fenced by epoch
//!
//! A [`KafkaSink`] wraps a transactional [`CommitSink`] (normally a
//! `QuarrySink`). Outputs are committed there first (with the offsets and
//! the checkpoint); only committed epochs are then produced to the output
//! topic, so nothing a reader saw is ever rolled back. Every output record
//! carries its epoch, its position in the epoch's partition slice and a
//! per-partition LSN; before publishing after a crash or an ambiguous produce
//! failure the sink reads the last record of each output partition and skips
//! everything already there. Produce requests use no client-side retries, so
//! the only way a record reaches the topic twice would be a second writer.
//! Together: exactly-once in the output topic for a single writer per output
//! partition, with publication lagging the commit. Readers see a prefix of a
//! partition, never a duplicate.
//!
//! # Scope
//!
//! Plaintext connections only (no TLS or SASL), uncompressed batches, no
//! Kafka transactions or idempotent-producer sequence numbers (rskafka 0.6
//! implements neither), no consumer groups. The output fence needs the last
//! record of each output partition: if retention deleted it, publishing
//! stops with a typed error instead of guessing.
use super::cdc::CdcPolicy;
use super::sink::{CommitRecord, CommitSink};
use super::{Circuit, EpochSource, TickInput};
use crate::zset::Accumulator;
use crate::{Budget, CdcHeader, Error, Limits, Row, ZSet};
use rskafka::BackoffConfig;
use rskafka::client::error::{Error as ClientError, ProtocolError};
use rskafka::client::partition::{Compression, OffsetAt, PartitionClient, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder};
use rskafka::record::Record;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// How to reach the brokers.
#[derive(Clone, Debug)]
pub struct KafkaConfig {
    /// `host:port` bootstrap brokers (plaintext).
    pub bootstrap: Vec<String>,
    pub client_id: String,
    /// How long one read or metadata operation keeps retrying a broker that
    /// is unreachable or not yet leader. Produce requests never retry.
    pub retry_for: Duration,
    /// Deadline of one operation including its retries.
    pub request_timeout: Duration,
    /// Upper bound of one fetch response.
    pub fetch_bytes: i32,
}
impl KafkaConfig {
    pub fn new(bootstrap: impl Into<String>) -> Self {
        Self {
            bootstrap: vec![bootstrap.into()],
            client_id: "vesper-flux".into(),
            retry_for: Duration::from_secs(20),
            request_timeout: Duration::from_secs(30),
            fetch_bytes: 1 << 20,
        }
    }
}

/// One connection to a Kafka cluster, with a private single-threaded Tokio
/// runtime: every call blocks until the broker answered.
pub struct KafkaConnection {
    rt: tokio::runtime::Runtime,
    /// Retries transient failures for `retry_for`.
    client: Client,
    /// Never retries (produce: a retry after a lost response could append
    /// the same batch twice).
    once: Client,
    cfg: KafkaConfig,
    partitions: RefCell<BTreeMap<(String, i32, bool), Arc<PartitionClient>>>,
    last_error: RefCell<Option<String>>,
}
impl std::fmt::Debug for KafkaConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KafkaConnection")
            .field("bootstrap", &self.cfg.bootstrap)
            .finish_non_exhaustive()
    }
}

fn classify(e: &ClientError) -> Error {
    match e {
        ClientError::ServerError { protocol_error, .. } => match protocol_error {
            ProtocolError::OffsetOutOfRange => Error::Kafka("offset out of range"),
            ProtocolError::TopicAlreadyExists => Error::Kafka("topic exists"),
            ProtocolError::UnknownTopicOrPartition => Error::Kafka("unknown topic or partition"),
            ProtocolError::MessageTooLarge | ProtocolError::RecordListTooLarge => {
                Error::Kafka("batch too large")
            }
            _ => Error::Kafka("broker error"),
        },
        _ => Error::Kafka("unavailable"),
    }
}

impl KafkaConnection {
    pub fn connect(cfg: &KafkaConfig) -> Result<Self, Error> {
        if cfg.bootstrap.is_empty() || cfg.fetch_bytes < 1024 {
            return Err(Error::Kafka("configuration"));
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::Kafka("runtime"))?;
        let build = |deadline: Duration| {
            ClientBuilder::new(cfg.bootstrap.clone())
                .client_id(cfg.client_id.clone())
                .backoff_config(BackoffConfig {
                    init_backoff: Duration::from_millis(50),
                    max_backoff: Duration::from_secs(1),
                    base: 3.0,
                    deadline: Some(deadline),
                })
                .build()
        };
        let last_error = RefCell::new(None);
        let connect = |deadline| match rt
            .block_on(async { tokio::time::timeout(cfg.request_timeout, build(deadline)).await })
        {
            Ok(Ok(c)) => Ok(c),
            Ok(Err(e)) => {
                *last_error.borrow_mut() = Some(format!("connect: {e}"));
                Err(classify(&e))
            }
            Err(_) => Err(Error::Kafka("timeout")),
        };
        let client = connect(cfg.retry_for)?;
        let once = connect(Duration::ZERO)?;
        Ok(Self {
            rt,
            client,
            once,
            cfg: cfg.clone(),
            partitions: RefCell::new(BTreeMap::new()),
            last_error,
        })
    }

    /// The client's message for the last failed operation.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.borrow().clone()
    }

    fn run<T>(
        &self,
        what: &str,
        f: impl std::future::Future<Output = Result<T, ClientError>>,
    ) -> Result<T, Error> {
        match self
            .rt
            .block_on(async { tokio::time::timeout(self.cfg.request_timeout, f).await })
        {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                *self.last_error.borrow_mut() = Some(format!("{what}: {e}"));
                Err(classify(&e))
            }
            Err(_) => {
                *self.last_error.borrow_mut() = Some(format!("{what}: timeout"));
                Err(Error::Kafka("timeout"))
            }
        }
    }

    fn partition(
        &self,
        topic: &str,
        partition: i32,
        once: bool,
    ) -> Result<Arc<PartitionClient>, Error> {
        let key = (topic.to_string(), partition, once);
        if let Some(p) = self.partitions.borrow().get(&key) {
            return Ok(Arc::clone(p));
        }
        let client = if once { &self.once } else { &self.client };
        let p = self.run(
            "partition",
            client.partition_client(topic, partition, UnknownTopicHandling::Error),
        )?;
        let p = Arc::new(p);
        self.partitions.borrow_mut().insert(key, Arc::clone(&p));
        Ok(p)
    }

    /// Create a topic with replication factor 1.
    pub fn create_topic(&self, topic: &str, partitions: i32) -> Result<(), Error> {
        let controller = self.client.controller_client().map_err(|e| classify(&e))?;
        self.run(
            "create topic",
            controller.create_topic(topic, partitions, 1, 30_000),
        )
    }

    /// Earliest retained and next (high watermark) offset of a partition.
    pub fn offsets(&self, topic: &str, partition: i32) -> Result<(i64, i64), Error> {
        let p = self.partition(topic, partition, false)?;
        let earliest = self.run("earliest offset", p.get_offset(OffsetAt::Earliest))?;
        let latest = self.run("latest offset", p.get_offset(OffsetAt::Latest))?;
        if earliest < 0 || latest < earliest {
            return Err(Error::Kafka("offsets"));
        }
        Ok((earliest, latest))
    }

    /// Append records as one batch with client retries: at-least-once (a
    /// lost response can duplicate the batch). Returns their offsets.
    pub fn produce(
        &self,
        topic: &str,
        partition: i32,
        records: Vec<Record>,
    ) -> Result<Vec<i64>, Error> {
        let p = self.partition(topic, partition, false)?;
        self.run("produce", p.produce(records, Compression::NoCompression))
    }

    /// Append records as one batch, one attempt: on error the batch may or
    /// may not be in the log.
    fn produce_once(
        &self,
        topic: &str,
        partition: i32,
        records: Vec<Record>,
    ) -> Result<Vec<i64>, Error> {
        let p = self.partition(topic, partition, true)?;
        let out = self.run("produce", p.produce(records, Compression::NoCompression));
        if out.is_err() {
            // A broken connection is cached in the client; reconnect next time.
            self.partitions
                .borrow_mut()
                .remove(&(topic.to_string(), partition, true));
        }
        out
    }

    /// Every record with offset in `from..to`, in offset order.
    pub fn fetch(
        &self,
        topic: &str,
        partition: i32,
        from: i64,
        to: i64,
    ) -> Result<Vec<(i64, Record)>, Error> {
        let p = self.partition(topic, partition, false)?;
        let mut out = Vec::new();
        let mut at = from;
        while at < to {
            let (records, high) =
                self.run("fetch", p.fetch_records(at, 1..self.cfg.fetch_bytes, 100))?;
            let before = out.len();
            for r in records {
                if r.offset < at {
                    continue;
                }
                if r.offset >= to {
                    break;
                }
                at = r.offset + 1;
                out.push((r.offset, r.record));
            }
            if out.len() == before {
                if high < to {
                    return Err(Error::Kafka("records missing below the high watermark"));
                }
                // Offsets in `at..to` hold no data records (control batches).
                break;
            }
        }
        Ok(out)
    }

    /// Delete every record below `offset`.
    pub fn delete_before(&self, topic: &str, partition: i32, offset: i64) -> Result<(), Error> {
        let p = self.partition(topic, partition, false)?;
        self.run("delete records", p.delete_records(offset, 30_000))
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
pub fn kafka_record(value: Vec<u8>) -> Record {
    Record {
        key: None,
        value: Some(value),
        headers: BTreeMap::new(),
        timestamp: rskafka::chrono::DateTime::from_timestamp_millis(0).unwrap_or_default(),
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
        for i in 0..self.count {
            let at = 8 + 16 * i;
            let o = i64::from_le_bytes(body[at..at + 8].try_into().map_err(|_| Error::Overflow)?);
            let l = u64::from_le_bytes(
                body[at + 8..at + 16]
                    .try_into()
                    .map_err(|_| Error::Overflow)?,
            );
            if o < previous[i].0 || l < previous[i].1 {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KafkaSourceLimits {
    /// Records cut into one epoch from one partition (1..=65536).
    pub max_records_per_epoch: usize,
    pub max_epochs: u64,
    /// Decoded epochs kept in memory for workers reading the same epoch.
    pub cache_epochs: usize,
}
impl Default for KafkaSourceLimits {
    fn default() -> Self {
        Self {
            max_records_per_epoch: 512,
            max_epochs: 1 << 20,
            cache_epochs: 16,
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
        let (input, lsns) = self.decode(&start, &ends)?;
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

    /// Fetch and decode the records in `start..ends` of every binding.
    fn decode(&self, start: &[(i64, u64)], ends: &[i64]) -> Result<(TickInput, Vec<u64>), Error> {
        let mut acc: Vec<Accumulator> = self
            .widths
            .iter()
            .map(|w| Accumulator::new(*w, self.limits))
            .collect::<Result<_, _>>()?;
        let mut budget = Budget::new(self.limits);
        let mut stats = self.stats.get();
        let mut lsns = Vec::with_capacity(start.len());
        for (i, (t, (from, first))) in self.topics.iter().zip(start).enumerate() {
            let binding = &self.policy.bindings[i];
            let width = self.widths[binding.input];
            let mut next = *first;
            for (_, record) in self.conn.fetch(&t.topic, t.partition, *from, ends[i])? {
                stats.fetched += 1;
                budget.charge(1)?;
                let (header, row) = decode_value(record.value.as_ref(), width)?;
                if self.policy.binding(header)? != i {
                    return Err(Error::Cdc("partition"));
                }
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
        Ok((TickInput { deltas }, lsns))
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
        let (input, got) = self.decode(&start, &ends)?;
        if got != lsns {
            // The partition no longer holds what the epoch was cut from.
            return Err(Error::Kafka("replay differs from the indexed epoch"));
        }
        self.remember(epoch, input.clone());
        Ok(input)
    }
}

// ----------------------------------------------------------------------
// Sink
// ----------------------------------------------------------------------

const H_EPOCH: &str = "vesper-flux-epoch";
const H_SEQ: &str = "vesper-flux-seq";
const H_CIRCUIT: &str = "vesper-flux-circuit";

/// Output topic layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KafkaSinkConfig {
    pub topic: String,
    /// Output partitions (1..=1024); a row goes to the partition of its
    /// `key_column` value.
    pub partitions: i32,
    pub key_column: usize,
    /// Records per produce request (one Kafka batch, appended atomically).
    pub max_batch_records: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KafkaSinkStats {
    pub published: u64,
    /// Records skipped because the topic already held them (after a crash
    /// or an ambiguous produce failure).
    pub already_present: u64,
    /// Fence reads (last record of an output partition).
    pub fences_read: u64,
    /// Publications that failed and were left for the next commit or flush.
    pub failures: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fence {
    Unknown,
    Empty,
    At { epoch: u64, seq: u32, lsn: u64 },
}

/// Commits into `inner`, then publishes committed epochs to Kafka (see the
/// module docs). A publication failure never fails the commit: it stays
/// pending until the next commit or [`KafkaSink::flush`].
pub struct KafkaSink<S: CommitSink> {
    inner: S,
    conn: KafkaConnection,
    cfg: KafkaSinkConfig,
    width: usize,
    fences: Vec<Fence>,
    pending: Option<Error>,
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
fn header_u64(r: &Record, name: &str) -> Result<u64, Error> {
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
fn circuit_tag(fp: &[u8; 32]) -> Vec<u8> {
    fp[..16].to_vec()
}
/// One decoded output record: epoch, seq, LSN, circuit tag, row, weight.
type OutputRecord = (u64, u32, u64, Vec<u8>, Row, i64);
fn decode_output(r: &Record, width: usize, partition: i32) -> Result<OutputRecord, Error> {
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
        {
            return Err(Error::Kafka("sink configuration"));
        }
        let fences = vec![Fence::Unknown; cfg.partitions as usize];
        Ok(Self {
            inner,
            conn,
            cfg,
            width,
            fences,
            pending: None,
            stats: KafkaSinkStats::default(),
        })
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

    fn read_fence(&mut self, p: i32, tag: &[u8]) -> Result<Fence, Error> {
        self.stats.fences_read += 1;
        let (earliest, latest) = self.conn.offsets(&self.cfg.topic, p)?;
        if latest == 0 {
            return Ok(Fence::Empty);
        }
        if earliest == latest {
            return Err(Error::Kafka("output fence lost to retention"));
        }
        let records = self.conn.fetch(&self.cfg.topic, p, latest - 1, latest)?;
        let (_, last) = records
            .last()
            .ok_or(Error::Kafka("output fence lost to retention"))?;
        let (epoch, seq, lsn, got, _, _) = decode_output(last, self.width, p)?;
        if got != tag {
            return Err(Error::Kafka("output topic belongs to another circuit"));
        }
        Ok(Fence::At { epoch, seq, lsn })
    }

    fn publish(&mut self, fingerprint: &[u8; 32], outputs: &[(u64, ZSet)]) -> Result<(), Error> {
        let tag = circuit_tag(fingerprint);
        for p in 0..self.cfg.partitions {
            let slot = p as usize;
            if self.fences[slot] == Fence::Unknown {
                self.fences[slot] = self.read_fence(p, &tag)?;
            }
            let mut fence = self.fences[slot];
            let mut batch: Vec<Record> = Vec::new();
            let mut last = fence;
            let mut next_lsn = match fence {
                Fence::At { lsn, .. } => lsn.checked_add(1).ok_or(Error::Overflow)?,
                _ => 0,
            };
            for (epoch, set) in outputs {
                if let Fence::At { epoch: fe, .. } = fence
                    && *epoch < fe
                {
                    continue;
                }
                let (key, parts) = (self.cfg.key_column, self.cfg.partitions);
                let rows = set
                    .entries()
                    .filter(|(row, _)| output_partition(row[key], parts) == p);
                for (seq, (row, weight)) in rows.enumerate() {
                    let seq = u32::try_from(seq).map_err(|_| Error::Overflow)?;
                    if let Fence::At {
                        epoch: fe, seq: fs, ..
                    } = fence
                        && (*epoch, seq) <= (fe, fs)
                    {
                        self.stats.already_present += 1;
                        continue;
                    }
                    let header = CdcHeader {
                        source_lsn: next_lsn,
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
                        .insert(H_SEQ.into(), u64::from(seq).to_be_bytes().to_vec());
                    record.headers.insert(H_CIRCUIT.into(), tag.clone());
                    batch.push(record);
                    last = Fence::At {
                        epoch: *epoch,
                        seq,
                        lsn: next_lsn,
                    };
                    next_lsn = next_lsn.checked_add(1).ok_or(Error::Overflow)?;
                    if batch.len() == self.cfg.max_batch_records {
                        self.produce(p, std::mem::take(&mut batch), last)?;
                        fence = last;
                    }
                }
            }
            if !batch.is_empty() {
                self.produce(p, batch, last)?;
            }
        }
        Ok(())
    }

    fn produce(&mut self, p: i32, batch: Vec<Record>, last: Fence) -> Result<(), Error> {
        let n = batch.len() as u64;
        match self.conn.produce_once(&self.cfg.topic, p, batch) {
            Ok(_) => {
                self.fences[p as usize] = last;
                self.stats.published += n;
                Ok(())
            }
            Err(e) => {
                // The batch may or may not be in the log: read it back first.
                self.fences[p as usize] = Fence::Unknown;
                Err(e)
            }
        }
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
        // A new incarnation knows nothing about the topic.
        self.fences.fill(Fence::Unknown);
        self.pending = Some(Error::Kafka("restarted"));
        Ok(())
    }
    fn outputs(&mut self) -> Result<Vec<(u64, ZSet)>, Error> {
        self.inner.outputs()
    }
}

/// Read an output topic back as per-epoch deltas, checking that every
/// partition is a gap- and duplicate-free sequence (LSN `0, 1, ...` and
/// increasing `(epoch, seq)`) written for one circuit.
pub fn read_kafka_output(
    conn: &KafkaConnection,
    circuit: &Circuit,
    topic: &str,
    partitions: i32,
) -> Result<BTreeMap<u64, ZSet>, Error> {
    let width = circuit.nodes[circuit.output].width;
    let limits = circuit.limits;
    let mut epochs: BTreeMap<u64, Vec<(Row, i64)>> = BTreeMap::new();
    let mut tag: Option<Vec<u8>> = None;
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
            if tag.get_or_insert_with(|| got_tag.clone()) != &got_tag {
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
