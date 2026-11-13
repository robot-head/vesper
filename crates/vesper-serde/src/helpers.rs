use crate::{Error, ErrorKind, JsonString, add};

/// A direct JSON key matcher for at most seven printable ASCII bytes.
/// Private constants prevent callers from supplying inconsistent masks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SwarFieldMatcher {
    field_idx: u16,
    word: u64,
    mask: u64,
    len: u8,
}
impl SwarFieldMatcher {
    pub fn compile_key(field_idx: u16, key: &str) -> Option<Self> {
        if key.len() > 7
            || !key
                .bytes()
                .all(|b| (0x20..=0x7e).contains(&b) && b != b'"' && b != b'\\')
        {
            return None;
        }
        let mut bytes = [0; 8];
        bytes[..key.len()].copy_from_slice(key.as_bytes());
        bytes[key.len()] = b'"';
        let count = key.len() + 1;
        Some(Self {
            field_idx,
            word: u64::from_le_bytes(bytes),
            mask: u64::MAX >> ((8 - count) * 8),
            len: key.len() as u8,
        })
    }
    pub const fn field_idx(self) -> u16 {
        self.field_idx
    }
    pub const fn key_len(self) -> usize {
        self.len as usize
    }
    /// Checks bytes immediately *after* an opening quote, including the closing
    /// quote. A short slice is safe and returns false; no padded over-read.
    /// This is lexical dispatch only; JSON syntax must be validated separately.
    pub fn matches_window(self, window: &[u8]) -> bool {
        let count = self.key_len() + 1;
        let Some(prefix) = window.get(..count) else {
            return false;
        };
        let mut bytes = [0; 8];
        bytes[..count].copy_from_slice(prefix);
        self.matches_u64_window(u64::from_le_bytes(bytes))
    }
    /// Semantic dispatch for a validated key, with an allocation-free fallback
    /// for escaped-equivalent spellings such as `"id"` and `"\\u0069d"`.
    pub fn matches_key(self, key: JsonString<'_>) -> bool {
        let bytes = self.word.to_le_bytes();
        let name = core::str::from_utf8(&bytes[..self.key_len()]).expect("compiled ASCII key");
        key.equals(name)
    }
    pub const fn matches_u64_window(self, word: u64) -> bool {
        word & self.mask == self.word
    }
}

/// Scalar reference kernel. Carry is boolean: a backslash at byte 63 escapes
/// byte zero of the next block. A bit marks any escaped byte, including a
/// backslash. This deliberately makes no SIMD or throughput claim.
pub fn compute_escaped_mask_64(backslashes: u64, carry_in: bool) -> (u64, bool) {
    let mut escaped = 0;
    let mut carry = carry_in;
    for i in 0..64 {
        if carry {
            escaped |= 1_u64 << i;
        }
        carry = backslashes & (1_u64 << i) != 0 && !carry;
    }
    (escaped, carry)
}

/// Checked schema ceilings. `Bounded(n)` is a total encoded-size ceiling.
/// `Dynamic { static_ceiling }` bounds only the statically bounded contribution
/// after excluding dynamic payloads. It gives neither an upper nor a lower
/// bound on total size: bounded fields may encode shorter than their ceilings.
/// Object callers start at `Bounded(2)` for the opening and closing braces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonSizeBound {
    Bounded(usize),
    Dynamic { static_ceiling: usize },
}
impl JsonSizeBound {
    pub const U8: Self = Self::Bounded(3);
    pub const U32: Self = Self::Bounded(10);
    pub const U64: Self = Self::Bounded(20);
    pub const I64: Self = Self::Bounded(20);
    /// Finite binary64 host encoding ceiling; nonfinite values still reject.
    pub const F64: Self = Self::Bounded(512);
    pub const BOOL: Self = Self::Bounded(5);
    /// Conservative quoted-string ceiling for at most `max_utf8_bytes` input
    /// bytes. Each control byte may need six output bytes (`\\u00xx`).
    pub fn string(max_utf8_bytes: usize) -> Result<Self, Error> {
        let escaped = max_utf8_bytes
            .checked_mul(6)
            .ok_or(Error::new(ErrorKind::Overflow, 0))?;
        Ok(Self::Bounded(add(escaped, 2)?))
    }
    /// `key_escaped_len` excludes the quotes. Arithmetic never wraps.
    pub fn combine_field(
        self,
        key_escaped_len: usize,
        field: Self,
        first: bool,
    ) -> Result<Self, Error> {
        let prefix = add(add(usize::from(!first), key_escaped_len)?, 3)?;
        let (a, dynamic_a) = self.parts();
        let (b, dynamic_b) = field.parts();
        let n = add(add(a, prefix)?, b)?;
        Ok(if dynamic_a || dynamic_b {
            Self::Dynamic { static_ceiling: n }
        } else {
            Self::Bounded(n)
        })
    }
    fn parts(self) -> (usize, bool) {
        match self {
            Self::Bounded(n) => (n, false),
            Self::Dynamic { static_ceiling } => (static_ceiling, true),
        }
    }
}
