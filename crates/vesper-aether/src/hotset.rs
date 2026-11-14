//! Hot-set prefetch plans (§37.1.3).
//!
//! A [`HotSet`] is the set of snapshot pages a restored guest touched first,
//! recorded from one lazy-restore session's fault order. Before the next
//! clone's vCPUs run, the host resolves those pages proactively in batches of
//! at most [`PREFETCH_BATCH_PAGES`] contiguous pages (2 MiB), so the guest does
//! not stall on its cold working set. [`HotSet::runs`] coalesces the set into
//! those batches; the Linux backend issues one `UFFDIO_CONTINUE` (shared memfd)
//! or `UFFDIO_COPY` (private copy) per batch.
//!
//! The design streams batches with io_uring; this host refuses
//! `io_uring_setup` (`EPERM`), so batches are plain ioctls on the server
//! thread. The on-disk form ([`HotSet::encode`]) is untrusted input when read
//! back and is fully validated by [`HotSet::decode`].
#![forbid(unsafe_code)]

/// 2 MiB of 4 KiB pages.
pub const PREFETCH_BATCH_PAGES: usize = 512;
/// Ceiling on recorded hot pages (4 GiB of 4 KiB pages).
pub const MAX_HOT_PAGES: usize = 1 << 20;
const MAGIC: [u8; 4] = *b"AHS1";
const HEADER_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotSetError {
    OutOfRange {
        index: usize,
    },
    TooMany,
    BadBatch,
    Truncated,
    BadMagic,
    TrailingBytes,
    /// Encoded indices must be strictly increasing.
    NotCanonical,
}

/// `pages` contiguous pages starting at `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRun {
    pub start: usize,
    pub pages: usize,
}

/// A sorted, duplicate-free set of page indices below `page_count`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotSet {
    page_count: usize,
    pages: Vec<usize>,
}

impl HotSet {
    pub fn new(page_count: usize, indices: &[usize]) -> Result<Self, HotSetError> {
        if indices.len() > MAX_HOT_PAGES || page_count > u32::MAX as usize {
            return Err(HotSetError::TooMany);
        }
        if let Some(&index) = indices.iter().find(|&&i| i >= page_count) {
            return Err(HotSetError::OutOfRange { index });
        }
        let mut pages = indices.to_vec();
        pages.sort_unstable();
        pages.dedup();
        Ok(Self { page_count, pages })
    }
    /// The first `limit` distinct pages in fault order.
    pub fn from_fault_order(
        page_count: usize,
        order: &[usize],
        limit: usize,
    ) -> Result<Self, HotSetError> {
        let mut seen = std::collections::BTreeSet::new();
        let mut first = Vec::new();
        for &i in order {
            if i >= page_count {
                return Err(HotSetError::OutOfRange { index: i });
            }
            if first.len() >= limit.min(MAX_HOT_PAGES) {
                break;
            }
            if seen.insert(i) {
                first.push(i);
            }
        }
        Self::new(page_count, &first)
    }
    pub fn page_count(&self) -> usize {
        self.page_count
    }
    pub fn pages(&self) -> &[usize] {
        &self.pages
    }
    pub fn len(&self) -> usize {
        self.pages.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
    pub fn contains(&self, index: usize) -> bool {
        self.pages.binary_search(&index).is_ok()
    }
    /// Maximal contiguous runs, each split to at most `batch` pages.
    pub fn runs(&self, batch: usize) -> Result<Vec<PageRun>, HotSetError> {
        if batch == 0 {
            return Err(HotSetError::BadBatch);
        }
        let mut out: Vec<PageRun> = Vec::new();
        for &p in &self.pages {
            match out.last_mut() {
                Some(r) if r.start + r.pages == p && r.pages < batch => r.pages += 1,
                _ => out.push(PageRun { start: p, pages: 1 }),
            }
        }
        Ok(out)
    }
    /// `"AHS1" || page_count (LE u64) || count (LE u32) || count × index
    /// (LE u32)`, indices strictly increasing.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_BYTES + 4 * self.pages.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&(self.page_count as u64).to_le_bytes());
        out.extend_from_slice(&(self.pages.len() as u32).to_le_bytes());
        for &p in &self.pages {
            out.extend_from_slice(&(p as u32).to_le_bytes());
        }
        out
    }
    pub fn decode(bytes: &[u8], expected_page_count: usize) -> Result<Self, HotSetError> {
        if bytes.len() < HEADER_BYTES {
            return Err(HotSetError::Truncated);
        }
        if bytes[0..4] != MAGIC {
            return Err(HotSetError::BadMagic);
        }
        let page_count = u64::from_le_bytes(bytes[4..12].try_into().expect("8"));
        if page_count != expected_page_count as u64 {
            return Err(HotSetError::OutOfRange {
                index: expected_page_count,
            });
        }
        let count = u32::from_le_bytes(bytes[12..16].try_into().expect("4")) as usize;
        if count > MAX_HOT_PAGES {
            return Err(HotSetError::TooMany);
        }
        let body = &bytes[HEADER_BYTES..];
        let need = count.checked_mul(4).ok_or(HotSetError::TooMany)?;
        if body.len() < need {
            return Err(HotSetError::Truncated);
        }
        if body.len() > need {
            return Err(HotSetError::TrailingBytes);
        }
        let mut pages = Vec::with_capacity(count);
        for c in body.as_chunks::<4>().0 {
            let i = u32::from_le_bytes(*c) as usize;
            if i >= expected_page_count {
                return Err(HotSetError::OutOfRange { index: i });
            }
            if pages.last().is_some_and(|&l| l >= i) {
                return Err(HotSetError::NotCanonical);
            }
            pages.push(i);
        }
        Ok(Self {
            page_count: expected_page_count,
            pages,
        })
    }
}
