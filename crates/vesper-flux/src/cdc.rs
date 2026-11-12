use super::{Circuit, Executor, TickInput};
use crate::{Budget, Error, Row, ZSet, zset::Accumulator};

/// Padding-free wire value, encoded explicitly as exactly 32 little-endian bytes.
/// Timestamp is arbitrary signed event time; ingestion never advances a frontier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CdcHeader {
    pub source_lsn: u64,
    pub event_ts_ms: i64,
    pub z_weight: i64,
    pub partition_id: u32,
    pub schema_id: u16,
    pub flags: u16,
}
impl CdcHeader {
    pub const BYTES: usize = 32;
    pub fn encode(self) -> [u8; Self::BYTES] {
        let mut out = [0; Self::BYTES];
        out[0..8].copy_from_slice(&self.source_lsn.to_le_bytes());
        out[8..16].copy_from_slice(&self.event_ts_ms.to_le_bytes());
        out[16..24].copy_from_slice(&self.z_weight.to_le_bytes());
        out[24..28].copy_from_slice(&self.partition_id.to_le_bytes());
        out[28..30].copy_from_slice(&self.schema_id.to_le_bytes());
        out[30..32].copy_from_slice(&self.flags.to_le_bytes());
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != Self::BYTES {
            return Err(Error::Cdc("header length"));
        }
        Ok(Self {
            source_lsn: u64::from_le_bytes(
                bytes[0..8].try_into().map_err(|_| Error::Cdc("header"))?,
            ),
            event_ts_ms: i64::from_le_bytes(
                bytes[8..16].try_into().map_err(|_| Error::Cdc("header"))?,
            ),
            z_weight: i64::from_le_bytes(
                bytes[16..24].try_into().map_err(|_| Error::Cdc("header"))?,
            ),
            partition_id: u32::from_le_bytes(
                bytes[24..28].try_into().map_err(|_| Error::Cdc("header"))?,
            ),
            schema_id: u16::from_le_bytes(
                bytes[28..30].try_into().map_err(|_| Error::Cdc("header"))?,
            ),
            flags: u16::from_le_bytes(bytes[30..32].try_into().map_err(|_| Error::Cdc("header"))?),
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeightPolicy {
    Unit,
    NonZero,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceBinding {
    pub partition_id: u32,
    pub schema_id: u16,
    pub input: usize,
    pub first_lsn: u64,
    pub weights: WeightPolicy,
}
/// Restricted offset policy: LSNs are consecutive integer record numbers, not
/// arbitrary byte offsets or compacted-source cursors. Partition IDs are unique,
/// sorted in canonical order; one fixed schema per partition. Several partitions
/// can feed the same circuit input. Every record has a 32-byte header followed by
/// exactly input_width * 8 signed LE row bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CdcPolicy {
    pub bindings: Vec<SourceBinding>,
    pub max_batch_bytes: usize,
    pub max_records: usize,
}
impl CdcPolicy {
    pub(super) fn validate(&self, circuit: &Circuit) -> Result<(), Error> {
        if self.bindings.is_empty()
            || self.bindings.len() > 1024
            || self.max_records == 0
            || self.max_records > 65536
            || self.max_records > circuit.limits.max_input_entries
            || self.max_batch_bytes == 0
            || self.max_batch_bytes > 8 * 1024 * 1024
        {
            return Err(Error::Limit("CDC policy"));
        }
        let mut previous = None;
        for binding in &self.bindings {
            if binding.input >= circuit.inputs.len() {
                return Err(Error::Schema);
            }
            if previous.is_some_and(|p| p >= binding.partition_id) {
                return Err(Error::Cdc("partition order"));
            }
            previous = Some(binding.partition_id);
        }
        Ok(())
    }
    pub(super) fn binding(&self, header: CdcHeader) -> Result<usize, Error> {
        let i = self
            .bindings
            .binary_search_by_key(&header.partition_id, |b| b.partition_id)
            .map_err(|_| Error::Cdc("partition"))?;
        let binding = &self.bindings[i];
        if header.schema_id != binding.schema_id {
            return Err(Error::Cdc("schema"));
        }
        if header.flags != 0 {
            return Err(Error::Cdc("reserved flags"));
        }
        if header.z_weight == 0
            || (binding.weights == WeightPolicy::Unit && ![-1, 1].contains(&header.z_weight))
        {
            return Err(Error::Cdc("weight"));
        }
        Ok(i)
    }
}
#[derive(Debug)]
pub(super) struct SourceState {
    pub(super) policy: CdcPolicy,
    /// Next required LSN. No record at MAX is accepted: its successor must fit.
    pub(super) next: Vec<u64>,
}
#[derive(Debug)]
pub struct Ingested {
    pub delta: ZSet,
    pub records: usize,
    pub max_event_ts_ms: i64,
}
impl Executor {
    /// Bind once before the first tick. Raw ticks are disabled in source mode.
    pub fn bind_sources(&mut self, policy: &CdcPolicy) -> Result<(), Error> {
        if self.ticks != 0 || self.source.is_some() {
            return Err(Error::SourceMode);
        }
        policy.validate(&self.circuit)?;
        self.source = Some(SourceState {
            policy: policy.clone(),
            next: policy.bindings.iter().map(|b| b.first_lsn).collect(),
        });
        Ok(())
    }
    pub fn next_source_lsn(&self, partition_id: u32) -> Option<u64> {
        let source = self.source.as_ref()?;
        let i = source
            .policy
            .bindings
            .binary_search_by_key(&partition_id, |b| b.partition_id)
            .ok()?;
        Some(source.next[i])
    }
    /// Encode a policy/schema-valid record. Future LSNs can be prepared; exact
    /// consecutive ordering is checked transactionally by ingest_cdc.
    pub fn encode_cdc_record(&self, header: CdcHeader, row: &[i64]) -> Result<Vec<u8>, Error> {
        let source = self.source.as_ref().ok_or(Error::SourceMode)?;
        let i = source.policy.binding(header)?;
        let width = self.circuit.inputs[source.policy.bindings[i].input];
        if row.len() != width {
            return Err(Error::Schema);
        }
        header.source_lsn.checked_add(1).ok_or(Error::Overflow)?;
        let size = width
            .checked_mul(8)
            .and_then(|n| n.checked_add(CdcHeader::BYTES))
            .ok_or(Error::Overflow)?;
        if size > source.policy.max_batch_bytes {
            return Err(Error::Limit("CDC bytes"));
        }
        let mut bytes = Vec::with_capacity(size);
        bytes.extend_from_slice(&header.encode());
        for value in row {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        Ok(bytes)
    }
    /// Decode the entire borrowed batch, then run a real differentiated tick.
    /// Only after success are source offsets published alongside all integrals.
    /// Empty batches, duplicate/gapped/reordered LSNs and unknown flags fail.
    pub fn ingest_cdc(&mut self, bytes: &[u8]) -> Result<Ingested, Error> {
        let source = self.source.as_ref().ok_or(Error::SourceMode)?;
        if bytes.is_empty() {
            return Err(Error::Cdc("empty batch"));
        }
        if bytes.len() > source.policy.max_batch_bytes {
            return Err(Error::Limit("CDC bytes"));
        }
        let mut next = source.next.clone();
        let mut acc: Vec<_> = self
            .circuit
            .inputs
            .iter()
            .map(|w| Accumulator::new(*w, self.circuit.limits))
            .collect::<Result<_, _>>()?;
        let mut budget = Budget::new(self.circuit.limits);
        let mut cursor = 0usize;
        let mut records = 0usize;
        let mut max_ts = i64::MIN;
        while cursor < bytes.len() {
            if records >= source.policy.max_records {
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
            let binding_index = source.policy.binding(header)?;
            let binding = &source.policy.bindings[binding_index];
            if header.source_lsn != next[binding_index] {
                return Err(Error::Cdc("source offset"));
            }
            next[binding_index] = header.source_lsn.checked_add(1).ok_or(Error::Overflow)?;
            let width = self.circuit.inputs[binding.input];
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
            acc[binding.input].add(&row, i128::from(header.z_weight), &mut budget)?;
            max_ts = max_ts.max(header.event_ts_ms);
            records = records.checked_add(1).ok_or(Error::Overflow)?;
            cursor = end;
        }
        let deltas = acc
            .into_iter()
            .map(Accumulator::finish)
            .collect::<Result<_, _>>()?;
        let delta = self.tick_inner(&TickInput { deltas })?;
        // No fallible operations after tick commits.
        self.source.as_mut().expect("validated bound source").next = next;
        Ok(Ingested {
            delta,
            records,
            max_event_ts_ms: max_ts,
        })
    }
}
