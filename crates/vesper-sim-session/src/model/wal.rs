//! A bounded, deterministic virtual write-ahead log.
//!
//! The log has three regions:
//! - `volatile`: records appended in this process epoch and not yet handed to
//!   a flush. They exist only in process memory.
//! - `in_flight`: the single batch being flushed. It is not durable until the
//!   flush completes. A crash persists an explicit, recorded prefix of it and
//!   optionally a torn prefix of the next frame.
//! - `durable`: the bytes of the simulated medium, written in LSN order.
//!
//! Durable bytes are a sequence of frames
//! `[payload_len: u32 LE][lsn: u64 LE][check: 8 bytes][payload]`, where
//! `check` is the first eight bytes of SHA-256 over `lsn LE || payload`.
//! Recovery (policy `wal-frame-v1`) scans frames from offset 0 and accepts the
//! longest prefix whose frames are complete, whose checks match, whose payload
//! is UTF-8 and whose LSNs are exactly 1, 2, 3, ...; the first violation and
//! everything after it is discarded and truncated from the medium. Nothing
//! here touches a real file, clock or random source.
use serde::{Deserialize, Serialize};
#[cfg(not(feature = "vesper-port"))]
use sha2::{Digest, Sha256};
use std::borrow::Cow;

pub const FRAME_HEADER_BYTES: usize = 20;

/// One record as appended by source: its LSN and canonical JSON payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalRecord {
    pub lsn: u64,
    pub payload: String,
}
impl WalRecord {
    #[cfg(feature = "vesper-port")]
    pub fn frame(&self) -> Vec<u8> {
        crate::port::wal_frame(self.lsn, &self.payload)
    }
    #[cfg(not(feature = "vesper-port"))]
    pub fn frame(&self) -> Vec<u8> {
        let payload = self.payload.as_bytes();
        let mut out = Vec::with_capacity(FRAME_HEADER_BYTES + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.lsn.to_le_bytes());
        out.extend_from_slice(&check(self.lsn, payload));
        out.extend_from_slice(payload);
        out
    }
    pub fn frame_len(&self) -> usize {
        FRAME_HEADER_BYTES + self.payload.len()
    }
}

#[cfg(not(feature = "vesper-port"))]
fn check(lsn: u64, payload: &[u8]) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(lsn.to_le_bytes());
    hasher.update(payload);
    let digest = hasher.finalize();
    let mut out = [0; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// Why recovery stopped before the end of the medium.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TailDamage {
    /// Fewer bytes than a frame header remain.
    TornHeader,
    /// The header is complete but the payload is cut short.
    TornPayload,
    /// The frame is complete but its check does not match.
    BadCheck,
    /// The frame is intact but its LSN is not the next expected one.
    OutOfOrder { expected: u64, found: u64 },
    /// The frame is intact but the payload is not UTF-8.
    BadPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovery {
    pub records: Vec<WalRecord>,
    pub valid_bytes: usize,
    pub discarded_bytes: usize,
    pub damage: Option<TailDamage>,
}

/// The port's damage code, expected LSN and found LSN.
#[cfg(feature = "vesper-port")]
fn damage(code: (u8, u64, u64)) -> Option<TailDamage> {
    match code {
        (0, _, _) => None,
        (1, _, _) => Some(TailDamage::TornHeader),
        (2, _, _) => Some(TailDamage::TornPayload),
        (3, _, _) => Some(TailDamage::BadCheck),
        (4, expected, found) => Some(TailDamage::OutOfOrder { expected, found }),
        (5, _, _) => Some(TailDamage::BadPayload),
        (other, _, _) => panic!("invalid tail damage {other} from Vesper"),
    }
}

/// Scan a medium with the `wal-frame-v1` policy. This never fails: damage is
/// reported and the valid prefix is returned.
#[cfg(feature = "vesper-port")]
pub fn scan(bytes: &[u8]) -> Recovery {
    let result = crate::port::wal_scan(bytes);
    let records = crate::port::scan_result_records(&result)
        .chunks_exact(3)
        .map(|r| {
            let (start, len) = (r[1] as usize, r[2] as usize);
            WalRecord {
                lsn: r[0],
                payload: std::str::from_utf8(&bytes[start..start + len])
                    .expect("the port validated the payload")
                    .into(),
            }
        })
        .collect();
    let valid_bytes = crate::port::scan_result_valid_bytes(&result) as usize;
    let damage = damage(crate::port::scan_result_damage(&result));
    Recovery {
        records,
        valid_bytes,
        discarded_bytes: bytes.len() - valid_bytes,
        damage,
    }
}

/// Scan a medium with the `wal-frame-v1` policy. This never fails: damage is
/// reported and the valid prefix is returned.
#[cfg(not(feature = "vesper-port"))]
pub fn scan(bytes: &[u8]) -> Recovery {
    let mut records = vec![];
    let mut offset = 0;
    let mut damage = None;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        if rest.len() < FRAME_HEADER_BYTES {
            damage = Some(TailDamage::TornHeader);
            break;
        }
        let len = u32::from_le_bytes(rest[0..4].try_into().unwrap()) as usize;
        let lsn = u64::from_le_bytes(rest[4..12].try_into().unwrap());
        if rest.len() - FRAME_HEADER_BYTES < len {
            damage = Some(TailDamage::TornPayload);
            break;
        }
        let payload = &rest[FRAME_HEADER_BYTES..FRAME_HEADER_BYTES + len];
        if rest[12..20] != check(lsn, payload) {
            damage = Some(TailDamage::BadCheck);
            break;
        }
        let expected = records.len() as u64 + 1;
        if lsn != expected {
            damage = Some(TailDamage::OutOfOrder {
                expected,
                found: lsn,
            });
            break;
        }
        let Ok(payload) = std::str::from_utf8(payload) else {
            damage = Some(TailDamage::BadPayload);
            break;
        };
        records.push(WalRecord {
            lsn,
            payload: payload.into(),
        });
        offset += FRAME_HEADER_BYTES + len;
    }
    Recovery {
        records,
        valid_bytes: offset,
        discarded_bytes: bytes.len() - offset,
        damage,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalLimits {
    /// Records across durable, in-flight and volatile regions.
    pub max_records: u64,
    /// Framed bytes across durable, in-flight and volatile regions.
    pub max_bytes: u64,
    /// Payload bytes of one record.
    pub max_record_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalError {
    /// `recover` must be the first operation on a log in each process epoch.
    NotRecovered,
    AlreadyRecovered,
    /// Only one flush may be in flight per log (unsupported otherwise).
    FlushInFlight,
    NoFlushInFlight,
    RecordTooLarge {
        bytes: u64,
        limit: u64,
    },
    LogFull {
        records: u64,
        bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub request: u64,
    pub records: Vec<WalRecord>,
}

/// The outcome of a crash for one log, after clamping the requested choice to
/// the data actually in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrashApplied {
    pub volatile_lost: u64,
    pub in_flight_records: u64,
    pub persisted_records: u64,
    pub torn_bytes: u64,
}

#[cfg(not(feature = "vesper-port"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wal {
    limits: WalLimits,
    durable: Vec<u8>,
    /// Durable records parsed by this epoch's recovery plus later flushes.
    view: Vec<WalRecord>,
    volatile: Vec<WalRecord>,
    in_flight: Option<Batch>,
    next_lsn: u64,
    recovered: bool,
    /// Bumped by every crash, so completions scheduled before it are stale.
    generation: u64,
}

#[cfg(not(feature = "vesper-port"))]
impl Wal {
    pub fn new(limits: WalLimits) -> Self {
        Self {
            limits,
            durable: vec![],
            view: vec![],
            volatile: vec![],
            in_flight: None,
            next_lsn: 1,
            recovered: false,
            generation: 0,
        }
    }
    pub fn durable_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(&self.durable)
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn is_recovered(&self) -> bool {
        self.recovered
    }
    pub fn volatile(&self) -> Vec<WalRecord> {
        self.volatile.clone()
    }
    pub fn in_flight(&self) -> Option<Batch> {
        self.in_flight.clone()
    }
    /// The `index`th durable record visible to this epoch (none before
    /// recovery).
    pub fn record(&self, index: usize) -> Option<WalRecord> {
        self.view.get(index).cloned()
    }
    /// Highest durable LSN visible to this epoch, or 0.
    pub fn durable_lsn(&self) -> u64 {
        self.view.last().map_or(0, |r| r.lsn)
    }

    /// Open the log for this epoch: scan, truncate damage, and continue LSNs
    /// after the last valid record.
    pub fn recover(&mut self) -> Result<Recovery, WalError> {
        if self.recovered {
            return Err(WalError::AlreadyRecovered);
        }
        let recovery = scan(&self.durable);
        self.durable.truncate(recovery.valid_bytes);
        self.view = recovery.records.clone();
        self.next_lsn = self.durable_lsn() + 1;
        self.recovered = true;
        Ok(recovery)
    }

    fn usage(&self) -> (u64, u64) {
        let pending = self
            .volatile
            .iter()
            .chain(self.in_flight.iter().flat_map(|b| &b.records));
        let (mut records, mut bytes) = (self.view.len() as u64, self.durable.len() as u64);
        for record in pending {
            records += 1;
            bytes += record.frame_len() as u64;
        }
        (records, bytes)
    }

    /// Check that `payload` can be appended without exceeding any bound.
    pub fn check_append(&self, payload: &str) -> Result<(), WalError> {
        if !self.recovered {
            return Err(WalError::NotRecovered);
        }
        let len = payload.len() as u64;
        if len > self.limits.max_record_bytes || len > u64::from(u32::MAX) {
            return Err(WalError::RecordTooLarge {
                bytes: len,
                limit: self.limits.max_record_bytes,
            });
        }
        let (records, bytes) = self.usage();
        let records = records + 1;
        let bytes = bytes + FRAME_HEADER_BYTES as u64 + len;
        if records > self.limits.max_records || bytes > self.limits.max_bytes {
            return Err(WalError::LogFull { records, bytes });
        }
        Ok(())
    }

    /// Append to the volatile region and return the assigned LSN.
    pub fn append(&mut self, payload: String) -> Result<u64, WalError> {
        self.check_append(&payload)?;
        let lsn = self.next_lsn;
        self.next_lsn += 1;
        self.volatile.push(WalRecord { lsn, payload });
        Ok(lsn)
    }

    pub fn check_flush(&self) -> Result<(), WalError> {
        if !self.recovered {
            return Err(WalError::NotRecovered);
        }
        if self.in_flight.is_some() {
            return Err(WalError::FlushInFlight);
        }
        Ok(())
    }

    /// Move every volatile record into the in-flight batch.
    pub fn begin_flush(&mut self, request: u64) -> Result<Batch, WalError> {
        self.check_flush()?;
        let records = std::mem::take(&mut self.volatile);
        Ok(self.in_flight.insert(Batch { request, records }).clone())
    }

    /// The device acknowledged the flush: the batch becomes durable.
    pub fn complete_flush(&mut self) -> Result<Batch, WalError> {
        let batch = self.in_flight.take().ok_or(WalError::NoFlushInFlight)?;
        for record in &batch.records {
            self.durable.extend_from_slice(&record.frame());
            self.view.push(record.clone());
        }
        Ok(batch)
    }

    /// The device reported failure: nothing in the batch became durable and
    /// its records return, oldest first, to the volatile region.
    pub fn fail_flush(&mut self) -> Result<Batch, WalError> {
        let batch = self.in_flight.take().ok_or(WalError::NoFlushInFlight)?;
        let mut records = batch.records.clone();
        records.append(&mut self.volatile);
        self.volatile = records;
        Ok(batch)
    }

    /// Process crash: volatile records vanish; the first `persisted` in-flight
    /// records reach the medium whole and, if a further record exists,
    /// `torn` bytes of its frame (clamped below the full frame) follow.
    pub fn crash(&mut self, persisted: u64, torn: u64) -> CrashApplied {
        let batch = self.in_flight.take().map(|b| b.records).unwrap_or_default();
        let in_flight_records = batch.len() as u64;
        let persisted = persisted.min(in_flight_records);
        for record in &batch[..persisted as usize] {
            self.durable.extend_from_slice(&record.frame());
        }
        let torn = match batch.get(persisted as usize) {
            Some(next) if torn > 0 => {
                let frame = next.frame();
                let torn = torn.min(frame.len() as u64 - 1);
                self.durable.extend_from_slice(&frame[..torn as usize]);
                torn
            }
            _ => 0,
        };
        let volatile_lost = self.volatile.len() as u64;
        self.volatile.clear();
        self.view.clear();
        self.recovered = false;
        self.generation += 1;
        CrashApplied {
            volatile_lost,
            in_flight_records,
            persisted_records: persisted,
            torn_bytes: torn,
        }
    }
}

/// Under `vesper-port` the Vesper state (port/src/lib.ves) owns the medium,
/// the regions, the bounds and the crash rules; this wrapper keeps each live
/// record's payload, keyed by LSN.
#[cfg(feature = "vesper-port")]
pub struct Wal {
    state: crate::port::WalState,
    payloads: std::collections::BTreeMap<u64, String>,
}

#[cfg(feature = "vesper-port")]
impl Clone for Wal {
    fn clone(&self) -> Self {
        Self {
            state: crate::port::WalState::clone(&self.state),
            payloads: self.payloads.clone(),
        }
    }
}

#[cfg(feature = "vesper-port")]
impl PartialEq for Wal {
    fn eq(&self, other: &Self) -> bool {
        self.payloads == other.payloads && crate::port::WalState::same(&self.state, &other.state)
    }
}

#[cfg(feature = "vesper-port")]
impl Eq for Wal {}

#[cfg(feature = "vesper-port")]
impl std::fmt::Debug for Wal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wal")
            .field("durable", &hex(&self.durable_bytes()))
            .field("in_flight", &self.in_flight())
            .field("volatile", &self.volatile())
            .field("generation", &self.generation())
            .finish()
    }
}

#[cfg(feature = "vesper-port")]
fn wal_result(r: crate::port::WalResult) -> Result<u64, WalError> {
    use crate::port::WalResult as R;
    match r {
        R::Ok(v) => Ok(v),
        R::NotRecovered => Err(WalError::NotRecovered),
        R::AlreadyRecovered => Err(WalError::AlreadyRecovered),
        R::FlushInFlight => Err(WalError::FlushInFlight),
        R::NoFlushInFlight => Err(WalError::NoFlushInFlight),
        R::RecordTooLarge(bytes, limit) => Err(WalError::RecordTooLarge { bytes, limit }),
        R::LogFull(records, bytes) => Err(WalError::LogFull { records, bytes }),
    }
}

#[cfg(feature = "vesper-port")]
impl Wal {
    pub fn new(limits: WalLimits) -> Self {
        Self {
            state: crate::port::WalState::new(
                limits.max_records,
                limits.max_bytes,
                limits.max_record_bytes,
            ),
            payloads: std::collections::BTreeMap::new(),
        }
    }
    pub fn durable_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Owned(crate::port::wal_state_durable(&self.state))
    }
    pub fn generation(&self) -> u64 {
        crate::port::wal_state_generation(&self.state)
    }
    pub fn is_recovered(&self) -> bool {
        crate::port::wal_state_is_recovered(&self.state)
    }
    fn records(&self, lsns: Vec<u64>) -> Vec<WalRecord> {
        lsns.into_iter()
            .map(|lsn| WalRecord {
                lsn,
                payload: self.payloads[&lsn].clone(),
            })
            .collect()
    }
    pub fn volatile(&self) -> Vec<WalRecord> {
        self.records(crate::port::wal_state_volatile_lsns(&self.state))
    }
    pub fn in_flight(&self) -> Option<Batch> {
        crate::port::wal_state_is_flushing(&self.state).then(|| Batch {
            request: crate::port::wal_state_request(&self.state),
            records: self.records(crate::port::wal_state_in_flight_lsns(&self.state)),
        })
    }
    /// The `index`th durable record visible to this epoch (none before
    /// recovery).
    pub fn record(&self, index: usize) -> Option<WalRecord> {
        // LSNs start at 1; 0 is the port's "past the end".
        match crate::port::wal_state_view_lsn(&self.state, index as u64) {
            0 => None,
            lsn => self.records(vec![lsn]).pop(),
        }
    }
    /// Highest durable LSN visible to this epoch, or 0.
    pub fn durable_lsn(&self) -> u64 {
        crate::port::wal_state_durable_lsn(&self.state)
    }

    /// Open the log for this epoch: scan, truncate damage, and continue LSNs
    /// after the last valid record.
    pub fn recover(&mut self) -> Result<Recovery, WalError> {
        let discarded_from = crate::port::wal_state_durable_len(&self.state);
        let recovery = crate::port::wal_state_recover(&mut self.state);
        if !crate::port::wal_recovery_ok(&recovery) {
            return Err(WalError::AlreadyRecovered);
        }
        let durable = self.durable_bytes();
        let records: Vec<WalRecord> = crate::port::wal_recovery_records(&recovery)
            .chunks_exact(3)
            .map(|r| {
                let (start, len) = (r[1] as usize, r[2] as usize);
                WalRecord {
                    lsn: r[0],
                    payload: std::str::from_utf8(&durable[start..start + len])
                        .expect("the port validated the payload")
                        .into(),
                }
            })
            .collect();
        let valid_bytes = crate::port::wal_recovery_valid_bytes(&recovery) as usize;
        let damage = damage(crate::port::wal_recovery_damage(&recovery));
        self.payloads = records.iter().map(|r| (r.lsn, r.payload.clone())).collect();
        Ok(Recovery {
            records,
            valid_bytes,
            discarded_bytes: discarded_from as usize - valid_bytes,
            damage,
        })
    }

    /// Check that `payload` can be appended without exceeding any bound.
    pub fn check_append(&self, payload: &str) -> Result<(), WalError> {
        wal_result(crate::port::wal_state_check_append(
            &self.state,
            payload.len() as u64,
        ))
        .map(drop)
    }

    /// Append to the volatile region and return the assigned LSN.
    pub fn append(&mut self, payload: String) -> Result<u64, WalError> {
        let lsn = wal_result(crate::port::wal_state_append(&mut self.state, &payload))?;
        self.payloads.insert(lsn, payload);
        Ok(lsn)
    }

    pub fn check_flush(&self) -> Result<(), WalError> {
        wal_result(crate::port::wal_state_check_flush(&self.state)).map(drop)
    }

    /// Move every volatile record into the in-flight batch.
    pub fn begin_flush(&mut self, request: u64) -> Result<Batch, WalError> {
        wal_result(crate::port::wal_state_begin_flush(&mut self.state, request))?;
        Ok(self.in_flight().expect("the port began a flush"))
    }

    /// The device acknowledged the flush: the batch becomes durable.
    pub fn complete_flush(&mut self) -> Result<Batch, WalError> {
        let batch = self.in_flight();
        wal_result(crate::port::wal_state_complete_flush(&mut self.state))?;
        Ok(batch.expect("the port completed a flush"))
    }

    /// The device reported failure: nothing in the batch became durable and
    /// its records return, oldest first, to the volatile region.
    pub fn fail_flush(&mut self) -> Result<Batch, WalError> {
        let batch = self.in_flight();
        wal_result(crate::port::wal_state_fail_flush(&mut self.state))?;
        Ok(batch.expect("the port failed a flush"))
    }

    /// Process crash: volatile records vanish; the first `persisted` in-flight
    /// records reach the medium whole and, if a further record exists,
    /// `torn` bytes of its frame (clamped below the full frame) follow.
    pub fn crash(&mut self, persisted: u64, torn: u64) -> CrashApplied {
        let c = crate::port::wal_state_crash(&mut self.state, persisted, torn);
        self.payloads.clear();
        CrashApplied {
            volatile_lost: c.volatile_lost,
            in_flight_records: c.in_flight_records,
            persisted_records: c.persisted_records,
            torn_bytes: c.torn_bytes,
        }
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wal() -> Wal {
        Wal::new(WalLimits {
            max_records: 8,
            max_bytes: 512,
            max_record_bytes: 64,
        })
    }

    #[test]
    fn unflushed_records_vanish_and_flushed_records_survive() {
        let mut log = wal();
        assert_eq!(log.append("1".into()), Err(WalError::NotRecovered));
        assert!(log.recover().unwrap().records.is_empty());
        assert_eq!(log.append("10".into()), Ok(1));
        assert_eq!(log.append("11".into()), Ok(2));
        log.begin_flush(7).unwrap();
        assert_eq!(log.append("12".into()), Ok(3));
        assert_eq!(log.check_flush(), Err(WalError::FlushInFlight));
        log.complete_flush().unwrap();
        assert_eq!(log.durable_lsn(), 2);
        let applied = log.crash(0, 0);
        assert_eq!(applied.volatile_lost, 1);
        let recovery = log.recover().unwrap();
        assert_eq!(
            recovery.records.iter().map(|r| r.lsn).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(recovery.damage, None);
        // LSNs continue after the last durable record.
        assert_eq!(log.append("13".into()), Ok(3));
    }

    #[test]
    fn crashes_persist_an_explicit_prefix_and_torn_tails_are_truncated() {
        let mut log = wal();
        log.recover().unwrap();
        for p in ["\"a\"", "\"b\"", "\"c\""] {
            log.append(p.into()).unwrap();
        }
        log.begin_flush(1).unwrap();
        let applied = log.crash(1, 7);
        assert_eq!(
            applied,
            CrashApplied {
                volatile_lost: 0,
                in_flight_records: 3,
                persisted_records: 1,
                torn_bytes: 7
            }
        );
        assert_eq!(log.durable_bytes().len(), FRAME_HEADER_BYTES + 3 + 7);
        let recovery = log.recover().unwrap();
        assert_eq!(recovery.records.len(), 1);
        assert_eq!(recovery.damage, Some(TailDamage::TornHeader));
        assert_eq!(recovery.discarded_bytes, 7);
        assert_eq!(log.durable_bytes().len(), FRAME_HEADER_BYTES + 3);

        // A torn payload (header complete) is also discarded.
        log.append("\"long payload\"".into()).unwrap();
        log.begin_flush(2).unwrap();
        log.crash(0, FRAME_HEADER_BYTES as u64 + 2);
        let recovery = log.recover().unwrap();
        assert_eq!(recovery.damage, Some(TailDamage::TornPayload));
        assert_eq!(recovery.records.len(), 1);

        // Requests beyond the in-flight data are clamped, never invented.
        log.append("\"z\"".into()).unwrap();
        log.begin_flush(3).unwrap();
        let applied = log.crash(5, 1000);
        assert_eq!((applied.persisted_records, applied.torn_bytes), (1, 0));
        assert_eq!(log.recover().unwrap().records.len(), 2);
    }

    #[test]
    fn recovery_rejects_bad_checks_and_out_of_order_frames() {
        let one = WalRecord {
            lsn: 1,
            payload: "1".into(),
        };
        let three = WalRecord {
            lsn: 3,
            payload: "3".into(),
        };
        let mut bytes = one.frame();
        bytes.extend(three.frame());
        let r = scan(&bytes);
        assert_eq!(r.records, vec![one.clone()]);
        assert_eq!(
            r.damage,
            Some(TailDamage::OutOfOrder {
                expected: 2,
                found: 3
            })
        );
        let mut corrupt = one.frame();
        *corrupt.last_mut().unwrap() ^= 1;
        let r = scan(&corrupt);
        assert_eq!((r.records.len(), r.damage), (0, Some(TailDamage::BadCheck)));
        assert_eq!(r.discarded_bytes, corrupt.len());
    }

    #[test]
    fn failed_flushes_keep_records_volatile_in_order_and_bounds_hold() {
        let mut log = wal();
        log.recover().unwrap();
        log.append("1".into()).unwrap();
        log.begin_flush(1).unwrap();
        log.append("2".into()).unwrap();
        log.fail_flush().unwrap();
        assert_eq!(
            log.volatile().iter().map(|r| r.lsn).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(log.durable_lsn(), 0);
        assert!(matches!(
            log.check_append(&"x".repeat(65)),
            Err(WalError::RecordTooLarge { .. })
        ));
        for _ in 0..6 {
            log.append("3".into()).unwrap();
        }
        assert!(matches!(
            log.append("4".into()),
            Err(WalError::LogFull { records: 9, .. })
        ));
        assert_eq!(log.recover(), Err(WalError::AlreadyRecovered));
    }
}
