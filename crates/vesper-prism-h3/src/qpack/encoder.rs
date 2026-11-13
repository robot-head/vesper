//! The QPACK encoder (RFC 9204 §2.1, §4.3, §4.4, §4.5).
//!
//! A single-pass encoder: the Base of each field section is the Insert
//! Count when encoding starts, entries inserted for the section are
//! referenced with post-base indices, and the Required Insert Count is the
//! largest referenced absolute index plus one. The encoder only references
//! unacknowledged entries while the peer's blocked-stream budget allows it,
//! and never evicts an entry that is unacknowledged or still referenced by
//! an unacknowledged section (§2.1.1).

use std::collections::VecDeque;

use super::prefix::{decode_int, encode_int, encode_string};
use super::static_table::STATIC_TABLE;
use super::{Field, QpackError, Reason, Table, encode_ric, entry_size};

/// How the encoder uses the dynamic table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderConfig {
    /// The capacity we would like; the table uses
    /// `min(this, peer's SETTINGS_QPACK_MAX_TABLE_CAPACITY)`.
    pub max_table_capacity: u64,
    /// Fields whose entry is larger than this share of the capacity
    /// (in 1/256ths) are sent as literals instead of being inserted.
    pub max_entry_share: u64,
    /// Unacknowledged field sections tracked; past this the encoder stops
    /// referencing the dynamic table until acknowledgments arrive.
    pub max_outstanding_sections: usize,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            max_table_capacity: 4096,
            max_entry_share: 192,
            max_outstanding_sections: 256,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncoderStats {
    pub sections: u64,
    pub inserts: u64,
    pub static_refs: u64,
    pub dynamic_refs: u64,
    pub literals: u64,
    pub blocking_sections: u64,
}

struct Section {
    stream_id: u64,
    ric: u64,
    /// Absolute indices referenced (with repetition).
    refs: Vec<u64>,
}

pub struct Encoder {
    cfg: EncoderConfig,
    table: Table,
    /// Outstanding-reference count per entry, parallel to the table.
    refs: VecDeque<u32>,
    /// The peer's SETTINGS (zero until received).
    peer_capacity: u64,
    peer_blocked: u64,
    max_entries: u64,
    known_received: u64,
    /// Unacknowledged sections with dynamic references, in send order.
    outstanding: VecDeque<Section>,
    pending: Vec<u8>,
    out: Vec<u8>,
    stats: EncoderStats,
    failed: Option<QpackError>,
}

impl Encoder {
    pub fn new(cfg: EncoderConfig) -> Self {
        Self {
            cfg,
            table: Table::default(),
            refs: VecDeque::new(),
            peer_capacity: 0,
            peer_blocked: 0,
            max_entries: 0,
            known_received: 0,
            outstanding: VecDeque::new(),
            pending: Vec::new(),
            out: Vec::new(),
            stats: EncoderStats::default(),
            failed: None,
        }
    }

    /// Applies the peer's SETTINGS_QPACK_MAX_TABLE_CAPACITY and
    /// SETTINGS_QPACK_BLOCKED_STREAMS, and sets our table capacity (a Set
    /// Dynamic Table Capacity instruction) if it is non-zero.
    pub fn apply_peer_settings(&mut self, max_table_capacity: u64, blocked_streams: u64) {
        self.peer_capacity = max_table_capacity;
        self.peer_blocked = blocked_streams;
        self.max_entries = max_table_capacity / 32;
        let cap = self.cfg.max_table_capacity.min(max_table_capacity);
        if cap > 0 && cap != self.table.capacity() {
            encode_int(&mut self.out, 0x20, 5, cap);
            self.table.set_capacity(cap);
        }
    }

    pub fn stats(&self) -> EncoderStats {
        self.stats
    }
    pub fn insert_count(&self) -> u64 {
        self.table.insert_count()
    }
    pub fn known_received_count(&self) -> u64 {
        self.known_received
    }
    pub fn table_capacity(&self) -> u64 {
        self.table.capacity()
    }
    pub fn outstanding_sections(&self) -> usize {
        self.outstanding.len()
    }

    /// Streams with a section whose Required Insert Count exceeds the
    /// Known Received Count: those that may be blocked at the decoder.
    pub fn blocking_streams(&self) -> usize {
        let mut ids: Vec<u64> = self
            .outstanding
            .iter()
            .filter(|s| s.ric > self.known_received)
            .map(|s| s.stream_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    }

    fn may_block(&self, stream_id: u64) -> bool {
        let already = self
            .outstanding
            .iter()
            .any(|s| s.stream_id == stream_id && s.ric > self.known_received);
        already || (self.blocking_streams() as u64) < self.peer_blocked
    }

    fn slot(&self, abs: u64) -> usize {
        (abs - self.table.first_index()) as usize
    }

    fn evictable(&self, abs: u64) -> bool {
        abs < self.known_received && self.refs[self.slot(abs)] == 0
    }

    /// Inserts if the entries that must go are all evictable.
    fn try_insert(&mut self, name: &[u8], value: &[u8]) -> Option<u64> {
        let size = entry_size(name, value);
        if size * 256 > self.table.capacity() * self.cfg.max_entry_share {
            return None;
        }
        let n = self.table.evictions_for(size)?;
        let first = self.table.first_index();
        if (first..first + n as u64).any(|abs| !self.evictable(abs)) {
            return None;
        }
        // Instruction: name reference (static or dynamic) or literal name.
        if let Some(i) = static_name(name) {
            encode_int(&mut self.out, 0xc0, 6, i as u64);
        } else if let Some(abs) = self.dynamic_name(name, true) {
            let rel = self.table.insert_count() - abs - 1;
            encode_int(&mut self.out, 0x80, 6, rel);
        } else {
            encode_string(&mut self.out, 0x40, 5, name);
        }
        encode_string(&mut self.out, 0, 7, value);
        for _ in 0..n {
            self.refs.pop_front();
        }
        self.table.insert(name.into(), value.into());
        self.refs.push_back(0);
        self.stats.inserts += 1;
        Some(self.table.insert_count() - 1)
    }

    /// Newest entry with this name and value usable by a section that
    /// `may_block` or not.
    fn dynamic_exact(&self, name: &[u8], value: &[u8], may_block: bool) -> Option<u64> {
        let limit = if may_block {
            self.table.insert_count()
        } else {
            self.known_received
        };
        (self.table.first_index()..limit)
            .rev()
            .find(|&abs| self.table.get(abs) == Some((name, value)))
    }

    fn dynamic_name(&self, name: &[u8], may_block: bool) -> Option<u64> {
        let limit = if may_block {
            self.table.insert_count()
        } else {
            self.known_received
        };
        (self.table.first_index()..limit)
            .rev()
            .find(|&abs| self.table.get(abs).is_some_and(|(n, _)| n == name))
    }

    /// Encodes one field section for `stream_id`. Encoder-stream
    /// instructions it needs are queued for [`Encoder::take_encoder_stream`],
    /// which the caller must send before (or with) the section.
    pub fn encode(&mut self, stream_id: u64, fields: &[Field]) -> Vec<u8> {
        self.stats.sections += 1;
        let base = self.table.insert_count();
        let use_dynamic = self.table.capacity() > 0
            && self.outstanding.len() < self.cfg.max_outstanding_sections
            && self.failed.is_none();
        let may_block = use_dynamic && self.may_block(stream_id);
        let mut lines = Vec::new();
        let mut refs: Vec<u64> = Vec::new();
        for f in fields {
            let exact = static_exact(&f.name, &f.value);
            if let (Some(i), false) = (exact, f.sensitive) {
                encode_int(&mut lines, 0xc0, 6, i as u64);
                self.stats.static_refs += 1;
                continue;
            }
            if use_dynamic && !f.sensitive {
                let abs = self
                    .dynamic_exact(&f.name, &f.value, may_block)
                    .or_else(|| {
                        if may_block {
                            self.try_insert(&f.name, &f.value)
                        } else {
                            // Insert for later sections (once acknowledged)
                            // but do not reference it: that would block.
                            if self.dynamic_exact(&f.name, &f.value, true).is_none() {
                                self.try_insert(&f.name, &f.value);
                            }
                            None
                        }
                    });
                if let Some(abs) = abs {
                    if abs < base {
                        encode_int(&mut lines, 0x80, 6, base - abs - 1);
                    } else {
                        encode_int(&mut lines, 0x10, 4, abs - base);
                    }
                    let slot = self.slot(abs);
                    self.refs[slot] += 1;
                    refs.push(abs);
                    self.stats.dynamic_refs += 1;
                    continue;
                }
            }
            // A literal, with a name reference where one exists.
            let n = if f.sensitive { 0x20 } else { 0 };
            self.stats.literals += 1;
            if let Some(i) = static_name(&f.name) {
                encode_int(&mut lines, 0x50 | n, 4, i as u64);
            } else if let Some(abs) = use_dynamic
                .then(|| self.dynamic_name(&f.name, may_block))
                .flatten()
            {
                if abs < base {
                    encode_int(&mut lines, 0x40 | n, 4, base - abs - 1);
                } else {
                    encode_int(&mut lines, n >> 2, 3, abs - base);
                }
                let slot = self.slot(abs);
                self.refs[slot] += 1;
                refs.push(abs);
            } else {
                encode_string(&mut lines, 0x20 | n >> 1, 3, &f.name);
            }
            encode_string(&mut lines, 0, 7, &f.value);
        }
        let ric = refs.iter().max().map_or(0, |m| m + 1);
        let mut out = Vec::with_capacity(lines.len() + 4);
        encode_int(&mut out, 0, 8, encode_ric(ric, self.max_entries));
        if ric == 0 {
            out.push(0);
        } else if base >= ric {
            encode_int(&mut out, 0, 7, base - ric);
        } else {
            encode_int(&mut out, 0x80, 7, ric - base - 1);
        }
        out.extend_from_slice(&lines);
        if ric > 0 {
            if ric > self.known_received {
                self.stats.blocking_sections += 1;
            }
            self.outstanding.push_back(Section {
                stream_id,
                ric,
                refs,
            });
        }
        out
    }

    fn release(&mut self, section: Section) {
        let first = self.table.first_index();
        for abs in section.refs {
            // Referenced entries cannot have been evicted.
            let slot = (abs - first) as usize;
            self.refs[slot] -= 1;
        }
    }

    /// Feeds bytes from the peer's decoder stream. Errors are
    /// QPACK_DECODER_STREAM_ERROR connection errors and are sticky.
    pub fn on_decoder_stream(&mut self, bytes: &[u8]) -> Result<(), QpackError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        self.pending.extend_from_slice(bytes);
        let mut at = 0;
        while at < self.pending.len() {
            let buf = &self.pending[at..];
            let first = buf[0];
            let bits = if first & 0x80 != 0 { 7 } else { 6 };
            let (value, n) = match decode_int(buf, bits) {
                Ok(v) => v,
                Err(super::prefix::PrefixError::Incomplete) => break,
                Err(e) => return Err(self.fail(Reason::from(e))),
            };
            at += n;
            if first & 0x80 != 0 {
                // Section Acknowledgment.
                let Some(i) = self.outstanding.iter().position(|s| s.stream_id == value) else {
                    return Err(self.fail(Reason::UnexpectedAcknowledgment(value)));
                };
                let s = self.outstanding.remove(i).expect("found");
                self.known_received = self.known_received.max(s.ric);
                self.release(s);
            } else if first & 0x40 != 0 {
                // Stream Cancellation.
                while let Some(i) = self.outstanding.iter().position(|s| s.stream_id == value) {
                    let s = self.outstanding.remove(i).expect("found");
                    self.release(s);
                }
            } else {
                // Insert Count Increment.
                if value == 0 {
                    return Err(self.fail(Reason::ZeroIncrement));
                }
                let krc = self
                    .known_received
                    .checked_add(value)
                    .filter(|k| *k <= self.table.insert_count())
                    .ok_or(Reason::IncrementBeyondInserts);
                match krc {
                    Ok(k) => self.known_received = k,
                    Err(r) => return Err(self.fail(r)),
                }
            }
        }
        self.pending.drain(..at);
        // Decoder instructions are at most 10 bytes.
        if self.pending.len() > 16 {
            return Err(self.fail(Reason::InstructionTooLong));
        }
        Ok(())
    }

    fn fail(&mut self, r: Reason) -> QpackError {
        let e = QpackError::DecoderStream(r);
        self.failed = Some(e);
        e
    }

    /// Encoder-stream bytes to send.
    pub fn take_encoder_stream(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
}

fn static_exact(name: &[u8], value: &[u8]) -> Option<usize> {
    STATIC_TABLE
        .iter()
        .position(|&(n, v)| n == name && v == value)
}

fn static_name(name: &[u8]) -> Option<usize> {
    STATIC_TABLE.iter().position(|&(n, _)| n == name)
}
