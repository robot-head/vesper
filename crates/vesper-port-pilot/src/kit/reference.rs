//! The Rust implementation of `vesper/kit.ves`, kept as the reference.
use super::{Command, KitError, TreeNode};
use std::collections::VecDeque;

/// `i * i` for `i < n` (wrapping).
pub fn squares(n: u64) -> Vec<u64> {
    (0..n).map(|i| i.wrapping_mul(i)).collect()
}

/// The decimal digits of `n`, least significant first.
pub fn digits(n: u64) -> Vec<u8> {
    let mut out = vec![(n % 10) as u8];
    let mut rest = n / 10;
    while rest != 0 {
        out.push((rest % 10) as u8);
        rest /= 10;
    }
    out
}

/// `n, n - 1, ..., 0` (empty when `n < 0`).
pub fn countdown(n: i32) -> Vec<i32> {
    (0..=n).rev().collect()
}

/// The wrapping sum.
pub fn sum(xs: &[u64]) -> u64 {
    xs.iter().fold(0, |a, &x| a.wrapping_add(x))
}

/// How many ASCII digits `text` holds.
pub fn count_digits(text: &[u8]) -> u64 {
    text.iter().filter(|c| c.is_ascii_digit()).count() as u64
}

/// Multiply every element by `k` (wrapping).
pub fn scale(xs: &mut [i32], k: i32) {
    for x in xs {
        *x = x.wrapping_mul(k);
    }
}

/// Append `i * i` for `i < n`.
pub fn push_squares(xs: &mut Vec<u64>, n: u64) {
    xs.extend(squares(n));
}

/// `x += by` (wrapping).
pub fn bump(x: &mut u64, by: u64) {
    *x = x.wrapping_add(by);
}

/// The largest of `init` and `xs`.
pub fn largest_u64(xs: &[u64], init: u64) -> u64 {
    xs.iter().copied().fold(init, u64::max)
}

/// The largest of `init` and `xs`.
pub fn largest_i64(xs: &[i64], init: i64) -> i64 {
    xs.iter().copied().fold(init, i64::max)
}

/// A running total that remembers every value.
#[derive(Debug, Default)]
pub struct Counter {
    hits: Vec<u64>,
    total: u64,
}

impl Counter {
    pub fn new() -> Counter {
        Counter::default()
    }
    pub fn record(&mut self, v: u64) {
        self.hits.push(v);
        self.total = self.total.wrapping_add(v);
    }
    pub fn total(&self) -> u64 {
        self.total
    }
    pub fn hits(&self) -> Vec<u64> {
        self.hits.clone()
    }
    pub fn reset(&mut self) {
        self.hits.clear();
        self.total = 0;
    }
}

/// A ticket that must be closed.
#[derive(Debug)]
#[must_use = "a linear Vesper value must be released by a consuming call"]
pub struct Ticket {
    id: u64,
}

impl Ticket {
    pub fn open(id: u64) -> Ticket {
        Ticket { id }
    }
    pub fn close(self) -> u64 {
        self.id
    }
}

/// A complete binary tree whose leaves are numbered `2^depth ..`.
#[derive(Debug)]
pub struct Tree(TreeNode);

impl Tree {
    pub fn build(depth: u64) -> Tree {
        fn grow(depth: u64, at: u64) -> TreeNode {
            if depth == 0 {
                TreeNode::Leaf(at)
            } else {
                let l = grow(depth - 1, at.wrapping_mul(2));
                let r = grow(depth - 1, at.wrapping_mul(2).wrapping_add(1));
                TreeNode::Node(Box::new(l), Box::new(r))
            }
        }
        Tree(grow(depth, 1))
    }
    pub fn sum(self) -> u64 {
        fn sum(t: &TreeNode) -> u64 {
            match t {
                TreeNode::Leaf(v) => *v,
                TreeNode::Node(l, r) => sum(l).wrapping_add(sum(r)),
            }
        }
        sum(&self.0)
    }
}

/// The tree behind a [`Tree`].
pub fn tree_shape(tree: &Tree) -> TreeNode {
    tree.0.clone()
}

/// A FIFO of tokens.
#[derive(Debug, Default)]
pub struct Tokens(VecDeque<u64>);

impl Tokens {
    pub fn new() -> Tokens {
        Tokens::default()
    }
    pub fn push(&mut self, token: u64) {
        self.0.push_back(token);
    }
    pub fn pop(&mut self) -> Option<u64> {
        self.0.pop_front()
    }
}

/// `"hello, " + name`.
pub fn greet(name: &str) -> String {
    format!("hello, {name}")
}

/// How many space-separated words `text` holds.
pub fn word_count(text: &str) -> u64 {
    text.split(' ').filter(|w| !w.is_empty()).count() as u64
}

/// Append `!`.
pub fn shout(s: &mut String) {
    s.push('!');
}

/// A text that remembers how often it was edited.
#[derive(Debug)]
pub struct Note {
    text: String,
    edits: u64,
}

impl Note {
    pub fn new(text: &str) -> Note {
        Note {
            text: text.to_string(),
            edits: 0,
        }
    }
    pub fn append(&mut self, more: &str) {
        self.text.push_str(more);
        self.edits = self.edits.wrapping_add(1);
    }
    pub fn edits(&self) -> u64 {
        self.edits
    }
    pub fn clear(&mut self) {
        self.text.clear();
        self.edits = self.edits.wrapping_add(1);
    }
}

/// The text of a [`Note`].
pub fn note_text(note: &Note) -> &str {
    &note.text
}

/// Word boundaries of space-separated `text`, as start/end pairs.
pub fn word_bounds(text: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, &c) in text.iter().enumerate() {
        match (c == b' ', start) {
            (true, Some(s)) => {
                out.extend([s as u64, i as u64]);
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        out.extend([s as u64, text.len() as u64]);
    }
    out
}

/// Whether `deadline` has passed at `now`.
pub fn expired(now: u64, deadline: u64) -> bool {
    now >= deadline
}

/// Send `bytes` to every peer, pausing after each.
pub fn plan_sends(peers: u32, bytes: u64, pause: u64) -> Vec<Command> {
    (0..peers)
        .flat_map(|to| [Command::Send { to, bytes }, Command::Wait(pause)])
        .collect()
}

/// The integer mean; fails on no values or a total over one million.
pub fn mean(xs: &[u64]) -> Result<u64, KitError> {
    if xs.is_empty() {
        return Err(KitError::Empty);
    }
    let total = sum(xs);
    if total > 1_000_000 {
        Err(KitError::TooLarge(total))
    } else {
        Ok(total / xs.len() as u64)
    }
}

/// `base` plus the clock's ticks.
pub fn stamp(base: u64) -> u64 {
    base.wrapping_add(super::shim::pilot_clock_ticks())
}

/// Live Vesper allocations: none in the Rust build.
pub fn live_allocations() -> i64 {
    0
}
