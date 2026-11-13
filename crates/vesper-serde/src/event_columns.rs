//! Fixed EventEnvelope projection into initialized caller-owned columns.
//!
//! Batch construction already validated every JSON subtree. Schema admission
//! and a complete two-pass work envelope precede every column write; this adds
//! no heap row staging, source linearity or aligned AoSoA allocation claim.
use crate::ndjson::{Error, ErrorKind, EventField, NdjsonCursor, RecordRef};
use crate::{JsonString, Node};

#[derive(Clone, Copy, Debug)]
pub struct EventEnvelope<'d, 'a> {
    pub id: u64,
    pub topic: JsonString<'a>,
    pub raw_meta: Node<'d, 'a>,
}

/// Slices contain initialized values. Successful projection fills only the
/// returned prefix; every returned error preserves all three complete slices.
pub struct EventColumns<'out, 'd, 'a> {
    pub ids: &'out mut [u64],
    pub topics: &'out mut [Option<JsonString<'a>>],
    pub raw_meta: &'out mut [Option<Node<'d, 'a>>],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionLimits {
    pub max_rows: usize,
    pub max_work: usize,
}
impl Default for ProjectionLimits {
    fn default() -> Self {
        Self {
            max_rows: 1024,
            max_work: 32 * 1024 * 1024,
        }
    }
}
impl ProjectionLimits {
    pub const HARD: Self = Self {
        max_rows: 16384,
        max_work: 512 * 1024 * 1024,
    };
    /// Standalone configuration errors have the batch-wide origin. Cursor
    /// operations relocate this error to their current record or physical EOF.
    pub fn validate(self) -> Result<(), Error> {
        if self.max_rows == 0
            || self.max_rows > Self::HARD.max_rows
            || self.max_work == 0
            || self.max_work > Self::HARD.max_work
        {
            return Err(Error {
                kind: ErrorKind::InvalidLimits,
                offset: 0,
                record: 0,
                line: 1,
            });
        }
        Ok(())
    }
}

fn at(record: RecordRef<'_, '_>, kind: ErrorKind) -> Error {
    Error {
        kind,
        offset: record.payload_span().start,
        record: record.ordinal(),
        line: record.line(),
    }
}
struct Work {
    remaining: usize,
    passes: usize,
}
impl Work {
    fn charge(&mut self, units: usize, record: RecordRef<'_, '_>) -> Result<(), Error> {
        let units = units
            .checked_mul(self.passes)
            .ok_or_else(|| at(record, ErrorKind::Overflow))?;
        self.remaining = self
            .remaining
            .checked_sub(units)
            .ok_or_else(|| at(record, ErrorKind::WorkLimit))?;
        Ok(())
    }
}

#[derive(Default)]
struct Fields<'d, 'a> {
    id: Option<Node<'d, 'a>>,
    topic: Option<Node<'d, 'a>>,
    raw_meta: Option<Node<'d, 'a>>,
}
impl<'d, 'a> Fields<'d, 'a> {
    fn insert(&mut self, name: JsonString<'_>, value: Node<'d, 'a>) {
        // Always perform the same three comparisons under their single prepaid
        // raw-key envelope, including unknown and escaped-equivalent names.
        let id = name.equals("id");
        let topic = name.equals("topic");
        let raw_meta = name.equals("raw_meta");
        if id {
            self.id = Some(value);
        }
        if topic {
            self.topic = Some(value);
        }
        if raw_meta {
            self.raw_meta = Some(value);
        }
    }
}

fn checked_event<'d, 'a>(
    record: RecordRef<'d, 'a>,
    work: &mut Work,
) -> Result<EventEnvelope<'d, 'a>, Error> {
    work.charge(1, record)?;
    let mut fields = Fields::default();
    for (name, value) in record.root().fields().map_err(|e| record.json_error(e))? {
        let comparisons = name
            .raw_content()
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_mul(24))
            .and_then(|n| n.checked_mul(3))
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| at(record, ErrorKind::Overflow))?;
        work.charge(comparisons, record)?;
        fields.insert(name, value);
    }
    // All required presence checks precede all conversions, in fixed order.
    let id = fields
        .id
        .ok_or_else(|| at(record, ErrorKind::MissingField(EventField::Id)))?;
    let topic = fields
        .topic
        .ok_or_else(|| at(record, ErrorKind::MissingField(EventField::Topic)))?;
    let raw_meta = fields
        .raw_meta
        .ok_or_else(|| at(record, ErrorKind::MissingField(EventField::RawMeta)))?;
    work.charge(
        id.raw()
            .len()
            .checked_add(1)
            .ok_or_else(|| at(record, ErrorKind::Overflow))?,
        record,
    )?;
    let id = id.as_u64().map_err(|e| record.json_error(e))?;
    work.charge(1, record)?;
    let topic = topic.string().map_err(|e| record.json_error(e))?;
    Ok(EventEnvelope {
        id,
        topic,
        raw_meta,
    })
}

fn prepaid_event<'d, 'a>(record: RecordRef<'d, 'a>) -> EventEnvelope<'d, 'a> {
    // Only project_into calls this after checking every immutable batch row and
    // paying both schema passes. There are no callbacks, fresh quota checks or
    // fallible returned results after the first output write.
    let mut fields = Fields::default();
    for (name, value) in record
        .root()
        .fields()
        .expect("preflight checked event object")
    {
        fields.insert(name, value);
    }
    let id = fields.id.expect("preflight checked id presence");
    let topic = fields.topic.expect("preflight checked topic presence");
    let raw_meta = fields
        .raw_meta
        .expect("preflight checked raw_meta presence");
    EventEnvelope {
        id: id.as_u64().expect("preflight checked exact u64 id"),
        topic: topic.string().expect("preflight checked topic string"),
        raw_meta,
    }
}

impl<'d, 'a> NdjsonCursor<'d, 'a> {
    /// Check one complete schema before advancing; borrowed views remain tied
    /// to Batch/input rather than to the temporary mutable cursor borrow.
    pub fn read_event(&mut self, limits: ProjectionLimits) -> Result<EventEnvelope<'d, 'a>, Error> {
        limits.validate().map_err(|e| self.current_error(e.kind))?;
        let record = self.peek().ok_or_else(|| self.end_error())?;
        let end = self
            .position()
            .checked_add(1)
            .ok_or_else(|| self.current_error(ErrorKind::Overflow))?;
        let remaining = limits
            .max_work
            .checked_sub(1)
            .ok_or_else(|| self.current_error(ErrorKind::WorkLimit))?;
        let row = checked_event(
            record,
            &mut Work {
                remaining,
                passes: 1,
            },
        )?;
        self.commit_to(end);
        Ok(row)
    }
}

/// Project ALL remaining records transactionally. No typed AoS vector is
/// allocated: the preflight holds only a single ephemeral row on the stack.
pub fn project_into<'out, 'd, 'a>(
    cursor: &mut NdjsonCursor<'d, 'a>,
    columns: EventColumns<'out, 'd, 'a>,
    limits: ProjectionLimits,
) -> Result<usize, Error> {
    limits
        .validate()
        .map_err(|e| cursor.current_error(e.kind))?;
    let count = cursor.remaining();
    if count > limits.max_rows {
        return Err(cursor.current_error(ErrorKind::RecordLimit));
    }
    if columns.ids.len() < count || columns.topics.len() < count || columns.raw_meta.len() < count {
        return Err(cursor.current_error(ErrorKind::OutputTooSmall));
    }
    if count == 0 {
        return Ok(0);
    }
    let start = cursor.position();
    let end = start
        .checked_add(count)
        .ok_or_else(|| cursor.current_error(ErrorKind::Overflow))?;
    let write_work = count
        .checked_mul(3)
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| cursor.current_error(ErrorKind::Overflow))?;
    let remaining = limits
        .max_work
        .checked_sub(write_work)
        .ok_or_else(|| cursor.current_error(ErrorKind::WorkLimit))?;
    let batch = cursor.batch();
    let mut work = Work {
        remaining,
        passes: 2,
    };
    for ordinal in start..end {
        let record = batch
            .get(ordinal)
            .expect("cursor remaining records belong to its immutable batch");
        checked_event(record, &mut work)?;
    }
    for (slot, ordinal) in (start..end).enumerate() {
        let row = prepaid_event(batch.get(ordinal).expect("preflight checked record bounds"));
        columns.ids[slot] = row.id;
        columns.topics[slot] = Some(row.topic);
        columns.raw_meta[slot] = Some(row.raw_meta);
    }
    cursor.commit_to(end);
    Ok(count)
}
