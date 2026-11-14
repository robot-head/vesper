//! Read-only memory-mapped weight files and the byte storage behind
//! [`crate::quant::Matrix`].
//!
//! [`Mapping::open`] maps a whole file read-only (`memmap2`, a private
//! read-only mapping). Matrices loaded from a mapping borrow ranges of it
//! through [`WeightBytes::Mapped`] without copying; every element is read
//! through bounds-checked slices and `from_le_bytes`, never by casting the
//! bytes to wider types.
//!
//! The one `unsafe` call is `memmap2::Mmap::map`. Its contract is that the
//! file is not truncated or modified while mapped; that cannot be enforced by
//! this process, so callers must only map weight files they control (a
//! concurrently truncated file can fault on access). [`Mapping::read`] loads
//! the same bytes into memory instead, with identical parsing.
#![allow(unsafe_code)]

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use crate::Error;

/// Largest weight file accepted by [`Mapping::open`] and [`Mapping::read`].
pub const MAX_WEIGHT_FILE_BYTES: u64 = 64 << 30;

#[derive(Debug)]
enum Backing {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

/// The bytes of one weight file, mapped or read.
#[derive(Debug)]
pub struct Mapping {
    backing: Backing,
}
impl Mapping {
    /// Maps `path` read-only.
    pub fn open(path: &Path) -> Result<Arc<Self>, Error> {
        let file = File::open(path).map_err(|_| Error::Io("open weight file"))?;
        let len = file
            .metadata()
            .map_err(|_| Error::Io("stat weight file"))?
            .len();
        if len > MAX_WEIGHT_FILE_BYTES {
            return Err(Error::Limit("weight file bytes"));
        }
        if len == 0 {
            // Zero-length mappings are not portable; parsing an empty input
            // reports the truncation as usual.
            return Ok(Arc::new(Self {
                backing: Backing::Owned(Vec::new()),
            }));
        }
        // SAFETY: a read-only private mapping of a file we just opened. The
        // documented contract (no concurrent truncation or modification) is
        // the caller's; all access goes through bounds-checked slices.
        let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|_| Error::Io("map weight file"))?;
        Ok(Arc::new(Self {
            backing: Backing::Mapped(map),
        }))
    }
    /// Reads `path` into memory (the non-mapped path, same parsing).
    pub fn read(path: &Path) -> Result<Arc<Self>, Error> {
        let len = std::fs::metadata(path)
            .map_err(|_| Error::Io("stat weight file"))?
            .len();
        if len > MAX_WEIGHT_FILE_BYTES {
            return Err(Error::Limit("weight file bytes"));
        }
        let bytes = std::fs::read(path).map_err(|_| Error::Io("read weight file"))?;
        Ok(Self::from_vec(bytes))
    }
    /// Wraps bytes already in memory.
    pub fn from_vec(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            backing: Backing::Owned(bytes),
        })
    }
    pub fn bytes(&self) -> &[u8] {
        match &self.backing {
            Backing::Mapped(m) => m,
            Backing::Owned(v) => v,
        }
    }
    pub fn len(&self) -> usize {
        self.bytes().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Whether the bytes are an operating-system file mapping.
    pub fn is_mapped(&self) -> bool {
        matches!(self.backing, Backing::Mapped(_))
    }
}

/// Weight payload bytes: owned, or a range of a shared [`Mapping`].
#[derive(Clone, Debug)]
pub enum WeightBytes {
    Owned(Vec<u8>),
    Mapped {
        mapping: Arc<Mapping>,
        offset: usize,
        len: usize,
    },
}
impl WeightBytes {
    /// A checked range of `mapping`.
    pub fn range(mapping: &Arc<Mapping>, offset: usize, len: usize) -> Result<Self, Error> {
        let end = offset.checked_add(len).ok_or(Error::Overflow)?;
        if end > mapping.len() {
            return Err(Error::Malformed("tensor data outside the file"));
        }
        Ok(Self::Mapped {
            mapping: Arc::clone(mapping),
            offset,
            len,
        })
    }
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(v) => v,
            Self::Mapped {
                mapping,
                offset,
                len,
            } => &mapping.bytes()[*offset..*offset + *len],
        }
    }
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Whether these bytes live in an operating-system file mapping.
    pub fn is_file_mapped(&self) -> bool {
        matches!(self, Self::Mapped { mapping, .. } if mapping.is_mapped())
    }
}
