use std::sync::Arc;
use vesper_span::{LineIndex, Range};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreLimits {
    pub max_document_bytes: usize,
    pub max_documents: usize,
    pub max_total_bytes: usize,
    pub max_uri_bytes: usize,
    pub max_changes: usize,
    /// Conservative per-mutation work units; read access uses structural caps.
    pub max_work: usize,
    pub index_limits: vesper_span::Limits,
}
impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_document_bytes: 128 * 1024,
            max_documents: 32,
            max_total_bytes: 2 * 1024 * 1024,
            max_uri_bytes: 4096,
            max_changes: 64,
            max_work: 16 * 1024 * 1024,
            index_limits: vesper_span::Limits::default(),
        }
    }
}
impl StoreLimits {
    pub const HARD: Self = Self {
        max_document_bytes: 1024 * 1024,
        max_documents: 256,
        max_total_bytes: 16 * 1024 * 1024,
        max_uri_bytes: 16384,
        max_changes: 4096,
        max_work: 128 * 1024 * 1024,
        index_limits: vesper_span::Limits::HARD,
    };
    pub fn validate(self) -> Result<(), StoreError> {
        self.index_limits.validate().map_err(StoreError::Index)?;
        let h = Self::HARD;
        if self.max_document_bytes > h.max_document_bytes
            || self.max_document_bytes > self.index_limits.max_source_bytes
            || self.max_documents > h.max_documents
            || self.max_total_bytes > h.max_total_bytes
            || self.max_uri_bytes > h.max_uri_bytes
            || self.max_changes > h.max_changes
            || self.max_work > h.max_work
        {
            return Err(StoreError::InvalidLimits);
        }
        Ok(())
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    InvalidLimits,
    InvalidUri,
    AlreadyOpen,
    NotOpen,
    Version,
    DocumentLimit,
    TotalLimit,
    ChangeLimit,
    WorkLimit,
    RevisionOverflow,
    RangeLength,
    Allocation,
    Index(vesper_span::Error),
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "document store {self:?}")
    }
}
impl std::error::Error for StoreError {}
#[derive(Clone, Copy, Debug)]
pub struct ContentChange<'a> {
    pub range: Option<Range>,
    pub range_length: Option<u32>,
    pub text: &'a str,
}
#[derive(Debug)]
pub struct Document {
    uri: String,
    version: i32,
    index: LineIndex,
    text: Arc<str>,
    revision: u64,
}
impl Document {
    pub fn uri(&self) -> &str {
        &self.uri
    }
    pub fn version(&self) -> i32 {
        self.version
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn text_arc(&self) -> Arc<str> {
        Arc::clone(&self.text)
    }
    pub fn index(&self) -> &LineIndex {
        &self.index
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}
#[derive(Debug)]
pub struct DocumentStore {
    limits: StoreLimits,
    documents: Vec<Document>,
    total_bytes: usize,
    revision: u64,
}
impl DocumentStore {
    pub fn new(limits: StoreLimits) -> Result<Self, StoreError> {
        limits.validate()?;
        Ok(Self {
            limits,
            documents: Vec::new(),
            total_bytes: 0,
            revision: 0,
        })
    }
    pub fn len(&self) -> usize {
        self.documents.len()
    }
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Document> {
        self.documents.iter()
    }
    /// Read-only bounded linear lookup. The document/URI caps bound work; this
    /// does not consume or enforce the mutation work ledger.
    pub fn get(&self, uri: &str) -> Option<&Document> {
        if self.valid_uri(uri).is_err() {
            return None;
        }
        self.documents.iter().find(|d| d.uri == uri)
    }
    fn valid_uri(&self, uri: &str) -> Result<(), StoreError> {
        if uri.is_empty()
            || uri.len() > self.limits.max_uri_bytes
            || uri.chars().any(char::is_control)
        {
            Err(StoreError::InvalidUri)
        } else {
            Ok(())
        }
    }
    fn lookup_work(&self) -> Result<usize, StoreError> {
        self.documents
            .len()
            .checked_mul(
                self.limits
                    .max_uri_bytes
                    .checked_add(1)
                    .ok_or(StoreError::WorkLimit)?,
            )
            .ok_or(StoreError::WorkLimit)
    }
    fn lookup(&self, uri: &str) -> Result<usize, StoreError> {
        self.valid_uri(uri)?;
        let mut work = Work(self.limits.max_work);
        work.spend(uri.len())?;
        work.spend(self.lookup_work()?)?;
        self.documents
            .iter()
            .position(|d| d.uri == uri)
            .ok_or(StoreError::NotOpen)
    }
    fn next_revision(&self) -> Result<u64, StoreError> {
        self.revision
            .checked_add(1)
            .ok_or(StoreError::RevisionOverflow)
    }
    fn text_size(&self, n: usize, old: usize) -> Result<usize, StoreError> {
        if n > self.limits.max_document_bytes {
            return Err(StoreError::DocumentLimit);
        }
        let total = self
            .total_bytes
            .checked_sub(old)
            .and_then(|v| v.checked_add(n))
            .ok_or(StoreError::TotalLimit)?;
        if total > self.limits.max_total_bytes {
            return Err(StoreError::TotalLimit);
        }
        Ok(total)
    }
    pub fn open(&mut self, uri: &str, version: i32, text: &str) -> Result<&Document, StoreError> {
        self.valid_uri(uri)?;
        let mut work = Work(self.limits.max_work);
        work.spend(uri.len())?;
        work.spend(self.lookup_work()?)?;
        if self.documents.iter().any(|d| d.uri == uri) {
            return Err(StoreError::AlreadyOpen);
        }
        if self.len() >= self.limits.max_documents {
            return Err(StoreError::DocumentLimit);
        }
        let total = self.text_size(text.len(), 0)?;
        let revision = self.next_revision()?;
        work.spend(text.len().checked_mul(3).ok_or(StoreError::WorkLimit)?)?;
        let mut owned_uri = String::new();
        owned_uri
            .try_reserve_exact(uri.len())
            .map_err(|_| StoreError::Allocation)?;
        owned_uri.push_str(uri);
        let text: Arc<str> = Arc::from(text);
        let index = LineIndex::new(Arc::clone(&text), self.limits.index_limits)
            .map_err(StoreError::Index)?;
        self.documents
            .try_reserve_exact(1)
            .map_err(|_| StoreError::Allocation)?;
        self.documents.push(Document {
            uri: owned_uri,
            version,
            index,
            text,
            revision,
        });
        self.total_bytes = total;
        self.revision = revision;
        Ok(self.documents.last().expect("inserted document"))
    }
    /// Edits are sequential against the immediately preceding candidate. No
    /// document, version, index or store revision is changed until all succeed.
    pub fn change(
        &mut self,
        uri: &str,
        version: i32,
        changes: &[ContentChange<'_>],
    ) -> Result<&Document, StoreError> {
        let at = self.lookup(uri)?;
        let old = &self.documents[at];
        if version <= old.version {
            return Err(StoreError::Version);
        }
        if changes.len() > self.limits.max_changes {
            return Err(StoreError::ChangeLimit);
        }
        let revision = self.next_revision()?;
        let mut work = Work(self.limits.max_work);
        work.spend(uri.len())?;
        work.spend(self.lookup_work()?)?;
        work.spend(old.text.len().checked_mul(2).ok_or(StoreError::WorkLimit)?)?;
        // Rebuilding the initial index avoids a mutable/shared index or a Clone
        // requirement. Both old and candidate retain immutable source ownership.
        let mut text = Arc::clone(&old.text);
        let mut index = LineIndex::new(Arc::clone(&text), self.limits.index_limits)
            .map_err(StoreError::Index)?;
        let mut total = self.total_bytes;
        for change in changes {
            work.spend(1)?;
            let (start, end) = if let Some(range) = change.range {
                work.spend(text.len().checked_mul(2).ok_or(StoreError::WorkLimit)?)?;
                let span = index.span(range).map_err(StoreError::Index)?;
                if let Some(expected) = change.range_length {
                    work.spend(span.end - span.start)?;
                    if text[span.start..span.end].encode_utf16().count() != expected as usize {
                        return Err(StoreError::RangeLength);
                    }
                }
                (span.start, span.end)
            } else {
                if change.range_length.is_some() {
                    return Err(StoreError::RangeLength);
                }
                (0, text.len())
            };
            let n = text
                .len()
                .checked_sub(end - start)
                .and_then(|v| v.checked_add(change.text.len()))
                .ok_or(StoreError::DocumentLimit)?;
            total = self.text_size(n, old.text.len())?;
            work.spend(n.checked_mul(4).ok_or(StoreError::WorkLimit)?)?;
            let mut candidate = String::new();
            candidate
                .try_reserve_exact(n)
                .map_err(|_| StoreError::Allocation)?;
            candidate.push_str(&text[..start]);
            candidate.push_str(change.text);
            candidate.push_str(&text[end..]);
            text = Arc::from(candidate);
            index = LineIndex::new(Arc::clone(&text), self.limits.index_limits)
                .map_err(StoreError::Index)?;
        }
        let document = &mut self.documents[at];
        document.text = text;
        document.index = index;
        document.version = version;
        document.revision = revision;
        self.total_bytes = total;
        self.revision = revision;
        Ok(document)
    }
    pub fn close(&mut self, uri: &str) -> Result<Document, StoreError> {
        let at = self.lookup(uri)?;
        let revision = self.next_revision()?;
        let document = self.documents.remove(at);
        self.total_bytes -= document.text.len();
        self.revision = revision;
        Ok(document)
    }
}
struct Work(usize);
impl Work {
    fn spend(&mut self, n: usize) -> Result<(), StoreError> {
        self.0 = self.0.checked_sub(n).ok_or(StoreError::WorkLimit)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revision_overflow_is_transactional() {
        let mut store = DocumentStore::new(StoreLimits::default()).unwrap();
        store.open("opaque:a", 1, "x").unwrap();
        store.revision = u64::MAX;
        assert_eq!(
            store.close("opaque:a").unwrap_err(),
            StoreError::RevisionOverflow
        );
        assert_eq!(
            store.change("opaque:a", 2, &[]).unwrap_err(),
            StoreError::RevisionOverflow
        );
        assert_eq!(
            store.open("opaque:b", 1, "").unwrap_err(),
            StoreError::RevisionOverflow
        );
        assert_eq!(store.get("opaque:a").unwrap().version(), 1);
        assert_eq!(store.len(), 1);
    }
}
