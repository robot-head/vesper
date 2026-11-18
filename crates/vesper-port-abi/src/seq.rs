//! Sequences and slices at the export boundary (`vesper.native.export-abi/2`).
//!
//! A Vesper `ChunkSeq[T]` is one word: a pointer to a three-word header
//! `{len, cap, data}` whose `data` holds `len` elements packed at
//! [`Abi::STRIDE`] bytes. A `Slice[T]` is the three-word view
//! `{header, start, len}`. Generated bindings use the functions below; each
//! one documents what it requires of the pointers it is given.
use super::*;

/// The two sequence entry points every v2 library exports
/// (`vesper_<lib>_seq_new` and `vesper_<lib>_seq_free`).
#[derive(Clone, Copy)]
pub struct SeqRuntime {
    /// `seq_new(capacity, stride)` returns a fresh, empty sequence handle.
    pub new: unsafe extern "C" fn(i64, i64) -> i64,
    /// `seq_free(handle)` frees the header and the element buffer.
    pub free: unsafe extern "C" fn(i64) -> i64,
}

/// Whether `T`'s Rust layout is the packed Vesper one, so `[T]` can be lent
/// to Vesper in place. Only the built-in scalars qualify: their sizes match
/// their strides, and every bit pattern Vesper writes is a valid value.
/// (Implementors of the safe [`Abi`] trait cannot opt in.)
fn native<T: Abi + 'static>() -> bool {
    use core::any::TypeId;
    let id = TypeId::of::<T>();
    let builtin = [
        TypeId::of::<u8>(),
        TypeId::of::<u32>(),
        TypeId::of::<u64>(),
        TypeId::of::<i64>(),
        TypeId::of::<usize>(),
        TypeId::of::<isize>(),
        TypeId::of::<f64>(),
    ];
    builtin.contains(&id) && core::mem::size_of::<T>() == T::STRIDE
}

/// Panic unless `T::STRIDE` and `T::WORDS` describe one layout, which every
/// raw access below relies on.
fn check_layout<T: Abi>() {
    let ok = match T::STRIDE {
        1 | 4 => T::WORDS == 1,
        stride => T::WORDS >= 1 && stride == T::WORDS * 8,
    };
    assert!(
        ok,
        "inconsistent Abi layout: STRIDE {} with WORDS {}",
        T::STRIDE,
        T::WORDS
    );
}

/// Read element `i` of a packed buffer.
///
/// # Safety
/// `data` must point to at least `i + 1` initialized elements of stride
/// `T::STRIDE`, eight-byte aligned when the stride is a multiple of eight.
pub unsafe fn read_elem<T: Abi>(data: *const u8, i: usize) -> T {
    check_layout::<T>();
    // SAFETY: the caller guarantees the element is in bounds and aligned.
    unsafe {
        let at = data.add(i * T::STRIDE);
        match T::STRIDE {
            1 => T::get(&[u64::from(*at)]),
            4 => T::get(&[u64::from(at.cast::<u32>().read_unaligned())]),
            _ => T::get(core::slice::from_raw_parts(at.cast::<u64>(), T::WORDS)),
        }
    }
}

/// Write element `i` of a packed buffer.
///
/// # Safety
/// As [`read_elem`], and the buffer must be writable.
pub unsafe fn write_elem<T: Abi>(data: *mut u8, i: usize, value: &T) {
    check_layout::<T>();
    // SAFETY: the caller guarantees the element is in bounds and aligned.
    unsafe {
        let at = data.add(i * T::STRIDE);
        match T::STRIDE {
            1 | 4 => {
                let mut word = [0u64];
                value.put(&mut word);
                if T::STRIDE == 1 {
                    *at = word[0] as u8;
                } else {
                    at.cast::<u32>().write_unaligned(word[0] as u32);
                }
            }
            _ => value.put(core::slice::from_raw_parts_mut(at.cast::<u64>(), T::WORDS)),
        }
    }
}

/// The header words of a live sequence handle: `(len, data)`.
///
/// # Safety
/// `handle` must be a live sequence handle from the same library.
unsafe fn header(handle: u64) -> (usize, *mut u8) {
    let h = handle as usize as *const u64;
    // SAFETY: a live handle points at its three header words.
    unsafe { (*h as usize, *h.add(2) as usize as *mut u8) }
}

/// A packed scratch buffer holding a copy of `items`, for element types
/// whose Rust layout differs from the Vesper one.
fn packed<T: Abi>(items: &[T]) -> Vec<u64> {
    let mut words = alloc::vec![0u64; (items.len() * T::STRIDE).div_ceil(8)];
    for (i, item) in items.iter().enumerate() {
        // SAFETY: `words` holds `items.len()` elements of `T::STRIDE` bytes.
        unsafe { write_elem(words.as_mut_ptr().cast::<u8>(), i, item) };
    }
    words
}

/// Lend `items` to Vesper as an `in Slice[T]` view and run `call` with the
/// pointer to the view's words. Elements whose Rust layout is the packed
/// Vesper layout (the built-in scalars, see `native`) are lent in place, others are copied.
pub fn with_slice<T: Abi + 'static, R>(items: &[T], call: impl FnOnce(*mut u64) -> R) -> R {
    let copy;
    let data = if native::<T>() {
        items.as_ptr().cast::<u8>()
    } else {
        copy = packed(items);
        copy.as_ptr().cast::<u8>()
    };
    let len = items.len() as u64;
    let mut header = [len, len, data as usize as u64];
    let mut view = [header.as_mut_ptr() as usize as u64, 0, len];
    call(view.as_mut_ptr())
}

/// Lend `items` to Vesper as an `inout Slice[T]` view. Non-native elements
/// are copied in and back after the call.
pub fn with_slice_mut<T: Abi + 'static, R>(items: &mut [T], call: impl FnOnce(*mut u64) -> R) -> R {
    let len = items.len() as u64;
    if native::<T>() {
        let mut header = [len, len, items.as_mut_ptr() as usize as u64];
        let mut view = [header.as_mut_ptr() as usize as u64, 0, len];
        return call(view.as_mut_ptr());
    }
    let mut copy = packed(items);
    let mut header = [len, len, copy.as_mut_ptr() as usize as u64];
    let mut view = [header.as_mut_ptr() as usize as u64, 0, len];
    let result = call(view.as_mut_ptr());
    for (i, item) in items.iter_mut().enumerate() {
        // SAFETY: `copy` still holds `len` packed elements; a slice cannot
        // grow or move its buffer.
        *item = unsafe { read_elem(copy.as_ptr().cast::<u8>(), i) };
    }
    result
}

/// Lend `items` to Vesper as an `in ChunkSeq[T]` and run `call` with the
/// handle (a header the caller owns; Vesper only reads it).
pub fn with_seq<T: Abi + 'static, R>(items: &[T], call: impl FnOnce(u64) -> R) -> R {
    let copy;
    let data = if native::<T>() {
        items.as_ptr().cast::<u8>()
    } else {
        copy = packed(items);
        copy.as_ptr().cast::<u8>()
    };
    let len = items.len() as u64;
    let header = [len, len, data as usize as u64];
    call(header.as_ptr() as usize as u64)
}

/// Copy `items` into a fresh Vesper sequence of the library `rt` belongs to.
///
/// # Safety
/// `rt` must be the runtime of a linked v2 library.
pub unsafe fn seq_from<T: Abi>(rt: SeqRuntime, items: &[T]) -> u64 {
    // SAFETY: `seq_new` returns a header whose buffer holds `cap` elements.
    unsafe {
        let handle = (rt.new)(items.len() as i64, T::STRIDE as i64) as u64;
        let h = handle as usize as *mut u64;
        let data = *h.add(2) as usize as *mut u8;
        for (i, item) in items.iter().enumerate() {
            write_elem(data, i, item);
        }
        *h = items.len() as u64;
        handle
    }
}

/// Copy a sequence into a `Vec` and free it (a returned `ChunkSeq[T]`).
///
/// # Safety
/// `handle` must be a live sequence of `rt`'s library holding `T` elements,
/// owned by the caller; it is freed.
pub unsafe fn seq_take<T: Abi>(rt: SeqRuntime, handle: u64) -> Vec<T> {
    // SAFETY: forwarded from the caller.
    unsafe {
        let items = seq_copy(handle);
        (rt.free)(handle as i64);
        items
    }
}

/// Copy the elements of a live sequence.
///
/// # Safety
/// `handle` must be a live sequence holding `T` elements.
pub unsafe fn seq_copy<T: Abi>(handle: u64) -> Vec<T> {
    // SAFETY: forwarded from the caller.
    unsafe {
        let (len, data) = header(handle);
        (0..len).map(|i| read_elem(data, i)).collect()
    }
}

/// Lend `items` to Vesper as an `inout ChunkSeq[T]`: the elements are
/// copied into a Vesper sequence, `call` runs with a pointer to its handle
/// word, and the (possibly grown or replaced) sequence is copied back into
/// `items` and freed.
pub fn with_seq_mut<T: Abi, R>(
    rt: SeqRuntime,
    items: &mut Vec<T>,
    call: impl FnOnce(*mut u64) -> R,
) -> R {
    // SAFETY: `rt` is the runtime of the library being called, and the
    // handle stays owned here: Vesper only borrows it for the call.
    unsafe {
        let mut handle = [seq_from(rt, items)];
        let result = call(handle.as_mut_ptr());
        *items = seq_take(rt, handle[0]);
        result
    }
}

/// A borrowed view of a sequence that an owned handle holds. It reads the
/// Vesper buffer in place; the borrow keeps the handle alive and unchanged.
pub struct SeqRef<'a, T> {
    data: *const u8,
    len: usize,
    _owner: PhantomData<&'a [T]>,
}

impl<T> Clone for SeqRef<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for SeqRef<'_, T> {}

impl<'a, T: Abi> SeqRef<'a, T> {
    /// # Safety
    /// `handle` must be a live sequence of `T` elements that stays alive and
    /// unchanged for `'a`.
    pub unsafe fn from_handle(handle: u64) -> Self {
        // SAFETY: forwarded from the caller.
        let (len, data) = unsafe { header(handle) };
        Self {
            data,
            len,
            _owner: PhantomData,
        }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn get(&self, i: usize) -> Option<T> {
        // SAFETY: `i < len` elements are initialized for `'a`.
        (i < self.len).then(|| unsafe { read_elem(self.data, i) })
    }
    pub fn iter(&self) -> impl Iterator<Item = T> + 'a {
        let this = *self;
        (0..this.len).map(move |i| {
            // SAFETY: as in `get`.
            unsafe { read_elem(this.data, i) }
        })
    }
    pub fn to_vec(&self) -> Vec<T>
    where
        T: 'static,
    {
        if native::<T>() {
            // SAFETY: `native` holds, so the buffer is `len` initialized `T`s
            // in their Rust layout (as in `as_slice`), and `T` is a plain
            // scalar, so a bitwise copy is a copy.
            let items = unsafe { self.packed_slice() };
            let mut out = Vec::with_capacity(items.len());
            unsafe {
                core::ptr::copy_nonoverlapping(items.as_ptr(), out.as_mut_ptr(), items.len());
                out.set_len(items.len());
            }
            return out;
        }
        self.iter().collect()
    }

    /// # Safety
    /// `native::<T>()` must hold.
    unsafe fn packed_slice(&self) -> &'a [T] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `len` packed elements are initialized and unchanged for
        // `'a` (`from_handle`), the stride is `size_of::<T>()` (`native`),
        // and Vesper aligns element buffers to eight bytes.
        unsafe { core::slice::from_raw_parts(self.data.cast::<T>(), self.len) }
    }
}

impl<'a, T: Packed> SeqRef<'a, T> {
    /// The elements in place, as a Rust slice: no copy and no per-element
    /// read (#991). Only packed scalars ([`Packed`]) have a Rust layout that
    /// is the Vesper one.
    pub fn as_slice(&self) -> &'a [T] {
        debug_assert!(native::<T>());
        // SAFETY: a `Packed` type is one of the `native` scalars.
        unsafe { self.packed_slice() }
    }
}

/// Element types whose Rust layout is the packed Vesper layout, so a
/// [`SeqRef`] of them can be viewed as a `&[T]` ([`SeqRef::as_slice`]).
/// Sealed: exactly the scalars that `with_slice` lends in place.
pub trait Packed: Abi + Copy + 'static + sealed::Sealed {}

mod sealed {
    pub trait Sealed {}
}

macro_rules! packed {
    ($($t:ty),*) => {$(
        impl sealed::Sealed for $t {}
        impl Packed for $t {}
    )*};
}
packed!(u8, u32, u64, i64, usize, isize, f64);

impl<T: Abi + core::fmt::Debug> core::fmt::Debug for SeqRef<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// The words of a value behind a Vesper `Box[T]` pointer.
///
/// # Safety
/// `pointer` must be a live box of a `words`-word value that stays alive and
/// unchanged for `'a`.
pub unsafe fn box_words<'a>(pointer: u64, words: usize) -> &'a [u64] {
    // SAFETY: forwarded from the caller.
    unsafe { core::slice::from_raw_parts(pointer as usize as *const u64, words) }
}

/// Lend `s` to Vesper as an `in Str` (or `in String`): `call` runs with a
/// pointer to the three-word value `{len, 0, data}`, which borrows the bytes
/// in place (capacity 0 marks borrowed bytes; Vesper never writes them).
pub fn with_str<R>(s: &str, call: impl FnOnce(*mut u64) -> R) -> R {
    let mut words = [s.len() as u64, 0, s.as_ptr() as usize as u64];
    call(words.as_mut_ptr())
}

/// Give `s` to Vesper as a `sink String`: `call` runs with a pointer to a
/// three-word value whose bytes are a copy in a buffer of `rt`'s library.
/// The callee owns that buffer, so it may keep the string (in a returned
/// handle, say) after `s` is dropped; it frees the buffer when it drops it.
pub fn with_string_owned<R>(rt: SeqRuntime, s: &str, call: impl FnOnce(*mut u64) -> R) -> R {
    // SAFETY: `seq_new(len, 1)` returns a header whose buffer holds `len`
    // bytes. Its words become the string value; setting the header's
    // capacity to 0 before `seq_free` frees only the header, so the buffer
    // passes to the callee.
    let mut words = unsafe {
        let handle = (rt.new)(s.len() as i64, 1) as u64;
        let h = handle as usize as *mut u64;
        let data = *h.add(2) as usize as *mut u8;
        if !s.is_empty() {
            core::ptr::copy_nonoverlapping(s.as_ptr(), data, s.len());
        }
        let words = [s.len() as u64, *h.add(1), *h.add(2)];
        *h.add(1) = 0;
        (rt.free)(handle as i64);
        words
    };
    call(words.as_mut_ptr())
}

/// Lend `s` to Vesper as an `inout String`. The value starts out borrowing
/// the Rust bytes (Vesper copies them before its first write); after the
/// call the (possibly grown or replaced) string is copied back into `s` and
/// its Vesper buffer freed.
///
/// Panics if Vesper left bytes that are not UTF-8, which a Vesper `String`
/// never holds.
pub fn with_string_mut<R>(rt: SeqRuntime, s: &mut String, call: impl FnOnce(*mut u64) -> R) -> R {
    // SAFETY: `seq_new(0, 1)` returns a three-word header with no buffer,
    // which is a valid `String` value; it stays owned here and Vesper only
    // borrows it for the call. `seq_free` frees the header and, when the
    // capacity is not 0, the buffer Vesper allocated.
    unsafe {
        let handle = (rt.new)(0, 1) as u64;
        let h = handle as usize as *mut u64;
        *h = s.len() as u64;
        *h.add(1) = 0;
        *h.add(2) = s.as_ptr() as usize as u64;
        let result = call(h);
        let text = string_copy(core::slice::from_raw_parts(h, 3));
        (rt.free)(handle as i64);
        *s = text;
        result
    }
}

/// Copy a returned `String` value (its three words) into a Rust `String`
/// and free its Vesper buffer.
///
/// # Safety
/// `words` must be a live Vesper string of `rt`'s library, owned by the
/// caller; its buffer is freed.
pub unsafe fn string_take(rt: SeqRuntime, words: &[u64]) -> String {
    // SAFETY: forwarded from the caller; the fresh header takes over the
    // string's buffer so that `seq_free` releases it (or skips borrowed
    // bytes, whose capacity is 0).
    unsafe {
        let text = string_copy(words);
        let handle = (rt.new)(0, 1) as u64;
        let h = handle as usize as *mut u64;
        for (i, word) in words[..3].iter().enumerate() {
            *h.add(i) = *word;
        }
        (rt.free)(handle as i64);
        text
    }
}

/// Borrow the bytes of a live Vesper string value (its three words).
///
/// # Safety
/// `words` must be a live Vesper string that stays alive and unchanged
/// while the result is borrowed.
pub unsafe fn str_view(words: &[u64]) -> &str {
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { string_bytes(words) };
    core::str::from_utf8(bytes).expect("a Vesper string holds UTF-8")
}

/// The bytes of a string value `{len, cap, data}`.
///
/// # Safety
/// `words` must be a live Vesper string.
unsafe fn string_bytes(words: &[u64]) -> &[u8] {
    let len = words[0] as usize;
    if len == 0 {
        return &[];
    }
    // SAFETY: forwarded from the caller.
    unsafe { core::slice::from_raw_parts(words[2] as usize as *const u8, len) }
}

/// Copy a string value into a Rust `String`.
///
/// # Safety
/// `words` must be a live Vesper string.
unsafe fn string_copy(words: &[u64]) -> String {
    // SAFETY: forwarded from the caller.
    String::from(unsafe { str_view(words) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A safe `Abi` implementation whose stride disagrees with its words.
    struct Wide(u64, u64);
    impl Abi for Wide {
        const WORDS: usize = 2;
        const STRIDE: usize = 8;
        fn put(&self, out: &mut [u64]) {
            out[0] = self.0;
            out[1] = self.1;
        }
        fn get(words: &[u64]) -> Self {
            Wide(words[0], words[1])
        }
    }

    #[test]
    #[should_panic(expected = "inconsistent Abi layout")]
    fn inconsistent_layouts_are_refused_before_raw_access() {
        with_slice(&[Wide(1, 2)], |_| ());
    }

    #[test]
    fn only_builtin_scalars_are_lent_in_place() {
        assert!(native::<u64>() && native::<u8>() && native::<u32>() && native::<f64>());
        assert!(!native::<bool>() && !native::<i32>() && !native::<(u64,)>());
    }

    #[test]
    fn copied_slices_round_trip() {
        let mut xs = [-1i32, 7, i32::MIN];
        with_slice_mut(&mut xs, |view| {
            // SAFETY: the view's header and buffer live for the call.
            let header = unsafe { *view } as usize as *const u64;
            let data = unsafe { *header.add(2) } as usize as *mut u8;
            unsafe { write_elem(data, 1, &8i32) };
        });
        assert_eq!(xs, [-1, 8, i32::MIN]);
    }

    #[test]
    fn strings_are_lent_and_viewed_in_place() {
        let text = "héllo";
        with_str(text, |words| {
            // SAFETY: the value lives for the call.
            let view = unsafe { str_view(core::slice::from_raw_parts(words, 3)) };
            assert_eq!(view, text);
            assert_eq!(view.as_ptr(), text.as_ptr());
            assert_eq!(unsafe { *words.add(1) }, 0);
        });
        assert_eq!(unsafe { str_view(&[0, 0, 0]) }, "");
    }

    #[test]
    fn packed_sequences_are_viewed_in_place() {
        let bytes = *b"GET / HTTP/1.1";
        let header = [
            bytes.len() as u64,
            bytes.len() as u64,
            bytes.as_ptr() as usize as u64,
        ];
        // SAFETY: `header` is a live sequence of `bytes` for the test.
        let seq = unsafe { SeqRef::<u8>::from_handle(header.as_ptr() as usize as u64) };
        assert_eq!(seq.as_slice(), &bytes);
        assert_eq!(seq.as_slice().as_ptr(), bytes.as_ptr());
        assert_eq!(seq.to_vec(), bytes.to_vec());
        let words = [3u64, u64::MAX];
        let header = [2, 2, words.as_ptr() as usize as u64];
        // SAFETY: as above.
        let seq = unsafe { SeqRef::<u64>::from_handle(header.as_ptr() as usize as u64) };
        assert_eq!(seq.as_slice(), &words);
        let empty = [0u64, 0, 0];
        // SAFETY: an empty sequence with no buffer.
        let seq = unsafe { SeqRef::<u32>::from_handle(empty.as_ptr() as usize as u64) };
        assert!(seq.as_slice().is_empty() && seq.to_vec().is_empty());
        // Other elements still convert one at a time.
        let narrow = [1u64, 0];
        let header = [2, 2, narrow.as_ptr() as usize as u64];
        // SAFETY: two one-byte `bool`s are in the first word.
        let seq = unsafe { SeqRef::<bool>::from_handle(header.as_ptr() as usize as u64) };
        assert_eq!(seq.to_vec(), [true, false]);
    }
}
