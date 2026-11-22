//! `.vsys` callback handles at the export boundary (#1502).

use crate::{Abi, Mirror};

/// A Vesper `.vsys` `callback` value: a C function pointer that the Vesper
/// library resolved (with `dlsym` or `vkGetDeviceProcAddr`, say) and keeps
/// inside one of its copy values.
///
/// `ID` names the callback type (the binding passes a hash of its name), so
/// a pointer of one callback type is not a pointer of another.
///
/// Rust can hold one, compare it and hand it back to the library, but
/// cannot call it, and its field is private. The word conversions the
/// generated bindings use ([`Abi::get`], [`crate::mirror_from_words`]) can
/// still make one from any word, so every binding wrapper that takes a
/// value holding a `FnPtr` is an `unsafe fn`: its caller promises that each
/// pointer is one the library returned, unchanged, and that the code it
/// points to stays valid (its library stays loaded) for every call Vesper
/// makes through it, including after the wrapper returns if the library
/// keeps the pointer.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FnPtr<const ID: u64>(u64);

impl<const ID: u64> FnPtr<ID> {
    /// The address, for logging or comparison. The handle stays usable.
    pub fn addr(self) -> u64 {
        self.0
    }
}

impl<const ID: u64> core::fmt::Debug for FnPtr<ID> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "FnPtr({:#x})", self.0)
    }
}

impl<const ID: u64> Abi for FnPtr<ID> {
    const WORDS: usize = 1;
    fn put(&self, out: &mut [u64]) {
        out[0] = self.0;
    }
    fn get(words: &[u64]) -> Self {
        Self(words[0])
    }
}

// SAFETY: `repr(transparent)` over one `u64`; the field is an address word.
unsafe impl<const ID: u64> Mirror for FnPtr<ID> {
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
        let f = FnPtr::<7>::get(&[0x1234]);
        f.put(&mut w);
        assert_eq!(w, [0x1234]);
        assert_eq!(f.addr(), 0x1234);
        assert_eq!(format!("{f:?}"), "FnPtr(0x1234)");
        let m: FnPtr<7> = crate::mirror_from_words(&[7]);
        assert_eq!(crate::mirror_words(&m), &[7]);
    }
}
