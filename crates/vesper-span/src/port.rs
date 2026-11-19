//! The port build's side of the boundary (`cfg(vesper_port)`): the types the
//! generated binding (`port/binding.rs`) names as `crate::port::*`, and the
//! flat encodings of edits and diagnostics that the Vesper functions read
//! (documented in `vesper/src/lib.ves`).

pub use crate::port_binding::*;
pub use crate::{ByteSpan, Error, Limits, Position, Range, Resource};
use crate::{DiagnosticData, SourceEdit};

/// The output length and the metadata bytes `validate_edits` charges.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditTotals {
    pub output: usize,
    pub metadata: usize,
}

/// `size_of` of the Rust records that the budgets charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sizes {
    pub line: usize,
    pub edit: usize,
    pub label: usize,
    pub fix: usize,
    pub diagnostic: usize,
}

const PRIMARY: u64 = 1 << 63;

/// Bounds the encodings by the budgets, so a batch that the checks reject
/// is not copied first. `work` is a lower bound of the work the checks charge
/// (only the charges that come before the expected text is read), so the
/// encoding stops only where the check is sure to fail at or before the item
/// it stops at. An edit past the budget keeps its words (the check reads the
/// lengths and raises) without its expected text.
struct Budget {
    source: usize,
    cap: usize,
    work: usize,
}

impl Budget {
    fn new(source: usize, limits: Limits) -> Self {
        Self {
            source,
            cap: limits.max_work,
            work: 0,
        }
    }
    /// Charges `n`, and says whether the work is still within the cap.
    fn charge(&mut self, n: usize) -> bool {
        self.work = self.work.saturating_add(n);
        self.work <= self.cap
    }
}

/// Pushes the edits, and says whether the encoding can go on.
fn push_edits(
    edits: &[SourceEdit],
    words: &mut Vec<u64>,
    pool: &mut String,
    budget: &mut Budget,
) -> bool {
    for e in edits {
        words.extend_from_slice(&[
            e.span.start as u64,
            e.span.end as u64,
            pool.len() as u64,
            e.expected.len() as u64,
            e.replacement.len() as u64,
        ]);
        // The check charges both lengths, then both endpoint scans, before it
        // reads the expected text.
        if !budget.charge(e.expected.len().saturating_add(e.replacement.len()))
            || !budget.charge(budget.source.saturating_mul(2))
        {
            return false;
        }
        pool.push_str(&e.expected);
    }
    true
}

/// Five words per edit, and the expected texts. The caller has already
/// checked the limits, the source size and the edit count.
pub fn encode_edits(source: usize, edits: &[SourceEdit], limits: Limits) -> (Vec<u64>, String) {
    let mut words = Vec::with_capacity(edits.len() * 5);
    let mut pool = String::new();
    push_edits(
        edits,
        &mut words,
        &mut pool,
        &mut Budget::new(source, limits),
    );
    (words, pool)
}

/// The sorted edits as `SourceEdits::apply` reads them.
pub fn encode_replacements(edits: &[SourceEdit]) -> (Vec<u64>, String) {
    let mut words = Vec::with_capacity(edits.len() * 3);
    let mut pool = String::with_capacity(edits.iter().map(|e| e.replacement.len()).sum());
    for e in edits {
        words.extend_from_slice(&[
            e.span.start as u64,
            e.span.end as u64,
            e.replacement.len() as u64,
        ]);
        pool.push_str(&e.replacement);
    }
    (words, pool)
}

/// The diagnostics encoding of `diagnostics_check`. The caller has already
/// checked the limits, the source size and the diagnostic count. The encoding
/// stops at the first label, fix or edit count past its budget, at the point
/// where the check raises for it, so its size is bounded by the limits.
pub fn encode_diagnostics(
    source: usize,
    data: &[DiagnosticData],
    limits: Limits,
) -> (Vec<u64>, String) {
    let mut words = Vec::new();
    let mut pool = String::new();
    let mut budget = Budget::new(source, limits);
    let (mut labels, mut fixes, mut edits) = (0usize, 0usize, 0usize);
    for d in data {
        words.extend_from_slice(&[
            d.code.len() as u64,
            d.message.len() as u64,
            d.help.is_some() as u64,
            d.help.as_ref().map_or(0, String::len) as u64,
            d.labels.len() as u64,
            d.fixes.len() as u64,
        ]);
        // The label count is checked before the labels are read.
        labels = labels.saturating_add(d.labels.len());
        if labels > limits.max_labels {
            break;
        }
        let mut within = true;
        for l in &d.labels {
            within &= budget.charge(source.saturating_mul(2));
            words.extend_from_slice(&[
                l.span.start as u64,
                l.span.end as u64,
                l.message.len() as u64 | if l.primary { PRIMARY } else { 0 },
            ]);
        }
        // The fix count is checked after the labels' primary flags are read.
        fixes = fixes.saturating_add(d.fixes.len());
        if !within || fixes > limits.max_fixes {
            break;
        }
        for f in &d.fixes {
            words.extend_from_slice(&[
                f.id.len() as u64,
                f.title.len() as u64,
                f.edits.len() as u64,
            ]);
            // The edit count is checked before the fix's edits are read.
            edits = edits.saturating_add(f.edits.len());
            if edits > limits.max_edits || !push_edits(&f.edits, &mut words, &mut pool, &mut budget)
            {
                return (words, pool);
            }
        }
    }
    (words, pool)
}
