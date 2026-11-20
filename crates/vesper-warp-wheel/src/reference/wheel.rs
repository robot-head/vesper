//! The Rust build of [`Wheel`] (see the module docs of `crate::wheel`).
//! Frozen from port wave P1 lane 3 (#900): bug fixes only.

use std::fmt;

use super::*;

const NIL: u32 = u32::MAX;
const FREE: u32 = u32::MAX;

struct Entry<T> {
    payload: Option<T>,
    deadline_ns: u64,
    tick: u64,
    seq: u64,
    generation: u64,
    prev: u32,
    next: u32,
    /// Index into `Wheel::lists`, or `FREE`.
    list: u32,
}

#[derive(Clone, Copy)]
struct Ends {
    head: u32,
    tail: u32,
}

const EMPTY: Ends = Ends {
    head: NIL,
    tail: NIL,
};

/// Fixed-capacity hierarchical hashed timing wheel owning payloads of `T`.
pub struct Wheel<T> {
    id: u64,
    tick_ns: u64,
    slot_bits: u32,
    levels: u32,
    slots: u32,
    entries: Vec<Entry<T>>,
    /// `levels * slots` wheel slots, then overflow, then ready.
    lists: Vec<Ends>,
    occupied: Vec<u64>,
    free_head: u32,
    len: u32,
    /// Next unopened tick; every tick below it has been opened.
    cursor: u64,
    now_ns: u64,
    next_seq: u64,
    /// Lower bound of the earliest overflow tick (may be stale-low after a
    /// cancel, which only makes the next re-placement earlier).
    overflow_min_tick: u64,
}

impl<T> fmt::Debug for Wheel<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Wheel")
            .field("id", &self.id)
            .field("tick_ns", &self.tick_ns)
            .field("slot_bits", &self.slot_bits)
            .field("levels", &self.levels)
            .field("capacity", &self.entries.len())
            .field("len", &self.len)
            .field("now_ns", &self.now_ns)
            .finish_non_exhaustive()
    }
}

impl<T> Wheel<T> {
    /// Build a wheel, preallocating all storage.
    pub fn new(config: WheelConfig) -> Result<Self, WheelConfigError> {
        if config.tick_ns == 0 {
            return Err(WheelConfigError::ZeroTick);
        }
        if !(1..=MAX_SLOT_BITS).contains(&config.slot_bits) {
            return Err(WheelConfigError::SlotBits(config.slot_bits));
        }
        if config.levels == 0
            || u32::from(config.levels) * u32::from(config.slot_bits) > MAX_SPAN_BITS
        {
            return Err(WheelConfigError::Levels(config.levels));
        }
        if config.capacity == 0 || config.capacity > MAX_CAPACITY {
            return Err(WheelConfigError::Capacity(config.capacity));
        }
        if config.start_ns > MAX_TIME_NS {
            return Err(WheelConfigError::StartOutOfRange);
        }
        let slot_bits = u32::from(config.slot_bits);
        let levels = u32::from(config.levels);
        let slots = 1u32 << slot_bits;
        let cap = config.capacity as usize;
        let mut entries = Vec::with_capacity(cap);
        for i in 0..config.capacity {
            entries.push(Entry {
                payload: None,
                deadline_ns: 0,
                tick: 0,
                seq: 0,
                generation: 0,
                prev: NIL,
                next: if i + 1 < config.capacity { i + 1 } else { NIL },
                list: FREE,
            });
        }
        let list_count = (levels * slots) as usize + 2;
        Ok(Self {
            id: next_wheel_id(),
            tick_ns: config.tick_ns,
            slot_bits,
            levels,
            slots,
            entries,
            lists: vec![EMPTY; list_count],
            occupied: vec![0; levels as usize],
            free_head: 0,
            len: 0,
            cursor: config.start_ns / config.tick_ns,
            now_ns: config.start_ns,
            next_seq: 0,
            overflow_min_tick: u64::MAX,
        })
    }

    /// Number of pending entries.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether no entry is pending.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Fixed capacity.
    pub fn capacity(&self) -> usize {
        self.entries.len()
    }

    /// Tick width in nanoseconds.
    pub fn tick_ns(&self) -> u64 {
        self.tick_ns
    }

    /// The wheel's clock: the latest `now` passed to `advance`/`poll`, or the
    /// configured start.
    pub fn now_ns(&self) -> u64 {
        self.now_ns
    }

    /// Store `payload` to fire at `deadline_ns`. A deadline at or before the
    /// wheel's clock is accepted and fires on the next poll.
    pub fn insert(&mut self, deadline_ns: u64, payload: T) -> Result<TimerHandle, InsertError<T>> {
        if deadline_ns > MAX_TIME_NS {
            return Err(InsertError {
                kind: InsertErrorKind::DeadlineOutOfRange,
                payload,
            });
        }
        let index = self.free_head;
        if index == NIL {
            return Err(InsertError {
                kind: InsertErrorKind::Full,
                payload,
            });
        }
        let seq = self.next_seq;
        // A u64 sequence cannot be exhausted by inserts in practice; wrapping
        // would only weaken FIFO order between entries 2^64 inserts apart.
        self.next_seq = self.next_seq.wrapping_add(1);
        let tick = deadline_ns / self.tick_ns;
        let entry = &mut self.entries[index as usize];
        self.free_head = entry.next;
        entry.payload = Some(payload);
        entry.deadline_ns = deadline_ns;
        entry.tick = tick;
        entry.seq = seq;
        let generation = entry.generation;
        self.len += 1;
        self.place(index);
        Ok(TimerHandle {
            wheel: self.id,
            index,
            generation,
        })
    }

    /// Deadline of a pending entry.
    pub fn deadline(&self, handle: &TimerHandle) -> Result<u64, HandleError> {
        let index = self.check(handle)?;
        Ok(self.entries[index as usize].deadline_ns)
    }

    /// Remove a pending entry and return its payload.
    pub fn cancel(&mut self, handle: TimerHandle) -> Result<T, HandleError> {
        let index = self.check(&handle)?;
        self.unlink(index);
        Ok(self.release(index))
    }

    /// Move the wheel's clock to `now_ns`, opening every tick that has work
    /// up to it. Does not yield entries; see [`Wheel::poll`].
    pub fn advance(&mut self, now_ns: u64) -> Result<(), AdvanceError> {
        if now_ns < self.now_ns {
            return Err(AdvanceError::Backwards {
                now_ns: self.now_ns,
                requested_ns: now_ns,
            });
        }
        if now_ns > MAX_TIME_NS {
            return Err(AdvanceError::OutOfRange);
        }
        self.now_ns = now_ns;
        let target = now_ns / self.tick_ns;
        while self.cursor <= target {
            match self.next_work_tick() {
                Some(tick) if tick <= target => {
                    self.cursor = tick;
                    self.open_tick(tick);
                    // tick <= target <= MAX_TIME_NS, so this cannot overflow.
                    self.cursor = tick + 1;
                }
                _ => {
                    self.cursor = target + 1;
                }
            }
        }
        Ok(())
    }

    /// Advance to `now_ns` and yield the earliest entry with
    /// `deadline_ns <= now_ns`, if any. Call repeatedly to drain all due
    /// entries in order.
    pub fn poll(&mut self, now_ns: u64) -> Result<Option<Expired<T>>, AdvanceError> {
        self.advance(now_ns)?;
        Ok(self.pop_due())
    }

    /// Earliest time at which polling may yield an entry or must do
    /// bookkeeping. Never later than the true earliest pending deadline;
    /// `None` when the wheel is empty.
    pub fn next_wake_hint_ns(&self) -> Option<u64> {
        let ready = self.lists[self.ready_list()];
        if ready.head != NIL {
            return Some(self.entries[ready.head as usize].deadline_ns);
        }
        self.next_work_tick()
            .map(|tick| tick.saturating_mul(self.tick_ns).max(self.now_ns))
    }

    /// Remove every pending entry, handing each `(deadline_ns, payload)` to
    /// `sink` in slab order (deterministic but unrelated to deadlines). All
    /// outstanding handles become stale. Does not allocate.
    pub fn drain(&mut self, mut sink: impl FnMut(u64, T)) {
        for index in 0..self.entries.len() as u32 {
            if self.entries[index as usize].list != FREE {
                let deadline = self.entries[index as usize].deadline_ns;
                self.unlink(index);
                let payload = self.release(index);
                sink(deadline, payload);
            }
        }
    }

    fn check(&self, handle: &TimerHandle) -> Result<u32, HandleError> {
        if handle.wheel != self.id {
            return Err(HandleError::Foreign);
        }
        let entry = self
            .entries
            .get(handle.index as usize)
            .ok_or(HandleError::Foreign)?;
        if entry.list == FREE || entry.generation != handle.generation {
            return Err(HandleError::Stale);
        }
        Ok(handle.index)
    }

    fn pop_due(&mut self) -> Option<Expired<T>> {
        let head = self.lists[self.ready_list()].head;
        if head == NIL || self.entries[head as usize].deadline_ns > self.now_ns {
            return None;
        }
        let deadline_ns = self.entries[head as usize].deadline_ns;
        self.unlink(head);
        Some(Expired {
            deadline_ns,
            payload: self.release(head),
        })
    }

    fn release(&mut self, index: u32) -> T {
        let entry = &mut self.entries[index as usize];
        let payload = entry
            .payload
            .take()
            .expect("pending timer entry always holds a payload");
        entry.generation = entry.generation.wrapping_add(1);
        entry.list = FREE;
        entry.prev = NIL;
        entry.next = self.free_head;
        self.free_head = index;
        self.len -= 1;
        payload
    }

    fn overflow_list(&self) -> usize {
        (self.levels * self.slots) as usize
    }

    fn ready_list(&self) -> usize {
        self.overflow_list() + 1
    }

    fn span_bits(&self) -> u32 {
        self.levels * self.slot_bits
    }

    fn slot_mask(&self) -> u64 {
        u64::from(self.slots - 1)
    }

    /// Place a detached entry relative to the cursor.
    fn place(&mut self, index: u32) {
        let tick = self.entries[index as usize].tick;
        if tick < self.cursor {
            self.insert_ready_sorted(index);
            return;
        }
        let diff = tick ^ self.cursor;
        let level = if diff == 0 {
            0
        } else {
            (63 - diff.leading_zeros()) / self.slot_bits
        };
        if level >= self.levels {
            self.overflow_min_tick = self.overflow_min_tick.min(tick);
            let list = self.overflow_list();
            self.push_back(list, index);
            return;
        }
        let slot = ((tick >> (level * self.slot_bits)) & self.slot_mask()) as u32;
        self.occupied[level as usize] |= 1u64 << slot;
        self.push_back((level * self.slots + slot) as usize, index);
    }

    fn push_back(&mut self, list: usize, index: u32) {
        let tail = self.lists[list].tail;
        {
            let entry = &mut self.entries[index as usize];
            entry.prev = tail;
            entry.next = NIL;
            entry.list = list as u32;
        }
        if tail == NIL {
            self.lists[list].head = index;
        } else {
            self.entries[tail as usize].next = index;
        }
        self.lists[list].tail = index;
    }

    fn unlink(&mut self, index: u32) {
        let (prev, next, list) = {
            let e = &self.entries[index as usize];
            (e.prev, e.next, e.list as usize)
        };
        if prev == NIL {
            self.lists[list].head = next;
        } else {
            self.entries[prev as usize].next = next;
        }
        if next == NIL {
            self.lists[list].tail = prev;
        } else {
            self.entries[next as usize].prev = prev;
        }
        if self.lists[list].head == NIL && list < self.overflow_list() {
            let level = list as u32 / self.slots;
            let slot = list as u32 % self.slots;
            self.occupied[level as usize] &= !(1u64 << slot);
        }
        if self.lists[list].head == NIL && list == self.overflow_list() {
            self.overflow_min_tick = u64::MAX;
        }
        let e = &mut self.entries[index as usize];
        e.prev = NIL;
        e.next = NIL;
    }

    /// Detach a whole list, returning its head (entries keep `next` links).
    fn take_list(&mut self, list: usize) -> u32 {
        let head = self.lists[list].head;
        self.lists[list] = EMPTY;
        if list < self.overflow_list() {
            let level = list as u32 / self.slots;
            let slot = list as u32 % self.slots;
            self.occupied[level as usize] &= !(1u64 << slot);
        }
        head
    }

    fn key(&self, index: u32) -> (u64, u64) {
        let e = &self.entries[index as usize];
        (e.deadline_ns, e.seq)
    }

    /// Insert into the sorted ready list, scanning from the tail. The new
    /// entry carries the newest sequence number, so it goes after every entry
    /// with a deadline `<=` its own.
    fn insert_ready_sorted(&mut self, index: u32) {
        let list = self.ready_list();
        let deadline = self.entries[index as usize].deadline_ns;
        let mut after = self.lists[list].tail;
        while after != NIL && self.entries[after as usize].deadline_ns > deadline {
            after = self.entries[after as usize].prev;
        }
        let next = if after == NIL {
            self.lists[list].head
        } else {
            self.entries[after as usize].next
        };
        {
            let e = &mut self.entries[index as usize];
            e.prev = after;
            e.next = next;
            e.list = list as u32;
        }
        if after == NIL {
            self.lists[list].head = index;
        } else {
            self.entries[after as usize].next = index;
        }
        if next == NIL {
            self.lists[list].tail = index;
        } else {
            self.entries[next as usize].prev = index;
        }
    }

    /// Smallest tick `>= cursor` at which a cascade, overflow re-placement or
    /// level-0 opening is needed.
    fn next_work_tick(&self) -> Option<u64> {
        let c = self.cursor;
        let mut best: Option<u64> = None;
        let mut consider = |t: u64| {
            best = Some(best.map_or(t, |b| b.min(t)));
        };
        for level in 0..self.levels {
            let bitmap = self.occupied[level as usize];
            if bitmap == 0 {
                continue;
            }
            let low = level * self.slot_bits;
            let rotation = low + self.slot_bits; // <= 63
            let position = ((c >> low) & self.slot_mask()) as u32;
            let base = (c >> rotation) << rotation;
            let ahead = bitmap & (u64::MAX << position);
            let tick = if ahead != 0 {
                let slot = u64::from(ahead.trailing_zeros());
                Some((base + (slot << low)).max(c))
            } else {
                // Unreachable under the placement invariant (occupied slots
                // are never behind the cursor); handled conservatively.
                let slot = u64::from(bitmap.trailing_zeros());
                base.checked_add(1u64 << rotation)
                    .and_then(|b| b.checked_add(slot << low))
            };
            if let Some(t) = tick {
                consider(t);
            }
        }
        if self.lists[self.overflow_list()].head != NIL {
            let span = self.span_bits();
            let block = (self.overflow_min_tick >> span) << span;
            consider(block.max(c));
        }
        best
    }

    /// Open tick `c` (== cursor): re-place overflow if its block is reached,
    /// cascade every level whose boundary `c` is, then move the level-0 slot
    /// to the ready list in sorted order.
    fn open_tick(&mut self, c: u64) {
        let overflow = self.overflow_list();
        if self.lists[overflow].head != NIL {
            let span = self.span_bits();
            if c >= (self.overflow_min_tick >> span) << span {
                self.overflow_min_tick = u64::MAX;
                let mut cur = self.take_list(overflow);
                while cur != NIL {
                    let next = self.entries[cur as usize].next;
                    self.place(cur);
                    cur = next;
                }
            }
        }
        for level in (1..self.levels).rev() {
            let low = level * self.slot_bits;
            if c & ((1u64 << low) - 1) != 0 {
                continue;
            }
            let slot = ((c >> low) & self.slot_mask()) as u32;
            let mut cur = self.take_list((level * self.slots + slot) as usize);
            while cur != NIL {
                let next = self.entries[cur as usize].next;
                self.place(cur);
                cur = next;
            }
        }
        let slot = (c & self.slot_mask()) as usize;
        let head = self.take_list(slot);
        if head == NIL {
            return;
        }
        let (head, tail) = self.sort_chain(head);
        let ready = self.ready_list();
        let ready_tail = self.lists[ready].tail;
        // Entries already on the ready list belong to earlier ticks, so the
        // sorted chain is appended after them.
        let mut prev = ready_tail;
        let mut cur = head;
        while cur != NIL {
            let e = &mut self.entries[cur as usize];
            e.prev = prev;
            e.list = ready as u32;
            prev = cur;
            cur = e.next;
        }
        if ready_tail == NIL {
            self.lists[ready].head = head;
        } else {
            self.entries[ready_tail as usize].next = head;
        }
        self.lists[ready].tail = tail;
    }

    /// Stable bottom-up merge sort of a `next`-linked chain by
    /// `(deadline_ns, seq)`, in place. Returns `(head, tail)`.
    fn sort_chain(&mut self, head: u32) -> (u32, u32) {
        let mut list = head;
        let mut width: usize = 1;
        loop {
            let mut p = list;
            list = NIL;
            let mut tail = NIL;
            let mut merges = 0usize;
            while p != NIL {
                merges += 1;
                let mut q = p;
                let mut psize = 0usize;
                while psize < width && q != NIL {
                    psize += 1;
                    q = self.entries[q as usize].next;
                }
                let mut qsize = width;
                while psize > 0 || (qsize > 0 && q != NIL) {
                    let take_p = if psize == 0 {
                        false
                    } else if qsize == 0 || q == NIL {
                        true
                    } else {
                        self.key(p) <= self.key(q)
                    };
                    let e = if take_p {
                        let e = p;
                        p = self.entries[p as usize].next;
                        psize -= 1;
                        e
                    } else {
                        let e = q;
                        q = self.entries[q as usize].next;
                        qsize -= 1;
                        e
                    };
                    if tail == NIL {
                        list = e;
                    } else {
                        self.entries[tail as usize].next = e;
                    }
                    tail = e;
                }
                p = q;
            }
            self.entries[tail as usize].next = NIL;
            if merges <= 1 {
                return (list, tail);
            }
            width = width.saturating_mul(2);
        }
    }
}

impl<T> Drop for Wheel<T> {
    fn drop(&mut self) {
        report_dropped(u64::from(self.len));
    }
}
