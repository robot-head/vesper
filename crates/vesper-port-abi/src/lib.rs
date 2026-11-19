//! Runtime support for Rust bindings of `vesper build --lib` libraries.
//!
//! The export ABI (`vesper.native.export-abi/1`, see `src/native_export.rs`
//! in the compiler) passes every value as eight-byte words: a scalar is one
//! word, a tuple or struct is its fields in order, and an enum is a tag word
//! (the variant index) followed by the variant's fields. Every value is at
//! least one word. [`Abi`] converts between Rust values and those words.
//! Generated bindings implement it for the library's own types; this crate
//! implements it for scalars, tuples, `Option`, `Result` and the Vesper
//! builtin error types.
//!
//! Export ABI v2 (`vesper.native.export-abi/2`) adds sequences, slices and
//! owned handles. Their runtime pieces are here too: [`with_slice`] and
//! [`with_slice_mut`] lend a Rust slice to Vesper as a `Slice[T]` view,
//! [`with_seq_mut`] lends a `Vec<T>` as an `inout ChunkSeq[T]`,
//! [`seq_take`] copies a returned `ChunkSeq[T]` into a `Vec<T>` and frees it,
//! and [`SeqRef`] reads a sequence a handle owns without copying it.
//! Strings cross as the three-word value `{len, cap, data}`: [`with_str`]
//! lends a `&str`, [`with_string_mut`] lends a `&mut String`, [`string_take`]
//! copies a returned `String` and frees it, and [`str_view`] borrows a
//! string a handle owns.
//!
//! Export ABI v3 (`vesper.native.export-abi/3`) lends Rust storage to Vesper
//! in place: a [`Mirror`] is a `#[repr(C)]` struct in the Vesper word layout
//! (cells [`Word`] and [`Words`]), passed by pointer for `in`/`inout`
//! parameters, and [`with_mirror_slice`]/[`with_mirror_slice_mut`] lend a
//! slice of mirrors without copying it.
//!
//! A handle may own values Rust cannot read: a sequence of other handles or
//! of boxed trait objects. Its view hands them out as [`OpaqueSeq`] and
//! [`OpaqueObject`], which say only how many there are and whether one is
//! present; dropping the handle frees them.
//!
//! This crate has no dependencies, so a crate with a `vesper-port` feature
//! can depend on it unconditionally.
#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;

mod mirror;
mod seq;
pub use mirror::*;
pub use seq::*;

/// A value with a fixed word layout at the export boundary.
pub trait Abi: Sized {
    /// Size in eight-byte words (at least 1).
    const WORDS: usize;
    /// Write the value's words to `out[..Self::WORDS]`.
    fn put(&self, out: &mut [u64]);
    /// Read a value from `words[..Self::WORDS]`.
    fn get(words: &[u64]) -> Self;
    /// Bytes per element inside a Vesper sequence buffer
    /// (`vesper::seq::elem_bytes`): 1 for `u8` and `bool`, 4 for `u32`, and
    /// the word size of the value for every other type. The sequence helpers
    /// check it against [`Abi::WORDS`] before any raw access (a stride of 1
    /// or 4 needs one word, any other stride is `WORDS * 8`) and panic on a
    /// mismatch, so a wrong implementation cannot reach out of bounds.
    const STRIDE: usize = Self::WORDS * 8;
}

/// `max(a, b)` in constant context.
pub const fn max(a: usize, b: usize) -> usize {
    if a > b { a } else { b }
}

macro_rules! integer {
    ($($t:ty => $stride:expr);*) => {$(
        impl Abi for $t {
            const WORDS: usize = 1;
            const STRIDE: usize = $stride;
            fn put(&self, out: &mut [u64]) {
                out[0] = *self as u64;
            }
            fn get(words: &[u64]) -> Self {
                words[0] as $t
            }
        }
    )*};
}
// Signed kinds are sign-extended into the word (`as u64` from a signed
// type does that), unsigned ones zero-extended; `usize`/`isize` are 64-bit
// on every target the export ABI supports.
integer!(
    u8 => 1; u16 => 8; u32 => 4; u64 => 8; usize => 8;
    i8 => 8; i16 => 8; i32 => 8; i64 => 8; isize => 8
);

impl Abi for bool {
    const WORDS: usize = 1;
    const STRIDE: usize = 1;
    fn put(&self, out: &mut [u64]) {
        out[0] = u64::from(*self);
    }
    fn get(words: &[u64]) -> Self {
        words[0] != 0
    }
}

impl Abi for f64 {
    const WORDS: usize = 1;
    fn put(&self, out: &mut [u64]) {
        out[0] = self.to_bits();
    }
    fn get(words: &[u64]) -> Self {
        f64::from_bits(words[0])
    }
}

/// An `f32` is the low half of its word (the high half is unspecified, so
/// `get` ignores it), and takes a whole word in a sequence buffer
/// (`vesper::seq::elem_bytes`): a `[f32]` is copied at the boundary, not
/// lent in place.
impl Abi for f32 {
    const WORDS: usize = 1;
    fn put(&self, out: &mut [u64]) {
        out[0] = u64::from(self.to_bits());
    }
    fn get(words: &[u64]) -> Self {
        f32::from_bits(words[0] as u32)
    }
}

/// `Unit` is one ignored word.
impl Abi for () {
    const WORDS: usize = 1;
    fn put(&self, out: &mut [u64]) {
        out[0] = 0;
    }
    fn get(_: &[u64]) -> Self {}
}

macro_rules! tuple {
    ($($name:ident $index:tt),+) => {
        impl<$($name: Abi),+> Abi for ($($name,)+) {
            const WORDS: usize = max(1, 0 $(+ $name::WORDS)+);
            #[allow(unused_assignments)]
            fn put(&self, out: &mut [u64]) {
                let mut at = 0;
                $(
                    self.$index.put(&mut out[at..]);
                    at += $name::WORDS;
                )+
            }
            #[allow(unused_assignments)]
            fn get(words: &[u64]) -> Self {
                let mut at = 0;
                ($(
                    {
                        let value = $name::get(&words[at..]);
                        at += $name::WORDS;
                        value
                    },
                )+)
            }
        }
    };
}
tuple!(A 0);
tuple!(A 0, B 1);
tuple!(A 0, B 1, C 2);
tuple!(A 0, B 1, C 2, D 3);
tuple!(A 0, B 1, C 2, D 3, E 4);
tuple!(A 0, B 1, C 2, D 3, E 4, F 5);
tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6);
tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);

/// Vesper `copy enum Option[T] { None, Some(T) }`.
impl<T: Abi> Abi for Option<T> {
    const WORDS: usize = 1 + T::WORDS;
    fn put(&self, out: &mut [u64]) {
        match self {
            None => out[0] = 0,
            Some(value) => {
                out[0] = 1;
                value.put(&mut out[1..]);
            }
        }
    }
    fn get(words: &[u64]) -> Self {
        match words[0] {
            0 => None,
            1 => Some(T::get(&words[1..])),
            tag => panic!("invalid Option tag {tag} from Vesper"),
        }
    }
}

/// Vesper `copy enum Result[T, E] { Ok(T), Err(E) }`.
impl<T: Abi, E: Abi> Abi for Result<T, E> {
    const WORDS: usize = 1 + max(T::WORDS, E::WORDS);
    fn put(&self, out: &mut [u64]) {
        match self {
            Ok(value) => {
                out[0] = 0;
                value.put(&mut out[1..]);
            }
            Err(error) => {
                out[0] = 1;
                error.put(&mut out[1..]);
            }
        }
    }
    fn get(words: &[u64]) -> Self {
        match words[0] {
            0 => Ok(T::get(&words[1..])),
            1 => Err(E::get(&words[1..])),
            tag => panic!("invalid Result tag {tag} from Vesper"),
        }
    }
}

/// Vesper's builtin `copy struct ArithOverflow {}`, raised by checked
/// arithmetic (`vesper.arith.checked/1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ArithOverflow;

impl Abi for ArithOverflow {
    const WORDS: usize = 1;
    fn put(&self, out: &mut [u64]) {
        out[0] = 0;
    }
    fn get(_: &[u64]) -> Self {
        ArithOverflow
    }
}

impl core::fmt::Display for ArithOverflow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("arithmetic overflow")
    }
}

/// Vesper's builtin `copy enum RuntimeError { DivisionByZero, IndexOutOfBounds }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeError {
    DivisionByZero,
    IndexOutOfBounds,
}

impl Abi for RuntimeError {
    const WORDS: usize = 1;
    fn put(&self, out: &mut [u64]) {
        out[0] = match self {
            RuntimeError::DivisionByZero => 0,
            RuntimeError::IndexOutOfBounds => 1,
        };
    }
    fn get(words: &[u64]) -> Self {
        match words[0] {
            0 => RuntimeError::DivisionByZero,
            1 => RuntimeError::IndexOutOfBounds,
            tag => panic!("invalid RuntimeError tag {tag} from Vesper"),
        }
    }
}

impl core::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            RuntimeError::DivisionByZero => "division by zero",
            RuntimeError::IndexOutOfBounds => "index out of bounds",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Abi + PartialEq + core::fmt::Debug>(value: T) {
        let mut words = [0u64; 16];
        value.put(&mut words);
        assert_eq!(T::get(&words), value);
    }

    #[test]
    fn layouts_match_the_compiler() {
        // Sizes from src/layout.rs: scalars are one word, products sum their
        // fields (at least one word), enums add a tag word.
        assert_eq!(<(i64, bool) as Abi>::WORDS, 2);
        assert_eq!(<Option<u32> as Abi>::WORDS, 2);
        assert_eq!(<Result<(i64, i64), ArithOverflow> as Abi>::WORDS, 3);
        assert_eq!(<Option<Option<u8>> as Abi>::WORDS, 3);
        round_trip((-5i64, true, 7u8));
        round_trip(Some((1u64, -2.5f64)));
        round_trip(Err::<u64, RuntimeError>(RuntimeError::IndexOutOfBounds));
        round_trip(Ok::<u64, RuntimeError>(u64::MAX));
        round_trip((i8::MIN, -1i16, i32::MIN, isize::MAX, u16::MAX, usize::MAX));
    }

    #[test]
    fn f32_is_the_low_half_of_its_word() {
        round_trip((1.5f32, -0.0f32, f32::MIN_POSITIVE));
        let mut words = [u64::MAX; 1];
        2.5f32.put(&mut words);
        assert_eq!(words[0], u64::from(2.5f32.to_bits()));
        // Vesper leaves the high half unspecified.
        assert_eq!(
            f32::get(&[0xdead_beef_0000_0000 | u64::from(3.0f32.to_bits())]),
            3.0
        );
        assert_eq!(<f32 as Abi>::STRIDE, 8);
    }

    #[test]
    fn signed_words_are_sign_extended() {
        let mut words = [0u64; 1];
        (-1i8).put(&mut words);
        assert_eq!(words[0], u64::MAX);
        200u8.put(&mut words);
        assert_eq!(words[0], 200);
    }

    #[test]
    fn option_tags_follow_the_builtin_declaration() {
        let mut words = [9u64; 2];
        Some(3u8).put(&mut words);
        assert_eq!(words, [1, 3]);
        None::<u8>.put(&mut words);
        assert_eq!(words[0], 0);
    }
}
