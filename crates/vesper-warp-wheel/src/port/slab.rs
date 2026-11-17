//! The token slab of the Vesper build (pattern b of the porting recipe):
//! Vesper schedules wheel entry indices, and the payload of entry `i` lives
//! in slot `i` here. Every slot is allocated up front, so storing and taking
//! a payload never allocates.

pub(crate) struct Slab<T> {
    slots: Vec<Option<T>>,
}

impl<T> Slab<T> {
    pub(crate) fn new(capacity: u32) -> Self {
        let mut slots = Vec::with_capacity(capacity as usize);
        slots.resize_with(capacity as usize, || None);
        Self { slots }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Store the payload of the entry Vesper just placed at `index`.
    pub(crate) fn put(&mut self, index: u64, value: T) {
        let slot = &mut self.slots[index as usize];
        debug_assert!(slot.is_none(), "Vesper reused a pending entry");
        *slot = Some(value);
    }

    /// Take the payload of the entry Vesper just released at `index`.
    pub(crate) fn take(&mut self, index: u64) -> T {
        self.slots[index as usize]
            .take()
            .expect("a released Vesper entry always holds a payload")
    }

    /// Move a payload from entry `from` to entry `to` (a re-insert).
    pub(crate) fn relocate(&mut self, from: u64, to: u64) {
        if from != to {
            let value = self.take(from);
            self.put(to, value);
        }
    }
}
