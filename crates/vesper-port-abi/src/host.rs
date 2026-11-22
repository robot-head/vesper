//! Calling an exported function from a host that holds only its entry
//! point (#1147): a JIT `WordFunction`, or an export-ABI symbol a binding
//! does not cover. [`HostCall`] lays out `Str` and `Slice[u8]` arguments the
//! way the export ABI passes them, by the address of their words, and
//! [`HostCall::seq`] and [`HostCall::string`] take a `ChunkSeq[T]` or
//! `String` result back into Rust and free it.
//!
//! ```ignore
//! let words: Vec<u64> = unsafe {
//!     HostCall::new().str(source).seq(RUNTIME, |args| entry(args[0]))
//! };
//! ```
use crate::{Abi, SeqRuntime, seq_take, string_take};
use alloc::{string::String, vec::Vec};

/// One argument of a [`HostCall`].
#[derive(Clone, Copy)]
enum Arg<'a> {
    Word(u64),
    Str(&'a str),
    Bytes(&'a [u8]),
}

/// The arguments of one call, borrowed for its duration.
#[derive(Default)]
pub struct HostCall<'a> {
    args: Vec<Arg<'a>>,
}

impl<'a> HostCall<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// A one-word argument (a scalar, a handle or an address).
    pub fn word(mut self, w: u64) -> Self {
        self.args.push(Arg::Word(w));
        self
    }

    /// An `in Str` (or `in String`) argument: the address of the value
    /// `{len, 0, data}`, which borrows `s` in place.
    pub fn str(mut self, s: &'a str) -> Self {
        self.args.push(Arg::Str(s));
        self
    }

    /// An `in Slice[u8]` argument: the address of the view
    /// `{header, 0, len}` over a header `{len, len, data}` that borrows
    /// `b` in place.
    pub fn bytes(mut self, b: &'a [u8]) -> Self {
        self.args.push(Arg::Bytes(b));
        self
    }

    /// Runs `call` with one word per argument, in order. The words stay
    /// valid for the call only.
    pub fn with_words<R>(&self, call: impl FnOnce(&[u64]) -> R) -> R {
        // Every value block is made before any address is taken, so none
        // moves while `call` runs.
        let mut values: Vec<[u64; 3]> = self
            .args
            .iter()
            .map(|a| match *a {
                Arg::Word(_) => [0; 3],
                Arg::Str(s) => [s.len() as u64, 0, s.as_ptr() as usize as u64],
                Arg::Bytes(b) => [b.len() as u64, b.len() as u64, b.as_ptr() as usize as u64],
            })
            .collect();
        let mut views: Vec<[u64; 3]> = values
            .iter_mut()
            .zip(&self.args)
            .map(|(v, a)| match *a {
                Arg::Bytes(b) => [v.as_mut_ptr() as usize as u64, 0, b.len() as u64],
                _ => [0; 3],
            })
            .collect();
        let words: Vec<u64> = self
            .args
            .iter()
            .zip(values.iter_mut().zip(views.iter_mut()))
            .map(|(a, (v, view))| match *a {
                Arg::Word(w) => w,
                Arg::Str(_) => v.as_mut_ptr() as usize as u64,
                Arg::Bytes(_) => view.as_mut_ptr() as usize as u64,
            })
            .collect();
        call(&words)
    }

    /// Calls a function returning a `ChunkSeq[T]` handle (one word) and
    /// copies the elements into a `Vec`, freeing the sequence.
    ///
    /// # Safety
    /// `call` must pass the words to a function of `rt`'s library (or JIT
    /// runtime) whose parameters take them as this call lays them out, and
    /// return its result: a live, owned sequence of `T` elements.
    pub unsafe fn seq<T: Abi + 'static>(
        &self,
        rt: SeqRuntime,
        call: impl FnOnce(&[u64]) -> u64,
    ) -> Vec<T> {
        let handle = self.with_words(call);
        // SAFETY: forwarded from the caller.
        unsafe { seq_take(rt, handle) }
    }

    /// Calls a function returning a `String` through a result pointer, as
    /// the export ABI returns an aggregate: `call` gets the address of a
    /// three-word result buffer and the argument words. The string is copied
    /// into a Rust `String` (as UTF-8, without validating it again) and its
    /// Vesper buffer freed.
    ///
    /// # Safety
    /// As for [`HostCall::seq`]; on return the buffer holds a live, owned
    /// Vesper string of `rt`'s library.
    pub unsafe fn string(&self, rt: SeqRuntime, call: impl FnOnce(*mut u64, &[u64])) -> String {
        let mut result = [0u64; 3];
        self.with_words(|words| call(result.as_mut_ptr(), words));
        // SAFETY: forwarded from the caller.
        unsafe { string_take(rt, &result) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SeqRef, str_view};

    #[test]
    fn arguments_are_laid_out_as_the_export_abi_passes_them() {
        let text = "héllo";
        let bytes = [1u8, 2, 3];
        HostCall::new()
            .word(7)
            .str(text)
            .bytes(&bytes)
            .with_words(|w| {
                assert_eq!(w.len(), 3);
                assert_eq!(w[0], 7);
                // SAFETY: the blocks live for the call.
                let s = unsafe { str_view(core::slice::from_raw_parts(w[1] as *const u64, 3)) };
                assert_eq!((s, s.as_ptr()), (text, text.as_ptr()));
                let view = unsafe { core::slice::from_raw_parts(w[2] as *const u64, 3) };
                assert_eq!(&view[1..], &[0, 3]);
                // SAFETY: the view's header is a live sequence of `bytes`.
                let seq = unsafe { SeqRef::<u8>::from_handle(view[0]) };
                assert_eq!(seq.as_slice(), &bytes);
                assert_eq!(seq.as_slice().as_ptr(), bytes.as_ptr());
            });
    }
}
