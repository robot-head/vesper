//! Relocatable data frames: the movable part of a suspended continuation.
//!
//! Invariant (ABI v1, "base-relative data"): the frame contains no absolute
//! addresses of itself, and generated code holds its base address only between
//! a (re)entry and the next park. Every park returns the current base in
//! [`crate::abi::ResumeSignal::data_base`]; code must reload it. Under that
//! invariant the frame may be copied to any 8-aligned allocation while the
//! fiber is parked.
use vesper_pulse::frames::{Address, FrameError, FrameStack};

/// Poison written into a retired allocation after relocation. Generated code
/// that (incorrectly) kept a stale base across a park reads this pattern.
pub const POISON: u64 = 0xDEAD_5EED_DEAD_5EED;

pub struct DataFrame {
    words: Box<[u64]>,
    /// Previous allocations kept poisoned until the fiber next stops running,
    /// so a stale-base access is observable instead of a use-after-free.
    retired: Vec<Box<[u64]>>,
    relocations: u64,
}

impl DataFrame {
    pub fn new(bytes: usize) -> Self {
        let words = bytes.div_ceil(8).max(1);
        Self {
            words: vec![0; words].into_boxed_slice(),
            retired: vec![],
            relocations: 0,
        }
    }
    pub fn base(&self) -> u64 {
        self.words.as_ptr() as u64
    }
    pub fn len(&self) -> usize {
        self.words.len() * 8
    }
    pub fn is_empty(&self) -> bool {
        false
    }
    pub fn relocations(&self) -> u64 {
        self.relocations
    }
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: u64 storage reinterpreted as initialized bytes.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.len()) }
    }
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, exclusively borrowed.
        unsafe { std::slice::from_raw_parts_mut(self.words.as_mut_ptr().cast(), self.len()) }
    }
    pub fn word(&self, index: usize) -> Option<u64> {
        self.words.get(index).copied()
    }
    fn install(&mut self, words: Box<[u64]>) {
        let mut old = std::mem::replace(&mut self.words, words);
        old.fill(POISON);
        self.retired.push(old);
        self.relocations += 1;
    }
    /// Copy the frame into a fresh allocation. The new allocation is created
    /// while the old one is still live, so the addresses always differ.
    pub fn relocate(&mut self) {
        let fresh: Box<[u64]> = self.words.to_vec().into_boxed_slice();
        self.install(fresh);
    }
    /// Move the image through a `vesper_pulse` [`FrameStack`] frame, relocate
    /// that stack's segments, and read the image back into a new allocation.
    /// The stack is left as it was on every path.
    pub fn relocate_through(&mut self, frames: &mut FrameStack) -> Result<(), FrameError> {
        let len = u32::try_from(self.len()).map_err(|_| FrameError::InvalidSize)?;
        let id = frames.push(len)?;
        let at = Address {
            frame: id,
            offset: 0,
        };
        let result = (|| {
            frames.write(at, self.bytes())?;
            frames.relocate()?;
            let image = frames.read(at, self.len())?;
            let fresh: Box<[u64]> = image
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| u64::from_le_bytes(*c))
                .collect();
            Ok(fresh)
        })();
        let popped = frames.pop(id);
        let fresh = result?;
        popped?;
        self.install(fresh);
        Ok(())
    }
    /// Free retired allocations. Called once the fiber has run past the
    /// reload point (it parked again or exited).
    pub fn release_retired(&mut self) {
        self.retired.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relocation_moves_and_poisons() {
        let mut d = DataFrame::new(20);
        assert_eq!(d.len(), 24);
        d.bytes_mut()[..8].copy_from_slice(&41u64.to_le_bytes());
        let before = d.base();
        d.relocate();
        assert_ne!(before, d.base());
        assert_eq!(d.word(0), Some(41));
        assert_eq!(d.retired[0][0], POISON);
        let mut frames = FrameStack::new(16).unwrap();
        let mid = d.base();
        d.relocate_through(&mut frames).unwrap();
        assert_ne!(mid, d.base());
        assert_eq!(d.word(0), Some(41));
        assert_eq!(frames.depth(), 0);
        assert_eq!(d.relocations(), 2);
        d.release_retired();
        assert!(d.retired.is_empty());
    }
}
