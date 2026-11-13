//! The QPACK decoder (RFC 9204 §2.2, §4.3, §4.4, §4.5).

use std::collections::VecDeque;

use super::prefix::{decode_int, decode_string, encode_int};
use super::static_table::STATIC_TABLE;
use super::{Field, QpackError, Reason, Table, decode_ric, entry_size};

/// What this decoder advertises and how much it buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecoderConfig {
    /// SETTINGS_QPACK_MAX_TABLE_CAPACITY: the most the encoder may use.
    pub max_table_capacity: u64,
    /// SETTINGS_QPACK_BLOCKED_STREAMS.
    pub max_blocked_streams: u64,
    /// SETTINGS_MAX_FIELD_SECTION_SIZE (RFC 9114 §4.2.2), in the RFC's
    /// unit: name + value + 32 bytes per field line.
    pub max_field_section_size: u64,
    /// Encoded bytes held for all blocked streams together.
    pub max_blocked_bytes: usize,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            max_table_capacity: 4096,
            max_blocked_streams: 16,
            max_field_section_size: 64 * 1024,
            max_blocked_bytes: 64 * 1024,
        }
    }
}

/// A decoded field section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSection {
    pub stream_id: u64,
    pub fields: Vec<Field>,
    pub required_insert_count: u64,
}

/// The result of offering a field section to the decoder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decoded {
    Fields(FieldSection),
    /// It references entries not yet received; the decoder holds a copy and
    /// returns it from [`Decoder::poll_unblocked`] once they arrive.
    Blocked,
}

struct Blocked {
    stream_id: u64,
    ric: u64,
    data: Vec<u8>,
}

pub struct Decoder {
    cfg: DecoderConfig,
    max_entries: u64,
    table: Table,
    /// Unprocessed encoder-stream bytes (at most one partial instruction).
    pending: Vec<u8>,
    blocked: Vec<Blocked>,
    blocked_bytes: usize,
    ready: VecDeque<Result<FieldSection, QpackError>>,
    /// Decoder-stream bytes not yet taken.
    out: Vec<u8>,
    /// The encoder's Known Received Count as our instructions imply it.
    acknowledged: u64,
    failed: Option<QpackError>,
}

impl Decoder {
    pub fn new(cfg: DecoderConfig) -> Self {
        Self {
            cfg,
            max_entries: cfg.max_table_capacity / 32,
            table: Table::default(),
            pending: Vec::new(),
            blocked: Vec::new(),
            blocked_bytes: 0,
            ready: VecDeque::new(),
            out: Vec::new(),
            acknowledged: 0,
            failed: None,
        }
    }

    pub fn config(&self) -> DecoderConfig {
        self.cfg
    }
    pub fn insert_count(&self) -> u64 {
        self.table.insert_count()
    }
    pub fn table_size(&self) -> u64 {
        self.table.size()
    }
    pub fn table_capacity(&self) -> u64 {
        self.table.capacity()
    }
    pub fn table_len(&self) -> usize {
        self.table.len()
    }
    pub fn blocked_streams(&self) -> usize {
        self.blocked.len()
    }

    fn fail(&mut self, e: QpackError) -> QpackError {
        self.failed = Some(e);
        e
    }

    /// Feeds bytes received on the peer's encoder stream. Any blocked
    /// section that becomes decodable is decoded and queued for
    /// [`Decoder::poll_unblocked`]. Errors are connection errors and are
    /// sticky.
    pub fn on_encoder_stream(&mut self, bytes: &[u8]) -> Result<(), QpackError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        // One instruction holds at most two strings that must fit the
        // table; Huffman coding can expand by 30/8.
        let limit = (self.cfg.max_table_capacity as usize)
            .saturating_mul(4)
            .saturating_add(64);
        self.pending.extend_from_slice(bytes);
        let mut at = 0;
        while at < self.pending.len() {
            match self.instruction(at) {
                Ok(Some(used)) => at += used,
                Ok(None) => break,
                Err(r) => return Err(self.fail(QpackError::EncoderStream(r))),
            }
        }
        self.pending.drain(..at);
        if self.pending.len() > limit {
            return Err(self.fail(QpackError::EncoderStream(Reason::InstructionTooLong)));
        }
        self.unblock()
    }

    /// One encoder instruction at `pending[at..]`; `None` if incomplete.
    fn instruction(&mut self, at: usize) -> Result<Option<usize>, Reason> {
        let buf = &self.pending[at..];
        let max = self.cfg.max_table_capacity as usize;
        let first = buf[0];
        let r = (|| {
            if first & 0x80 != 0 {
                // Insert with name reference: 1 T index(6+), value(7+).
                let (index, n) = decode_int(buf, 6)?;
                let name = if first & 0x40 != 0 {
                    Ok(static_entry(index).map(|(n, _)| n.to_vec()))
                } else {
                    Ok(self.relative(index).map(|(n, _)| n.to_vec()))
                };
                let (value, m) = decode_string(&buf[n..], 7, max)?;
                Ok((name, Some(value), n + m))
            } else if first & 0x40 != 0 {
                // Insert with literal name: 01 H len(5+), value(7+).
                let (name, n) = decode_string(buf, 5, max)?;
                let (value, m) = decode_string(&buf[n..], 7, max)?;
                Ok((Ok(Some(name)), Some(value), n + m))
            } else if first & 0x20 != 0 {
                // Set dynamic table capacity: 001 capacity(5+).
                let (cap, n) = decode_int(buf, 5)?;
                Ok((Err(cap), None, n))
            } else {
                // Duplicate: 000 index(5+).
                let (index, n) = decode_int(buf, 5)?;
                let entry = self.relative(index).map(|(n, v)| (n.to_vec(), v.to_vec()));
                Ok((
                    Ok(entry.as_ref().map(|e| e.0.clone())),
                    entry.map(|e| e.1),
                    n,
                ))
            }
        })();
        let (name, value, used) = match r {
            Ok(x) => x,
            Err(super::prefix::PrefixError::Incomplete) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        match (name, value) {
            (Err(cap), _) => {
                if cap > self.cfg.max_table_capacity {
                    return Err(Reason::CapacityExceeded);
                }
                self.table.set_capacity(cap);
            }
            (Ok(Some(name)), Some(value)) => {
                if entry_size(&name, &value) > self.table.capacity() {
                    return Err(Reason::EntryTooLarge);
                }
                self.table.insert(name.into(), value.into());
            }
            (Ok(None), _) => {
                return Err(if first & 0xc0 == 0xc0 {
                    let (index, _) = decode_int(&self.pending[at..], 6).map_err(Reason::from)?;
                    Reason::BadStaticIndex(index)
                } else {
                    Reason::BadDynamicIndex
                });
            }
            (Ok(Some(_)), None) => unreachable!("inserts carry a value"),
        }
        Ok(Some(used))
    }

    /// An entry by encoder-stream relative index (insert count - 1 - index).
    fn relative(&self, index: u64) -> Option<(&[u8], &[u8])> {
        let abs = self
            .table
            .insert_count()
            .checked_sub(index)?
            .checked_sub(1)?;
        self.table.get(abs)
    }

    /// Decodes a field section received on `stream_id`.
    pub fn decode_section(&mut self, stream_id: u64, data: &[u8]) -> Result<Decoded, QpackError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        if self.blocked.iter().any(|b| b.stream_id == stream_id) {
            return Err(QpackError::Decompression(Reason::StreamAlreadyBlocked(
                stream_id,
            )));
        }
        let (encoded, n) =
            decode_int(data, 8).map_err(|e| self.fail(QpackError::Decompression(e.into())))?;
        let ric = decode_ric(encoded, self.max_entries, self.table.insert_count())
            .ok_or_else(|| self.fail(QpackError::Decompression(Reason::BadRequiredInsertCount)))?;
        if ric > self.table.insert_count() {
            // Validate the base prefix now, so garbage is not held.
            decode_int(&data[n..], 7)
                .map_err(|e| self.fail(QpackError::Decompression(e.into())))?;
            if self.blocked.len() as u64 >= self.cfg.max_blocked_streams {
                return Err(self.fail(QpackError::Decompression(Reason::TooManyBlockedStreams)));
            }
            if data.len() > self.cfg.max_blocked_bytes - self.blocked_bytes {
                return Err(self.fail(QpackError::Decompression(Reason::BlockedBytesLimit)));
            }
            self.blocked_bytes += data.len();
            self.blocked.push(Blocked {
                stream_id,
                ric,
                data: data.to_vec(),
            });
            return Ok(Decoded::Blocked);
        }
        self.finish(stream_id, ric, &data[n..]).map(Decoded::Fields)
    }

    fn finish(
        &mut self,
        stream_id: u64,
        ric: u64,
        rest: &[u8],
    ) -> Result<FieldSection, QpackError> {
        match self.lines(ric, rest) {
            Ok((fields, size)) => {
                if ric > 0 {
                    if size > self.cfg.max_field_section_size {
                        self.cancel_stream(stream_id);
                    } else {
                        encode_int(&mut self.out, 0x80, 7, stream_id);
                        self.acknowledged = self.acknowledged.max(ric);
                    }
                }
                if size > self.cfg.max_field_section_size {
                    return Err(QpackError::FieldSectionTooLarge { stream_id });
                }
                Ok(FieldSection {
                    stream_id,
                    fields,
                    required_insert_count: ric,
                })
            }
            Err(r) => Err(self.fail(QpackError::Decompression(r))),
        }
    }

    /// The field lines after the prefix's Required Insert Count.
    fn lines(&self, ric: u64, buf: &[u8]) -> Result<(Vec<Field>, u64), Reason> {
        let (delta, mut at) = decode_int(buf, 7)?;
        let base = if buf[0] & 0x80 == 0 {
            ric.checked_add(delta).ok_or(Reason::IntegerOverflow)?
        } else {
            if delta >= ric {
                return Err(Reason::NegativeBase);
            }
            ric - delta - 1
        };
        // Strings longer than the whole section limit cannot be valid.
        let max = usize::try_from(self.cfg.max_field_section_size).unwrap_or(usize::MAX);
        let mut fields = Vec::new();
        let mut size: u64 = 0;
        let mut max_ref: Option<u64> = None;
        let dynamic =
            |abs: Option<u64>, max_ref: &mut Option<u64>| -> Result<(&[u8], &[u8]), Reason> {
                let abs = abs.ok_or(Reason::BadDynamicIndex)?;
                if abs >= ric {
                    return Err(Reason::BadDynamicIndex);
                }
                *max_ref = Some(max_ref.map_or(abs, |m: u64| m.max(abs)));
                self.table.get(abs).ok_or(Reason::BadDynamicIndex)
            };
        while at < buf.len() {
            let b = &buf[at..];
            let first = b[0];
            let (field, used) = if first & 0x80 != 0 {
                // Indexed field line: 1 T index(6+).
                let (index, n) = decode_int(b, 6)?;
                let (name, value) = if first & 0x40 != 0 {
                    static_entry(index).ok_or(Reason::BadStaticIndex(index))?
                } else {
                    dynamic(
                        base.checked_sub(index).and_then(|v| v.checked_sub(1)),
                        &mut max_ref,
                    )?
                };
                (Field::new(name, value), n)
            } else if first & 0x40 != 0 {
                // Literal with name reference: 01 N T index(4+), value(7+).
                let (index, n) = decode_int(b, 4)?;
                let name = if first & 0x10 != 0 {
                    static_entry(index).ok_or(Reason::BadStaticIndex(index))?.0
                } else {
                    dynamic(
                        base.checked_sub(index).and_then(|v| v.checked_sub(1)),
                        &mut max_ref,
                    )?
                    .0
                };
                let (value, m) = decode_string(&b[n..], 7, max)?;
                (
                    Field {
                        name: name.to_vec(),
                        value,
                        sensitive: first & 0x20 != 0,
                    },
                    n + m,
                )
            } else if first & 0x20 != 0 {
                // Literal with literal name: 001 N H len(3+), value(7+).
                let (name, n) = decode_string(b, 3, max)?;
                let (value, m) = decode_string(&b[n..], 7, max)?;
                (
                    Field {
                        name,
                        value,
                        sensitive: first & 0x10 != 0,
                    },
                    n + m,
                )
            } else if first & 0x10 != 0 {
                // Indexed post-base: 0001 index(4+).
                let (index, n) = decode_int(b, 4)?;
                let (name, value) = dynamic(base.checked_add(index), &mut max_ref)?;
                (Field::new(name, value), n)
            } else {
                // Literal with post-base name reference: 0000 N index(3+).
                let (index, n) = decode_int(b, 3)?;
                let name = dynamic(base.checked_add(index), &mut max_ref)?.0;
                let (value, m) = decode_string(&b[n..], 7, max)?;
                (
                    Field {
                        name: name.to_vec(),
                        value,
                        sensitive: first & 0x08 != 0,
                    },
                    n + m,
                )
            };
            at += used;
            size = size.saturating_add(field.size());
            // Far past the limit: stop decoding rather than buffer more.
            if size > self.cfg.max_field_section_size.saturating_mul(2) {
                return Ok((fields, size));
            }
            fields.push(field);
        }
        // A Required Insert Count larger than the references need could
        // block a stream for nothing (§2.2.1 permits treating it as an error).
        if ric > 0 && max_ref.map(|m| m + 1) != Some(ric) {
            return Err(Reason::BadRequiredInsertCount);
        }
        Ok((fields, size))
    }

    fn unblock(&mut self) -> Result<(), QpackError> {
        let inserted = self.table.insert_count();
        let mut i = 0;
        while i < self.blocked.len() {
            if self.blocked[i].ric <= inserted {
                let b = self.blocked.remove(i);
                self.blocked_bytes -= b.data.len();
                let (_, n) = decode_int(&b.data, 8).expect("validated when blocked");
                match self.finish(b.stream_id, b.ric, &b.data[n..]) {
                    Ok(s) => self.ready.push_back(Ok(s)),
                    Err(e @ QpackError::FieldSectionTooLarge { .. }) => {
                        self.ready.push_back(Err(e))
                    }
                    Err(e) => return Err(e),
                }
            } else {
                i += 1;
            }
        }
        Ok(())
    }

    /// A previously blocked section, now decoded (or rejected as too large).
    pub fn poll_unblocked(&mut self) -> Option<Result<FieldSection, QpackError>> {
        self.ready.pop_front()
    }

    /// The stream was reset or abandoned: drop its blocked section and tell
    /// the encoder (Stream Cancellation), unless no dynamic table is allowed.
    pub fn cancel_stream(&mut self, stream_id: u64) {
        if let Some(i) = self.blocked.iter().position(|b| b.stream_id == stream_id) {
            let b = self.blocked.remove(i);
            self.blocked_bytes -= b.data.len();
        }
        self.ready
            .retain(|r| !matches!(r, Ok(s) if s.stream_id == stream_id));
        if self.cfg.max_table_capacity > 0 {
            encode_int(&mut self.out, 0x40, 6, stream_id);
        }
    }

    /// Decoder-stream bytes to send, ending with an Insert Count Increment
    /// for any insertions not yet acknowledged.
    pub fn take_decoder_stream(&mut self) -> Vec<u8> {
        let inserted = self.table.insert_count();
        if inserted > self.acknowledged {
            encode_int(&mut self.out, 0x00, 6, inserted - self.acknowledged);
            self.acknowledged = inserted;
        }
        std::mem::take(&mut self.out)
    }
}

fn static_entry(index: u64) -> Option<(&'static [u8], &'static [u8])> {
    STATIC_TABLE.get(usize::try_from(index).ok()?).copied()
}
