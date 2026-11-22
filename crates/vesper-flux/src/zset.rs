use crate::{Budget, Error, Limits};
use std::collections::BTreeMap;

pub type Row = Vec<i64>;

/// Canonical nonzero weights; private representation prevents malformed batches.
///
/// A set is the flat cells the Vesper kernels read and write in place: each row's `width` columns, then its weight, rows strictly
/// ascending. [`ZSet::entries`] lends rows as `&Row`, so it builds them once,
/// on first use.
pub struct ZSet {
    pub(crate) width: usize,
    pub(crate) cells: Vec<i64>,
    rows: std::sync::OnceLock<Vec<Row>>,
}
impl ZSet {
    pub fn empty(width: usize, limits: Limits) -> Result<Self, Error> {
        if width > limits.max_columns {
            return Err(Error::Limit("columns"));
        }
        Ok(Self::from_rows(width, BTreeMap::new()))
    }
    /// Borrows the raw owner, including on overflow/limit/schema errors.
    pub fn consolidate(
        width: usize,
        entries: &[(Row, i64)],
        limits: Limits,
    ) -> Result<Self, Error> {
        if entries.len() > limits.max_input_entries {
            return Err(Error::Limit("input entries"));
        }
        if width > limits.max_columns {
            return Err(Error::Limit("columns"));
        }
        // The kernel takes rows of the right width; the first other row is
        // a schema error once the rows before it are added (its own charge
        // comes first). Only the entries the work budget can reach are
        // looked at or copied, as in the Rust loop: with every row before
        // it of the right width, entry `reach - 1` cannot be paid for.
        let reach = entries
            .len()
            .min((limits.max_work / (width + 1)).saturating_add(1));
        let good = entries[..reach]
            .iter()
            .position(|(row, _)| row.len() != width)
            .unwrap_or(reach);
        let mut cells = Vec::with_capacity(good * (width + 1));
        for (row, weight) in &entries[..good] {
            cells.extend_from_slice(row);
            cells.push(*weight);
        }
        let mut budget = Budget::new(limits);
        let out = match crate::port::consolidate(width, &cells, good, limits, &mut budget) {
            // An overflowing sum fails only when the accumulator finishes,
            // after every entry: a later bad row comes first. The kernel
            // passed the work check, so the prefix's charges were paid.
            Err(Error::Overflow) if good < entries.len() => {
                budget.remaining -= good * (width + 1);
                None
            }
            out => Some(out?),
        };
        if let Some((row, _)) = entries.get(good) {
            budget.charge(1usize.checked_add(row.len()).ok_or(Error::Overflow)?)?;
            return Err(Error::Schema);
        }
        out.ok_or(Error::Overflow)
    }
    pub fn width(&self) -> usize {
        self.width
    }
    pub fn len(&self) -> usize {
        self.cells.len() / (self.width + 1)
    }
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
    pub fn entries(&self) -> impl Iterator<Item = (&Row, i64)> {
        let rows = self
            .rows
            .get_or_init(|| self.iter().map(|(r, _)| r.to_vec()).collect());
        rows.iter().zip(self.iter().map(|(_, w)| w))
    }
    pub fn plus(&self, other: &Self, limits: Limits) -> Result<Self, Error> {
        union(&[self, other], limits, &mut Budget::new(limits))
    }
    /// A set from rows already canonical (every weight nonzero).
    pub(crate) fn from_rows(width: usize, rows: BTreeMap<Row, i64>) -> Self {
        let mut cells = Vec::with_capacity(rows.len() * (width + 1));
        for (row, weight) in rows {
            cells.extend_from_slice(&row);
            cells.push(weight);
        }
        Self::from_cells(width, cells)
    }
    /// A set from the canonical cells a kernel returned.
    pub(crate) fn from_cells(width: usize, cells: Vec<i64>) -> Self {
        Self {
            width,
            cells,
            rows: std::sync::OnceLock::new(),
        }
    }
    /// Rows and weights in ascending row order, rows borrowed as slices.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&[i64], i64)> {
        let width = self.width;
        self.cells
            .chunks_exact(width + 1)
            .map(move |c| (&c[..width], c[width]))
    }
    /// The rows, borrowed as slices, in ascending order.
    pub(crate) fn rows(&self) -> impl Iterator<Item = &[i64]> {
        self.iter().map(|(r, _)| r)
    }
}
impl Clone for ZSet {
    fn clone(&self) -> Self {
        Self::from_cells(self.width, self.cells.clone())
    }
}
impl PartialEq for ZSet {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width && self.cells == other.cells
    }
}
impl Eq for ZSet {}
/// Prints what the Rust build's derived `Debug` prints.
impl std::fmt::Debug for ZSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct Rows<'a>(&'a ZSet);
        impl std::fmt::Debug for Rows<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_map().entries(self.0.iter()).finish()
            }
        }
        f.debug_struct("ZSet")
            .field("width", &self.width)
            .field("rows", &Rows(self))
            .finish()
    }
}
/// Builds sets from decoded records one row at a time, for every input of
/// a circuit at once (`widths`). The rows and their i128 sums live in the
/// Vesper accumulator (`vesper/src/acc.ves`); this wrapper charges the work
/// and checks the width first, as the Rust accumulator did.
pub(crate) struct Accumulator {
    widths: Vec<usize>,
    pub(crate) acc: crate::port::Acc,
}
impl Accumulator {
    pub(crate) fn new(widths: &[usize], limits: Limits) -> Result<Self, Error> {
        if widths.iter().any(|w| *w > limits.max_columns) {
            return Err(Error::Limit("columns"));
        }
        Ok(Self {
            widths: widths.to_vec(),
            acc: crate::port::Acc::new(widths, limits.max_rows),
        })
    }
    pub(crate) fn add(
        &mut self,
        input: usize,
        row: &[i64],
        weight: i128,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        budget.charge(1usize.checked_add(row.len()).ok_or(Error::Overflow)?)?;
        if row.len() != self.widths[input] {
            return Err(Error::Schema);
        }
        self.acc.add(input, row, weight)
    }
    /// Every input's set, in input order.
    pub(crate) fn finish(self) -> Result<Vec<ZSet>, Error> {
        self.widths
            .iter()
            .enumerate()
            .map(|(i, w)| Ok(ZSet::from_cells(*w, self.acc.finish(i)?)))
            .collect()
    }
}
/// Up to three sets per kernel call; a set of another width is a schema
/// error after the sets before it are added.
pub(crate) fn union(sets: &[&ZSet], limits: Limits, budget: &mut Budget) -> Result<ZSet, Error> {
    let width = sets.first().ok_or(Error::Schema)?.width;
    let good = sets
        .iter()
        .position(|s| s.width != width)
        .unwrap_or(sets.len());
    if good > 3 {
        return Err(Error::Partition("union of more than three sets"));
    }
    let out = crate::port::union(width, &sets[..good], limits, budget)?;
    if good < sets.len() {
        return Err(Error::Schema);
    }
    Ok(out)
}
