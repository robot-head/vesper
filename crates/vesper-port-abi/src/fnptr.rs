//! `.vsys` callback handles at the export boundary (#1502).

use crate::{Abi, Mirror};

/// A Vesper `.vsys` `callback` value: a C function pointer that the Vesper
/// library resolved (with `dlsym` or `vkGetDeviceProcAddr`, say) and keeps
/// inside one of its copy values.
///
/// Rust can hold one, compare it and hand it back to the library, but has
/// no safe way to call it or to make one from an address: the field is
/// private. The only constructors are the word conversions the generated
/// bindings use ([`Abi::get`], [`crate::mirror_from_words`]). Rust code that
/// calls them itself vouches that the word came from the library.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FnPtr(u64);

impl FnPtr {
    /// The address, for logging or comparison. The handle stays usable.
    pub fn addr(self) -> u64 {
        self.0
    }
}

impl core::fmt::Debug for FnPtr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "FnPtr({:#x})", self.0)
    }
}

impl Abi for FnPtr {
    const WORDS: usize = 1;
    fn put(&self, out: &mut [u64]) {
        out[0] = self.0;
    }
    fn get(words: &[u64]) -> Self {
        Self(words[0])
    }
}

// SAFETY: `repr(transparent)` over one `u64`; the field is an address word.
unsafe impl Mirror for FnPtr {
    const WORDS: usize = 1;
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::format;

    #[test]
    fn a_callback_word_round_trips() {
        let mut w = [0u64];
        let f = FnPtr::get(&[0x1234]);
        f.put(&mut w);
        assert_eq!(w, [0x1234]);
        assert_eq!(f.addr(), 0x1234);
        assert_eq!(format!("{f:?}"), "FnPtr(0x1234)");
        let m: FnPtr = crate::mirror_from_words(&[7]);
        assert_eq!(crate::mirror_words(&m), &[7]);
    }
}
