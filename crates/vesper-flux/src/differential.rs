//! Differential tests of the Vesper accumulator, CDC decoder and input-log
//! codec against the Rust code they replaced (the last Rust commit's
//! `zset::Accumulator`, `Executor::ingest_cdc` record loop and `log.rs`
//! codec, kept here as the reference). Every case runs both and compares
//! the outcome, the sets, the bytes and the work left.
use crate::zset::Accumulator;
use crate::{Budget, CdcHeader, CdcPolicy, Error, Limits, Row, SourceBinding, WeightPolicy, ZSet};
use std::collections::BTreeMap;

/// The Rust accumulator as it was before the port (one per input).
struct Reference {
    width: usize,
    limits: Limits,
    rows: BTreeMap<Row, i128>,
}
impl Reference {
    fn new(width: usize, limits: Limits) -> Result<Self, Error> {
        if width > limits.max_columns {
            return Err(Error::Limit("columns"));
        }
        Ok(Self {
            width,
            limits,
            rows: BTreeMap::new(),
        })
    }
    fn add(&mut self, row: &[i64], weight: i128, budget: &mut Budget) -> Result<(), Error> {
        budget.charge(1usize.checked_add(row.len()).ok_or(Error::Overflow)?)?;
        if row.len() != self.width {
            return Err(Error::Schema);
        }
        if weight == 0 {
            return Ok(());
        }
        if !self.rows.contains_key(row) && self.rows.len() >= self.limits.max_rows {
            return Err(Error::Limit("rows"));
        }
        let old = self.rows.get(row).copied().unwrap_or(0);
        self.rows.insert(
            row.to_vec(),
            old.checked_add(weight).ok_or(Error::Overflow)?,
        );
        Ok(())
    }
    fn finish(self) -> Result<ZSet, Error> {
        let mut rows = BTreeMap::new();
        for (row, w) in self.rows {
            if w != 0 {
                rows.insert(row, i64::try_from(w).map_err(|_| Error::Overflow)?);
            }
        }
        Ok(ZSet::from_rows(self.width, rows))
    }
}

/// A small deterministic generator (SplitMix64).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn cell(&mut self) -> i64 {
        match self.below(8) {
            0 => i64::MIN,
            1 => i64::MAX,
            2 => -1,
            _ => self.below(4) as i64,
        }
    }
    fn weight(&mut self) -> i128 {
        match self.below(60) {
            0..=5 => 0,
            6 => i128::from(i64::MAX),
            7 => i128::from(i64::MIN),
            8 => -i128::from(i64::MIN),
            9 => i128::MAX - 3,
            10 => i128::MIN + 3,
            _ => self.below(5) as i128 - 2,
        }
    }
}

fn limits(rng: &mut Rng) -> Limits {
    Limits {
        max_columns: 1 + rng.below(4) as usize,
        max_rows: rng.below(24) as usize,
        max_work: 20 + rng.below(1500) as usize,
        ..Limits::default()
    }
}

#[test]
fn accumulator_matches_the_rust_accumulator() {
    let mut rng = Rng(0x5eed_acc0);
    let mut nonempty = 0;
    let mut errors = BTreeMap::new();
    for _ in 0..20_000 {
        let limits = limits(&mut rng);
        let widths: Vec<usize> = (0..1 + rng.below(3))
            .map(|_| rng.below(4) as usize)
            .collect();
        let ported = Accumulator::new(&widths, limits);
        let reference: Result<Vec<Reference>, Error> =
            widths.iter().map(|w| Reference::new(*w, limits)).collect();
        let (mut ported, mut reference) = match (ported, reference) {
            (Ok(p), Ok(r)) => (p, r),
            (Err(p), Err(r)) => {
                assert_eq!(p, r);
                *errors.entry(format!("{p:?}")).or_insert(0) += 1;
                continue;
            }
            (p, r) => panic!("new: {:?} vs {:?}", p.err(), r.err()),
        };
        let mut pb = Budget::new(limits);
        let mut rb = Budget::new(limits);
        let mut failed = None;
        for _ in 0..rng.below(40) {
            let input = rng.below(widths.len() as u64) as usize;
            let len = if rng.below(200) == 0 {
                rng.below(4) as usize
            } else {
                widths[input]
            };
            let row: Row = (0..len).map(|_| rng.cell()).collect();
            let weight = rng.weight();
            let p = ported.add(input, &row, weight, &mut pb);
            let r = reference[input].add(&row, weight, &mut rb);
            assert_eq!(p, r, "add of {row:?} x {weight}");
            assert_eq!(pb.remaining, rb.remaining);
            if let Err(e) = p {
                failed = Some(e);
                break;
            }
        }
        if let Some(e) = failed {
            *errors.entry(format!("{e:?}")).or_insert(0) += 1;
            continue;
        }
        let p = ported.finish();
        let r: Result<Vec<ZSet>, Error> = reference.into_iter().map(Reference::finish).collect();
        assert_eq!(p, r);
        match p {
            Ok(sets) => nonempty += sets.iter().filter(|s| !s.is_empty()).count(),
            Err(e) => *errors.entry(format!("{e:?}")).or_insert(0) += 1,
        }
    }
    // The cases reach every outcome.
    assert!(nonempty > 3_000, "{nonempty}: {errors:?}");
    for e in [
        "Limit(\"rows\")",
        "Limit(\"work\")",
        "Overflow",
        "Schema",
        "Limit(\"columns\")",
    ] {
        assert!(errors.get(e).copied().unwrap_or(0) > 20, "{e}: {errors:?}");
    }
}

/// `Executor::ingest_cdc`'s record loop as it was before the port.
fn reference_ingest(
    bytes: &[u8],
    policy: &CdcPolicy,
    inputs: &[usize],
    limits: Limits,
    next: &mut [u64],
    budget: &mut Budget,
) -> Result<(Vec<ZSet>, usize, i64), Error> {
    let mut acc: Vec<_> = inputs
        .iter()
        .map(|w| Reference::new(*w, limits))
        .collect::<Result<_, _>>()?;
    let mut cursor = 0usize;
    let mut records = 0usize;
    let mut max_ts = i64::MIN;
    while cursor < bytes.len() {
        if records >= policy.max_records {
            return Err(Error::Limit("CDC records"));
        }
        budget.charge(1)?;
        let header_end = cursor
            .checked_add(CdcHeader::BYTES)
            .ok_or(Error::Overflow)?;
        let header = CdcHeader::decode(
            bytes
                .get(cursor..header_end)
                .ok_or(Error::Cdc("truncated header"))?,
        )?;
        let binding_index = policy.binding(header)?;
        let binding = &policy.bindings[binding_index];
        if header.source_lsn != next[binding_index] {
            return Err(Error::Cdc("source offset"));
        }
        next[binding_index] = header.source_lsn.checked_add(1).ok_or(Error::Overflow)?;
        let width = inputs[binding.input];
        let end = width
            .checked_mul(8)
            .and_then(|len| header_end.checked_add(len))
            .ok_or(Error::Overflow)?;
        let payload = bytes
            .get(header_end..end)
            .ok_or(Error::Cdc("truncated row"))?;
        budget.charge(width)?;
        let row: Row = payload
            .as_chunks::<8>()
            .0
            .iter()
            .map(|chunk| i64::from_le_bytes(*chunk))
            .collect();
        acc[binding.input].add(&row, i128::from(header.z_weight), budget)?;
        max_ts = max_ts.max(header.event_ts_ms);
        records = records.checked_add(1).ok_or(Error::Overflow)?;
        cursor = end;
    }
    let sets = acc
        .into_iter()
        .map(Reference::finish)
        .collect::<Result<_, _>>()?;
    Ok((sets, records, max_ts))
}

#[test]
fn cdc_decoder_matches_the_rust_loop() {
    let mut rng = Rng(0x5eed_cdc0);
    let mut ok = 0;
    let mut errors = BTreeMap::new();
    for _ in 0..20_000 {
        let limits = Limits {
            max_columns: 4,
            ..limits(&mut rng)
        };
        let inputs: Vec<usize> = (0..1 + rng.below(3))
            .map(|_| rng.below(4) as usize)
            .collect();
        let mut partition = 0u32;
        let bindings: Vec<SourceBinding> = (0..1 + rng.below(3))
            .map(|_| {
                partition += 1 + rng.below(3) as u32;
                SourceBinding {
                    partition_id: partition,
                    schema_id: rng.below(2) as u16,
                    input: rng.below(inputs.len() as u64) as usize,
                    first_lsn: if rng.below(10) == 0 {
                        u64::MAX - 1
                    } else {
                        rng.below(3)
                    },
                    weights: if rng.below(2) == 0 {
                        WeightPolicy::Unit
                    } else {
                        WeightPolicy::NonZero
                    },
                }
            })
            .collect();
        let policy = CdcPolicy {
            max_batch_bytes: 1 << 16,
            max_records: 1 + rng.below(12) as usize,
            bindings,
        };
        let mut next: Vec<u64> = policy.bindings.iter().map(|b| b.first_lsn).collect();
        let mut bytes = Vec::new();
        let mut lsns = next.clone();
        for _ in 0..1 + rng.below(14) {
            let b = rng.below(policy.bindings.len() as u64) as usize;
            let binding = &policy.bindings[b];
            let broken = rng.below(25) == 0;
            let header = CdcHeader {
                source_lsn: if rng.below(30) == 0 {
                    lsns[b].wrapping_add(1)
                } else {
                    lsns[b]
                },
                event_ts_ms: rng.cell(),
                z_weight: match rng.below(12) {
                    0 => 0,
                    1 => 2,
                    2 => i64::MIN,
                    3 => i64::MAX,
                    _ => [1, -1][rng.below(2) as usize],
                },
                partition_id: if broken { 999 } else { binding.partition_id },
                schema_id: if rng.below(40) == 0 {
                    7
                } else {
                    binding.schema_id
                },
                flags: u16::from(rng.below(40) == 0),
            };
            lsns[b] = lsns[b].wrapping_add(1);
            bytes.extend_from_slice(&header.encode());
            for _ in 0..inputs[binding.input] {
                bytes.extend_from_slice(&rng.cell().to_le_bytes());
            }
        }
        if rng.below(20) == 0 {
            bytes.truncate(rng.below(bytes.len() as u64) as usize + 1);
        }
        let mut ref_next = next.clone();
        let mut rb = Budget::new(limits);
        let r = reference_ingest(&bytes, &policy, &inputs, limits, &mut ref_next, &mut rb);
        let mut pb = Budget::new(limits);
        let p = Accumulator::new(&inputs, limits).and_then(|mut acc| {
            let (records, ts) =
                crate::port::cdc_ingest(&mut acc, &bytes, &policy, &inputs, &mut next, &mut pb)?;
            Ok((acc.finish()?, records, ts))
        });
        assert_eq!(p, r);
        match r {
            Ok(_) => {
                ok += 1;
                assert_eq!(next, ref_next);
                assert_eq!(pb.remaining, rb.remaining);
            }
            Err(e) => *errors.entry(format!("{e:?}")).or_insert(0) += 1,
        }
    }
    assert!(ok > 2_000, "{ok}: {errors:?}");
    for e in [
        "Limit(\"rows\")",
        "Limit(\"work\")",
        "Limit(\"CDC records\")",
        "Overflow",
        "Cdc(\"partition\")",
        "Cdc(\"weight\")",
        "Cdc(\"source offset\")",
        "Cdc(\"schema\")",
        "Cdc(\"reserved flags\")",
    ] {
        assert!(errors.get(e).copied().unwrap_or(0) > 5, "{e}: {errors:?}");
    }
    assert!(errors.keys().any(|e| e.contains("truncated")), "{errors:?}");
}

mod log_reference {
    //! `log.rs`'s header, frame and decoder before the port.
    use crate::{Error, LogLimits, Row, ZSet};
    use std::collections::BTreeMap;

    pub(super) fn header(widths: &[usize]) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(b"VFLUXIL1");
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&(widths.len() as u32).to_le_bytes());
        for w in widths {
            h.extend_from_slice(&(*w as u32).to_le_bytes());
        }
        let sum = blake3::hash(&h);
        h.extend_from_slice(sum.as_bytes());
        h
    }

    pub(super) fn frame_sum(len: u32, payload: &[u8]) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(b"vesper.flux.input-log.frame/1");
        h.update(&len.to_le_bytes());
        h.update(payload);
        *h.finalize().as_bytes()
    }

    pub(super) fn frame(
        epoch: u64,
        deltas: &[ZSet],
        widths: &[usize],
        limits: LogLimits,
    ) -> Result<Vec<u8>, Error> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&epoch.to_le_bytes());
        let mut rows = 0usize;
        for (delta, width) in deltas.iter().zip(widths) {
            if delta.width != *width {
                return Err(Error::Schema);
            }
            rows = rows.checked_add(delta.len()).ok_or(Error::Overflow)?;
            if rows > limits.max_rows_per_frame {
                return Err(Error::Limit("log rows"));
            }
            payload.extend_from_slice(&(delta.len() as u32).to_le_bytes());
            for (row, weight) in delta.iter() {
                for cell in row {
                    payload.extend_from_slice(&cell.to_le_bytes());
                }
                payload.extend_from_slice(&weight.to_le_bytes());
            }
            if payload.len() > limits.max_frame_bytes {
                return Err(Error::Limit("log frame bytes"));
            }
        }
        let len = payload.len() as u32;
        let mut frame = Vec::new();
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(&frame_sum(len, &payload));
        Ok(frame)
    }

    pub(super) fn decode(
        payload: &[u8],
        epoch: u64,
        widths: &[usize],
        limits: LogLimits,
    ) -> Result<Vec<ZSet>, Error> {
        let mut cursor = 0usize;
        let mut take = |n: usize| -> Result<&[u8], Error> {
            let end = cursor.checked_add(n).ok_or(Error::Overflow)?;
            let bytes = payload
                .get(cursor..end)
                .ok_or(Error::Storage("log frame"))?;
            cursor = end;
            Ok(bytes)
        };
        let stored = u64::from_le_bytes(take(8)?.try_into().map_err(|_| Error::Overflow)?);
        if stored != epoch {
            return Err(Error::Storage("log epoch"));
        }
        let mut deltas = Vec::with_capacity(widths.len());
        let mut total = 0usize;
        for width in widths {
            let count = u32::from_le_bytes(take(4)?.try_into().map_err(|_| Error::Overflow)?);
            let count = count as usize;
            total = total.checked_add(count).ok_or(Error::Overflow)?;
            if total > limits.max_rows_per_frame {
                return Err(Error::Limit("log rows"));
            }
            let mut rows: BTreeMap<Row, i64> = BTreeMap::new();
            for _ in 0..count {
                let bytes = take((width + 1) * 8)?;
                let cells = bytes.as_chunks::<8>().0;
                let row: Vec<i64> = cells[..*width]
                    .iter()
                    .map(|c| i64::from_le_bytes(*c))
                    .collect();
                let weight = i64::from_le_bytes(cells[*width]);
                if weight == 0 || rows.last_key_value().is_some_and(|(last, _)| last >= &row) {
                    return Err(Error::Storage("noncanonical log rows"));
                }
                rows.insert(row, weight);
            }
            deltas.push(ZSet::from_rows(*width, rows));
        }
        if cursor != payload.len() {
            return Err(Error::Storage("log frame"));
        }
        Ok(deltas)
    }
}

#[test]
fn input_log_codec_matches_the_rust_codec() {
    let mut rng = Rng(0x5eed_1061);
    let mut errors = BTreeMap::new();
    let mut ok = 0;
    for case in 0..6_000 {
        let widths: Vec<usize> = (0..1 + rng.below(3))
            .map(|_| rng.below(4) as usize)
            .collect();
        assert_eq!(
            crate::port::log_header(&widths),
            log_reference::header(&widths)
        );
        let limits = crate::LogLimits {
            max_rows_per_frame: rng.below(40) as usize,
            max_frame_bytes: 16 + rng.below(1200) as usize,
            ..crate::LogLimits::default()
        };
        let set_limits = Limits {
            max_rows: 64,
            ..Limits::default()
        };
        let deltas: Vec<ZSet> = widths
            .iter()
            .map(|w| {
                let w = if rng.below(30) == 0 { w + 1 } else { *w };
                let entries: Vec<(Row, i64)> = (0..rng.below(16))
                    .map(|_| {
                        (
                            (0..w).map(|_| rng.cell()).collect(),
                            rng.below(5) as i64 - 2,
                        )
                    })
                    .collect();
                ZSet::consolidate(w, &entries, set_limits).expect("set")
            })
            .collect();
        let epoch = rng.below(1 << 40);
        let p = crate::port::log_frame(epoch, &deltas, &widths, limits);
        let r = log_reference::frame(epoch, &deltas, &widths, limits);
        assert_eq!(p, r, "case {case}");
        let mut frame = match r {
            Ok(f) => f,
            Err(e) => {
                *errors.entry(format!("{e:?}")).or_insert(0) += 1;
                continue;
            }
        };
        // Damage some frames: a flipped byte, a cut, a forged checksum.
        let len = u32::from_le_bytes(frame[..4].try_into().unwrap());
        match rng.below(6) {
            0 => {
                let i = 4 + rng.below(frame.len() as u64 - 4) as usize;
                frame[i] ^= 1 << rng.below(8);
            }
            1 => {
                // A wrong payload with a valid checksum reaches the decoder.
                let i = 4 + rng.below(u64::from(len)) as usize;
                frame[i] ^= 1 << rng.below(8);
                let sum = log_reference::frame_sum(len, &frame[4..4 + len as usize]);
                let at = frame.len() - 32;
                frame[at..].copy_from_slice(&sum);
            }
            _ => {}
        }
        let body = &frame[4..];
        let checks = crate::port::log_frame_ok(body, len);
        let expected = log_reference::frame_sum(len, &body[..len as usize]) == body[len as usize..];
        assert_eq!(checks, expected, "case {case}");
        if !checks {
            *errors.entry("checksum".to_string()).or_insert(0) += 1;
            continue;
        }
        let read_epoch = if rng.below(20) == 0 { epoch + 1 } else { epoch };
        let payload = &body[..len as usize];
        let p = crate::port::log_decode(payload, read_epoch, &widths, limits);
        let r = log_reference::decode(payload, read_epoch, &widths, limits);
        assert_eq!(p, r, "case {case}");
        match r {
            Ok(_) => ok += 1,
            Err(e) => *errors.entry(format!("{e:?}")).or_insert(0) += 1,
        }
    }
    assert!(ok > 1_000, "{ok}: {errors:?}");
    for e in [
        "Schema",
        "Limit(\"log rows\")",
        "Limit(\"log frame bytes\")",
        "checksum",
        "Storage(\"log epoch\")",
    ] {
        assert!(errors.get(e).copied().unwrap_or(0) > 10, "{e}: {errors:?}");
    }
    assert!(
        errors
            .keys()
            .filter(|e| e.contains("log frame\"") || e.contains("noncanonical"))
            .count()
            == 2,
        "{errors:?}"
    );
}

/// Rows that all collide under a fixed multiplicative hash
/// (`[x, (M ^ x) * M]` for the golden-ratio multiplier) stay cheap: the
/// table's hash is keyed per accumulator.
#[test]
fn colliding_rows_stay_cheap() {
    const M: u64 = 0x9E37_79B9_7F4A_7C15;
    let limits = Limits {
        max_rows: 1 << 16,
        max_work: usize::MAX,
        ..Limits::default()
    };
    let mut acc = Accumulator::new(&[2], limits).unwrap();
    let mut budget = Budget::new(limits);
    let n = 1u64 << 16;
    let start = std::time::Instant::now();
    for x in 0..n {
        let row = [x as i64, ((M ^ x).wrapping_mul(M)) as i64];
        acc.add(0, &row, 1, &mut budget).unwrap();
    }
    let set = acc.finish().unwrap().remove(0);
    assert_eq!(set.len(), n as usize);
    // Quadratic probing of 65,536 rows takes minutes; this takes well
    // under a second even in a debug build.
    assert!(start.elapsed() < std::time::Duration::from_secs(30));
}
