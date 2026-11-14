//! Transactional output sinks for the cluster's exactly-once commit.
//!
//! A commit makes three things visible together: the output deltas of every
//! epoch the checkpoint covers, the source offset (the last covered epoch of
//! the input log) and the handle of the global checkpoint manifest. Recovery
//! and coordinator failover read the last [`CommitRecord`] back from the sink,
//! so the sink is the single durable source of truth for "how far did we get".
//!
//! [`QuarrySink`] stores the record and the outputs in one Vesper Quarry
//! write transaction (`Db::begin` .. `Db::commit`, which syncs the WAL before
//! returning), so after any crash either the old record and none of the new
//! outputs, or the new record and all of them, are visible. [`MemorySink`]
//! keeps the same protocol in memory and survives simulated crashes, for
//! tests and for [`super::run_cluster`].
use crate::{Error, Limits, Row, ZSet};
use std::collections::BTreeMap;
use vesper_loom_cas::ObjectId;
use vesper_quarry::device::{Device, FsDevice, SimDevice};
use vesper_quarry::durable::{Db, DbError, DbErrorKind, DbLimits, MAX_VALUE, RecordId, Txn};

use super::checkpoint::durable::blob_id;

/// The durable commit record: source offset plus global checkpoint handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitRecord {
    /// Execution identity of the (unplanned) circuit this sink belongs to.
    pub fingerprint: [u8; 32],
    /// Last input-log epoch covered: the committed source offset.
    pub after_epoch: u64,
    /// Workers of the checkpoint the record names.
    pub workers: u32,
    /// Global checkpoint manifest.
    pub global: ObjectId,
}

/// A sink that commits outputs and a [`CommitRecord`] atomically.
pub trait CommitSink {
    /// The last durable commit record, if any.
    fn last_commit(&mut self) -> Result<Option<CommitRecord>, Error>;
    /// Atomically publish `outputs` (epoch, delta) and replace the record.
    /// Epochs must be increasing, after the previous record and at most
    /// `record.after_epoch`; the new record must not move backwards.
    fn commit(&mut self, record: &CommitRecord, outputs: &[(u64, ZSet)]) -> Result<(), Error>;
    /// Simulated crash of the process holding the sink: lose volatile state
    /// and recover from durable state.
    fn crash(&mut self) -> Result<(), Error>;
    /// Every committed output delta, in epoch order.
    fn outputs(&mut self) -> Result<Vec<(u64, ZSet)>, Error>;
}

fn check_commit(
    previous: Option<&CommitRecord>,
    record: &CommitRecord,
    outputs: &[(u64, ZSet)],
) -> Result<(), Error> {
    if let Some(p) = previous {
        if p.fingerprint != record.fingerprint {
            return Err(Error::Checkpoint("sink belongs to another circuit"));
        }
        if record.after_epoch < p.after_epoch {
            return Err(Error::Checkpoint("sink offset regression"));
        }
    }
    let floor = previous.map(|p| p.after_epoch);
    let mut last = floor;
    for (epoch, _) in outputs {
        if last.is_some_and(|l| *epoch <= l) || *epoch > record.after_epoch {
            return Err(Error::Checkpoint("sink output epoch"));
        }
        last = Some(*epoch);
    }
    Ok(())
}

/// In-memory sink whose contents survive [`CommitSink::crash`].
#[derive(Debug, Default)]
pub struct MemorySink {
    record: Option<CommitRecord>,
    outputs: Vec<(u64, ZSet)>,
}
impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }
}
impl CommitSink for MemorySink {
    fn last_commit(&mut self) -> Result<Option<CommitRecord>, Error> {
        Ok(self.record)
    }
    fn commit(&mut self, record: &CommitRecord, outputs: &[(u64, ZSet)]) -> Result<(), Error> {
        check_commit(self.record.as_ref(), record, outputs)?;
        self.outputs.extend(outputs.iter().cloned());
        self.record = Some(*record);
        Ok(())
    }
    fn crash(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn outputs(&mut self) -> Result<Vec<(u64, ZSet)>, Error> {
        Ok(self.outputs.clone())
    }
}

/// How a device behaves when the process holding it crashes.
pub trait CrashMedium: Device + Sized {
    /// The device as the next incarnation finds it.
    fn after_crash(self) -> Self;
}
/// A process crash: the OS keeps every completed write, so the files are
/// reopened as they are (Quarry recovery still runs).
impl CrashMedium for FsDevice {
    fn after_crash(self) -> Self {
        self
    }
}
/// The stricter power-loss model: only synced bytes survive.
impl CrashMedium for SimDevice {
    fn after_crash(self) -> Self {
        self.crash(vesper_quarry::device::CrashChoice::LoseUnsynced)
    }
}

/// A borrowed simulated device (lets a test keep it across a failed open).
impl CrashMedium for &mut SimDevice {
    fn after_crash(self) -> Self {
        *self = self.crash(vesper_quarry::device::CrashChoice::LoseUnsynced);
        self
    }
}

/// What [`QuarrySink::open`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SinkRecovery {
    pub record: Option<CommitRecord>,
    pub output_rows: usize,
    /// Transactions Quarry rolled back during recovery.
    pub rolled_back: usize,
}

const RECORD_TAG: u8 = 1;
const OUTPUT_TAG: u8 = 2;
/// One per committed epoch, carrying the delta's width, so an empty delta
/// round-trips and every committed epoch stays visible.
const EPOCH_TAG: u8 = 3;
const EPOCH_BYTES: usize = 1 + 8 + 4;
const RECORD_VERSION: u16 = 1;
const RECORD_BYTES: usize = 1 + 2 + 32 + 8 + 4 + 32;
/// Commits between Quarry checkpoints (which truncate the log).
const CHECKPOINT_EVERY: u64 = 8;

fn dberr(e: DbError) -> Error {
    Error::Storage(match e.kind {
        DbErrorKind::Io => "sink io",
        DbErrorKind::Crashed => "sink crashed",
        DbErrorKind::Corrupt => "sink corrupt",
        DbErrorKind::Limit | DbErrorKind::LogFull | DbErrorKind::Full => "sink full",
        _ => "sink",
    })
}

fn encode_record(r: &CommitRecord) -> Vec<u8> {
    let mut b = Vec::with_capacity(RECORD_BYTES);
    b.push(RECORD_TAG);
    b.extend_from_slice(&RECORD_VERSION.to_le_bytes());
    b.extend_from_slice(&r.fingerprint);
    b.extend_from_slice(&r.after_epoch.to_le_bytes());
    b.extend_from_slice(&r.workers.to_le_bytes());
    b.extend_from_slice(r.global.digest.as_bytes());
    b
}
fn decode_record(b: &[u8]) -> Result<CommitRecord, Error> {
    if b.len() != RECORD_BYTES || b[0] != RECORD_TAG || b[1..3] != RECORD_VERSION.to_le_bytes() {
        return Err(Error::Storage("sink record"));
    }
    let arr = |r: std::ops::Range<usize>| -> Result<&[u8], Error> {
        b.get(r).ok_or(Error::Storage("sink record"))
    };
    Ok(CommitRecord {
        fingerprint: arr(3..35)?
            .try_into()
            .map_err(|_| Error::Storage("sink record"))?,
        after_epoch: u64::from_le_bytes(
            arr(35..43)?
                .try_into()
                .map_err(|_| Error::Storage("sink record"))?,
        ),
        workers: u32::from_le_bytes(
            arr(43..47)?
                .try_into()
                .map_err(|_| Error::Storage("sink record"))?,
        ),
        global: blob_id(
            arr(47..79)?
                .try_into()
                .map_err(|_| Error::Storage("sink record"))?,
        ),
    })
}
fn encode_output(epoch: u64, row: &Row, weight: i64) -> Result<Vec<u8>, Error> {
    let len = 1 + 8 + 8 + 4 + 8 * row.len();
    if len > MAX_VALUE {
        return Err(Error::Limit("sink row bytes"));
    }
    let mut b = Vec::with_capacity(len);
    b.push(OUTPUT_TAG);
    b.extend_from_slice(&epoch.to_le_bytes());
    b.extend_from_slice(&weight.to_le_bytes());
    b.extend_from_slice(
        &u32::try_from(row.len())
            .map_err(|_| Error::Overflow)?
            .to_le_bytes(),
    );
    for c in row {
        b.extend_from_slice(&c.to_le_bytes());
    }
    Ok(b)
}
fn encode_epoch(epoch: u64, width: usize) -> Result<Vec<u8>, Error> {
    let mut b = Vec::with_capacity(EPOCH_BYTES);
    b.push(EPOCH_TAG);
    b.extend_from_slice(&epoch.to_le_bytes());
    b.extend_from_slice(
        &u32::try_from(width)
            .map_err(|_| Error::Overflow)?
            .to_le_bytes(),
    );
    Ok(b)
}
fn decode_epoch(b: &[u8], limits: Limits) -> Result<(u64, usize), Error> {
    let bad = || Error::Storage("sink epoch");
    if b.len() != EPOCH_BYTES || b[0] != EPOCH_TAG {
        return Err(bad());
    }
    let epoch = u64::from_le_bytes(b[1..9].try_into().map_err(|_| bad())?);
    let width = usize::try_from(u32::from_le_bytes(b[9..13].try_into().map_err(|_| bad())?))
        .map_err(|_| Error::Overflow)?;
    if width > limits.max_columns {
        return Err(bad());
    }
    Ok((epoch, width))
}
fn decode_output(b: &[u8], limits: Limits) -> Result<(u64, Row, i64), Error> {
    let bad = || Error::Storage("sink output");
    if b.len() < 21 || b[0] != OUTPUT_TAG {
        return Err(bad());
    }
    let epoch = u64::from_le_bytes(b[1..9].try_into().map_err(|_| bad())?);
    let weight = i64::from_le_bytes(b[9..17].try_into().map_err(|_| bad())?);
    let width = usize::try_from(u32::from_le_bytes(b[17..21].try_into().map_err(|_| bad())?))
        .map_err(|_| Error::Overflow)?;
    if width > limits.max_columns || b.len() != 21 + 8 * width || weight == 0 {
        return Err(bad());
    }
    let row = b[21..]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| i64::from_le_bytes(*c))
        .collect();
    Ok((epoch, row, weight))
}

/// Exactly-once sink in a Vesper Quarry durable store (see the module docs).
/// One output record per (epoch, row, weight) plus one commit record, all
/// written in one Quarry transaction per checkpoint commit.
pub struct QuarrySink<D: CrashMedium> {
    db: Option<Db<D>>,
    db_limits: DbLimits,
    limits: Limits,
    record: Option<(RecordId, CommitRecord)>,
    commits: u64,
}
impl<D: CrashMedium> std::fmt::Debug for QuarrySink<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuarrySink")
            .field("record", &self.record)
            .finish()
    }
}
impl<D: CrashMedium> QuarrySink<D> {
    /// Open (creating if empty) and recover the store, then read the record.
    /// `limits` bound the decoded output rows (columns per row).
    pub fn open(
        dev: D,
        db_limits: DbLimits,
        limits: Limits,
    ) -> Result<(Self, SinkRecovery), Error> {
        let (db, report) = Db::open(dev, db_limits).map_err(dberr)?;
        let mut sink = Self {
            db: Some(db),
            db_limits,
            limits,
            record: None,
            commits: 0,
        };
        let rows = sink.load()?;
        let record = sink.record.map(|(_, r)| r);
        Ok((
            sink,
            SinkRecovery {
                record,
                output_rows: rows,
                rolled_back: report.rolled_back.len(),
            },
        ))
    }
    fn db(&mut self) -> Result<&mut Db<D>, Error> {
        self.db.as_mut().ok_or(Error::Storage("sink unavailable"))
    }
    /// Scan for the single commit record; check every output record decodes
    /// and lies at or before the record's offset.
    fn load(&mut self) -> Result<usize, Error> {
        let limits = self.limits;
        let rows = self.db()?.scan().map_err(dberr)?;
        let mut record = None;
        let mut outputs = 0usize;
        let mut max_epoch = None;
        let mut widths = BTreeMap::new();
        let mut row_widths = Vec::new();
        for (id, bytes) in &rows {
            match bytes.first() {
                Some(&RECORD_TAG) => {
                    if record.replace((*id, decode_record(bytes)?)).is_some() {
                        return Err(Error::Storage("sink has two commit records"));
                    }
                }
                Some(&OUTPUT_TAG) => {
                    let (epoch, row, _) = decode_output(bytes, limits)?;
                    max_epoch = max_epoch.max(Some(epoch));
                    row_widths.push((epoch, row.len()));
                    outputs += 1;
                }
                Some(&EPOCH_TAG) => {
                    let (epoch, width) = decode_epoch(bytes, limits)?;
                    if widths.insert(epoch, width).is_some() {
                        return Err(Error::Storage("sink epoch named twice"));
                    }
                    max_epoch = max_epoch.max(Some(epoch));
                }
                _ => return Err(Error::Storage("sink record tag")),
            }
        }
        match (record, max_epoch) {
            (None, Some(_)) => return Err(Error::Storage("sink outputs without a record")),
            (Some((_, r)), Some(e)) if e > r.after_epoch => {
                return Err(Error::Storage("sink output after its record"));
            }
            _ => {}
        }
        for (epoch, width) in row_widths {
            if widths.get(&epoch) != Some(&width) {
                return Err(Error::Storage("sink output width"));
            }
        }
        self.record = record;
        Ok(outputs)
    }
    /// The device, for crash tests.
    pub fn device(&self) -> Option<&D> {
        self.db.as_ref().map(|d| d.device())
    }
    /// Give up the store without writing anything more.
    pub fn into_device(self) -> Option<D> {
        self.db.map(|d| d.into_device())
    }
    fn commit_txn(
        db: &mut Db<D>,
        txn: &mut Txn,
        at: Option<RecordId>,
        record: &CommitRecord,
        outputs: &[(u64, ZSet)],
    ) -> Result<RecordId, Error> {
        for (epoch, delta) in outputs {
            db.insert(txn, &encode_epoch(*epoch, delta.width)?)
                .map_err(dberr)?;
            for (row, weight) in delta.entries() {
                db.insert(txn, &encode_output(*epoch, row, weight)?)
                    .map_err(dberr)?;
            }
        }
        let bytes = encode_record(record);
        match at {
            Some(id) => {
                db.update(txn, id, &bytes).map_err(dberr)?;
                Ok(id)
            }
            None => db.insert(txn, &bytes).map_err(dberr),
        }
    }
}
impl<D: CrashMedium> CommitSink for QuarrySink<D> {
    fn last_commit(&mut self) -> Result<Option<CommitRecord>, Error> {
        self.db()?;
        Ok(self.record.map(|(_, r)| r))
    }
    fn commit(&mut self, record: &CommitRecord, outputs: &[(u64, ZSet)]) -> Result<(), Error> {
        let previous = self.record;
        check_commit(previous.as_ref().map(|(_, r)| r), record, outputs)?;
        let db = self.db()?;
        let mut txn = db.begin().map_err(dberr)?;
        match Self::commit_txn(db, &mut txn, previous.map(|(id, _)| id), record, outputs) {
            Ok(id) => {
                db.commit(txn).map_err(dberr)?;
                self.record = Some((id, *record));
            }
            Err(e) => {
                // Roll back; if even that fails the store is poisoned and the
                // next crash recovery rolls the transaction back.
                let _ = db.abort(txn);
                return Err(e);
            }
        }
        self.commits += 1;
        if self.commits.is_multiple_of(CHECKPOINT_EVERY) {
            self.db()?.checkpoint().map_err(dberr)?;
        }
        Ok(())
    }
    fn crash(&mut self) -> Result<(), Error> {
        let dev = self
            .db
            .take()
            .ok_or(Error::Storage("sink unavailable"))?
            .into_device()
            .after_crash();
        let (db, _) = Db::open(dev, self.db_limits).map_err(dberr)?;
        self.db = Some(db);
        self.record = None;
        self.load()?;
        Ok(())
    }
    fn outputs(&mut self) -> Result<Vec<(u64, ZSet)>, Error> {
        let limits = self.limits;
        let rows = self.db()?.scan().map_err(dberr)?;
        let mut by_epoch: BTreeMap<u64, (usize, Vec<(Row, i64)>)> = BTreeMap::new();
        for (_, bytes) in &rows {
            if bytes.first() == Some(&EPOCH_TAG) {
                let (epoch, width) = decode_epoch(bytes, limits)?;
                by_epoch.entry(epoch).or_default().0 = width;
            }
        }
        for (_, bytes) in &rows {
            if bytes.first() == Some(&OUTPUT_TAG) {
                let (epoch, row, weight) = decode_output(bytes, limits)?;
                match by_epoch.get_mut(&epoch) {
                    Some((width, rows)) if *width == row.len() => rows.push((row, weight)),
                    _ => return Err(Error::Storage("sink output width")),
                }
            }
        }
        let unbounded = Limits {
            max_input_entries: usize::MAX,
            max_rows: usize::MAX,
            max_work: usize::MAX,
            ..limits
        };
        by_epoch
            .into_iter()
            .map(|(e, (width, rows))| Ok((e, ZSet::consolidate(width, &rows, unbounded)?)))
            .collect()
    }
}
