//! The Rust virtqueue walks that the Vesper port (`vesper/src/vq.ves`)
//! replaced (SH6 lane 14, #1409), kept as the tests' independent reference:
//! `differential` runs both on the same random rings and `abba` times them.
//! The Vesper walk is the only one the crate builds outside its tests.

use super::*;
use crate::virtio::{FLAG_INDIRECT, FLAG_NEXT};

mod abba;
mod differential;

struct Work(usize);
impl Work {
    fn charge(&mut self) -> Result<(), QueueError> {
        self.0 = self.0.checked_sub(1).ok_or(QueueError::Limit)?;
        Ok(())
    }
}

/// Builds a chain's buffer list with the checks every format shares.
struct ChainBuilder<'m> {
    memory: &'m GuestMemory,
    limits: QueueLimits,
    buffers: Vec<Buffer>,
    readable: Vec<u8>,
    writable_len: u64,
    total: u64,
    saw_write: bool,
}
impl<'m> ChainBuilder<'m> {
    fn new(memory: &'m GuestMemory, limits: QueueLimits) -> Self {
        Self {
            memory,
            limits,
            buffers: Vec::new(),
            readable: Vec::new(),
            writable_len: 0,
            total: 0,
            saw_write: false,
        }
    }
    fn push(&mut self, gpa: u64, len: u32, writable: bool) -> Result<(), QueueError> {
        if self.buffers.len() >= self.limits.max_chain_descriptors {
            return Err(QueueError::Limit);
        }
        if len == 0 {
            return Err(QueueError::EmptyBuffer);
        }
        gpa.checked_add(u64::from(len))
            .ok_or(QueueError::Overflow)?;
        if self.saw_write && !writable {
            return Err(QueueError::ReadAfterWrite);
        }
        self.saw_write |= writable;
        if !self.memory.contains(gpa, u64::from(len)) {
            return Err(QueueError::UnauthorizedRange);
        }
        self.total = self
            .total
            .checked_add(u64::from(len))
            .ok_or(QueueError::Overflow)?;
        if self.total > self.limits.max_chain_bytes {
            return Err(QueueError::Limit);
        }
        if writable {
            self.writable_len += u64::from(len);
        } else {
            // Single fetch: the readable bytes are copied out now, once.
            let at = self.readable.len();
            self.readable.resize(at + len as usize, 0);
            self.memory
                .read(gpa, &mut self.readable[at..])
                .map_err(|_| QueueError::UnauthorizedRange)?;
        }
        self.buffers.push(Buffer { gpa, len, writable });
        Ok(())
    }
}

/// A decoded 16-byte descriptor (either format's field order).
#[derive(Clone, Copy, Debug)]
struct RawDesc {
    addr: u64,
    len: u32,
    /// Split: `flags`. Packed: `flags`.
    flags: u16,
    /// Split: `next`. Packed: buffer id.
    tail: u16,
}
fn decode_split(b: &[u8]) -> RawDesc {
    RawDesc {
        addr: u64::from_le_bytes(b[0..8].try_into().expect("8")),
        len: u32::from_le_bytes(b[8..12].try_into().expect("4")),
        flags: u16::from_le_bytes(b[12..14].try_into().expect("2")),
        tail: u16::from_le_bytes(b[14..16].try_into().expect("2")),
    }
}
fn decode_packed(b: &[u8]) -> RawDesc {
    RawDesc {
        addr: u64::from_le_bytes(b[0..8].try_into().expect("8")),
        len: u32::from_le_bytes(b[8..12].try_into().expect("4")),
        tail: u16::from_le_bytes(b[12..14].try_into().expect("2")),
        flags: u16::from_le_bytes(b[14..16].try_into().expect("2")),
    }
}

impl VirtQueue {
    /// The Rust implementation of [`Self::capture`].
    pub(crate) fn capture_reference(
        &mut self,
        memory: &GuestMemory,
        hint: Option<AvailHint>,
    ) -> Result<Option<Chain>, QueueError> {
        match self.layout.format {
            RingFormat::Split => self.capture_split(memory, hint),
            RingFormat::Packed => self.capture_packed(memory, hint),
        }
    }

    fn capture_split(
        &mut self,
        memory: &GuestMemory,
        hint: Option<AvailHint>,
    ) -> Result<Option<Chain>, QueueError> {
        let size = self.layout.size;
        let read16 = |gpa| {
            memory
                .read_u16(gpa)
                .map_err(|_| QueueError::UnauthorizedRange)
        };
        let avail_idx = read16(self.layout.driver + 2)?;
        let pending = avail_idx.wrapping_sub(self.last_avail);
        if pending > size {
            return Err(QueueError::AvailOverrun);
        }
        if pending == 0 {
            return Ok(None);
        }
        if let Some(AvailHint::Split { next_idx }) = hint
            && next_idx == self.last_avail
        {
            // The ring holds more than the notification announced.
            self.stats.hint_mismatches += 1;
            return Ok(None);
        }
        let slot = u64::from(self.last_avail % size);
        let head = read16(self.layout.driver + 4 + 2 * slot)?;
        if head >= size {
            return Err(QueueError::InvalidBufferId);
        }
        if self.busy[usize::from(head)] != 0 {
            return Err(QueueError::BufferIdInUse);
        }
        let mut work = Work(self.limits.max_work);
        let mut builder = ChainBuilder::new(memory, self.limits);
        let mut descriptors: Vec<u16> = Vec::new();
        let mut indirect = false;
        let mut index = head;
        loop {
            work.charge()?;
            if descriptors.len() >= usize::from(size) || descriptors.contains(&index) {
                return Err(QueueError::ChainLoop);
            }
            if self.busy[usize::from(size) + usize::from(index)] != 0 {
                return Err(QueueError::DescriptorInUse);
            }
            let mut raw = [0u8; DESCRIPTOR_BYTES];
            memory
                .read(self.layout.desc + DESC * u64::from(index), &mut raw)
                .map_err(|_| QueueError::UnauthorizedRange)?;
            let d = decode_split(&raw);
            descriptors.push(index);
            if d.flags & !(FLAG_NEXT | FLAG_WRITE | FLAG_INDIRECT) != 0 {
                return Err(QueueError::UnsupportedFlags);
            }
            if d.flags & FLAG_INDIRECT != 0 {
                if !self.features.indirect {
                    return Err(QueueError::IndirectNotNegotiated);
                }
                if index != head || d.flags & FLAG_NEXT != 0 {
                    return Err(QueueError::IndirectPlacement);
                }
                indirect = true;
                self.walk_split_indirect(memory, d, &mut builder, &mut work)?;
                break;
            }
            builder.push(d.addr, d.len, d.flags & FLAG_WRITE != 0)?;
            if d.flags & FLAG_NEXT == 0 {
                break;
            }
            if d.tail >= size {
                return Err(QueueError::InvalidNext);
            }
            index = d.tail;
        }
        // Commit.
        for &i in &descriptors {
            self.busy[usize::from(size) + usize::from(i)] = 1;
        }
        self.busy[usize::from(head)] = 1;
        self.last_avail = self.last_avail.wrapping_add(1);
        Ok(Some(self.commit(head, 1, indirect, builder, descriptors)))
    }

    /// Reads a split indirect table once and walks it from entry 0 (§2.7.5.3).
    fn walk_split_indirect(
        &self,
        memory: &GuestMemory,
        d: RawDesc,
        builder: &mut ChainBuilder<'_>,
        work: &mut Work,
    ) -> Result<(), QueueError> {
        let table = self.read_table(memory, d)?;
        let entries = table.len() / DESCRIPTOR_BYTES;
        let mut seen = vec![false; entries];
        let mut i = 0usize;
        loop {
            work.charge()?;
            if seen[i] {
                return Err(QueueError::ChainLoop);
            }
            seen[i] = true;
            let e = decode_split(&table[i * DESCRIPTOR_BYTES..(i + 1) * DESCRIPTOR_BYTES]);
            if e.flags & FLAG_INDIRECT != 0 {
                return Err(QueueError::IndirectNested);
            }
            if e.flags & !(FLAG_NEXT | FLAG_WRITE) != 0 {
                return Err(QueueError::UnsupportedFlags);
            }
            builder.push(e.addr, e.len, e.flags & FLAG_WRITE != 0)?;
            if e.flags & FLAG_NEXT == 0 {
                return Ok(());
            }
            i = usize::from(e.tail);
            if i >= entries {
                return Err(QueueError::InvalidNext);
            }
        }
    }

    fn read_table(&self, memory: &GuestMemory, d: RawDesc) -> Result<Vec<u8>, QueueError> {
        let len = d.len as usize;
        if len == 0 || !len.is_multiple_of(DESCRIPTOR_BYTES) {
            return Err(QueueError::IndirectTableLength);
        }
        if len / DESCRIPTOR_BYTES > self.limits.max_chain_descriptors {
            return Err(QueueError::Limit);
        }
        let mut table = vec![0u8; len];
        memory
            .read(d.addr, &mut table)
            .map_err(|_| QueueError::UnauthorizedRange)?;
        Ok(table)
    }

    fn packed_available(flags: u16, wrap: bool) -> bool {
        (flags & FLAG_AVAIL != 0) == wrap && (flags & FLAG_USED != 0) != wrap
    }
    fn capture_packed(
        &mut self,
        memory: &GuestMemory,
        hint: Option<AvailHint>,
    ) -> Result<Option<Chain>, QueueError> {
        if let Some(AvailHint::Packed {
            next_off,
            next_wrap,
        }) = hint
            && (next_off, next_wrap) == self.avail
        {
            return Ok(None);
        }
        let size = usize::from(self.layout.size);
        if self.occupied == size {
            return Ok(None);
        }
        let mut work = Work(self.limits.max_work);
        let mut builder = ChainBuilder::new(memory, self.limits);
        let mut cursor = self.avail;
        let mut slots = 0u16;
        let mut indirect = false;
        let id;
        loop {
            work.charge()?;
            if self.occupied + usize::from(slots) >= size {
                return Err(if slots == 0 {
                    QueueError::Full
                } else {
                    QueueError::IncompleteChain
                });
            }
            let mut raw = [0u8; DESCRIPTOR_BYTES];
            memory
                .read(self.layout.desc + DESC * u64::from(cursor.0), &mut raw)
                .map_err(|_| QueueError::UnauthorizedRange)?;
            let d = decode_packed(&raw);
            if !Self::packed_available(d.flags, cursor.1) {
                if slots == 0 {
                    return Ok(None);
                }
                return Err(QueueError::IncompleteChain);
            }
            if d.flags & !(FLAG_NEXT | FLAG_WRITE | FLAG_INDIRECT | FLAG_AVAIL | FLAG_USED) != 0 {
                return Err(QueueError::UnsupportedFlags);
            }
            slots += 1;
            cursor = self.advance(cursor, 1);
            if d.flags & FLAG_INDIRECT != 0 {
                if !self.features.indirect {
                    return Err(QueueError::IndirectNotNegotiated);
                }
                if slots != 1 || d.flags & FLAG_NEXT != 0 {
                    return Err(QueueError::IndirectPlacement);
                }
                indirect = true;
                // §2.8.19: the table's entries are used in order; their
                // buffer ids are reserved and ignored.
                let table = self.read_table(memory, d)?;
                for e in table.as_chunks::<DESCRIPTOR_BYTES>().0 {
                    work.charge()?;
                    let e = decode_packed(e);
                    if e.flags & FLAG_INDIRECT != 0 {
                        return Err(QueueError::IndirectNested);
                    }
                    if e.flags & !(FLAG_WRITE | FLAG_NEXT) != 0 {
                        return Err(QueueError::UnsupportedFlags);
                    }
                    builder.push(e.addr, e.len, e.flags & FLAG_WRITE != 0)?;
                }
                id = d.tail;
                break;
            }
            builder.push(d.addr, d.len, d.flags & FLAG_WRITE != 0)?;
            if d.flags & FLAG_NEXT == 0 {
                id = d.tail;
                break;
            }
        }
        if usize::from(id) >= size {
            return Err(QueueError::InvalidBufferId);
        }
        if self.busy[usize::from(id)] != 0 {
            return Err(QueueError::BufferIdInUse);
        }
        self.busy[usize::from(id)] = 1;
        self.occupied += usize::from(slots);
        self.avail = cursor;
        Ok(Some(self.commit(id, slots, indirect, builder, Vec::new())))
    }

    fn commit(
        &mut self,
        id: u16,
        slots: u16,
        indirect: bool,
        builder: ChainBuilder<'_>,
        descriptors: Vec<u16>,
    ) -> Chain {
        let ChainBuilder {
            buffers,
            readable,
            writable_len,
            ..
        } = builder;
        self.commit_parts(
            id,
            slots,
            indirect,
            buffers,
            readable,
            writable_len,
            descriptors,
        )
    }
}
