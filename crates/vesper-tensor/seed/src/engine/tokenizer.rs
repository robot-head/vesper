//! The frozen Rust prompt encoder (bug fixes only until self-hosting
//! Stage 2). The package vesper-tensor encodes with the Vesper port
//! (vesper/src/tokens.ves); this seed copy, which the compiler links, keeps
//! the Rust encoder. Moved here from src/tokenizer.rs by SH6 lane 11 (#1406).
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};

use crate::llama::Tokenizer;
use crate::tokenizer::{BOS, EOS, MAX_PROMPT_BYTES, NEVER_MERGE};
use crate::Error;

/// UTF-8 continuation bytes are `0b10xx_xxxx` (RFC 3629 §3).
const UTF8_CONTINUATION_MASK: u8 = 0xc0;
const UTF8_CONTINUATION: u8 = 0x80;

/// A vocabulary lookup table over a [`Tokenizer`].
#[derive(Debug)]
pub struct Encoder<'t> {
    tokenizer: &'t Tokenizer,
    lookup: HashMap<&'t [u8], u32>,
}

#[derive(Clone, Copy, PartialEq)]
struct Score(f32);
impl Eq for Score {}
impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Score {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

const NONE: usize = usize::MAX;
struct Node {
    token: u32,
    prev: usize,
    next: usize,
    version: u32,
    alive: bool,
}

impl<'t> Encoder<'t> {
    pub fn new(tokenizer: &'t Tokenizer) -> Self {
        let mut lookup = HashMap::with_capacity(tokenizer.vocab());
        for id in 0..tokenizer.vocab() as u32 {
            let piece = tokenizer.piece(id).expect("id below vocab");
            lookup.entry(piece).or_insert(id);
        }
        Self { tokenizer, lookup }
    }
    /// The id of a vocabulary piece (lowest id on duplicates).
    pub fn id(&self, piece: &[u8]) -> Option<u32> {
        self.lookup.get(piece).copied()
    }
    fn byte_token(&self, b: u8) -> Result<u32, Error> {
        let name = format!("<0x{b:02X}>");
        self.id(name.as_bytes())
            .ok_or(Error::Malformed("vocabulary lacks a byte-fallback piece"))
    }
    /// Encodes `text` (see the module documentation).
    pub fn encode(&self, text: &[u8], bos: bool, eos: bool) -> Result<Vec<u32>, Error> {
        if text.len() > MAX_PROMPT_BYTES {
            return Err(Error::Limit("prompt bytes"));
        }
        let mut initial = Vec::with_capacity(text.len() + 1);
        if !text.is_empty() {
            initial.push(
                self.id(b" ")
                    .ok_or(Error::Malformed("vocabulary lacks the \" \" piece"))?,
            );
        }
        let mut start = 0usize;
        let mut len = 0usize;
        for (i, &b) in text.iter().enumerate() {
            if b & UTF8_CONTINUATION_MASK != UTF8_CONTINUATION || len == 0 {
                start = i;
                len = 0;
            }
            len += 1;
            let continues = text
                .get(i + 1)
                .is_some_and(|next| next & UTF8_CONTINUATION_MASK == UTF8_CONTINUATION);
            if continues && len < 4 {
                continue;
            }
            let unit = &text[start..start + len];
            match self.id(unit) {
                Some(id) => initial.push(id),
                None => {
                    for &byte in unit {
                        initial.push(self.byte_token(byte)?);
                    }
                }
            }
            len = 0;
        }
        let mut out = Vec::with_capacity(initial.len() + 2);
        if bos {
            out.push(BOS);
        }
        out.extend(self.merge(initial));
        if eos {
            out.push(EOS);
        }
        Ok(out)
    }

    fn merge(&self, tokens: Vec<u32>) -> Vec<u32> {
        let n = tokens.len();
        let mut nodes: Vec<Node> = tokens
            .iter()
            .enumerate()
            .map(|(i, &token)| Node {
                token,
                prev: if i == 0 { NONE } else { i - 1 },
                next: if i + 1 == n { NONE } else { i + 1 },
                version: 0,
                alive: true,
            })
            .collect();
        // (score, leftmost first, left, left version, right, right version, merged id)
        type Candidate = (Score, Reverse<usize>, usize, u32, usize, u32, u32);
        let mut heap: BinaryHeap<Candidate> = BinaryHeap::new();
        let mut buf = Vec::new();
        let candidate = |nodes: &[Node], i: usize, buf: &mut Vec<u8>| -> Option<Candidate> {
            let j = nodes[i].next;
            if j == NONE {
                return None;
            }
            buf.clear();
            buf.extend_from_slice(self.tokenizer.piece(nodes[i].token)?);
            buf.extend_from_slice(self.tokenizer.piece(nodes[j].token)?);
            let id = self.id(buf)?;
            let score = self.tokenizer.score(id)?;
            (score > NEVER_MERGE).then_some((
                Score(score),
                Reverse(i),
                i,
                nodes[i].version,
                j,
                nodes[j].version,
                id,
            ))
        };
        for i in 0..n {
            if let Some(c) = candidate(&nodes, i, &mut buf) {
                heap.push(c);
            }
        }
        while let Some((_, _, i, vi, j, vj, id)) = heap.pop() {
            let fresh = nodes[i].alive
                && nodes[j].alive
                && nodes[i].next == j
                && nodes[i].version == vi
                && nodes[j].version == vj;
            if !fresh {
                continue;
            }
            nodes[i].token = id;
            nodes[i].version += 1;
            nodes[j].alive = false;
            let after = nodes[j].next;
            nodes[i].next = after;
            if after != NONE {
                nodes[after].prev = i;
            }
            let before = nodes[i].prev;
            if before != NONE {
                if let Some(c) = candidate(&nodes, before, &mut buf) {
                    heap.push(c);
                }
            }
            if let Some(c) = candidate(&nodes, i, &mut buf) {
                heap.push(c);
            }
        }
        let mut out = Vec::with_capacity(n);
        let mut i = if n == 0 { NONE } else { 0 };
        while i != NONE {
            out.push(nodes[i].token);
            i = nodes[i].next;
        }
        out
    }
}
