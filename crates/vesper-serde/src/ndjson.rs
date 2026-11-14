//! Fully validated immutable NDJSON batches with aggregate logical budgets.
//!
//! Each record retains a bounded JSON token tape, built by the scalar parser or
//! from the stage 1 SIMD structural index (see [`Engine`]); both engines build
//! identical tapes and statistics. This is not incremental or zero-heap
//! streaming; every record validates before any view is published.
use crate::simd::{Isa, StructuralIndex};
use crate::{IndexedJson, Limits, Node};
use std::{fmt, mem::size_of, ops::Range};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventField {
    Id,
    Topic,
    RawMeta,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    InvalidLimits,
    InputLimit,
    RecordLimit,
    TokenLimit,
    IndexLimit,
    MetadataLimit,
    WorkLimit,
    BlankLine,
    OutputTooSmall,
    Overflow,
    AllocationFailed,
    End,
    MissingField(EventField),
    Json(crate::ErrorKind),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub offset: usize,
    pub record: usize,
    pub line: usize,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} at byte {} (record {}, line {})",
            self.kind, self.offset, self.record, self.line
        )
    }
}
impl std::error::Error for Error {}
fn initial(kind: ErrorKind) -> Error {
    Error {
        kind,
        offset: 0,
        record: 0,
        line: 1,
    }
}

/// How each record is indexed. Tapes, statistics and accept/reject decisions
/// are identical; only error offsets may differ on malformed records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Engine {
    /// Byte-at-a-time recursive descent.
    Scalar,
    /// Stage 1 SIMD structural index on the given path, then the stage 2
    /// tape builder. The stage 1 buffers are reused across records.
    Simd(Isa),
}
impl Engine {
    /// SIMD on the fastest path this CPU supports.
    pub fn detect() -> Self {
        Engine::Simd(Isa::detect())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BatchLimits {
    pub max_input_bytes: usize,
    pub max_records: usize,
    pub max_tokens: usize,
    pub max_index_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
    pub record: Limits,
}
impl Default for BatchLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024 * 1024,
            max_records: 1024,
            max_tokens: 65_536,
            max_index_bytes: 4 * 1024 * 1024,
            max_metadata_bytes: 256 * 1024,
            max_work: 32 * 1024 * 1024,
            record: Limits {
                max_input_bytes: 64 * 1024,
                max_depth: 64,
                max_tokens: 65_536,
                max_string_bytes: 64 * 1024,
                max_total_string_bytes: 64 * 1024,
                max_work: 16 * 1024 * 1024,
                max_index_bytes: 4 * 1024 * 1024,
            },
        }
    }
}
impl BatchLimits {
    pub const HARD: Self = Self {
        max_input_bytes: 16 * 1024 * 1024,
        max_records: 16_384,
        max_tokens: 1_048_576,
        max_index_bytes: 64 * 1024 * 1024,
        max_metadata_bytes: 4 * 1024 * 1024,
        max_work: 512 * 1024 * 1024,
        record: Limits {
            max_input_bytes: 1024 * 1024,
            max_depth: 128,
            max_tokens: 1_048_576,
            max_string_bytes: 1024 * 1024,
            max_total_string_bytes: 1024 * 1024,
            max_work: 512 * 1024 * 1024,
            max_index_bytes: 64 * 1024 * 1024,
        },
    };
    pub fn validate(self) -> Result<(), Error> {
        let hard = Self::HARD;
        for (value, ceiling) in [
            (self.max_input_bytes, hard.max_input_bytes),
            (self.max_records, hard.max_records),
            (self.max_tokens, hard.max_tokens),
            (self.max_index_bytes, hard.max_index_bytes),
            (self.max_metadata_bytes, hard.max_metadata_bytes),
            (self.max_work, hard.max_work),
            (self.record.max_input_bytes, hard.record.max_input_bytes),
            (self.record.max_depth, hard.record.max_depth),
            (self.record.max_tokens, hard.record.max_tokens),
            (self.record.max_string_bytes, hard.record.max_string_bytes),
            (
                self.record.max_total_string_bytes,
                hard.record.max_total_string_bytes,
            ),
            (self.record.max_work, hard.record.max_work),
            (self.record.max_index_bytes, hard.record.max_index_bytes),
        ] {
            if value == 0 || value > ceiling {
                return Err(initial(ErrorKind::InvalidLimits));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BatchStats {
    pub records: usize,
    pub tokens: usize,
    pub index_bytes: usize,
    pub metadata_bytes: usize,
    pub work: usize,
}
#[derive(Debug)]
struct Record<'a> {
    json: IndexedJson<'a>,
    payload: Range<usize>,
    line: usize,
}
#[derive(Debug)]
pub struct NdjsonBatch<'a> {
    records: Vec<Record<'a>>,
    input_len: usize,
    eof_line: usize,
    stats: BatchStats,
}

fn relocate(error: crate::Error, payload: &Range<usize>, record: usize, line: usize) -> Error {
    let Some(offset) = payload
        .start
        .checked_add(error.offset)
        .filter(|n| *n <= payload.end)
    else {
        return Error {
            kind: ErrorKind::Overflow,
            offset: payload.start,
            record,
            line,
        };
    };
    Error {
        kind: ErrorKind::Json(error.kind),
        offset,
        record,
        line,
    }
}

impl<'a> NdjsonBatch<'a> {
    /// Parse with [`Engine::detect`]: the stage 1 SIMD index when available.
    pub fn parse(input: &'a [u8], limits: BatchLimits) -> Result<Self, Error> {
        Self::parse_with(input, limits, Engine::detect())
    }

    pub fn parse_with(input: &'a [u8], limits: BatchLimits, engine: Engine) -> Result<Self, Error> {
        limits.validate()?;
        if let Engine::Simd(isa) = engine
            && !isa.is_available()
        {
            return Err(initial(ErrorKind::Json(crate::ErrorKind::IsaUnavailable)));
        }
        let mut index = StructuralIndex::new();
        if input.len() > limits.max_input_bytes {
            return Err(initial(ErrorKind::InputLimit));
        }
        let mut work = input
            .len()
            .checked_mul(3)
            .ok_or(initial(ErrorKind::Overflow))?;
        if work > limits.max_work {
            return Err(initial(ErrorKind::WorkLimit));
        }
        let mut newlines = 0usize;
        for &byte in input {
            if byte == b'\n' {
                newlines = newlines
                    .checked_add(1)
                    .ok_or(initial(ErrorKind::Overflow))?;
            }
        }
        let count = if input.is_empty() {
            0
        } else {
            newlines
                .checked_add(usize::from(input.last() != Some(&b'\n')))
                .ok_or(initial(ErrorKind::Overflow))?
        };
        if count > limits.max_records {
            return Err(initial(ErrorKind::RecordLimit));
        }
        let eof_line = newlines
            .checked_add(1)
            .ok_or(initial(ErrorKind::Overflow))?;
        let metadata_bytes = count
            .checked_mul(size_of::<Record<'a>>())
            .ok_or(initial(ErrorKind::Overflow))?;
        if metadata_bytes > limits.max_metadata_bytes {
            return Err(initial(ErrorKind::MetadataLimit));
        }
        work = work
            .checked_add(count.checked_mul(32).ok_or(initial(ErrorKind::Overflow))?)
            .and_then(|n| n.checked_add(metadata_bytes))
            .ok_or(initial(ErrorKind::Overflow))?;
        if work > limits.max_work {
            return Err(initial(ErrorKind::WorkLimit));
        }
        // Charge all requested record-vector payload before its sole reserve.
        let mut records = Vec::new();
        records
            .try_reserve_exact(count)
            .map_err(|_| initial(ErrorKind::AllocationFailed))?;
        let mut stats = BatchStats {
            metadata_bytes,
            work,
            ..Default::default()
        };
        let mut start = 0usize;
        if !input.is_empty() {
            for (ordinal, frame) in input.split_inclusive(|b| *b == b'\n').enumerate() {
                let line = ordinal.checked_add(1).ok_or(initial(ErrorKind::Overflow))?;
                let here = |kind| Error {
                    kind,
                    offset: start,
                    record: ordinal,
                    line,
                };
                let end = start
                    .checked_add(frame.len())
                    .ok_or_else(|| here(ErrorKind::Overflow))?;
                let payload = if let Some(body) = frame.strip_suffix(b"\n") {
                    body.strip_suffix(b"\r").unwrap_or(body)
                } else {
                    frame
                };
                let payload_end = start
                    .checked_add(payload.len())
                    .ok_or_else(|| here(ErrorKind::Overflow))?;
                if payload.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r')) {
                    return Err(here(ErrorKind::BlankLine));
                }
                if payload.len() > limits.record.max_input_bytes {
                    return Err(here(ErrorKind::InputLimit));
                }
                let remaining_tokens = limits
                    .max_tokens
                    .checked_sub(stats.tokens)
                    .ok_or_else(|| here(ErrorKind::TokenLimit))?;
                let remaining_index = limits
                    .max_index_bytes
                    .checked_sub(stats.index_bytes)
                    .ok_or_else(|| here(ErrorKind::IndexLimit))?;
                let remaining_work = limits
                    .max_work
                    .checked_sub(stats.work)
                    .ok_or_else(|| here(ErrorKind::WorkLimit))?;
                if remaining_tokens == 0 {
                    return Err(here(ErrorKind::TokenLimit));
                }
                if remaining_index == 0 {
                    return Err(here(ErrorKind::IndexLimit));
                }
                if payload.len() > remaining_work {
                    return Err(here(ErrorKind::WorkLimit));
                }
                let record_limits = Limits {
                    max_input_bytes: limits
                        .record
                        .max_input_bytes
                        .min(limits.max_input_bytes)
                        .min(payload.len()),
                    max_tokens: limits.record.max_tokens.min(remaining_tokens),
                    max_index_bytes: limits.record.max_index_bytes.min(remaining_index),
                    max_work: limits.record.max_work.min(remaining_work),
                    ..limits.record
                };
                let span = start..payload_end;
                let json = match engine {
                    Engine::Scalar => IndexedJson::parse(payload, record_limits),
                    Engine::Simd(isa) => {
                        IndexedJson::parse_with_index(payload, record_limits, isa, &mut index)
                    }
                }
                .map_err(|error| relocate(error, &span, ordinal, line))?;
                let tokens = json.token_count();
                let index = json.index_bytes();
                let parsed_work = json.work_used();
                if tokens > remaining_tokens {
                    return Err(here(ErrorKind::TokenLimit));
                }
                if index > remaining_index || index > record_limits.max_index_bytes {
                    return Err(here(ErrorKind::IndexLimit));
                }
                if parsed_work > remaining_work {
                    return Err(here(ErrorKind::WorkLimit));
                }
                stats.tokens = stats
                    .tokens
                    .checked_add(tokens)
                    .ok_or_else(|| here(ErrorKind::Overflow))?;
                stats.index_bytes = stats
                    .index_bytes
                    .checked_add(index)
                    .ok_or_else(|| here(ErrorKind::Overflow))?;
                stats.work = stats
                    .work
                    .checked_add(parsed_work)
                    .ok_or_else(|| here(ErrorKind::Overflow))?;
                if records.len() >= count {
                    return Err(here(ErrorKind::Overflow));
                }
                records.push(Record {
                    json,
                    payload: span,
                    line,
                });
                start = end;
            }
        }
        if records.len() != count || start != input.len() {
            return Err(initial(ErrorKind::Overflow));
        }
        stats.records = records.len();
        Ok(Self {
            records,
            input_len: input.len(),
            eof_line,
            stats,
        })
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn get(&self, ordinal: usize) -> Option<RecordRef<'_, 'a>> {
        self.records.get(ordinal).map(|_| RecordRef {
            batch: self,
            ordinal,
        })
    }
    pub fn cursor(&self) -> NdjsonCursor<'_, 'a> {
        NdjsonCursor {
            batch: self,
            next: 0,
        }
    }
    pub fn stats(&self) -> BatchStats {
        self.stats
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RecordRef<'d, 'a> {
    batch: &'d NdjsonBatch<'a>,
    ordinal: usize,
}
impl<'d, 'a> RecordRef<'d, 'a> {
    pub fn root(self) -> Node<'d, 'a> {
        self.batch.records[self.ordinal].json.root()
    }
    pub fn ordinal(self) -> usize {
        self.ordinal
    }
    pub fn line(self) -> usize {
        self.batch.records[self.ordinal].line
    }
    pub fn payload_span(self) -> Range<usize> {
        self.batch.records[self.ordinal].payload.clone()
    }
    /// Relocate diagnostic data only. This cannot construct a validated view.
    pub fn json_error(self, error: crate::Error) -> Error {
        relocate(error, &self.payload_span(), self.ordinal, self.line())
    }
}

#[derive(Debug)]
pub struct NdjsonCursor<'d, 'a> {
    batch: &'d NdjsonBatch<'a>,
    next: usize,
}
impl<'d, 'a> NdjsonCursor<'d, 'a> {
    pub fn position(&self) -> usize {
        self.next
    }
    pub fn remaining(&self) -> usize {
        self.batch()
            .len()
            .checked_sub(self.next)
            .expect("cursor stays in batch bounds")
    }
    pub fn peek(&self) -> Option<RecordRef<'d, 'a>> {
        self.batch().get(self.next)
    }
    pub fn read_raw(&mut self) -> Result<RecordRef<'d, 'a>, Error> {
        let record = self.peek().ok_or_else(|| self.end_error())?;
        let next = self
            .next
            .checked_add(1)
            .ok_or_else(|| self.current_error(ErrorKind::Overflow))?;
        self.commit_to(next);
        Ok(record)
    }
    pub(crate) fn batch(&self) -> &'d NdjsonBatch<'a> {
        self.batch
    }
    pub(crate) fn end_error(&self) -> Error {
        Error {
            kind: ErrorKind::End,
            offset: self.batch().input_len,
            record: self.next,
            line: self.batch().eof_line,
        }
    }
    pub(crate) fn current_error(&self, kind: ErrorKind) -> Error {
        if let Some(record) = self.peek() {
            Error {
                kind,
                offset: record.payload_span().start,
                record: record.ordinal(),
                line: record.line(),
            }
        } else {
            Error {
                kind,
                ..self.end_error()
            }
        }
    }
    pub(crate) fn commit_to(&mut self, end: usize) {
        assert!(
            self.next <= end && end <= self.batch().len(),
            "prevalidated cursor commit stays monotonic and in bounds"
        );
        self.next = end;
    }
}
