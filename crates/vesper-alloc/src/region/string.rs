//! Region-backed UTF-8 strings.
//!
//! [`RegionString`] wraps a private `RegionVec<u8>` whose initialized bytes
//! are always valid UTF-8. Every safe entry point either takes `&str`/`char`
//! (already valid), validates raw bytes, or checks `char` boundaries before
//! shortening. Byte views are shared (`&[u8]`); the only mutable view is
//! `&mut str`, whose safe API cannot break UTF-8.
use super::Region;
use super::vec::{RegionSlice, RegionVec};
use crate::backing::ChunkBacking;
use crate::error::{AllocError, HandleError, RegionError, StringError};
use crate::region::RegionId;

/// Unique owning handle to a region-backed growable UTF-8 string
/// (`RegionString[r]`). Same ownership rules as [`RegionVec`].
///
/// The byte buffer is private, so vector APIs that could write arbitrary
/// bytes do not apply to it:
///
/// ```compile_fail
/// use vesper_alloc::{Region, RegionConfig};
/// let mut r = Region::new(RegionConfig::default()).unwrap();
/// let s = r.string_from_str("ok").unwrap();
/// r.vec_as_mut_slice(&s.buf).unwrap()[0] = 0xff;
/// ```
#[derive(Debug)]
pub struct RegionString {
    buf: RegionVec<u8>,
}

/// Copyable read-only handle to a frozen region string (`RegionStr[r]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionStr {
    buf: RegionSlice<u8>,
}

impl RegionString {
    pub fn region(&self) -> RegionId {
        self.buf.region()
    }
}

impl RegionStr {
    pub fn region(&self) -> RegionId {
        self.buf.region()
    }
}

impl<B: ChunkBacking> Region<B> {
    pub fn string_new(&mut self) -> Result<RegionString, AllocError> {
        self.string_with_capacity(0)
    }

    /// Empty string with room for `cap` bytes.
    pub fn string_with_capacity(&mut self, cap: usize) -> Result<RegionString, AllocError> {
        Ok(RegionString {
            buf: self.vec_with_capacity(cap)?,
        })
    }

    /// Copies `s` into a string of exactly `s.len()` bytes of capacity.
    pub fn string_from_str(&mut self, s: &str) -> Result<RegionString, AllocError> {
        let mut out = self.string_with_capacity(s.len())?;
        self.vec_extend_from_slice(&mut out.buf, s.as_bytes())
            .expect("capacity was reserved");
        Ok(out)
    }

    /// Validates `bytes` as UTF-8 before allocating anything.
    pub fn string_from_utf8(&mut self, bytes: &[u8]) -> Result<RegionString, StringError> {
        let s = std::str::from_utf8(bytes).map_err(StringError::Utf8)?;
        Ok(self.string_from_str(s)?)
    }

    /// Length in bytes.
    pub fn string_len(&self, s: &RegionString) -> Result<usize, HandleError> {
        self.vec_len(&s.buf)
    }

    pub fn string_is_empty(&self, s: &RegionString) -> Result<bool, HandleError> {
        self.vec_is_empty(&s.buf)
    }

    /// Capacity in bytes.
    pub fn string_capacity(&self, s: &RegionString) -> Result<usize, HandleError> {
        self.vec_capacity(&s.buf)
    }

    pub fn string_as_str(&self, s: &RegionString) -> Result<&str, HandleError> {
        let bytes = self.vec_as_slice(&s.buf)?;
        debug_assert!(std::str::from_utf8(bytes).is_ok());
        // SAFETY: every safe mutation keeps the initialized bytes UTF-8.
        Ok(unsafe { std::str::from_utf8_unchecked(bytes) })
    }

    /// Exclusive `&mut str` view. Safe `str` methods preserve UTF-8.
    pub fn string_as_mut_str(&mut self, s: &RegionString) -> Result<&mut str, HandleError> {
        let bytes = self.vec_as_mut_slice(&s.buf)?;
        // SAFETY: as in `string_as_str`; the bytes are only exposed as `str`.
        Ok(unsafe { std::str::from_utf8_unchecked_mut(bytes) })
    }

    /// Read-only byte view. There is deliberately no `&mut [u8]` view.
    pub fn string_as_bytes(&self, s: &RegionString) -> Result<&[u8], HandleError> {
        self.vec_as_slice(&s.buf)
    }

    pub fn string_reserve(
        &mut self,
        s: &mut RegionString,
        additional: usize,
    ) -> Result<(), RegionError> {
        self.vec_reserve(&mut s.buf, additional)
    }

    pub fn string_reserve_exact(
        &mut self,
        s: &mut RegionString,
        additional: usize,
    ) -> Result<(), RegionError> {
        self.vec_reserve_exact(&mut s.buf, additional)
    }

    /// Appends `text`, all or nothing.
    pub fn string_push_str(&mut self, s: &mut RegionString, text: &str) -> Result<(), RegionError> {
        self.vec_extend_from_slice(&mut s.buf, text.as_bytes())
    }

    pub fn string_push(&mut self, s: &mut RegionString, ch: char) -> Result<(), RegionError> {
        let mut utf8 = [0; 4];
        self.string_push_str(s, ch.encode_utf8(&mut utf8))
    }

    /// Validates `bytes` and appends them. Concatenating two valid UTF-8
    /// sequences is valid, so validating the suffix alone suffices.
    pub fn string_push_utf8(
        &mut self,
        s: &mut RegionString,
        bytes: &[u8],
    ) -> Result<(), StringError> {
        let text = std::str::from_utf8(bytes).map_err(StringError::Utf8)?;
        Ok(self.string_push_str(s, text)?)
    }

    /// Removes the last `char`.
    pub fn string_pop(&mut self, s: &mut RegionString) -> Result<Option<char>, HandleError> {
        let Some(ch) = self.string_as_str(s)?.chars().next_back() else {
            return Ok(None);
        };
        let new_len = self.string_len(s)? - ch.len_utf8();
        self.vec_truncate(&s.buf, new_len)
            .expect("bytes are copy and the handle was just resolved");
        Ok(Some(ch))
    }

    /// Shortens to `new_len` bytes, which must be a `char` boundary. A longer
    /// `new_len` is a no-op.
    pub fn string_truncate(&mut self, s: &RegionString, new_len: usize) -> Result<(), StringError> {
        let text = self.string_as_str(s)?;
        if new_len >= text.len() {
            return Ok(());
        }
        if !text.is_char_boundary(new_len) {
            return Err(StringError::NotCharBoundary { index: new_len });
        }
        Ok(self.vec_truncate(&s.buf, new_len)?)
    }

    pub fn string_clear(&mut self, s: &RegionString) -> Result<(), HandleError> {
        self.string_truncate(s, 0).map_err(|e| match e {
            StringError::Region(RegionError::Handle(h)) => h,
            _ => unreachable!("clearing bytes only fails on the handle"),
        })
    }

    /// Releases the string's buffer; every alias becomes stale.
    pub fn string_release(&mut self, s: RegionString) -> Result<(), HandleError> {
        self.vec_release(s.buf).map_err(|e| match e.error {
            RegionError::Handle(h) => h,
            _ => unreachable!("byte vectors are never linear"),
        })
    }

    /// Freezes the string into a copyable read-only handle.
    pub fn string_freeze(&mut self, s: RegionString) -> Result<RegionStr, HandleError> {
        match self.vec_freeze(s.buf) {
            Ok(buf) => Ok(RegionStr { buf }),
            Err(e) => match e.error {
                RegionError::Handle(h) => Err(h),
                _ => unreachable!("byte vectors are never linear"),
            },
        }
    }

    pub fn str_len(&self, s: &RegionStr) -> Result<usize, HandleError> {
        self.slice_len(&s.buf)
    }

    pub fn str_as_str(&self, s: &RegionStr) -> Result<&str, HandleError> {
        let bytes = self.slice(&s.buf)?;
        debug_assert!(std::str::from_utf8(bytes).is_ok());
        // SAFETY: frozen from a `RegionString`, whose bytes are UTF-8.
        Ok(unsafe { std::str::from_utf8_unchecked(bytes) })
    }

    /// Releases a frozen string; every copy becomes stale.
    pub fn str_release(&mut self, s: RegionStr) -> Result<(), HandleError> {
        self.slice_release(s.buf)
    }
}
