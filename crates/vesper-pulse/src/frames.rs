//! Relocatable data frames for suspended native computations. Stored addresses
//! name a live frame and byte offset; no address points into a Vec allocation.
//! Machine code, continuation entry points and drop plans belong to the caller.
use std::{collections::BTreeMap, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameId(u64);
impl FrameId {
    pub fn bits(self) -> u64 {
        self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    pub frame: FrameId,
    pub offset: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    Allocation,
    ExhaustedIds,
    InvalidSize,
    StaleFrame,
    OutOfBounds,
    NotTopFrame,
}
impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for FrameError {}
struct Segment {
    bytes: Vec<u8>,
    used: usize,
}
struct Frame {
    id: FrameId,
    segment: usize,
    start: usize,
    length: usize,
}
/// A stack has unique ownership and deliberately has no Clone implementation.
/// IDs are monotonic, including after popping; a stale address cannot acquire a
/// new owner when its physical storage is reused.
pub struct FrameStack {
    segment_size: usize,
    segments: Vec<Segment>,
    frames: Vec<Frame>,
    locations: BTreeMap<FrameId, usize>,
    next_id: u64,
}
impl FrameStack {
    pub fn new(segment_size: usize) -> Result<Self, FrameError> {
        if segment_size == 0 || segment_size > u32::MAX as usize {
            return Err(FrameError::InvalidSize);
        }
        Ok(Self {
            segment_size,
            segments: vec![],
            frames: vec![],
            locations: BTreeMap::new(),
            next_id: 1,
        })
    }
    pub fn depth(&self) -> usize {
        self.frames.len()
    }
    pub fn segments(&self) -> usize {
        self.segments.len()
    }
    pub fn bytes(&self) -> usize {
        self.frames.iter().map(|f| f.length).sum()
    }
    pub fn live_id(&self, bits: u64) -> Result<FrameId, FrameError> {
        let id = FrameId(bits);
        if self.locations.contains_key(&id) {
            Ok(id)
        } else {
            Err(FrameError::StaleFrame)
        }
    }
    pub fn write_address(
        &mut self,
        destination: Address,
        value: Address,
    ) -> Result<(), FrameError> {
        self.range(value, 0)?;
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&value.frame.bits().to_le_bytes());
        bytes[8..12].copy_from_slice(&value.offset.to_le_bytes());
        self.write(destination, &bytes)
    }
    pub fn read_address(&self, source: Address) -> Result<Address, FrameError> {
        let bytes = self.read(source, 16)?;
        let address = Address {
            frame: self.live_id(u64::from_le_bytes(bytes[..8].try_into().unwrap()))?,
            offset: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        };
        self.range(address, 0)?;
        Ok(address)
    }
    pub fn push(&mut self, length: u32) -> Result<FrameId, FrameError> {
        let id = FrameId(self.next_id);
        let next = self
            .next_id
            .checked_add(1)
            .ok_or(FrameError::ExhaustedIds)?;
        let length = length as usize;
        let aligned = length
            .max(1)
            .checked_add(7)
            .ok_or(FrameError::InvalidSize)?
            & !7;
        let need_segment = self
            .segments
            .last()
            .is_none_or(|s| s.bytes.len() - s.used < aligned);
        if need_segment {
            let capacity = self.segment_size.max(aligned);
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(capacity)
                .map_err(|_| FrameError::Allocation)?;
            bytes.resize(capacity, 0);
            self.segments.push(Segment { bytes, used: 0 });
        }
        let segment = self.segments.len() - 1;
        let storage = &mut self.segments[segment];
        let start = storage.used;
        storage.bytes[start..start + aligned].fill(0);
        storage.used += aligned;
        self.locations.insert(id, self.frames.len());
        self.frames.push(Frame {
            id,
            segment,
            start,
            length,
        });
        self.next_id = next;
        Ok(id)
    }
    fn range(
        &self,
        address: Address,
        length: usize,
    ) -> Result<(usize, std::ops::Range<usize>), FrameError> {
        let index = self
            .locations
            .get(&address.frame)
            .ok_or(FrameError::StaleFrame)?;
        let frame = &self.frames[*index];
        let end = (address.offset as usize)
            .checked_add(length)
            .ok_or(FrameError::OutOfBounds)?;
        if end > frame.length {
            return Err(FrameError::OutOfBounds);
        }
        Ok((
            frame.segment,
            frame.start + address.offset as usize..frame.start + end,
        ))
    }
    pub fn read(&self, address: Address, length: usize) -> Result<&[u8], FrameError> {
        let (segment, range) = self.range(address, length)?;
        Ok(&self.segments[segment].bytes[range])
    }
    pub fn write(&mut self, address: Address, bytes: &[u8]) -> Result<(), FrameError> {
        let (segment, range) = self.range(address, bytes.len())?;
        self.segments[segment].bytes[range].copy_from_slice(bytes);
        Ok(())
    }
    pub fn read_word(&self, address: Address) -> Result<u64, FrameError> {
        Ok(u64::from_le_bytes(
            self.read(address, 8)?.try_into().unwrap(),
        ))
    }
    pub fn write_word(&mut self, address: Address, value: u64) -> Result<(), FrameError> {
        self.write(address, &value.to_le_bytes())
    }
    /// Return the frame's owned representation for its explicit drop/unpack
    /// plan. A failed allocation or out-of-order pop leaves the stack intact.
    pub fn pop(&mut self, id: FrameId) -> Result<Vec<u8>, FrameError> {
        let frame = self.frames.last().ok_or(FrameError::StaleFrame)?;
        if frame.id != id {
            return Err(if self.locations.contains_key(&id) {
                FrameError::NotTopFrame
            } else {
                FrameError::StaleFrame
            });
        }
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(frame.length)
            .map_err(|_| FrameError::Allocation)?;
        owned.extend_from_slice(
            &self.segments[frame.segment].bytes[frame.start..frame.start + frame.length],
        );
        let frame = self.frames.pop().unwrap();
        self.locations.remove(&id);
        self.segments[frame.segment].used = frame.start;
        while self.segments.last().is_some_and(|s| s.used == 0) {
            self.segments.pop();
        }
        Ok(owned)
    }
    /// Allocate every replacement before swapping any segment. Both success
    /// and allocation failure preserve the logical address space and ownership.
    pub fn relocate(&mut self) -> Result<(), FrameError> {
        let mut replacements = Vec::new();
        replacements
            .try_reserve_exact(self.segments.len())
            .map_err(|_| FrameError::Allocation)?;
        for segment in &self.segments {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(segment.bytes.len())
                .map_err(|_| FrameError::Allocation)?;
            bytes.extend_from_slice(&segment.bytes);
            replacements.push(Segment {
                bytes,
                used: segment.used,
            });
        }
        self.segments = replacements;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn segmented_relocation_preserves_places_and_mutation() {
        let mut stack = FrameStack::new(16).unwrap();
        let a = stack.push(16).unwrap();
        let b = stack.push(24).unwrap();
        let x = Address {
            frame: a,
            offset: 8,
        };
        let y = Address {
            frame: b,
            offset: 16,
        };
        stack.write_word(x, 20).unwrap();
        stack.write_word(y, 22).unwrap();
        let old_addresses = stack
            .segments
            .iter()
            .map(|s| s.bytes.as_ptr() as usize)
            .collect::<Vec<_>>();
        stack.relocate().unwrap();
        for (old, new) in old_addresses.iter().zip(&stack.segments) {
            assert_ne!(*old, new.bytes.as_ptr() as usize);
        }
        assert_eq!(
            stack.read_word(x).unwrap() + stack.read_word(y).unwrap(),
            42
        );
        stack.write_word(x, 42).unwrap();
        assert_eq!(stack.read_word(x).unwrap(), 42);
        assert_eq!(stack.pop(b).unwrap()[16..24], 22u64.to_le_bytes());
        assert_eq!(stack.segments(), 1);
    }
    #[test]
    fn popping_never_reuses_a_logical_address() {
        let mut stack = FrameStack::new(32).unwrap();
        let first = stack.push(8).unwrap();
        let stale = Address {
            frame: first,
            offset: 0,
        };
        stack.write_word(stale, 99).unwrap();
        stack.pop(first).unwrap();
        let second = stack.push(8).unwrap();
        assert_ne!(first, second);
        assert_eq!(stack.read_word(stale), Err(FrameError::StaleFrame));
        assert_eq!(
            stack.read_word(Address {
                frame: second,
                offset: 0
            }),
            Ok(0)
        );
    }
    #[test]
    fn stored_place_descriptors_need_no_pointer_fixup() {
        let mut stack = FrameStack::new(16).unwrap();
        let parent = stack.push(16).unwrap();
        let child = stack.push(8).unwrap();
        let slot = Address {
            frame: parent,
            offset: 0,
        };
        let referent = Address {
            frame: child,
            offset: 0,
        };
        stack.write_word(referent, 42).unwrap();
        stack.write_address(slot, referent).unwrap();
        stack.relocate().unwrap();
        assert_eq!(stack.read_word(stack.read_address(slot).unwrap()), Ok(42));
        stack.pop(child).unwrap();
        assert_eq!(stack.read_address(slot), Err(FrameError::StaleFrame));
    }
    #[test]
    fn bounds_and_lifo_errors_preserve_owners() {
        let mut stack = FrameStack::new(8).unwrap();
        let a = stack.push(8).unwrap();
        let b = stack.push(1).unwrap();
        assert_eq!(stack.pop(a), Err(FrameError::NotTopFrame));
        assert_eq!(
            stack.write_word(
                Address {
                    frame: b,
                    offset: 0
                },
                42
            ),
            Err(FrameError::OutOfBounds)
        );
        assert_eq!(
            stack.read(
                Address {
                    frame: a,
                    offset: u32::MAX
                },
                usize::MAX
            ),
            Err(FrameError::OutOfBounds)
        );
        assert_eq!(stack.depth(), 2);
        assert_eq!(stack.bytes(), 9);
        assert_eq!(stack.pop(b), Ok(vec![0]));
        assert_eq!(stack.pop(a), Ok(vec![0; 8]));
        assert_eq!(stack.depth(), 0);
        assert_eq!(stack.segments(), 0);
    }
}
