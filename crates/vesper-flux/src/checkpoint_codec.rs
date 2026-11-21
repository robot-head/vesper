//! The byte writer and reader of the checkpoint formats, and the source
//! section's codec, as the Rust checkpoint image codec had them. The
//! durable checkpointer (`durable.rs`) still encodes its manifests and
//! batches with them until its own codec moves to Vesper; the reference of
//! the checkpoint differential test (`checkpoint_reference.rs`) uses them too.
use super::super::Circuit;
use super::super::cdc::{CdcPolicy, SourceBinding, SourceState, WeightPolicy};
use super::CheckpointLimits;
use crate::{Budget, Error};

pub(crate) struct Writer {
    pub(crate) bytes: Vec<u8>,
    pub(crate) limit: usize,
}
impl Writer {
    pub(crate) fn put(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(Error::Overflow)?;
        if end > self.limit {
            return Err(Error::Limit("checkpoint bytes"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    pub(crate) fn u8(&mut self, n: u8) -> Result<(), Error> {
        self.put(&[n])
    }
    pub(crate) fn u16(&mut self, n: u16) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
    pub(crate) fn u32(&mut self, n: u32) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
    pub(crate) fn u64(&mut self, n: u64) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
    pub(crate) fn usize(&mut self, n: usize) -> Result<(), Error> {
        self.u64(u64::try_from(n).map_err(|_| Error::Overflow)?)
    }
    pub(crate) fn i64(&mut self, n: i64) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
}
pub(crate) struct Reader<'a, 'b> {
    pub(crate) bytes: &'a [u8],
    pub(crate) cursor: usize,
    pub(crate) budget: &'b mut Budget,
}
impl<'a> Reader<'a, '_> {
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        self.budget.charge(1)?;
        self.take_prepaid(n)
    }
    pub(crate) fn take_prepaid(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.cursor.checked_add(n).ok_or(Error::Overflow)?;
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .ok_or(Error::Checkpoint("truncated"))?;
        self.cursor = end;
        Ok(bytes)
    }
    /// Charge all scalar reads and validate the complete encoded row before
    /// allocating its vector; no partial row growth precedes those checks.
    pub(crate) fn row(&mut self, width: usize) -> Result<(Vec<i64>, i64), Error> {
        let reads = width.checked_add(1).ok_or(Error::Overflow)?;
        self.budget.charge(reads)?;
        let size = reads.checked_mul(8).ok_or(Error::Overflow)?;
        let bytes = self.take_prepaid(size)?;
        let cells = width.checked_mul(8).ok_or(Error::Overflow)?;
        let row = bytes[..cells]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|cell| i64::from_le_bytes(*cell))
            .collect();
        let weight = i64::from_le_bytes(
            bytes[cells..]
                .try_into()
                .map_err(|_| Error::Checkpoint("weight"))?,
        );
        Ok((row, weight))
    }
    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| Error::Checkpoint("u16"))?,
        ))
    }
    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| Error::Checkpoint("u32"))?,
        ))
    }
    pub(crate) fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| Error::Checkpoint("u64"))?,
        ))
    }
    pub(crate) fn usize(&mut self) -> Result<usize, Error> {
        usize::try_from(self.u64()?).map_err(|_| Error::Overflow)
    }
}

pub(crate) fn write_source(w: &mut Writer, source: Option<&SourceState>) -> Result<(), Error> {
    match source {
        None => w.u8(0)?,
        Some(s) => {
            w.u8(1)?;
            w.usize(s.policy.max_batch_bytes)?;
            w.usize(s.policy.max_records)?;
            w.u32(u32::try_from(s.policy.bindings.len()).map_err(|_| Error::Overflow)?)?;
            for (b, next) in s.policy.bindings.iter().zip(&s.next) {
                w.u32(b.partition_id)?;
                w.u16(b.schema_id)?;
                w.usize(b.input)?;
                w.u64(b.first_lsn)?;
                w.u8(match b.weights {
                    WeightPolicy::Unit => 0,
                    WeightPolicy::NonZero => 1,
                })?;
                w.u64(*next)?;
            }
        }
    }
    Ok(())
}
pub(crate) fn read_source(
    r: &mut Reader<'_, '_>,
    circuit: &Circuit,
    limits: CheckpointLimits,
) -> Result<Option<SourceState>, Error> {
    let source = match r.u8()? {
        0 => None,
        1 => {
            let max_batch_bytes = r.usize()?;
            let max_records = r.usize()?;
            let count = usize::try_from(r.u32()?).map_err(|_| Error::Overflow)?;
            if count > limits.max_sources || count > 1024 {
                return Err(Error::Limit("checkpoint sources"));
            }
            let mut bindings = Vec::new();
            let mut next = Vec::new();
            for _ in 0..count {
                let partition_id = r.u32()?;
                let schema_id = r.u16()?;
                let input = r.usize()?;
                let first_lsn = r.u64()?;
                let weights = match r.u8()? {
                    0 => WeightPolicy::Unit,
                    1 => WeightPolicy::NonZero,
                    _ => return Err(Error::Checkpoint("weight policy")),
                };
                let next_lsn = r.u64()?;
                bindings.push(SourceBinding {
                    partition_id,
                    schema_id,
                    input,
                    first_lsn,
                    weights,
                });
                next.push(next_lsn);
            }
            let policy = CdcPolicy {
                bindings,
                max_batch_bytes,
                max_records,
            };
            policy.validate(circuit)?;
            Some(SourceState { policy, next })
        }
        _ => return Err(Error::Checkpoint("source tag")),
    };
    Ok(source)
}
