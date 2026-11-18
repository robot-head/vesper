//! Export ABI v3 (`vesper.native.export-abi/3`): Rust storage in the Vesper
//! layout, lent to Vesper in place.
//!
//! A v3 binding declares a `#[repr(C)]` *mirror* (`repr::<Type>`) for every
//! copy struct of the library. A mirror's fields are eight-byte words in the
//! Vesper field order, so an `in`/`inout` parameter of that type, or a
//! borrowed `Slice[T]` of it, crosses as a pointer to the caller's own
//! storage: no word conversion and no scratch copy. The binding checks the
//! layout at compile time with `size_of`/`align_of`/`offset_of` asserts.
//!
//! Field cells:
//!
//! * `i64`, `u64`, `isize`, `usize` and `f64` fields are the plain Rust type:
//!   Vesper stores them as the full word, and every bit pattern is valid.
//! * [`Word<T>`] holds a narrower integer, a `bool`, `Unit` or a field-less
//!   enum in its canonical word (sign- or zero-extended, `0`/`1`, the tag).
//!   Vesper code relies on that form, so the word is private and only
//!   written through [`Word::new`]/[`Word::set`]; reading converts.
//! * [`Words<T, N>`] holds any other copy value (an `Option`, a `Result`, a
//!   tuple, an enum with payloads) as its `N` words, converted with [`Abi`]
//!   only when the field is read or written.
//! * A nested copy struct is its own mirror.
//!
//! The binding asserts every offset, so a mirror that does not match the
//! Vesper layout does not compile:
//!
//! ```
//! use vesper_port_abi::{Mirror, Word};
//! #[repr(C)]
//! #[derive(Clone, Copy)]
//! pub struct Sample {
//!     pub at: u64,
//!     pub level: Word<u8>,
//! }
//! // SAFETY: two words, asserted below.
//! unsafe impl Mirror for Sample {
//!     const WORDS: usize = 2;
//! }
//! const _: () = {
//!     assert!(core::mem::size_of::<Sample>() == 2 * 8);
//!     assert!(core::mem::align_of::<Sample>() == 8);
//!     assert!(core::mem::offset_of!(Sample, at) == 0 * 8);
//!     assert!(core::mem::offset_of!(Sample, level) == 1 * 8);
//! };
//! ```
//!
//! ```compile_fail
//! use vesper_port_abi::Word;
//! #[repr(C)]
//! #[derive(Clone, Copy)]
//! pub struct Sample {
//!     pub level: Word<u8>, // Vesper puts `at` first
//!     pub at: u64,
//! }
//! const _: () = assert!(core::mem::offset_of!(Sample, at) == 0 * 8);
//! ```
//!
//! ```compile_fail
//! #[repr(C)]
//! #[derive(Clone, Copy)]
//! pub struct Sample {
//!     pub at: u64, // the `level` word is missing
//! }
//! const _: () = assert!(core::mem::size_of::<Sample>() == 2 * 8);
//! ```
//!
//! An `inout` mirror and a borrowed slice of the same storage cannot be
//! passed to one call: the wrapper takes `&mut T` and `&[T]`, and the borrow
//! checker refuses the alias.
//!
//! ```compile_fail
//! # #[derive(Clone, Copy)] pub struct Sample { pub at: u64 }
//! fn step(state: &mut Sample, history: &[Sample]) {}
//! let mut all = [Sample { at: 1 }, Sample { at: 2 }];
//! step(&mut all[0], &all);
//! ```
use super::*;
use core::fmt;

/// Rust storage whose layout is the Vesper word layout of a copy struct.
///
/// # Safety
/// The implementor must be `#[repr(C)]`, exactly `WORDS * 8` bytes, eight-
/// byte aligned, with no padding, and every one of its fields a plain word
/// (`i64`, `u64`, `isize`, `usize`, `f64`), a [`Word`], a [`Words`] or
/// another `Mirror`. Generated bindings implement it and assert the layout
/// at compile time.
pub unsafe trait Mirror: Copy + 'static {
    /// Size in eight-byte words.
    const WORDS: usize;
}

/// The words of a mirror, read in place.
pub fn mirror_words<T: Mirror>(value: &T) -> &[u64] {
    assert_eq!(core::mem::size_of::<T>(), T::WORDS * 8);
    // SAFETY: a `Mirror` is `WORDS` initialized, aligned words (the trait's
    // contract), and the borrow keeps it alive.
    unsafe { core::slice::from_raw_parts((value as *const T).cast::<u64>(), T::WORDS) }
}

/// A mirror built from canonical words, such as those [`Abi::put`] writes.
///
/// The words must be the Vesper form of a value (canonical [`Word`]s), which
/// is what every generated `Abi` implementation writes; other bits are not
/// unsafe for Rust, but Vesper code reading them computes garbage.
pub fn mirror_from_words<T: Mirror>(words: &[u64]) -> T {
    assert!(words.len() >= T::WORDS);
    assert_eq!(core::mem::size_of::<T>(), T::WORDS * 8);
    // SAFETY: a `Mirror` is plain words (the trait's contract), so any
    // `WORDS` words are a value of it; `read_unaligned` needs no alignment.
    unsafe { words.as_ptr().cast::<T>().read_unaligned() }
}

/// One word holding a narrow copy value in its canonical Vesper form.
#[repr(transparent)]
pub struct Word<T> {
    word: u64,
    _value: PhantomData<fn() -> T>,
}

impl<T: Abi> Word<T> {
    /// The canonical word of `value`.
    pub fn new(value: T) -> Self {
        const { assert!(T::WORDS == 1, "a Word holds a one-word value") };
        let mut word = [0u64];
        value.put(&mut word);
        Self {
            word: word[0],
            _value: PhantomData,
        }
    }

    /// The value.
    pub fn get(&self) -> T {
        T::get(&[self.word])
    }

    /// Replace the value.
    pub fn set(&mut self, value: T) {
        *self = Self::new(value);
    }
}

impl<T> Word<T> {
    /// The raw word.
    pub const fn bits(&self) -> u64 {
        self.word
    }
}

impl<T: Abi + Default> Default for Word<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Clone for Word<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Word<T> {}

/// Canonical words are equal exactly when the values are.
impl<T> PartialEq for Word<T> {
    fn eq(&self, other: &Self) -> bool {
        self.word == other.word
    }
}

impl<T> Eq for Word<T> {}

impl<T> fmt::Debug for Word<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Word({:#x})", self.word)
    }
}

/// `N` words holding a copy value in its Vesper layout, converted on access.
#[repr(transparent)]
pub struct Words<T, const N: usize> {
    words: [u64; N],
    _value: PhantomData<fn() -> T>,
}

impl<T: Abi, const N: usize> Words<T, N> {
    /// The words of `value`.
    pub fn new(value: T) -> Self {
        const { assert!(T::WORDS == N, "Words<T, N> needs N == T::WORDS") };
        let mut words = [0u64; N];
        value.put(&mut words);
        Self {
            words,
            _value: PhantomData,
        }
    }

    /// The value.
    pub fn get(&self) -> T {
        T::get(&self.words)
    }

    /// Replace the value.
    pub fn set(&mut self, value: T) {
        *self = Self::new(value);
    }
}

impl<T, const N: usize> Words<T, N> {
    /// The raw words.
    pub const fn bits(&self) -> &[u64; N] {
        &self.words
    }
}

impl<T: Abi + Default, const N: usize> Default for Words<T, N> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T, const N: usize> Clone for Words<T, N> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, const N: usize> Copy for Words<T, N> {}

impl<T, const N: usize> PartialEq for Words<T, N> {
    fn eq(&self, other: &Self) -> bool {
        self.words == other.words
    }
}

impl<T, const N: usize> Eq for Words<T, N> {}

impl<T, const N: usize> fmt::Debug for Words<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Words({:x?})", self.words)
    }
}

// A full word, a `Word` and `Words` are plain words: any bits are a value.
macro_rules! word_mirrors {
    ($($t:ty),*) => {$(
        // SAFETY: one eight-byte, eight-byte-aligned word; any bits are valid.
        unsafe impl Mirror for $t {
            const WORDS: usize = 1;
        }
    )*};
}
word_mirrors!(u64, i64, usize, isize, f64);

// SAFETY: `repr(transparent)` over one `u64`.
unsafe impl<T: 'static> Mirror for Word<T> {
    const WORDS: usize = 1;
}

// SAFETY: `repr(transparent)` over `[u64; N]`.
unsafe impl<T: 'static, const N: usize> Mirror for Words<T, N> {
    const WORDS: usize = N;
}

/// The mirror of a Vesper `Option[T]` (#991): the tag word (`0` is `None`,
/// `1` is `Some`) and the payload cell, which is `T`'s mirror or word cell.
/// A v3 library built with `// @export abi 3 options` lends `in`/`inout`
/// `Option` parameters as this mirror and writes `Option` results into one,
/// with no conversion.
///
/// The payload of a `None` is unspecified words (zero when Rust built it).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OptionMirror<T: Mirror> {
    tag: u64,
    value: T,
}

// SAFETY: `repr(C)` of a word and a mirror, so `1 + T::WORDS` words with no
// padding, eight-byte aligned, and any bits are a value.
unsafe impl<T: Mirror> Mirror for OptionMirror<T> {
    const WORDS: usize = 1 + T::WORDS;
}

/// All-zero words of a mirror.
fn zeroed<T: Mirror>() -> T {
    // SAFETY: a `Mirror` is plain words (the trait's contract), so the zero
    // bit pattern is a value of it.
    unsafe { core::mem::zeroed() }
}

impl<T: Mirror> OptionMirror<T> {
    /// `None`.
    pub fn none() -> Self {
        Self {
            tag: 0,
            value: zeroed(),
        }
    }

    /// `Some(value)`.
    pub fn some(value: T) -> Self {
        Self { tag: 1, value }
    }

    /// The tag word as Vesper wrote it.
    pub const fn tag(&self) -> u64 {
        self.tag
    }

    pub fn is_some(&self) -> bool {
        self.as_ref().is_some()
    }

    pub fn is_none(&self) -> bool {
        !self.is_some()
    }

    /// The payload, borrowed in place.
    ///
    /// # Panics
    /// When the tag is neither 0 nor 1 (not a value Vesper writes).
    pub fn as_ref(&self) -> Option<&T> {
        match self.tag {
            0 => None,
            1 => Some(&self.value),
            tag => panic!("invalid Option tag {tag}"),
        }
    }

    /// The payload, mutably borrowed in place.
    pub fn as_mut(&mut self) -> Option<&mut T> {
        match self.tag {
            0 => None,
            1 => Some(&mut self.value),
            tag => panic!("invalid Option tag {tag}"),
        }
    }

    /// The value as a Rust `Option`.
    pub fn get(&self) -> Option<T> {
        self.as_ref().copied()
    }

    /// Replace the value.
    pub fn set(&mut self, value: Option<T>) {
        *self = value.into();
    }
}

impl<T: Mirror> From<Option<T>> for OptionMirror<T> {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(v) => Self::some(v),
            None => Self::none(),
        }
    }
}

impl<T: Mirror> From<OptionMirror<T>> for Option<T> {
    fn from(value: OptionMirror<T>) -> Self {
        value.get()
    }
}

impl<T: Mirror> Default for OptionMirror<T> {
    fn default() -> Self {
        Self::none()
    }
}

impl<T: Mirror + PartialEq> PartialEq for OptionMirror<T> {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}

impl<T: Mirror + fmt::Debug> fmt::Debug for OptionMirror<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.as_ref(), f)
    }
}

/// The payload words of a [`ResultMirror`]: either cell, as long as the
/// longer one.
#[repr(C)]
#[derive(Clone, Copy)]
union Payload<T: Copy, E: Copy> {
    ok: T,
    err: E,
}

/// The mirror of a Vesper `Result[T, E]` (#991): the tag word (`0` is `Ok`,
/// `1` is `Err`) and the payload, `1 + max(T::WORDS, E::WORDS)` words in
/// all. Both cells start right after the tag, as in the Vesper layout.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ResultMirror<T: Mirror, E: Mirror> {
    tag: u64,
    payload: Payload<T, E>,
}

// SAFETY: `repr(C)` of a word and a `repr(C)` union of two mirrors, so
// `1 + max` words (both are whole words, eight-byte aligned) with no
// padding. Rust-built values are zeroed before a cell is written, so every
// word is initialized, and any bits are a value of either cell.
unsafe impl<T: Mirror, E: Mirror> Mirror for ResultMirror<T, E> {
    const WORDS: usize = 1 + if T::WORDS > E::WORDS {
        T::WORDS
    } else {
        E::WORDS
    };
}

impl<T: Mirror, E: Mirror> ResultMirror<T, E> {
    /// All words zero, then `tag` and one cell written.
    fn zeroed_with(tag: u64) -> Self {
        let mut out: Self = zeroed();
        out.tag = tag;
        out
    }

    /// `Ok(value)`.
    pub fn ok(value: T) -> Self {
        let mut out = Self::zeroed_with(0);
        out.payload.ok = value;
        out
    }

    /// `Err(error)`.
    pub fn err(error: E) -> Self {
        let mut out = Self::zeroed_with(1);
        out.payload.err = error;
        out
    }

    /// The tag word as Vesper wrote it.
    pub const fn tag(&self) -> u64 {
        self.tag
    }

    pub fn is_ok(&self) -> bool {
        self.as_ref().is_ok()
    }

    pub fn is_err(&self) -> bool {
        !self.is_ok()
    }

    /// The payload, borrowed in place.
    ///
    /// # Panics
    /// When the tag is neither 0 nor 1 (not a value Vesper writes).
    pub fn as_ref(&self) -> Result<&T, &E> {
        // SAFETY: both cells are plain words that start at the payload, so
        // either reading is a value; the tag says which one Vesper wrote.
        unsafe {
            match self.tag {
                0 => Ok(&self.payload.ok),
                1 => Err(&self.payload.err),
                tag => panic!("invalid Result tag {tag}"),
            }
        }
    }

    /// The value as a Rust `Result`.
    pub fn get(&self) -> Result<T, E> {
        self.as_ref().map(|t| *t).map_err(|e| *e)
    }

    /// Replace the value.
    pub fn set(&mut self, value: Result<T, E>) {
        *self = value.into();
    }
}

impl<T: Mirror, E: Mirror> From<Result<T, E>> for ResultMirror<T, E> {
    fn from(value: Result<T, E>) -> Self {
        match value {
            Ok(v) => Self::ok(v),
            Err(e) => Self::err(e),
        }
    }
}

impl<T: Mirror, E: Mirror> From<ResultMirror<T, E>> for Result<T, E> {
    fn from(value: ResultMirror<T, E>) -> Self {
        value.get()
    }
}

impl<T: Mirror + PartialEq, E: Mirror + PartialEq> PartialEq for ResultMirror<T, E> {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}

impl<T: Mirror + fmt::Debug, E: Mirror + fmt::Debug> fmt::Debug for ResultMirror<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.as_ref(), f)
    }
}

/// Lend `items` to Vesper as an `in Slice[T]` view, in place.
pub fn with_mirror_slice<T: Mirror, R>(items: &[T], call: impl FnOnce(*mut u64) -> R) -> R {
    let len = items.len() as u64;
    // Vesper only reads an `in` slice; the header and view are scratch.
    let mut header = [len, len, items.as_ptr() as usize as u64];
    let mut view = [header.as_mut_ptr() as usize as u64, 0, len];
    call(view.as_mut_ptr())
}

/// Lend `items` to Vesper as an `inout Slice[T]` view, in place.
pub fn with_mirror_slice_mut<T: Mirror, R>(items: &mut [T], call: impl FnOnce(*mut u64) -> R) -> R {
    let len = items.len() as u64;
    let mut header = [len, len, items.as_mut_ptr() as usize as u64];
    let mut view = [header.as_mut_ptr() as usize as u64, 0, len];
    call(view.as_mut_ptr())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Probe {
        a: u64,
        b: Word<i8>,
        c: Words<Option<u32>, 2>,
    }
    // SAFETY: four words, `repr(C)`, word fields only (asserted below).
    unsafe impl Mirror for Probe {
        const WORDS: usize = 4;
    }
    const _: () = assert!(core::mem::size_of::<Probe>() == 32);
    const _: () = assert!(core::mem::offset_of!(Probe, c) == 16);

    #[test]
    fn words_are_canonical() {
        assert_eq!(Word::new(-1i8).bits(), u64::MAX);
        assert_eq!(Word::new(200u8).bits(), 200);
        assert_eq!(Word::new(true).bits(), 1);
        assert_eq!(Word::new(i32::MIN).get(), i32::MIN);
        let mut w = Word::new(7u16);
        w.set(u16::MAX);
        assert_eq!((w.get(), w.bits()), (u16::MAX, 0xffff));
        let some = Words::<Option<u32>, 2>::new(Some(9));
        assert_eq!(some.bits(), &[1, 9]);
        assert_eq!(some.get(), Some(9));
    }

    #[test]
    fn mirrors_round_trip_through_words() {
        let p = Probe {
            a: u64::MAX,
            b: Word::new(-3),
            c: Words::new(None),
        };
        let words = mirror_words(&p);
        assert_eq!(words, &[u64::MAX, (-3i64) as u64, 0, 0]);
        assert_eq!(mirror_from_words::<Probe>(words), p);
    }

    #[test]
    fn option_and_result_mirrors_have_the_vesper_layout() {
        type O = OptionMirror<Probe>;
        assert_eq!(core::mem::size_of::<O>(), O::WORDS * 8);
        assert_eq!(O::WORDS, 5);
        let some = O::some(Probe {
            a: 7,
            b: Word::new(-1),
            c: Words::new(Some(3)),
        });
        assert_eq!(mirror_words(&some), &[1, 7, u64::MAX, 1, 3]);
        assert_eq!(mirror_words(&O::none()), &[0, 0, 0, 0, 0]);
        assert_eq!(mirror_from_words::<O>(&[1, 7, u64::MAX, 1, 3]), some);
        assert_eq!(some.get().map(|p| p.a), Some(7));
        assert!(O::default().is_none());
        let narrow: OptionMirror<Word<u8>> = Some(Word::new(200)).into();
        assert_eq!(mirror_words(&narrow), &[1, 200]);

        type R = ResultMirror<u64, Probe>;
        assert_eq!(core::mem::size_of::<R>(), R::WORDS * 8);
        assert_eq!(R::WORDS, 5);
        let ok = R::ok(9);
        assert_eq!(mirror_words(&ok), &[0, 9, 0, 0, 0]);
        assert_eq!(ok.get().ok(), Some(9));
        let err = R::err(Probe {
            a: 1,
            b: Word::new(2),
            c: Words::new(None),
        });
        assert_eq!(mirror_words(&err), &[1, 1, 2, 0, 0]);
        assert!(err.is_err());
        let longer_ok = ResultMirror::<Probe, Word<bool>>::err(Word::new(true));
        assert_eq!(mirror_words(&longer_ok), &[1, 1, 0, 0, 0]);
    }

    #[test]
    #[should_panic(expected = "invalid Option tag 2")]
    fn invalid_tags_are_refused() {
        let _ = mirror_from_words::<OptionMirror<u64>>(&[2, 0]).get();
    }
}
