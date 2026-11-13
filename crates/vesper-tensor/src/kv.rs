//! Bounded CPU key/value storage with 16-token pages.
//!
//! The default format stores BF16 as opaque little-endian two-byte elements in
//! `[head, width]` order; no floating-point conversion or finiteness policy is
//! applied to it. [`KvPool::with_format`] can instead store little-endian FP32
//! elements (bit-exact copies of the caller's values) for inference that must
//! not round its cache. The
//! pool owns its backing bytes; a non-clone sequence handle authorizes access.
//! Explicit release is required: dropping a handle does not reclaim its pages.
//! This host ownership protocol does not implement source-language linearity.
use crate::next_owner;

pub const TOKENS_PER_PAGE: usize = 16;
const MAX_STORAGE: usize = 64 * 1024 * 1024;
const MAX_PAGE_REFERENCES: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KvError {
    InvalidConfig,
    Overflow,
    Budget,
    AllocationFailed,
    SequenceLimit,
    OutOfPages,
    TokenLimit,
    Shape,
    Position,
    ForeignOwner,
    StaleGeneration,
    StaleCheckpoint,
    GenerationExhausted,
    /// The step's element format does not match the pool's [`KvFormat`].
    Format,
}

/// Element encoding of every key/value element in a pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KvFormat {
    /// Opaque little-endian BF16 bit patterns (two bytes per element).
    Bf16,
    /// Little-endian IEEE binary32 bit patterns (four bytes per element).
    F32,
}
impl KvFormat {
    pub const fn element_bytes(self) -> usize {
        match self {
            Self::Bf16 => 2,
            Self::F32 => 4,
        }
    }
}
impl std::fmt::Display for KvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for KvError {}

#[derive(Clone, Copy, Debug)]
pub struct KvConfig {
    pub heads: usize,
    pub head_dim: usize,
    pub pages: usize,
    pub max_sequences: usize,
    pub max_tokens_per_sequence: usize,
    /// Payload storage budget; excludes bounded Rust bookkeeping/allocator overhead.
    pub max_bytes: usize,
}
#[derive(Clone, Copy, Debug)]
pub struct Bf16Step<'a> {
    pub heads: usize,
    pub head_dim: usize,
    pub bytes: &'a [u8],
}
#[derive(Debug)]
pub struct KvToken<'a> {
    pub key: &'a [u8],
    pub value: &'a [u8],
}

/// A borrowed page of filled K/V token slots (see [`KvPool::read_page`]).
#[derive(Debug)]
pub struct KvPage<'a> {
    pub first_position: usize,
    pub tokens: usize,
    pub keys: &'a [u8],
    pub values: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    slot: usize,
    generation: u64,
}
/// An exclusive host sequence owner. Intentionally neither Clone nor Copy.
#[must_use = "release the sequence explicitly to reclaim its pages"]
#[derive(Debug)]
pub struct Sequence {
    pool: u64,
    key: Key,
}
/// A speculative boundary, valid only in its originating sequence and epoch.
/// Successful rollback invalidates every checkpoint from that epoch.
#[derive(Debug)]
pub struct Checkpoint {
    pool: u64,
    key: Key,
    epoch: u64,
    tokens: usize,
}
/// Release errors return the live owner so callers can retry on its real pool.
#[derive(Debug)]
pub struct ReleaseFailure {
    pub error: KvError,
    pub sequence: Sequence,
}
impl std::fmt::Display for ReleaseFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for ReleaseFailure {}

#[derive(Clone, Copy, Debug)]
struct Lease {
    page: usize,
    generation: u64,
}
#[derive(Debug)]
struct Page {
    owner: Option<Key>,
    generation: u64,
}
#[derive(Debug)]
struct Slot {
    generation: u64,
    active: bool,
    epoch: u64,
    tokens: usize,
    pages: Vec<Lease>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KvStats {
    pub active_sequences: usize,
    pub allocated_pages: usize,
    pub free_pages: usize,
    pub payload_bytes: usize,
}

#[derive(Debug)]
pub struct KvPool {
    identity: u64,
    config: KvConfig,
    format: KvFormat,
    token_bytes: usize,
    page_bytes: usize,
    storage: Vec<u8>,
    pages: Vec<Page>,
    slots: Vec<Slot>,
}

fn reserved<T>(count: usize) -> Result<Vec<T>, KvError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(count)
        .map_err(|_| KvError::AllocationFailed)?;
    Ok(result)
}
impl KvPool {
    /// Allocates payload and bounded sequence tables once. Append/read/rollback/
    /// release allocate no additional backing storage.
    pub fn new(config: KvConfig) -> Result<Self, KvError> {
        Self::with_format(config, KvFormat::Bf16)
    }
    /// As [`KvPool::new`], storing elements in `format`. The byte budget counts
    /// the format's element size.
    pub fn with_format(config: KvConfig, format: KvFormat) -> Result<Self, KvError> {
        if config.heads == 0
            || config.head_dim == 0
            || config.pages == 0
            || config.pages > 4096
            || config.max_sequences == 0
            || config.max_sequences > 1024
            || config.max_tokens_per_sequence == 0
            || config.max_tokens_per_sequence > 65_536
            || config.max_bytes == 0
            || config.max_bytes > MAX_STORAGE
        {
            return Err(KvError::InvalidConfig);
        }
        let elements = config
            .heads
            .checked_mul(config.head_dim)
            .ok_or(KvError::Overflow)?;
        let token_bytes = elements
            .checked_mul(format.element_bytes())
            .ok_or(KvError::Overflow)?;
        let page_bytes = token_bytes
            .checked_mul(TOKENS_PER_PAGE)
            .and_then(|n| n.checked_mul(2))
            .ok_or(KvError::Overflow)?;
        let bytes = page_bytes
            .checked_mul(config.pages)
            .ok_or(KvError::Overflow)?;
        if bytes > config.max_bytes {
            return Err(KvError::Budget);
        }
        if config.max_tokens_per_sequence > config.pages * TOKENS_PER_PAGE {
            return Err(KvError::InvalidConfig);
        }
        let refs = config.max_tokens_per_sequence.div_ceil(TOKENS_PER_PAGE);
        if refs
            .checked_mul(config.max_sequences)
            .ok_or(KvError::Overflow)?
            > MAX_PAGE_REFERENCES
        {
            return Err(KvError::Budget);
        }
        let identity = next_owner().map_err(|_| KvError::GenerationExhausted)?;
        let mut storage = reserved(bytes)?;
        storage.resize(bytes, 0);
        let mut pages = reserved(config.pages)?;
        pages.extend((0..config.pages).map(|_| Page {
            owner: None,
            generation: 0,
        }));
        let mut slots = reserved(config.max_sequences)?;
        for _ in 0..config.max_sequences {
            slots.push(Slot {
                generation: 0,
                active: false,
                epoch: 0,
                tokens: 0,
                pages: reserved(refs)?,
            });
        }
        Ok(Self {
            identity,
            config,
            format,
            token_bytes,
            page_bytes,
            storage,
            pages,
            slots,
        })
    }
    pub fn config(&self) -> KvConfig {
        self.config
    }
    pub fn format(&self) -> KvFormat {
        self.format
    }
    pub fn stats(&self) -> KvStats {
        let active_sequences = self.slots.iter().filter(|s| s.active).count();
        let allocated_pages = self.pages.iter().filter(|p| p.owner.is_some()).count();
        KvStats {
            active_sequences,
            allocated_pages,
            free_pages: self.pages.len() - allocated_pages,
            payload_bytes: self.storage.len(),
        }
    }
    pub fn begin(&mut self) -> Result<Sequence, KvError> {
        let slot = self
            .slots
            .iter()
            .position(|s| !s.active && s.generation < u64::MAX)
            .ok_or_else(|| {
                if self.slots.iter().any(|s| !s.active) {
                    KvError::GenerationExhausted
                } else {
                    KvError::SequenceLimit
                }
            })?;
        let state = &mut self.slots[slot];
        let generation = state
            .generation
            .checked_add(1)
            .ok_or(KvError::GenerationExhausted)?;
        state.generation = generation;
        state.active = true;
        state.epoch = 0;
        state.tokens = 0;
        debug_assert!(state.pages.is_empty());
        Ok(Sequence {
            pool: self.identity,
            key: Key { slot, generation },
        })
    }
    fn state(&self, sequence: &Sequence) -> Result<&Slot, KvError> {
        if sequence.pool != self.identity {
            return Err(KvError::ForeignOwner);
        }
        let state = self
            .slots
            .get(sequence.key.slot)
            .ok_or(KvError::StaleGeneration)?;
        if !state.active || state.generation != sequence.key.generation {
            return Err(KvError::StaleGeneration);
        }
        Ok(state)
    }
    fn lease(&self, key: Key, lease: Lease) -> Result<(), KvError> {
        let page = self.pages.get(lease.page).ok_or(KvError::StaleGeneration)?;
        if page.owner != Some(key) || page.generation != lease.generation {
            return Err(KvError::StaleGeneration);
        }
        Ok(())
    }
    fn validate_pages(&self, sequence: &Sequence) -> Result<(), KvError> {
        for &lease in &self.state(sequence)?.pages {
            self.lease(sequence.key, lease)?;
        }
        Ok(())
    }
    pub fn len(&self, sequence: &Sequence) -> Result<usize, KvError> {
        Ok(self.state(sequence)?.tokens)
    }
    pub fn is_empty(&self, sequence: &Sequence) -> Result<bool, KvError> {
        Ok(self.len(sequence)? == 0)
    }
    /// `position` must equal the current length. Both complete K/V steps are
    /// validated before allocation or mutation; errors retain the sequence.
    pub fn append(
        &mut self,
        sequence: &mut Sequence,
        position: usize,
        key: Bf16Step<'_>,
        value: Bf16Step<'_>,
    ) -> Result<(), KvError> {
        self.state(sequence)?;
        if self.format != KvFormat::Bf16 {
            return Err(KvError::Format);
        }
        for step in [key, value] {
            if step.heads != self.config.heads
                || step.head_dim != self.config.head_dim
                || step.bytes.len() != self.token_bytes
            {
                return Err(KvError::Shape);
            }
        }
        self.append_with(sequence, position, |out, plane| {
            out.copy_from_slice(if plane == 0 { key.bytes } else { value.bytes });
        })
    }
    /// Appends one `[heads, head_dim]` FP32 key/value step to an
    /// [`KvFormat::F32`] pool. Values are stored bit-exactly (including NaN
    /// payloads); the same validation and no-allocation rules as
    /// [`KvPool::append`] apply.
    pub fn append_f32(
        &mut self,
        sequence: &mut Sequence,
        position: usize,
        key: &[f32],
        value: &[f32],
    ) -> Result<(), KvError> {
        self.state(sequence)?;
        if self.format != KvFormat::F32 {
            return Err(KvError::Format);
        }
        let elements = self.config.heads * self.config.head_dim;
        if key.len() != elements || value.len() != elements {
            return Err(KvError::Shape);
        }
        self.append_with(sequence, position, |out, plane| {
            let source = if plane == 0 { key } else { value };
            for (chunk, v) in out.chunks_exact_mut(4).zip(source) {
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        })
    }
    fn append_with(
        &mut self,
        sequence: &mut Sequence,
        position: usize,
        write: impl Fn(&mut [u8], usize),
    ) -> Result<(), KvError> {
        let state = self.state(sequence)?;
        if position != state.tokens {
            return Err(KvError::Position);
        }
        let new_len = position.checked_add(1).ok_or(KvError::Overflow)?;
        if new_len > self.config.max_tokens_per_sequence {
            return Err(KvError::TokenLimit);
        }
        self.validate_pages(sequence)?;
        let new_page = position.is_multiple_of(TOKENS_PER_PAGE);
        let lease = if new_page {
            let page = self
                .pages
                .iter()
                .position(|p| p.owner.is_none() && p.generation < u64::MAX)
                .ok_or_else(|| {
                    if self.pages.iter().any(|p| p.owner.is_none()) {
                        KvError::GenerationExhausted
                    } else {
                        KvError::OutOfPages
                    }
                })?;
            Lease {
                page,
                generation: self.pages[page]
                    .generation
                    .checked_add(1)
                    .ok_or(KvError::GenerationExhausted)?,
            }
        } else {
            *state.pages.last().ok_or(KvError::StaleGeneration)?
        };
        // No fallible operation remains. Table capacity was reserved at creation.
        if new_page {
            let start = lease.page * self.page_bytes;
            self.storage[start..start + self.page_bytes].fill(0);
            self.pages[lease.page] = Page {
                owner: Some(sequence.key),
                generation: lease.generation,
            };
            self.slots[sequence.key.slot].pages.push(lease);
        }
        let start = lease.page * self.page_bytes + (position % TOKENS_PER_PAGE) * self.token_bytes;
        write(&mut self.storage[start..start + self.token_bytes], 0);
        let start = start + TOKENS_PER_PAGE * self.token_bytes;
        write(&mut self.storage[start..start + self.token_bytes], 1);
        self.slots[sequence.key.slot].tokens = new_len;
        Ok(())
    }
    pub fn read(&self, sequence: &Sequence, position: usize) -> Result<KvToken<'_>, KvError> {
        let state = self.state(sequence)?;
        if position >= state.tokens {
            return Err(KvError::Position);
        }
        let lease = state.pages[position / TOKENS_PER_PAGE];
        self.lease(sequence.key, lease)?;
        let start = lease.page * self.page_bytes + (position % TOKENS_PER_PAGE) * self.token_bytes;
        let value = start + TOKENS_PER_PAGE * self.token_bytes;
        Ok(KvToken {
            key: &self.storage[start..start + self.token_bytes],
            value: &self.storage[value..value + self.token_bytes],
        })
    }
    pub fn read_head(
        &self,
        sequence: &Sequence,
        position: usize,
        head: usize,
    ) -> Result<KvToken<'_>, KvError> {
        if head >= self.config.heads {
            return Err(KvError::Shape);
        }
        let token = self.read(sequence, position)?;
        let bytes = self.config.head_dim * self.format.element_bytes();
        let start = head * bytes;
        Ok(KvToken {
            key: &token.key[start..start + bytes],
            value: &token.value[start..start + bytes],
        })
    }
    /// Number of pages currently leased by `sequence`.
    pub fn page_count(&self, sequence: &Sequence) -> Result<usize, KvError> {
        Ok(self.state(sequence)?.pages.len())
    }
    /// Borrowed view of one leased page: only its filled token slots, keys and
    /// values each laid out as `[token, head, width]` elements of the pool's
    /// format. The page's lease is validated against its current owner.
    pub fn read_page(&self, sequence: &Sequence, index: usize) -> Result<KvPage<'_>, KvError> {
        let state = self.state(sequence)?;
        let lease = *state.pages.get(index).ok_or(KvError::Position)?;
        self.lease(sequence.key, lease)?;
        let first = index * TOKENS_PER_PAGE;
        let tokens = (state.tokens - first).min(TOKENS_PER_PAGE);
        let start = lease.page * self.page_bytes;
        let plane = TOKENS_PER_PAGE * self.token_bytes;
        let used = tokens * self.token_bytes;
        Ok(KvPage {
            first_position: first,
            tokens,
            keys: &self.storage[start..start + used],
            values: &self.storage[start + plane..start + plane + used],
        })
    }
    pub fn checkpoint(&self, sequence: &Sequence) -> Result<Checkpoint, KvError> {
        let state = self.state(sequence)?;
        if state.epoch == u64::MAX {
            return Err(KvError::GenerationExhausted);
        }
        Ok(Checkpoint {
            pool: self.identity,
            key: sequence.key,
            epoch: state.epoch,
            tokens: state.tokens,
        })
    }
    /// Roll back to an epoch-bound checkpoint. Clears every removed token's
    /// bytes (including the retained page tail) and frees/zeros complete pages.
    /// A failed rollback leaves both owner and all stored bytes unchanged.
    pub fn rollback(
        &mut self,
        sequence: &mut Sequence,
        checkpoint: &Checkpoint,
    ) -> Result<(), KvError> {
        let state = self.state(sequence)?;
        if checkpoint.pool != self.identity || checkpoint.key != sequence.key {
            return Err(KvError::ForeignOwner);
        }
        if checkpoint.epoch != state.epoch || checkpoint.tokens > state.tokens {
            return Err(KvError::StaleCheckpoint);
        }
        let epoch = state
            .epoch
            .checked_add(1)
            .ok_or(KvError::GenerationExhausted)?;
        self.validate_pages(sequence)?;
        let keep = checkpoint.tokens.div_ceil(TOKENS_PER_PAGE);
        let slot = sequence.key.slot;
        while self.slots[slot].pages.len() > keep {
            let lease = self.slots[slot]
                .pages
                .pop()
                .expect("prevalidated nonempty page table");
            self.clear_page(lease.page);
        }
        let tail = checkpoint.tokens % TOKENS_PER_PAGE;
        if tail != 0 {
            let page = self.slots[slot].pages[keep - 1].page;
            let start = page * self.page_bytes;
            let plane = TOKENS_PER_PAGE * self.token_bytes;
            self.storage[start + tail * self.token_bytes..start + plane].fill(0);
            self.storage[start + plane + tail * self.token_bytes..start + self.page_bytes].fill(0);
        }
        self.slots[slot].tokens = checkpoint.tokens;
        self.slots[slot].epoch = epoch;
        Ok(())
    }
    fn clear_page(&mut self, page: usize) {
        let start = page * self.page_bytes;
        self.storage[start..start + self.page_bytes].fill(0);
        self.pages[page].owner = None;
    }
    /// Consumes the sequence only on success; foreign/stale owners are returned
    /// in `ReleaseFailure`. Release is also the explicit cancellation operation.
    pub fn release(&mut self, sequence: Sequence) -> Result<(), ReleaseFailure> {
        if let Err(error) = self.validate_pages(&sequence) {
            return Err(ReleaseFailure { error, sequence });
        }
        let slot = sequence.key.slot;
        while let Some(lease) = self.slots[slot].pages.pop() {
            self.clear_page(lease.page);
        }
        self.slots[slot].tokens = 0;
        self.slots[slot].active = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pool() -> KvPool {
        KvPool::new(KvConfig {
            heads: 1,
            head_dim: 1,
            pages: 2,
            max_sequences: 1,
            max_tokens_per_sequence: 32,
            max_bytes: 128,
        })
        .unwrap()
    }
    fn add(pool: &mut KvPool, seq: &mut Sequence, pos: usize) -> Result<(), KvError> {
        let step = Bf16Step {
            heads: 1,
            head_dim: 1,
            bytes: &[0xff, 0x7f],
        };
        pool.append(seq, pos, step, step)
    }
    #[test]
    fn truncated_tails_and_released_pages_are_actually_zero() {
        let mut pool = pool();
        let mut seq = pool.begin().unwrap();
        add(&mut pool, &mut seq, 0).unwrap();
        let mark = pool.checkpoint(&seq).unwrap();
        for i in 1..17 {
            add(&mut pool, &mut seq, i).unwrap();
        }
        pool.rollback(&mut seq, &mark).unwrap();
        let lease = pool.slots[0].pages[0];
        let start = lease.page * pool.page_bytes;
        assert_eq!(&pool.storage[start..start + 2], &[0xff, 0x7f]);
        assert!(pool.storage[start + 2..start + 32].iter().all(|b| *b == 0));
        assert!(pool.storage[start + 34..start + 64].iter().all(|b| *b == 0));
        for (i, page) in pool.pages.iter().enumerate() {
            if page.owner.is_none() {
                assert!(pool.storage[i * 64..i * 64 + 64].iter().all(|b| *b == 0));
            }
        }
        pool.release(seq).unwrap();
        assert!(pool.storage.iter().all(|b| *b == 0));
    }
    #[test]
    fn generation_exhaustion_and_stale_handles_fail_without_mutation() {
        let mut pool = pool();
        let mut seq = pool.begin().unwrap();
        for i in 0..16 {
            add(&mut pool, &mut seq, i).unwrap();
        }
        let free = pool.pages.iter().position(|p| p.owner.is_none()).unwrap();
        pool.pages[free].generation = u64::MAX;
        let before = pool.storage.clone();
        let stats = pool.stats();
        assert_eq!(
            add(&mut pool, &mut seq, 16),
            Err(KvError::GenerationExhausted)
        );
        assert_eq!(pool.storage, before);
        assert_eq!(pool.stats(), stats);
        assert_eq!(pool.len(&seq), Ok(16));
        let checkpoint = pool.checkpoint(&seq).unwrap();
        pool.slots[0].epoch = u64::MAX;
        let exhausted = Checkpoint {
            epoch: u64::MAX,
            ..checkpoint
        };
        assert_eq!(
            pool.rollback(&mut seq, &exhausted),
            Err(KvError::GenerationExhausted)
        );
        assert_eq!(pool.storage, before);
        assert_eq!(
            pool.checkpoint(&seq).unwrap_err(),
            KvError::GenerationExhausted
        );
        let stale = Sequence {
            pool: seq.pool,
            key: Key {
                generation: seq.key.generation - 1,
                ..seq.key
            },
        };
        assert_eq!(pool.read(&stale, 0).unwrap_err(), KvError::StaleGeneration);
        let failed = pool.release(stale).unwrap_err();
        assert_eq!(failed.error, KvError::StaleGeneration);
        assert_eq!(pool.storage, before);
        pool.release(seq).unwrap();
        pool.slots[0].generation = u64::MAX;
        let stats = pool.stats();
        assert_eq!(pool.begin().unwrap_err(), KvError::GenerationExhausted);
        assert_eq!(pool.stats(), stats);
    }
    #[test]
    fn page_generation_mismatch_is_checked_before_append_read_rollback_release() {
        let mut pool = pool();
        let mut seq = pool.begin().unwrap();
        add(&mut pool, &mut seq, 0).unwrap();
        let mark = pool.checkpoint(&seq).unwrap();
        let lease = pool.slots[0].pages[0];
        pool.pages[lease.page].generation += 1;
        let before = pool.storage.clone();
        assert_eq!(pool.read(&seq, 0).unwrap_err(), KvError::StaleGeneration);
        assert_eq!(add(&mut pool, &mut seq, 1), Err(KvError::StaleGeneration));
        assert_eq!(
            pool.rollback(&mut seq, &mark),
            Err(KvError::StaleGeneration)
        );
        let failed = pool.release(seq).unwrap_err();
        assert_eq!(failed.error, KvError::StaleGeneration);
        assert_eq!(pool.storage, before);
        pool.pages[lease.page].generation = lease.generation;
        pool.release(failed.sequence).unwrap();
    }
}
