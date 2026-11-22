//! Prompt encoding with the llama2.c / SentencePiece-BPE algorithm.
//!
//! [`Encoder::encode`] reproduces llama2.c `run.c` `encode()`:
//!
//! 1. optional BOS (id 1);
//! 2. a dummy-prefix token for `" "` when the text is non-empty;
//! 3. one token per UTF-8 code point when that code point is a vocabulary
//!    piece, otherwise one byte-fallback token `<0xHH>` per byte (code points
//!    are cut after four bytes, and stray continuation bytes stand alone, as
//!    in `run.c`);
//! 4. repeatedly merge the adjacent pair whose concatenation is a vocabulary
//!    piece with the highest score (the leftmost pair on ties; scores at or
//!    below `-1e10` never merge), until no pair merges;
//! 5. optional EOS (id 2).
//!
//! Two deliberate differences from `run.c`: the BOS token never takes part
//! in merges (in `run.c` it could, if the vocabulary held a piece starting
//! with the BOS piece), and a missing `" "` or `<0xHH>` piece is a typed error
//! instead of an out-of-range id. Duplicate pieces resolve to the lowest id
//! (`run.c`'s binary search picks an unspecified one).
//!
//! Merging uses a priority queue with lazy invalidation, so the cost is
//! `O(n log n)` lookups instead of `run.c`'s `O(n^2)`; the chosen merge at
//! every step is the same.
//!
//! The package `vesper-tensor` encodes with the Vesper port
//! (`vesper/src/tokens.ves`); the seed copy the compiler links runs the
//! frozen Rust encoder in `seed/src/engine/tokenizer.rs`.
#[cfg(vesper_port)]
use crate::llama::Tokenizer;
#[cfg(vesper_port)]
use crate::Error;

#[cfg(not(vesper_port))]
pub use crate::engine::tokenizer::Encoder;

pub const BOS: u32 = 1;
pub const EOS: u32 = 2;
/// Largest prompt accepted by [`Encoder::encode`].
pub const MAX_PROMPT_BYTES: usize = 1 << 20;
/// Pairs scored at or below this never merge.
#[cfg_attr(vesper_port, allow(dead_code))]
pub(crate) const NEVER_MERGE: f32 = -1e10;

/// A vocabulary lookup table over a [`Tokenizer`]: the vocabulary flattened
/// for the Vesper port (`vesper/src/tokens.ves`), with its hash table.
#[cfg(vesper_port)]
#[derive(Debug)]
pub struct Encoder<'t> {
    // Ties the flattened copy to the tokenizer it was built from.
    #[allow(dead_code)]
    tokenizer: &'t Tokenizer,
    bytes: Vec<u8>,
    offsets: Vec<u64>,
    scores: Vec<u32>,
    table: Vec<u32>,
    key: usize,
}

#[cfg(vesper_port)]
impl<'t> Encoder<'t> {
    pub fn new(tokenizer: &'t Tokenizer) -> Self {
        let vocab = tokenizer.vocab();
        let mut bytes = Vec::new();
        let mut offsets = Vec::with_capacity(vocab + 1);
        let mut scores = Vec::with_capacity(vocab);
        let mut longest = 0;
        offsets.push(0);
        for id in 0..vocab as u32 {
            let piece = tokenizer.piece(id).expect("id below vocab");
            bytes.extend_from_slice(piece);
            offsets.push(bytes.len() as u64);
            scores.push(tokenizer.score(id).expect("id below vocab").to_bits());
            longest = longest.max(piece.len());
        }
        let mut table = vec![0; (2 * vocab).next_power_of_two().max(2)];
        crate::port::must(crate::port::binding::vocab_table(
            &bytes, &offsets, &mut table,
        ));
        Self {
            tokenizer,
            bytes,
            offsets,
            scores,
            table,
            key: (2 * longest).max(6),
        }
    }
    /// The id of a vocabulary piece (lowest id on duplicates).
    pub fn id(&self, piece: &[u8]) -> Option<u32> {
        let e = crate::port::must(crate::port::binding::vocab_lookup(
            &self.bytes,
            &self.offsets,
            &self.table,
            piece,
        ));
        e.checked_sub(1)
    }
    /// Encodes `text` (see the module documentation).
    pub fn encode(&self, text: &[u8], bos: bool, eos: bool) -> Result<Vec<u32>, Error> {
        if text.len() > MAX_PROMPT_BYTES {
            return Err(Error::Limit("prompt bytes"));
        }
        let mut out = vec![0; text.len() + 3];
        let mut key = vec![0; self.key];
        // The port writes every work word before it reads it, so one
        // per-thread buffer serves every call without being cleared.
        thread_local! {
            static WORK: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        let words = 15 * (text.len() + 1) + 4;
        let status = WORK.with(|work| {
            let mut work = work.borrow_mut();
            if work.len() < words {
                work.resize(words, 0);
            }
            crate::port::binding::encode_prompt(
                text,
                &self.bytes,
                &self.offsets,
                &self.scores,
                &self.table,
                bos as u32,
                eos as u32,
                &mut out,
                &mut work[..words],
                &mut key,
            )
        });
        let n = crate::port::check(status, |fault| match fault {
            crate::port::Fault::NoSpacePiece => {
                Error::Malformed("vocabulary lacks the \" \" piece")
            }
            crate::port::Fault::NoBytePiece => {
                Error::Malformed("vocabulary lacks a byte-fallback piece")
            }
            _ => Error::Limit("prompt bytes"),
        })?;
        out.truncate(n as usize);
        Ok(out)
    }
}
